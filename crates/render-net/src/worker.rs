use std::io;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use crate::{BatchOptions, CancelToken, FetchError, FetchRequest, FetchResult, HttpTransport};

enum Command {
    Fetch {
        request: FetchRequest,
        cancel: CancelToken,
        response: Sender<FetchResult>,
    },
    Batch {
        requests: Vec<FetchRequest>,
        options: BatchOptions,
        cancel: CancelToken,
        response: Sender<Vec<FetchResult>>,
    },
}

/// Background transport dispatcher. Submitting work never performs network I/O
/// on the caller's thread.
#[derive(Clone, Debug)]
pub struct NetworkWorker {
    commands: Sender<Command>,
}

impl NetworkWorker {
    /// Starts the dispatcher thread.
    ///
    /// # Errors
    ///
    /// Returns the operating-system thread creation error when the dispatcher
    /// cannot be started.
    pub fn start(transport: HttpTransport) -> io::Result<Self> {
        let (commands, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("render-net-dispatch".into())
            .spawn(move || dispatch(&transport, &receiver))?;
        Ok(Self { commands })
    }

    /// Queues one GET and immediately returns its typed result handle.
    #[must_use]
    pub fn submit(&self, request: FetchRequest) -> RequestHandle<FetchResult> {
        let (response, receiver) = mpsc::channel();
        let cancel = CancelToken::default();
        let command = Command::Fetch {
            request,
            cancel: cancel.clone(),
            response,
        };
        if let Err(error) = self.commands.send(command)
            && let Command::Fetch { response, .. } = error.0
        {
            let _ignored = response.send(Err(FetchError::WorkerStopped));
        }
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
        let command = Command::Batch {
            requests,
            options,
            cancel: cancel.clone(),
            response,
        };
        if let Err(error) = self.commands.send(command)
            && let Command::Batch {
                requests, response, ..
            } = error.0
        {
            let stopped = requests
                .iter()
                .map(|_| Err(FetchError::WorkerStopped))
                .collect();
            let _ignored = response.send(stopped);
        }
        RequestHandle { receiver, cancel }
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
    /// [`RecvTimeoutError::Disconnected`] if the producer stopped.
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

fn dispatch(transport: &HttpTransport, receiver: &Receiver<Command>) {
    while let Ok(command) = receiver.recv() {
        match command {
            Command::Fetch {
                request,
                cancel,
                response,
            } => {
                let child_transport = transport.clone();
                spawn_response(move || {
                    let result = child_transport.fetch(&request, &cancel);
                    let _ignored = response.send(result);
                });
            }
            Command::Batch {
                requests,
                options,
                cancel,
                response,
            } => {
                let child_transport = transport.clone();
                spawn_response(move || {
                    let result = child_transport.fetch_batch(requests, &options, &cancel);
                    let _ignored = response.send(result);
                });
            }
        }
    }
}

fn spawn_response(task: impl FnOnce() + Send + 'static) {
    let _spawn_result = thread::Builder::new()
        .name("render-net-request".into())
        .spawn(task);
}
