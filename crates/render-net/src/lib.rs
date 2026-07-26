//! Bounded HTTP/HTTPS transport for rENDER.
//!
//! This crate is deliberately below browser Fetch semantics. It does not own
//! navigation/history, CORS, caching, cookies, content sniffing, or document
//! decoding. It only transfers bytes for already-normalized [`url::Url`]s.
//! HTTPS uses rustls and Web PKI verification; no API disables verification.

mod batch;
mod transport;
mod worker;

pub use batch::{BatchOptions, FixedOriginLimit, Origin, OriginConcurrencyPolicy};
pub use transport::{
    CancelToken, ContentType, FetchConfig, FetchError, FetchRequest, FetchResponse, FetchResult,
    Header, HttpStatus, HttpTransport,
};
pub use worker::{NetworkWorker, RequestHandle};

pub use url::Url;
