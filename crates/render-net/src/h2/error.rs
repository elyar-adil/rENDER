use std::fmt;

/// A 31-bit HTTP/2 stream identifier.
///
/// The most significant bit is reserved on the wire and is masked by
/// [`StreamId::new`]. Stream identifier 0 is reserved for connection-level
/// frames and is represented by [`StreamId::CONNECTION`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StreamId(u32);

impl StreamId {
    /// The connection-level pseudo stream used by `SETTINGS`, `PING`,
    /// `GOAWAY`, and connection-level `WINDOW_UPDATE` frames.
    pub const CONNECTION: Self = Self(0);

    /// Creates an identifier, discarding the reserved high bit.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw & 0x7fff_ffff)
    }

    /// Returns the wire value (always in the 31-bit range).
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Whether this is the connection-level pseudo stream.
    #[must_use]
    pub const fn is_connection(self) -> bool {
        self.0 == 0
    }

    /// Whether this identifier belongs to a client-initiated stream.
    #[must_use]
    pub const fn is_client_initiated(self) -> bool {
        self.0 % 2 == 1
    }
}

impl fmt::Display for StreamId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "stream {}", self.0)
    }
}

/// HTTP/2 error codes defined by RFC 9113 section 7.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ErrorCode {
    NoError,
    ProtocolError,
    InternalError,
    FlowControlError,
    SettingsTimeout,
    StreamClosed,
    FrameSizeError,
    RefusedStream,
    Cancel,
    CompressionError,
    ConnectError,
    EnhanceYourCalm,
    InadequateSecurity,
    Http11Required,
    Unknown(u32),
}

impl ErrorCode {
    /// Decodes an error code from the wire.
    #[must_use]
    pub const fn from_u32(raw: u32) -> Self {
        match raw {
            0x0 => Self::NoError,
            0x1 => Self::ProtocolError,
            0x2 => Self::InternalError,
            0x3 => Self::FlowControlError,
            0x4 => Self::SettingsTimeout,
            0x5 => Self::StreamClosed,
            0x6 => Self::FrameSizeError,
            0x7 => Self::RefusedStream,
            0x8 => Self::Cancel,
            0x9 => Self::CompressionError,
            0xa => Self::ConnectError,
            0xb => Self::EnhanceYourCalm,
            0xc => Self::InadequateSecurity,
            0xd => Self::Http11Required,
            other => Self::Unknown(other),
        }
    }

    /// Returns the wire value.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::NoError => 0x0,
            Self::ProtocolError => 0x1,
            Self::InternalError => 0x2,
            Self::FlowControlError => 0x3,
            Self::SettingsTimeout => 0x4,
            Self::StreamClosed => 0x5,
            Self::FrameSizeError => 0x6,
            Self::RefusedStream => 0x7,
            Self::Cancel => 0x8,
            Self::CompressionError => 0x9,
            Self::ConnectError => 0xa,
            Self::EnhanceYourCalm => 0xb,
            Self::InadequateSecurity => 0xc,
            Self::Http11Required => 0xd,
            Self::Unknown(value) => value,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoError => formatter.write_str("NO_ERROR"),
            Self::ProtocolError => formatter.write_str("PROTOCOL_ERROR"),
            Self::InternalError => formatter.write_str("INTERNAL_ERROR"),
            Self::FlowControlError => formatter.write_str("FLOW_CONTROL_ERROR"),
            Self::SettingsTimeout => formatter.write_str("SETTINGS_TIMEOUT"),
            Self::StreamClosed => formatter.write_str("STREAM_CLOSED"),
            Self::FrameSizeError => formatter.write_str("FRAME_SIZE_ERROR"),
            Self::RefusedStream => formatter.write_str("REFUSED_STREAM"),
            Self::Cancel => formatter.write_str("CANCEL"),
            Self::CompressionError => formatter.write_str("COMPRESSION_ERROR"),
            Self::ConnectError => formatter.write_str("CONNECT_ERROR"),
            Self::EnhanceYourCalm => formatter.write_str("ENHANCE_YOUR_CALM"),
            Self::InadequateSecurity => formatter.write_str("INADEQUATE_SECURITY"),
            Self::Http11Required => formatter.write_str("HTTP_1_1_REQUIRED"),
            Self::Unknown(value) => write!(formatter, "unknown error code {value}"),
        }
    }
}

/// A frame codec failure.
///
/// A frame error carries the HTTP/2 error code that must be reported and
/// whether the peer has to receive a `GOAWAY` (connection error) or a
/// `RST_STREAM` (stream error).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FrameError {
    /// The peer closed the connection cleanly at a frame boundary.
    Closed,
    /// The underlying stream failed while reading or writing a frame.
    Io(String),
    /// The frame violated the protocol.
    Protocol {
        code: ErrorCode,
        /// `None` for a connection error, `Some` for a stream error.
        stream_id: Option<StreamId>,
        detail: String,
    },
}

impl FrameError {
    pub(crate) fn connection(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self::Protocol {
            code,
            stream_id: None,
            detail: detail.into(),
        }
    }

    pub(crate) fn stream(stream_id: StreamId, code: ErrorCode, detail: impl Into<String>) -> Self {
        Self::Protocol {
            code,
            stream_id: Some(stream_id),
            detail: detail.into(),
        }
    }

    /// The error code to report to the peer.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::Closed | Self::Io(_) => ErrorCode::NoError,
            Self::Protocol { code, .. } => *code,
        }
    }

    /// The affected stream for a stream error.
    #[must_use]
    pub const fn stream_id(&self) -> Option<StreamId> {
        match self {
            Self::Protocol { stream_id, .. } => *stream_id,
            Self::Closed | Self::Io(_) => None,
        }
    }

    /// Whether the peer must be sent a `GOAWAY` rather than a `RST_STREAM`.
    #[must_use]
    pub const fn is_connection_error(&self) -> bool {
        matches!(
            self,
            Self::Protocol {
                stream_id: None,
                ..
            }
        )
    }
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => formatter.write_str("peer closed the connection"),
            Self::Io(message) => write!(formatter, "frame I/O failed: {message}"),
            Self::Protocol {
                code,
                stream_id,
                detail,
            } => {
                if let Some(stream_id) = stream_id {
                    write!(formatter, "{stream_id}: {code}: {detail}")
                } else {
                    write!(formatter, "connection: {code}: {detail}")
                }
            }
        }
    }
}

/// HPACK decoding or encoding failures (RFC 7541).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HpackError {
    /// An indexed field referenced an entry outside both tables.
    InvalidIndex { index: usize },
    /// Index 0 is not a valid indexed header field.
    IndexZero,
    /// An integer representation exceeded the supported range.
    IntegerOverflow,
    /// An integer representation ended in the middle of a continuation.
    TruncatedInteger,
    /// A string literal's declared length exceeded the remaining block.
    TruncatedString { expected: usize, remaining: usize },
    /// A string literal exceeded the configured sanity limit.
    StringTooLong { length: usize, limit: usize },
    /// A dynamic table size update appeared after a header field.
    SizeUpdateNotAtStart,
    /// A dynamic table size update exceeded the decoder's advertised limit.
    SizeUpdateTooLarge { requested: usize, limit: usize },
    /// A Huffman code has no matching symbol.
    InvalidHuffmanCode,
    /// A Huffman string contained the EOS symbol.
    EosInHuffman,
    /// Trailing Huffman bits were not the EOS prefix.
    InvalidHuffmanPadding,
    /// The decoded header list exceeded the configured limit.
    HeaderListTooLarge { limit: usize },
}

impl fmt::Display for HpackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIndex { index } => {
                write!(formatter, "invalid HPACK table index {index}")
            }
            Self::IndexZero => formatter.write_str("HPACK index 0 is reserved"),
            Self::IntegerOverflow => formatter.write_str("HPACK integer overflow"),
            Self::TruncatedInteger => formatter.write_str("truncated HPACK integer"),
            Self::TruncatedString {
                expected,
                remaining,
            } => write!(
                formatter,
                "truncated HPACK string: declared {expected} bytes, {remaining} remain"
            ),
            Self::StringTooLong { length, limit } => write!(
                formatter,
                "HPACK string of {length} bytes exceeds limit {limit}"
            ),
            Self::SizeUpdateNotAtStart => {
                formatter.write_str("HPACK dynamic table size update after a header field")
            }
            Self::SizeUpdateTooLarge { requested, limit } => write!(
                formatter,
                "HPACK table size update to {requested} exceeds limit {limit}"
            ),
            Self::InvalidHuffmanCode => formatter.write_str("invalid HPACK Huffman code"),
            Self::EosInHuffman => formatter.write_str("HPACK Huffman string contains EOS"),
            Self::InvalidHuffmanPadding => formatter.write_str("invalid HPACK Huffman padding"),
            Self::HeaderListTooLarge { limit } => {
                write!(formatter, "decoded HPACK header list exceeds limit {limit}")
            }
        }
    }
}

/// A feature that this foundation intentionally does not implement yet.
///
/// These are returned as errors rather than silently accepted. Priority
/// scheduling is a separate case: RFC 9113 permits an endpoint to ignore
/// priority signals, so [`H2Connection`](crate::h2::H2Connection) validates and
/// counts `PRIORITY` frames but does not schedule by them. See the module
/// documentation for the remaining work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedFeature {
    /// Server push and `PUSH_PROMISE` are not implemented.
    ServerPush,
    /// Priority scheduling is validated but ignored; no scheduler exists.
    PriorityScheduling,
    /// TLS/ALPN negotiation is outside this cleartext module.
    TlsAlpn,
    /// HTTP/2 `CONNECT` (and extended CONNECT) framing is not modelled by
    /// [`H2Request`](crate::h2::H2Request), which always emits
    /// `:scheme`/`:path`.
    Connect,
}

impl fmt::Display for UnsupportedFeature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ServerPush => formatter.write_str("HTTP/2 server push"),
            Self::PriorityScheduling => formatter.write_str("HTTP/2 priority scheduling"),
            Self::TlsAlpn => formatter.write_str("TLS ALPN negotiation"),
            Self::Connect => formatter.write_str("HTTP/2 CONNECT"),
        }
    }
}

/// A typed HTTP/2 failure.
///
/// Connection-level failures are reported to the peer with `GOAWAY`;
/// stream-level failures with `RST_STREAM`. The [`H2Error::code`] accessor
/// supplies the code for either frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum H2Error {
    /// The frame layer rejected a frame.
    Frame(FrameError),
    /// HPACK decoding or encoding failed.
    Hpack(HpackError),
    /// A socket read or write failed.
    Io(String),
    /// The peer closed the connection at a frame boundary.
    ConnectionClosed,
    /// A connection-level protocol violation not detected by the frame parser.
    Protocol { code: ErrorCode, detail: String },
    /// A stream-level protocol violation.
    Stream {
        stream_id: StreamId,
        code: ErrorCode,
        detail: String,
    },
    /// The peer reset a stream.
    StreamReset {
        stream_id: StreamId,
        code: ErrorCode,
    },
    /// The peer sent `GOAWAY`.
    GoAway {
        last_stream_id: StreamId,
        code: ErrorCode,
        debug_data: Vec<u8>,
    },
    /// A feature that is deliberately not implemented was requested.
    Unsupported(UnsupportedFeature),
    /// The caller supplied invalid configuration.
    InvalidConfig(String),
    /// The caller supplied an invalid request (pseudo headers, illegal field
    /// names, forbidden connection-specific headers, or a body on `GET`).
    InvalidRequest(String),
    /// The peer's `SETTINGS_MAX_CONCURRENT_STREAMS` limit is already reached.
    ConcurrentStreamLimit { limit: u32 },
    /// The client-initiated stream identifier space is exhausted.
    StreamIdExhausted,
    /// A response body exceeded [`H2Config::max_body_size`](crate::h2::H2Config::max_body_size).
    ResponseBodyTooLarge { limit: usize },
    /// A header block exceeded the configured compressed-size limit.
    HeaderBlockTooLarge { limit: usize },
    /// The decoded header list exceeded the configured limit.
    HeaderListTooLarge { limit: usize },
}

impl H2Error {
    /// The error code that should accompany `GOAWAY` or `RST_STREAM`.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::Frame(error) => error.code(),
            Self::Hpack(_) => ErrorCode::CompressionError,
            Self::Protocol { code, .. }
            | Self::Stream { code, .. }
            | Self::StreamReset { code, .. }
            | Self::GoAway { code, .. } => *code,
            Self::Unsupported(UnsupportedFeature::ServerPush) => ErrorCode::ProtocolError,
            Self::ResponseBodyTooLarge { .. }
            | Self::HeaderBlockTooLarge { .. }
            | Self::HeaderListTooLarge { .. } => ErrorCode::EnhanceYourCalm,
            Self::Io(_)
            | Self::ConnectionClosed
            | Self::Unsupported(_)
            | Self::InvalidConfig(_)
            | Self::InvalidRequest(_)
            | Self::ConcurrentStreamLimit { .. }
            | Self::StreamIdExhausted => ErrorCode::NoError,
        }
    }

    /// The stream this error belongs to, when it is stream-scoped.
    #[must_use]
    pub const fn stream_id(&self) -> Option<StreamId> {
        match self {
            Self::Frame(error) => error.stream_id(),
            Self::Stream { stream_id, .. } | Self::StreamReset { stream_id, .. } => {
                Some(*stream_id)
            }
            _ => None,
        }
    }

    /// Whether this failure requires the connection to be torn down.
    #[must_use]
    pub const fn is_connection_error(&self) -> bool {
        match self {
            Self::Frame(error) => error.is_connection_error(),
            Self::Hpack(_)
            | Self::Protocol { .. }
            | Self::HeaderBlockTooLarge { .. }
            | Self::HeaderListTooLarge { .. }
            | Self::Unsupported(UnsupportedFeature::ServerPush) => true,
            _ => false,
        }
    }
}

impl From<FrameError> for H2Error {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

impl From<HpackError> for H2Error {
    fn from(error: HpackError) -> Self {
        Self::Hpack(error)
    }
}

impl From<std::io::Error> for H2Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

impl fmt::Display for H2Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => write!(formatter, "{error}"),
            Self::Hpack(error) => write!(formatter, "HPACK: {error}"),
            Self::Io(message) => write!(formatter, "HTTP/2 I/O failed: {message}"),
            Self::ConnectionClosed => formatter.write_str("HTTP/2 connection closed by peer"),
            Self::Protocol { code, detail } => write!(formatter, "{code}: {detail}"),
            Self::Stream {
                stream_id,
                code,
                detail,
            } => write!(formatter, "{stream_id}: {code}: {detail}"),
            Self::StreamReset { stream_id, code } => {
                write!(formatter, "{stream_id} reset by peer: {code}")
            }
            Self::GoAway {
                last_stream_id,
                code,
                debug_data,
            } => {
                write!(formatter, "GOAWAY (last stream {last_stream_id}, {code})")?;
                if !debug_data.is_empty() {
                    write!(formatter, ": {}", String::from_utf8_lossy(debug_data))?;
                }
                Ok(())
            }
            Self::Unsupported(feature) => {
                write!(formatter, "unsupported HTTP/2 feature: {feature}")
            }
            Self::InvalidConfig(message) => {
                write!(formatter, "invalid HTTP/2 configuration: {message}")
            }
            Self::InvalidRequest(message) => {
                write!(formatter, "invalid HTTP/2 request: {message}")
            }
            Self::ConcurrentStreamLimit { limit } => {
                write!(formatter, "HTTP/2 concurrent stream limit {limit} reached")
            }
            Self::StreamIdExhausted => {
                formatter.write_str("HTTP/2 client stream identifier space exhausted")
            }
            Self::ResponseBodyTooLarge { limit } => {
                write!(formatter, "HTTP/2 response body exceeds limit {limit}")
            }
            Self::HeaderBlockTooLarge { limit } => {
                write!(formatter, "HTTP/2 header block exceeds limit {limit}")
            }
            Self::HeaderListTooLarge { limit } => {
                write!(formatter, "HTTP/2 header list exceeds limit {limit}")
            }
        }
    }
}

impl std::error::Error for H2Error {}
