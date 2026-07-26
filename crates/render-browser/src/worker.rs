//! Bounded, latest-write-wins scheduling for CPU rendering.
//!
//! This module deliberately knows nothing about winit or native surfaces. A
//! browser can use the wake callback to send an `EventLoopProxy` user event,
//! then drain completed CPU frames without polling on a timer.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;

const MAX_WORKERS: usize = 4;
const MAX_QUEUE_CAPACITY: usize = 64;

/// All state that makes a rendered frame safe to commit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderIdentity {
    pub tab_id: u64,
    pub generation: u64,
    pub dom_revision: u64,
    pub viewport: RenderViewport,
    pub scroll_offset: RenderOffset,
    pub external_styles_generation: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderViewport {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RenderOffset {
    pub x: f32,
    pub y: f32,
}

/// An immutable render input. Native window/surface objects must never be put
/// in this payload; only owned, `Send` snapshots cross the boundary.
#[derive(Debug)]
pub struct RenderJob<P> {
    pub identity: RenderIdentity,
    pub source_snapshot: Arc<str>,
    pub payload: P,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderFailure {
    Cancelled,
    Task(String),
    Panicked { message: String },
}

impl fmt::Display for RenderFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("render job was superseded"),
            Self::Task(message) => write!(formatter, "render job failed: {message}"),
            Self::Panicked { message } => write!(formatter, "render worker panicked: {message}"),
        }
    }
}

impl std::error::Error for RenderFailure {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderSubmitError {
    WorkerStopped,
}

impl fmt::Display for RenderSubmitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkerStopped => formatter.write_str("render worker has stopped"),
        }
    }
}

impl std::error::Error for RenderSubmitError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderWorkerStartError {
    ZeroQueueCapacity,
    QueueCapacityTooLarge,
    ZeroWorkers,
    TooManyWorkers,
    ThreadSpawn,
}

impl fmt::Display for RenderWorkerStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ZeroQueueCapacity => "render queue capacity must be non-zero",
            Self::QueueCapacityTooLarge => "render queue capacity exceeds its resource limit",
            Self::ZeroWorkers => "render worker count must be non-zero",
            Self::TooManyWorkers => "render worker count exceeds its resource limit",
            Self::ThreadSpawn => "could not spawn a render worker thread",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for RenderWorkerStartError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderWorkerOptions {
    pub queue_capacity: usize,
    pub worker_count: usize,
}

impl Default for RenderWorkerOptions {
    fn default() -> Self {
        Self {
            queue_capacity: 8,
            worker_count: 2,
        }
    }
}

#[derive(Debug)]
pub struct CompletedRender<R> {
    pub identity: RenderIdentity,
    pub result: Result<R, RenderFailure>,
}

/// A cheap cancellation flag checked by cooperative render stages.
#[derive(Clone, Debug)]
pub struct RenderCancellation {
    cancelled: Arc<AtomicBool>,
}

impl RenderCancellation {
    fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Fail the current cooperative stage when a newer identity superseded it.
    ///
    /// # Errors
    ///
    /// Returns [`RenderFailure::Cancelled`] after cancellation is requested.
    pub fn check(&self) -> Result<(), RenderFailure> {
        if self.is_cancelled() {
            Err(RenderFailure::Cancelled)
        } else {
            Ok(())
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

struct Queued<P> {
    job: RenderJob<P>,
    cancellation: RenderCancellation,
}

struct QueueState<P> {
    jobs: VecDeque<Queued<P>>,
    active: HashMap<u64, Vec<RenderCancellation>>,
    stopped: bool,
}

struct Shared<P, R> {
    queue: Mutex<QueueState<P>>,
    available: Condvar,
    completed: Mutex<VecDeque<CompletedRender<R>>>,
    latest: Mutex<HashMap<u64, RenderIdentity>>,
    queue_capacity: usize,
}

/// Owns a finite render pool and non-blocking producer/completion queues.
pub struct RenderWorker<P, R> {
    shared: Arc<Shared<P, R>>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl<P, R> fmt::Debug for RenderWorker<P, R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderWorker")
            .field("thread_count", &self.threads.len())
            .field("queue_capacity", &self.shared.queue_capacity)
            .finish_non_exhaustive()
    }
}

impl<P, R> RenderWorker<P, R>
where
    P: Send + 'static,
    R: Send + 'static,
{
    /// Start a finite render pool and its bounded queues.
    ///
    /// # Errors
    ///
    /// Returns a typed configuration or thread-spawn error.
    pub fn start<F, W>(
        options: RenderWorkerOptions,
        process: F,
        wake: W,
    ) -> Result<Self, RenderWorkerStartError>
    where
        F: Fn(RenderJob<P>, &RenderCancellation) -> Result<R, RenderFailure>
            + Send
            + Sync
            + 'static,
        W: Fn() + Send + Sync + 'static,
    {
        validate_options(options)?;
        let shared = Arc::new(Shared {
            queue: Mutex::new(QueueState {
                jobs: VecDeque::with_capacity(options.queue_capacity),
                active: HashMap::new(),
                stopped: false,
            }),
            available: Condvar::new(),
            completed: Mutex::new(VecDeque::with_capacity(
                options.queue_capacity.saturating_add(options.worker_count),
            )),
            latest: Mutex::new(HashMap::new()),
            queue_capacity: options.queue_capacity,
        });
        let process = Arc::new(process);
        let wake = Arc::new(wake);
        let mut threads: Vec<thread::JoinHandle<()>> = Vec::with_capacity(options.worker_count);
        for index in 0..options.worker_count {
            let thread_shared = Arc::clone(&shared);
            let thread_process = Arc::clone(&process);
            let thread_wake = Arc::clone(&wake);
            let spawn = thread::Builder::new()
                .name(format!("render-worker-{index}"))
                .spawn(move || worker_loop(thread_shared, thread_process, thread_wake));
            let Ok(handle) = spawn else {
                stop_shared(&shared);
                for handle in threads {
                    let _ = handle.join();
                }
                return Err(RenderWorkerStartError::ThreadSpawn);
            };
            threads.push(handle);
        }
        Ok(Self { shared, threads })
    }

    /// Submit without waiting for capacity. Older work for the same tab is
    /// cancelled and replaced; if all slots belong to other tabs, the oldest
    /// queued job is evicted so memory remains bounded.
    ///
    /// # Errors
    ///
    /// Returns [`RenderSubmitError::WorkerStopped`] after shutdown begins.
    pub fn submit(&self, job: RenderJob<P>) -> Result<(), RenderSubmitError> {
        let identity = job.identity;
        let cancellation = RenderCancellation::new();
        let mut queue = self
            .shared
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if queue.stopped {
            return Err(RenderSubmitError::WorkerStopped);
        }

        if let Some(active) = queue.active.get(&identity.tab_id) {
            for token in active {
                token.cancel();
            }
        }
        queue.jobs.retain(|queued| {
            if queued.job.identity.tab_id == identity.tab_id {
                queued.cancellation.cancel();
                false
            } else {
                true
            }
        });
        if queue.jobs.len() == self.shared.queue_capacity
            && let Some(evicted) = queue.jobs.pop_front()
        {
            evicted.cancellation.cancel();
        }
        self.shared
            .latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(identity.tab_id, identity);
        queue.jobs.push_back(Queued { job, cancellation });
        drop(queue);
        self.shared.available.notify_one();
        Ok(())
    }

    /// Cancel and forget all work for a closed or zero-sized tab.
    pub fn cancel_tab(&self, tab_id: u64) {
        let mut queue = self
            .shared
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(active) = queue.active.get(&tab_id) {
            for token in active {
                token.cancel();
            }
        }
        queue.jobs.retain(|queued| {
            if queued.job.identity.tab_id == tab_id {
                queued.cancellation.cancel();
                false
            } else {
                true
            }
        });
        drop(queue);
        self.shared
            .latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&tab_id);
    }

    /// Drain only commit-safe results. Superseded identities and cancelled
    /// results are discarded here, before application code can install them.
    #[must_use]
    pub fn drain_latest(&self) -> Vec<CompletedRender<R>> {
        let mut completed = self
            .shared
            .completed
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let latest = self
            .shared
            .latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        completed
            .drain(..)
            .filter(|completed| {
                latest.get(&completed.identity.tab_id) == Some(&completed.identity)
                    && !matches!(completed.result, Err(RenderFailure::Cancelled))
            })
            .collect()
    }

    /// Stop accepting new work. Running stages observe cancellation at their
    /// next cooperative check. This method never waits for expensive work.
    pub fn shutdown(&self) {
        stop_shared(&self.shared);
    }

    #[cfg(test)]
    fn queued_len(&self) -> usize {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .jobs
            .len()
    }
}

impl<P, R> Drop for RenderWorker<P, R> {
    fn drop(&mut self) {
        stop_shared(&self.shared);
        // Dropping JoinHandles detaches workers so closing the GUI never waits
        // for a currently-running layout/raster operation.
        self.threads.clear();
    }
}

fn validate_options(options: RenderWorkerOptions) -> Result<(), RenderWorkerStartError> {
    if options.queue_capacity == 0 {
        return Err(RenderWorkerStartError::ZeroQueueCapacity);
    }
    if options.queue_capacity > MAX_QUEUE_CAPACITY {
        return Err(RenderWorkerStartError::QueueCapacityTooLarge);
    }
    if options.worker_count == 0 {
        return Err(RenderWorkerStartError::ZeroWorkers);
    }
    if options.worker_count > MAX_WORKERS {
        return Err(RenderWorkerStartError::TooManyWorkers);
    }
    Ok(())
}

fn stop_shared<P, R>(shared: &Shared<P, R>) {
    let mut queue = shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
    queue.stopped = true;
    for queued in queue.jobs.drain(..) {
        queued.cancellation.cancel();
    }
    for active in queue.active.values() {
        for token in active {
            token.cancel();
        }
    }
    drop(queue);
    shared.available.notify_all();
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the worker thread owns these Arc references for its full lifetime"
)]
fn worker_loop<P, R, F, W>(shared: Arc<Shared<P, R>>, process: Arc<F>, wake: Arc<W>)
where
    P: Send + 'static,
    R: Send + 'static,
    F: Fn(RenderJob<P>, &RenderCancellation) -> Result<R, RenderFailure> + Send + Sync + 'static,
    W: Fn() + Send + Sync + 'static,
{
    loop {
        let queued = {
            let mut queue = shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if let Some(queued) = queue.jobs.pop_front() {
                    queue
                        .active
                        .entry(queued.job.identity.tab_id)
                        .or_default()
                        .push(queued.cancellation.clone());
                    break Some(queued);
                }
                if queue.stopped {
                    break None;
                }
                queue = shared
                    .available
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let Some(queued) = queued else {
            return;
        };
        let identity = queued.job.identity;
        let result = if queued.cancellation.is_cancelled() {
            Err(RenderFailure::Cancelled)
        } else {
            match catch_unwind(AssertUnwindSafe(|| {
                process(queued.job, &queued.cancellation)
            })) {
                Ok(result) => result,
                Err(payload) => Err(RenderFailure::Panicked {
                    message: panic_message(payload.as_ref()),
                }),
            }
        };
        {
            let mut queue = shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(active) = queue.active.get_mut(&identity.tab_id) {
                active
                    .retain(|token| !Arc::ptr_eq(&token.cancelled, &queued.cancellation.cancelled));
                if active.is_empty() {
                    queue.active.remove(&identity.tab_id);
                }
            }
        }
        if !queued.cancellation.is_cancelled()
            || matches!(result, Err(RenderFailure::Panicked { .. }))
        {
            let mut completed = shared
                .completed
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let completion_capacity = shared.queue_capacity.saturating_add(MAX_WORKERS);
            if completed.len() == completion_capacity {
                completed.pop_front();
            }
            completed.push_back(CompletedRender { identity, result });
            drop(completed);
            wake();
        }
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{
        RenderFailure, RenderIdentity, RenderJob, RenderOffset, RenderViewport, RenderWorker,
        RenderWorkerOptions,
    };

    fn identity(tab: u64, generation: u64, width: u32, scroll_y: f32) -> RenderIdentity {
        RenderIdentity {
            tab_id: tab,
            generation,
            dom_revision: 7,
            viewport: RenderViewport { width, height: 600 },
            scroll_offset: RenderOffset {
                x: 0.0,
                y: scroll_y,
            },
            external_styles_generation: 3,
        }
    }

    fn job<P>(identity: RenderIdentity, payload: P) -> RenderJob<P> {
        RenderJob {
            identity,
            source_snapshot: Arc::from("<html></html>"),
            payload,
        }
    }

    #[test]
    fn ui_submit_returns_while_render_is_blocked() {
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let worker = RenderWorker::start(
            RenderWorkerOptions {
                queue_capacity: 2,
                worker_count: 1,
            },
            {
                let entered = Arc::clone(&entered);
                let release = Arc::clone(&release);
                move |job: RenderJob<usize>, _| {
                    entered.wait();
                    release.wait();
                    Ok(job.payload)
                }
            },
            || {},
        )
        .expect("worker starts");

        let started = Instant::now();
        worker.submit(job(identity(1, 1, 800, 0.0), 10)).unwrap();
        assert!(started.elapsed() < Duration::from_millis(50));
        entered.wait();
        release.wait();
    }

    #[test]
    fn stale_resize_and_scroll_results_never_commit() {
        let first_entered = Arc::new(Barrier::new(2));
        let first_release = Arc::new(Barrier::new(2));
        let worker = RenderWorker::start(
            RenderWorkerOptions {
                queue_capacity: 4,
                worker_count: 1,
            },
            {
                let first_entered = Arc::clone(&first_entered);
                let first_release = Arc::clone(&first_release);
                move |job: RenderJob<usize>, _| {
                    if job.payload == 1 {
                        first_entered.wait();
                        first_release.wait();
                    }
                    Ok(job.payload)
                }
            },
            || {},
        )
        .unwrap();
        worker.submit(job(identity(1, 1, 800, 0.0), 1)).unwrap();
        first_entered.wait();
        worker.submit(job(identity(1, 2, 900, 0.0), 2)).unwrap();
        worker.submit(job(identity(1, 3, 900, 120.0), 3)).unwrap();
        first_release.wait();

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let completed = worker.drain_latest();
            if !completed.is_empty() {
                assert_eq!(completed.len(), 1);
                assert_eq!(completed[0].identity, identity(1, 3, 900, 120.0));
                assert_eq!(completed[0].result, Ok(3));
                break;
            }
            assert!(Instant::now() < deadline, "latest render did not finish");
            thread::yield_now();
        }
    }

    #[test]
    fn tabs_have_independent_latest_identities() {
        let worker = RenderWorker::start(
            RenderWorkerOptions::default(),
            |job: RenderJob<usize>, _| Ok(job.payload),
            || {},
        )
        .unwrap();
        worker.submit(job(identity(1, 1, 800, 0.0), 11)).unwrap();
        worker.submit(job(identity(2, 1, 800, 0.0), 21)).unwrap();
        worker.submit(job(identity(1, 2, 800, 50.0), 12)).unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut values = Vec::new();
        while values.len() < 2 {
            values.extend(
                worker
                    .drain_latest()
                    .into_iter()
                    .map(|completed| completed.result.unwrap()),
            );
            assert!(Instant::now() < deadline, "tab renders did not finish");
            thread::yield_now();
        }
        values.sort_unstable();
        assert_eq!(values, [12, 21]);
    }

    #[test]
    fn queue_is_bounded_and_superseded_work_is_cancelled() {
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let cancellations = Arc::new(AtomicUsize::new(0));
        let worker = RenderWorker::start(
            RenderWorkerOptions {
                queue_capacity: 2,
                worker_count: 1,
            },
            {
                let entered = Arc::clone(&entered);
                let release = Arc::clone(&release);
                let cancellations = Arc::clone(&cancellations);
                move |job: RenderJob<usize>, cancellation| {
                    if job.payload == 1 {
                        entered.wait();
                        release.wait();
                        if cancellation.is_cancelled() {
                            cancellations.fetch_add(1, Ordering::Relaxed);
                            return Err(RenderFailure::Cancelled);
                        }
                    }
                    Ok(job.payload)
                }
            },
            || {},
        )
        .unwrap();
        worker.submit(job(identity(1, 1, 800, 0.0), 1)).unwrap();
        entered.wait();
        for generation in 2_u16..20 {
            worker
                .submit(job(
                    identity(1, u64::from(generation), 800, f32::from(generation)),
                    usize::from(generation),
                ))
                .unwrap();
            assert!(worker.queued_len() <= 2);
        }
        release.wait();
        let deadline = Instant::now() + Duration::from_secs(2);
        while cancellations.load(Ordering::Relaxed) == 0 {
            assert!(
                Instant::now() < deadline,
                "active task did not observe cancellation"
            );
            thread::yield_now();
        }
    }

    #[test]
    fn worker_panics_are_typed_failures() {
        let worker = RenderWorker::start(
            RenderWorkerOptions::default(),
            |_job: RenderJob<()>, _| -> Result<(), RenderFailure> { panic!("boom") },
            || {},
        )
        .unwrap();
        worker.submit(job(identity(1, 1, 800, 0.0), ())).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(completed) = worker.drain_latest().pop() {
                assert_eq!(
                    completed.result,
                    Err(RenderFailure::Panicked {
                        message: "boom".to_owned()
                    })
                );
                break;
            }
            assert!(Instant::now() < deadline, "panic result did not arrive");
            thread::yield_now();
        }
    }
}
