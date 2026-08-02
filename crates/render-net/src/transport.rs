use std::fmt;
use std::io::Read;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use ureq::ResponseExt;
use url::Url;

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

/// A normalized HTTP GET request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchRequest {
    pub url: Url,
    /// Optional request `Accept` value. Browser content negotiation policy
    /// belongs to the caller rather than this transport adapter.
    pub accept: Option<String>,
    /// Optional serialized Cookie request header supplied by the browser
    /// context. The transport remains stateless and never owns a cookie jar.
    pub cookie: Option<String>,
    /// Optional single HTTP byte range. Multipart ranges are intentionally not
    /// exposed until the media/cache layer can consume multipart responses.
    pub byte_range: Option<ByteRange>,
}

impl FetchRequest {
    #[must_use]
    pub const fn get(url: Url) -> Self {
        Self {
            url,
            accept: None,
            cookie: None,
            byte_range: None,
        }
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
}

/// Bounded transport configuration.
#[derive(Clone, Debug)]
pub struct FetchConfig {
    pub redirect_limit: u32,
    pub max_body_bytes: usize,
    pub max_header_bytes: usize,
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
    RedirectLimitExceeded { limit: u32 },
    HeaderLimitExceeded { limit: usize },
    BodyLimitExceeded { limit: usize },
    InvalidByteRange { start: u64, end: u64 },
    EmptyByteRangeSuffix,
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
            .timeout_global(Some(config.timeout))
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

    /// Performs one bounded HTTP/HTTPS GET.
    ///
    /// # Errors
    ///
    /// Returns a typed [`FetchError`] for cancellation, invalid schemes,
    /// configured limit violations, TLS verification, and transport failures.
    pub fn fetch(&self, request: &FetchRequest, cancel: &CancelToken) -> FetchResult {
        validate_scheme(&request.url)?;
        if cancel.is_cancelled() {
            return Err(FetchError::Cancelled);
        }

        let mut current_url = request.url.clone();
        let mut redirect_chain = vec![request.url.clone()];
        let mut redirects = Vec::new();
        let mut redirect_count = 0;

        loop {
            if cancel.is_cancelled() {
                return Err(FetchError::Cancelled);
            }

            let mut builder = self.agent.get(current_url.as_str());
            if let Some(accept) = request.accept.as_deref() {
                builder = builder.header("Accept", accept);
            }
            // A caller-provided Cookie header was computed for the original
            // URL. Reusing it only on same-origin hops avoids leaking it to a
            // cross-origin redirect while retaining normal login redirects.
            if same_origin(&current_url, &request.url)
                && let Some(cookie) = request.cookie.as_deref()
            {
                builder = builder.header("Cookie", cookie);
            }
            if let Some(byte_range) = request.byte_range {
                builder = builder.header("Range", &byte_range.header_value());
            }
            let mut response = builder.call().map_err(|error| self.map_error(error))?;

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
                let next_url = current_url
                    .join(location.trim())
                    .map_err(|error| FetchError::InvalidUrl(error.to_string()))?;
                validate_scheme(&next_url)?;
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
            let body = read_bounded_body(
                response.body_mut().as_reader(),
                self.config.max_body_bytes,
                cancel,
            )?;

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

    fn map_error(&self, error: ureq::Error) -> FetchError {
        match error {
            ureq::Error::TooManyRedirects => FetchError::RedirectLimitExceeded {
                limit: self.config.redirect_limit,
            },
            ureq::Error::LargeResponseHeader(_, _) => FetchError::HeaderLimitExceeded {
                limit: self.config.max_header_bytes,
            },
            ureq::Error::Timeout(_) => FetchError::Timeout,
            ureq::Error::HostNotFound => FetchError::Dns,
            ureq::Error::Tls(message) => FetchError::Tls(message.to_owned()),
            ureq::Error::Rustls(error) => FetchError::Tls(error.to_string()),
            ureq::Error::TlsRequired => FetchError::Tls("TLS was required but unavailable".into()),
            ureq::Error::BadUri(message) => FetchError::InvalidUrl(message),
            ureq::Error::Protocol(error) => FetchError::Protocol(error.to_string()),
            ureq::Error::Io(error) => map_io_error(&error),
            other => FetchError::Transport(other.to_string()),
        }
    }
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

fn read_bounded_body(
    mut reader: impl Read,
    limit: usize,
    cancel: &CancelToken,
) -> Result<Vec<u8>, FetchError> {
    let mut body = Vec::with_capacity(limit.min(64 * 1024));
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(FetchError::Cancelled);
        }
        let read = reader
            .read(&mut chunk)
            .map_err(|error| FetchError::Io(error.to_string()))?;
        if read == 0 {
            return Ok(body);
        }
        let remaining = limit.saturating_sub(body.len());
        if read > remaining {
            return Err(FetchError::BodyLimitExceeded { limit });
        }
        body.extend_from_slice(&chunk[..read]);
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

    use super::{
        ContentType, FetchConfig, FetchError, map_io_error, normalize_redirect_url,
        parse_content_type,
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
}
