//! HTTP/2 foundation (RFC 9113) and HPACK (RFC 7541).
//!
//! This module is a self-contained HTTP/2 client stack: a frame codec with
//! per-type validation, an HPACK encoder/decoder with the static and dynamic
//! tables plus Huffman coding, connection preface and SETTINGS exchange, stream
//! state, and send/receive flow-control accounting. [`H2Connection`] multiplexes
//! independent requests over one `Read + Write` transport and dispatches
//! response frames to the correct stream.
//!
//! # Cleartext scope and the transport wiring point
//!
//! This turn deliberately stops at a cleartext (prior-knowledge) connection so
//! the whole stack can be exercised against an in-process loopback server. TLS
//! with ALPN `h2` is **not** wired yet. The exact integration point for the next
//! turn is `HttpTransport::send_hop` in
//! `crates/render-net/src/transport.rs`: after a connection is established with
//! ALPN `h2` (or when the caller opts into cleartext prior knowledge), build an
//! [`H2Connection`] over that stream, call [`H2Connection::send_request`], then
//! [`H2Connection::read_response`], and translate [`H2Response`] into the
//! existing `FetchResponse`. The hop loop in `HttpTransport::fetch_transfer`
//! remains the owner of redirects, cookies, timeouts, and observers; no
//! HTTP/1.1 code is changed by this module.
//!
//! # Explicit non-features
//!
//! - `PUSH_PROMISE` is parsed but server push is rejected with
//!   [`H2Error::Unsupported`]`(`[`UnsupportedFeature::ServerPush`]`)`, a
//!   connection-level protocol error. It is never silently accepted.
//! - `PRIORITY` frames and the priority block of `HEADERS` are validated and
//!   counted in [`H2Stats::priority_frames_ignored`], but no scheduler consumes
//!   them. Ignoring priority is permitted by RFC 9113; the limitation is
//!   visible in the API rather than hidden.
//! - TLS/ALPN is outside this module, as described above.
//! - HTTP/2 `CONNECT` and extended CONNECT are rejected with
//!   [`H2Error::Unsupported`]`(`[`UnsupportedFeature::Connect`]`)`; the request
//!   model always emits `:scheme` and `:path`.
//! - Informational 1xx responses are decoded and discarded while waiting for
//!   the final response.
//! - Extensions that require new frame types are preserved as unknown frames
//!   and ignored, as RFC 9113 section 4.1 requires.

#![allow(
    clippy::module_name_repetitions,
    reason = "protocol names such as H2Connection and FrameHeader are clearer than renaming around the module path"
)]

mod connection;
mod error;
mod frame;
mod hpack;

pub use connection::{H2Config, H2Connection, H2Request, H2Response, H2Stats};
pub use error::{ErrorCode, FrameError, H2Error, HpackError, StreamId, UnsupportedFeature};
pub use frame::{
    CONNECTION_PREFACE, DEFAULT_MAX_FRAME_SIZE, FLAG_ACK, FLAG_END_HEADERS, FLAG_END_STREAM,
    FLAG_PADDED, FLAG_PRIORITY, Frame, FrameHeader, FrameKind, FrameType, MAX_MAX_FRAME_SIZE,
    Priority, Setting, SettingId,
};
pub use hpack::{HeaderField, HpackDecoder, HpackEncoder, Indexing, huffman};
