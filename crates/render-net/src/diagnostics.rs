//! Terminal-outcome reporting for the transport.
//!
//! Every request that reaches the transport ends in exactly one of two
//! outcomes, and both are reported here: a response, or a
//! [`FetchError`] that names the [`FetchPhase`] it failed in and how long it
//! had been running. A request that stalls used to be indistinguishable from a
//! fast one because nothing was logged and the error carried no timing; the
//! default [`StderrObserver`] closes that gap without any call-site change.

use std::fmt;
use std::time::Duration;

use url::Url;

use crate::{FetchError, HttpMethod, HttpStatus};

/// The transport phase a request had reached when it reached a terminal
/// outcome.
///
/// A phase is the last one known to have been in progress. It is exact for
/// timeouts, because ureq reports which of its own phase budgets expired, and
/// best-effort for other failures: see
/// [`FetchError::Failed`](crate::FetchError::Failed).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum FetchPhase {
    /// Before any network work, or a failure ureq does not attribute to a
    /// network phase: request validation, URL handling, and `data:` decoding.
    Request,
    /// Waiting for its turn in a batch, or for the batch's own budget. A
    /// request that never got a transfer slot did not stall in the network.
    Queued,
    /// Host name resolution.
    Dns,
    /// Opening the TCP connection to a resolved address. ureq's connect phase
    /// covers the socket setup and its per-address fallback, so this also
    /// covers "this address never answered".
    TcpConnect,
    /// The TLS handshake over an open connection.
    TlsHandshake,
    /// Writing the request line, headers, and body. ureq bounds the wait for
    /// the response headers with the same budget, so this also covers a request
    /// body that was never fully written.
    RequestSend,
    /// Waiting for the response status line and headers.
    ResponseHeaders,
    /// Reading the response body.
    BodyTransfer,
}

impl FetchPhase {
    /// The stable lowercase label used in logs and error messages.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Queued => "queued",
            Self::Dns => "dns resolve",
            Self::TcpConnect => "tcp connect",
            Self::TlsHandshake => "tls handshake",
            Self::RequestSend => "request send",
            Self::ResponseHeaders => "response headers",
            Self::BodyTransfer => "body transfer",
        }
    }
}

impl fmt::Display for FetchPhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One request's terminal outcome, handed to a [`FetchObserver`].
///
/// The borrow keeps reporting allocation-free on the hot path; the URLs and
/// error belong to the reporting call and must not be retained.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum FetchEvent<'a> {
    /// The request finished with a response, including 4xx and 5xx statuses.
    Completed {
        method: HttpMethod,
        /// The URL the caller asked for, before any redirect.
        url: &'a Url,
        status: HttpStatus,
        body_bytes: usize,
        /// Wall time for the whole request, redirects included.
        elapsed: Duration,
    },
    /// The request failed. `error` already carries its phase and elapsed time.
    Failed {
        method: HttpMethod,
        url: &'a Url,
        /// Wall time for the whole request, redirects included.
        elapsed: Duration,
        error: &'a FetchError,
    },
}

/// Sink for per-request terminal outcomes.
///
/// Implementations must be cheap and must not block: they run on network
/// threads, once per request, inside the request's own latency budget.
pub trait FetchObserver: fmt::Debug + Send + Sync + 'static {
    /// Reports one request's terminal outcome.
    fn on_fetch_event(&self, event: &FetchEvent<'_>);
}

/// Default observer: one stderr line per failure, per slow request, and - with
/// `RENDER_NET_LOG=1` - per request.
///
/// Failures and slow requests are unconditional, because those are exactly the
/// cases that used to be invisible. A successful fast request is only noise
/// until somebody is measuring, so it needs the environment switch. The slow
/// threshold defaults to [`StderrObserver::DEFAULT_SLOW_THRESHOLD`] and can be
/// changed with `RENDER_NET_SLOW_MS=<milliseconds>`.
#[derive(Debug)]
pub struct StderrObserver {
    slow_threshold: Duration,
    log_everything: bool,
}

impl StderrObserver {
    /// A request at or above this duration is reported even when it succeeded.
    pub const DEFAULT_SLOW_THRESHOLD: Duration = Duration::from_secs(1);

    /// Builds the default observer, honoring `RENDER_NET_LOG` and
    /// `RENDER_NET_SLOW_MS`.
    #[must_use]
    pub fn from_environment() -> Self {
        let log_everything =
            std::env::var("RENDER_NET_LOG").is_ok_and(|value| !value.is_empty() && value != "0");
        let slow_threshold = std::env::var("RENDER_NET_SLOW_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map_or(Self::DEFAULT_SLOW_THRESHOLD, Duration::from_millis);
        Self {
            slow_threshold,
            log_everything,
        }
    }

    /// Builds an observer that reports failures and requests at or above
    /// `slow_threshold`, and nothing else.
    #[must_use]
    pub const fn with_slow_threshold(slow_threshold: Duration) -> Self {
        Self {
            slow_threshold,
            log_everything: false,
        }
    }
}

impl Default for StderrObserver {
    fn default() -> Self {
        Self::from_environment()
    }
}

impl FetchObserver for StderrObserver {
    fn on_fetch_event(&self, event: &FetchEvent<'_>) {
        match *event {
            FetchEvent::Completed {
                method,
                url,
                status,
                body_bytes,
                elapsed,
            } => {
                if elapsed < self.slow_threshold && !self.log_everything {
                    return;
                }
                let marker = if elapsed >= self.slow_threshold {
                    "SLOW"
                } else {
                    "ok"
                };
                eprintln!(
                    "render-net {marker} {} {url} {} {body_bytes} bytes in {:.1}ms \
                     (slow threshold {}ms)",
                    method.as_str(),
                    status.as_u16(),
                    elapsed.as_secs_f64() * 1000.0,
                    self.slow_threshold.as_millis()
                );
            }
            FetchEvent::Failed {
                method, url, error, ..
            } => eprintln!("render-net FAIL {} {url}: {error}", method.as_str()),
        }
    }
}

/// Observer that discards every outcome, for callers that log elsewhere.
#[derive(Clone, Copy, Debug, Default)]
pub struct NullObserver;

impl FetchObserver for NullObserver {
    fn on_fetch_event(&self, _event: &FetchEvent<'_>) {}
}

#[cfg(test)]
mod tests {
    use super::{FetchEvent, FetchObserver, FetchPhase, NullObserver, StderrObserver};
    use crate::{FetchError, FetchRequest, HttpMethod, Url};
    use std::time::Duration;

    #[test]
    fn phase_labels_are_stable_and_readable() {
        assert_eq!(FetchPhase::TcpConnect.to_string(), "tcp connect");
        assert_eq!(FetchPhase::Dns.as_str(), "dns resolve");
        assert_eq!(FetchPhase::BodyTransfer.to_string(), "body transfer");
        assert_eq!(FetchPhase::Queued.as_str(), "queued");
    }

    #[test]
    fn null_observer_accepts_events_without_observer_state() {
        let url = Url::parse("https://example.invalid/style.css").expect("test URL");
        let observer = NullObserver;
        observer.on_fetch_event(&FetchEvent::Completed {
            method: HttpMethod::Get,
            url: &url,
            status: crate::HttpStatus::from_u16(200),
            body_bytes: 7,
            elapsed: Duration::from_millis(1),
        });
        observer.on_fetch_event(&FetchEvent::Failed {
            method: HttpMethod::Get,
            url: &url,
            elapsed: Duration::from_millis(1),
            error: &FetchError::Dns,
        });
    }

    #[test]
    fn a_request_observer_is_constructible_without_a_logger() {
        // The default observer only writes to stderr, so a transport built with
        // it needs no logging facade installed.
        let observer = StderrObserver::with_slow_threshold(Duration::from_secs(30));
        let config = crate::FetchConfig {
            observer: std::sync::Arc::new(observer),
            ..crate::FetchConfig::default()
        };
        let transport = crate::HttpTransport::new(config);
        let url = Url::parse("data:text/plain,hello").expect("data URL");
        let response = transport
            .fetch(&FetchRequest::get(url), &crate::CancelToken::default())
            .expect("data URL response");
        assert_eq!(response.body, b"hello");
    }
}
