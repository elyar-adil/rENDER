//! HTTP/2 foundation (RFC 9113) and HPACK (RFC 7541).
//!
//! This module is a self-contained HTTP/2 client stack: a frame codec with
//! per-type validation, an HPACK encoder/decoder with the static and dynamic
//! tables plus Huffman coding, connection preface and SETTINGS exchange, stream
//! state, and send/receive flow-control accounting. [`H2Connection`] multiplexes
//! independent requests over one `Read + Write` transport and dispatches
//! response frames to the correct stream.
//!
//! # Transport integration
//!
//! The connection itself is transport-agnostic: [`H2Connection`] works over any
//! `Read + Write` stream and still supports cleartext prior knowledge for tests.
//! `HttpTransport` in `crates/render-net/src/transport.rs` owns TLS: it offers
//! ALPN `h2` and `http/1.1`, and only constructs an [`H2Connection`] when the
//! server selected `h2`. Other HTTPS origins keep the existing ureq HTTP/1.1
//! path, and plain `http` origins never receive h2 prior knowledge implicitly.
//!
//! The transport pools one connection per origin behind a mutex. A request
//! calls [`H2Connection::send_request`] and then polls with
//! [`H2Connection::poll_frame`] (bounded by a socket read timeout) until
//! [`H2Connection::take_response`] completes. Holding the socket lock for only
//! one frame at a time lets independent streams take turns while frames for all
//! of them are dispatched by stream id. Redirects, cookies, request validation,
//! observers, and fetch phase budgets stay in `HttpTransport::fetch_transfer`.
//! A server that negotiates anything other than `h2` uses HTTP/1.1; a TLS
//! verification failure is reported as such. A protocol failure on an
//! already-negotiated h2 connection evicts and reconnects rather than silently
//! mixing protocols.
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
