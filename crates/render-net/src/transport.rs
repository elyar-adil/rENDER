use std::fmt;
use std::io::Read;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use ureq::ResponseExt;
use url::Url;

use crate::CookieJar;

/// Minimum average body throughput once [`FetchConfig::timeout`] has elapsed.
///
/// The body transfer has no whole-transfer wall clock (a large resource that
/// keeps making progress must not spuriously fail), so this floor bounds how
/// long a connection that only trickles bytes can pin a worker thread.
const MIN_BODY_BYTES_PER_SECOND: usize = 1024;

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
    /// before the transfer fails with [`FetchError::Timeout`]. The response
    /// body has no wall clock: a body that keeps making progress may
    /// legitimately run longer than this budget overall (a CDN stylesheet
    /// trickling in over a slow link, for example). Such transfers remain
    /// bounded by `max_body_bytes`, by a per-read idle bound of this budget,
    /// and by a minimum average rate of [`MIN_BODY_BYTES_PER_SECOND`] once
    /// this budget has elapsed. The budget is also enforced across the whole
    /// redirect chain.
    pub timeout: Duration,
    pub user_agent: String,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            redirect_limit: 10,
            max_body_bytes: 16 * 1024 * 1024,
            max_header_bytes: 64 * 1024,
            timeout: Duration::from_secs(30),
            user_agent: format!(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
                 AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/120.0.0.0 Safari/537.36 rENDER/{}",
                env!("CARGO_PKG_VERSION")
            ),
        }
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
    Protocol(String),
    Io(String),
    WorkerStopped,
    Transport(String),
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
            Self::Protocol(message) => write!(formatter, "HTTP protocol error: {message}"),
            Self::Io(message) => write!(formatter, "network I/O error: {message}"),
            Self::WorkerStopped => formatter.write_str("network worker stopped"),
            Self::Transport(message) => write!(formatter, "transport error: {message}"),
        }
    }
}

impl std::error::Error for FetchError {}

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

/// Cloneable blocking HTTP transport. Call this on a network thread, or use
/// [`crate::NetworkWorker`] from GUI/event-loop code.
#[derive(Clone)]
pub struct HttpTransport {
    config: Arc<FetchConfig>,
    agent: ureq::Agent,
}

impl fmt::Debug for HttpTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpTransport")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl HttpTransport {
    /// Creates a transport with verified rustls HTTPS and bounded headers.
    #[must_use]
    pub fn new(config: FetchConfig) -> Self {
        let agent_config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // Redirects are handled here so intermediate response headers are
            // available to the browser cookie jar.
            .max_redirects(0)
            .max_redirects_will_error(true)
            .max_response_header_size(config.max_header_bytes)
            // Split header/body timeout design. There is deliberately no
            // end-to-end wall clock (`timeout_global`/`timeout_per_call`) and
            // no `timeout_recv_response`/`timeout_recv_body` either: in ureq
            // those two are absolute phase budgets whose deadline keeps
            // ticking into the body read, so any value here would kill large
            // transfers that keep making progress. Instead the request phases
            // (resolve, connect, send, and header reception, which stays
            // bounded by the send-request deadline) each get `config.timeout`,
            // while the body phase is bounded by `read_bounded_body`: a
            // per-read idle bound of `config.timeout`, the minimum-progress
            // floor, and `max_body_bytes`.
            .timeout_global(None)
            .timeout_resolve(Some(config.timeout))
            .timeout_connect(Some(config.timeout))
            .timeout_send_request(Some(config.timeout))
            .timeout_send_body(Some(config.timeout))
            .timeout_recv_response(None)
            .timeout_recv_body(None)
            .user_agent(config.user_agent.clone())
            .build();
        Self {
            config: Arc::new(config),
            agent: agent_config.into(),
        }
    }

    #[must_use]
    pub fn config(&self) -> &FetchConfig {
        &self.config
    }

    /// Performs one bounded HTTP(S) request or `data:` URL decode.
    ///
    /// # Errors
    ///
    /// Returns a typed [`FetchError`] for cancellation, invalid schemes,
    /// invalid request construction, configured limit violations, TLS
    /// verification, and transport failures.
    #[allow(
        clippy::too_many_lines,
        reason = "redirect/cookie/timeout handling reads as one pipeline"
    )]
    pub fn fetch(&self, request: &FetchRequest, cancel: &CancelToken) -> FetchResult {
        validate_request(request)?;
        if request.url.scheme() == "data" {
            return self.fetch_data_url(request, cancel);
        }
        validate_scheme(&request.url)?;
        if cancel.is_cancelled() {
            return Err(FetchError::Cancelled);
        }

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
            let response = self
                .send_hop(current_method, &current_url, hop_body, &hop_headers)
                .map_err(|error| classify_ureq_error(&error, &self.config))?;

            if cancel.is_cancelled() {
                return Err(FetchError::Cancelled);
            }

            let status = HttpStatus(response.status().as_u16());
            let headers = response
                .headers()
                .iter()
                .map(|(name, value)| Header {
                    name: name.as_str().to_owned(),
                    value: value.as_bytes().to_vec(),
                })
                .collect::<Vec<_>>();
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
                // the per-stage timeouts unboundedly.
                if started.elapsed() >= self.config.timeout {
                    return Err(FetchError::Timeout);
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

            let final_url = normalize_redirect_url(
                Url::parse(&response.get_uri().to_string())
                    .map_err(|error| FetchError::InvalidUrl(error.to_string()))?,
                &request.url,
            );
            if let Some(last) = redirect_chain.last_mut() {
                *last = final_url.clone();
            }
            let content_type = header_text(&headers, "content-type").and_then(parse_content_type);
            let body = self.read_bounded_body(response.into_body().into_reader(), cancel)?;
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

    /// Sends one hop of the request chain through ureq, applying the method
    /// and the prepared headers. Bodyless methods use `call` (no
    /// `Content-Length` on the wire); body-carrying methods send the bytes and
    /// let ureq derive `Content-Length` from the body.
    fn send_hop(
        &self,
        method: HttpMethod,
        url: &Url,
        body: Option<&[u8]>,
        headers: &[(String, String)],
    ) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let uri = url.as_str();
        match (method, body) {
            (HttpMethod::Get, _) => decorate(self.agent.get(uri), headers).call(),
            (HttpMethod::Head, _) => decorate(self.agent.head(uri), headers).call(),
            (HttpMethod::Delete, None) => decorate(self.agent.delete(uri), headers).call(),
            // DELETE with a body is legal HTTP but needs ureq's explicit
            // escape hatch.
            (HttpMethod::Delete, Some(bytes)) => {
                decorate(self.agent.delete(uri).force_send_body(), headers).send(bytes)
            }
            (HttpMethod::Post, Some(bytes)) => decorate(self.agent.post(uri), headers).send(bytes),
            (HttpMethod::Post, None) => decorate(self.agent.post(uri), headers).send_empty(),
            (HttpMethod::Put, Some(bytes)) => decorate(self.agent.put(uri), headers).send(bytes),
            (HttpMethod::Put, None) => decorate(self.agent.put(uri), headers).send_empty(),
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
    /// stylesheet over a slow link). Instead, once the configured budget has
    /// elapsed overall the transfer must average at least
    /// [`MIN_BODY_BYTES_PER_SECOND`], and each individual read may idle at most
    /// that budget before the transfer fails.
    ///
    /// ureq applies no socket deadline during the body phase (its remaining
    /// timeouts are all absolute phase budgets), so the blocking reader is
    /// pumped on a dedicated thread and this loop bounds every read via a
    /// channel receive timeout. A pump left behind on a timeout or
    /// cancellation stays blocked on its socket until the peer closes the
    /// connection, then unwinds on its own.
    fn read_bounded_body(
        &self,
        reader: impl Read + Send + 'static,
        cancel: &CancelToken,
    ) -> Result<Vec<u8>, FetchError> {
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
            let chunk = match chunk_rx.recv_timeout(self.config.timeout) {
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
            if elapsed > self.config.timeout {
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
        result
    }

    /// Classifies a failure surfaced by the body reader. ureq wraps its typed
    /// errors inside `std::io::Error` (decoders pass them through verbatim), so
    /// a stalled body read is still a [`FetchError::Timeout`], not a generic
    /// I/O failure.
    fn map_body_read_error(&self, error: &std::io::Error) -> FetchError {
        let classified = error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<ureq::Error>())
            .map(|error| classify_ureq_error(error, &self.config));
        let mapped = classified.unwrap_or_else(|| FetchError::Io(error.to_string()));
        if matches!(mapped, FetchError::Timeout) {
            eprintln!("[timeout-debug] ureq body read timed out: {error}");
        }
        mapped
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

/// Applies the prepared hop headers to a ureq request builder.
fn decorate<TBuilder>(
    builder: ureq::RequestBuilder<TBuilder>,
    headers: &[(String, String)],
) -> ureq::RequestBuilder<TBuilder> {
    headers.iter().fold(builder, |builder, (name, value)| {
        builder.header(name.as_str(), value.as_str())
    })
}

/// Maps a typed ureq failure onto the transport's error vocabulary.
fn classify_ureq_error(error: &ureq::Error, config: &FetchConfig) -> FetchError {
    match error {
        ureq::Error::TooManyRedirects => FetchError::RedirectLimitExceeded {
            limit: config.redirect_limit,
        },
        ureq::Error::LargeResponseHeader(_, _) => FetchError::HeaderLimitExceeded {
            limit: config.max_header_bytes,
        },
        ureq::Error::Timeout(_) => FetchError::Timeout,
        ureq::Error::HostNotFound => FetchError::Dns,
        ureq::Error::Tls(message) => FetchError::Tls((*message).to_owned()),
        ureq::Error::Rustls(error) => FetchError::Tls(error.to_string()),
        ureq::Error::TlsRequired => FetchError::Tls("TLS was required but unavailable".into()),
        ureq::Error::BadUri(message) => FetchError::InvalidUrl(message.clone()),
        ureq::Error::Protocol(error) => FetchError::Protocol(error.to_string()),
        ureq::Error::Io(error) => map_io_error(error),
        other => FetchError::Transport(other.to_string()),
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
        base64::engine::general_purpose::STANDARD
            .decode(encoded_body)
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

    use super::{
        CancelToken, ContentType, FetchConfig, FetchError, FetchRequest, HttpTransport,
        map_io_error, normalize_redirect_url, parse_content_type,
    };
    use url::Url;

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
        assert!(request.contains("accept-encoding: gzip, br"));
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
}
