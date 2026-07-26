use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, mpsc};
use std::thread;

use url::Url;

use crate::{CancelToken, FetchError, FetchRequest, FetchResult, HttpTransport};

/// Network origin used only for transport concurrency accounting.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Origin {
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
}

impl Origin {
    #[must_use]
    pub fn from_url(url: &Url) -> Self {
        Self {
            scheme: url.scheme().to_ascii_lowercase(),
            host: url.host_str().unwrap_or_default().to_ascii_lowercase(),
            port: url.port_or_known_default(),
        }
    }
}

/// Hook for browser policy to cap simultaneous transfers per origin.
/// Returning zero pauses that origin until another policy is supplied.
pub trait OriginConcurrencyPolicy: fmt::Debug + Send + Sync + 'static {
    fn max_concurrency(&self, origin: &Origin) -> usize;
}

/// The same concurrency cap for every origin.
#[derive(Clone, Copy, Debug)]
pub struct FixedOriginLimit(pub usize);

impl OriginConcurrencyPolicy for FixedOriginLimit {
    fn max_concurrency(&self, _origin: &Origin) -> usize {
        self.0
    }
}

/// Parallel batch limits. Results always retain request input order.
#[derive(Clone, Debug)]
pub struct BatchOptions {
    pub max_concurrency: usize,
    pub origin_policy: Arc<dyn OriginConcurrencyPolicy>,
}

impl Default for BatchOptions {
    fn default() -> Self {
        Self {
            max_concurrency: 8,
            origin_policy: Arc::new(FixedOriginLimit(6)),
        }
    }
}

impl HttpTransport {
    /// Loads resources concurrently while preserving input order. This method
    /// blocks its current network thread; [`crate::NetworkWorker`] runs it in
    /// the background for GUI/event-loop callers.
    #[must_use]
    pub fn fetch_batch(
        &self,
        requests: Vec<FetchRequest>,
        options: &BatchOptions,
        cancel: &CancelToken,
    ) -> Vec<FetchResult> {
        if requests.is_empty() {
            return Vec::new();
        }
        if options.max_concurrency == 0 {
            return requests
                .iter()
                .map(|_| {
                    Err(FetchError::Transport(
                        "batch concurrency must be non-zero".into(),
                    ))
                })
                .collect();
        }

        let result_len = requests.len();
        let mut pending = requests.into_iter().enumerate().collect::<VecDeque<_>>();
        let mut results = std::iter::repeat_with(|| None)
            .take(result_len)
            .collect::<Vec<Option<FetchResult>>>();
        let mut active_by_origin = HashMap::<Origin, usize>::new();
        let mut active = 0_usize;
        let (completion_tx, completion_rx) = mpsc::channel();

        while pending.len() + active > 0 {
            if cancel.is_cancelled() {
                return cancelled_results(result_len);
            }

            while active < options.max_concurrency {
                let Some(position) = next_eligible(&pending, &active_by_origin, options) else {
                    break;
                };
                let Some((index, request)) = pending.remove(position) else {
                    break;
                };
                let origin = Origin::from_url(&request.url);
                *active_by_origin.entry(origin.clone()).or_default() += 1;
                active += 1;
                let transport = self.clone();
                let child_cancel = cancel.clone();
                let child_tx = completion_tx.clone();
                thread::spawn(move || {
                    let result = transport.fetch(&request, &child_cancel);
                    let _ignored = child_tx.send((index, origin, result));
                });
            }

            if active == 0 {
                // Every pending origin is paused by policy (limit zero).
                for (index, _) in pending.drain(..) {
                    results[index] = Some(Err(FetchError::Transport(
                        "per-origin concurrency policy paused this origin".into(),
                    )));
                }
                break;
            }

            match completion_rx.recv_timeout(std::time::Duration::from_millis(10)) {
                Ok((index, origin, result)) => {
                    results[index] = Some(result);
                    active = active.saturating_sub(1);
                    if let Some(count) = active_by_origin.get_mut(&origin) {
                        *count = count.saturating_sub(1);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        results
            .into_iter()
            .map(|result| {
                result.unwrap_or_else(|| {
                    Err(FetchError::Transport(
                        "batch worker ended without a result".into(),
                    ))
                })
            })
            .collect()
    }
}

fn next_eligible(
    pending: &VecDeque<(usize, FetchRequest)>,
    active: &HashMap<Origin, usize>,
    options: &BatchOptions,
) -> Option<usize> {
    pending.iter().position(|(_, request)| {
        let origin = Origin::from_url(&request.url);
        let current = active.get(&origin).copied().unwrap_or_default();
        current < options.origin_policy.max_concurrency(&origin)
    })
}

fn cancelled_results(count: usize) -> Vec<FetchResult> {
    std::iter::repeat_with(|| Err(FetchError::Cancelled))
        .take(count)
        .collect()
}
