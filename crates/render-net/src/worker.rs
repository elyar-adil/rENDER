use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use crate::{
    BatchOptions, CancelToken, FetchError, FetchRequest, FetchResult, HttpTransport, Origin,
};

const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(10);
const QUEUE_FULL_MESSAGE: &str = "network worker queue is full";
const PAUSED_ORIGIN_MESSAGE: &str = "per-origin concurrency policy paused this origin";
const MISSING_BATCH_RESULT_MESSAGE: &str = "batch worker ended without a result";

type OperationId = u64;

enum Command {
    Fetch {
        id: OperationId,
        permit: OperationPermit,
        request: FetchRequest,
        cancel: CancelToken,
        response: Sender<FetchResult>,
    },
    Batch {
        id: OperationId,
        permit: OperationPermit,
        requests: Vec<FetchRequest>,
        options: BatchOptions,
        cancel: CancelToken,
        response: Sender<Vec<FetchResult>>,
    },
}

impl Command {
    fn reject(self, error: FetchError) {
        match self {
            Self::Fetch { response, .. } => {
                let _ignored = response.send(Err(error));
            }
            Self::Batch {
                requests, response, ..
            } => {
                let results = requests.iter().map(|_| Err(error.clone())).collect();
                let _ignored = response.send(results);
            }
        }
    }
}

enum Event {
    Command(Box<Command>),
    Completion(Box<Completion>),
}

struct Completion {
    operation_id: OperationId,
    index: Option<usize>,
    origin: Origin,
    result: FetchResult,
}

#[derive(Debug)]
struct TransferJob {
    operation_id: OperationId,
    index: Option<usize>,
    request: FetchRequest,
    cancel: CancelToken,
    origin: Origin,
}

#[derive(Debug)]
struct JobQueue {
    capacity: usize,
    state: Mutex<JobQueueState>,
    available: Condvar,
}

#[derive(Debug)]
struct JobQueueState {
    jobs: VecDeque<Box<TransferJob>>,
    closed: bool,
}

impl JobQueue {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: Mutex::new(JobQueueState {
                jobs: VecDeque::new(),
                closed: false,
            }),
            available: Condvar::new(),
        }
    }

    fn has_capacity(&self) -> bool {
        let state = self.state.lock().expect("network job queue lock");
        !state.closed && state.jobs.len() < self.capacity
    }

    fn push(&self, job: Box<TransferJob>) -> Result<(), Box<TransferJob>> {
        let mut state = self.state.lock().expect("network job queue lock");
        if state.closed || state.jobs.len() >= self.capacity {
            return Err(job);
        }
        state.jobs.push_back(job);
        self.available.notify_one();
        Ok(())
    }

    fn pop(&self) -> Option<Box<TransferJob>> {
        let mut state = self.state.lock().expect("network job queue lock");
        loop {
            if let Some(job) = state.jobs.pop_front() {
                return Some(job);
            }
            if state.closed {
                return None;
            }
            state = self.available.wait(state).expect("network job queue lock");
        }
    }

    fn close(&self) {
        let mut state = self.state.lock().expect("network job queue lock");
        state.closed = true;
        state.jobs.clear();
        self.available.notify_all();
    }
}

#[derive(Debug)]
struct Runtime {
    jobs: Arc<JobQueue>,
    next_operation_id: AtomicU64,
    operation_capacity: usize,
    operations: AtomicU64,
}

impl Runtime {
    fn new(queue_capacity: usize) -> Self {
        Self {
            jobs: Arc::new(JobQueue::new(queue_capacity)),
            next_operation_id: AtomicU64::new(1),
            operation_capacity: queue_capacity,
            operations: AtomicU64::new(0),
        }
    }

    fn next_operation_id(&self) -> OperationId {
        self.next_operation_id.fetch_add(1, Ordering::Relaxed)
    }

    fn reserve_operation(self: &Arc<Self>) -> Option<OperationPermit> {
        let mut current = self.operations.load(Ordering::Acquire);
        loop {
            if current >= self.operation_capacity as u64 {
                return None;
            }
            match self.operations.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(OperationPermit(Arc::clone(self))),
                Err(observed) => current = observed,
            }
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.jobs.close();
    }
}

struct OperationPermit(Arc<Runtime>);

impl Drop for OperationPermit {
    fn drop(&mut self) {
        self.0.operations.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Bounds the shared execution pool used by [`NetworkWorker`].
///
/// The default reserves one logical CPU for the browser/UI thread where the
/// platform reports more than one CPU, while capping network threads at 16.
/// Both the command and transfer queues use `queue_capacity` so callers never
/// create an unbounded backlog behind stalled connections.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetworkWorkerConfig {
    pub worker_count: usize,
    pub queue_capacity: usize,
}

impl Default for NetworkWorkerConfig {
    fn default() -> Self {
        let worker_count = thread::available_parallelism()
            .map_or(1, |parallelism| parallelism.get().saturating_sub(1).max(1))
            .min(16);
        Self {
            worker_count,
            queue_capacity: worker_count.saturating_mul(8).clamp(16, 128),
        }
    }
}

/// Background transport dispatcher backed by a bounded shared worker pool.
/// Submitting work never performs network I/O on the caller's thread.
#[derive(Clone, Debug)]
pub struct NetworkWorker {
    events: SyncSender<Event>,
    runtime: Arc<Runtime>,
}

impl NetworkWorker {
    /// Starts a worker using [`NetworkWorkerConfig::default`].
    ///
    /// # Errors
    ///
    /// Returns the operating-system thread creation error when the dispatcher
    /// or an execution worker cannot be started.
    pub fn start(transport: HttpTransport) -> io::Result<Self> {
        Self::start_with_config(transport, NetworkWorkerConfig::default())
    }

    /// Starts a worker with an explicit bounded pool configuration.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error for zero limits, or an operating-system
    /// thread creation error when a worker cannot be started.
    pub fn start_with_config(
        transport: HttpTransport,
        config: NetworkWorkerConfig,
    ) -> io::Result<Self> {
        if config.worker_count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "network worker count must be non-zero",
            ));
        }
        if config.queue_capacity == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "network worker queue capacity must be non-zero",
            ));
        }

        let transport = Arc::new(transport);
        let runtime = Arc::new(Runtime::new(config.queue_capacity));
        let (events, receiver) = mpsc::sync_channel(config.queue_capacity);
        let dispatcher_jobs = Arc::clone(&runtime.jobs);
        thread::Builder::new()
            .name("render-net-dispatch".into())
            .spawn(move || {
                Dispatcher {
                    receiver,
                    jobs: dispatcher_jobs,
                }
                .run();
            })?;

        for index in 0..config.worker_count {
            let jobs = Arc::clone(&runtime.jobs);
            let events = events.clone();
            let transport = Arc::clone(&transport);
            if let Err(error) = thread::Builder::new()
                .name(format!("render-net-worker-{index}"))
                .spawn(move || {
                    TransferWorker {
                        transport,
                        jobs,
                        events,
                    }
                    .run();
                })
            {
                runtime.jobs.close();
                return Err(error);
            }
        }

        Ok(Self { events, runtime })
    }

    /// Queues one GET and immediately returns its typed result handle.
    #[must_use]
    pub fn submit(&self, request: FetchRequest) -> RequestHandle<FetchResult> {
        let (response, receiver) = mpsc::channel();
        let cancel = CancelToken::default();
        let Some(permit) = self.runtime.reserve_operation() else {
            let _ignored = response.send(Err(FetchError::Transport(QUEUE_FULL_MESSAGE.into())));
            return RequestHandle { receiver, cancel };
        };
        let command = Command::Fetch {
            id: self.runtime.next_operation_id(),
            permit,
            request,
            cancel: cancel.clone(),
            response,
        };
        self.enqueue(command);
        RequestHandle { receiver, cancel }
    }

    /// Queues an ordered parallel batch and immediately returns its handle.
    #[must_use]
    pub fn submit_batch(
        &self,
        requests: Vec<FetchRequest>,
        options: BatchOptions,
    ) -> RequestHandle<Vec<FetchResult>> {
        let (response, receiver) = mpsc::channel();
        let cancel = CancelToken::default();
        let Some(permit) = self.runtime.reserve_operation() else {
            let results = requests
                .iter()
                .map(|_| Err(FetchError::Transport(QUEUE_FULL_MESSAGE.into())))
                .collect();
            let _ignored = response.send(results);
            return RequestHandle { receiver, cancel };
        };
        let command = Command::Batch {
            id: self.runtime.next_operation_id(),
            permit,
            requests,
            options,
            cancel: cancel.clone(),
            response,
        };
        self.enqueue(command);
        RequestHandle { receiver, cancel }
    }

    fn enqueue(&self, command: Command) {
        match self.events.try_send(Event::Command(Box::new(command))) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(Event::Command(command))) => {
                command.reject(FetchError::Transport(QUEUE_FULL_MESSAGE.into()));
            }
            Err(mpsc::TrySendError::Disconnected(Event::Command(command))) => {
                command.reject(FetchError::WorkerStopped);
            }
            Err(
                mpsc::TrySendError::Full(Event::Completion(_))
                | mpsc::TrySendError::Disconnected(Event::Completion(_)),
            ) => {
                unreachable!("only commands are submitted by callers")
            }
        }
    }
}

/// Typed response channel plus cooperative cancellation.
#[derive(Debug)]
pub struct RequestHandle<T> {
    receiver: Receiver<T>,
    cancel: CancelToken,
}

impl<T> RequestHandle<T> {
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    #[must_use]
    pub fn cancellation_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Attempts to receive without blocking.
    ///
    /// # Errors
    ///
    /// Returns [`TryRecvError::Empty`] while work is in progress, or
    /// [`TryRecvError::Disconnected`] if the response producer stopped.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.receiver.try_recv()
    }

    /// Waits up to `timeout` for the typed response.
    ///
    /// # Errors
    ///
    /// Returns [`RecvTimeoutError::Timeout`] if the deadline expires, or
    /// [`RecvTimeoutError::Disconnected`] if the response producer stopped.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }

    /// Waits for the typed response.
    ///
    /// # Errors
    ///
    /// Returns [`mpsc::RecvError`] only if the response producer stopped
    /// without sending a value.
    pub fn recv(self) -> Result<T, mpsc::RecvError> {
        self.receiver.recv()
    }
}

enum Operation {
    Fetch(FetchOperation),
    Batch(BatchOperation),
}

impl Operation {
    fn is_cancelled(&self) -> bool {
        match self {
            Self::Fetch(operation) => operation.cancel.is_cancelled(),
            Self::Batch(operation) => operation.cancel.is_cancelled(),
        }
    }

    fn cancel(self) {
        match self {
            Self::Fetch(operation) => {
                let _ignored = operation.response.send(Err(FetchError::Cancelled));
            }
            Self::Batch(operation) => {
                let results = std::iter::repeat_with(|| Err(FetchError::Cancelled))
                    .take(operation.results.len())
                    .collect();
                let _ignored = operation.response.send(results);
            }
        }
    }

    fn stop(self) {
        match self {
            Self::Fetch(operation) => {
                let _ignored = operation.response.send(Err(FetchError::WorkerStopped));
            }
            Self::Batch(operation) => {
                let results = std::iter::repeat_with(|| Err(FetchError::WorkerStopped))
                    .take(operation.results.len())
                    .collect();
                let _ignored = operation.response.send(results);
            }
        }
    }
}

struct FetchOperation {
    _permit: OperationPermit,
    request: FetchRequest,
    cancel: CancelToken,
    response: Sender<FetchResult>,
    scheduled: bool,
}

struct BatchOperation {
    _permit: OperationPermit,
    pending: VecDeque<(usize, FetchRequest)>,
    results: Vec<Option<FetchResult>>,
    active_by_origin: HashMap<Origin, usize>,
    active: usize,
    options: BatchOptions,
    cancel: CancelToken,
    response: Sender<Vec<FetchResult>>,
}

impl BatchOperation {
    fn new(
        permit: OperationPermit,
        requests: Vec<FetchRequest>,
        options: BatchOptions,
        cancel: CancelToken,
        response: Sender<Vec<FetchResult>>,
    ) -> Self {
        let result_len = requests.len();
        Self {
            _permit: permit,
            pending: requests.into_iter().enumerate().collect(),
            results: std::iter::repeat_with(|| None).take(result_len).collect(),
            active_by_origin: HashMap::new(),
            active: 0,
            options,
            cancel,
            response,
        }
    }

    fn next_job(&mut self, operation_id: OperationId) -> BatchSchedule {
        if self.options.max_concurrency == 0 {
            for result in &mut self.results {
                *result = Some(Err(FetchError::Transport(
                    "batch concurrency must be non-zero".into(),
                )));
            }
            self.pending.clear();
            return BatchSchedule::Finish;
        }
        if self.pending.is_empty() {
            return if self.active == 0 {
                BatchSchedule::Finish
            } else {
                BatchSchedule::Wait
            };
        }
        if self.active >= self.options.max_concurrency {
            return BatchSchedule::Wait;
        }
        let Some(position) = self.pending.iter().position(|(_, request)| {
            let origin = Origin::from_url(&request.url);
            let current = self
                .active_by_origin
                .get(&origin)
                .copied()
                .unwrap_or_default();
            current < self.options.origin_policy.max_concurrency(&origin)
        }) else {
            if self.active == 0 {
                for (index, _) in self.pending.drain(..) {
                    self.results[index] =
                        Some(Err(FetchError::Transport(PAUSED_ORIGIN_MESSAGE.into())));
                }
                return BatchSchedule::Finish;
            }
            return BatchSchedule::Wait;
        };

        let (index, request) = self
            .pending
            .remove(position)
            .expect("eligible batch request remains pending");
        let origin = Origin::from_url(&request.url);
        *self.active_by_origin.entry(origin.clone()).or_default() += 1;
        self.active += 1;
        let can_schedule_more =
            self.active < self.options.max_concurrency && !self.pending.is_empty();
        BatchSchedule::Job(
            Box::new(TransferJob {
                operation_id,
                index: Some(index),
                request,
                cancel: self.cancel.clone(),
                origin,
            }),
            can_schedule_more,
        )
    }

    fn complete(&mut self, index: usize, origin: &Origin, result: FetchResult) -> bool {
        if let Some(slot) = self.results.get_mut(index)
            && slot.is_none()
        {
            *slot = Some(result);
            self.active = self.active.saturating_sub(1);
            if let Some(count) = self.active_by_origin.get_mut(origin) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.active_by_origin.remove(origin);
                }
            }
        }
        self.pending.is_empty() && self.active == 0
    }

    fn finish(self) {
        let results = self
            .results
            .into_iter()
            .map(|result| {
                result.unwrap_or_else(|| {
                    Err(FetchError::Transport(MISSING_BATCH_RESULT_MESSAGE.into()))
                })
            })
            .collect();
        let _ignored = self.response.send(results);
    }
}

enum BatchSchedule {
    Job(Box<TransferJob>, bool),
    Wait,
    Finish,
}

#[derive(Default)]
struct Scheduler {
    operations: HashMap<OperationId, Operation>,
    ready: VecDeque<OperationId>,
}

impl Scheduler {
    fn accept(&mut self, command: Command) {
        match command {
            Command::Fetch {
                id,
                permit,
                request,
                cancel,
                response,
            } => {
                if cancel.is_cancelled() {
                    let _ignored = response.send(Err(FetchError::Cancelled));
                    return;
                }
                self.operations.insert(
                    id,
                    Operation::Fetch(FetchOperation {
                        _permit: permit,
                        request,
                        cancel,
                        response,
                        scheduled: false,
                    }),
                );
                self.ready.push_back(id);
            }
            Command::Batch {
                id,
                permit,
                requests,
                options,
                cancel,
                response,
            } => {
                if cancel.is_cancelled() {
                    let results = requests
                        .iter()
                        .map(|_| Err(FetchError::Cancelled))
                        .collect();
                    let _ignored = response.send(results);
                    return;
                }
                if requests.is_empty() {
                    let _ignored = response.send(Vec::new());
                    return;
                }
                self.operations.insert(
                    id,
                    Operation::Batch(BatchOperation::new(
                        permit, requests, options, cancel, response,
                    )),
                );
                self.ready.push_back(id);
            }
        }
    }

    fn cancel_cancelled(&mut self) {
        let cancelled = self
            .operations
            .iter()
            .filter_map(|(id, operation)| operation.is_cancelled().then_some(*id))
            .collect::<Vec<_>>();
        for id in cancelled {
            if let Some(operation) = self.operations.remove(&id) {
                operation.cancel();
            }
        }
        self.ready.retain(|id| self.operations.contains_key(id));
    }

    fn schedule(&mut self, jobs: &JobQueue) -> bool {
        loop {
            if !jobs.has_capacity() {
                return true;
            }
            let Some(id) = self.ready.pop_front() else {
                return true;
            };
            let Some(operation) = self.operations.get_mut(&id) else {
                continue;
            };
            match operation {
                Operation::Fetch(fetch) => {
                    if fetch.scheduled {
                        continue;
                    }
                    fetch.scheduled = true;
                    let origin = Origin::from_url(&fetch.request.url);
                    let job = Box::new(TransferJob {
                        operation_id: id,
                        index: None,
                        request: fetch.request.clone(),
                        cancel: fetch.cancel.clone(),
                        origin,
                    });
                    if jobs.push(job).is_err() {
                        return false;
                    }
                }
                Operation::Batch(batch) => match batch.next_job(id) {
                    BatchSchedule::Job(job, can_schedule_more) => {
                        if jobs.push(job).is_err() {
                            return false;
                        }
                        if can_schedule_more {
                            self.ready.push_back(id);
                        }
                    }
                    BatchSchedule::Wait => {}
                    BatchSchedule::Finish => {
                        let Some(Operation::Batch(batch)) = self.operations.remove(&id) else {
                            continue;
                        };
                        batch.finish();
                    }
                },
            }
        }
    }

    fn complete(&mut self, completion: Completion) {
        let Some(operation) = self.operations.get_mut(&completion.operation_id) else {
            return;
        };
        match operation {
            Operation::Fetch(_) => {
                let Some(Operation::Fetch(operation)) =
                    self.operations.remove(&completion.operation_id)
                else {
                    return;
                };
                let _ignored = operation.response.send(completion.result);
            }
            Operation::Batch(batch) => {
                let Some(index) = completion.index else {
                    return;
                };
                let complete = batch.complete(index, &completion.origin, completion.result);
                if complete {
                    let Some(Operation::Batch(batch)) =
                        self.operations.remove(&completion.operation_id)
                    else {
                        return;
                    };
                    batch.finish();
                } else if !batch.pending.is_empty() {
                    self.ready.push_back(completion.operation_id);
                }
            }
        }
    }

    fn stop_all(&mut self) {
        for (_, operation) in self.operations.drain() {
            operation.stop();
        }
        self.ready.clear();
    }
}

struct Dispatcher {
    receiver: Receiver<Event>,
    jobs: Arc<JobQueue>,
}

impl Dispatcher {
    fn run(self) {
        let Self { receiver, jobs } = self;
        let mut scheduler = Scheduler::default();
        loop {
            scheduler.cancel_cancelled();
            if !scheduler.schedule(&jobs) {
                scheduler.stop_all();
                jobs.close();
                return;
            }
            match receiver.recv_timeout(CANCELLATION_POLL_INTERVAL) {
                Ok(Event::Command(command)) => scheduler.accept(*command),
                Ok(Event::Completion(completion)) => scheduler.complete(*completion),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    scheduler.stop_all();
                    jobs.close();
                    return;
                }
            }
        }
    }
}

struct TransferWorker {
    transport: Arc<HttpTransport>,
    jobs: Arc<JobQueue>,
    events: SyncSender<Event>,
}

impl TransferWorker {
    fn run(self) {
        let Self {
            transport,
            jobs,
            events,
        } = self;
        while let Some(job) = jobs.pop() {
            let result = if job.cancel.is_cancelled() {
                Err(FetchError::Cancelled)
            } else {
                transport.fetch(&job.request, &job.cancel)
            };
            let completion = Completion {
                operation_id: job.operation_id,
                index: job.index,
                origin: job.origin.clone(),
                result,
            };
            if events
                .send(Event::Completion(Box::new(completion)))
                .is_err()
            {
                return;
            }
        }
    }
}
