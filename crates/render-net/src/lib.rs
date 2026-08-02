//! Bounded HTTP/HTTPS transport for rENDER.
//!
//! This crate is deliberately below browser Fetch semantics. It does not own
//! navigation/history, CORS, caching, content sniffing, or document decoding.
//! The transport is stateless; the exported [`CookieJar`] is an explicit
//! browser-context helper that callers use to decorate requests and absorb
//! response cookies. It only transfers bytes for normalized [`url::Url`]s.
//! HTTPS uses rustls and Web PKI verification; no API disables verification.

mod batch;
mod cookie;
mod transport;
mod worker;

pub use batch::{BatchOptions, FixedOriginLimit, Origin, OriginConcurrencyPolicy};
pub use cookie::{Cookie, CookieIssue, CookieJar, CookieLimits, CookieRejection, SameSite};
pub use transport::{
    ByteRange, CancelToken, ContentType, FetchConfig, FetchError, FetchRequest, FetchResponse,
    FetchResult, Header, HttpStatus, HttpTransport, RedirectResponse,
};
pub use worker::{NetworkWorker, RequestHandle};

pub use url::Url;
