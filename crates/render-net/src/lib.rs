//! Bounded HTTP/HTTPS transport for rENDER.
//!
//! This crate is deliberately below browser Fetch semantics. It does not own
//! navigation/history, CORS, caching, content sniffing, or document decoding.
//! The transport is stateless; the exported [`CookieJar`] is an explicit
//! browser-context helper that callers use to decorate requests and absorb
//! response cookies. It only transfers bytes for normalized [`url::Url`]s.
//! HTTPS uses rustls and Web PKI verification; no API disables verification.
//!
//! Every request reaches a visible terminal outcome: a [`FetchResponse`], or a
//! [`FetchError`] that names the [`FetchPhase`] it failed in and how long it ran,
//! reported through [`FetchConfig::observer`]. A batch that outlives
//! [`BatchOptions::timeout`] reports the requests that had not completed
//! instead of leaving its handle open forever.

mod batch;
mod cookie;
mod diagnostics;
mod transport;
mod worker;

pub use batch::{
    BatchOptions, DEFAULT_PER_ORIGIN_CONCURRENCY, FixedOriginLimit, ORIGINS_BEFORE_TOTAL_CEILING,
    Origin, OriginConcurrencyPolicy,
};
pub use cookie::{Cookie, CookieIssue, CookieJar, CookieLimits, CookieRejection, SameSite};
pub use diagnostics::{FetchEvent, FetchObserver, FetchPhase, NullObserver, StderrObserver};
pub use transport::{
    ByteRange, CacheValidators, CancelToken, ContentType, FetchConfig, FetchError, FetchRequest,
    FetchResponse, FetchResult, Header, HttpMethod, HttpStatus, HttpTransport, RedirectResponse,
};
pub use worker::{NetworkWorker, NetworkWorkerConfig, RequestHandle, queue_full_message};

pub use url::Url;
