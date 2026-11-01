use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Read;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use ureq::ResponseExt;
use url::Url;

use crate::CookieJar;
use crate::diagnostics::{FetchEvent, FetchObserver, FetchPhase, StderrObserver};
use crate::h2::{H2Config, H2Connection, H2Error, H2Request, H2Response, StreamId};
use crate::{DEFAULT_PER_ORIGIN_CONCURRENCY, ORIGINS_BEFORE_TOTAL_CEILING};

/// Minimum average body throughput once [`FetchConfig::body_idle_timeout`]
/// has elapsed.
///
/// The body transfer has no whole-transfer wall clock (a large resource that
/// keeps making progress must not spuriously fail), so this floor bounds how
/// long a connection that only trickles bytes can pin a worker thread.
const MIN_BODY_BYTES_PER_SECOND: usize = 1024;

/// How long one request may hold the per-connection socket lock while waiting
/// for a frame before yielding it to the other multiplexed streams.
///
/// The mutex is what serialises socket access for a shared `H2Connection`; the
/// bound is what keeps one idle stream from preventing another from sending a
/// request. Frames themselves are still multiplexed and dispatched by stream
/// id as they arrive.
const H2_POLL_READ_TIMEOUT: Duration = Duration::from_millis(20);

/// Request bodies up to the default HTTP/2 initial stream window are sent
/// without needing a `WINDOW_UPDATE` during the request. Larger uploads use
/// the HTTP/1.1 path rather than parking a shared h2 connection in its
/// flow-control pump.
const H2_MAX_INLINE_BODY: usize = 60 * 1024;

/// Cooperative cancellation shared by a request and its caller.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// Requests cancellation. Blocking socket operations observe this at their
    /// next transport checkpoint; queued batch work is cancelled immediately.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Reports whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// HTTP status returned by the origin, including non-success statuses.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct HttpStatus(u16);

impl HttpStatus {
    /// Returns the numeric HTTP status code.
    #[must_use]
    pub const fn as_u16(self) -> u16 {
        self.0
    }

    /// Whether the status is in the inclusive 200..=299 range.
    #[must_use]
    pub const fn is_success(self) -> bool {
        self.0 >= 200 && self.0 <= 299
    }

    /// Creates a status from a wire status code.
    #[must_use]
    pub const fn from_u16(status: u16) -> Self {
        Self(status)
    }
}

/// Validators that can be sent with a conditional cache request.
///
/// Values are kept verbatim (including an entity-tag's quotes and optional
/// weak marker) so an origin can apply its normal validator comparison rules.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CacheValidators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

impl CacheValidators {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.etag.is_none() && self.last_modified.is_none()
    }

    #[must_use]
    pub fn from_headers(headers: &[Header]) -> Self {
        Self {
            etag: header_text(headers, "etag").map(str::to_owned),
            last_modified: header_text(headers, "last-modified").map(str::to_owned),
        }
    }
}

/// A response header. Values remain bytes so legal non-UTF-8 field values are
/// not silently corrupted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Header {
    pub name: String,
    pub value: Vec<u8>,
}

/// Parsed metadata from the `Content-Type` response header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentType {
    /// Lowercase ASCII media type, for example `text/html`.
    pub media_type: String,
    /// Lowercase charset label when a `charset` parameter was present.
    pub charset: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ByteRange {
    From { start: u64 },
    Inclusive { start: u64, end: u64 },
    Suffix { length: u64 },
}

impl ByteRange {
    /// Create an inclusive byte range.
    ///
    /// # Errors
    ///
    /// Returns an error when `end` precedes `start`.
    pub fn inclusive(start: u64, end: u64) -> Result<Self, FetchError> {
        if end < start {
            return Err(FetchError::InvalidByteRange { start, end });
        }
        Ok(Self::Inclusive { start, end })
    }

    /// Create a suffix range requesting the final `length` bytes.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero-length suffix.
    pub fn suffix(length: u64) -> Result<Self, FetchError> {
        if length == 0 {
            return Err(FetchError::EmptyByteRangeSuffix);
        }
        Ok(Self::Suffix { length })
    }

    fn header_value(self) -> String {
        match self {
            Self::From { start } => format!("bytes={start}-"),
            Self::Inclusive { start, end } => format!("bytes={start}-{end}"),
            Self::Suffix { length } => format!("bytes=-{length}"),
        }
    }
}

/// HTTP request methods the transport can put on the wire.
///
/// The transport normalizes whatever the upper layers supply (navigation,
/// `fetch()`, XHR) onto one of these methods.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum HttpMethod {
    /// `GET`, the default for navigation and resource loads.
    #[default]
    Get,
    /// `POST`, used by `fetch()`/XHR submissions.
    Post,
    /// `PUT`.
    Put,
    /// `DELETE`. May carry a request body per HTTP semantics.
    Delete,
    /// `HEAD`, a `GET` without a response body.
    Head,
    /// `OPTIONS`, the CORS preflight method. Pages can also send it.
    Options,
    /// `PATCH`, which the Fetch standard allows pages to use.
    Patch,
}

impl HttpMethod {
    /// Parse an uppercase wire token ("GET", "POST", ...) into the method.
    #[must_use]
    pub fn from_wire(token: &str) -> Option<Self> {
        match token {
            "GET" => Some(Self::Get),
            "POST" => Some(Self::Post),
            "PUT" => Some(Self::Put),
            "DELETE" => Some(Self::Delete),
            "HEAD" => Some(Self::Head),
            "OPTIONS" => Some(Self::Options),
            "PATCH" => Some(Self::Patch),
            _ => None,
        }
    }

    /// The method token exactly as it appears on the request line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
            Self::Patch => "PATCH",
        }
    }

    /// Whether the method may carry a request body. `GET` and `HEAD` must not;
    /// attaching one is rejected with [`FetchError::InvalidRequest`].
    #[must_use]
    pub const fn allows_body(self) -> bool {
        !matches!(self, Self::Get | Self::Head)
    }
}

/// Headers the transport owns on the wire and therefore rejects from request
/// builders.
///
/// `Host` follows the request URL, the framing headers (`Content-Length`,
/// `Transfer-Encoding`, `Connection`) are derived from the body and the
/// connection lifecycle, and `Cookie` carries redirect-scope rules only the
/// transport's per-hop cookie machinery may apply (callers use
/// [`FetchRequest::with_cookie`]).
const RESERVED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "cookie",
];

/// A normalized HTTP request.
///
/// The default method is [`HttpMethod::Get`]; callers only need the builders
/// for anything else. Callers may attach custom `headers` and a request
/// `body`; headers reserved for the transport ([`RESERVED_HEADERS`]) are
/// rejected when the request is issued.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchRequest {
    pub url: Url,
    /// Request method, defaulting to [`HttpMethod::Get`].
    pub method: HttpMethod,
    /// Extra request headers sent on every hop, verbatim. `Content-Length` is
    /// derived from `body` automatically; see [`RESERVED_HEADERS`] for the
    /// headers a caller must not set.
    pub headers: Vec<(String, String)>,
    /// Optional request body. Only valid on methods where
    /// [`HttpMethod::allows_body`] holds.
    pub body: Option<Vec<u8>>,
    /// Optional request `Accept` value. Browser content negotiation policy
    /// belongs to the caller rather than this transport adapter.
    pub accept: Option<String>,
    /// Optional serialized Cookie request header supplied by the browser
    /// context. The transport remains stateless and never owns a cookie jar.
    pub cookie: Option<String>,
    /// Optional single HTTP byte range. Multipart ranges are intentionally not
    /// exposed until the media/cache layer can consume multipart responses.
    pub byte_range: Option<ByteRange>,
    /// Optional validators used for conditional cache revalidation.
    pub cache_validators: Option<CacheValidators>,
}

impl FetchRequest {
    /// Creates a request with an explicit method.
    #[must_use]
    pub const fn new(method: HttpMethod, url: Url) -> Self {
        Self {
            url,
            method,
            headers: Vec::new(),
            body: None,
            accept: None,
            cookie: None,
            byte_range: None,
            cache_validators: None,
        }
    }

    #[must_use]
    pub const fn get(url: Url) -> Self {
        Self::new(HttpMethod::Get, url)
    }

    /// Creates a `POST` request.
    #[must_use]
    pub const fn post(url: Url) -> Self {
        Self::new(HttpMethod::Post, url)
    }

    /// Creates a `PUT` request.
    #[must_use]
    pub const fn put(url: Url) -> Self {
        Self::new(HttpMethod::Put, url)
    }

    /// Creates a `DELETE` request.
    #[must_use]
    pub const fn delete(url: Url) -> Self {
        Self::new(HttpMethod::Delete, url)
    }

    /// Creates a `HEAD` request.
    #[must_use]
    pub const fn head(url: Url) -> Self {
        Self::new(HttpMethod::Head, url)
    }

    /// Overrides the request method.
    #[must_use]
    pub const fn with_method(mut self, method: HttpMethod) -> Self {
        self.method = method;
        self
    }

    /// Adds a custom request header sent on every hop. A header of the same
    /// name set here replaces the transport-managed equivalents (`Accept`,
    /// `Range`, conditional validators); reserved headers
    /// ([`RESERVED_HEADERS`]) are rejected when the request is issued.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Attaches a request body. Rejected at fetch time for methods where
    /// [`HttpMethod::allows_body`] does not hold.
    #[must_use]
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }

    #[must_use]
    pub fn with_accept(mut self, accept: impl Into<String>) -> Self {
        self.accept = Some(accept.into());
        self
    }

    #[must_use]
    pub fn with_cookie(mut self, cookie: impl Into<String>) -> Self {
        self.cookie = Some(cookie.into());
        self
    }

    #[must_use]
    pub const fn with_byte_range(mut self, byte_range: ByteRange) -> Self {
        self.byte_range = Some(byte_range);
        self
    }

    /// Adds validators for a conditional `GET` (`If-None-Match` and/or
    /// `If-Modified-Since`). Empty validators are treated as absent.
    #[must_use]
    pub fn with_cache_validators(mut self, validators: CacheValidators) -> Self {
        self.cache_validators = (!validators.is_empty()).then_some(validators);
        self
    }

    /// Adds an entity-tag validator for conditional revalidation.
    #[must_use]
    pub fn with_etag(mut self, etag: impl Into<String>) -> Self {
        let mut validators = self.cache_validators.take().unwrap_or_default();
        validators.etag = Some(etag.into());
        self.cache_validators = Some(validators);
        self
    }

    /// Adds a Last-Modified validator for conditional revalidation.
    #[must_use]
    pub fn with_last_modified(mut self, last_modified: impl Into<String>) -> Self {
        let mut validators = self.cache_validators.take().unwrap_or_default();
        validators.last_modified = Some(last_modified.into());
        self.cache_validators = Some(validators);
        self
    }
}

/// Bounded transport configuration.
#[derive(Clone, Debug)]
pub struct FetchConfig {
    pub redirect_limit: u32,
    pub max_body_bytes: usize,
    pub max_header_bytes: usize,
    /// Per-phase budget, deliberately NOT a whole-transfer cap.
    ///
    /// The DNS lookup, the connection, sending the request (headers and
    /// body), and receiving the response headers each take up to this long
    /// before the transfer fails with [`FetchError::Timeout`], and the whole
    /// redirect chain shares it. The response body has no wall clock: a body
    /// that keeps making progress may legitimately run longer than this budget
    /// overall (a CDN stylesheet trickling in over a slow link, for example).
    ///
    /// This is the **fallback** for the two phase budgets below, not a
    /// substitute for them. It exists so that a caller who wants every request
    /// bounded sets one number, and so that the phases which genuinely have no
    /// budget of their own (name resolution, request write) have one. Set
    /// [`FetchConfig::response_timeout`] or [`FetchConfig::body_idle_timeout`]
    /// to decouple a phase from it; leaving them `None` keeps "one number
    /// bounds everything", which is what the defaults do.
    pub timeout: Duration,
    /// Budget for the connect phase only: the TCP connect and, for `https`,
    /// the TLS handshake over the socket it opened.
    ///
    /// This is deliberately much smaller than [`FetchConfig::timeout`] and
    /// exists because of how ureq 3.3 fails over between addresses. There is no
    /// happy-eyeballs racing: `TcpConnector` walks the resolved addresses in
    /// order and hands each one a slice of `timeout_connect` taken from a
    /// geometric series that sums to the whole budget (curl's fallback
    /// schedule). The first address therefore receives about two thirds of the
    /// budget for a dual-stack host and a bit over half for a four-address CDN
    /// host, and it burns that slice in full when the address is a black hole
    /// (a filtered AAAA record, a route that silently drops SYNs). With the
    /// per-request budget used as the connect budget, one unresponsive record
    /// costs ~16s of a 30s request before the next address is even tried, and
    /// nothing reports it. A dedicated connect budget caps the worst case at
    /// itself, which is what makes the failure visible and bounded, while the
    /// rest of [`FetchConfig::timeout`] stays available to the phases that can
    /// legitimately need it (a large body trickling in over a slow link).
    ///
    /// The TLS handshake shares this budget in ureq, but it runs *after*
    /// address selection on the socket that connected, so a high-latency link
    /// is not penalised by the geometric split. A zero value is the opt-out:
    /// the connect phase then runs on [`FetchConfig::timeout`] rather than with
    /// no bound at all. See [`FetchConfig::effective_connect_timeout`].
    pub connect_timeout: Duration,
    /// Budget for the response phase: how long to wait for the status line and
    /// headers once the request has been written. `None` follows
    /// [`FetchConfig::timeout`].
    ///
    /// **What the default protects against:** an origin (or a proxy in front of
    /// it) that accepts the connection, swallows the request, and then says
    /// nothing at all. This is the most common way a page load dies, and the
    /// failure is reported as [`FetchPhase::ResponseHeaders`].
    ///
    /// The wait was bounded before this field existed, but only by borrowing
    /// ureq's send-request budget, so it had no budget of its own, no name, and
    /// no way to be tuned without also changing how the request write behaves.
    ///
    /// **Why it is a separate knob and not just `timeout`:** the wait is not
    /// the only thing `timeout` is used for, and the two need different values
    /// in practice. A page load wants a short bound here (a subresource that
    /// gets no headers is a lost subresource, and the sooner it is given up on
    /// the sooner the rest of the page proceeds) while the same page's body
    /// transfer may legitimately need a long one. Conflating them is what made
    /// the original hang invisible: the phase that needed a bound had none of
    /// its own and had to borrow one, so tuning it for the body silently
    /// changed the header behaviour, and nothing documented which was which.
    ///
    /// ureq has no `recv_headers` knob: the header wait is bounded by the
    /// send-request budget, because `Timeout::RecvResponse`'s deadline is
    /// re-derived from the recorded send time. This transport therefore sets
    /// `timeout_send_request` to this budget, which is what actually bounds the
    /// wait. The consequence is that a timeout here is reported by ureq as a
    /// send-phase reason; for a bodyless request the loop in `fetch_transfer`
    /// rewrites that to [`FetchPhase::ResponseHeaders`], because a `GET` that
    /// wrote a few hundred bytes and left the send phase can only have expired
    /// while waiting for the origin to answer.
    pub response_timeout: Option<Duration>,
    /// **Idle-read** bound for the response body: how long a body read may wait
    /// for the *next* byte once the previous one arrived. `None` follows
    /// [`FetchConfig::timeout`].
    ///
    /// This is an idle bound and must stay one. **What the default protects
    /// against** is a connection that delivered part of a body and then went
    /// quiet forever - a half-open socket, a proxy that dropped the upstream,
    /// an origin that stopped writing mid-resource. Such a transfer otherwise
    /// holds its worker thread, its socket, and its pool slot until the process
    /// exits, and the engine cannot tell that asset from one that is merely
    /// slow.
    ///
    /// It is deliberately **not** a total transfer budget. A 2 MB stylesheet on
    /// a slow link is fine, and a large stylesheet corpus is exactly what this
    /// engine struggles with, so a wall clock on the body would fail the
    /// transfers that matter most while fixing the ones that do not. Nothing
    /// here has a total body budget: a body that keeps arriving is bounded only
    /// by `max_body_bytes`, by this idle bound, and by the
    /// [`MIN_BODY_BYTES_PER_SECOND`] floor.
    ///
    /// Because it is an idle bound it is also the one body budget that is safe
    /// to push to the socket as a read deadline, and doing so is what stops the
    /// transport from leaving a thread blocked on a dead socket: see
    /// `HttpTransport::with_proxy` for why `timeout_recv_body` is set here and
    /// `timeout_recv_response` is not.
    pub body_idle_timeout: Option<Duration>,
    /// Idle connections the pool keeps for one origin (scheme, host, port and
    /// proxy together).
    ///
    /// This is the same-origin ceiling that matters once connection reuse works,
    /// and it is a decision rather than a default: over HTTP/1.1 a page cannot
    /// have more requests in flight against one origin than this, so keeping
    /// more idle connections than that would hold sockets that could never be
    /// reused, and keeping fewer throws away connections the next wave of
    /// requests would have taken for free. The default is
    /// [`DEFAULT_PER_ORIGIN_CONCURRENCY`](crate::DEFAULT_PER_ORIGIN_CONCURRENCY),
    /// the per-origin limit [`BatchOptions`](crate::BatchOptions) also applies,
    /// so the two cannot drift apart.
    ///
    /// Note that this bounds what is *kept*, not what runs: requests in flight
    /// are not in the pool. Zero keeps no idle connection for an origin, which
    /// is equivalent to disabling reuse for it.
    pub idle_connections_per_origin: usize,
    /// Idle connections the pool keeps in total, across all origins.
    ///
    /// A ceiling of one origin's worth would make a page with a CDN on it pay a
    /// handshake for the CDN's assets, so the default is four origins' worth.
    /// This is a socket count, not a request count: a connection in flight is
    /// not held here.
    pub idle_connections_total: usize,
    pub user_agent: String,
    /// Where every request's terminal outcome is reported. The default writes
    /// failures and slow requests to stderr; see [`StderrObserver`].
    pub observer: Arc<dyn FetchObserver>,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            redirect_limit: 10,
            max_body_bytes: 16 * 1024 * 1024,
            max_header_bytes: 64 * 1024,
            timeout: Duration::from_secs(30),
            connect_timeout: Self::DEFAULT_CONNECT_TIMEOUT,
            // Left `None` on purpose, so all three phases follow `timeout` and
            // one number still bounds a whole request. Each field names the
            // failure it protects against, for a caller that needs to pull one
            // phase out of the shared budget.
            response_timeout: None,
            body_idle_timeout: None,
            idle_connections_per_origin: DEFAULT_PER_ORIGIN_CONCURRENCY,
            idle_connections_total: DEFAULT_PER_ORIGIN_CONCURRENCY * ORIGINS_BEFORE_TOTAL_CEILING,
            user_agent: std::env::var("RENDER_USER_AGENT").unwrap_or_else(|_| {
                format!(
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
                     AppleWebKit/537.36 (KHTML, like Gecko) \
                     Chrome/120.0.0.0 Safari/537.36 rENDER/{}",
                    env!("CARGO_PKG_VERSION")
                )
            }),
            observer: Arc::new(StderrObserver::from_environment()),
        }
    }
}

impl FetchConfig {
    /// Default connect budget: [`FetchConfig::DEFAULT_TIMEOUT`], sized for the
    /// address-failure walk described on [`FetchConfig::connect_timeout`].
    ///
    /// A connect either succeeds in well under this or is failing; nothing
    /// legitimate needs longer, and a shorter bound is what keeps one
    /// unreachable address from eating a whole request.
    pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

    /// Default overall per-phase budget, and the fallback for
    /// [`FetchConfig::response_timeout`] and
    /// [`FetchConfig::body_idle_timeout`] when they are `None`.
    ///
    /// Sized for the slowest phase that has no budget of its own - name
    /// resolution and the request write. It is generous because a page load
    /// that fails at 30s per asset has already lost, and because a slow but
    /// healthy origin must not be mistaken for a broken one.
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

    /// The connect budget this config will actually enforce.
    ///
    /// A zero [`FetchConfig::connect_timeout`] is the documented opt-out, and
    /// resolves to the overall budget rather than to "unbounded", so the
    /// connect phase is never the one phase with no bound at all.
    #[must_use]
    pub fn effective_connect_timeout(&self) -> Duration {
        if self.connect_timeout > Duration::ZERO {
            self.connect_timeout.min(self.timeout)
        } else {
            self.timeout
        }
    }

    /// The response-header budget this config will actually enforce.
    #[must_use]
    pub fn effective_response_timeout(&self) -> Duration {
        self.response_timeout.unwrap_or(self.timeout)
    }

    /// The body idle-read budget this config will actually enforce.
    #[must_use]
    pub fn effective_body_idle_timeout(&self) -> Duration {
        self.body_idle_timeout.unwrap_or(self.timeout)
    }
}

/// Successful response bytes and transport metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchResponse {
    pub requested_url: Url,
    pub final_url: Url,
    /// Redirect chain including the requested and final URLs.
    pub redirect_chain: Vec<Url>,
    /// Redirect responses followed before the final response.
    pub redirects: Vec<RedirectResponse>,
    pub status: HttpStatus,
    pub headers: Vec<Header>,
    pub content_type: Option<ContentType>,
    pub body: Vec<u8>,
}

/// Typed transport failures. HTTP 4xx/5xx responses are successful transport
/// results and retain their status in [`FetchResponse`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum FetchError {
    Cancelled,
    UnsupportedScheme(String),
    InvalidUrl(String),
    Dns,
    Timeout,
    Tls(String),
    RedirectLimitExceeded {
        limit: u32,
    },
    HeaderLimitExceeded {
        limit: usize,
    },
    BodyLimitExceeded {
        limit: usize,
    },
    InvalidByteRange {
        start: u64,
        end: u64,
    },
    EmptyByteRangeSuffix,
    /// A caller-supplied header collides with one the transport owns on the
    /// wire (`Host`, `Content-Length`, `Transfer-Encoding`, `Connection`,
    /// `Cookie`). The payload is the offending header name as supplied.
    ReservedHeader(String),
    /// The request is malformed at the transport layer, for example a body
    /// attached to a `GET` or `HEAD`.
    InvalidRequest(String),
    /// A failure that happened inside a network operation, tagged with the
    /// [`FetchPhase`] it happened in and the time spent before it surfaced.
    ///
    /// This is what makes a stall visible: the wrapped `source` keeps the
    /// original typed failure, and the phase plus elapsed time say *where* and
    /// *how long*. `elapsed` is the time from the start of the failing
    /// operation, which for a hop covers everything ureq does in one call -
    /// resolve, connect, TLS, send, and response headers - because only ureq
    /// can see the boundaries between those. Failures raised before any I/O
    /// (request validation, URL handling, cancellation) and transport policy
    /// failures stay untagged, since there is no phase to blame.
    Failed {
        phase: FetchPhase,
        elapsed: Duration,
        source: Box<FetchError>,
    },
    Protocol(String),
    Io(String),
    WorkerStopped,
    Transport(String),
}

impl FetchError {
    /// The transport phase this failure happened in, when it is known.
    #[must_use]
    pub const fn phase(&self) -> Option<FetchPhase> {
        match self {
            Self::Failed { phase, .. } => Some(*phase),
            _ => None,
        }
    }

    /// How long the failing operation ran before this failure surfaced.
    #[must_use]
    pub const fn elapsed(&self) -> Option<Duration> {
        match self {
            Self::Failed { elapsed, .. } => Some(*elapsed),
            _ => None,
        }
    }

    /// The original failure with the phase and elapsed time removed, so callers
    /// that branch on the typed variants do not have to unwrap.
    #[must_use]
    pub fn into_inner(self) -> FetchError {
        match self {
            Self::Failed { source, .. } => *source,
            other => other,
        }
    }

    /// Tags a failure with the phase it happened in and how long it took.
    ///
    /// Cancellation is passed through untouched: it is a caller decision, not a
    /// transport phase, and callers match on it directly.
    pub(crate) fn in_phase(self, phase: FetchPhase, elapsed: Duration) -> Self {
        match self {
            Self::Cancelled => self,
            source => Self::Failed {
                phase,
                elapsed,
                source: Box::new(source),
            },
        }
    }
}

impl fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("request cancelled"),
            Self::UnsupportedScheme(scheme) => {
                write!(formatter, "unsupported URL scheme: {scheme}")
            }
            Self::InvalidUrl(message) => write!(formatter, "invalid request URL: {message}"),
            Self::Dns => formatter.write_str("host name lookup failed"),
            Self::Timeout => formatter.write_str("request timed out"),
            Self::Tls(message) => write!(formatter, "TLS verification/transport failed: {message}"),
            Self::RedirectLimitExceeded { limit } => {
                write!(formatter, "redirect limit exceeded ({limit})")
            }
            Self::HeaderLimitExceeded { limit } => {
                write!(formatter, "response header limit exceeded ({limit} bytes)")
            }
            Self::BodyLimitExceeded { limit } => {
                write!(formatter, "response body limit exceeded ({limit} bytes)")
            }
            Self::InvalidByteRange { start, end } => {
                write!(formatter, "invalid byte range {start}-{end}")
            }
            Self::EmptyByteRangeSuffix => {
                formatter.write_str("byte-range suffix length must be non-zero")
            }
            Self::ReservedHeader(name) => write!(
                formatter,
                "header '{name}' is managed by the transport and cannot be set explicitly"
            ),
            Self::InvalidRequest(message) => write!(formatter, "invalid request: {message}"),
            Self::Failed {
                phase,
                elapsed,
                source,
            } => write!(
                formatter,
                "{phase}: {source} after {}ms",
                elapsed.as_millis()
            ),
            Self::Protocol(message) => write!(formatter, "HTTP protocol error: {message}"),
            Self::Io(message) => write!(formatter, "network I/O error: {message}"),
            Self::WorkerStopped => formatter.write_str("network worker stopped"),
            Self::Transport(message) => write!(formatter, "transport error: {message}"),
        }
    }
}

impl std::error::Error for FetchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Failed { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

pub type FetchResult = Result<FetchResponse, FetchError>;

/// Metadata for one HTTP redirect response followed by the transport.
///
/// Keeping these headers lets the browser context process cookies set during
/// a redirect chain without making the transport own browser state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RedirectResponse {
    pub url: Url,
    pub status: HttpStatus,
    pub headers: Vec<Header>,
}

/// Why an explicit HTTP proxy address was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProxyConfigErrorKind {
    /// The input was empty or only whitespace.
    Empty,
    /// The input could not be parsed as a URL.
    InvalidUrl,
    /// The scheme was present but is not `http`.
    UnsupportedScheme,
    /// The URL has no host.
    MissingHost,
    /// The URL carries a path, query or fragment that a proxy address cannot use.
    UnexpectedPath,
    /// `ureq` rejected the address (for example an invalid port).
    InvalidProxy,
}

/// A rejected proxy address.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProxyConfigError {
    kind: ProxyConfigErrorKind,
    message: String,
}

impl ProxyConfigError {
    fn new(kind: ProxyConfigErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> ProxyConfigErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ProxyConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProxyConfigError {}

/// A validated explicit HTTP proxy.
///
/// The browser keeps the canonical address for its settings UI and hands the
/// transport the pre-parsed `ureq` proxy. Constructing one here means an
/// invalid address is rejected before it can reach a request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpProxyConfig {
    address: String,
    proxy: ureq::Proxy,
}

impl HttpProxyConfig {
    /// Parses `host:port` or `http://host:port`.
    ///
    /// # Errors
    ///
    /// Returns [`ProxyConfigError`] for an empty address, an unsupported
    /// scheme, a missing host, trailing path/query/fragment components, or an
    /// address `ureq` cannot use.
    pub fn parse(input: &str) -> Result<Self, ProxyConfigError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(ProxyConfigError::new(
                ProxyConfigErrorKind::Empty,
                "proxy address is empty",
            ));
        }
        let normalized = if trimmed.contains("://") {
            trimmed.to_owned()
        } else {
            format!("http://{trimmed}")
        };
        let url = Url::parse(&normalized).map_err(|error| {
            ProxyConfigError::new(
                ProxyConfigErrorKind::InvalidUrl,
                format!("invalid proxy URL: {error}"),
            )
        })?;
        if url.scheme() != "http" {
            return Err(ProxyConfigError::new(
                ProxyConfigErrorKind::UnsupportedScheme,
                format!("unsupported proxy scheme {:?}", url.scheme()),
            ));
        }
        if url.host_str().is_none() {
            return Err(ProxyConfigError::new(
                ProxyConfigErrorKind::MissingHost,
                "proxy URL has no host",
            ));
        }
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            return Err(ProxyConfigError::new(
                ProxyConfigErrorKind::UnexpectedPath,
                "proxy address must not contain a path, query or fragment",
            ));
        }
        let address = normalized.trim_end_matches('/').to_owned();
        let proxy = ureq::Proxy::new(&address).map_err(|error| {
            ProxyConfigError::new(
                ProxyConfigErrorKind::InvalidProxy,
                format!("invalid proxy address: {error}"),
            )
        })?;
        Ok(Self { address, proxy })
    }

    /// The canonical address shown in settings and persisted to the profile.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }
}

struct TransportAgent {
    agent: ureq::Agent,
    /// Whether the proxy in use was resolved implicitly from the environment or
    /// the platform, rather than handed in by a caller. See
    /// [`HttpTransport::new`].
    implicit_proxy: bool,
    /// Whether any proxy is configured at all. HTTP/2 is only attempted when
    /// the hop is direct: a proxied origin stays on the HTTP/1.1 path (which
    /// uses ureq's CONNECT support), never bypassing the proxy.
    has_proxy: bool,
}

/// One origin (`https` host and port) whose HTTP/2 connection is pooled.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct H2Origin {
    host: String,
    port: u16,
}

impl H2Origin {
    fn from_url(url: &Url) -> Option<Self> {
        if url.scheme() != "https" {
            return None;
        }
        Some(Self {
            host: url.host_str()?.to_owned(),
            port: url.port_or_known_default()?,
        })
    }

    /// The `:authority` value, including a bracketed IPv6 literal and a
    /// non-default port.
    fn authority(&self) -> String {
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == 443 {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }

    fn server_name(&self) -> Result<ServerName<'static>, FetchError> {
        // `Url::host_str` keeps the brackets around an IPv6 literal, but
        // `ServerName` expects the bare address (or a DNS name).
        let host = self
            .host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        ServerName::try_from(host)
            .map_err(|_| FetchError::InvalidUrl(format!("invalid TLS server name {:?}", self.host)))
    }
}

type H2Stream = StreamOwned<ClientConnection, TcpStream>;

/// A shared HTTP/2 connection plus the mutex that serialises socket access.
///
/// The socket itself can only be read by one thread at a time. Each streaming
/// request holds the mutex for at most [`H2_POLL_READ_TIMEOUT`] while waiting
/// for a frame, then releases it so another stream can send or consume its
/// response. A request write briefly holds the same mutex and is bounded by the
/// socket write timeout. Frames for all streams on the connection are
/// dispatched by stream id by [`H2Connection::poll_frame`], so the requests do
/// share one TCP connection even though byte-level socket access is serialised.
struct H2Entry {
    connection: Mutex<H2Connection<H2Stream>>,
}

/// Per-origin HTTP/2 connection pool.
///
/// Direct `https` origins only. A hop that inherits an HTTP proxy is never
/// given an h2 connection here: it stays on ureq's HTTP/1.1 path, which already
/// performs the `CONNECT` tunnel, so no request can silently bypass a proxy.
struct H2Pool {
    tls: Arc<ClientConfig>,
    entries: Mutex<HashMap<H2Origin, Arc<H2Entry>>>,
    /// Per-origin creation locks: concurrent first fetches must not race into
    /// opening several connections that would immediately be discarded, while
    /// different origins still establish their connections in parallel.
    connect_locks: Mutex<HashMap<H2Origin, Arc<Mutex<()>>>>,
    /// Origins whose TLS server selected ALPN `http/1.1` (or no ALPN). They are
    /// sent straight to ureq on later requests instead of paying a probe
    /// handshake every time.
    http1_only: Mutex<HashSet<H2Origin>>,
    max_entries: usize,
}

struct H2Failure {
    error: FetchError,
    phase: FetchPhase,
    /// The connection is unusable and must leave the pool.
    evict: bool,
    /// No response headers were seen, so a safe method may be retried once.
    retry: bool,
}

enum H2AttemptError {
    /// The server negotiated HTTP/1.1 or no ALPN; use the ureq path.
    FallbackHttp1(String),
    Failed(H2Failure),
}

impl H2Pool {
    fn new(config: &FetchConfig) -> Self {
        let roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        Self::with_roots(config, roots)
    }

    fn with_roots(config: &FetchConfig, roots: RootCertStore) -> Self {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("the ring provider supports the default TLS versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        // Advertising `http/1.1` as well as `h2` is what lets an origin that
        // only speaks HTTP/1.1 complete the probe and fall back cleanly.
        tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Self {
            tls: Arc::new(tls),
            entries: Mutex::new(HashMap::new()),
            connect_locks: Mutex::new(HashMap::new()),
            http1_only: Mutex::new(HashSet::new()),
            max_entries: config.idle_connections_total.max(1),
        }
    }

    fn is_http1_only(&self, origin: &H2Origin) -> bool {
        self.http1_only
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(origin)
    }

    fn mark_http1_only(&self, origin: &H2Origin) {
        self.http1_only
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(origin.clone());
    }

    fn remove(&self, origin: &H2Origin, entry: &Arc<H2Entry>) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries
            .get(origin)
            .is_some_and(|current| Arc::ptr_eq(current, entry))
        {
            entries.remove(origin);
        }
    }

    fn entry_for(
        &self,
        origin: &H2Origin,
        config: &FetchConfig,
        cancel: &CancelToken,
    ) -> Result<Arc<H2Entry>, H2AttemptError> {
        let existing = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(origin)
            .cloned();
        if let Some(entry) = existing {
            let closed = entry
                .connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_closed();
            if !closed {
                return Ok(entry);
            }
            self.remove(origin, &entry);
        }

        let creation_lock = self
            .connect_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(origin.clone())
            .or_default()
            .clone();
        let _connecting = creation_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Another request may have established the connection while this one
        // waited for the creation lock.
        let existing = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(origin)
            .cloned();
        if let Some(entry) = existing {
            let closed = entry
                .connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_closed();
            if !closed {
                return Ok(entry);
            }
            self.remove(origin, &entry);
        }

        let connection = self.connect(origin, config, cancel)?;
        let entry = Arc::new(H2Entry {
            connection: Mutex::new(connection),
        });

        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries.len() >= self.max_entries
            && let Some(victim) = entries.keys().find(|key| *key != origin).cloned()
        {
            entries.remove(&victim);
        }
        entries.insert(origin.clone(), Arc::clone(&entry));
        drop(entries);
        Ok(entry)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "TCP, TLS, ALPN, and the HTTP/2 preface belong to one bounded connect pipeline"
    )]
    fn connect(
        &self,
        origin: &H2Origin,
        config: &FetchConfig,
        cancel: &CancelToken,
    ) -> Result<H2Connection<H2Stream>, H2AttemptError> {
        let started = Instant::now();
        let connect_timeout = config.effective_connect_timeout();
        if cancel.is_cancelled() {
            return Err(H2AttemptError::Failed(H2Failure {
                error: FetchError::Cancelled,
                phase: FetchPhase::TcpConnect,
                evict: false,
                retry: false,
            }));
        }

        let resolve_host = origin.host.trim_start_matches('[').trim_end_matches(']');
        let addresses =
            resolve_h2(resolve_host, origin.port, connect_timeout).map_err(|error| {
                H2AttemptError::Failed(H2Failure {
                    error,
                    phase: FetchPhase::Dns,
                    evict: false,
                    retry: false,
                })
            })?;
        let server_name = origin.server_name().map_err(|error| {
            H2AttemptError::Failed(H2Failure {
                error,
                phase: FetchPhase::TlsHandshake,
                evict: false,
                retry: false,
            })
        })?;

        let mut last_error = None;
        let mut socket = None;
        for address in addresses {
            let remaining = connect_timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                break;
            }
            match TcpStream::connect_timeout(&address, remaining) {
                Ok(stream) => {
                    socket = Some(stream);
                    break;
                }
                Err(error) => last_error = Some(error),
            }
        }
        let Some(socket) = socket else {
            let error = if started.elapsed() >= connect_timeout {
                FetchError::Timeout
            } else {
                FetchError::Io(last_error.map_or_else(
                    || "no address resolved".to_owned(),
                    |error| error.to_string(),
                ))
            };
            return Err(H2AttemptError::Failed(H2Failure {
                error,
                phase: FetchPhase::TcpConnect,
                evict: false,
                retry: false,
            }));
        };

        let handshake_budget = connect_timeout.saturating_sub(started.elapsed()).max(
            // A zero remaining budget would disable the timeout entirely on
            // some platforms; keep a tiny positive bound instead.
            Duration::from_millis(1),
        );
        let _ = socket.set_nodelay(true);
        let _ = socket.set_read_timeout(Some(handshake_budget));
        let _ = socket.set_write_timeout(Some(handshake_budget));

        let client =
            ClientConnection::new(Arc::clone(&self.tls), server_name).map_err(|error| {
                H2AttemptError::Failed(H2Failure {
                    error: FetchError::Tls(error.to_string()),
                    phase: FetchPhase::TlsHandshake,
                    evict: false,
                    retry: false,
                })
            })?;
        let mut stream = StreamOwned::new(client, socket);
        {
            let StreamOwned { conn, sock } = &mut stream;
            if let Err(error) = conn.complete_io(sock) {
                let timed_out = matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                );
                let error = if timed_out {
                    FetchError::Timeout
                } else if let Some(tls_error) = rustls_error_from_io(&error) {
                    FetchError::Tls(tls_error.to_string())
                } else {
                    FetchError::Tls(error.to_string())
                };
                return Err(H2AttemptError::Failed(H2Failure {
                    error,
                    phase: FetchPhase::TlsHandshake,
                    evict: false,
                    retry: false,
                }));
            }
        }

        if stream.conn.alpn_protocol() != Some(b"h2".as_slice()) {
            let negotiated = stream.conn.alpn_protocol().map_or_else(
                || "none".to_owned(),
                |protocol| String::from_utf8_lossy(protocol).into_owned(),
            );
            drop(stream);
            return Err(H2AttemptError::FallbackHttp1(format!(
                "ALPN negotiated {negotiated}, not h2"
            )));
        }

        // The initial SETTINGS exchange runs inside the connect budget; only
        // afterwards is the socket switched to the short polling timeout that
        // lets all multiplexed streams take turns.
        let h2_config = H2Config {
            max_body_size: config.max_body_bytes,
            max_header_list_size: u32::try_from(config.max_header_bytes).unwrap_or(u32::MAX),
            max_header_block_size: config.max_header_bytes.max(1024),
            ..H2Config::default()
        };
        let connection = H2Connection::connect(stream, h2_config).map_err(|error| {
            let timed_out = started.elapsed() >= connect_timeout;
            let (error, phase) = if timed_out {
                (FetchError::Timeout, FetchPhase::TcpConnect)
            } else {
                (map_h2_error(&error, config), FetchPhase::TlsHandshake)
            };
            H2AttemptError::Failed(H2Failure {
                error,
                phase,
                evict: false,
                retry: false,
            })
        })?;

        let socket = connection_socket(&connection);
        let _ = socket.set_read_timeout(Some(H2_POLL_READ_TIMEOUT));
        let _ = socket.set_write_timeout(Some(config.effective_response_timeout()));
        Ok(connection)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the pool forwards the already-prepared hop request without rebuilding it"
    )]
    fn send(
        &self,
        origin: &H2Origin,
        config: &FetchConfig,
        method: HttpMethod,
        url: &Url,
        headers: &[(String, String)],
        body: Option<&[u8]>,
        cancel: &CancelToken,
    ) -> Result<H2Response, H2AttemptError> {
        let mut attempt = 0;
        loop {
            let entry = self.entry_for(origin, config, cancel)?;
            match send_h2_request(&entry, config, method, url, headers, body, cancel) {
                Ok(response) => return Ok(response),
                Err(failure) => {
                    if failure.evict {
                        self.remove(origin, &entry);
                    }
                    let safe_to_retry = matches!(
                        method,
                        HttpMethod::Get
                            | HttpMethod::Head
                            | HttpMethod::Options
                            | HttpMethod::Put
                            | HttpMethod::Delete
                    );
                    if failure.retry && safe_to_retry && attempt == 0 {
                        attempt = 1;
                        continue;
                    }
                    return Err(H2AttemptError::Failed(failure));
                }
            }
        }
    }
}

/// The socket behind a connected [`H2Connection`], used to change timeouts
/// after the handshake.
fn connection_socket(connection: &H2Connection<H2Stream>) -> &TcpStream {
    &connection.io().sock
}

/// Resolves `host:port` in a worker so a stuck resolver cannot outlive the
/// connect budget. A timed-out resolver thread is abandoned; the OS resolver
/// has no cancellation API.
fn resolve_h2(host: &str, port: u16, timeout: Duration) -> Result<Vec<SocketAddr>, FetchError> {
    let host = host.to_owned();
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("render-net-h2-resolve".to_owned())
        .spawn(move || {
            let result = (host.as_str(), port)
                .to_socket_addrs()
                .map(std::iter::Iterator::collect::<Vec<_>>)
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        })
        .map_err(|error| FetchError::Transport(format!("resolver spawn failed: {error}")))?;
    match receiver.recv_timeout(timeout) {
        Ok(Ok(addresses)) if !addresses.is_empty() => Ok(addresses),
        Ok(Ok(_)) => Err(FetchError::Dns),
        Ok(Err(message)) => Err(FetchError::Io(message)),
        Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {
            Err(FetchError::Timeout)
        }
    }
}

fn map_h2_error(error: &H2Error, config: &FetchConfig) -> FetchError {
    match error {
        H2Error::ConnectionClosed | H2Error::Io(_) => FetchError::Io(error.to_string()),
        H2Error::GoAway { .. } | H2Error::Protocol { .. } | H2Error::Frame(_) => {
            FetchError::Protocol(error.to_string())
        }
        H2Error::Hpack(_) => FetchError::Protocol(format!("HPACK: {error}")),
        H2Error::StreamReset { .. } | H2Error::Stream { .. } => {
            if let H2Error::Stream { detail, .. } = error
                && detail.starts_with("response body exceeds limit")
            {
                return FetchError::BodyLimitExceeded {
                    limit: config.max_body_bytes,
                };
            }
            FetchError::Protocol(error.to_string())
        }
        H2Error::ResponseBodyTooLarge { .. } => FetchError::BodyLimitExceeded {
            limit: config.max_body_bytes,
        },
        H2Error::HeaderBlockTooLarge { .. } | H2Error::HeaderListTooLarge { .. } => {
            FetchError::HeaderLimitExceeded {
                limit: config.max_header_bytes,
            }
        }
        H2Error::InvalidRequest(message) => FetchError::InvalidRequest(message.clone()),
        H2Error::InvalidConfig(message) => FetchError::Transport(message.clone()),
        H2Error::ConcurrentStreamLimit { limit } => {
            FetchError::Transport(format!("HTTP/2 concurrent stream limit {limit} reached"))
        }
        H2Error::StreamIdExhausted => {
            FetchError::Transport("HTTP/2 stream identifier space exhausted".to_owned())
        }
        H2Error::Unsupported(feature) => {
            FetchError::Transport(format!("unsupported HTTP/2 feature: {feature}"))
        }
    }
}

fn is_h2_forbidden_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-connection"
            | "transfer-encoding"
            | "upgrade"
            | "accept-encoding"
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "send and the polling read loop share one stream id and one set of deadlines"
)]
fn send_h2_request(
    entry: &H2Entry,
    config: &FetchConfig,
    method: HttpMethod,
    url: &Url,
    headers: &[(String, String)],
    body: Option<&[u8]>,
    cancel: &CancelToken,
) -> Result<H2Response, H2Failure> {
    let authority = H2Origin::from_url(url).map_or_else(
        || url.host_str().unwrap_or_default().to_owned(),
        |origin| origin.authority(),
    );
    let mut path = url.path().to_owned();
    if let Some(query) = url.query() {
        if !query.is_empty() {
            path.push('?');
            path.push_str(query);
        }
    } else if path.is_empty() {
        path.push('/');
    }

    let mut request = H2Request::new(method.as_str(), "https", authority, path);
    let mut has_user_agent = false;
    for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        if lower == "user-agent" {
            has_user_agent = true;
        }
        if is_h2_forbidden_header(&lower) {
            continue;
        }
        request = request.with_header(lower, value);
    }
    if !has_user_agent {
        request = request.with_header("user-agent", &config.user_agent);
    }
    // The h2 path has no content decoder, so it must not claim an encoding it
    // cannot undo. Caller-supplied `Accept-Encoding` is dropped above for the
    // same reason; ureq's HTTP/1.1 path keeps its gzip behaviour.
    request = request.with_header("accept-encoding", "identity");
    if let Some(body) = body {
        request = request.with_body(body.to_vec());
    }

    let send_started = Instant::now();
    let stream_id = {
        let mut connection = entry
            .connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if connection.is_closed() {
            return Err(H2Failure {
                error: FetchError::Protocol("HTTP/2 connection is closed".to_owned()),
                phase: FetchPhase::RequestSend,
                evict: true,
                retry: true,
            });
        }
        connection.send_request(&request).map_err(|error| {
            let connection_error = error.is_connection_error()
                || matches!(error, H2Error::ConnectionClosed | H2Error::Io(_));
            H2Failure {
                error: map_h2_error(&error, config),
                phase: FetchPhase::RequestSend,
                evict: connection_error,
                retry: connection_error,
            }
        })?
    };

    let response_timeout = config.effective_response_timeout();
    let body_idle = config.effective_body_idle_timeout();
    let headers_deadline = send_started + response_timeout;
    let mut status_seen = false;
    let mut body_started = Instant::now();
    let mut last_progress = Instant::now();
    let mut last_body_len = 0_usize;

    loop {
        if cancel.is_cancelled() {
            cancel_h2_stream(entry, stream_id);
            return Err(H2Failure {
                error: FetchError::Cancelled,
                phase: FetchPhase::RequestSend,
                evict: false,
                retry: false,
            });
        }

        let now = Instant::now();
        if !status_seen && now >= headers_deadline {
            cancel_h2_stream(entry, stream_id);
            return Err(H2Failure {
                error: FetchError::Timeout,
                phase: FetchPhase::ResponseHeaders,
                evict: false,
                retry: false,
            });
        }

        let mut completed = None;
        let mut stream_error = None;
        let mut connection_error = None;
        {
            let mut connection = entry
                .connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match connection.take_response(stream_id) {
                Ok(Some(response)) => completed = Some(response),
                Ok(None) => {
                    if connection.response_status(stream_id).is_some() && !status_seen {
                        status_seen = true;
                        body_started = Instant::now();
                        last_progress = body_started;
                    }
                    if status_seen {
                        let body_len = connection.response_body_len(stream_id).unwrap_or(0);
                        if body_len > last_body_len {
                            last_body_len = body_len;
                            last_progress = Instant::now();
                        }
                    }
                    match connection.poll_frame() {
                        Ok(_) => {}
                        Err(error) => connection_error = Some(error),
                    }
                    if connection_error.is_none() {
                        match connection.take_response(stream_id) {
                            Ok(response) => completed = response,
                            Err(error) => stream_error = Some(error),
                        }
                        if stream_error.is_none() && status_seen {
                            let body_len = connection.response_body_len(stream_id).unwrap_or(0);
                            if body_len > last_body_len {
                                last_body_len = body_len;
                                last_progress = Instant::now();
                            }
                        }
                    }
                }
                Err(error) => stream_error = Some(error),
            }
        }

        if let Some(response) = completed {
            return Ok(response);
        }
        if let Some(error) = stream_error {
            cancel_h2_stream(entry, stream_id);
            return Err(H2Failure {
                error: map_h2_error(&error, config),
                phase: FetchPhase::BodyTransfer,
                evict: false,
                retry: false,
            });
        }
        if let Some(error) = connection_error {
            let evict = error.is_connection_error()
                || matches!(
                    error,
                    H2Error::ConnectionClosed | H2Error::Io(_) | H2Error::GoAway { .. }
                );
            return Err(H2Failure {
                error: map_h2_error(&error, config),
                phase: if status_seen {
                    FetchPhase::BodyTransfer
                } else {
                    FetchPhase::ResponseHeaders
                },
                evict,
                retry: evict && !status_seen,
            });
        }

        let now = Instant::now();
        if status_seen {
            if now.duration_since(last_progress) >= body_idle {
                cancel_h2_stream(entry, stream_id);
                return Err(H2Failure {
                    error: FetchError::Timeout,
                    phase: FetchPhase::BodyTransfer,
                    evict: false,
                    retry: false,
                });
            }
            let body_elapsed = now.duration_since(body_started);
            if body_elapsed > body_idle {
                let elapsed_seconds = body_elapsed.as_secs().max(1);
                let minimum = MIN_BODY_BYTES_PER_SECOND
                    .saturating_mul(usize::try_from(elapsed_seconds).unwrap_or(usize::MAX));
                if last_body_len < minimum {
                    cancel_h2_stream(entry, stream_id);
                    return Err(H2Failure {
                        error: FetchError::Timeout,
                        phase: FetchPhase::BodyTransfer,
                        evict: false,
                        retry: false,
                    });
                }
            }
        }
    }
}

fn cancel_h2_stream(entry: &H2Entry, stream_id: StreamId) {
    let mut connection = entry
        .connection
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = connection.cancel_stream(stream_id);
}

/// The wire response of one hop, before the transport converts it to a
/// [`FetchResponse`]. HTTP/2 bodies are already fully assembled by the h2
/// connection; HTTP/1.1 bodies are still streamed from ureq.
enum HopResponse {
    Http1(ureq::http::Response<ureq::Body>),
    Http2(H2Response),
}

impl HopResponse {
    fn status(&self) -> u16 {
        match self {
            Self::Http1(response) => response.status().as_u16(),
            Self::Http2(response) => response.status,
        }
    }

    fn headers(&self) -> Vec<Header> {
        match self {
            Self::Http1(response) => response
                .headers()
                .iter()
                .map(|(name, value)| Header {
                    name: name.as_str().to_owned(),
                    value: value.as_bytes().to_vec(),
                })
                .collect(),
            Self::Http2(response) => response
                .headers
                .iter()
                .map(|field| Header {
                    name: String::from_utf8_lossy(&field.name).into_owned(),
                    value: field.value.clone(),
                })
                .collect(),
        }
    }

    fn final_url(&self, current_url: &Url, request_url: &Url) -> Result<Url, FetchError> {
        match self {
            Self::Http1(response) => Ok(normalize_redirect_url(
                Url::parse(&response.get_uri().to_string())
                    .map_err(|error| FetchError::InvalidUrl(error.to_string()))?,
                request_url,
            )),
            Self::Http2(_) => Ok(normalize_redirect_url(current_url.clone(), request_url)),
        }
    }
}

/// Cloneable blocking HTTP transport. Call this on a network thread, or use
/// [`crate::NetworkWorker`] from GUI/event-loop code.
///
/// The agent is behind a lock so an explicit proxy chosen at runtime can be
/// applied without restarting the worker pool or cancelling requests already in
/// flight: each fetch snapshots the agent it starts with, and only later fetches
/// see the new proxy.
///
/// HTTPS hops negotiate ALPN `h2` first. When the origin supports it, requests
/// to that origin share one multiplexed HTTP/2 connection; otherwise the hop
/// uses ureq's HTTP/1.1 path. Hops that inherit an HTTP proxy always stay on
/// HTTP/1.1 (ureq's `CONNECT` tunnel), so h2 can never bypass a configured
/// proxy. Plain `http` URLs also stay on HTTP/1.1.
#[derive(Clone)]
pub struct HttpTransport {
    config: Arc<FetchConfig>,
    agent: Arc<RwLock<TransportAgent>>,
    h2: Arc<H2Pool>,
}

impl fmt::Debug for HttpTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpTransport")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// Whether one hop inherits the transport's proxy or must go direct.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HopProxy {
    Inherit,
    Direct,
}

/// Whether this hop inherits the transport's proxy or bypasses it.
///
/// An implicitly resolved proxy must not capture loopback targets; an
/// explicitly configured one always applies.
fn hop_proxy(implicit_proxy: bool, url: &Url) -> HopProxy {
    if implicit_proxy && is_loopback_target(url) {
        HopProxy::Direct
    } else {
        HopProxy::Inherit
    }
}

fn build_agent(config: &FetchConfig, proxy: Option<ureq::Proxy>) -> ureq::Agent {
    build_agent_with_tls(config, proxy, None)
}

fn build_agent_with_tls(
    config: &FetchConfig,
    proxy: Option<ureq::Proxy>,
    tls: Option<ureq::tls::TlsConfig>,
) -> ureq::Agent {
    let connect_timeout = config.effective_connect_timeout();
    let response_timeout = config.effective_response_timeout();
    let body_idle_timeout = config.effective_body_idle_timeout();
    let builder = ureq::Agent::config_builder()
        .http_status_as_error(false)
        // Redirects are handled here so intermediate response headers are
        // available to the browser cookie jar.
        .max_redirects(0)
        .max_redirects_will_error(true)
        .max_response_header_size(config.max_header_bytes)
        // ---- Phase budgets. Three separate bounds, three separate jobs. ----
        //
        // There is deliberately no end-to-end wall clock
        // (`timeout_global`/`timeout_per_call`): a large stylesheet corpus
        // over a slow link outlives any total budget, and a total budget
        // fails the transfers that matter most.
        .timeout_global(None)
        .timeout_resolve(Some(config.timeout))
        // Connect: the TCP connect, the per-address fallback walk, and (for
        // `https`) the TLS handshake on the socket that connected. Own
        // budget because one black-holed address must not cost a whole
        // request; see `FetchConfig::connect_timeout`.
        .timeout_connect(Some(connect_timeout))
        // Response: the wait for the status line and headers. ureq has no
        // dedicated knob for this, and `Timeout::RecvResponse`'s deadline is
        // re-derived from the recorded *send* time, so the send budget is
        // what actually bounds it. Both send budgets take the same value so
        // a bodyless request and a body-carrying one are bounded identically
        // before and after their write completes; `fetch_transfer` then
        // names the phase. See `FetchConfig::response_timeout`.
        .timeout_send_request(Some(response_timeout))
        .timeout_send_body(Some(response_timeout))
        // `timeout_recv_response` stays `None` on purpose, and this is the
        // one non-obvious knob in the whole agent. It is NOT symmetric with
        // `timeout_recv_body`: `CallTimings::next_timeout` derives
        // `RecvBody`'s deadline from *now* on every read (so it is an
        // idle-read bound), but derives `RecvResponse`'s from the instant
        // the headers completed (so it is a total budget covering headers
        // *and* the entire body). Setting it would therefore cap total body
        // time and fail every large download, which is the exact confusion
        // this layout exists to avoid. The header wait is bounded above
        // instead.
        .timeout_recv_response(None)
        // Body: an idle-read bound, and the only body budget that is safe
        // to push all the way down to the socket. Setting it here means a
        // stalled body read fails inside ureq as
        // `Error::Timeout(RecvBody)` - a real socket deadline, reported as
        // `FetchPhase::BodyTransfer` - rather than only being noticed by
        // `read_bounded_body` watching a channel while its pump thread
        // stays blocked on a dead socket. `read_bounded_body` still applies
        // the same bound independently, so the two must agree, and the
        // pump's is the one that stays authoritative for cancellation and
        // the minimum-progress floor.
        .timeout_recv_body(Some(body_idle_timeout))
        // ureq 3.3's Brotli reader can finish decoding before it drains
        // the length-delimited wire body, so the connection never returns
        // to its pool. Gzip keeps compression and reliably reuses the
        // connection across the many same-origin assets on real pages.
        .accept_encoding("gzip")
        // The idle-connection ceilings, set here rather than left to ureq's
        // defaults (10 total, 3 per host). A per-host default of 3 is below
        // the per-origin concurrency this crate allows, so on a page that
        // pulls many assets from one origin - the shape of every stylesheet
        // corpus - every wave after the first reopens the connections the
        // pool just discarded. See `FetchConfig::idle_connections_per_origin`.
        .max_idle_connections(config.idle_connections_total)
        .max_idle_connections_per_host(config.idle_connections_per_origin)
        .user_agent(config.user_agent.clone())
        .proxy(proxy);
    let builder = if let Some(tls) = tls {
        builder.tls_config(tls)
    } else {
        builder
    };
    builder.build().into()
}

impl HttpTransport {
    /// Creates a transport with verified rustls HTTPS and bounded headers.
    ///
    /// A proxy resolved from the environment (`ALL_PROXY`/`HTTPS_PROXY`/`HTTP_PROXY`
    /// with `NO_PROXY`) or, on Windows, the system proxy settings applies to
    /// every request issued through this transport, except requests to loopback
    /// targets.
    ///
    /// Loopback is excluded because that is what browsers do by default
    /// (Chrome's and Firefox's implicit-proxy bypass lists both start with
    /// `localhost` and the loopback ranges), and because a proxy that cannot
    /// serve a loopback address costs a whole extra hop per request: a local
    /// development server or an on-device endpoint is not a remote resource.
    /// A proxy the caller hands to [`HttpTransport::with_proxy`] is explicit
    /// policy and is used for every target, loopback included.
    #[must_use]
    pub fn new(config: FetchConfig) -> Self {
        let proxy = ureq::Proxy::try_from_env();
        if let Some(proxy) = &proxy {
            eprintln!(
                "render-net proxy: {}:{} (from environment or system settings)",
                proxy.host(),
                proxy.port(),
            );
        }
        let implicit_proxy = proxy.is_some();
        let has_proxy = proxy.is_some();
        let agent = build_agent(&config, proxy);
        let h2 = Arc::new(H2Pool::new(&config));
        Self {
            config: Arc::new(config),
            agent: Arc::new(RwLock::new(TransportAgent {
                agent,
                implicit_proxy,
                has_proxy,
            })),
            h2,
        }
    }

    /// Creates a transport with an explicit browser-configured HTTP proxy.
    ///
    /// Unlike a proxy inherited from the environment, an explicit proxy is
    /// used for every target, including loopback addresses.
    #[must_use]
    pub fn with_http_proxy(config: FetchConfig, proxy: Option<HttpProxyConfig>) -> Self {
        let has_proxy = proxy.is_some();
        let agent = build_agent(&config, proxy.map(|proxy| proxy.proxy));
        let h2 = Arc::new(H2Pool::new(&config));
        Self {
            config: Arc::new(config),
            agent: Arc::new(RwLock::new(TransportAgent {
                agent,
                implicit_proxy: false,
                has_proxy,
            })),
            h2,
        }
    }

    /// Replaces the proxy used by subsequent requests.
    ///
    /// Requests already in flight keep the agent they started with, so saving a
    /// proxy setting neither cancels a page load nor tears down its connections.
    /// Passing `None` means direct connections; it does not re-read the
    /// environment.
    pub fn set_http_proxy(&self, proxy: Option<HttpProxyConfig>) {
        let has_proxy = proxy.is_some();
        let agent = build_agent(&self.config, proxy.map(|proxy| proxy.proxy));
        let mut state = self
            .agent
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.agent = agent;
        state.implicit_proxy = false;
        state.has_proxy = has_proxy;
    }

    /// Creates a transport that routes every request through `proxy` when set.
    ///
    /// This is the injection point for callers with their own proxy policy;
    /// [`HttpTransport::new`] resolves the proxy from the environment and the
    /// platform instead. Both constructors use [`build_agent`], so the proxy and
    /// non-proxy paths cannot drift apart in their phase budgets: the only
    /// difference is the `implicit_proxy` flag that decides whether a loopback
    /// target bypasses the proxy.
    #[must_use]
    pub fn with_proxy(config: FetchConfig, proxy: Option<ureq::Proxy>) -> Self {
        let has_proxy = proxy.is_some();
        let agent = build_agent(&config, proxy);
        let h2 = Arc::new(H2Pool::new(&config));
        Self {
            config: Arc::new(config),
            agent: Arc::new(RwLock::new(TransportAgent {
                agent,
                implicit_proxy: false,
                has_proxy,
            })),
            h2,
        }
    }

    /// Test-only constructor that points both TLS stacks at a private root
    /// store. It never disables certificate verification.
    #[cfg(test)]
    fn with_test_roots(
        config: FetchConfig,
        roots: RootCertStore,
        tls: ureq::tls::TlsConfig,
    ) -> Self {
        let agent = build_agent_with_tls(&config, None, Some(tls));
        let h2 = Arc::new(H2Pool::with_roots(&config, roots));
        Self {
            config: Arc::new(config),
            agent: Arc::new(RwLock::new(TransportAgent {
                agent,
                implicit_proxy: false,
                has_proxy: false,
            })),
            h2,
        }
    }

    #[must_use]
    pub fn config(&self) -> &FetchConfig {
        &self.config
    }

    /// Returns the agent a fetch should use, whether its proxy is implicit,
    /// and whether any proxy is configured at all.
    fn agent_snapshot(&self) -> (ureq::Agent, bool, bool) {
        let state = self
            .agent
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.agent.clone(), state.implicit_proxy, state.has_proxy)
    }

    /// Performs one bounded HTTP(S) request or `data:` URL decode.
    ///
    /// Every call reaches a terminal outcome: either a [`FetchResponse`], or a
    /// [`FetchError`] that names the [`FetchPhase`] it failed in and how long it
    /// had been running. Both are reported to
    /// [`FetchConfig::observer`](FetchConfig::observer), so a stalled request is
    /// never silent.
    ///
    /// # Errors
    ///
    /// Returns a typed [`FetchError`] for cancellation, invalid schemes,
    /// invalid request construction, configured limit violations, TLS
    /// verification, and transport failures.
    pub fn fetch(&self, request: &FetchRequest, cancel: &CancelToken) -> FetchResult {
        let started = Instant::now();
        let outcome = self.fetch_transfer(request, cancel);
        self.report(request, started, &outcome);
        outcome
    }

    /// Reports one request's terminal outcome to the configured observer.
    fn report(&self, request: &FetchRequest, started: Instant, outcome: &FetchResult) {
        let elapsed = started.elapsed();
        let event = match outcome {
            Ok(response) => FetchEvent::Completed {
                method: request.method,
                url: &request.url,
                status: response.status,
                body_bytes: response.body.len(),
                elapsed,
            },
            Err(error) => FetchEvent::Failed {
                method: request.method,
                url: &request.url,
                elapsed,
                error,
            },
        };
        self.config.observer.on_fetch_event(&event);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "redirect/cookie/timeout handling reads as one pipeline"
    )]
    fn fetch_transfer(&self, request: &FetchRequest, cancel: &CancelToken) -> FetchResult {
        validate_request(request)?;
        if request.url.scheme() == "data" {
            return self.fetch_data_url(request, cancel);
        }
        validate_scheme(&request.url)?;
        if cancel.is_cancelled() {
            return Err(FetchError::Cancelled);
        }
        // One snapshot for the whole redirect chain: changing the proxy while a
        // chain is in flight applies to later fetches, not halfway through this
        // one.
        let (agent, implicit_proxy, proxy_configured) = self.agent_snapshot();

        let mut current_url = request.url.clone();
        // Redirect method semantics are applied per hop below; the original
        // request stays untouched.
        let mut current_method = request.method;
        let mut hop_body: Option<&[u8]> = request.body.as_deref();
        let mut redirect_chain = vec![request.url.clone()];
        let mut redirects = Vec::new();
        let mut redirect_count = 0;
        // Cookies set by responses inside this chain. RFC 6265 application is
        // hop-by-hop: a `Set-Cookie` from an earlier response must decorate the
        // next request when its domain/path rules match. The jar is per fetch,
        // so the transport itself stays stateless; the caller's jar still sees
        // the same cookies via `absorb_response`.
        let mut hop_cookies = CookieJar::default();
        let started = Instant::now();

        loop {
            if cancel.is_cancelled() {
                return Err(FetchError::Cancelled);
            }

            // Custom headers first so the transport-managed fields below
            // cannot duplicate them; a caller-supplied name wins.
            let mut hop_headers = request.headers.clone();
            {
                let mut managed = |name: &str, value: String| {
                    if !hop_headers
                        .iter()
                        .any(|(existing, _)| existing.eq_ignore_ascii_case(name))
                    {
                        hop_headers.push((name.to_owned(), value));
                    }
                };
                if let Some(accept) = request.accept.as_deref() {
                    managed("Accept", accept.to_owned());
                }
                // Chromium sends client hints on every navigation; sites use
                // them (together with the UA) to select a client bundle and
                // feature set. Without them a Chromium UA looks like a browser
                // that predates these hints, and Bing serves an older variant.
                managed(
                    "Sec-CH-UA",
                    "\"Not_A Brand\";v=\"8\", \"Chromium\";v=\"120\", \"Google Chrome\";v=\"120\""
                        .to_owned(),
                );
                managed("Sec-CH-UA-Mobile", "?0".to_owned());
                managed("Sec-CH-UA-Platform", "\"Windows\"".to_owned());
                managed("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8".to_owned());
                // The caller-provided Cookie header was computed for the
                // original URL. Reusing it only on same-origin hops avoids
                // leaking it to a cross-origin redirect, while cookies the
                // chain itself set flow by their own domain/path rules either
                // way.
                let caller_cookie = same_origin(&current_url, &request.url)
                    .then_some(request.cookie.as_deref())
                    .flatten();
                if let Some(cookie) = combined_cookie_header(
                    caller_cookie,
                    hop_cookies.cookie_header(&current_url).as_deref(),
                ) {
                    managed("Cookie", cookie);
                }
                if let Some(byte_range) = request.byte_range {
                    managed("Range", byte_range.header_value());
                }
                if redirect_count == 0
                    && same_origin(&current_url, &request.url)
                    && let Some(validators) = request.cache_validators.as_ref()
                {
                    if let Some(etag) = validators.etag.as_deref() {
                        managed("If-None-Match", etag.to_owned());
                    }
                    if let Some(last_modified) = validators.last_modified.as_deref() {
                        managed("If-Modified-Since", last_modified.to_owned());
                    }
                }
            }
            let response = self.send_hop(
                &agent,
                implicit_proxy,
                proxy_configured,
                current_method,
                &current_url,
                hop_body,
                &hop_headers,
                cancel,
            )?;

            if cancel.is_cancelled() {
                return Err(FetchError::Cancelled);
            }

            let status = HttpStatus(response.status());
            let headers = response.headers();
            if is_redirect_status(status)
                && self.config.redirect_limit > 0
                && let Some(location) = header_text(&headers, "location")
            {
                if redirect_count >= self.config.redirect_limit {
                    return Err(FetchError::RedirectLimitExceeded {
                        limit: self.config.redirect_limit,
                    });
                }
                // Everything except the final body transfer keeps the
                // configured budget in total; a redirect chain must not multiply
                // the per-stage timeouts unboundedly. The redirect response
                // arrived, so the budget ran out while acting on it.
                if started.elapsed() >= self.config.timeout {
                    return Err(FetchError::Timeout
                        .in_phase(FetchPhase::ResponseHeaders, started.elapsed()));
                }
                // Redirect method semantics (fetch/HTTP): 301 and 302
                // historically rewrite `POST` to `GET`, and 303 rewrites
                // `POST` and `PUT`; the body is dropped whenever the method is
                // rewritten. 307 and 308 preserve both.
                let rewritten = match status.as_u16() {
                    301 | 302 => current_method == HttpMethod::Post,
                    303 => matches!(current_method, HttpMethod::Post | HttpMethod::Put),
                    _ => false,
                };
                if rewritten {
                    current_method = HttpMethod::Get;
                    hop_body = None;
                }
                let next_url = current_url
                    .join(location.trim())
                    .map_err(|error| FetchError::InvalidUrl(error.to_string()))?;
                validate_scheme(&next_url)?;
                absorb_hop_set_cookies(&mut hop_cookies, &current_url, &headers);
                redirects.push(RedirectResponse {
                    url: current_url.clone(),
                    status,
                    headers,
                });
                current_url = next_url.clone();
                redirect_chain.push(next_url);
                redirect_count = redirect_count.saturating_add(1);
                continue;
            }

            if matches!(&response, HopResponse::Http2(_))
                && let Some(encoding) = header_text(&headers, "content-encoding")
                && !encoding.eq_ignore_ascii_case("identity")
            {
                return Err(FetchError::Protocol(format!(
                    "HTTP/2 response used content-encoding {encoding:?} despite an identity request"
                )));
            }
            let final_url = response.final_url(&current_url, &request.url)?;
            if let Some(last) = redirect_chain.last_mut() {
                *last = final_url.clone();
            }
            let content_type = header_text(&headers, "content-type").and_then(parse_content_type);
            let body = match response {
                HopResponse::Http1(response) => {
                    self.read_bounded_body(response.into_body().into_reader(), cancel)?
                }
                HopResponse::Http2(response) => response.body,
            };
            // A `HEAD` response has no body by definition; stray bytes from a
            // non-conforming origin are drained above but never surfaced.
            let body = if current_method == HttpMethod::Head {
                Vec::new()
            } else {
                body
            };

            return Ok(FetchResponse {
                requested_url: request.url.clone(),
                final_url,
                redirect_chain,
                redirects,
                status,
                headers,
                content_type,
                body,
            });
        }
    }

    /// Sends one hop, preferring HTTP/2 when the origin is direct HTTPS and
    /// TLS ALPN selected `h2`, and otherwise using the ureq HTTP/1.1 path.
    #[allow(
        clippy::too_many_arguments,
        reason = "one hop already carries the prepared URL, headers, body, and proxy policy"
    )]
    fn send_hop(
        &self,
        agent: &ureq::Agent,
        implicit_proxy: bool,
        proxy_configured: bool,
        method: HttpMethod,
        url: &Url,
        body: Option<&[u8]>,
        headers: &[(String, String)],
        cancel: &CancelToken,
    ) -> Result<HopResponse, FetchError> {
        let hop_started = Instant::now();
        let proxy = hop_proxy(implicit_proxy, url);
        let direct = !proxy_configured || proxy == HopProxy::Direct;
        let body_fits_h2 = body.is_none_or(|bytes| bytes.len() <= H2_MAX_INLINE_BODY);
        if direct
            && body_fits_h2
            && let Some(origin) = H2Origin::from_url(url)
            && !self.h2.is_http1_only(&origin)
        {
            match self
                .h2
                .send(&origin, &self.config, method, url, headers, body, cancel)
            {
                Ok(response) => return Ok(HopResponse::Http2(response)),
                Err(H2AttemptError::FallbackHttp1(_reason)) => {
                    // The probe completed and the server chose HTTP/1.1 (or
                    // sent no ALPN). Remember it so later requests do not pay
                    // for the probe again; the request itself continues below
                    // on ureq's HTTP/1.1 stack.
                    self.h2.mark_http1_only(&origin);
                }
                Err(H2AttemptError::Failed(failure)) => {
                    return Err(failure.error.in_phase(failure.phase, hop_started.elapsed()));
                }
            }
        }

        let response =
            Self::send_http1_hop(agent, proxy, method, url, body, headers).map_err(|error| {
                let (error, mut phase) = classify_ureq_error(&error, &self.config);
                // ureq reports a `SendRequest` timeout against the send-request
                // budget, which this transport deliberately also uses to bound
                // the wait for the response headers (see the phase-budget layout
                // in `HttpTransport::with_proxy`), so that reason is ambiguous by
                // itself. A request without a body writes a few hundred bytes
                // and leaves the send phase immediately, so for those the budget
                // can only have expired while waiting for the origin to answer.
                if body.is_none() && phase == FetchPhase::RequestSend {
                    phase = FetchPhase::ResponseHeaders;
                }
                error.in_phase(phase, hop_started.elapsed())
            })?;
        Ok(HopResponse::Http1(response))
    }

    /// Sends one hop through ureq, applying the method and prepared headers.
    /// Bodyless methods use `call` (no `Content-Length` on the wire);
    /// body-carrying methods send the bytes and let ureq derive
    /// `Content-Length` from the body.
    fn send_http1_hop(
        agent: &ureq::Agent,
        proxy: HopProxy,
        method: HttpMethod,
        url: &Url,
        body: Option<&[u8]>,
        headers: &[(String, String)],
    ) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let uri = url.as_str();
        match (method, body) {
            (HttpMethod::Get, _) => decorate(agent.get(uri), headers, proxy).call(),
            (HttpMethod::Head, _) => decorate(agent.head(uri), headers, proxy).call(),
            (HttpMethod::Delete, None) => decorate(agent.delete(uri), headers, proxy).call(),
            // DELETE with a body is legal HTTP but needs ureq's explicit
            // escape hatch.
            (HttpMethod::Delete, Some(bytes)) => {
                decorate(agent.delete(uri).force_send_body(), headers, proxy).send(bytes)
            }
            (HttpMethod::Post, Some(bytes)) => {
                decorate(agent.post(uri), headers, proxy).send(bytes)
            }
            (HttpMethod::Post, None) => decorate(agent.post(uri), headers, proxy).send_empty(),
            (HttpMethod::Put, Some(bytes)) => decorate(agent.put(uri), headers, proxy).send(bytes),
            (HttpMethod::Put, None) => decorate(agent.put(uri), headers, proxy).send_empty(),
            (HttpMethod::Patch, Some(bytes)) => {
                decorate(agent.patch(uri), headers, proxy).send(bytes)
            }
            (HttpMethod::Patch, None) => decorate(agent.patch(uri), headers, proxy).send_empty(),
            (HttpMethod::Options, None) => decorate(agent.options(uri), headers, proxy).call(),
            (HttpMethod::Options, Some(bytes)) => {
                decorate(agent.options(uri).force_send_body(), headers, proxy).send(bytes)
            }
        }
    }

    fn fetch_data_url(&self, request: &FetchRequest, cancel: &CancelToken) -> FetchResult {
        if cancel.is_cancelled() {
            return Err(FetchError::Cancelled);
        }
        let (content_type, body) = decode_data_url(&request.url, self.config.max_body_bytes)?;
        if cancel.is_cancelled() {
            return Err(FetchError::Cancelled);
        }
        Ok(FetchResponse {
            requested_url: request.url.clone(),
            final_url: request.url.clone(),
            redirect_chain: vec![request.url.clone()],
            redirects: Vec::new(),
            status: HttpStatus(200),
            headers: Vec::new(),
            content_type: Some(content_type),
            body,
        })
    }

    /// Reads the response body within [`FetchConfig::max_body_bytes`], keeping
    /// cooperative cancellation responsive and bounding how long a transfer can
    /// stall or trickle.
    ///
    /// There is deliberately no whole-body wall clock: a transfer that keeps
    /// making progress must finish regardless of total duration (a large CDN
    /// stylesheet over a slow link). Instead, once the idle bound has elapsed
    /// overall the transfer must average at least
    /// [`MIN_BODY_BYTES_PER_SECOND`], and each individual read may idle at most
    /// that bound - [`FetchConfig::body_idle_timeout`], the same value handed to
    /// ureq as `timeout_recv_body` - before the transfer fails.
    ///
    /// ureq's body budget is a socket read deadline, so a stalled read normally
    /// surfaces here as a ureq `RecvBody` timeout and the pump unwinds on its
    /// own. This loop applies the same bound again, independently, because it is
    /// the thing that also serves cancellation and the minimum-progress floor,
    /// and because a decoder that stalls without touching the socket (a gzip
    /// member that never completes) would otherwise be unbounded. Whichever
    /// fires first wins, and both produce the same terminal outcome.
    fn read_bounded_body(
        &self,
        reader: impl Read + Send + 'static,
        cancel: &CancelToken,
    ) -> Result<Vec<u8>, FetchError> {
        let idle_timeout = self.config.effective_body_idle_timeout();
        let (chunk_tx, chunk_rx) = mpsc::channel::<std::io::Result<Vec<u8>>>();
        let pump = thread::Builder::new()
            .name("render-net-body-pump".to_owned())
            .spawn(move || {
                let mut reader = reader;
                let mut chunk = [0_u8; 16 * 1024];
                loop {
                    match reader.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(count) => {
                            if chunk_tx.send(Ok(chunk[..count].to_vec())).is_err() {
                                break;
                            }
                        }
                        Err(error) => {
                            let _ = chunk_tx.send(Err(error));
                            break;
                        }
                    }
                }
            })
            .map_err(|error| FetchError::Transport(format!("body pump spawn failed: {error}")))?;

        let limit = self.config.max_body_bytes;
        let mut body = Vec::with_capacity(limit.min(64 * 1024));
        let mut pump = Some(pump);
        let started = Instant::now();
        let result = loop {
            if cancel.is_cancelled() {
                break Err(FetchError::Cancelled);
            }
            let chunk = match chunk_rx.recv_timeout(idle_timeout) {
                Ok(Ok(chunk)) => chunk,
                Ok(Err(error)) => break Err(self.map_body_read_error(&error)),
                // A read that idles past the budget is a stalled transfer; the
                // minimum-progress floor below covers transfers that trickle.
                Err(mpsc::RecvTimeoutError::Timeout) => break Err(FetchError::Timeout),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // The pump exited: either a clean EOF or a panic.
                    break match pump.take().expect("pump handle before disconnect").join() {
                        Ok(()) => Ok(std::mem::take(&mut body)),
                        Err(_) => Err(FetchError::Protocol(
                            "response body reader stopped unexpectedly".into(),
                        )),
                    };
                }
            };
            let remaining = limit.saturating_sub(body.len());
            if chunk.len() > remaining {
                break Err(FetchError::BodyLimitExceeded { limit });
            }
            body.extend_from_slice(&chunk);
            let elapsed = started.elapsed();
            if elapsed > idle_timeout {
                let elapsed_seconds = usize::try_from(elapsed.as_secs()).unwrap_or(usize::MAX);
                if body.len() < MIN_BODY_BYTES_PER_SECOND.saturating_mul(elapsed_seconds) {
                    break Err(FetchError::Timeout);
                }
            }
        };
        // On success the pump has already hit EOF, so joining is instant; on
        // failure paths the pump may legitimately still be blocked on its
        // socket and must not be waited on.
        if result.is_ok() {
            let _ = pump.take().map(std::thread::JoinHandle::join);
        }
        // Every failure from here happened while reading the body, so it is the
        // one phase this loop can attribute exactly. Cancellation stays
        // untagged.
        result.map_err(|error| error.in_phase(FetchPhase::BodyTransfer, started.elapsed()))
    }

    /// Classifies a failure surfaced by the body reader. ureq wraps its typed
    /// errors inside `std::io::Error` (decoders pass them through verbatim), so
    /// a stalled body read is still a [`FetchError::Timeout`], not a generic
    /// I/O failure.
    fn map_body_read_error(&self, error: &std::io::Error) -> FetchError {
        error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<ureq::Error>())
            .map_or_else(
                || FetchError::Io(error.to_string()),
                |error| classify_ureq_error(error, &self.config).0,
            )
    }
}

/// Validates request-level invariants before any I/O: a body only on methods
/// that can carry one, and no caller-supplied reserved headers.
fn validate_request(request: &FetchRequest) -> Result<(), FetchError> {
    if request.body.is_some() && !request.method.allows_body() {
        return Err(FetchError::InvalidRequest(format!(
            "{} requests cannot carry a body",
            request.method.as_str()
        )));
    }
    for (name, _) in &request.headers {
        if RESERVED_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(FetchError::ReservedHeader(name.clone()));
        }
    }
    Ok(())
}

/// Applies the prepared hop headers to a ureq request builder, plus a
/// per-request proxy override when the hop must not use the transport's proxy.
fn decorate<TBuilder>(
    builder: ureq::RequestBuilder<TBuilder>,
    headers: &[(String, String)],
    proxy: HopProxy,
) -> ureq::RequestBuilder<TBuilder> {
    let builder = headers.iter().fold(builder, |builder, (name, value)| {
        builder.header(name.as_str(), value.as_str())
    });
    match proxy {
        // A request-level `proxy(None)` is the only way to keep one connection
        // pool and still reach a loopback target directly: ureq keys pooled
        // connections by proxy as well as authority, so a direct loopback
        // connection never collides with a proxied one.
        HopProxy::Direct => builder.config().proxy(None).build(),
        HopProxy::Inherit => builder,
    }
}

/// Whether a URL points at this machine.
///
/// Covers the loopback addresses and the `localhost` names RFC 6761 reserves
/// for them, which is the set browsers exempt from an implicit proxy.
fn is_loopback_target(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain == "localhost" || domain.ends_with(".localhost")
        }
        None => false,
    }
}

/// Maps a typed ureq failure onto the transport's error vocabulary and the
/// transport phase it happened in.
///
/// ureq names a phase only for its own timeouts. Every other failure is mapped
/// from its kind, which is exact for name resolution, TLS, and protocol
/// failures; a non-timeout socket failure is reported against the connect
/// phase because that is where the overwhelming majority of them come from
/// (refused, reset, unreachable, or an address that never answered).
fn classify_ureq_error(error: &ureq::Error, config: &FetchConfig) -> (FetchError, FetchPhase) {
    match error {
        ureq::Error::TooManyRedirects => (
            FetchError::RedirectLimitExceeded {
                limit: config.redirect_limit,
            },
            FetchPhase::ResponseHeaders,
        ),
        ureq::Error::LargeResponseHeader(_, _) => (
            FetchError::HeaderLimitExceeded {
                limit: config.max_header_bytes,
            },
            FetchPhase::ResponseHeaders,
        ),
        ureq::Error::Timeout(reason) => (FetchError::Timeout, phase_of_timeout(*reason)),
        ureq::Error::HostNotFound => (FetchError::Dns, FetchPhase::Dns),
        ureq::Error::Tls(message) => (
            FetchError::Tls((*message).to_owned()),
            FetchPhase::TlsHandshake,
        ),
        ureq::Error::Rustls(error) => {
            (FetchError::Tls(error.to_string()), FetchPhase::TlsHandshake)
        }
        ureq::Error::TlsRequired => (
            FetchError::Tls("TLS was required but unavailable".into()),
            FetchPhase::TlsHandshake,
        ),
        ureq::Error::ConnectProxyFailed(reason) => (
            FetchError::Transport(format!("CONNECT proxy failed: {reason}")),
            FetchPhase::TcpConnect,
        ),
        ureq::Error::BadUri(message) => {
            (FetchError::InvalidUrl(message.clone()), FetchPhase::Request)
        }
        ureq::Error::Protocol(error) => (
            FetchError::Protocol(error.to_string()),
            FetchPhase::ResponseHeaders,
        ),
        ureq::Error::Io(error) => (map_io_error(error), FetchPhase::TcpConnect),
        // A failure ureq does not attribute to a phase. It is reported against
        // the request phase rather than guessed at.
        other => (
            FetchError::Transport(other.to_string()),
            FetchPhase::Request,
        ),
    }
}

/// The transport phase whose budget expired, from ureq's own reason.
fn phase_of_timeout(reason: ureq::Timeout) -> FetchPhase {
    match reason {
        ureq::Timeout::Resolve => FetchPhase::Dns,
        // ureq's connect phase covers opening the connection, including its
        // per-address fallback and the TLS handshake that follows on the
        // socket that connected. "tcp connect" is the actionable label: it is
        // the phase a stalled or filtered address shows up in.
        ureq::Timeout::Connect => FetchPhase::TcpConnect,
        ureq::Timeout::SendRequest | ureq::Timeout::SendBody | ureq::Timeout::Await100 => {
            FetchPhase::RequestSend
        }
        ureq::Timeout::RecvResponse => FetchPhase::ResponseHeaders,
        ureq::Timeout::RecvBody => FetchPhase::BodyTransfer,
        // This crate never configures the global or per-call budgets
        // (`timeout_global`/`timeout_per_call` stay unset), so they cannot be
        // why a request failed; neither can any reason ureq adds later.
        ureq::Timeout::Global | ureq::Timeout::PerCall | _ => FetchPhase::Request,
    }
}

/// Absorbs `Set-Cookie` fields from one redirect response into the per-fetch
/// jar so they decorate the next hop.
fn absorb_hop_set_cookies(jar: &mut CookieJar, origin: &Url, headers: &[Header]) {
    for header in headers {
        if header.name.eq_ignore_ascii_case("set-cookie")
            && let Ok(value) = std::str::from_utf8(&header.value)
        {
            // A single invalid cookie must never break the redirect chain;
            // rejection details belong to the caller's jar via absorb_response.
            let _ignored = jar.set_cookie(origin, value);
        }
    }
}

/// Combines the caller-provided `Cookie` header with cookies the redirect chain
/// set so far, producing the header for the next hop.
///
/// Per RFC 6265 a later `Set-Cookie` replaces an earlier cookie with the same
/// (case-sensitive) name, so the caller's pairs are dropped when the chain set
/// a cookie of that name. Pair order is otherwise preserved, then followed by
/// the hop cookies.
fn combined_cookie_header(caller: Option<&str>, hop: Option<&str>) -> Option<String> {
    let Some(hop) = hop else {
        return caller.map(str::to_owned);
    };
    let Some(caller) = caller else {
        return Some(hop.to_owned());
    };
    let hop_names = hop
        .split(';')
        .filter_map(|pair| pair.split_once('=').map(|(name, _)| name.trim()))
        .collect::<Vec<_>>();
    let retained = caller
        .split(';')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
        .filter(|pair| {
            let name = pair.split_once('=').map_or(*pair, |(name, _)| name.trim());
            !hop_names.contains(&name)
        })
        .collect::<Vec<_>>();
    if retained.is_empty() {
        return Some(hop.to_owned());
    }
    let mut combined = retained.join("; ");
    combined.push_str("; ");
    combined.push_str(hop);
    Some(combined)
}

fn is_redirect_status(status: HttpStatus) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

fn header_text<'a>(headers: &'a [Header], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .and_then(|header| std::str::from_utf8(&header.value).ok())
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme().eq_ignore_ascii_case(right.scheme())
        && left
            .host_str()
            .zip(right.host_str())
            .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
        && left.port_or_known_default() == right.port_or_known_default()
}

fn normalize_redirect_url(mut url: Url, request_url: &Url) -> Url {
    let path = url.path().to_owned();
    let marker = format!("//{}/", request_url.host_str().unwrap_or_default());
    if path
        .get(..marker.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&marker))
    {
        let corrected = &path[marker.len() - 1..];
        url.set_path(corrected);
    }
    url
}

fn map_io_error(error: &std::io::Error) -> FetchError {
    if let Some(error) = rustls_error_from_io(error) {
        FetchError::Tls(error.to_string())
    } else {
        FetchError::Io(error.to_string())
    }
}

fn rustls_error_from_io(error: &std::io::Error) -> Option<&rustls::Error> {
    let mut current = error
        .get_ref()
        .map(|source| source as &(dyn std::error::Error + 'static));
    while let Some(source) = current {
        if let Some(error) = source.downcast_ref::<rustls::Error>() {
            return Some(error);
        }
        current = source.source();
    }
    None
}

fn validate_scheme(url: &Url) -> Result<(), FetchError> {
    match url.scheme() {
        "http" | "https" => Ok(()),
        scheme => Err(FetchError::UnsupportedScheme(scheme.to_owned())),
    }
}

fn decode_data_url(url: &Url, body_limit: usize) -> Result<(ContentType, Vec<u8>), FetchError> {
    let payload = url.path();
    let (metadata, encoded_body) = payload.split_once(',').ok_or_else(|| {
        FetchError::InvalidUrl("data URL is missing its required comma separator".to_owned())
    })?;
    let (media_type, is_base64) = data_media_type(metadata)?;
    let body = if is_base64 {
        let encoded_body = percent_decode_data(encoded_body)?;
        // Data URLs copied out of HTML often carry ASCII whitespace in the
        // base64 payload (line wrapping, a stray space before `==`). Browsers
        // strip it before decoding; keeping it made valid images fail.
        let compact: Vec<u8> = encoded_body
            .into_iter()
            .filter(|byte| !matches!(*byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0c))
            .collect();
        base64::engine::general_purpose::STANDARD
            .decode(compact)
            .map_err(|error| FetchError::InvalidUrl(format!("invalid base64 data URL: {error}")))?
    } else {
        percent_decode_data(encoded_body)?
    };
    if body.len() > body_limit {
        return Err(FetchError::BodyLimitExceeded { limit: body_limit });
    }
    Ok((
        parse_content_type(&media_type).expect("validated data media type"),
        body,
    ))
}

fn data_media_type(metadata: &str) -> Result<(String, bool), FetchError> {
    let mut parts = metadata.split(';');
    let first = parts.next().unwrap_or_default();
    let mut media_type = if first.is_empty() {
        "text/plain".to_owned()
    } else if first.contains('/') {
        first.to_ascii_lowercase()
    } else {
        return Err(FetchError::InvalidUrl(format!(
            "invalid data URL media type '{first}'"
        )));
    };
    let mut is_base64 = false;
    for parameter in parts {
        if parameter.eq_ignore_ascii_case("base64") {
            if is_base64 {
                return Err(FetchError::InvalidUrl(
                    "data URL contains more than one base64 marker".to_owned(),
                ));
            }
            is_base64 = true;
        } else if !parameter.is_empty() {
            media_type.push(';');
            media_type.push_str(parameter);
        }
    }
    if first.is_empty() && !media_type.contains("charset=") {
        media_type.push_str(";charset=US-ASCII");
    }
    if parse_content_type(&media_type).is_none() {
        return Err(FetchError::InvalidUrl(format!(
            "invalid data URL content type '{media_type}'"
        )));
    }
    Ok((media_type, is_base64))
}

fn percent_decode_data(value: &str) -> Result<Vec<u8>, FetchError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let high = bytes.get(index + 1).and_then(|byte| hex_value(*byte));
        let low = bytes.get(index + 2).and_then(|byte| hex_value(*byte));
        let (Some(high), Some(low)) = (high, low) else {
            return Err(FetchError::InvalidUrl(
                "data URL contains an invalid percent escape".to_owned(),
            ));
        };
        decoded.push((high << 4) | low);
        index += 3;
    }
    Ok(decoded)
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_content_type(value: &str) -> Option<ContentType> {
    let mut parts = value.split(';');
    let media_type = parts.next()?.trim().to_ascii_lowercase();
    if media_type.is_empty() || !media_type.contains('/') {
        return None;
    }
    let charset = parts.find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        name.trim().eq_ignore_ascii_case("charset").then(|| {
            value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_ascii_lowercase()
        })
    });
    Some(ContentType {
        media_type,
        charset,
    })
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::io::{Read as _, Write as _};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use base64::Engine as _;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs1KeyDer};
    use rustls::{RootCertStore, ServerConfig, ServerConnection, StreamOwned};

    use super::{
        CancelToken, ContentType, FetchConfig, FetchError, FetchPhase, FetchRequest, HttpTransport,
        map_io_error, normalize_redirect_url, parse_content_type, phase_of_timeout,
    };
    use crate::h2::{
        ErrorCode, Frame, FrameKind, HeaderField, HpackDecoder, HpackEncoder, Setting, SettingId,
        StreamId,
    };
    use url::Url;

    #[test]
    fn the_idle_ceiling_matches_the_per_origin_concurrency_policy() {
        // The idle-connection ceiling is derived from the per-origin
        // concurrency the batch policy applies, so the two must not drift: too
        // few idle connections and every wave after the first reopens what the
        // pool just discarded, too many and sockets are held that could never be
        // reused. Both defaults read the same constant, and this asserts they
        // still agree through the policy rather than only through the constant.
        let per_origin = crate::DEFAULT_PER_ORIGIN_CONCURRENCY;
        let config = FetchConfig::default();
        assert_eq!(config.idle_connections_per_origin, per_origin);
        assert_eq!(
            config.idle_connections_total,
            per_origin * crate::ORIGINS_BEFORE_TOTAL_CEILING
        );

        let options = crate::BatchOptions::default();
        let url = Url::parse("https://cdn.example/assets/sheet.css").expect("test URL");
        assert_eq!(
            options
                .origin_policy
                .max_concurrency(&crate::Origin::from_url(&url)),
            per_origin,
            "the batch's per-origin limit and the pool's idle ceiling must be one decision"
        );
        assert!(
            options.max_concurrency >= per_origin,
            "a batch window narrower than the per-origin limit would serialise one origin"
        );
    }

    #[test]
    fn expired_phase_budgets_map_onto_transport_phases() {
        assert_eq!(phase_of_timeout(ureq::Timeout::Resolve), FetchPhase::Dns);
        assert_eq!(
            phase_of_timeout(ureq::Timeout::Connect),
            FetchPhase::TcpConnect
        );
        assert_eq!(
            phase_of_timeout(ureq::Timeout::SendRequest),
            FetchPhase::RequestSend
        );
        assert_eq!(
            phase_of_timeout(ureq::Timeout::SendBody),
            FetchPhase::RequestSend
        );
        assert_eq!(
            phase_of_timeout(ureq::Timeout::RecvResponse),
            FetchPhase::ResponseHeaders
        );
        assert_eq!(
            phase_of_timeout(ureq::Timeout::RecvBody),
            FetchPhase::BodyTransfer
        );
    }

    #[test]
    fn the_connect_budget_is_a_fraction_of_the_request_budget() {
        let config = FetchConfig::default();
        assert!(
            config.connect_timeout * 4 <= config.timeout,
            "one unresponsive address must cost well under the whole per-request budget: \
             connect {}ms of a {}ms request",
            config.connect_timeout.as_millis(),
            config.timeout.as_millis()
        );
        assert!(
            config.connect_timeout >= Duration::from_secs(1),
            "the connect budget still has to cover a TCP connect plus a TLS handshake"
        );
    }

    #[test]
    fn failures_carry_their_phase_and_elapsed_time() {
        let error =
            FetchError::Timeout.in_phase(FetchPhase::TcpConnect, Duration::from_millis(2500));
        assert_eq!(error.phase(), Some(FetchPhase::TcpConnect));
        assert_eq!(error.elapsed(), Some(Duration::from_millis(2500)));
        assert_eq!(
            error.to_string(),
            "tcp connect: request timed out after 2500ms"
        );
        assert!(std::error::Error::source(&error).is_some());
        assert_eq!(error.into_inner(), FetchError::Timeout);

        // Cancellation is a caller decision, not a phase, and must stay
        // matchable as-is.
        let cancelled = FetchError::Cancelled.in_phase(FetchPhase::BodyTransfer, Duration::ZERO);
        assert_eq!(cancelled, FetchError::Cancelled);
        assert_eq!(cancelled.phase(), None);

        // A failure raised before any I/O has no phase to blame.
        assert_eq!(FetchError::Dns.phase(), None);
        assert_eq!(FetchError::Dns.into_inner(), FetchError::Dns);
    }

    #[test]
    fn loopback_targets_are_recognized_for_the_implicit_proxy_bypass() {
        for url in [
            "http://127.0.0.1:8080/",
            "http://127.9.9.9/",
            "http://[::1]:8080/",
            "http://localhost:8080/",
            "http://app.localhost/",
            "http://LOCALHOST./",
        ] {
            assert!(
                super::is_loopback_target(&Url::parse(url).expect("loopback URL")),
                "{url} points at this machine"
            );
        }
        for url in [
            "https://example.com/",
            "https://notlocalhost.example.com/",
            "https://localhost.example.com/",
            "https://192.0.2.1/",
        ] {
            assert!(
                !super::is_loopback_target(&Url::parse(url).expect("remote URL")),
                "{url} is a remote host"
            );
        }
    }

    /// A proxy that only records that a hop was routed to it.
    fn spawn_counting_proxy(hits: Arc<AtomicUsize>) -> std::net::SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind proxy");
        let address = listener.local_addr().expect("read proxy address");
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                hits.fetch_add(1, Ordering::SeqCst);
                drop(stream);
            }
        });
        address
    }

    /// An origin that answers one keep-alive-less response.
    fn spawn_origin_once() -> std::net::SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind origin");
        let address = listener.local_addr().expect("read origin address");
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = Vec::new();
                let mut chunk = [0_u8; 512];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(count) => request.extend_from_slice(&chunk[..count]),
                    }
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
            }
        });
        address
    }

    #[test]
    fn a_loopback_hop_bypasses_a_system_proxy_but_an_explicit_one_is_honored() {
        let hits = Arc::new(AtomicUsize::new(0));
        let proxy_address = spawn_counting_proxy(Arc::clone(&hits));
        let origin = spawn_origin_once();
        let url = Url::parse(&format!("http://{origin}/resource")).expect("origin URL");
        let proxy = ureq::Proxy::new(&format!("http://{proxy_address}")).expect("proxy");
        let explicit = HttpTransport::with_proxy(FetchConfig::default(), Some(proxy));

        // Explicit policy: the hop goes to the proxy, which drops it.
        let _outcome = explicit.fetch(&FetchRequest::get(url.clone()), &CancelToken::default());
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "a proxy the caller configured applies to every target"
        );

        // The same agent with a proxy that came from the environment or the
        // platform: browsers do not route loopback through those, and neither
        // does this transport.
        let system = explicit.clone();
        system.agent.write().expect("agent lock").implicit_proxy = true;
        system
            .fetch(&FetchRequest::get(url), &CancelToken::default())
            .expect("a loopback hop must not depend on a proxy that cannot serve it");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "an implicitly resolved proxy must not capture loopback traffic"
        );
    }

    #[test]
    fn an_explicit_proxy_can_be_swapped_without_restarting_the_transport() {
        let hits = Arc::new(AtomicUsize::new(0));
        let proxy_address = spawn_counting_proxy(Arc::clone(&hits));
        let origin = spawn_origin_once();
        let url = Url::parse(&format!("http://{origin}/resource")).expect("origin URL");
        let transport = HttpTransport::with_http_proxy(FetchConfig::default(), None);

        transport
            .fetch(&FetchRequest::get(url.clone()), &CancelToken::default())
            .expect("a direct first request reaches the origin");
        assert_eq!(hits.load(Ordering::SeqCst), 0);

        let config =
            super::HttpProxyConfig::parse(&format!("http://{proxy_address}")).expect("valid proxy");
        transport.set_http_proxy(Some(config));
        let _outcome = transport.fetch(&FetchRequest::get(url.clone()), &CancelToken::default());
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "requests after the swap use the new proxy"
        );

        transport.set_http_proxy(None);
        assert!(
            !transport.agent_snapshot().1,
            "the replacement is explicit policy, not an implicit environment proxy"
        );
    }

    #[test]
    fn proxy_addresses_are_validated_and_canonicalized() {
        for (input, expected) in [
            ("127.0.0.1:8080", "http://127.0.0.1:8080"),
            ("http://127.0.0.1:8080", "http://127.0.0.1:8080"),
            ("http://proxy.example:3128/", "http://proxy.example:3128"),
        ] {
            let config = super::HttpProxyConfig::parse(input).expect("valid proxy");
            assert_eq!(config.address(), expected);
        }
        for (input, kind) in [
            ("", super::ProxyConfigErrorKind::Empty),
            ("   ", super::ProxyConfigErrorKind::Empty),
            (
                "socks5://127.0.0.1:1080",
                super::ProxyConfigErrorKind::UnsupportedScheme,
            ),
            (
                "https://127.0.0.1:8080",
                super::ProxyConfigErrorKind::UnsupportedScheme,
            ),
            ("http://", super::ProxyConfigErrorKind::InvalidUrl),
            (
                "http://127.0.0.1:8080/path",
                super::ProxyConfigErrorKind::UnexpectedPath,
            ),
        ] {
            let error = super::HttpProxyConfig::parse(input).expect_err("invalid proxy");
            assert_eq!(error.kind(), kind, "{input:?}");
        }
    }

    #[test]
    fn default_user_agent_is_browser_compatible_and_product_identifiable() {
        let user_agent = FetchConfig::default().user_agent;
        assert!(user_agent.starts_with("Mozilla/5.0 "));
        assert!(user_agent.contains("AppleWebKit/537.36"));
        assert!(user_agent.contains("Chrome/"));
        assert!(user_agent.contains("rENDER/"));
    }

    #[test]
    fn http_methods_carry_wire_names_and_body_rules() {
        use super::HttpMethod;

        assert_eq!(HttpMethod::default(), HttpMethod::Get);
        assert_eq!(HttpMethod::Get.as_str(), "GET");
        assert_eq!(HttpMethod::Post.as_str(), "POST");
        assert_eq!(HttpMethod::Put.as_str(), "PUT");
        assert_eq!(HttpMethod::Delete.as_str(), "DELETE");
        assert_eq!(HttpMethod::Head.as_str(), "HEAD");
        assert!(!HttpMethod::Get.allows_body());
        assert!(!HttpMethod::Head.allows_body());
        assert!(HttpMethod::Post.allows_body());
        assert!(HttpMethod::Put.allows_body());
        assert!(HttpMethod::Delete.allows_body());
        assert_eq!(HttpMethod::Options.as_str(), "OPTIONS");
        assert_eq!(HttpMethod::Patch.as_str(), "PATCH");
        assert_eq!(HttpMethod::from_wire("OPTIONS"), Some(HttpMethod::Options));
        assert_eq!(HttpMethod::from_wire("PATCH"), Some(HttpMethod::Patch));
        assert_eq!(HttpMethod::from_wire("TRACE"), None);
    }

    #[test]
    fn rejects_reserved_headers_and_bodyless_method_bodies_before_transport() {
        use super::HttpMethod;

        // Validation runs before any I/O, so the discard port keeps this test
        // off the network entirely.
        let url = Url::parse("http://127.0.0.1:9/rejected").unwrap();
        let transport = HttpTransport::new(FetchConfig::default());
        let cancel = CancelToken::default();

        let error = transport
            .fetch(
                &FetchRequest::get(url.clone()).with_header("Host", "example.com"),
                &cancel,
            )
            .unwrap_err();
        assert_eq!(error, FetchError::ReservedHeader("Host".into()));

        let error = transport
            .fetch(
                &FetchRequest::post(url.clone()).with_header("Content-Length", "12"),
                &cancel,
            )
            .unwrap_err();
        assert_eq!(error, FetchError::ReservedHeader("Content-Length".into()));

        let error = transport
            .fetch(
                &FetchRequest::get(url.clone()).with_header("TRANSFER-ENCODING", "chunked"),
                &cancel,
            )
            .unwrap_err();
        assert_eq!(
            error,
            FetchError::ReservedHeader("TRANSFER-ENCODING".into())
        );

        let error = transport
            .fetch(
                &FetchRequest::get(url.clone()).with_header("Cookie", "sid=1"),
                &cancel,
            )
            .unwrap_err();
        assert_eq!(error, FetchError::ReservedHeader("Cookie".into()));

        let error = transport
            .fetch(&FetchRequest::get(url.clone()).with_body("x"), &cancel)
            .unwrap_err();
        assert_eq!(
            error,
            FetchError::InvalidRequest("GET requests cannot carry a body".into())
        );

        let error = transport
            .fetch(
                &FetchRequest::new(HttpMethod::Head, url.clone()).with_body("x"),
                &cancel,
            )
            .unwrap_err();
        assert_eq!(
            error,
            FetchError::InvalidRequest("HEAD requests cannot carry a body".into())
        );
    }

    #[test]
    fn parses_content_type_and_charset_case_insensitively() {
        assert_eq!(
            parse_content_type("Text/HTML; boundary=x; CHARSET=\"GBK\""),
            Some(ContentType {
                media_type: "text/html".into(),
                charset: Some("gbk".into()),
            })
        );
        assert_eq!(parse_content_type("not-a-media-type"), None);
    }

    #[test]
    fn classifies_rustls_errors_wrapped_by_io_as_tls() {
        let error = io::Error::new(
            io::ErrorKind::InvalidData,
            rustls::Error::General("certificate rejected".into()),
        );

        assert_eq!(
            map_io_error(&error),
            FetchError::Tls("unexpected error: certificate rejected".into())
        );
    }

    #[test]
    fn preserves_non_tls_io_errors() {
        let error = io::Error::new(io::ErrorKind::ConnectionReset, "peer reset connection");

        assert_eq!(
            map_io_error(&error),
            FetchError::Io("peer reset connection".into())
        );
    }

    #[test]
    fn preserves_protocol_relative_redirect_paths() {
        let request = Url::parse("https://www.zhihu.com/").unwrap();
        let parsed = Url::parse("https://www.zhihu.com//www.zhihu.com/signin?next=%2F").unwrap();
        let normalized = normalize_redirect_url(parsed, &request);
        assert_eq!(normalized.as_str(), "https://www.zhihu.com/signin?next=%2F");
    }

    #[test]
    fn fetches_percent_encoded_and_base64_data_urls_with_bounded_bodies() {
        let transport = HttpTransport::new(FetchConfig {
            max_body_bytes: 5,
            ..FetchConfig::default()
        });
        let encoded = Url::parse("data:text/plain;charset=utf-8,hello%20world").unwrap();
        let response = transport
            .fetch(&FetchRequest::get(encoded.clone()), &CancelToken::default())
            .unwrap_err();
        assert_eq!(response, FetchError::BodyLimitExceeded { limit: 5 });

        let base64 = Url::parse("data:image/png;base64,AAECAw==").unwrap();
        let response = transport
            .fetch(&FetchRequest::get(base64.clone()), &CancelToken::default())
            .expect("data URL response");
        assert_eq!(response.requested_url, base64);
        assert_eq!(response.final_url, base64);
        assert_eq!(response.status.as_u16(), 200);
        assert_eq!(response.content_type.unwrap().media_type, "image/png");
        assert_eq!(response.body, [0, 1, 2, 3]);

        let escaped_base64 = Url::parse("data:image/png;base64,AAECAw%3D%3D").unwrap();
        let response = transport
            .fetch(&FetchRequest::get(escaped_base64), &CancelToken::default())
            .expect("percent-encoded base64 data URL response");
        assert_eq!(response.body, [0, 1, 2, 3]);

        let whitespace = Url::parse("data:image/png;base64,AAEC Aw==").unwrap();
        let response = transport
            .fetch(&FetchRequest::get(whitespace), &CancelToken::default())
            .expect("base64 with ASCII whitespace decodes");
        assert_eq!(response.body, [0, 1, 2, 3]);
    }

    #[test]
    fn rejects_malformed_data_urls_without_using_the_network() {
        let transport = HttpTransport::new(FetchConfig::default());
        let malformed = Url::parse("data:image/png;base64,%%% ").unwrap();
        let error = transport
            .fetch(&FetchRequest::get(malformed), &CancelToken::default())
            .expect_err("invalid base64 must fail");
        assert!(matches!(error, FetchError::InvalidUrl(_)));
    }

    #[test]
    fn sends_conditional_cache_validators() {
        let seen_request = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let captured = std::sync::Arc::clone(&seen_request);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 512];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut chunk).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..count]);
            }
            *captured.lock().unwrap() = String::from_utf8_lossy(&request).into_owned();
            stream
                .write_all(b"HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        let url = Url::parse(&format!("http://{address}/resource")).unwrap();
        let request = FetchRequest::get(url)
            .with_etag("\"v1\"")
            .with_last_modified("Wed, 21 Oct 2015 07:28:00 GMT");
        let response = HttpTransport::new(FetchConfig::default())
            .fetch(&request, &CancelToken::default())
            .unwrap();
        server.join().unwrap();
        assert_eq!(response.status.as_u16(), 304);
        let request = seen_request.lock().unwrap().to_ascii_lowercase();
        assert!(request.contains("if-none-match: \"v1\""));
        assert!(request.contains("if-modified-since: wed, 21 oct 2015 07:28:00 gmt"));
        // Conditional revalidation must repeat the same content negotiation as
        // the original request (for example `Vary: Accept-Encoding`), so the
        // automatically advertised encodings stay identical on every request.
        assert!(request.contains("accept-encoding: gzip"));
    }

    #[test]
    fn combines_caller_and_hop_cookies_with_server_values_winning() {
        use super::combined_cookie_header;

        // Without chain cookies the caller header must stay byte-identical.
        assert_eq!(
            combined_cookie_header(Some("a=1; b=2"), None).as_deref(),
            Some("a=1; b=2")
        );
        assert_eq!(combined_cookie_header(None, None), None);
        // Chain cookies alone.
        assert_eq!(
            combined_cookie_header(None, Some("hop=1")).as_deref(),
            Some("hop=1")
        );
        // Merged: caller pairs retained, server-set values win name collisions
        // (cookie names are case-sensitive per RFC 6265).
        assert_eq!(
            combined_cookie_header(Some("a=1; b=2"), Some("b=server; c=3")).as_deref(),
            Some("a=1; b=server; c=3")
        );
        // A caller header fully overridden by the chain.
        assert_eq!(
            combined_cookie_header(Some("a=1"), Some("A=server")).as_deref(),
            Some("a=1; A=server")
        );
    }

    // ------------------------------------------------------------------
    // TLS / HTTP/2 transport tests.
    //
    // The certificate chain below is a throwaway test CA and a `localhost`
    // leaf (SAN DNS:localhost, IP:127.0.0.1) valid until 2036. It exists only
    // so the loopback TLS server can be verified without network access or
    // disabling certificate verification.
    // ------------------------------------------------------------------

    const TEST_CA_CERT_DER_B64: &str = "MIIC+jCCAeKgAwIBAgIIHe/fBjKyHoYwDQYJKoZIhvcNAQELBQAwGzEZMBcGA1UEAxMQckVOREVSIFRlc3QgUm9vdDAeFw0yNDAxMDEwMDAwMDBaFw0zNjAxMDEwMDAwMDBaMBsxGTAXBgNVBAMTEHJFTkRFUiBUZXN0IFJvb3QwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQC+xMif1j+eWBccBTQYGw/ta/f4wIFYgvCA5Vs/hGk6SW16zfK4KQ5m7VYk7FcfcjmWxLPhFFQHFYe+VM6jvQxlbbYE4VaOQf4Hq8fCIyqG7zWwxNXLkFm3luqPgMdzU5blq+jfWoaRgtjoHXRLcG2R9+oLhyzZzNYu1LgkJwexywrTfp4/NgQ2nLkG8MZUXYxck9EMgrVP4XnBtEEfUXOLAGSQ9znsGDJLiqFFRtvPXXbEtFwCutMz2MDl+JKH6RBbiHdv4X+nk/W8ZLU2TRYK8DMGOw+Mv9xki0R/kxwCuKm/SKzt/7MlfD9/0tEUnpWBt40knP0HkX0cNMrwJkshAgMBAAGjQjBAMA8GA1UdEwEB/wQFMAMBAf8wDgYDVR0PAQH/BAQDAgEGMB0GA1UdDgQWBBRvdrtzYmIhjOqh9tLh1uQ7l9QE4jANBgkqhkiG9w0BAQsFAAOCAQEASWRnql8fqJCEVsADnGpBHLIy+Hwa8eVmQ+PSXsBXsjhQbDr7t0w9R9C6+oaTUOtwlwmNBcvGB+HSf1fglfJNl3MvibuQcfXre1Aw3heNQC6xADmRpodnU3zCTGDCoXGp5uHUbE1QA3DeyMMwU33VizsfCGI7l1KEbntTi3F6iGWkrxwa4FFa3bvh6ANNTXgFoBk/lXg5rV0l6xDDc+2Zjt12t+9wdc29Z5WXqxgob+g9qLA8riVz1Trxv4l4JApgaudMuHMbOeP/V6j3hvYpfLmq7Q5vsJWckzYn7t7wEf7ujHqmOTsXbBjp5WTRo1rv1ZC5AJd8DkZoAQGcbqlZKw==";
    const TEST_SERVER_CERT_DER_B64: &str = "MIIDCjCCAfKgAwIBAgIQcv+ifFH9xEjWzA8poe9hVDANBgkqhkiG9w0BAQsFADAbMRkwFwYDVQQDExByRU5ERVIgVGVzdCBSb290MB4XDTI0MDEwMTAwMDAwMFoXDTM2MDEwMTAwMDAwMFowFDESMBAGA1UEAxMJbG9jYWxob3N0MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAqecjQPLynw0I/jGsv1KQZFsDF7Vw8rm5R4irvh2p40NdDI9v2BQ4PfNRnSmUJ7Uv2zne/yLo2VAMnOU71TlMvjnUqLP2TvP4nMwOEZni89EfTLUhYaV6/QA6zpFnNnZeimQjQzfBF2lokdznFyHaOVzys/XvPLCaxCoTOqa9nF7Rq5/C2gL9CWBQyodzq1zfmnoIbprFaJgmydHqLuAgsr1YAVc2j9+r5CugGJKQusdAy1WiLJCcVzK0eJb41L85yC1pkXVC2jzC5JoLVYAn1CF1EDnUHioMIKytAEGzQLILiiHcj3vzdeTJjb1m/g9g1g6aS9EZzGa2OfEruZBfJQIDAQABo1EwTzAaBgNVHREEEzARgglsb2NhbGhvc3SHBH8AAAEwDAYDVR0TAQH/BAIwADAOBgNVHQ8BAf8EBAMCBaAwEwYDVR0lBAwwCgYIKwYBBQUHAwEwDQYJKoZIhvcNAQELBQADggEBAAIezD01Dj1YSS6cm/NaJpxr1lzhIGtxEb9JgKhYZKFOxjHYKfowbwRYUvY56FDqlKNPb9ZZ8YHpmYm4un2auyA45uVwhw3tn8I0LLyXxsdFVKKp6vzqCN4pQGt8ImraucqP8jRGGZU/szLMAiRdRTL8yJ5elKbBITDr9zyrdDeXuC7Jxz8vxp1jf2Pew7vNwhT5fob3cQ2NP5/h9XQa+0XZ0srkUsKPsoes1zxYnoCZUKECjJNcO3isNSXAxaS/04JaM1K/UonW9M/zthgATPeWb/Wm4iQscYF35tH1cOWYTJ6feOj/GeGnAdg47RQZ+bokFtVl5aO65UzOA8YbDHs=";
    const TEST_SERVER_KEY_PKCS1_B64: &str = "MIIEpAIBAAKCAQEAqecjQPLynw0I/jGsv1KQZFsDF7Vw8rm5R4irvh2p40NdDI9v2BQ4PfNRnSmUJ7Uv2zne/yLo2VAMnOU71TlMvjnUqLP2TvP4nMwOEZni89EfTLUhYaV6/QA6zpFnNnZeimQjQzfBF2lokdznFyHaOVzys/XvPLCaxCoTOqa9nF7Rq5/C2gL9CWBQyodzq1zfmnoIbprFaJgmydHqLuAgsr1YAVc2j9+r5CugGJKQusdAy1WiLJCcVzK0eJb41L85yC1pkXVC2jzC5JoLVYAn1CF1EDnUHioMIKytAEGzQLILiiHcj3vzdeTJjb1m/g9g1g6aS9EZzGa2OfEruZBfJQIDAQABAoIBAEsh6EgPZ1UIWaK26tJMO4f1az+vv7vGf3VvrarLyVWzc1FbmLTv9BMJJ10dhuOsTvZd+AvRLzDyIjG7eSMITLSONElKE7DEWHtWDR6hc2goYPP14UYh/GonENpKGF9QJuyB7coKvLuSpGhkTFjBYpQdU0JbjijDXB1FEe3M0Gp2FtS9XJJQjK4wb8HfQz71z33+y1KjxUAe54/LlvMgkcJ2oo3aiUMuKwYrwVgzypCqvmN1DaFq/OQ3hyekmFTnOPdVtWtw2mZgpZtPGN3Dj/k0l59BIjAX5OOGLWaw86b1WUeWo7VcEj11a6ICYtzI8wqB50h6/dFROCESOLgWiZ0CgYEAwrO7QoeV3GF3nXlWK54XbUtc4Aw4X7URl9hpngXpurLXmZOBWbCuVNSfDrl6TvA+3UHBht9o2EutReEi77yeZrmdvwUK+OYB2UgjLqgpo5cIWh+N0aIEuZGq2h2mWG8RO+Z3Bww1t0rupsNc3B7G6Mfn5ovmpR8XTyTLg1rcRgcCgYEA32SuxiBLuobYOgg/D3sfY2rtezF/6RlCGj95tdhRayPs30swqzjSOHM6EYSAvX1aVMQoVrXjHnGmI4GlV/jM2MIg/rqulwXuihHfDNQ8gSzv1BJCMNls7Qz2LFeH9mSdkTAcHpLZe2j7zBrXFCHqNaaZGJ+XlBOwSsBcfQERRnMCgYARu671i0XR8aqkVBXOGTMHCFMoMNS9ms9EZjiQZPd5wqoyWaXxVk86eLcX7zq9j9IZ8tA+iYCSGqpob+3vqd0/e+5rJHlwW/qVIKXbrQNF3A6trqwLCNenUyYdgY/SBCOssfvrT2hhDlkYJTkC72ascvCjJeDW7zL/SVmj2BWx5wKBgQCwCIu8rlhebMHYwQTIMzJhyjHHAROVYIT5YAmNpR9LeSzALGT2l7j4KLyabzpvl2bmIwjSg1Osesv7cL8VGoZ5TEhp0juf4Fm/TSPh3SCsffeiupyUF9FA3xThjjwiidisPyf5Y6ijGfBJPuE7CRqhGl3LzH7KfdVOolR/YKb3RwKBgQCXfzX5tFtoSKoBs9tYpTYs0qVyeyYX4y58WHSODG5k4+Y+9bdjsB+WPM34Pw9iP2/57Mfst3UTmv9G/lT/A2Iu26BgQSfrSBDtRp6nRTkGR2KbpLgAsNJmnefx3Kf47lgYDDwggANkENxfpnKJ5hUc4PbHZAQ1b3Z3V7R8SkqU0Q==";

    fn test_der(encoded: &str) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("embedded test fixture is valid base64")
    }

    fn test_ca_cert() -> CertificateDer<'static> {
        CertificateDer::from(test_der(TEST_CA_CERT_DER_B64))
    }

    fn test_server_config(alpn: &[&str]) -> Arc<ServerConfig> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let certs = vec![CertificateDer::from(test_der(TEST_SERVER_CERT_DER_B64))];
        let key = PrivateKeyDer::Pkcs1(PrivatePkcs1KeyDer::from(test_der(
            TEST_SERVER_KEY_PKCS1_B64,
        )));
        let mut config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("ring supports the default TLS versions")
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .expect("test certificate and key match");
        config.alpn_protocols = alpn
            .iter()
            .map(|protocol| protocol.as_bytes().to_vec())
            .collect();
        Arc::new(config)
    }

    fn test_transport() -> HttpTransport {
        let mut roots = RootCertStore::empty();
        roots
            .add(test_ca_cert())
            .expect("test CA is a valid trust anchor");
        let certificate_der = test_der(TEST_CA_CERT_DER_B64);
        let certificate = ureq::tls::Certificate::from_der(&certificate_der).to_owned();
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::Rustls)
            .root_certs(ureq::tls::RootCerts::new_with_certs(&[certificate]))
            .build();
        HttpTransport::with_test_roots(FetchConfig::default(), roots, tls)
    }

    type TestHandler = Box<dyn FnOnce(TcpStream, Arc<ServerConfig>) + Send>;

    fn spawn_tls_server(
        config: Arc<ServerConfig>,
        handlers: Vec<TestHandler>,
    ) -> (SocketAddr, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback TLS server");
        let address = listener.local_addr().expect("read TLS server address");
        let handle = thread::spawn(move || {
            for handler in handlers {
                let (stream, _) = listener.accept().expect("accept TLS client");
                handler(stream, Arc::clone(&config));
            }
        });
        (address, handle)
    }

    fn tls_server_stream(
        stream: TcpStream,
        config: &ServerConfig,
    ) -> StreamOwned<ServerConnection, TcpStream> {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
        let _ = stream.set_nodelay(true);
        StreamOwned::new(
            ServerConnection::new(Arc::new(config.clone())).expect("TLS server connection"),
            stream,
        )
    }

    struct TestH2Server {
        stream: StreamOwned<ServerConnection, TcpStream>,
        decoder: HpackDecoder,
        encoder: HpackEncoder,
    }

    impl TestH2Server {
        fn new(stream: TcpStream, config: &ServerConfig) -> Self {
            Self {
                stream: tls_server_stream(stream, config),
                decoder: HpackDecoder::new(4096),
                encoder: HpackEncoder::new(4096),
            }
        }

        fn handshake(&mut self) -> Option<String> {
            let mut preface = [0_u8; 24];
            self.stream
                .read_exact(&mut preface)
                .expect("read HTTP/2 client preface");
            assert_eq!(&preface, crate::h2::CONNECTION_PREFACE);
            let frame = self.read_frame();
            match frame.kind() {
                FrameKind::Settings {
                    ack: false,
                    settings,
                } => assert!(
                    settings
                        .iter()
                        .any(|setting| setting.id == SettingId::EnablePush && setting.value == 0),
                    "client disables server push"
                ),
                other => panic!("expected client SETTINGS, got {other:?}"),
            }
            self.write_frame(&Frame::settings(vec![
                Setting::new(SettingId::MaxConcurrentStreams, 32),
                Setting::new(SettingId::InitialWindowSize, 65_535),
            ]));
            let ack = self.read_frame();
            assert!(
                matches!(ack.kind(), FrameKind::Settings { ack: true, .. }),
                "client acknowledges server SETTINGS"
            );
            self.stream
                .conn
                .alpn_protocol()
                .map(|protocol| String::from_utf8_lossy(protocol).into_owned())
        }

        fn read_frame(&mut self) -> Frame {
            Frame::read_from(&mut self.stream, 16_384).expect("read a valid HTTP/2 frame")
        }

        fn write_frame(&mut self, frame: &Frame) {
            frame
                .write_to(&mut self.stream)
                .expect("write an HTTP/2 frame");
            self.stream.flush().expect("flush HTTP/2 frame");
        }

        fn read_request(&mut self) -> (StreamId, Vec<HeaderField>) {
            let mut fragments = Vec::new();
            let mut stream_id = None;
            let mut end_headers = false;
            while !end_headers {
                let frame = self.read_frame();
                match frame.kind() {
                    FrameKind::Headers {
                        end_headers: request_end_headers,
                        fragment,
                        ..
                    } => {
                        stream_id = Some(frame.header().stream_id());
                        end_headers = *request_end_headers;
                        fragments.extend_from_slice(fragment);
                    }
                    FrameKind::Continuation {
                        end_headers: continuation_end,
                        fragment,
                    } => {
                        end_headers = *continuation_end;
                        fragments.extend_from_slice(fragment);
                    }
                    other => panic!("expected request HEADERS, got {other:?}"),
                }
            }
            let fields = self
                .decoder
                .decode(&fragments)
                .expect("decode request headers");
            (stream_id.expect("request stream id"), fields)
        }

        fn send_response(&mut self, stream_id: StreamId, status: u16, body: &[u8]) {
            let fields = vec![
                HeaderField::new(b":status".as_slice(), status.to_string().into_bytes()),
                HeaderField::new("content-type", "text/plain; charset=utf-8"),
            ];
            let block = self.encoder.encode(&fields);
            self.write_frame(&Frame::headers(
                stream_id,
                body.is_empty(),
                true,
                None,
                block,
            ));
            if !body.is_empty() {
                self.write_frame(&Frame::data(stream_id, true, body.to_vec()));
            }
        }

        fn send_large_response(&mut self, stream_id: StreamId, body: &[u8]) {
            let fields = vec![
                HeaderField::new(b":status".as_slice(), b"200"),
                HeaderField::new("content-type", "application/octet-stream"),
            ];
            let block = self.encoder.encode(&fields);
            self.write_frame(&Frame::headers(stream_id, false, true, None, block));

            let mut offset = 0;
            let mut window = 65_535_usize;
            while offset < body.len() {
                let chunk = (body.len() - offset).min(16_000).min(window);
                let end = offset + chunk == body.len();
                self.write_frame(&Frame::data(
                    stream_id,
                    end,
                    body[offset..offset + chunk].to_vec(),
                ));
                offset += chunk;
                let expected_updates = if end { 1 } else { 2 };
                let mut received = 0_usize;
                while received < expected_updates {
                    let frame = self.read_frame();
                    if let FrameKind::WindowUpdate { increment } = frame.kind() {
                        assert_eq!(
                            usize::try_from(*increment).expect("window increment fits usize"),
                            chunk,
                            "each DATA frame is replenished exactly once"
                        );
                        received += 1;
                    }
                }
                if !end {
                    window += chunk;
                }
            }
        }

        fn drain_window_updates(&mut self, expected: usize) {
            for _ in 0..expected {
                let frame = self.read_frame();
                assert!(
                    matches!(frame.kind(), FrameKind::WindowUpdate { .. }),
                    "expected WINDOW_UPDATE, got {:?}",
                    frame.kind()
                );
            }
        }
    }

    fn field<'a>(fields: &'a [HeaderField], name: &str) -> Option<&'a [u8]> {
        fields
            .iter()
            .find(|field| field.name == name.as_bytes())
            .map(|field| field.value.as_slice())
    }

    #[test]
    fn tls_alpn_h2_multiplexes_two_requests_over_one_connection() {
        let config = test_server_config(&["h2"]);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&seen);
        let (address, server) = spawn_tls_server(
            config,
            vec![Box::new(move |stream, config| {
                let mut server = TestH2Server::new(stream, &config);
                assert_eq!(
                    server.handshake().as_deref(),
                    Some("h2"),
                    "the server must negotiate HTTP/2 through ALPN"
                );
                let (first, first_fields) = server.read_request();
                let (second, second_fields) = server.read_request();
                *captured.lock().expect("capture request paths") = vec![
                    field(&first_fields, ":path")
                        .map(|path| String::from_utf8_lossy(path).into_owned())
                        .unwrap_or_default(),
                    field(&second_fields, ":path")
                        .map(|path| String::from_utf8_lossy(path).into_owned())
                        .unwrap_or_default(),
                ];
                assert_eq!(
                    field(&first_fields, ":scheme"),
                    Some(&b"https"[..]),
                    "h2 requests carry the real scheme"
                );
                // Complete the later stream first: both requests must already
                // be open on the single connection for this to be possible.
                server.send_response(second, 201, b"second");
                server.send_response(first, 202, b"first");
                server.drain_window_updates(2);
            })],
        );

        let transport = test_transport();
        let first_url =
            Url::parse(&format!("https://127.0.0.1:{}/one", address.port())).expect("first h2 URL");
        let second_url = Url::parse(&format!("https://127.0.0.1:{}/two", address.port()))
            .expect("second h2 URL");
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for url in [first_url, second_url] {
            let transport = transport.clone();
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                transport
                    .fetch(&FetchRequest::get(url), &CancelToken::default())
                    .expect("h2 fetch")
            }));
        }
        barrier.wait();
        let mut responses = workers
            .into_iter()
            .map(|worker| worker.join().expect("h2 worker"))
            .collect::<Vec<_>>();
        responses.sort_by_key(|response| response.status.as_u16());
        assert_eq!(responses[0].status.as_u16(), 201);
        assert_eq!(responses[0].body, b"second");
        assert_eq!(responses[1].status.as_u16(), 202);
        assert_eq!(responses[1].body, b"first");
        server.join().expect("TLS h2 server");

        let mut paths = seen.lock().expect("read captured paths").clone();
        paths.sort();
        assert_eq!(paths, ["/one", "/two"]);
    }

    #[test]
    fn tls_without_h2_alpn_falls_back_to_http1() {
        let config = test_server_config(&["http/1.1"]);
        let (address, server) = spawn_tls_server(
            config,
            vec![
                // First connection is the h2 probe: it negotiates http/1.1 and
                // the transport closes it without sending an HTTP request.
                Box::new(|stream, config| {
                    let mut tls = tls_server_stream(stream, &config);
                    let mut byte = [0_u8; 1];
                    let _ = tls.read(&mut byte);
                    assert_eq!(
                        tls.conn.alpn_protocol(),
                        Some(b"http/1.1".as_slice()),
                        "probe negotiated HTTP/1.1"
                    );
                }),
                // Second connection is the real HTTP/1.1 request.
                Box::new(|stream, config| {
                    let mut tls = tls_server_stream(stream, &config);
                    let mut request = Vec::new();
                    let mut chunk = [0_u8; 512];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        let count = tls.read(&mut chunk).expect("read HTTP/1.1 request");
                        if count == 0 {
                            break;
                        }
                        request.extend_from_slice(&chunk[..count]);
                    }
                    let request = String::from_utf8_lossy(&request);
                    assert!(
                        request.starts_with("GET /fallback HTTP/1.1\r\n"),
                        "HTTP/1.1 fallback request line: {request:?}"
                    );
                    tls.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nh1",
                    )
                    .expect("write HTTP/1.1 response");
                    tls.flush().expect("flush HTTP/1.1 response");
                }),
            ],
        );

        let url = Url::parse(&format!("https://127.0.0.1:{}/fallback", address.port()))
            .expect("fallback URL");
        let response = test_transport()
            .fetch(&FetchRequest::get(url), &CancelToken::default())
            .expect("HTTP/1.1 fallback fetch");
        assert_eq!(response.status.as_u16(), 200);
        assert_eq!(response.body, b"h1");
        server.join().expect("TLS fallback server");
    }

    #[test]
    fn h2_response_larger_than_the_initial_window_completes() {
        let body = (0..120_000)
            .map(|index| u8::try_from(index % 251).expect("byte value"))
            .collect::<Vec<_>>();
        let expected = body.clone();
        let config = test_server_config(&["h2"]);
        let (address, server) = spawn_tls_server(
            config,
            vec![Box::new(move |stream, config| {
                let mut server = TestH2Server::new(stream, &config);
                assert_eq!(server.handshake().as_deref(), Some("h2"));
                let (stream_id, _) = server.read_request();
                server.send_large_response(stream_id, &expected);
            })],
        );

        let url = Url::parse(&format!("https://127.0.0.1:{}/large", address.port()))
            .expect("large body URL");
        let response = test_transport()
            .fetch(&FetchRequest::get(url), &CancelToken::default())
            .expect("large h2 body fetch");
        assert_eq!(response.status.as_u16(), 200);
        assert_eq!(response.body, body);
        server.join().expect("TLS h2 flow-control server");
    }

    #[test]
    fn h2_pool_reuses_one_connection_for_sequential_requests() {
        let config = test_server_config(&["h2"]);
        let (address, server) = spawn_tls_server(
            config,
            vec![Box::new(|stream, config| {
                let mut server = TestH2Server::new(stream, &config);
                assert_eq!(server.handshake().as_deref(), Some("h2"));
                for body in [b"first".as_slice(), b"second".as_slice()] {
                    let (stream_id, fields) = server.read_request();
                    assert_eq!(field(&fields, ":scheme"), Some(&b"https"[..]));
                    server.send_response(stream_id, 200, body);
                    server.drain_window_updates(1);
                }
            })],
        );

        let transport = test_transport();
        for (path, expected) in [
            ("/first", b"first".as_slice()),
            ("/second", b"second".as_slice()),
        ] {
            let url = Url::parse(&format!("https://127.0.0.1:{}{path}", address.port()))
                .expect("sequential h2 URL");
            let response = transport
                .fetch(&FetchRequest::get(url), &CancelToken::default())
                .expect("sequential h2 fetch");
            assert_eq!(response.status.as_u16(), 200);
            assert_eq!(response.body, expected);
        }
        server.join().expect("TLS h2 reuse server");
    }

    #[test]
    fn h2_eof_evicts_the_pool_entry_and_a_retry_succeeds() {
        let config = test_server_config(&["h2"]);
        let connections = Arc::new(AtomicUsize::new(0));
        let first_count = Arc::clone(&connections);
        let second_count = Arc::clone(&connections);
        let (address, server) = spawn_tls_server(
            config,
            vec![
                Box::new(move |stream, config| {
                    first_count.fetch_add(1, Ordering::SeqCst);
                    let mut server = TestH2Server::new(stream, &config);
                    assert_eq!(server.handshake().as_deref(), Some("h2"));
                    let _ = server.read_request();
                    // Drop the TLS stream without a response: the client sees
                    // EOF at a frame boundary and must reconnect once.
                }),
                Box::new(move |stream, config| {
                    second_count.fetch_add(1, Ordering::SeqCst);
                    let mut server = TestH2Server::new(stream, &config);
                    assert_eq!(server.handshake().as_deref(), Some("h2"));
                    let (stream_id, _) = server.read_request();
                    server.send_response(stream_id, 200, b"after-eof");
                    server.drain_window_updates(1);
                }),
            ],
        );

        let url = Url::parse(&format!("https://127.0.0.1:{}/eof", address.port()))
            .expect("EOF retry URL");
        let response = test_transport()
            .fetch(&FetchRequest::get(url), &CancelToken::default())
            .expect("retry after EOF");
        assert_eq!(response.status.as_u16(), 200);
        assert_eq!(response.body, b"after-eof");
        assert_eq!(
            connections.load(Ordering::SeqCst),
            2,
            "the EOF connection is evicted and exactly one reconnect is made"
        );
        server.join().expect("TLS h2 EOF server");
    }

    #[test]
    fn h2_goaway_evicts_the_pool_entry_and_a_retry_succeeds() {
        let config = test_server_config(&["h2"]);
        let connections = Arc::new(AtomicUsize::new(0));
        let first_count = Arc::clone(&connections);
        let second_count = Arc::clone(&connections);
        let (address, server) = spawn_tls_server(
            config,
            vec![
                Box::new(move |stream, config| {
                    first_count.fetch_add(1, Ordering::SeqCst);
                    let mut server = TestH2Server::new(stream, &config);
                    assert_eq!(server.handshake().as_deref(), Some("h2"));
                    let (stream_id, _) = server.read_request();
                    server.write_frame(&Frame::goaway(
                        stream_id,
                        ErrorCode::NoError,
                        b"rotate".to_vec(),
                    ));
                }),
                Box::new(move |stream, config| {
                    second_count.fetch_add(1, Ordering::SeqCst);
                    let mut server = TestH2Server::new(stream, &config);
                    assert_eq!(server.handshake().as_deref(), Some("h2"));
                    let (stream_id, _) = server.read_request();
                    server.send_response(stream_id, 200, b"after-goaway");
                    server.drain_window_updates(1);
                }),
            ],
        );

        let url =
            Url::parse(&format!("https://127.0.0.1:{}/retry", address.port())).expect("retry URL");
        let response = test_transport()
            .fetch(&FetchRequest::get(url), &CancelToken::default())
            .expect("retry after GOAWAY");
        assert_eq!(response.status.as_u16(), 200);
        assert_eq!(response.body, b"after-goaway");
        assert_eq!(
            connections.load(Ordering::SeqCst),
            2,
            "the dead connection is evicted and exactly one reconnect is made"
        );
        server.join().expect("TLS h2 GOAWAY server");
    }
}
