use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use url::Url;

use crate::diagnostics::FetchPhase;
use crate::{CancelToken, FetchError, FetchRequest, FetchResult, HttpTransport};

/// The exact error message a batch whose transfer threads all died carries.
const MISSING_BATCH_RESULT_MESSAGE: &str = "batch worker ended without a result";

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

/// Requests one origin may have in flight at once.
///
/// This is the per-origin limit browsers apply to HTTP/1.1, and it is the number
/// the transport's idle-connection ceiling is derived from: keeping fewer idle
/// connections than this throws away sockets the next request could have reused,
/// and keeping more holds sockets that could never be reused. Both
/// [`BatchOptions::default`] and [`FetchConfig::idle_connections_per_origin`]
/// read it, so the two cannot drift.
pub const DEFAULT_PER_ORIGIN_CONCURRENCY: usize = 6;

/// How many origins' worth of idle connections the pool keeps in total.
///
/// A page's main origin plus its CDNs is the shape that matters, so the total
/// ceiling is a small multiple of the per-origin one rather than a number
/// picked for its own sake. See [`FetchConfig::idle_connections_total`].
pub const ORIGINS_BEFORE_TOTAL_CEILING: usize = 4;

/// Parallel batch limits. Results always retain request input order.
#[derive(Clone, Debug)]
pub struct BatchOptions {
    pub max_concurrency: usize,
    pub origin_policy: Arc<dyn OriginConcurrencyPolicy>,
    /// Whole-batch budget, measured from the first scheduled request. Zero
    /// disables the bound.
    ///
    /// A batch is only useful to a page once it is finished, so waiting for the
    /// slowest request in a batch of resources that are all stalling is worse
    /// than reporting the ones that are not going to arrive: the caller gets a
    /// typed [`FetchError::Failed`] in [`FetchPhase::Queued`](crate::FetchPhase::Queued)
    /// for every request that had not completed, carrying the time the batch ran
    /// for, and the completed results with it. Without this, a batch whose
    /// resources all stall reports nothing at all and the page waits forever.
    pub timeout: Duration,
}

impl Default for BatchOptions {
    fn default() -> Self {
        Self {
            // A page cannot have more than `DEFAULT_PER_ORIGIN_CONCURRENCY`
            // requests in flight against one origin over HTTP/1.1, so a wider
            // overall window only queues work the origin limit will hold back.
            max_concurrency: DEFAULT_PER_ORIGIN_CONCURRENCY + 2,
            origin_policy: Arc::new(FixedOriginLimit(DEFAULT_PER_ORIGIN_CONCURRENCY)),
            timeout: Duration::from_secs(30),
        }
    }
}

impl HttpTransport {
    /// Loads resources concurrently while preserving input order. This method
    /// blocks its current network thread; [`crate::NetworkWorker`] runs it in
    /// the background for GUI/event-loop callers.
    ///
    /// Returns when every request has finished or [`BatchOptions::timeout`]
    /// has elapsed, whichever comes first. Requests still in flight when the
    /// budget expires are reported as failures rather than waited for, and are
    /// left to finish on their own threads.
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
        let started = Instant::now();
        let bounded = options.timeout > Duration::ZERO;

        while pending.len() + active > 0 {
            if cancel.is_cancelled() {
                return cancelled_results(result_len);
            }
            if bounded && started.elapsed() >= options.timeout {
                break;
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
                    // Reached either because the batch budget expired (the loop
                    // breaks on it) or because a transfer thread died without
                    // reporting; the elapsed time tells the two apart.
                    let elapsed = started.elapsed();
                    if bounded && elapsed >= options.timeout {
                        Err(FetchError::Timeout.in_phase(FetchPhase::Queued, elapsed))
                    } else {
                        Err(FetchError::Transport(MISSING_BATCH_RESULT_MESSAGE.into()))
                    }
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
