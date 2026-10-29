//! Minimal official test262 runner for the supported JavaScript slice.
//!
//! This runner intentionally reports four states instead of manufacturing a
//! pass rate: pass, fail, skip, and unsupported. The manifest is deliberately
//! small while the interpreter grows; every listed path is an official file
//! from the pinned test262 revision.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use render_core::html::parse_document;
use render_core::js::{
    CompiledScript, JsError, JsErrorKind, JsRuntime, JsValue, ModuleLoader, RuntimeLimits,
};
use url::Url;

const TEST262_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/test262");
const REVISION: &str = "5ef1e5723be95296f36afb0386676fed0205869c";
const BASELINE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/test262-baseline.tsv");

static DEFAULT_HARNESS: OnceLock<Result<Vec<CompiledScript>, String>> = OnceLock::new();

/// Execution-step budget of each test262 script. The browser keeps the
/// default budget, which is sized for page scripts. Generated Unicode
/// property-escape tests build and scan strings of about a million code points
/// inside the test body, work the specification does not bound and that a
/// page never does, so only this host raises the budget.
const TEST262_EXECUTION_STEPS: usize = 50_000_000;

fn test262_limits() -> RuntimeLimits {
    RuntimeLimits {
        max_execution_steps: TEST262_EXECUTION_STEPS,
        ..RuntimeLimits::default()
    }
}

#[derive(Debug, Default)]
struct Metadata {
    flags: Vec<String>,
    includes: Vec<String>,
    features: Vec<String>,
    negative: Option<NegativeExpectation>,
}

#[derive(Debug)]
struct NegativeExpectation {
    phase: String,
    error_type: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Pass,
    Fail,
    Skip,
    Unsupported,
    Timeout,
    Crash,
}

impl Status {
    const ALL: [Self; 6] = [
        Self::Pass,
        Self::Fail,
        Self::Skip,
        Self::Unsupported,
        Self::Timeout,
        Self::Crash,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Skip => "skip",
            Self::Unsupported => "unsupported",
            Self::Timeout => "timeout",
            Self::Crash => "crash",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.label() == value)
    }
}

#[derive(Clone, Debug)]
struct ResultRecord {
    path: String,
    variant: String,
    status: Status,
    detail: String,
}

#[test]
#[ignore = "full pinned test262 measurement; tools/check.sh runs this in release"]
fn pinned_test262_manifest_reports_a_real_baseline() {
    assert_pinned_revision();
    if !Path::new(TEST262_ROOT).join("test").is_dir() {
        return;
    }
    let mut paths = discover_test_paths();
    if let Ok(prefix) = env::var("RENDER_TEST262_PATH_PREFIX") {
        paths.retain(|path| path_matches_prefix(path, &prefix));
    }
    if let Some(max_files) = env_usize("RENDER_TEST262_MAX_FILES") {
        paths.truncate(max_files);
    }
    assert!(
        !paths.is_empty(),
        "test262 tree must contain JavaScript tests"
    );

    if env::var_os("RENDER_TEST262_MODE").as_deref() == Some("worker".as_ref()) {
        run_worker(&paths).expect("test262 worker infrastructure must be reliable");
        return;
    }
    if env::var_os("RENDER_TEST262_MODE").as_deref() == Some("one".as_ref()) {
        let paths = env::var("RENDER_TEST262_ONE_PATHS").expect("one-shot needs paths");
        let handle = std::thread::Builder::new()
            .stack_size(512 * 1024 * 1024)
            .spawn(move || {
                let mut output = BufWriter::new(io::stdout().lock());
                for path in paths.lines() {
                    for record in run_manifest_case(path) {
                        write!(output, "R	").ok();
                        write_record(&mut output, &record).ok();
                    }
                    writeln!(output, "E	{}", encode_field(path)).ok();
                    output.flush().ok();
                }
            })
            .expect("spawn one-shot test thread");
        handle.join().expect("one-shot test thread");
        return;
    }

    let summary =
        run_parallel(&paths).expect("test262 coordinator infrastructure must be reliable");
    assert!(summary.variants > 0, "test262 tree produced no variants");
    println!(
        "test262 revision {REVISION}: files={} variants={} pass={} fail={} skip={} unsupported={} timeout={} crash={}",
        paths.len(),
        summary.variants,
        summary.counts[0],
        summary.counts[1],
        summary.counts[2],
        summary.counts[3],
        summary.counts[4],
        summary.counts[5]
    );
    println!("test262 categories:");
    for (category, counts) in &summary.categories {
        println!(
            "  {category}: pass={} fail={} skip={} unsupported={} timeout={} crash={}",
            counts[0], counts[1], counts[2], counts[3], counts[4], counts[5]
        );
    }
    println!("test262 non-pass clusters: {:#?}", summary.clusters);
    for sample in summary.samples {
        println!(
            "{}\t{}\t{}\t{}",
            sample.status.label(),
            sample.path,
            sample.variant,
            sample.detail
        );
    }

    let full_run = env::var("RENDER_TEST262_PATH_PREFIX").is_err()
        && env_usize("RENDER_TEST262_MAX_FILES").is_none();
    if env::var_os("RENDER_TEST262_UPDATE_BASELINE").is_some() {
        write_baseline(&summary.buckets);
        println!("test262 baseline rewritten: {BASELINE_PATH}");
    } else if full_run {
        enforce_baseline(&summary.buckets);
        println!(
            "test262 baseline gate: {} buckets ok",
            summary.buckets.len()
        );
    }
}

/// First two path segments ("built-ins/Array", "language/statements", ...):
/// fine enough that a regression in one area cannot hide behind another.
fn baseline_bucket(path: &str) -> &'static str {
    let mut segments = path.split('/');
    match (segments.next(), segments.next()) {
        (Some(first), Some(second)) => {
            let mut bucket = String::from(first);
            bucket.push('/');
            bucket.push_str(second);
            // Leaked on purpose: bucket strings are tiny, bounded by the
            // test262 directory layout, and live for the process lifetime.
            Box::leak(bucket.into_boxed_str())
        }
        _ => "other",
    }
}

fn write_baseline(buckets: &BTreeMap<String, [usize; 6]>) {
    use std::fmt::Write as _;
    let index_of = |status: Status| {
        Status::ALL
            .iter()
            .position(|candidate| *candidate == status)
            .expect("known")
    };
    let mut output = String::from(
        "# test262 conformance baseline (bucket\tpass\tfail\ttimeout\tcrash\ttotal)\n",
    );
    for (bucket, counts) in buckets {
        let pass = counts[index_of(Status::Pass)];
        let fail = counts[index_of(Status::Fail)];
        let timeout = counts[index_of(Status::Timeout)];
        let crash = counts[index_of(Status::Crash)];
        let total: usize = counts.iter().sum();
        let _ = writeln!(
            output,
            "{bucket}\t{pass}\t{fail}\t{timeout}\t{crash}\t{total}"
        );
    }
    std::fs::write(BASELINE_PATH, output).expect("write test262 baseline");
}

fn parse_baseline(text: &str) -> Vec<(String, usize, usize)> {
    text.lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            (
                fields[0].to_owned(),
                fields[1].parse::<usize>().expect("baseline pass count"),
                fields[5].parse::<usize>().expect("baseline total"),
            )
        })
        .collect()
}

/// Gate a full run against the checked-in baseline: a bucket must not lose
/// passes that this run's own timeouts or crashes do not explain (timing
/// flakes and cross-platform stack differences stay tolerated; a genuine
/// Fail never is).
fn enforce_baseline(buckets: &BTreeMap<String, [usize; 6]>) {
    let text = std::fs::read_to_string(BASELINE_PATH).unwrap_or_else(|error| {
        panic!(
            "test262 baseline {BASELINE_PATH} is missing (generate it with \
             RENDER_TEST262_UPDATE_BASELINE=1): {error}"
        )
    });
    let index_of = |status: Status| {
        Status::ALL
            .iter()
            .position(|candidate| *candidate == status)
            .expect("known")
    };
    let mut regressions = Vec::new();
    for (bucket, pass, total) in parse_baseline(&text) {
        let Some(counts) = buckets.get(&bucket) else {
            regressions.push(format!(
                "{bucket}: missing from this run (baseline total {total})"
            ));
            continue;
        };
        let current_pass = counts[index_of(Status::Pass)];
        let total: usize = counts.iter().sum();
        // Two tolerance layers: per-run crash/timeout placement shifts a
        // handful of variants even with isolated slices, so tiny buckets get
        // a small absolute allowance; large buckets cap the allowance at a
        // small fraction so a mass-crash cascade or a real collapse can
        // never hide behind it.
        let excused = (counts[index_of(Status::Timeout)] + counts[index_of(Status::Crash)])
            .min(total / 20)
            .max(4)
            .max(pass / 20);
        if current_pass + excused < pass {
            regressions.push(format!(
                "{bucket}: pass {current_pass} + excused {excused} < baseline {pass}"
            ));
        }
    }
    assert!(
        regressions.is_empty(),
        "test262 conformance regressed against the baseline:\n{}",
        regressions.join("\n")
    );
}

#[derive(Debug, Default)]
struct Summary {
    variants: usize,
    counts: [usize; 6],
    categories: BTreeMap<String, [usize; 6]>,
    /// Fine-grained per-area buckets (first two path segments) used for the
    /// checked-in conformance baseline.
    buckets: BTreeMap<String, [usize; 6]>,
    clusters: BTreeMap<String, usize>,
    samples: Vec<ResultRecord>,
}

impl Summary {
    fn add(&mut self, record: ResultRecord) {
        self.variants = self.variants.saturating_add(1);
        let index = Status::ALL
            .iter()
            .position(|status| *status == record.status)
            .expect("status must be in Status::ALL");
        self.counts[index] = self.counts[index].saturating_add(1);
        self.categories
            .entry(category_for_path(&record.path).to_owned())
            .or_default()[index] += 1;
        self.buckets
            .entry(baseline_bucket(&record.path).to_owned())
            .or_default()[index] += 1;
        if record.status != Status::Pass {
            *self
                .clusters
                .entry(classify_detail(&record.detail))
                .or_default() += 1;
            if self.samples.len() < 100 {
                self.samples.push(record);
            }
        }
    }
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    current: Option<(String, Instant)>,
    records: Vec<ResultRecord>,
    /// Identity of this worker process within its slot. Reader threads of a
    /// killed worker keep delivering lines until the pipe closes; without
    /// this tag a stale `Eof` would kill the healthy replacement and crash
    /// every path it would otherwise run (a run-long cascade).
    generation: u64,
}

#[derive(Debug)]
enum WorkerEvent {
    Line {
        slot: usize,
        generation: u64,
        line: String,
    },
    Eof {
        slot: usize,
        generation: u64,
    },
}

/// Monotonic worker identity shared by every slot.
static WORKER_GENERATIONS: AtomicU64 = AtomicU64::new(1);

#[allow(clippy::too_many_lines)]
fn run_parallel(paths: &[String]) -> io::Result<Summary> {
    let worker_count = env_usize("RENDER_TEST262_WORKERS").unwrap_or_else(|| {
        thread::available_parallelism().map_or(1, |count| count.get().saturating_div(2).clamp(1, 8))
    });
    let timeout =
        Duration::from_secs(env_usize("RENDER_TEST262_TIMEOUT_SECS").unwrap_or(30) as u64);
    let run_dir = env::var_os("RENDER_TEST262_RUN_DIR").map_or_else(
        || {
            // Millisecond-unique: a recycled pid must never resurrect a
            // stale resume cache and double-count this run's results.
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_millis());
            Path::new(TEST262_ROOT)
                .join("../../target/test262-runs")
                .join(format!("run-{unique}"))
        },
        |dir| {
            // Cargo runs integration tests with the crate directory as the
            // working directory. Resolve a relative override against the
            // workspace root so "target/test262-runs/..." means what a reader
            // typing the command at the repository root expects.
            let path = PathBuf::from(dir);
            if path.is_absolute() {
                path
            } else {
                Path::new(TEST262_ROOT).join("../..").join(path)
            }
        },
    );
    fs::create_dir_all(&run_dir)?;
    let results_path = run_dir.join("results.tsv");
    let completed_path = run_dir.join("completed.txt");
    let mut summary = Summary::default();
    for record in read_records(&results_path)? {
        summary.add(record);
    }
    let mut completed = read_completed(&completed_path)?;
    let mut pending = paths
        .iter()
        .filter(|path| !completed.contains(path.as_str()))
        .cloned()
        .collect::<VecDeque<_>>();
    let mut results = BufWriter::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&results_path)?,
    );
    let mut completed_file = BufWriter::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&completed_path)?,
    );
    let (sender, receiver) = mpsc::channel();
    let mut workers = BTreeMap::new();
    // A resumed run can already have every path in `completed.txt`; spawning a
    // worker with no first assignment would leave it waiting on stdin forever.
    for id in 0..worker_count.min(pending.len()) {
        workers.insert(id, spawn_worker(id, &sender)?);
    }
    for worker in workers.values_mut() {
        assign_next(worker, &mut pending)?;
    }

    let started = Instant::now();
    while !workers.is_empty() {
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(WorkerEvent::Line {
                slot,
                generation,
                line,
            }) => {
                if let Some(worker) = workers.get_mut(&slot) {
                    if worker.generation != generation {
                        // A killed worker's final records must not be
                        // attributed to its successor's current path.
                        continue;
                    }
                    handle_worker_line(
                        worker,
                        &line,
                        &mut pending,
                        &mut summary,
                        &mut completed,
                        &mut results,
                        &mut completed_file,
                    )?;
                    if worker.current.is_none() {
                        assign_next(worker, &mut pending)?;
                    }
                }
            }
            Ok(WorkerEvent::Eof { slot, generation }) => {
                // Only the live generation may report a death; a stale Eof
                // from an already-replaced process is ignored.
                if workers
                    .get(&slot)
                    .is_none_or(|worker| worker.generation == generation)
                {
                    replace_failed_worker(
                        slot,
                        Status::Crash,
                        "worker process exited unexpectedly",
                        &mut workers,
                        &mut pending,
                        &sender,
                        &mut summary,
                        &mut completed,
                        &mut results,
                        &mut completed_file,
                    )?;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        let timed_out = workers
            .iter()
            .filter_map(|(id, worker)| {
                worker.current.as_ref().and_then(|(_, case_started)| {
                    (case_started.elapsed() >= timeout).then_some(*id)
                })
            })
            .collect::<Vec<_>>();
        for id in timed_out {
            replace_failed_worker(
                id,
                Status::Timeout,
                "test file exceeded the hard timeout",
                &mut workers,
                &mut pending,
                &sender,
                &mut summary,
                &mut completed,
                &mut results,
                &mut completed_file,
            )?;
        }
        workers.retain(|_, worker| worker.current.is_some() || !pending.is_empty());
        if summary.variants % 1_000 == 0 && summary.variants > 0 {
            eprintln!(
                "test262 progress: completed_files={}/{} variants={} elapsed={:.1}s",
                completed.len(),
                paths.len(),
                summary.variants,
                started.elapsed().as_secs_f64()
            );
        }
        if pending.is_empty() && workers.values().all(|worker| worker.current.is_none()) {
            break;
        }
    }
    for worker in workers.values_mut() {
        let _ = worker.child.kill();
        let _ = worker.child.wait();
    }
    results.flush()?;
    completed_file.flush()?;
    Ok(summary)
}

fn env_usize(name: &str) -> Option<usize> {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
}

fn spawn_worker(slot: usize, sender: &mpsc::Sender<WorkerEvent>) -> io::Result<Worker> {
    let generation = WORKER_GENERATIONS.fetch_add(1, Ordering::Relaxed);
    let mut child = Command::new(env::current_exe()?)
        .args([
            "--ignored",
            "--exact",
            "pinned_test262_manifest_reports_a_real_baseline",
            "--nocapture",
        ])
        .env("RENDER_TEST262_MODE", "worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("worker stdin was not piped"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("worker stdout was not piped"))?;
    let sender = sender.clone();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender
                .send(WorkerEvent::Line {
                    slot,
                    generation,
                    line,
                })
                .is_err()
            {
                return;
            }
        }
        let _ = sender.send(WorkerEvent::Eof { slot, generation });
    });
    Ok(Worker {
        child,
        stdin,
        current: None,
        records: Vec::new(),
        generation,
    })
}

fn assign_next(worker: &mut Worker, pending: &mut VecDeque<String>) -> io::Result<()> {
    let Some(path) = pending.pop_front() else {
        return Ok(());
    };
    writeln!(worker.stdin, "{}", encode_field(&path))?;
    worker.stdin.flush()?;
    worker.current = Some((path, Instant::now()));
    worker.records.clear();
    Ok(())
}

fn handle_worker_line(
    worker: &mut Worker,
    line: &str,
    pending: &mut VecDeque<String>,
    summary: &mut Summary,
    completed: &mut BTreeSet<String>,
    results: &mut impl Write,
    completed_file: &mut impl Write,
) -> io::Result<()> {
    if let Some(payload) = line.strip_prefix("R\t") {
        if let Some(record) = parse_record(payload) {
            worker.records.push(record);
        }
        return Ok(());
    }
    if let Some(payload) = line.strip_prefix("Q\t") {
        // Slice-process requeue: run these again before pulling new work.
        for path in payload.split('\t').rev() {
            pending.push_front(decode_field(path));
        }
        return Ok(());
    }
    let Some(encoded_path) = line.strip_prefix("E\t") else {
        return Ok(());
    };
    let path = decode_field(encoded_path);
    if worker.current.as_ref().map(|(current, _)| current) != Some(&path) {
        // A replaced worker's in-flight line can land after its successor
        // takes over the id; dropping the stale completion keeps the
        // coordinator alive (the replacement re-runs that test anyway).
        return Ok(());
    }
    for record in worker.records.drain(..) {
        write_record(results, &record)?;
        summary.add(record);
    }
    writeln!(completed_file, "{}", encode_field(&path))?;
    results.flush()?;
    completed_file.flush()?;
    completed.insert(path);
    worker.current = None;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn replace_failed_worker(
    id: usize,
    status: Status,
    detail: &str,
    workers: &mut BTreeMap<usize, Worker>,
    pending: &mut VecDeque<String>,
    sender: &mpsc::Sender<WorkerEvent>,
    summary: &mut Summary,
    completed: &mut BTreeSet<String>,
    results: &mut impl Write,
    completed_file: &mut impl Write,
) -> io::Result<()> {
    let Some(mut worker) = workers.remove(&id) else {
        return Ok(());
    };
    let _ = worker.child.kill();
    let _ = worker.child.wait();
    if let Some((path, _)) = worker.current.take() {
        for variant in expected_variants(&path) {
            let record = ResultRecord {
                path: path.clone(),
                variant,
                status,
                detail: detail.to_owned(),
            };
            write_record(results, &record)?;
            summary.add(record);
        }
        writeln!(completed_file, "{}", encode_field(&path))?;
        completed.insert(path);
        results.flush()?;
        completed_file.flush()?;
    }
    if !pending.is_empty() {
        let mut replacement = spawn_worker(id, sender)?;
        assign_next(&mut replacement, pending)?;
        workers.insert(id, replacement);
    }
    Ok(())
}

fn run_worker(_paths: &[String]) -> io::Result<()> {
    // The interpreter recurses deeply on minified tests; the worker's main
    // thread needs the same generous stack the browser shell uses, or one
    // deep test aborts the whole worker and cascades crashes across its
    // queue.
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(worker_loop)
        .expect("spawn test262 worker thread");
    handle.join().expect("test262 worker thread")
}

fn worker_loop() -> io::Result<()> {
    let mut output = BufWriter::new(io::stdout().lock());
    for line in io::stdin().lock().lines() {
        let path = decode_field(&line?);
        for record in run_manifest_case(&path) {
            write!(output, "R\t")?;
            write_record(&mut output, &record)?;
        }
        writeln!(output, "E\t{}", encode_field(&path))?;
        output.flush()?;
    }
    Ok(())
}

fn write_record(writer: &mut impl Write, record: &ResultRecord) -> io::Result<()> {
    writeln!(
        writer,
        "{}\t{}\t{}\t{}",
        record.status.label(),
        encode_field(&record.path),
        encode_field(&record.variant),
        encode_field(&record.detail)
    )
}

fn parse_record(line: &str) -> Option<ResultRecord> {
    let mut fields = line.splitn(4, '\t');
    let status = fields.next().and_then(Status::parse)?;
    Some(ResultRecord {
        path: decode_field(fields.next()?),
        variant: decode_field(fields.next()?),
        status,
        detail: decode_field(fields.next()?),
    })
}

fn read_records(path: &Path) -> io::Result<Vec<ResultRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(BufReader::new(File::open(path)?)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| parse_record(&line))
        .collect())
}

fn read_completed(path: &Path) -> io::Result<BTreeSet<String>> {
    if !path.exists() {
        return Ok(BTreeSet::new());
    }
    Ok(BufReader::new(File::open(path)?)
        .lines()
        .map_while(Result::ok)
        .map(|line| decode_field(&line))
        .collect())
}

fn encode_field(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\t', "%09")
        .replace('\n', "%0A")
}

fn decode_field(value: &str) -> String {
    value
        .replace("%0A", "\n")
        .replace("%09", "\t")
        .replace("%25", "%")
}

fn expected_variants(path: &str) -> Vec<String> {
    let source_path = Path::new(TEST262_ROOT).join("test").join(path);
    let Ok(source) = fs::read_to_string(source_path) else {
        return vec!["manifest".to_owned()];
    };
    let Ok(metadata) = parse_metadata(&source) else {
        return vec!["metadata".to_owned()];
    };
    if path.contains("_FIXTURE") {
        vec!["fixture".to_owned()]
    } else if metadata.flags.iter().any(|flag| flag == "module") {
        vec!["module".to_owned()]
    } else if metadata.flags.iter().any(|flag| flag == "async") {
        vec!["unsupported".to_owned()]
    } else if metadata.flags.iter().any(|flag| flag == "raw") {
        vec!["raw".to_owned()]
    } else if metadata.flags.iter().any(|flag| flag == "onlyStrict") {
        vec!["strict".to_owned()]
    } else if metadata.flags.iter().any(|flag| flag == "noStrict") {
        vec!["sloppy".to_owned()]
    } else {
        vec!["sloppy".to_owned(), "strict".to_owned()]
    }
}

fn category_for_path(path: &str) -> &str {
    if path.starts_with("language/") {
        "language"
    } else if path.starts_with("built-ins/") {
        "built-ins"
    } else if path.starts_with("intl402/") {
        "intl402"
    } else if path.starts_with("annexB/") {
        "annexB"
    } else if path.starts_with("staging/") {
        "staging"
    } else {
        "other"
    }
}

fn classify_detail(detail: &str) -> String {
    detail
        .split_once(':')
        .map_or(detail, |(prefix, _)| prefix)
        .chars()
        .take(120)
        .collect()
}

fn assert_pinned_revision() {
    let revision_path = Path::new(TEST262_ROOT).join(".render-revision");
    let Ok(actual_revision) = fs::read_to_string(revision_path) else {
        eprintln!("third_party/test262 is absent; skipping pinned test262 manifest run");
        return;
    };
    assert_eq!(
        actual_revision.trim(),
        REVISION,
        "vendored test262 revision does not match the runner"
    );
}

fn discover_test_paths() -> Vec<String> {
    let mut paths = Vec::new();
    collect_test_paths(&Path::new(TEST262_ROOT).join("test"), &mut paths);
    paths.sort();
    paths
}

fn path_matches_prefix(path: &str, prefix: &str) -> bool {
    let prefix = prefix.trim().replace('\\', "/");
    let prefix = prefix.trim_matches('/');
    prefix.is_empty() || path == prefix || path.starts_with(&format!("{prefix}/"))
}

fn collect_test_paths(directory: &Path, paths: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_test_paths(&path, paths);
        } else if path.extension().is_some_and(|extension| extension == "js")
            && let Ok(relative) = path.strip_prefix(Path::new(TEST262_ROOT).join("test"))
        {
            paths.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

fn run_manifest_case(relative_path: &str) -> Vec<ResultRecord> {
    let path = Path::new(TEST262_ROOT).join("test").join(relative_path);
    let source = match fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) => {
            return vec![ResultRecord {
                path: relative_path.to_owned(),
                variant: "manifest".to_owned(),
                status: Status::Fail,
                detail: format!("cannot read official test: {error}"),
            }];
        }
    };
    run_source_case(relative_path, &source)
}

/// Judge one test source without reading it from the pinned checkout.
///
/// Keeping the decision logic separate from the file read is what lets the
/// runner's own tests exercise classification and module routing on a clone
/// that has no `third_party/test262`; the suite itself still reads the
/// official files.
fn run_source_case(relative_path: &str, source: &str) -> Vec<ResultRecord> {
    let metadata = match parse_metadata(source) {
        Ok(metadata) => metadata,
        Err(error) => {
            return vec![ResultRecord {
                path: relative_path.to_owned(),
                variant: "metadata".to_owned(),
                status: Status::Fail,
                detail: error,
            }];
        }
    };

    if relative_path.contains("_FIXTURE") {
        return vec![ResultRecord {
            path: relative_path.to_owned(),
            variant: "fixture".to_owned(),
            status: Status::Skip,
            detail: "module fixture is not a standalone test".to_owned(),
        }];
    }
    // The host is a browser main thread, which cannot block (AgentCanSuspend is
    // false), so a test that needs a blocking agent is outside this host's profile.
    if metadata.flags.iter().any(|flag| flag == "CanBlockIsTrue") {
        return unsupported(
            relative_path,
            "the host main thread cannot block (AgentCanSuspend is false)",
        );
    }
    if metadata.flags.iter().any(|flag| flag == "module") {
        return run_module_test(relative_path, source, &metadata);
    }

    let variants = if metadata.flags.iter().any(|flag| flag == "raw") {
        vec![("raw", false)]
    } else if metadata.flags.iter().any(|flag| flag == "onlyStrict") {
        vec![("strict", true)]
    } else if metadata.flags.iter().any(|flag| flag == "noStrict") {
        vec![("sloppy", false)]
    } else {
        vec![("sloppy", false), ("strict", true)]
    };

    variants
        .into_iter()
        .map(|(variant, strict)| run_variant(relative_path, source, &metadata, variant, strict))
        .collect()
}

fn run_variant(
    relative_path: &str,
    source: &str,
    metadata: &Metadata,
    variant: &str,
    strict: bool,
) -> ResultRecord {
    if metadata.flags.iter().any(|flag| flag == "async") {
        return run_async_variant(relative_path, source, metadata, variant, strict);
    }
    let raw = metadata.flags.iter().any(|flag| flag == "raw");
    let program = if strict && !raw {
        format!("\"use strict\";\n{source}")
    } else {
        source.to_owned()
    };
    let script = match CompiledScript::compile(&program, &RuntimeLimits::default()) {
        Ok(script) => script,
        Err(error) => {
            return classify_result(relative_path, variant, metadata, Err::<(), JsError>(error));
        }
    };

    let mut document = parse_document("<!doctype html><p>test262</p>");
    let mut runtime = test_runtime(&document.dom, relative_path);
    if !raw
        && let Err(detail) = install_harness(&mut runtime, &mut document.dom, &metadata.includes)
    {
        return ResultRecord {
            path: relative_path.to_owned(),
            variant: variant.to_owned(),
            status: Status::Fail,
            detail,
        };
    }

    let result = runtime.execute_compiled(&mut document.dom, &script);
    classify_result(
        relative_path,
        variant,
        metadata,
        result.map(|outcome| outcome.value),
    )
}

/// Captures what the harness prints. `$DONE` (from `doneprintHandle.js`)
/// reports an async test's outcome through `print`.
// The shim accumulates text rather than pushing into an array: an array write is a
// [[Set]] that a test may have intercepted with an inherited setter, and that must not
// break the host's own reporting (INTERPRETING.md "Asynchronous Tests").
const PRINT_SHIM: &str = "var __test262_printed = ''; function print(message) { __test262_printed += String(message) + '\\n'; }";
const PRINTED_LOG: &str = "__test262_printed";

/// ECMA-262 `async` tests (INTERPRETING.md "Asynchronous Tests"): run the
/// script, then every queued job, and only then read what `$DONE` printed. The
/// test passes only when the log holds `Test262:AsyncTestComplete`.
fn run_async_variant(
    relative_path: &str,
    source: &str,
    metadata: &Metadata,
    variant: &str,
    strict: bool,
) -> ResultRecord {
    let fail = |detail: String| ResultRecord {
        path: relative_path.to_owned(),
        variant: variant.to_owned(),
        status: Status::Fail,
        detail,
    };
    let program = if strict {
        format!("\"use strict\";\n{source}")
    } else {
        source.to_owned()
    };
    let script = match CompiledScript::compile(&program, &RuntimeLimits::default()) {
        Ok(script) => script,
        Err(error) => {
            return classify_result(relative_path, variant, metadata, Err::<(), JsError>(error));
        }
    };

    let mut document = parse_document("<!doctype html><p>test262</p>");
    let mut runtime = test_runtime(&document.dom, relative_path);
    let shim = CompiledScript::compile(PRINT_SHIM, &RuntimeLimits::default())
        .expect("print shim compiles");
    if let Err(error) = runtime.execute_compiled(&mut document.dom, &shim) {
        return fail(format!("print shim failed: {}", error.message()));
    }
    // The host provides `$DONE` to every async test (INTERPRETING.md), so the
    // handle is installed even when the metadata does not list it.
    let mut includes = metadata.includes.clone();
    if !includes
        .iter()
        .any(|include| include == "doneprintHandle.js")
    {
        includes.push("doneprintHandle.js".to_owned());
    }
    if let Err(detail) = install_harness(&mut runtime, &mut document.dom, &includes) {
        return fail(detail);
    }
    if let Err(error) = runtime.execute_compiled(&mut document.dom, &script) {
        return fail(format!(
            "unexpected {:?}: {}",
            error.kind(),
            error.message()
        ));
    }
    if let Err(error) = drain_microtasks(&mut runtime, &mut document.dom) {
        return fail(format!(
            "unexpected {:?} in a queued job: {}",
            error.kind(),
            error.message()
        ));
    }
    async_completion(&mut runtime, &mut document.dom, relative_path, variant)
}

/// Judge an async test by what `$DONE` printed: the test passes only when the
/// log holds `Test262:AsyncTestComplete`.
fn async_completion(
    runtime: &mut JsRuntime,
    dom: &mut render_core::dom::Dom,
    relative_path: &str,
    variant: &str,
) -> ResultRecord {
    let fail = |detail: String| ResultRecord {
        path: relative_path.to_owned(),
        variant: variant.to_owned(),
        status: Status::Fail,
        detail,
    };
    let log = match CompiledScript::compile(PRINTED_LOG, &RuntimeLimits::default()) {
        Ok(log) => log,
        Err(error) => return fail(format!("print log expression: {}", error.message())),
    };
    let printed = match runtime.execute_compiled(dom, &log) {
        Ok(outcome) => outcome.value.to_js_string().to_lossy(),
        Err(error) => return fail(format!("print log failed: {}", error.message())),
    };
    let lines: Vec<&str> = printed.lines().collect();
    if lines.contains(&"Test262:AsyncTestComplete") {
        return ResultRecord {
            path: relative_path.to_owned(),
            variant: variant.to_owned(),
            status: Status::Pass,
            detail: "async test reported completion".to_owned(),
        };
    }
    match lines
        .iter()
        .find(|line| line.starts_with("Test262:AsyncTestFailure"))
    {
        Some(line) => fail((*line).to_owned()),
        None => fail("async test never reported completion".to_owned()),
    }
}

/// Runs queued jobs until none remain, as a host's microtask checkpoint does.
fn drain_microtasks(
    runtime: &mut JsRuntime,
    dom: &mut render_core::dom::Dom,
) -> Result<(), JsError> {
    loop {
        let pending = runtime.take_pending_microtasks();
        if pending.is_empty() {
            return Ok(());
        }
        for microtask in pending {
            runtime.invoke_microtask(dom, microtask)?;
        }
    }
}

fn default_harness_sources() -> io::Result<Vec<String>> {
    ["assert.js", "sta.js"]
        .into_iter()
        .map(|name| fs::read_to_string(Path::new(TEST262_ROOT).join("harness").join(name)))
        .collect()
}

/// The `$262` host object of the current realm (INTERPRETING.md, "Host-defined
/// functions"): `detachArrayBuffer`, `createRealm`, `evalScript` and `global`.
/// Each realm gets its own, so a realm made by `createRealm` has one too.
fn install_test262_host(
    runtime: &mut JsRuntime,
    dom: &mut render_core::dom::Dom,
) -> Result<(), String> {
    let script = CompiledScript::compile("var $262 = { global: this };", &RuntimeLimits::default())
        .map_err(|error| error.message().to_owned())?;
    runtime
        .execute_nested(dom, &script)
        .map_err(|error| error.message().to_owned())?;
    let Some(JsValue::Object(host)) = runtime.realm().global("$262") else {
        return Err("the $262 host object is not defined".to_owned());
    };
    runtime.define_host_function(host, "detachArrayBuffer", detach_array_buffer_host);
    runtime.define_host_function(host, "createRealm", create_realm_host);
    runtime.define_host_function(host, "evalScript", eval_script_host);
    Ok(())
}

fn detach_array_buffer_host(
    runtime: &mut JsRuntime,
    _dom: &mut render_core::dom::Dom,
    arguments: &[JsValue],
) -> Result<JsValue, JsError> {
    runtime.detach_array_buffer(arguments.first().unwrap_or(&JsValue::Undefined))?;
    Ok(JsValue::Undefined)
}

/// `$262.createRealm()`: a new realm with its own global object, whose `$262`
/// is returned. The new realm's `$262` is installed while it is current, so
/// its functions belong to that realm.
#[allow(
    clippy::unnecessary_wraps,
    reason = "a host function is a HostFunction, which returns a Result"
)]
fn create_realm_host(
    runtime: &mut JsRuntime,
    dom: &mut render_core::dom::Dom,
    _arguments: &[JsValue],
) -> Result<JsValue, JsError> {
    let realm = runtime.create_realm(dom);
    runtime
        .with_realm(realm, |runtime| install_test262_host(runtime, dom))
        .expect("the $262 host installs in a fresh realm");
    Ok(runtime
        .with_realm(realm, |runtime| runtime.realm().global("$262"))
        .unwrap_or(JsValue::Undefined))
}

/// `$262.evalScript(source)`: runs `source` as a script in the realm of the
/// `evalScript` function. A parse error is a `SyntaxError` of that realm.
fn eval_script_host(
    runtime: &mut JsRuntime,
    dom: &mut render_core::dom::Dom,
    arguments: &[JsValue],
) -> Result<JsValue, JsError> {
    let source = arguments.first().map_or_else(
        || "undefined".to_owned(),
        |value| value.to_js_string().to_lossy(),
    );
    let script = CompiledScript::compile(&source, &RuntimeLimits::default())?;
    Ok(runtime.execute_nested(dom, &script)?.value)
}

fn install_harness(
    runtime: &mut JsRuntime,
    dom: &mut render_core::dom::Dom,
    includes: &[String],
) -> Result<(), String> {
    install_test262_host(runtime, dom)?;
    let harness = DEFAULT_HARNESS.get_or_init(|| {
        default_harness_sources()
            .and_then(|sources| {
                sources
                    .iter()
                    .map(|source| CompiledScript::compile(source, &RuntimeLimits::default()))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "default test262 harness compilation failed with {:?} at {:?}: {}",
                                error.kind(),
                                error.offset(),
                                error.message()
                            ),
                        )
                    })
            })
            .map_err(|error| error.to_string())
    });
    let scripts = harness.as_ref().map_err(Clone::clone)?;
    for (index, script) in scripts.iter().enumerate() {
        runtime.execute_compiled(dom, script).map_err(|error| {
            format!(
                "test262 harness {index} failed with {:?} at {:?}: {}",
                error.kind(),
                error.offset(),
                error.message()
            )
        })?;
    }
    for include in includes {
        let include_path = Path::new(TEST262_ROOT).join("harness").join(include);
        let include_source = fs::read_to_string(&include_path)
            .map_err(|_| format!("cannot read harness include {include}"))?;
        let script = CompiledScript::compile(&include_source, &RuntimeLimits::default()).map_err(
            |error| {
                format!(
                    "harness include {include} compilation failed with {:?}: {}",
                    error.kind(),
                    error.message()
                )
            },
        )?;
        runtime.execute_compiled(dom, &script).map_err(|error| {
            format!(
                "harness include {include} failed with {:?}: {}",
                error.kind(),
                error.message()
            )
        })?;
    }
    Ok(())
}

fn classify_result<T>(
    relative_path: &str,
    variant: &str,
    metadata: &Metadata,
    result: Result<T, JsError>,
) -> ResultRecord {
    let (conforms, detail) = match (&metadata.negative, result) {
        (None, Ok(_)) => (true, "completed without an uncaught exception".to_owned()),
        (None, Err(error)) => (
            false,
            format!("unexpected {:?}: {}", error.kind(), error.message()),
        ),
        (Some(expected), Err(error)) => {
            let matches_expected = negative_matches(expected, &error);
            let detail = if matches_expected {
                format!(
                    "matched negative {}:{} (actual {:?})",
                    expected.phase,
                    expected.error_type,
                    error.kind()
                )
            } else {
                format!(
                    "negative mismatch expected {}:{} but got {:?}: {}",
                    expected.phase,
                    expected.error_type,
                    error.kind(),
                    error.message()
                )
            };
            (matches_expected, detail)
        }
        (Some(expected), Ok(_)) => (
            false,
            format!(
                "negative expected {}:{} but script completed",
                expected.phase, expected.error_type
            ),
        ),
    };
    ResultRecord {
        path: relative_path.to_owned(),
        variant: variant.to_owned(),
        status: if conforms { Status::Pass } else { Status::Fail },
        detail,
    }
}

fn negative_matches(expected: &NegativeExpectation, error: &JsError) -> bool {
    let actual_type = match error.kind() {
        JsErrorKind::Syntax => "SyntaxError",
        JsErrorKind::Reference => "ReferenceError",
        JsErrorKind::Type => "TypeError",
        JsErrorKind::Dom | JsErrorKind::ResourceLimit | JsErrorKind::Throw => "Error",
    };
    let actual_phase = if error.kind() == JsErrorKind::Syntax {
        "parse"
    } else {
        "runtime"
    };
    expected.phase == actual_phase && expected.error_type == actual_type
}

/// A module test (test262 INTERPRETING.md, `module` flag): the file is the
/// entry module. Its phases are the spec's: a parse, the load of its graph
/// (resolution), its link, and its evaluation (runtime).
fn run_module_test(path: &str, source: &str, metadata: &Metadata) -> Vec<ResultRecord> {
    let record = match metadata
        .negative
        .as_ref()
        .map(|expected| expected.phase.as_str())
    {
        Some("parse") => {
            let expected = metadata.negative.as_ref().expect("negative metadata");
            let (status, detail) =
                match CompiledScript::compile_module(source, &RuntimeLimits::default()) {
                    Err(error) if negative_matches(expected, &error) => (
                        Status::Pass,
                        format!(
                            "matched negative {}:{}",
                            expected.phase, expected.error_type
                        ),
                    ),
                    Err(error) => (
                        Status::Fail,
                        format!(
                            "negative mismatch expected {}:{} but got {error}",
                            expected.phase, expected.error_type
                        ),
                    ),
                    Ok(_) => (
                        Status::Fail,
                        format!(
                            "negative expected {}:{} but the module compiled",
                            expected.phase, expected.error_type
                        ),
                    ),
                };
            ResultRecord {
                path: path.to_owned(),
                variant: "module".to_owned(),
                status,
                detail,
            }
        }
        _ => run_module_variant(path, source, metadata),
    };
    vec![record]
}

/// The URL of a test file in the checkout: the base of its relative specifiers.
fn test_file_url(relative_path: &str) -> Result<Url, String> {
    let path = Path::new(TEST262_ROOT).join("test").join(relative_path);
    let canonical = fs::canonicalize(&path)
        .map_err(|error| format!("cannot resolve {}: {error}", path.display()))?;
    Url::from_file_path(&canonical)
        .map_err(|()| format!("{} is not an absolute file path", canonical.display()))
}

/// A runtime whose script base URL is the test file, with the file loader
/// installed, so `import()` and module requests resolve relative to the test.
fn test_runtime(dom: &render_core::dom::Dom, relative_path: &str) -> JsRuntime {
    let url = test_file_url(relative_path)
        .unwrap_or_else(|_| Url::parse("about:blank").expect("about:blank is a URL"));
    let mut runtime = JsRuntime::with_limits_and_url(dom, test262_limits(), &url);
    runtime.set_module_loader(Box::new(Test262ModuleLoader));
    runtime
}

/// Loads modules from the test262 checkout (ECMA-262 16.2.1.10
/// `HostLoadImportedModule` for a file host). A specifier resolves against the
/// URL of the module or script that requests it.
#[derive(Debug)]
struct Test262ModuleLoader;

impl ModuleLoader for Test262ModuleLoader {
    fn load(
        &mut self,
        referrer: &str,
        specifier: &str,
        attributes: &[(String, String)],
    ) -> Result<(String, CompiledScript), JsError> {
        if !attributes.is_empty() {
            return Err(JsError::type_error(
                "import attributes are not supported by the test262 host",
            ));
        }
        let base = Url::parse(referrer)
            .map_err(|error| JsError::type_error(format!("bad referrer {referrer}: {error}")))?;
        let url = base
            .join(specifier)
            .map_err(|error| JsError::type_error(format!("bad specifier {specifier}: {error}")))?;
        let path = url
            .to_file_path()
            .map_err(|()| JsError::type_error(format!("{url} is not a file")))?;
        let source = fs::read_to_string(&path)
            .map_err(|error| JsError::type_error(format!("cannot load {url}: {error}")))?;
        let script = CompiledScript::compile_module(&source, &RuntimeLimits::default())?;
        Ok((url.to_string(), script))
    }
}

/// Whether an error is the one a negative expectation names, at the stage it occurred.
fn module_error_matches(expected: &NegativeExpectation, stage: &str, error: &JsError) -> bool {
    let actual_type = match error.kind() {
        JsErrorKind::Syntax => "SyntaxError",
        JsErrorKind::Reference => "ReferenceError",
        JsErrorKind::Type => "TypeError",
        JsErrorKind::Dom | JsErrorKind::ResourceLimit | JsErrorKind::Throw => "Error",
    };
    expected.phase == stage && expected.error_type == actual_type
}

/// Run a module test's stages in order and judge the first outcome against the
/// test's negative expectation, if any.
#[allow(clippy::too_many_lines)]
fn run_module_variant(relative_path: &str, source: &str, metadata: &Metadata) -> ResultRecord {
    let variant = "module";
    let fail = |detail: String| ResultRecord {
        path: relative_path.to_owned(),
        variant: variant.to_owned(),
        status: Status::Fail,
        detail,
    };
    let url = match test_file_url(relative_path) {
        Ok(url) => url,
        Err(detail) => return fail(detail),
    };
    let key = url.to_string();
    let script = match CompiledScript::compile_module(source, &RuntimeLimits::default()) {
        Ok(script) => script,
        Err(error) => {
            return fail(format!(
                "unexpected {:?}: {}",
                error.kind(),
                error.message()
            ));
        }
    };
    let mut document = parse_document("<!doctype html><p>test262</p>");
    let mut runtime = JsRuntime::with_url(&document.dom, &url);
    runtime.set_module_loader(Box::new(Test262ModuleLoader));
    let asynchronous = metadata.flags.iter().any(|flag| flag == "async");
    let mut includes = metadata.includes.clone();
    if asynchronous {
        let shim = CompiledScript::compile(PRINT_SHIM, &RuntimeLimits::default())
            .expect("print shim compiles");
        if let Err(error) = runtime.execute_compiled(&mut document.dom, &shim) {
            return fail(format!("print shim failed: {}", error.message()));
        }
        if !includes
            .iter()
            .any(|include| include == "doneprintHandle.js")
        {
            includes.push("doneprintHandle.js".to_owned());
        }
    }
    if let Err(detail) = install_harness(&mut runtime, &mut document.dom, &includes) {
        return fail(detail);
    }

    // Each stage either fails with an error (judged against a negative
    // expectation) or moves on.
    let stage_error = match runtime.load_module(&key, &script) {
        Err(error) => Some(("resolution", error)),
        Ok(()) => None,
    }
    .or_else(|| {
        runtime
            .link_module(&key)
            .err()
            .map(|error| ("resolution", error))
    })
    .or_else(|| {
        runtime
            .evaluate_module(&mut document.dom, &key)
            .err()
            .map(|error| ("runtime", error))
    });
    let stage_error = match stage_error {
        Some(failure) => Some(failure),
        None => match drain_microtasks(&mut runtime, &mut document.dom) {
            Err(error) => Some(("runtime", error)),
            Ok(()) => runtime
                .module_evaluation_error(&key)
                .map(|error| ("runtime", error)),
        },
    };
    if let Some((stage, error)) = stage_error {
        return match &metadata.negative {
            Some(expected) if module_error_matches(expected, stage, &error) => ResultRecord {
                path: relative_path.to_owned(),
                variant: variant.to_owned(),
                status: Status::Pass,
                detail: format!(
                    "matched negative {}:{}",
                    expected.phase, expected.error_type
                ),
            },
            Some(expected) => fail(format!(
                "negative mismatch expected {}:{} but got {stage} {:?}: {}",
                expected.phase,
                expected.error_type,
                error.kind(),
                error.message()
            )),
            None => fail(format!(
                "unexpected {stage} {:?}: {}",
                error.kind(),
                error.message()
            )),
        };
    }
    if let Some(expected) = &metadata.negative {
        return fail(format!(
            "negative expected {}:{} but the module completed",
            expected.phase, expected.error_type
        ));
    }
    if asynchronous {
        return async_completion(&mut runtime, &mut document.dom, relative_path, variant);
    }
    ResultRecord {
        path: relative_path.to_owned(),
        variant: variant.to_owned(),
        status: Status::Pass,
        detail: "module completed without an uncaught exception".to_owned(),
    }
}

fn unsupported(path: &str, detail: &str) -> Vec<ResultRecord> {
    vec![ResultRecord {
        path: path.to_owned(),
        variant: "unsupported".to_owned(),
        status: Status::Unsupported,
        detail: detail.to_owned(),
    }]
}

fn parse_metadata(source: &str) -> Result<Metadata, String> {
    let Some(start) = source.find("/*---") else {
        return Ok(Metadata::default());
    };
    let body_start = start + "/*---".len();
    let Some(relative_end) = source[body_start..].find("---*/") else {
        return Err("unterminated test262 frontmatter".to_owned());
    };
    let body = &source[body_start..body_start + relative_end];
    let mut metadata = Metadata::default();
    let mut negative_phase = None;
    let mut negative_type = None;
    let mut in_negative = false;
    // The list whose YAML block sequence (`key:` then `- item` lines) is open.
    let mut block_list: Option<ListKey> = None;
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line == "---" {
            continue;
        }
        if let Some(key) = block_list
            && let Some(item) = line.strip_prefix('-')
        {
            list_for(&mut metadata, key).push(unquote(item.trim()));
            continue;
        }
        block_list = None;
        if line == "negative:" {
            in_negative = true;
            continue;
        }
        if in_negative && line.starts_with("phase:") {
            negative_phase = Some(value_after_colon(line));
            continue;
        }
        if in_negative && line.starts_with("type:") {
            negative_type = Some(value_after_colon(line));
            continue;
        }
        in_negative = false;
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let list = match key {
            "flags" => ListKey::Flags,
            "includes" => ListKey::Includes,
            "features" => ListKey::Features,
            _ => continue,
        };
        // An empty value opens a block sequence; otherwise the value is an inline list.
        if value.trim().is_empty() {
            block_list = Some(list);
        } else {
            *list_for(&mut metadata, list) = parse_list(value);
        }
    }
    if negative_phase.is_some() != negative_type.is_some() {
        return Err("negative metadata must contain both phase and type".to_owned());
    }
    if let (Some(phase), Some(error_type)) = (negative_phase, negative_type) {
        metadata.negative = Some(NegativeExpectation { phase, error_type });
    }
    Ok(metadata)
}

#[derive(Clone, Copy)]
enum ListKey {
    Flags,
    Includes,
    Features,
}

fn list_for(metadata: &mut Metadata, key: ListKey) -> &mut Vec<String> {
    match key {
        ListKey::Flags => &mut metadata.flags,
        ListKey::Includes => &mut metadata.includes,
        ListKey::Features => &mut metadata.features,
    }
}

fn unquote(item: &str) -> String {
    item.trim_matches(['\'', '"']).to_owned()
}

fn value_after_colon(line: &str) -> String {
    line.split_once(':')
        .map_or_else(String::new, |(_, value)| value.trim().to_owned())
}

fn parse_list(value: &str) -> Vec<String> {
    value
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(str::trim)
        .map(|item| item.trim_matches(['\'', '"']))
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod runner_tests {
    #[test]
    fn baseline_parser_reads_bucket_pass_and_total_columns() {
        let text = "# comment
built-ins/Array	420	7	1	0	900
language	88	2	0	1	200
";
        let parsed = super::parse_baseline(text);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "built-ins/Array");
        assert_eq!(parsed[0].1, 420);
        assert_eq!(parsed[0].2, 900);
        assert_eq!(parsed[1].0, "language");
        assert_eq!(parsed[1].1, 88);
    }

    #[test]
    fn baseline_bucket_uses_first_two_path_segments() {
        assert_eq!(
            super::baseline_bucket("built-ins/Array/from.js"),
            "built-ins/Array"
        );
        assert_eq!(
            super::baseline_bucket("language/statements/for-of/x.js"),
            "language/statements"
        );
        assert_eq!(
            super::baseline_bucket("staging/Smoke.js"),
            "staging/Smoke.js"
                .split('/')
                .take(2)
                .collect::<Vec<_>>()
                .join("/")
        );
        assert_eq!(super::baseline_bucket("loose.js"), "other");
    }

    use render_core::js::{CompiledScript, RuntimeLimits};

    use super::{Status, parse_metadata, path_matches_prefix, run_variant};

    #[test]
    fn path_prefix_filter_respects_directory_boundaries() {
        assert!(path_matches_prefix(
            "built-ins/Array/from.js",
            "built-ins/Array"
        ));
        assert!(path_matches_prefix(
            "language/statements/for.js",
            "language"
        ));
        assert!(!path_matches_prefix(
            "built-ins/ArrayBuffer/name.js",
            "built-ins/Array"
        ));
    }

    #[test]
    fn parses_inline_flags_includes_and_negative_metadata() {
        let metadata = parse_metadata(
            "/*---\nflags: [onlyStrict, generated]\nincludes: [helper.js]\nfeatures: [Promise]\nnegative:\n  phase: parse\n  type: SyntaxError\n---*/",
        )
        .expect("frontmatter should parse");
        assert_eq!(metadata.flags, ["onlyStrict", "generated"]);
        assert_eq!(metadata.includes, ["helper.js"]);
        assert_eq!(metadata.features, ["Promise"]);
        let negative = metadata.negative.expect("negative metadata should exist");
        assert_eq!(negative.phase, "parse");
        assert_eq!(negative.error_type, "SyntaxError");
    }

    #[test]
    fn parses_block_sequence_flags_includes_and_features() {
        let metadata = parse_metadata(
            "/*---\nflags:\n  - noStrict\n  - async\nincludes:\n  - 'propertyHelper.js'\n  - doneprintHandle.js\nfeatures:\n  - Promise\nnegative:\n  phase: runtime\n  type: TypeError\n---*/",
        )
        .expect("block sequences should parse");
        assert_eq!(metadata.flags, ["noStrict", "async"]);
        assert_eq!(
            metadata.includes,
            ["propertyHelper.js", "doneprintHandle.js"]
        );
        assert_eq!(metadata.features, ["Promise"]);
        let negative = metadata.negative.expect("negative metadata should exist");
        assert_eq!(negative.phase, "runtime");
        assert_eq!(negative.error_type, "TypeError");
    }

    #[test]
    fn rejects_partial_negative_metadata() {
        let error = parse_metadata("/*---\nnegative:\n  phase: runtime\n---*/")
            .expect_err("negative metadata needs phase and type");
        assert!(error.contains("both phase and type"));
    }

    #[test]
    fn parse_negative_is_classified_before_harness_execution() {
        let metadata = parse_metadata(
            "/*---\nflags: [onlyStrict]\nnegative:\n  phase: parse\n  type: SyntaxError\n---*/",
        )
        .expect("frontmatter should parse");
        let record = run_variant(
            "synthetic/parse-negative.js",
            "var public = 1;",
            &metadata,
            "strict",
            true,
        );
        assert_eq!(record.status, Status::Pass);
        assert!(record.detail.contains("matched negative parse:SyntaxError"));
    }

    #[test]
    fn official_default_harness_compiles_when_checkout_is_present() {
        let Ok(sources) = super::default_harness_sources() else {
            eprintln!("third_party/test262 is absent; skipping harness compile smoke test");
            return;
        };
        for source in sources {
            CompiledScript::compile(&source, &RuntimeLimits::default())
                .expect("official default harness must compile");
        }
    }

    #[test]
    fn classifies_blocking_agent_tests_as_unsupported_on_a_browser_main_thread() {
        // The classification reads the `CanBlockIsTrue` flag, so a synthetic
        // source exercises it without requiring the pinned test262 checkout.
        let records = super::run_source_case(
            "synthetic/blocking-agent.js",
            "/*---\nflags: [CanBlockIsTrue]\n---*/\n0;",
        );
        assert_eq!(records[0].status, super::Status::Unsupported);
    }

    #[test]
    fn runs_module_tests_instead_of_classifying_them_unsupported() {
        // Module-flagged files run as module graphs (ECMA-262 16.2.1), so the
        // runner judges them rather than skipping them. A parse-negative module
        // keeps this test independent of the pinned checkout's file contents.
        let records = super::run_source_case(
            "synthetic/module-parse-negative.js",
            "/*---\nflags: [module]\nnegative:\n  phase: parse\n  type: SyntaxError\n---*/\nexport default class {",
        );
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, super::Status::Pass);
        assert!(
            records[0]
                .detail
                .contains("matched negative parse:SyntaxError")
        );
    }
}
