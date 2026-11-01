use std::collections::HashMap;
use std::io::{Read, Write};

use super::error::{ErrorCode, FrameError, H2Error, HpackError, StreamId, UnsupportedFeature};
use super::frame::{
    CONNECTION_PREFACE, DEFAULT_MAX_FRAME_SIZE, Frame, FrameKind, MAX_MAX_FRAME_SIZE, Setting,
    SettingId,
};
use super::hpack::{HeaderField, HpackDecoder, HpackEncoder};

/// Largest flow-control window permitted by RFC 9113.
const MAX_WINDOW_SIZE: i64 = 0x7fff_ffff;
/// The initial connection-level receive window (RFC 9113 section 6.9.2).
const INITIAL_CONNECTION_WINDOW: i64 = 65_535;
/// Sanity bound for debug data attached to locally generated `GOAWAY`.
const MAX_GOAWAY_DEBUG: usize = 256;
/// Our chosen HPACK encoder capacity, even when a peer advertises a larger
/// `SETTINGS_HEADER_TABLE_SIZE`. RFC 7541 permits an encoder to use less.
const MAX_ENCODER_TABLE_SIZE: usize = 64 << 10;

/// Configuration for a cleartext (prior-knowledge) HTTP/2 client connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct H2Config {
    /// Largest frame payload this endpoint accepts. Advertised through
    /// `SETTINGS_MAX_FRAME_SIZE`; legal values are 16384..=16777215.
    pub max_frame_size: u32,
    /// Initial per-stream receive window. Advertised through
    /// `SETTINGS_INITIAL_WINDOW_SIZE`.
    pub initial_window_size: u32,
    /// HPACK dynamic table capacity this endpoint allows its peer's encoder to
    /// use. Advertised through `SETTINGS_HEADER_TABLE_SIZE`.
    pub header_table_size: u32,
    /// Advisory decoded header-list bound advertised through
    /// `SETTINGS_MAX_HEADER_LIST_SIZE`.
    pub max_header_list_size: u32,
    /// Compressed header block bound across `HEADERS` + `CONTINUATION`.
    pub max_header_block_size: usize,
    /// Bound on a response body assembled from `DATA` frames.
    pub max_body_size: usize,
}

impl Default for H2Config {
    fn default() -> Self {
        Self {
            max_frame_size: DEFAULT_MAX_FRAME_SIZE,
            initial_window_size: 65_535,
            header_table_size: 4096,
            max_header_list_size: 1 << 20,
            max_header_block_size: 64 << 10,
            max_body_size: 8 << 20,
        }
    }
}

impl H2Config {
    fn validate(&self) -> Result<(), H2Error> {
        if !(DEFAULT_MAX_FRAME_SIZE..=MAX_MAX_FRAME_SIZE).contains(&self.max_frame_size) {
            return Err(H2Error::InvalidConfig(format!(
                "max_frame_size {} is outside 16384..=16777215",
                self.max_frame_size
            )));
        }
        if self.initial_window_size > 0x7fff_ffff {
            return Err(H2Error::InvalidConfig(
                "initial_window_size exceeds 2^31-1".to_owned(),
            ));
        }
        if self.max_header_block_size == 0 {
            return Err(H2Error::InvalidConfig(
                "max_header_block_size must be non-zero".to_owned(),
            ));
        }
        Ok(())
    }
}

/// A client request over HTTP/2.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct H2Request {
    pub method: String,
    pub scheme: String,
    pub authority: String,
    pub path: String,
    pub headers: Vec<HeaderField>,
    pub body: Option<Vec<u8>>,
}

impl H2Request {
    /// Creates a request with explicit method, scheme, authority, and path.
    #[must_use]
    pub fn new(
        method: impl Into<String>,
        scheme: impl Into<String>,
        authority: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            method: method.into(),
            scheme: scheme.into(),
            authority: authority.into(),
            path: path.into(),
            headers: Vec::new(),
            body: None,
        }
    }

    /// Creates a cleartext `GET` for `authority` and `path`.
    #[must_use]
    pub fn get(authority: impl Into<String>, path: impl Into<String>) -> Self {
        Self::new("GET", "http", authority, path)
    }

    /// Adds an ASCII-valued request header. Validation happens in
    /// [`H2Connection::send_request`].
    #[must_use]
    pub fn with_header(mut self, name: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        self.headers
            .push(HeaderField::new(name.as_ref(), value.as_ref()));
        self
    }

    /// Adds an arbitrary-octet request header.
    #[must_use]
    pub fn with_header_bytes(mut self, name: impl AsRef<[u8]>, value: impl AsRef<[u8]>) -> Self {
        self.headers.push(HeaderField::new(name, value));
        self
    }

    /// Attaches a request body, sent in flow-controlled `DATA` frames.
    #[must_use]
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Overrides the scheme (`http` or `https`).
    #[must_use]
    pub fn with_scheme(mut self, scheme: impl Into<String>) -> Self {
        self.scheme = scheme.into();
        self
    }
}

/// A complete HTTP/2 response. The pseudo-header `:status` is exposed as
/// [`H2Response::status`]; regular headers and trailers stay in order in
/// [`H2Response::headers`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct H2Response {
    pub stream_id: StreamId,
    pub status: u16,
    pub headers: Vec<HeaderField>,
    pub body: Vec<u8>,
}

/// Observable counters for protocol decisions that are accepted but not
/// implemented, plus basic frame totals.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct H2Stats {
    pub frames_read: u64,
    pub frames_written: u64,
    pub settings_acks_sent: u64,
    pub window_updates_sent: u64,
    pub goaway_sent: u64,
    /// `PRIORITY` frames validated and intentionally not scheduled by.
    pub priority_frames_ignored: u64,
    /// Unknown frame types ignored as RFC 9113 section 4.1 requires.
    pub unknown_frames_ignored: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PeerSettings {
    max_concurrent_streams: Option<u32>,
    initial_window_size: u32,
    max_frame_size: u32,
}

impl Default for PeerSettings {
    fn default() -> Self {
        Self {
            max_concurrent_streams: None,
            initial_window_size: 65_535,
            max_frame_size: DEFAULT_MAX_FRAME_SIZE,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StreamState {
    Open,
    HalfClosedLocal,
    HalfClosedRemote,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectionState {
    Open,
    Closed,
}

struct Stream {
    state: StreamState,
    send_window: i64,
    recv_window: i64,
    end_stream_sent: bool,
    end_stream_received: bool,
    response_status: Option<u16>,
    response_headers: Vec<HeaderField>,
    body: Vec<u8>,
}

impl Stream {
    fn new(send_window: u32, recv_window: u32) -> Self {
        Self {
            state: StreamState::Open,
            send_window: i64::from(send_window),
            recv_window: i64::from(recv_window),
            end_stream_sent: false,
            end_stream_received: false,
            response_status: None,
            response_headers: Vec::new(),
            body: Vec::new(),
        }
    }

    const fn is_active(&self) -> bool {
        matches!(
            self.state,
            StreamState::Open | StreamState::HalfClosedLocal | StreamState::HalfClosedRemote
        )
    }
}

struct PendingInboundHeaders {
    stream_id: StreamId,
    trailers: bool,
    end_stream: bool,
    fragments: Vec<u8>,
}

/// A synchronous cleartext HTTP/2 client connection.
///
/// The caller owns a `Read + Write` transport. [`H2Connection::connect`] sends
/// the client connection preface and SETTINGS, reads the server's initial
/// SETTINGS, and acknowledges them. Multiple [`H2Connection::send_request`]
/// calls can be outstanding on one connection; [`H2Connection::read_response`]
/// dispatches frames to the requested stream, which is the multiplexing
/// foundation the HTTP/1.1 transport currently lacks.
///
/// Scope boundaries:
/// - **Server push** is not implemented. Receiving `PUSH_PROMISE` fails with
///   [`H2Error::Unsupported`] and a protocol error, never a silent drop.
/// - **Priority scheduling** is not implemented. `PRIORITY` and the priority
///   block of `HEADERS` are validated, counted in
///   [`H2Stats::priority_frames_ignored`], and otherwise ignored as RFC 9113
///   permits.
/// - **TLS/ALPN** is outside this module. This type performs cleartext prior
///   knowledge so it can be tested without the network. Wiring it behind rustls
///   ALPN is the next transport step.
/// - Informational (1xx) responses are decoded and discarded while waiting
///   for the final response.
pub struct H2Connection<S> {
    io: S,
    config: H2Config,
    state: ConnectionState,
    encoder: HpackEncoder,
    decoder: HpackDecoder,
    peer: PeerSettings,
    next_stream_id: u32,
    streams: HashMap<StreamId, Stream>,
    conn_send_window: i64,
    conn_recv_window: i64,
    pending_inbound: Option<PendingInboundHeaders>,
    goaway: Option<(StreamId, ErrorCode, Vec<u8>)>,
    stats: H2Stats,
}

impl<S: Read + Write> H2Connection<S> {
    /// Performs the client connection preface and SETTINGS handshake.
    ///
    /// This blocks until the server's initial (non-ACK) SETTINGS frame arrives
    /// and the acknowledgement has been written.
    ///
    /// # Errors
    ///
    /// Returns [`H2Error::InvalidConfig`] for an illegal local setting,
    /// [`H2Error::Io`] or [`H2Error::ConnectionClosed`] for transport failure,
    /// and a protocol error if the server's first frame is not SETTINGS or its
    /// settings violate RFC 9113.
    pub fn connect(io: S, config: H2Config) -> Result<Self, H2Error> {
        config.validate()?;
        let decoder =
            HpackDecoder::new(usize::try_from(config.header_table_size).unwrap_or(usize::MAX))
                .with_header_list_limit(
                    usize::try_from(config.max_header_list_size).unwrap_or(usize::MAX),
                );
        let mut connection = Self {
            io,
            config,
            state: ConnectionState::Open,
            encoder: HpackEncoder::new(4096),
            decoder,
            peer: PeerSettings::default(),
            next_stream_id: 1,
            streams: HashMap::new(),
            conn_send_window: INITIAL_CONNECTION_WINDOW,
            conn_recv_window: INITIAL_CONNECTION_WINDOW,
            pending_inbound: None,
            goaway: None,
            stats: H2Stats::default(),
        };
        connection.write_connection_preface()?;
        connection.send_local_settings()?;
        if let Err(error) = connection.read_initial_settings() {
            if error.is_connection_error() {
                connection.fail_connection(&error);
            }
            return Err(error);
        }
        Ok(connection)
    }

    /// Sends one request and returns its stream identifier.
    ///
    /// A request body is split into `DATA` frames bounded by both flow-control
    /// windows and the peer's maximum frame size. If the windows are exhausted,
    /// this method reads and processes control frames (and buffers any response
    /// frames) until `WINDOW_UPDATE` makes progress.
    ///
    /// # Errors
    ///
    /// Returns [`H2Error::InvalidRequest`] for malformed pseudo headers,
    /// uppercase or illegal field names, forbidden connection-specific
    /// headers, or an invalid scheme/path; [`H2Error::Unsupported`] is
    /// reserved for protocol features this foundation does not implement;
    /// and [`H2Error::StreamIdExhausted`] / [`H2Error::ConcurrentStreamLimit`]
    /// report stream-space exhaustion.
    pub fn send_request(&mut self, request: &H2Request) -> Result<StreamId, H2Error> {
        self.ensure_open()?;
        validate_request(request)?;
        if let Some(limit) = self.peer.max_concurrent_streams {
            let active = self
                .streams
                .values()
                .filter(|stream| stream.is_active())
                .count();
            if active >= usize::try_from(limit).unwrap_or(usize::MAX) {
                return Err(H2Error::ConcurrentStreamLimit { limit });
            }
        }

        let fields = request_fields(request);
        let block = self.encoder.encode(&fields);
        let stream_id = self.allocate_stream_id()?;
        let body = request.body.as_deref().unwrap_or(&[]);
        let end_on_headers = body.is_empty();

        self.streams.insert(
            stream_id,
            Stream::new(
                self.peer.initial_window_size,
                self.config.initial_window_size,
            ),
        );
        self.send_header_block(stream_id, end_on_headers, &block)?;
        if end_on_headers {
            self.mark_local_end(stream_id)?;
        } else {
            self.send_body(stream_id, body)?;
        }
        Ok(stream_id)
    }

    /// Reads frames until `stream_id` has a complete response.
    ///
    /// Frames for other multiplexed streams are decoded into their own buffers
    /// and are available to later `read_response` calls for those streams.
    ///
    /// # Errors
    ///
    /// Returns a typed [`H2Error`] for a peer `RST_STREAM` or `GOAWAY`, a
    /// transport failure, a malformed response header block, a flow-control
    /// violation, or a response larger than [`H2Config::max_body_size`].
    pub fn read_response(&mut self, stream_id: StreamId) -> Result<H2Response, H2Error> {
        self.ensure_open()?;
        if !self.streams.contains_key(&stream_id) {
            return Err(H2Error::InvalidRequest(format!(
                "{stream_id} was not opened by this connection"
            )));
        }
        loop {
            if let Some(response) = self.completed_response(stream_id)? {
                return Ok(response);
            }
            self.pump_one_frame()?;
        }
    }

    /// Sends a `GOAWAY(NO_ERROR)` and closes the local side.
    ///
    /// # Errors
    ///
    /// Returns an I/O or frame serialization error if the best-effort
    /// `GOAWAY` cannot be written. The connection is marked closed in all
    /// cases once this method returns.
    pub fn close(&mut self) -> Result<(), H2Error> {
        if self.state == ConnectionState::Open {
            let last = self.last_stream_id();
            let result = self.write_frame(&Frame::goaway(last, ErrorCode::NoError, Vec::new()));
            self.stats.goaway_sent += 1;
            self.state = ConnectionState::Closed;
            result?;
        }
        Ok(())
    }

    /// Returns the protocol counters accumulated so far.
    #[must_use]
    pub const fn stats(&self) -> &H2Stats {
        &self.stats
    }

    /// The server's advertised initial window size for streams we send on.
    #[must_use]
    pub const fn peer_initial_window_size(&self) -> u32 {
        self.peer.initial_window_size
    }

    /// The server's advertised maximum frame size for frames we send.
    #[must_use]
    pub const fn peer_max_frame_size(&self) -> u32 {
        self.peer.max_frame_size
    }

    /// Whether the connection has been closed locally or by the peer.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        matches!(self.state, ConnectionState::Closed)
    }

    fn write_connection_preface(&mut self) -> Result<(), H2Error> {
        self.io
            .write_all(CONNECTION_PREFACE)
            .map_err(|error| H2Error::Io(error.to_string()))?;
        self.io
            .flush()
            .map_err(|error| H2Error::Io(error.to_string()))
    }

    fn send_local_settings(&mut self) -> Result<(), H2Error> {
        let settings = vec![
            Setting::new(SettingId::HeaderTableSize, self.config.header_table_size),
            Setting::new(SettingId::EnablePush, 0),
            Setting::new(
                SettingId::InitialWindowSize,
                self.config.initial_window_size,
            ),
            Setting::new(SettingId::MaxFrameSize, self.config.max_frame_size),
            Setting::new(
                SettingId::MaxHeaderListSize,
                self.config.max_header_list_size,
            ),
        ];
        self.write_frame(&Frame::settings(settings))?;
        if self.config.initial_window_size > 65_535 {
            let increment = self.config.initial_window_size - 65_535;
            self.write_frame(&Frame::window_update(StreamId::CONNECTION, increment))?;
            self.stats.window_updates_sent += 1;
            self.conn_recv_window = i64::from(self.config.initial_window_size);
        }
        Ok(())
    }

    fn read_initial_settings(&mut self) -> Result<(), H2Error> {
        let frame = self.read_next_frame()?;
        match frame.kind() {
            FrameKind::Settings {
                ack: false,
                settings,
            } => {
                self.apply_peer_settings(settings)?;
                self.write_frame(&Frame::settings_ack())?;
                self.stats.settings_acks_sent += 1;
                Ok(())
            }
            FrameKind::Settings { ack: true, .. } => Err(H2Error::Protocol {
                code: ErrorCode::ProtocolError,
                detail: "server preface must start with non-ACK SETTINGS".to_owned(),
            }),
            _ => Err(H2Error::Protocol {
                code: ErrorCode::ProtocolError,
                detail: "server preface must start with SETTINGS".to_owned(),
            }),
        }
    }

    fn apply_peer_settings(&mut self, settings: &[Setting]) -> Result<(), H2Error> {
        for setting in settings {
            match setting.id {
                SettingId::HeaderTableSize => {
                    let size = usize::try_from(setting.value)
                        .unwrap_or(usize::MAX)
                        .min(MAX_ENCODER_TABLE_SIZE);
                    self.encoder.set_max_table_size(size);
                }
                SettingId::EnablePush => {
                    if setting.value != 0 {
                        return Err(H2Error::Protocol {
                            code: ErrorCode::ProtocolError,
                            detail: "server must not enable push".to_owned(),
                        });
                    }
                }
                SettingId::MaxConcurrentStreams => {
                    self.peer.max_concurrent_streams = Some(setting.value);
                }
                SettingId::InitialWindowSize => {
                    let new_size = i64::from(setting.value);
                    let delta = new_size - i64::from(self.peer.initial_window_size);
                    self.peer.initial_window_size = setting.value;
                    for stream in self
                        .streams
                        .values_mut()
                        .filter(|stream| stream.is_active())
                    {
                        stream.send_window += delta;
                        if stream.send_window > MAX_WINDOW_SIZE {
                            return Err(H2Error::Protocol {
                                code: ErrorCode::FlowControlError,
                                detail: "SETTINGS_INITIAL_WINDOW_SIZE overflows a stream window"
                                    .to_owned(),
                            });
                        }
                    }
                }
                SettingId::MaxFrameSize => {
                    self.peer.max_frame_size = setting.value;
                }
                SettingId::MaxHeaderListSize | SettingId::Unknown(_) => {}
            }
        }
        Ok(())
    }

    fn allocate_stream_id(&mut self) -> Result<StreamId, H2Error> {
        let raw = self.next_stream_id;
        if raw > 0x7fff_ffff {
            return Err(H2Error::StreamIdExhausted);
        }
        let next = raw.checked_add(2).ok_or(H2Error::StreamIdExhausted)?;
        self.next_stream_id = next;
        Ok(StreamId::new(raw))
    }

    fn last_stream_id(&self) -> StreamId {
        let raw = self.next_stream_id.saturating_sub(2);
        StreamId::new(raw)
    }

    fn send_header_block(
        &mut self,
        stream_id: StreamId,
        end_stream: bool,
        block: &[u8],
    ) -> Result<(), H2Error> {
        let max = usize::try_from(self.peer.max_frame_size).unwrap_or(usize::MAX);
        let first_end = block.len() <= max;
        let first_len = block.len().min(max);
        let frame = Frame::headers(stream_id, end_stream, first_end, None, &block[..first_len]);
        self.write_frame(&frame)?;

        let mut offset = first_len;
        while offset < block.len() {
            let end = (offset + max).min(block.len());
            let end_headers = end == block.len();
            let frame = Frame::continuation(stream_id, end_headers, &block[offset..end]);
            self.write_frame(&frame)?;
            offset = end;
        }
        Ok(())
    }

    fn send_body(&mut self, stream_id: StreamId, body: &[u8]) -> Result<(), H2Error> {
        let mut offset = 0;
        while offset < body.len() {
            let available = loop {
                if let Some(stream) = self.streams.get(&stream_id)
                    && stream.end_stream_received
                {
                    return Err(H2Error::Stream {
                        stream_id,
                        code: ErrorCode::StreamClosed,
                        detail: "peer ended the stream before the request body was sent".to_owned(),
                    });
                }
                let available = self
                    .conn_send_window
                    .min(
                        self.streams
                            .get(&stream_id)
                            .map_or(0, |stream| stream.send_window),
                    )
                    .max(0);
                if available > 0 {
                    break available;
                }
                self.pump_one_frame()?;
            };

            let max_frame = usize::try_from(self.peer.max_frame_size).unwrap_or(usize::MAX);
            let remaining = body.len() - offset;
            let chunk = usize::try_from(available)
                .unwrap_or(usize::MAX)
                .min(max_frame)
                .min(remaining);
            let end = offset + chunk == body.len();
            let frame = Frame::data(stream_id, end, &body[offset..offset + chunk]);
            self.write_frame(&frame)?;
            let sent = i64::try_from(chunk).expect("frame chunk fits i64");
            self.conn_send_window -= sent;
            if let Some(stream) = self.streams.get_mut(&stream_id) {
                stream.send_window -= sent;
            }
            offset += chunk;
            if end {
                self.mark_local_end(stream_id)?;
            }
        }
        Ok(())
    }

    fn pump_one_frame(&mut self) -> Result<(), H2Error> {
        let result = self
            .read_next_frame()
            .and_then(|frame| self.handle_frame(&frame));
        if let Err(error) = &result {
            self.report_error(error);
        }
        result
    }

    fn read_next_frame(&mut self) -> Result<Frame, H2Error> {
        match Frame::read_from(&mut self.io, self.config.max_frame_size) {
            Ok(frame) => {
                self.stats.frames_read += 1;
                Ok(frame)
            }
            Err(FrameError::Closed) => {
                self.state = ConnectionState::Closed;
                Err(H2Error::ConnectionClosed)
            }
            Err(FrameError::Io(message)) => {
                self.state = ConnectionState::Closed;
                Err(H2Error::Io(message))
            }
            Err(error) => Err(H2Error::Frame(error)),
        }
    }

    fn write_frame(&mut self, frame: &Frame) -> Result<(), H2Error> {
        match frame.write_to(&mut self.io) {
            Ok(()) => {
                self.stats.frames_written += 1;
            }
            Err(FrameError::Io(message)) => {
                self.state = ConnectionState::Closed;
                return Err(H2Error::Io(message));
            }
            Err(error) => return Err(H2Error::Frame(error)),
        }
        self.io
            .flush()
            .map_err(|error| H2Error::Io(error.to_string()))
    }

    fn handle_frame(&mut self, frame: &Frame) -> Result<(), H2Error> {
        if let Some(pending) = &self.pending_inbound {
            match frame.kind() {
                FrameKind::Continuation { .. }
                    if frame.header().stream_id() == pending.stream_id => {}
                FrameKind::Continuation { .. } => {
                    return Err(H2Error::Protocol {
                        code: ErrorCode::ProtocolError,
                        detail: "CONTINUATION on a different stream than the open header block"
                            .to_owned(),
                    });
                }
                _ => {
                    return Err(H2Error::Protocol {
                        code: ErrorCode::ProtocolError,
                        detail: "header block was interrupted before END_HEADERS".to_owned(),
                    });
                }
            }
        } else if matches!(frame.kind(), FrameKind::Continuation { .. }) {
            return Err(H2Error::Protocol {
                code: ErrorCode::ProtocolError,
                detail: "CONTINUATION without a preceding HEADERS frame".to_owned(),
            });
        }

        let header = frame.header();
        let stream_id = header.stream_id();
        match frame.kind() {
            FrameKind::Data { data } => {
                self.handle_data(stream_id, header.end_stream(), header.length(), data)
            }
            FrameKind::Headers {
                end_stream,
                end_headers,
                priority,
                fragment,
            } => {
                if priority.is_some() {
                    self.stats.priority_frames_ignored += 1;
                }
                self.handle_headers(stream_id, *end_stream, *end_headers, fragment)
            }
            FrameKind::Priority { .. } => {
                self.stats.priority_frames_ignored += 1;
                Ok(())
            }
            FrameKind::RstStream { error_code } => self.handle_rst_stream(stream_id, *error_code),
            FrameKind::Settings { ack, settings } => {
                if *ack {
                    return Ok(());
                }
                let settings = settings.clone();
                self.apply_peer_settings(&settings)?;
                self.write_frame(&Frame::settings_ack())?;
                self.stats.settings_acks_sent += 1;
                Ok(())
            }
            FrameKind::PushPromise { .. } => {
                Err(H2Error::Unsupported(UnsupportedFeature::ServerPush))
            }
            FrameKind::Ping { ack, opaque } => {
                if !*ack {
                    self.write_frame(&Frame::ping(true, *opaque))?;
                }
                Ok(())
            }
            FrameKind::GoAway {
                last_stream_id,
                error_code,
                debug_data,
            } => {
                self.goaway = Some((*last_stream_id, *error_code, debug_data.clone()));
                self.state = ConnectionState::Closed;
                Err(H2Error::GoAway {
                    last_stream_id: *last_stream_id,
                    code: *error_code,
                    debug_data: debug_data.clone(),
                })
            }
            FrameKind::WindowUpdate { increment } => {
                self.handle_window_update(stream_id, *increment)
            }
            FrameKind::Continuation {
                end_headers,
                fragment,
            } => self.handle_continuation(stream_id, *end_headers, fragment),
            FrameKind::Unknown { .. } => {
                self.stats.unknown_frames_ignored += 1;
                Ok(())
            }
        }
    }

    fn handle_data(
        &mut self,
        stream_id: StreamId,
        end_stream: bool,
        flow_length: u32,
        data: &[u8],
    ) -> Result<(), H2Error> {
        let Some(stream) = self.streams.get(&stream_id) else {
            return Err(H2Error::Protocol {
                code: ErrorCode::ProtocolError,
                detail: format!("DATA on idle {stream_id}"),
            });
        };
        if stream.end_stream_received {
            return Err(H2Error::Stream {
                stream_id,
                code: ErrorCode::StreamClosed,
                detail: "DATA after END_STREAM".to_owned(),
            });
        }
        if stream.response_status.is_none() {
            return Err(H2Error::Stream {
                stream_id,
                code: ErrorCode::ProtocolError,
                detail: "DATA arrived before response HEADERS".to_owned(),
            });
        }

        let flow_length = i64::from(flow_length);
        self.conn_recv_window -= flow_length;
        if self.conn_recv_window < 0 {
            return Err(H2Error::Protocol {
                code: ErrorCode::FlowControlError,
                detail: "connection receive window exceeded".to_owned(),
            });
        }

        let max_body = self.config.max_body_size;
        let stream = self
            .streams
            .get_mut(&stream_id)
            .expect("stream existence checked above");
        stream.recv_window -= flow_length;
        if stream.recv_window < 0 {
            return Err(H2Error::Stream {
                stream_id,
                code: ErrorCode::FlowControlError,
                detail: "stream receive window exceeded".to_owned(),
            });
        }
        if stream.body.len() + data.len() > max_body {
            return Err(H2Error::Stream {
                stream_id,
                code: ErrorCode::EnhanceYourCalm,
                detail: format!("response body exceeds limit {max_body}"),
            });
        }
        stream.body.extend_from_slice(data);
        if end_stream {
            stream.end_stream_received = true;
            stream.state = match stream.state {
                StreamState::Open => StreamState::HalfClosedRemote,
                StreamState::HalfClosedLocal => StreamState::Closed,
                other => other,
            };
        }

        let increment = u32::try_from(flow_length).unwrap_or(0);
        if increment > 0 {
            self.send_window_update(StreamId::CONNECTION, increment)?;
            if !end_stream {
                self.send_window_update(stream_id, increment)?;
            }
        }
        Ok(())
    }

    fn handle_headers(
        &mut self,
        stream_id: StreamId,
        end_stream: bool,
        end_headers: bool,
        fragment: &[u8],
    ) -> Result<(), H2Error> {
        let Some(stream) = self.streams.get(&stream_id) else {
            return Err(H2Error::Protocol {
                code: ErrorCode::ProtocolError,
                detail: format!("HEADERS on idle {stream_id}"),
            });
        };
        if stream.end_stream_received {
            return Err(H2Error::Stream {
                stream_id,
                code: ErrorCode::StreamClosed,
                detail: "HEADERS after END_STREAM".to_owned(),
            });
        }
        let trailers = stream.response_status.is_some();
        if end_headers {
            self.finish_header_block(stream_id, trailers, end_stream, fragment)
        } else {
            self.check_header_block_size(fragment.len())?;
            self.pending_inbound = Some(PendingInboundHeaders {
                stream_id,
                trailers,
                end_stream,
                fragments: fragment.to_vec(),
            });
            Ok(())
        }
    }

    fn handle_continuation(
        &mut self,
        stream_id: StreamId,
        end_headers: bool,
        fragment: &[u8],
    ) -> Result<(), H2Error> {
        let Some(pending) = self.pending_inbound.as_ref() else {
            return Err(H2Error::Protocol {
                code: ErrorCode::ProtocolError,
                detail: "CONTINUATION without an open header block".to_owned(),
            });
        };
        debug_assert_eq!(pending.stream_id, stream_id);
        let next_len = pending.fragments.len() + fragment.len();
        self.pending_inbound
            .as_mut()
            .expect("checked above")
            .fragments
            .extend_from_slice(fragment);
        self.check_header_block_size(next_len)?;
        if end_headers {
            let pending = self
                .pending_inbound
                .take()
                .expect("pending header block checked above");
            self.finish_header_block(
                pending.stream_id,
                pending.trailers,
                pending.end_stream,
                &pending.fragments,
            )?;
        }
        Ok(())
    }

    fn check_header_block_size(&self, size: usize) -> Result<(), H2Error> {
        if size > self.config.max_header_block_size {
            return Err(H2Error::HeaderBlockTooLarge {
                limit: self.config.max_header_block_size,
            });
        }
        Ok(())
    }

    fn finish_header_block(
        &mut self,
        stream_id: StreamId,
        trailers: bool,
        end_stream: bool,
        fragment: &[u8],
    ) -> Result<(), H2Error> {
        self.check_header_block_size(fragment.len())?;
        let decoded = match self.decoder.decode(fragment) {
            Ok(decoded) => decoded,
            Err(HpackError::HeaderListTooLarge { limit }) => {
                return Err(H2Error::HeaderListTooLarge { limit });
            }
            Err(error) => return Err(H2Error::Hpack(error)),
        };
        let list_size: usize = decoded.iter().map(HeaderField::size).sum();
        let limit = usize::try_from(self.config.max_header_list_size).unwrap_or(usize::MAX);
        if list_size > limit {
            return Err(H2Error::Stream {
                stream_id,
                code: ErrorCode::EnhanceYourCalm,
                detail: format!("decoded header list exceeds limit {limit}"),
            });
        }
        if trailers && !end_stream {
            return Err(H2Error::Stream {
                stream_id,
                code: ErrorCode::ProtocolError,
                detail: "trailing HEADERS must carry END_STREAM".to_owned(),
            });
        }
        let (status, regular) = validate_response_fields(stream_id, &decoded, trailers)?;
        if let Some(status) = status {
            if (100..=199).contains(&status) {
                if end_stream {
                    return Err(H2Error::Stream {
                        stream_id,
                        code: ErrorCode::ProtocolError,
                        detail: "informational response ended the stream".to_owned(),
                    });
                }
                return Ok(());
            }
            let stream = self
                .streams
                .get_mut(&stream_id)
                .expect("stream existence checked before decoding");
            stream.response_status = Some(status);
            stream.response_headers.extend(regular);
        } else {
            let stream = self
                .streams
                .get_mut(&stream_id)
                .expect("stream existence checked before decoding");
            stream.response_headers.extend(regular);
        }
        if end_stream {
            self.mark_remote_end(stream_id)?;
        }
        Ok(())
    }

    fn handle_rst_stream(&mut self, stream_id: StreamId, code: ErrorCode) -> Result<(), H2Error> {
        let Some(stream) = self.streams.get_mut(&stream_id) else {
            return Err(H2Error::Protocol {
                code: ErrorCode::ProtocolError,
                detail: format!("RST_STREAM on idle {stream_id}"),
            });
        };
        if stream.state == StreamState::Closed {
            return Ok(());
        }
        stream.state = StreamState::Closed;
        Err(H2Error::StreamReset { stream_id, code })
    }

    fn handle_window_update(&mut self, stream_id: StreamId, increment: u32) -> Result<(), H2Error> {
        let increment = i64::from(increment);
        if stream_id.is_connection() {
            self.conn_send_window += increment;
            if self.conn_send_window > MAX_WINDOW_SIZE {
                return Err(H2Error::Protocol {
                    code: ErrorCode::FlowControlError,
                    detail: "connection send window overflow".to_owned(),
                });
            }
            return Ok(());
        }
        let Some(stream) = self.streams.get_mut(&stream_id) else {
            return Err(H2Error::Protocol {
                code: ErrorCode::ProtocolError,
                detail: format!("WINDOW_UPDATE on idle {stream_id}"),
            });
        };
        if stream.state == StreamState::Closed {
            return Ok(());
        }
        stream.send_window += increment;
        if stream.send_window > MAX_WINDOW_SIZE {
            return Err(H2Error::Stream {
                stream_id,
                code: ErrorCode::FlowControlError,
                detail: "stream send window overflow".to_owned(),
            });
        }
        Ok(())
    }

    fn send_window_update(&mut self, stream_id: StreamId, increment: u32) -> Result<(), H2Error> {
        if increment == 0 {
            return Ok(());
        }
        self.write_frame(&Frame::window_update(stream_id, increment))?;
        self.stats.window_updates_sent += 1;
        Ok(())
    }

    fn mark_local_end(&mut self, stream_id: StreamId) -> Result<(), H2Error> {
        let stream = self
            .streams
            .get_mut(&stream_id)
            .ok_or_else(|| H2Error::InvalidRequest(format!("unknown {stream_id}")))?;
        if stream.end_stream_sent {
            return Ok(());
        }
        stream.end_stream_sent = true;
        stream.state = match stream.state {
            StreamState::Open => StreamState::HalfClosedLocal,
            StreamState::HalfClosedRemote => StreamState::Closed,
            other => other,
        };
        Ok(())
    }

    fn mark_remote_end(&mut self, stream_id: StreamId) -> Result<(), H2Error> {
        let stream = self
            .streams
            .get_mut(&stream_id)
            .ok_or_else(|| H2Error::InvalidRequest(format!("unknown {stream_id}")))?;
        if stream.end_stream_received {
            return Err(H2Error::Stream {
                stream_id,
                code: ErrorCode::StreamClosed,
                detail: "duplicate END_STREAM".to_owned(),
            });
        }
        stream.end_stream_received = true;
        stream.state = match stream.state {
            StreamState::Open => StreamState::HalfClosedRemote,
            StreamState::HalfClosedLocal => StreamState::Closed,
            other => other,
        };
        Ok(())
    }

    fn completed_response(&self, stream_id: StreamId) -> Result<Option<H2Response>, H2Error> {
        let stream = self
            .streams
            .get(&stream_id)
            .ok_or_else(|| H2Error::InvalidRequest(format!("unknown {stream_id}")))?;
        if !stream.end_stream_received {
            return Ok(None);
        }
        let Some(status) = stream.response_status else {
            return Err(H2Error::Stream {
                stream_id,
                code: ErrorCode::ProtocolError,
                detail: "response ended without a final :status".to_owned(),
            });
        };
        Ok(Some(H2Response {
            stream_id,
            status,
            headers: stream.response_headers.clone(),
            body: stream.body.clone(),
        }))
    }

    fn report_error(&mut self, error: &H2Error) {
        match error {
            H2Error::StreamReset { stream_id, .. } => {
                if let Some(stream) = self.streams.get_mut(stream_id) {
                    stream.state = StreamState::Closed;
                }
            }
            H2Error::GoAway { .. } | H2Error::ConnectionClosed | H2Error::Io(_) => {
                self.state = ConnectionState::Closed;
            }
            _ if error.is_connection_error() => self.fail_connection(error),
            _ => {
                if let Some(stream_id) = error.stream_id() {
                    let code = error.code();
                    let _ = self.write_frame(&Frame::rst_stream(stream_id, code));
                    if let Some(stream) = self.streams.get_mut(&stream_id) {
                        stream.state = StreamState::Closed;
                    }
                }
            }
        }
    }

    fn fail_connection(&mut self, error: &H2Error) {
        if self.state == ConnectionState::Closed {
            return;
        }
        let code = if error.code() == ErrorCode::NoError {
            ErrorCode::InternalError
        } else {
            error.code()
        };
        let mut debug = error.to_string().into_bytes();
        debug.truncate(MAX_GOAWAY_DEBUG);
        let last = self.last_stream_id();
        let _ = self.write_frame(&Frame::goaway(last, code, debug));
        self.stats.goaway_sent += 1;
        self.state = ConnectionState::Closed;
    }

    fn ensure_open(&self) -> Result<(), H2Error> {
        if let Some((last_stream_id, code, debug_data)) = &self.goaway {
            return Err(H2Error::GoAway {
                last_stream_id: *last_stream_id,
                code: *code,
                debug_data: debug_data.clone(),
            });
        }
        if self.state == ConnectionState::Closed {
            return Err(H2Error::ConnectionClosed);
        }
        Ok(())
    }
}

fn request_fields(request: &H2Request) -> Vec<HeaderField> {
    let mut fields = Vec::with_capacity(4 + request.headers.len() + 1);
    fields.push(HeaderField::new(
        b":method".as_slice(),
        request.method.as_bytes(),
    ));
    fields.push(HeaderField::new(
        b":scheme".as_slice(),
        request.scheme.as_bytes(),
    ));
    fields.push(HeaderField::new(
        b":authority".as_slice(),
        request.authority.as_bytes(),
    ));
    fields.push(HeaderField::new(
        b":path".as_slice(),
        request.path.as_bytes(),
    ));
    let has_content_length = request
        .headers
        .iter()
        .any(|field| field.name.eq_ignore_ascii_case(b"content-length"));
    if let Some(body) = &request.body
        && !has_content_length
    {
        fields.push(HeaderField::new(
            b"content-length".as_slice(),
            body.len().to_string().into_bytes(),
        ));
    }
    fields.extend(request.headers.iter().cloned());
    fields
}

fn validate_request(request: &H2Request) -> Result<(), H2Error> {
    if request.method.eq_ignore_ascii_case("CONNECT") {
        return Err(H2Error::Unsupported(UnsupportedFeature::Connect));
    }
    if !is_token(request.method.as_bytes()) {
        return Err(H2Error::InvalidRequest(format!(
            "method {:?} is not an HTTP token",
            request.method
        )));
    }
    if request.scheme != "http" && request.scheme != "https" {
        return Err(H2Error::InvalidRequest(format!(
            "unsupported scheme {:?}",
            request.scheme
        )));
    }
    if request.authority.is_empty()
        || !request
            .authority
            .bytes()
            .all(|byte| byte.is_ascii_graphic())
    {
        return Err(H2Error::InvalidRequest(
            "authority must be non-empty visible ASCII".to_owned(),
        ));
    }
    if !request.path.starts_with('/') && request.path != "*" {
        return Err(H2Error::InvalidRequest(format!(
            "path {:?} must start with '/'",
            request.path
        )));
    }
    if !request.path.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(H2Error::InvalidRequest(
            "path must be visible ASCII".to_owned(),
        ));
    }
    for field in &request.headers {
        validate_field_name(&field.name).map_err(|detail| {
            H2Error::InvalidRequest(format!(
                "header {:?}: {detail}",
                String::from_utf8_lossy(&field.name)
            ))
        })?;
        if field.name.first() == Some(&b':') {
            return Err(H2Error::InvalidRequest(
                "caller-supplied pseudo headers are not allowed".to_owned(),
            ));
        }
        if is_connection_specific(&field.name) {
            return Err(H2Error::InvalidRequest(format!(
                "connection-specific header {:?} is forbidden in HTTP/2",
                String::from_utf8_lossy(&field.name)
            )));
        }
        if field.name == b"te" && !field.value.eq_ignore_ascii_case(b"trailers") {
            return Err(H2Error::InvalidRequest(
                "TE may only carry \"trailers\" in HTTP/2".to_owned(),
            ));
        }
        if !valid_field_value(&field.value) {
            return Err(H2Error::InvalidRequest(
                "header value contains NUL, CR, or LF".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_response_fields(
    stream_id: StreamId,
    fields: &[HeaderField],
    trailers: bool,
) -> Result<(Option<u16>, Vec<HeaderField>), H2Error> {
    let mut status = None;
    let mut regular = Vec::new();
    let mut saw_regular = false;

    for field in fields {
        if field.name.first() == Some(&b':') {
            if trailers {
                return Err(H2Error::Stream {
                    stream_id,
                    code: ErrorCode::ProtocolError,
                    detail: "pseudo header in trailers".to_owned(),
                });
            }
            if saw_regular {
                return Err(H2Error::Stream {
                    stream_id,
                    code: ErrorCode::ProtocolError,
                    detail: "pseudo header after regular header".to_owned(),
                });
            }
            if field.name != b":status" {
                return Err(H2Error::Stream {
                    stream_id,
                    code: ErrorCode::ProtocolError,
                    detail: format!(
                        "unexpected response pseudo header {:?}",
                        String::from_utf8_lossy(&field.name)
                    ),
                });
            }
            if status.is_some() {
                return Err(H2Error::Stream {
                    stream_id,
                    code: ErrorCode::ProtocolError,
                    detail: "duplicate :status".to_owned(),
                });
            }
            status = Some(parse_status(stream_id, &field.value)?);
        } else {
            validate_field_name(&field.name).map_err(|detail| H2Error::Stream {
                stream_id,
                code: ErrorCode::ProtocolError,
                detail,
            })?;
            if !valid_field_value(&field.value) {
                return Err(H2Error::Stream {
                    stream_id,
                    code: ErrorCode::ProtocolError,
                    detail: "response header value contains NUL, CR, or LF".to_owned(),
                });
            }
            saw_regular = true;
            regular.push(field.clone());
        }
    }

    if !trailers && status.is_none() {
        return Err(H2Error::Stream {
            stream_id,
            code: ErrorCode::ProtocolError,
            detail: "response HEADERS is missing :status".to_owned(),
        });
    }
    Ok((status, regular))
}

fn parse_status(stream_id: StreamId, value: &[u8]) -> Result<u16, H2Error> {
    if value.len() != 3 || !value.iter().all(u8::is_ascii_digit) {
        return Err(H2Error::Stream {
            stream_id,
            code: ErrorCode::ProtocolError,
            detail: "response :status must be exactly three digits".to_owned(),
        });
    }
    let status = value.iter().fold(0_u16, |accumulator, digit| {
        accumulator * 10 + u16::from(*digit - b'0')
    });
    Ok(status)
}

fn validate_field_name(name: &[u8]) -> Result<(), String> {
    if name.is_empty() {
        return Err("empty header name".to_owned());
    }
    if name.iter().any(u8::is_ascii_uppercase) {
        return Err("HTTP/2 header names must be lowercase".to_owned());
    }
    if !name.iter().all(|byte| is_token_byte(*byte)) {
        return Err("header name contains a non-token byte".to_owned());
    }
    Ok(())
}

fn valid_field_value(value: &[u8]) -> bool {
    !value.iter().any(|byte| matches!(byte, 0x00 | 0x0a | 0x0d))
}

fn is_connection_specific(name: &[u8]) -> bool {
    name == b"connection"
        || name == b"keep-alive"
        || name == b"proxy-connection"
        || name == b"transfer-encoding"
        || name == b"upgrade"
}

fn is_token(value: &[u8]) -> bool {
    !value.is_empty() && value.iter().all(|byte| is_token_byte(*byte))
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

#[cfg(test)]
mod tests;
