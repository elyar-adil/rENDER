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
use std::sync::{OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use render_core::html::parse_document;
use render_core::js::{CompiledScript, JsError, JsErrorKind, JsRuntime, RuntimeLimits};

const TEST262_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/test262");
const REVISION: &str = "5ef1e5723be95296f36afb0386676fed0205869c";

static DEFAULT_HARNESS: OnceLock<Result<Vec<CompiledScript>, String>> = OnceLock::new();

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
}

#[derive(Debug, Default)]
struct Summary {
    variants: usize,
    counts: [usize; 6],
    categories: BTreeMap<String, [usize; 6]>,
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
}

#[derive(Debug)]
enum WorkerEvent {
    Line(usize, String),
    Eof(usize),
}

#[allow(clippy::too_many_lines)]
fn run_parallel(paths: &[String]) -> io::Result<Summary> {
    let worker_count = env_usize("RENDER_TEST262_WORKERS").unwrap_or_else(|| {
        thread::available_parallelism().map_or(1, |count| count.get().saturating_div(2).clamp(1, 8))
    });
    let timeout =
        Duration::from_secs(env_usize("RENDER_TEST262_TIMEOUT_SECS").unwrap_or(30) as u64);
    let run_dir = env::var_os("RENDER_TEST262_RUN_DIR").map_or_else(
        || {
            Path::new(TEST262_ROOT)
                .join("../../target/test262-runs")
                .join(format!("run-{}", std::process::id()))
        },
        PathBuf::from,
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
    for id in 0..worker_count.min(pending.len().max(1)) {
        workers.insert(id, spawn_worker(id, &sender)?);
    }
    for worker in workers.values_mut() {
        assign_next(worker, &mut pending)?;
    }

    let started = Instant::now();
    while !workers.is_empty() {
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(WorkerEvent::Line(id, line)) => {
                if let Some(worker) = workers.get_mut(&id) {
                    handle_worker_line(
                        worker,
                        &line,
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
            Ok(WorkerEvent::Eof(id)) => {
                replace_failed_worker(
                    id,
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

fn spawn_worker(id: usize, sender: &mpsc::Sender<WorkerEvent>) -> io::Result<Worker> {
    let mut child = Command::new(env::current_exe()?)
        .args([
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
            if sender.send(WorkerEvent::Line(id, line)).is_err() {
                return;
            }
        }
        let _ = sender.send(WorkerEvent::Eof(id));
    });
    Ok(Worker {
        child,
        stdin,
        current: None,
        records: Vec::new(),
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
    let Some(encoded_path) = line.strip_prefix("E\t") else {
        return Ok(());
    };
    let path = decode_field(encoded_path);
    if worker.current.as_ref().map(|(current, _)| current) != Some(&path) {
        return Err(io::Error::other("worker completed an unexpected test path"));
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
    } else if metadata
        .flags
        .iter()
        .any(|flag| flag == "module" || flag == "async")
    {
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
        } else if path.extension().is_some_and(|extension| extension == "js") {
            if let Ok(relative) = path.strip_prefix(Path::new(TEST262_ROOT).join("test")) {
                paths.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
}

fn run_manifest_case(relative_path: &str) -> Vec<ResultRecord> {
    let path = Path::new(TEST262_ROOT).join("test").join(relative_path);
    let source = match fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) => {
            if relative_path.starts_with("language/module-code/") {
                return unsupported(relative_path, "module evaluation is not implemented");
            }
            return vec![ResultRecord {
                path: relative_path.to_owned(),
                variant: "manifest".to_owned(),
                status: Status::Fail,
                detail: format!("cannot read official test: {error}"),
            }];
        }
    };
    let metadata = match parse_metadata(&source) {
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
    if metadata.flags.iter().any(|flag| flag == "module") {
        return unsupported(relative_path, "module evaluation is not implemented");
    }
    if metadata.flags.iter().any(|flag| flag == "async") {
        return unsupported(relative_path, "async print completion is not implemented");
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
        .map(|(variant, strict)| run_variant(relative_path, &source, &metadata, variant, strict))
        .collect()
}

fn run_variant(
    relative_path: &str,
    source: &str,
    metadata: &Metadata,
    variant: &str,
    strict: bool,
) -> ResultRecord {
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
    let mut runtime = JsRuntime::new(&document.dom);
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

fn default_harness_sources() -> io::Result<Vec<String>> {
    ["assert.js", "sta.js"]
        .into_iter()
        .map(|name| fs::read_to_string(Path::new(TEST262_ROOT).join("harness").join(name)))
        .collect()
}

fn install_harness(
    runtime: &mut JsRuntime,
    dom: &mut render_core::dom::Dom,
    includes: &[String],
) -> Result<(), String> {
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
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line == "---" {
            continue;
        }
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
        if !line.starts_with(' ') && !line.starts_with('-') {
            in_negative = false;
        }
        if line.starts_with("flags:") {
            metadata.flags = parse_list(&value_after_colon(line));
        } else if line.starts_with("includes:") {
            metadata.includes = parse_list(&value_after_colon(line));
        } else if line.starts_with("features:") {
            metadata.features = parse_list(&value_after_colon(line));
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
    use render_core::js::{CompiledScript, RuntimeLimits};

    use super::{Status, parse_metadata, path_matches_prefix, run_manifest_case, run_variant};

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
    fn classifies_module_tests_as_unsupported() {
        let records = run_manifest_case("language/module-code/eval-export-dflt-cls-anon-semi.js");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, Status::Unsupported);
    }
}
