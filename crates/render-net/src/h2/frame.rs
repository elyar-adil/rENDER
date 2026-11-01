use std::fmt;
use std::io::{self, Read, Write};

use super::error::{ErrorCode, FrameError, StreamId};

/// Smallest frame payload size every endpoint must accept (RFC 9113 4.2).
pub const DEFAULT_MAX_FRAME_SIZE: u32 = 16_384;
/// Largest legal frame payload size.
pub const MAX_MAX_FRAME_SIZE: u32 = 16_777_215;
/// The client connection preface specified by RFC 9113 3.4.
pub const CONNECTION_PREFACE: &[u8; 24] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

pub const FLAG_ACK: u8 = 0x1;
pub const FLAG_END_STREAM: u8 = 0x1;
pub const FLAG_END_HEADERS: u8 = 0x4;
pub const FLAG_PADDED: u8 = 0x8;
pub const FLAG_PRIORITY: u8 = 0x20;

/// Frame types defined by RFC 9113 section 6.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FrameType {
    Data,
    Headers,
    Priority,
    RstStream,
    Settings,
    PushPromise,
    Ping,
    GoAway,
    WindowUpdate,
    Continuation,
    Unknown(u8),
}

impl FrameType {
    #[must_use]
    pub const fn from_u8(raw: u8) -> Self {
        match raw {
            0x0 => Self::Data,
            0x1 => Self::Headers,
            0x2 => Self::Priority,
            0x3 => Self::RstStream,
            0x4 => Self::Settings,
            0x5 => Self::PushPromise,
            0x6 => Self::Ping,
            0x7 => Self::GoAway,
            0x8 => Self::WindowUpdate,
            0x9 => Self::Continuation,
            other => Self::Unknown(other),
        }
    }

    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Data => 0x0,
            Self::Headers => 0x1,
            Self::Priority => 0x2,
            Self::RstStream => 0x3,
            Self::Settings => 0x4,
            Self::PushPromise => 0x5,
            Self::Ping => 0x6,
            Self::GoAway => 0x7,
            Self::WindowUpdate => 0x8,
            Self::Continuation => 0x9,
            Self::Unknown(value) => value,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Data => "DATA",
            Self::Headers => "HEADERS",
            Self::Priority => "PRIORITY",
            Self::RstStream => "RST_STREAM",
            Self::Settings => "SETTINGS",
            Self::PushPromise => "PUSH_PROMISE",
            Self::Ping => "PING",
            Self::GoAway => "GOAWAY",
            Self::WindowUpdate => "WINDOW_UPDATE",
            Self::Continuation => "CONTINUATION",
            Self::Unknown(_) => "UNKNOWN",
        }
    }
}

/// A decoded 9-octet frame header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameHeader {
    length: u32,
    frame_type: FrameType,
    flags: u8,
    stream_id: StreamId,
}

impl FrameHeader {
    /// The declared payload length in octets.
    #[must_use]
    pub const fn length(self) -> u32 {
        self.length
    }

    #[must_use]
    pub const fn frame_type(self) -> FrameType {
        self.frame_type
    }

    /// The raw flags octet.
    #[must_use]
    pub const fn flags(self) -> u8 {
        self.flags
    }

    #[must_use]
    pub const fn stream_id(self) -> StreamId {
        self.stream_id
    }

    #[must_use]
    pub const fn end_stream(self) -> bool {
        self.flags & FLAG_END_STREAM != 0
    }

    #[must_use]
    pub const fn end_headers(self) -> bool {
        self.flags & FLAG_END_HEADERS != 0
    }

    #[must_use]
    pub const fn ack(self) -> bool {
        self.flags & FLAG_ACK != 0
    }

    #[must_use]
    pub const fn padded(self) -> bool {
        self.flags & FLAG_PADDED != 0
    }

    #[must_use]
    pub const fn priority(self) -> bool {
        self.flags & FLAG_PRIORITY != 0
    }
}

/// Stream priority fields carried by `HEADERS` and `PRIORITY` frames.
///
/// `weight` is the raw wire value; the effective weight is `weight + 1`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Priority {
    pub exclusive: bool,
    pub dependency: StreamId,
    pub weight: u8,
}

impl Priority {
    #[must_use]
    pub const fn new(exclusive: bool, dependency: StreamId, weight: u8) -> Self {
        Self {
            exclusive,
            dependency,
            weight,
        }
    }

    fn encode(self, out: &mut Vec<u8>) {
        let dependency = self.dependency.get() | if self.exclusive { 0x8000_0000 } else { 0 };
        out.extend_from_slice(&dependency.to_be_bytes());
        out.push(self.weight);
    }
}

/// Identifiers for `SETTINGS` parameters (RFC 9113 6.5.2).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SettingId {
    HeaderTableSize,
    EnablePush,
    MaxConcurrentStreams,
    InitialWindowSize,
    MaxFrameSize,
    MaxHeaderListSize,
    Unknown(u16),
}

impl SettingId {
    #[must_use]
    pub const fn from_u16(raw: u16) -> Self {
        match raw {
            0x1 => Self::HeaderTableSize,
            0x2 => Self::EnablePush,
            0x3 => Self::MaxConcurrentStreams,
            0x4 => Self::InitialWindowSize,
            0x5 => Self::MaxFrameSize,
            0x6 => Self::MaxHeaderListSize,
            other => Self::Unknown(other),
        }
    }

    #[must_use]
    pub const fn as_u16(self) -> u16 {
        match self {
            Self::HeaderTableSize => 0x1,
            Self::EnablePush => 0x2,
            Self::MaxConcurrentStreams => 0x3,
            Self::InitialWindowSize => 0x4,
            Self::MaxFrameSize => 0x5,
            Self::MaxHeaderListSize => 0x6,
            Self::Unknown(value) => value,
        }
    }
}

/// One `SETTINGS` identifier/value pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Setting {
    pub id: SettingId,
    pub value: u32,
}

impl Setting {
    #[must_use]
    pub const fn new(id: SettingId, value: u32) -> Self {
        Self { id, value }
    }
}

/// A decoded frame. Padding has already been removed from `DATA`, `HEADERS`,
/// and `PUSH_PROMISE`; `FrameHeader::length` still reports the flow-controlled
/// on-wire length.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    header: FrameHeader,
    kind: FrameKind,
}

/// Typed payloads. Unknown frame types are preserved so the connection can
/// ignore them as RFC 9113 requires.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FrameKind {
    Data {
        data: Vec<u8>,
    },
    Headers {
        end_stream: bool,
        end_headers: bool,
        priority: Option<Priority>,
        fragment: Vec<u8>,
    },
    Priority {
        priority: Priority,
    },
    RstStream {
        error_code: ErrorCode,
    },
    Settings {
        ack: bool,
        settings: Vec<Setting>,
    },
    PushPromise {
        end_headers: bool,
        promised_stream_id: StreamId,
        fragment: Vec<u8>,
    },
    Ping {
        ack: bool,
        opaque: [u8; 8],
    },
    GoAway {
        last_stream_id: StreamId,
        error_code: ErrorCode,
        debug_data: Vec<u8>,
    },
    WindowUpdate {
        increment: u32,
    },
    Continuation {
        end_headers: bool,
        fragment: Vec<u8>,
    },
    Unknown {
        frame_type: u8,
        payload: Vec<u8>,
    },
}

impl Frame {
    #[must_use]
    pub const fn header(&self) -> FrameHeader {
        self.header
    }

    #[must_use]
    pub const fn kind(&self) -> &FrameKind {
        &self.kind
    }

    #[must_use]
    pub fn into_kind(self) -> FrameKind {
        self.kind
    }

    #[must_use]
    pub fn data(stream_id: StreamId, end_stream: bool, data: impl Into<Vec<u8>>) -> Self {
        let flags = if end_stream { FLAG_END_STREAM } else { 0 };
        let data = data.into();
        let header = FrameHeader {
            length: u32::try_from(data.len()).unwrap_or(u32::MAX),
            frame_type: FrameType::Data,
            flags,
            stream_id,
        };
        Self {
            header,
            kind: FrameKind::Data { data },
        }
    }

    #[must_use]
    pub fn headers(
        stream_id: StreamId,
        end_stream: bool,
        end_headers: bool,
        priority: Option<Priority>,
        fragment: impl Into<Vec<u8>>,
    ) -> Self {
        let mut flags = 0;
        if end_stream {
            flags |= FLAG_END_STREAM;
        }
        if end_headers {
            flags |= FLAG_END_HEADERS;
        }
        if priority.is_some() {
            flags |= FLAG_PRIORITY;
        }
        let fragment = fragment.into();
        let length = u32::try_from(fragment.len() + if priority.is_some() { 5 } else { 0 })
            .unwrap_or(u32::MAX);
        Self {
            header: FrameHeader {
                length,
                frame_type: FrameType::Headers,
                flags,
                stream_id,
            },
            kind: FrameKind::Headers {
                end_stream,
                end_headers,
                priority,
                fragment,
            },
        }
    }

    #[must_use]
    pub fn priority(stream_id: StreamId, priority: Priority) -> Self {
        Self {
            header: FrameHeader {
                length: 5,
                frame_type: FrameType::Priority,
                flags: 0,
                stream_id,
            },
            kind: FrameKind::Priority { priority },
        }
    }

    #[must_use]
    pub fn rst_stream(stream_id: StreamId, error_code: ErrorCode) -> Self {
        Self {
            header: FrameHeader {
                length: 4,
                frame_type: FrameType::RstStream,
                flags: 0,
                stream_id,
            },
            kind: FrameKind::RstStream { error_code },
        }
    }

    #[must_use]
    pub fn settings(settings: Vec<Setting>) -> Self {
        Self {
            header: FrameHeader {
                length: u32::try_from(settings.len() * 6).unwrap_or(u32::MAX),
                frame_type: FrameType::Settings,
                flags: 0,
                stream_id: StreamId::CONNECTION,
            },
            kind: FrameKind::Settings {
                ack: false,
                settings,
            },
        }
    }

    #[must_use]
    pub fn settings_ack() -> Self {
        Self {
            header: FrameHeader {
                length: 0,
                frame_type: FrameType::Settings,
                flags: FLAG_ACK,
                stream_id: StreamId::CONNECTION,
            },
            kind: FrameKind::Settings {
                ack: true,
                settings: Vec::new(),
            },
        }
    }

    #[must_use]
    pub fn push_promise(
        stream_id: StreamId,
        promised_stream_id: StreamId,
        end_headers: bool,
        fragment: impl Into<Vec<u8>>,
    ) -> Self {
        let fragment = fragment.into();
        Self {
            header: FrameHeader {
                length: u32::try_from(4 + fragment.len()).unwrap_or(u32::MAX),
                frame_type: FrameType::PushPromise,
                flags: if end_headers { FLAG_END_HEADERS } else { 0 },
                stream_id,
            },
            kind: FrameKind::PushPromise {
                end_headers,
                promised_stream_id,
                fragment,
            },
        }
    }

    #[must_use]
    pub fn ping(ack: bool, opaque: [u8; 8]) -> Self {
        Self {
            header: FrameHeader {
                length: 8,
                frame_type: FrameType::Ping,
                flags: if ack { FLAG_ACK } else { 0 },
                stream_id: StreamId::CONNECTION,
            },
            kind: FrameKind::Ping { ack, opaque },
        }
    }

    #[must_use]
    pub fn goaway(
        last_stream_id: StreamId,
        error_code: ErrorCode,
        debug_data: impl Into<Vec<u8>>,
    ) -> Self {
        let debug_data = debug_data.into();
        Self {
            header: FrameHeader {
                length: u32::try_from(8 + debug_data.len()).unwrap_or(u32::MAX),
                frame_type: FrameType::GoAway,
                flags: 0,
                stream_id: StreamId::CONNECTION,
            },
            kind: FrameKind::GoAway {
                last_stream_id,
                error_code,
                debug_data,
            },
        }
    }

    #[must_use]
    pub fn window_update(stream_id: StreamId, increment: u32) -> Self {
        Self {
            header: FrameHeader {
                length: 4,
                frame_type: FrameType::WindowUpdate,
                flags: 0,
                stream_id,
            },
            kind: FrameKind::WindowUpdate { increment },
        }
    }

    #[must_use]
    pub fn continuation(
        stream_id: StreamId,
        end_headers: bool,
        fragment: impl Into<Vec<u8>>,
    ) -> Self {
        let fragment = fragment.into();
        Self {
            header: FrameHeader {
                length: u32::try_from(fragment.len()).unwrap_or(u32::MAX),
                frame_type: FrameType::Continuation,
                flags: if end_headers { FLAG_END_HEADERS } else { 0 },
                stream_id,
            },
            kind: FrameKind::Continuation {
                end_headers,
                fragment,
            },
        }
    }

    /// Reads one frame from `reader`, enforcing `max_frame_size`.
    ///
    /// A clean EOF before any byte of the next header is
    /// [`FrameError::Closed`]; one inside a frame is an I/O error.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError`] for truncated frames, oversized frames, frame
    /// type-specific length violations, stream identifier violations, and
    /// malformed padding or priority blocks.
    pub fn read_from<R: Read>(reader: &mut R, max_frame_size: u32) -> Result<Self, FrameError> {
        let mut raw = [0_u8; 9];
        if !read_full(reader, &mut raw)? {
            return Err(FrameError::Closed);
        }
        let length = (u32::from(raw[0]) << 16) | (u32::from(raw[1]) << 8) | u32::from(raw[2]);
        if length > max_frame_size {
            return Err(FrameError::connection(
                ErrorCode::FrameSizeError,
                format!("frame length {length} exceeds configured maximum {max_frame_size}"),
            ));
        }
        let frame_type = FrameType::from_u8(raw[3]);
        let flags = raw[4];
        let stream_id = StreamId::new(u32::from_be_bytes([raw[5], raw[6], raw[7], raw[8]]));
        let mut payload = vec![0_u8; usize::try_from(length).unwrap_or(0)];
        if !read_full(reader, &mut payload)? {
            return Err(FrameError::Io(
                "connection ended inside a frame payload".to_owned(),
            ));
        }
        decode_frame(length, frame_type, flags, stream_id, payload)
    }

    /// Decodes exactly one complete frame from `bytes`.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError`] if `bytes` does not hold exactly one well-formed
    /// frame or the frame exceeds `max_frame_size`.
    pub fn decode(bytes: &[u8], max_frame_size: u32) -> Result<Self, FrameError> {
        if bytes.len() < 9 {
            return Err(FrameError::connection(
                ErrorCode::FrameSizeError,
                "truncated frame header",
            ));
        }
        let length = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        let expected = 9 + usize::try_from(length).unwrap_or(usize::MAX);
        if bytes.len() != expected {
            return Err(FrameError::connection(
                ErrorCode::FrameSizeError,
                format!(
                    "frame declares {length} payload bytes, buffer holds {}",
                    bytes.len().saturating_sub(9)
                ),
            ));
        }
        if length > max_frame_size {
            return Err(FrameError::connection(
                ErrorCode::FrameSizeError,
                format!("frame length {length} exceeds configured maximum {max_frame_size}"),
            ));
        }
        decode_frame(
            length,
            FrameType::from_u8(bytes[3]),
            bytes[4],
            StreamId::new(u32::from_be_bytes([bytes[5], bytes[6], bytes[7], bytes[8]])),
            bytes[9..].to_vec(),
        )
    }

    /// Serializes this frame.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError`] if the payload exceeds the protocol's 24-bit
    /// length field or the frame violates a stream-identifier or length rule.
    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        let (frame_type, flags, stream_id, payload) = self.encode_parts()?;
        let length = u32::try_from(payload.len()).map_err(|_| {
            FrameError::connection(ErrorCode::FrameSizeError, "frame payload too large")
        })?;
        if length > MAX_MAX_FRAME_SIZE {
            return Err(FrameError::connection(
                ErrorCode::FrameSizeError,
                "frame payload exceeds the 24-bit length field",
            ));
        }
        let mut out = Vec::with_capacity(9 + payload.len());
        out.extend_from_slice(&length.to_be_bytes()[1..]);
        out.push(frame_type.as_u8());
        out.push(flags);
        out.extend_from_slice(&stream_id.get().to_be_bytes());
        out.extend_from_slice(&payload);
        Ok(out)
    }

    /// Writes one serialized frame to `writer`.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError`] if serialization or the write fails.
    pub fn write_to<W: Write>(&self, writer: &mut W) -> Result<(), FrameError> {
        let bytes = self.encode()?;
        writer
            .write_all(&bytes)
            .map_err(|error| FrameError::Io(error.to_string()))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one protocol-dispatch match is clearer than splitting each wire type into a helper"
    )]
    fn encode_parts(&self) -> Result<(FrameType, u8, StreamId, Vec<u8>), FrameError> {
        let stream_id = self.header.stream_id;
        match &self.kind {
            FrameKind::Data { data } => {
                validate_stream(stream_id, FrameType::Data)?;
                let flags = if self.header.end_stream() {
                    FLAG_END_STREAM
                } else {
                    0
                };
                Ok((FrameType::Data, flags, stream_id, data.clone()))
            }
            FrameKind::Headers {
                end_stream,
                end_headers,
                priority,
                fragment,
            } => {
                validate_stream(stream_id, FrameType::Headers)?;
                let mut flags = 0;
                if *end_stream {
                    flags |= FLAG_END_STREAM;
                }
                if *end_headers {
                    flags |= FLAG_END_HEADERS;
                }
                let mut payload = Vec::with_capacity(fragment.len() + 5);
                if let Some(priority) = priority {
                    if priority.dependency == stream_id {
                        return Err(FrameError::stream(
                            stream_id,
                            ErrorCode::ProtocolError,
                            "HEADERS priority depends on its own stream",
                        ));
                    }
                    flags |= FLAG_PRIORITY;
                    priority.encode(&mut payload);
                }
                payload.extend_from_slice(fragment);
                Ok((FrameType::Headers, flags, stream_id, payload))
            }
            FrameKind::Priority { priority } => {
                validate_stream(stream_id, FrameType::Priority)?;
                if priority.dependency == stream_id {
                    return Err(FrameError::stream(
                        stream_id,
                        ErrorCode::ProtocolError,
                        "PRIORITY frame depends on itself",
                    ));
                }
                let mut payload = Vec::with_capacity(5);
                priority.encode(&mut payload);
                Ok((FrameType::Priority, 0, stream_id, payload))
            }
            FrameKind::RstStream { error_code } => {
                validate_stream(stream_id, FrameType::RstStream)?;
                Ok((
                    FrameType::RstStream,
                    0,
                    stream_id,
                    error_code.as_u32().to_be_bytes().to_vec(),
                ))
            }
            FrameKind::Settings { ack, settings } => {
                if !stream_id.is_connection() {
                    return Err(FrameError::connection(
                        ErrorCode::ProtocolError,
                        "SETTINGS frame on a non-zero stream",
                    ));
                }
                let mut payload = Vec::with_capacity(settings.len() * 6);
                for setting in settings {
                    validate_setting(setting.id, setting.value)?;
                    payload.extend_from_slice(&setting.id.as_u16().to_be_bytes());
                    payload.extend_from_slice(&setting.value.to_be_bytes());
                }
                let flags = if *ack { FLAG_ACK } else { 0 };
                if *ack && !payload.is_empty() {
                    return Err(FrameError::connection(
                        ErrorCode::FrameSizeError,
                        "SETTINGS ACK with a non-empty payload",
                    ));
                }
                Ok((FrameType::Settings, flags, StreamId::CONNECTION, payload))
            }
            FrameKind::PushPromise {
                end_headers,
                promised_stream_id,
                fragment,
            } => {
                validate_stream(stream_id, FrameType::PushPromise)?;
                if promised_stream_id.is_connection() {
                    return Err(FrameError::connection(
                        ErrorCode::ProtocolError,
                        "PUSH_PROMISE cannot promise stream 0",
                    ));
                }
                let flags = if *end_headers { FLAG_END_HEADERS } else { 0 };
                let mut payload = Vec::with_capacity(4 + fragment.len());
                payload.extend_from_slice(&promised_stream_id.get().to_be_bytes());
                payload.extend_from_slice(fragment);
                Ok((FrameType::PushPromise, flags, stream_id, payload))
            }
            FrameKind::Ping { ack, opaque } => {
                if !stream_id.is_connection() {
                    return Err(FrameError::connection(
                        ErrorCode::ProtocolError,
                        "PING frame on a non-zero stream",
                    ));
                }
                Ok((
                    FrameType::Ping,
                    if *ack { FLAG_ACK } else { 0 },
                    StreamId::CONNECTION,
                    opaque.to_vec(),
                ))
            }
            FrameKind::GoAway {
                last_stream_id,
                error_code,
                debug_data,
            } => {
                if !stream_id.is_connection() {
                    return Err(FrameError::connection(
                        ErrorCode::ProtocolError,
                        "GOAWAY frame on a non-zero stream",
                    ));
                }
                let mut payload = Vec::with_capacity(8 + debug_data.len());
                payload.extend_from_slice(&last_stream_id.get().to_be_bytes());
                payload.extend_from_slice(&error_code.as_u32().to_be_bytes());
                payload.extend_from_slice(debug_data);
                Ok((FrameType::GoAway, 0, StreamId::CONNECTION, payload))
            }
            FrameKind::WindowUpdate { increment } => {
                if *increment == 0 || *increment > 0x7fff_ffff {
                    return Err(FrameError::connection(
                        ErrorCode::ProtocolError,
                        "WINDOW_UPDATE increment must be in 1..=2^31-1",
                    ));
                }
                let payload = increment.to_be_bytes().to_vec();
                Ok((FrameType::WindowUpdate, 0, stream_id, payload))
            }
            FrameKind::Continuation {
                end_headers,
                fragment,
            } => {
                validate_stream(stream_id, FrameType::Continuation)?;
                Ok((
                    FrameType::Continuation,
                    if *end_headers { FLAG_END_HEADERS } else { 0 },
                    stream_id,
                    fragment.clone(),
                ))
            }
            FrameKind::Unknown {
                frame_type,
                payload,
            } => Ok((
                FrameType::Unknown(*frame_type),
                0,
                stream_id,
                payload.clone(),
            )),
        }
    }
}

fn validate_stream(stream_id: StreamId, frame_type: FrameType) -> Result<(), FrameError> {
    if stream_id.is_connection() {
        Err(FrameError::connection(
            ErrorCode::ProtocolError,
            format!("{} frame requires a non-zero stream", frame_type.name()),
        ))
    } else {
        Ok(())
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one protocol-dispatch match is clearer than splitting each wire type into a helper"
)]
fn decode_frame(
    length: u32,
    frame_type: FrameType,
    flags: u8,
    stream_id: StreamId,
    payload: Vec<u8>,
) -> Result<Frame, FrameError> {
    debug_assert_eq!(length, u32::try_from(payload.len()).unwrap_or(u32::MAX));
    let (header, kind) = match frame_type {
        FrameType::Data => {
            validate_stream(stream_id, frame_type)?;
            let data = strip_padding(flags, &payload)?;
            (
                FrameHeader {
                    length,
                    frame_type,
                    flags,
                    stream_id,
                },
                FrameKind::Data { data },
            )
        }
        FrameType::Headers => {
            validate_stream(stream_id, frame_type)?;
            let body = strip_padding(flags, &payload)?;
            let (priority, fragment) = parse_priority(flags, stream_id, body)?;
            (
                FrameHeader {
                    length,
                    frame_type,
                    flags,
                    stream_id,
                },
                FrameKind::Headers {
                    end_stream: flags & FLAG_END_STREAM != 0,
                    end_headers: flags & FLAG_END_HEADERS != 0,
                    priority,
                    fragment,
                },
            )
        }
        FrameType::Priority => {
            validate_stream(stream_id, frame_type)?;
            require_length(length, 5, frame_type)?;
            let priority = decode_priority(stream_id, &payload)?;
            (
                FrameHeader {
                    length,
                    frame_type,
                    flags,
                    stream_id,
                },
                FrameKind::Priority { priority },
            )
        }
        FrameType::RstStream => {
            validate_stream(stream_id, frame_type)?;
            require_length(length, 4, frame_type)?;
            (
                FrameHeader {
                    length,
                    frame_type,
                    flags,
                    stream_id,
                },
                FrameKind::RstStream {
                    error_code: ErrorCode::from_u32(read_u32(&payload)),
                },
            )
        }
        FrameType::Settings => {
            if !stream_id.is_connection() {
                return Err(FrameError::connection(
                    ErrorCode::ProtocolError,
                    "SETTINGS frame on a non-zero stream",
                ));
            }
            if flags & FLAG_ACK != 0 {
                if length != 0 {
                    return Err(FrameError::connection(
                        ErrorCode::FrameSizeError,
                        "SETTINGS ACK must have an empty payload",
                    ));
                }
                (
                    FrameHeader {
                        length,
                        frame_type,
                        flags,
                        stream_id,
                    },
                    FrameKind::Settings {
                        ack: true,
                        settings: Vec::new(),
                    },
                )
            } else {
                if !length.is_multiple_of(6) {
                    return Err(FrameError::connection(
                        ErrorCode::FrameSizeError,
                        "SETTINGS payload length is not a multiple of 6",
                    ));
                }
                let mut settings = Vec::with_capacity(payload.len() / 6);
                for chunk in payload.chunks_exact(6) {
                    let id = SettingId::from_u16(u16::from_be_bytes([chunk[0], chunk[1]]));
                    let value = read_u32(&chunk[2..6]);
                    validate_setting(id, value)?;
                    settings.push(Setting::new(id, value));
                }
                (
                    FrameHeader {
                        length,
                        frame_type,
                        flags,
                        stream_id,
                    },
                    FrameKind::Settings {
                        ack: false,
                        settings,
                    },
                )
            }
        }
        FrameType::PushPromise => {
            validate_stream(stream_id, frame_type)?;
            let body = strip_padding(flags, &payload)?;
            if body.len() < 4 {
                return Err(FrameError::connection(
                    ErrorCode::FrameSizeError,
                    "PUSH_PROMISE is shorter than its promised stream id",
                ));
            }
            let promised_stream_id = StreamId::new(read_u32(&body[..4]));
            if promised_stream_id.is_connection() {
                return Err(FrameError::connection(
                    ErrorCode::ProtocolError,
                    "PUSH_PROMISE promises stream 0",
                ));
            }
            (
                FrameHeader {
                    length,
                    frame_type,
                    flags,
                    stream_id,
                },
                FrameKind::PushPromise {
                    end_headers: flags & FLAG_END_HEADERS != 0,
                    promised_stream_id,
                    fragment: body[4..].to_vec(),
                },
            )
        }
        FrameType::Ping => {
            if !stream_id.is_connection() {
                return Err(FrameError::connection(
                    ErrorCode::ProtocolError,
                    "PING frame on a non-zero stream",
                ));
            }
            require_length(length, 8, frame_type)?;
            let mut opaque = [0_u8; 8];
            opaque.copy_from_slice(&payload);
            (
                FrameHeader {
                    length,
                    frame_type,
                    flags,
                    stream_id,
                },
                FrameKind::Ping {
                    ack: flags & FLAG_ACK != 0,
                    opaque,
                },
            )
        }
        FrameType::GoAway => {
            if !stream_id.is_connection() {
                return Err(FrameError::connection(
                    ErrorCode::ProtocolError,
                    "GOAWAY frame on a non-zero stream",
                ));
            }
            if length < 8 {
                return Err(FrameError::connection(
                    ErrorCode::FrameSizeError,
                    "GOAWAY payload is shorter than 8 octets",
                ));
            }
            (
                FrameHeader {
                    length,
                    frame_type,
                    flags,
                    stream_id,
                },
                FrameKind::GoAway {
                    last_stream_id: StreamId::new(read_u32(&payload[..4])),
                    error_code: ErrorCode::from_u32(read_u32(&payload[4..8])),
                    debug_data: payload[8..].to_vec(),
                },
            )
        }
        FrameType::WindowUpdate => {
            require_length(length, 4, frame_type)?;
            let increment = read_u32(&payload) & 0x7fff_ffff;
            if increment == 0 {
                return if stream_id.is_connection() {
                    Err(FrameError::connection(
                        ErrorCode::ProtocolError,
                        "WINDOW_UPDATE increment is zero",
                    ))
                } else {
                    Err(FrameError::stream(
                        stream_id,
                        ErrorCode::ProtocolError,
                        "WINDOW_UPDATE increment is zero",
                    ))
                };
            }
            (
                FrameHeader {
                    length,
                    frame_type,
                    flags,
                    stream_id,
                },
                FrameKind::WindowUpdate { increment },
            )
        }
        FrameType::Continuation => {
            validate_stream(stream_id, frame_type)?;
            (
                FrameHeader {
                    length,
                    frame_type,
                    flags,
                    stream_id,
                },
                FrameKind::Continuation {
                    end_headers: flags & FLAG_END_HEADERS != 0,
                    fragment: payload,
                },
            )
        }
        FrameType::Unknown(value) => (
            FrameHeader {
                length,
                frame_type,
                flags,
                stream_id,
            },
            FrameKind::Unknown {
                frame_type: value,
                payload,
            },
        ),
    };
    Ok(Frame { header, kind })
}

fn validate_setting(id: SettingId, value: u32) -> Result<(), FrameError> {
    match id {
        SettingId::EnablePush if value > 1 => Err(FrameError::connection(
            ErrorCode::ProtocolError,
            "SETTINGS_ENABLE_PUSH must be 0 or 1",
        )),
        SettingId::InitialWindowSize if value > 0x7fff_ffff => Err(FrameError::connection(
            ErrorCode::FlowControlError,
            "SETTINGS_INITIAL_WINDOW_SIZE exceeds 2^31-1",
        )),
        SettingId::MaxFrameSize
            if !(DEFAULT_MAX_FRAME_SIZE..=MAX_MAX_FRAME_SIZE).contains(&value) =>
        {
            Err(FrameError::connection(
                ErrorCode::ProtocolError,
                "SETTINGS_MAX_FRAME_SIZE is outside the legal range",
            ))
        }
        _ => Ok(()),
    }
}

fn require_length(length: u32, expected: u32, frame_type: FrameType) -> Result<(), FrameError> {
    if length == expected {
        Ok(())
    } else {
        Err(FrameError::connection(
            ErrorCode::FrameSizeError,
            format!(
                "{} frame length {length} is not {expected}",
                frame_type.name()
            ),
        ))
    }
}

fn strip_padding(flags: u8, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if flags & FLAG_PADDED == 0 {
        return Ok(payload.to_vec());
    }
    let Some((&padding_length, rest)) = payload.split_first() else {
        return Err(FrameError::connection(
            ErrorCode::ProtocolError,
            "padded frame has no Pad Length octet",
        ));
    };
    let padding_length = usize::from(padding_length);
    if padding_length > rest.len() {
        return Err(FrameError::connection(
            ErrorCode::ProtocolError,
            "padding is longer than the frame payload",
        ));
    }
    Ok(rest[..rest.len() - padding_length].to_vec())
}

fn parse_priority(
    flags: u8,
    stream_id: StreamId,
    body: Vec<u8>,
) -> Result<(Option<Priority>, Vec<u8>), FrameError> {
    if flags & FLAG_PRIORITY == 0 {
        return Ok((None, body));
    }
    if body.len() < 5 {
        return Err(FrameError::connection(
            ErrorCode::FrameSizeError,
            "HEADERS priority block is shorter than 5 octets",
        ));
    }
    let priority = decode_priority(stream_id, &body[..5])?;
    Ok((Some(priority), body[5..].to_vec()))
}

fn decode_priority(stream_id: StreamId, bytes: &[u8]) -> Result<Priority, FrameError> {
    let raw = read_u32(&bytes[..4]);
    let dependency = StreamId::new(raw);
    if dependency == stream_id && !stream_id.is_connection() {
        return Err(FrameError::stream(
            stream_id,
            ErrorCode::ProtocolError,
            "stream depends on itself",
        ));
    }
    Ok(Priority {
        exclusive: raw & 0x8000_0000 != 0,
        dependency,
        weight: bytes[4],
    })
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn read_full<R: Read>(reader: &mut R, buffer: &mut [u8]) -> Result<bool, FrameError> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => {
                return Err(FrameError::Io("connection ended inside a frame".to_owned()));
            }
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(FrameError::Io(error.to_string())),
        }
    }
    Ok(true)
}

impl fmt::Display for FrameHeader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} len={} flags=0x{:02x} {}",
            self.frame_type.name(),
            self.length,
            self.flags,
            self.stream_id
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error_code(result: Result<Frame, FrameError>) -> ErrorCode {
        result.expect_err("frame should be rejected").code()
    }

    #[test]
    fn round_trips_every_defined_frame_type() {
        let frames = vec![
            Frame::data(StreamId::new(1), true, b"hello".to_vec()),
            Frame::headers(
                StreamId::new(1),
                false,
                true,
                Some(Priority::new(true, StreamId::new(3), 15)),
                b"header block".to_vec(),
            ),
            Frame::priority(
                StreamId::new(3),
                Priority::new(false, StreamId::new(1), 200),
            ),
            Frame::rst_stream(StreamId::new(1), ErrorCode::Cancel),
            Frame::settings(vec![
                Setting::new(SettingId::HeaderTableSize, 4096),
                Setting::new(SettingId::MaxFrameSize, 32_768),
            ]),
            Frame::settings_ack(),
            Frame::push_promise(
                StreamId::new(1),
                StreamId::new(2),
                true,
                b"promise".to_vec(),
            ),
            Frame::ping(false, *b"12345678"),
            Frame::goaway(
                StreamId::new(1),
                ErrorCode::ProtocolError,
                b"debug".to_vec(),
            ),
            Frame::window_update(StreamId::CONNECTION, 1024),
            Frame::continuation(StreamId::new(1), true, b"rest".to_vec()),
        ];
        for frame in frames {
            let bytes = frame.encode().expect("encode frame");
            let decoded = Frame::decode(&bytes, MAX_MAX_FRAME_SIZE).expect("decode frame");
            assert_eq!(decoded.header().frame_type(), frame.header().frame_type());
            assert_eq!(decoded.header().stream_id(), frame.header().stream_id());
            assert_eq!(decoded.kind(), frame.kind());
        }
    }

    #[test]
    fn decodes_padded_data_and_headers() {
        // DATA, PADDED|END_STREAM, stream 1, pad length 2, "abc", two zero pads.
        let bytes = [0, 0, 6, 0x0, 0x9, 0, 0, 0, 1, 2, b'a', b'b', b'c', 0, 0];
        let frame = Frame::decode(&bytes, 16_384).expect("padded data");
        assert_eq!(frame.header().length(), 6);
        assert_eq!(
            frame.kind(),
            &FrameKind::Data {
                data: b"abc".to_vec()
            }
        );

        // HEADERS with priority and one pad byte.
        let mut payload = vec![1]; // pad length
        payload.extend_from_slice(&[0, 0, 0, 3, 16]); // dependency 3, weight 16
        payload.push(b'x');
        payload.push(0);
        let mut wire = vec![
            0,
            0,
            u8::try_from(payload.len()).unwrap(),
            1,
            0x2d,
            0,
            0,
            0,
            1,
        ];
        wire.extend_from_slice(&payload);
        let frame = Frame::decode(&wire, 16_384).expect("padded headers");
        match frame.kind() {
            FrameKind::Headers {
                end_stream,
                end_headers,
                priority,
                fragment,
            } => {
                assert!(*end_stream);
                assert!(*end_headers);
                assert_eq!(*priority, Some(Priority::new(false, StreamId::new(3), 16)));
                assert_eq!(fragment, b"x");
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }

    #[test]
    fn rejects_oversized_and_malformed_frames() {
        // Declared length larger than the configured maximum.
        let bytes = [0, 0x40, 0x01, 0x0, 0, 0, 0, 0, 1];
        assert_eq!(
            error_code(Frame::decode(&bytes, 16_384)),
            ErrorCode::FrameSizeError
        );

        // DATA on stream 0.
        let bytes = [0, 0, 1, 0x0, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            error_code(Frame::decode(&bytes, 16_384)),
            ErrorCode::ProtocolError
        );

        // SETTINGS ACK with a payload.
        let bytes = [0, 0, 6, 0x4, 0x1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1];
        assert_eq!(
            error_code(Frame::decode(&bytes, 16_384)),
            ErrorCode::FrameSizeError
        );

        // PING with the wrong length.
        let bytes = [0, 0, 7, 0x6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            error_code(Frame::decode(&bytes, 16_384)),
            ErrorCode::FrameSizeError
        );

        // PRIORITY that depends on itself is a stream error.
        let error = Frame::decode(&[0, 0, 5, 0x2, 0, 0, 0, 0, 3, 0, 0, 0, 3, 0], 16_384)
            .expect_err("self dependency");
        assert_eq!(error.code(), ErrorCode::ProtocolError);
        assert_eq!(error.stream_id(), Some(StreamId::new(3)));

        // WINDOW_UPDATE increment 0 is a stream error on a stream and a
        // connection error on stream 0.
        let stream_error = Frame::decode(&[0, 0, 4, 0x8, 0, 0, 0, 0, 1, 0, 0, 0, 0], 16_384)
            .expect_err("zero stream window");
        assert_eq!(stream_error.stream_id(), Some(StreamId::new(1)));
        let connection_error = Frame::decode(&[0, 0, 4, 0x8, 0, 0, 0, 0, 0, 0, 0, 0, 0], 16_384)
            .expect_err("zero connection window");
        assert!(connection_error.is_connection_error());

        // Padding longer than the payload.
        let bytes = [0, 0, 2, 0x0, 0x8, 0, 0, 0, 1, 5, 0];
        assert_eq!(
            error_code(Frame::decode(&bytes, 16_384)),
            ErrorCode::ProtocolError
        );
    }

    #[test]
    fn validates_settings_ranges() {
        // INITIAL_WINDOW_SIZE above 2^31-1.
        let bytes = [0, 0, 6, 0x4, 0, 0, 0, 0, 0, 0, 4, 0x80, 0, 0, 0];
        assert_eq!(
            error_code(Frame::decode(&bytes, 16_384)),
            ErrorCode::FlowControlError
        );

        // MAX_FRAME_SIZE below the required 16384.
        let bytes = [0, 0, 6, 0x4, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0x3f, 0xff];
        assert_eq!(
            error_code(Frame::decode(&bytes, 16_384)),
            ErrorCode::ProtocolError
        );

        // Unknown setting is preserved, not rejected.
        let bytes = [0, 0, 6, 0x4, 0, 0, 0, 0, 0, 0x12, 0x34, 0, 0, 0, 7];
        let frame = Frame::decode(&bytes, 16_384).expect("unknown setting");
        assert_eq!(
            frame.kind(),
            &FrameKind::Settings {
                ack: false,
                settings: vec![Setting::new(SettingId::Unknown(0x1234), 7)],
            }
        );
    }

    #[test]
    fn reads_frame_from_reader_and_reports_clean_eof() {
        let bytes = Frame::ping(false, *b"abcdefgh")
            .encode()
            .expect("encode ping");
        let mut cursor = io::Cursor::new(bytes);
        let frame = Frame::read_from(&mut cursor, 16_384).expect("read ping");
        assert!(matches!(frame.kind(), FrameKind::Ping { ack: false, .. }));

        let error = Frame::read_from(&mut io::Cursor::new(Vec::new()), 16_384).expect_err("eof");
        assert_eq!(error, FrameError::Closed);
    }
}
