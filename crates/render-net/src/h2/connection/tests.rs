use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::*;
use crate::h2::HpackError;
use crate::h2::frame::{CONNECTION_PREFACE, Frame, FrameKind, Priority, Setting, SettingId};
use crate::h2::hpack::{HeaderField, HpackDecoder, HpackEncoder, Indexing};

fn spawn_server(handler: impl FnOnce(TcpStream) + Send + 'static) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback h2 server");
    let address = listener.local_addr().expect("read loopback address");
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept h2 client");
        handler(stream);
    });
    (address, handle)
}

fn connect(address: SocketAddr) -> H2Connection<TcpStream> {
    let stream = TcpStream::connect(address).expect("connect loopback h2 client");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set client read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set client write timeout");
    H2Connection::connect(stream, H2Config::default()).expect("HTTP/2 handshake")
}

fn field<'a>(fields: &'a [HeaderField], name: &str) -> Option<&'a [u8]> {
    fields
        .iter()
        .find(|field| field.name == name.as_bytes())
        .map(|field| field.value.as_slice())
}

fn response_header<'a>(response: &'a H2Response, name: &str) -> Option<&'a [u8]> {
    field(&response.headers, name)
}

struct TestServer {
    stream: TcpStream,
    decoder: HpackDecoder,
    encoder: HpackEncoder,
}

impl TestServer {
    fn new(stream: TcpStream) -> Self {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set server read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("set server write timeout");
        Self {
            stream,
            decoder: HpackDecoder::new(4096),
            encoder: HpackEncoder::new(4096),
        }
    }

    fn read_frame(&mut self) -> Frame {
        Frame::read_from(&mut self.stream, 16_384).expect("server reads a valid frame")
    }

    fn write_frame(&mut self, frame: &Frame) {
        frame
            .write_to(&mut self.stream)
            .expect("server writes a frame");
        self.stream.flush().expect("flush server frame");
    }

    fn handshake(&mut self, settings: &[Setting]) {
        let mut preface = [0_u8; 24];
        self.stream
            .read_exact(&mut preface)
            .expect("read client preface");
        assert_eq!(&preface, CONNECTION_PREFACE, "client connection preface");

        let frame = self.read_frame();
        match frame.kind() {
            FrameKind::Settings {
                ack: false,
                settings: client_settings,
            } => {
                assert!(
                    client_settings
                        .iter()
                        .any(|setting| setting.id == SettingId::EnablePush && setting.value == 0),
                    "client disables push"
                );
            }
            other => panic!("expected client SETTINGS, got {other:?}"),
        }

        self.write_frame(&Frame::settings(settings.to_vec()));
        let ack = self.read_frame();
        assert!(
            matches!(ack.kind(), FrameKind::Settings { ack: true, .. }),
            "client acknowledges server SETTINGS"
        );
    }

    fn read_request(&mut self) -> (StreamId, bool, Vec<HeaderField>) {
        let mut fragments = Vec::new();
        let mut stream_id = None;
        let mut end_stream = false;
        let mut end_headers = false;
        while !end_headers {
            let frame = self.read_frame();
            match frame.kind() {
                FrameKind::Headers {
                    end_stream: request_end,
                    end_headers: request_end_headers,
                    fragment,
                    ..
                } => {
                    stream_id = Some(frame.header().stream_id());
                    end_stream = *request_end;
                    end_headers = *request_end_headers;
                    fragments.extend_from_slice(fragment);
                }
                FrameKind::Continuation {
                    end_headers: continuation_end,
                    fragment,
                } => {
                    end_headers = *continuation_end;
                    fragments.extend_from_slice(fragment);
                }
                other => panic!("expected request HEADERS, got {other:?}"),
            }
        }
        let fields = self
            .decoder
            .decode(&fragments)
            .expect("server decodes request headers");
        (stream_id.expect("request stream id"), end_stream, fields)
    }

    fn send_response(
        &mut self,
        stream_id: StreamId,
        status: u16,
        headers: &[HeaderField],
        chunks: &[&[u8]],
        huffman: bool,
    ) {
        let mut fields = Vec::with_capacity(headers.len() + 1);
        fields.push(HeaderField::new(
            b":status".as_slice(),
            status.to_string().into_bytes(),
        ));
        fields.extend(headers.iter().cloned());
        let pairs: Vec<(HeaderField, Indexing)> = fields
            .into_iter()
            .map(|field| (field, Indexing::Incremental))
            .collect();
        let block = self.encoder.encode_with(&pairs, huffman);
        if block.len() > 1 {
            // Exercise the receiver's CONTINUATION path: END_STREAM belongs on
            // the initial HEADERS frame, END_HEADERS on the final continuation.
            let (first, rest) = block.split_at(1);
            self.write_frame(&Frame::headers(
                stream_id,
                chunks.is_empty(),
                false,
                None,
                first.to_vec(),
            ));
            self.write_frame(&Frame::continuation(stream_id, true, rest.to_vec()));
        } else {
            self.write_frame(&Frame::headers(
                stream_id,
                chunks.is_empty(),
                true,
                None,
                block,
            ));
        }
        for (index, chunk) in chunks.iter().enumerate() {
            let end = index + 1 == chunks.len();
            self.write_frame(&Frame::data(stream_id, end, chunk.to_vec()));
        }
    }

    fn read_request_body(&mut self) -> (Vec<u8>, Vec<u32>) {
        let mut body = Vec::new();
        let mut updates = Vec::new();
        loop {
            let frame = self.read_frame();
            let stream_id = frame.header().stream_id();
            match frame.kind() {
                FrameKind::Data { data } => {
                    body.extend_from_slice(data);
                    if frame.header().end_stream() {
                        break;
                    }
                    let increment = frame.header().length();
                    updates.push(increment);
                    self.write_frame(&Frame::window_update(stream_id, increment));
                }
                other => panic!("expected request DATA, got {other:?}"),
            }
        }
        (body, updates)
    }

    fn read_goaway(&mut self) -> ErrorCode {
        let frame = self.read_frame();
        match frame.kind() {
            FrameKind::GoAway { error_code, .. } => *error_code,
            other => panic!("expected GOAWAY, got {other:?}"),
        }
    }

    fn drain_window_updates(&mut self, expected: usize) {
        for _ in 0..expected {
            let frame = self.read_frame();
            assert!(
                matches!(frame.kind(), FrameKind::WindowUpdate { .. }),
                "expected WINDOW_UPDATE, got {:?}",
                frame.kind()
            );
        }
    }
}

#[test]
fn loopback_get_returns_huffman_headers_and_body() {
    let (address, server) = spawn_server(|stream| {
        let mut server = TestServer::new(stream);
        server.handshake(&[Setting::new(SettingId::MaxFrameSize, 16_384)]);
        let (stream_id, end_stream, fields) = server.read_request();
        assert_eq!(stream_id, StreamId::new(1));
        assert!(end_stream, "GET request headers carry END_STREAM");
        assert_eq!(field(&fields, ":method"), Some(&b"GET"[..]));
        assert_eq!(field(&fields, ":scheme"), Some(&b"http"[..]));
        assert_eq!(field(&fields, ":path"), Some(&b"/"[..]));
        assert_eq!(field(&fields, ":authority"), Some(&b"example.test"[..]));
        assert_eq!(field(&fields, "x-request"), Some(&b"yes"[..]));
        // Priority is validated, counted, and intentionally not scheduled.
        server.write_frame(&Frame::priority(
            stream_id,
            Priority::new(false, StreamId::CONNECTION, 15),
        ));
        server.send_response(
            stream_id,
            200,
            &[
                HeaderField::new("content-type", "text/plain; charset=utf-8"),
                HeaderField::new("x-marker", "huffman"),
            ],
            &[b"hello h2".as_slice()],
            true,
        );
        // Keep the receive side drained so closing the socket is orderly.
        server.drain_window_updates(1);
    });

    let mut client = connect(address);
    let request = H2Request::get("example.test", "/").with_header("x-request", "yes");
    let stream_id = client.send_request(&request).expect("send GET");
    let response = client.read_response(stream_id).expect("read GET response");

    assert_eq!(response.stream_id, stream_id);
    assert_eq!(response.status, 200);
    assert_eq!(
        response_header(&response, "content-type"),
        Some(&b"text/plain; charset=utf-8"[..])
    );
    assert_eq!(
        response_header(&response, "x-marker"),
        Some(&b"huffman"[..])
    );
    assert_eq!(response.body, b"hello h2");
    assert!(client.stats().frames_read > 0);
    assert_eq!(client.stats().priority_frames_ignored, 1);
    assert_eq!(client.stats().goaway_sent, 0);

    server.join().expect("server thread");
}

#[test]
fn loopback_splits_data_and_returns_flow_control_window_updates() {
    let body: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let chunks: [&[u8]; 3] = [&body[..10], &body[10..21], &body[21..]];
    let (address, server) = spawn_server(move |stream| {
        let mut server = TestServer::new(stream);
        server.handshake(&[Setting::new(SettingId::InitialWindowSize, 65_535)]);
        let (stream_id, end_stream, _) = server.read_request();
        assert!(end_stream);
        server.send_response(stream_id, 200, &[], &chunks, false);

        let mut connection_increment = 0_u32;
        let mut stream_increment = 0_u32;
        let mut updates = 0;
        while updates < 5 {
            let frame = server.read_frame();
            if let FrameKind::WindowUpdate { increment } = frame.kind() {
                if frame.header().stream_id().is_connection() {
                    connection_increment += *increment;
                } else {
                    assert_eq!(frame.header().stream_id(), stream_id);
                    stream_increment += *increment;
                }
                updates += 1;
            }
        }
        assert_eq!(
            connection_increment,
            u32::try_from(body.len()).expect("body length fits u32"),
            "connection window is replenished for every DATA frame"
        );
        assert_eq!(
            stream_increment,
            u32::try_from(chunks[0].len() + chunks[1].len()).expect("chunk sum"),
            "stream window is replenished for non-terminal DATA frames"
        );
    });

    let mut client = connect(address);
    let stream_id = client
        .send_request(&H2Request::get("flow.test", "/"))
        .expect("send GET");
    let response = client.read_response(stream_id).expect("read response");

    assert_eq!(response.status, 200);
    assert_eq!(response.body, body);
    assert_eq!(client.stats().window_updates_sent, 5);
    server.join().expect("server thread");
}

#[test]
fn loopback_rst_stream_is_a_typed_stream_error() {
    let (address, server) = spawn_server(|stream| {
        let mut server = TestServer::new(stream);
        server.handshake(&[]);
        let (stream_id, _, _) = server.read_request();
        server.write_frame(&Frame::rst_stream(stream_id, ErrorCode::Cancel));
    });

    let mut client = connect(address);
    let stream_id = client
        .send_request(&H2Request::get("reset.test", "/"))
        .expect("send GET");
    let error = client
        .read_response(stream_id)
        .expect_err("RST_STREAM must fail the response");
    assert_eq!(
        error,
        H2Error::StreamReset {
            stream_id,
            code: ErrorCode::Cancel,
        }
    );
    assert!(
        !client.is_closed(),
        "a stream reset does not close the connection"
    );
    server.join().expect("server thread");
}

#[test]
fn loopback_goaway_is_a_typed_connection_error() {
    let (address, server) = spawn_server(|stream| {
        let mut server = TestServer::new(stream);
        server.handshake(&[]);
        let (stream_id, _, _) = server.read_request();
        server.write_frame(&Frame::goaway(
            stream_id,
            ErrorCode::ProtocolError,
            b"stop".to_vec(),
        ));
    });

    let mut client = connect(address);
    let stream_id = client
        .send_request(&H2Request::get("goaway.test", "/"))
        .expect("send GET");
    let error = client
        .read_response(stream_id)
        .expect_err("GOAWAY must fail the response");
    match error {
        H2Error::GoAway {
            last_stream_id,
            code,
            debug_data,
        } => {
            assert_eq!(last_stream_id, StreamId::new(1));
            assert_eq!(code, ErrorCode::ProtocolError);
            assert_eq!(debug_data, b"stop");
        }
        other => panic!("expected GOAWAY, got {other:?}"),
    }
    assert!(client.is_closed());
    server.join().expect("server thread");
}

#[test]
fn malformed_frame_is_rejected_and_reported_with_goaway() {
    let (address, server) = spawn_server(|stream| {
        let mut server = TestServer::new(stream);
        server.handshake(&[]);
        let (_, _, _) = server.read_request();
        // DATA, PADDED|END_STREAM, stream 1, Pad Length 5 with only one payload
        // byte: padding exceeds the frame and is a connection PROTOCOL_ERROR.
        server
            .stream
            .write_all(&[0, 0, 2, 0x0, 0x9, 0, 0, 0, 1, 5, 0])
            .expect("write malformed DATA");
        server.stream.flush().expect("flush malformed DATA");
        assert_eq!(server.read_goaway(), ErrorCode::ProtocolError);
    });

    let mut client = connect(address);
    let stream_id = client
        .send_request(&H2Request::get("malformed.test", "/"))
        .expect("send GET");
    let error = client
        .read_response(stream_id)
        .expect_err("malformed frame must be rejected");
    assert_eq!(error.code(), ErrorCode::ProtocolError);
    assert!(matches!(error, H2Error::Frame(_)));
    assert!(client.is_closed());
    server.join().expect("server thread");
}

#[test]
fn malformed_hpack_is_rejected_and_reported_with_compression_error() {
    let (address, server) = spawn_server(|stream| {
        let mut server = TestServer::new(stream);
        server.handshake(&[]);
        let (stream_id, _, _) = server.read_request();
        // Indexed header field with index 0 is an HPACK decoding error.
        server.write_frame(&Frame::headers(stream_id, true, true, None, vec![0x80]));
        assert_eq!(server.read_goaway(), ErrorCode::CompressionError);
    });

    let mut client = connect(address);
    let stream_id = client
        .send_request(&H2Request::get("hpack.test", "/"))
        .expect("send GET");
    let error = client
        .read_response(stream_id)
        .expect_err("HPACK error must fail the stream");
    assert_eq!(error.code(), ErrorCode::CompressionError);
    assert!(matches!(error, H2Error::Hpack(HpackError::IndexZero)));
    assert!(client.is_closed());
    server.join().expect("server thread");
}

#[test]
fn server_push_is_rejected_as_an_unsupported_feature() {
    let (address, server) = spawn_server(|stream| {
        let mut server = TestServer::new(stream);
        server.handshake(&[]);
        let (stream_id, _, _) = server.read_request();
        server.write_frame(&Frame::push_promise(
            stream_id,
            StreamId::new(2),
            true,
            Vec::new(),
        ));
        assert_eq!(server.read_goaway(), ErrorCode::ProtocolError);
    });

    let mut client = connect(address);
    let stream_id = client
        .send_request(&H2Request::get("push.test", "/"))
        .expect("send GET");
    let error = client
        .read_response(stream_id)
        .expect_err("PUSH_PROMISE must be rejected");
    assert_eq!(error, H2Error::Unsupported(UnsupportedFeature::ServerPush));
    assert!(client.is_closed());
    server.join().expect("server thread");
}

#[test]
fn multiplexes_two_requests_with_out_of_order_responses() {
    let (address, server) = spawn_server(|stream| {
        let mut server = TestServer::new(stream);
        server.handshake(&[Setting::new(SettingId::MaxConcurrentStreams, 10)]);
        let (first, _, first_fields) = server.read_request();
        let (second, _, second_fields) = server.read_request();
        assert_eq!(first, StreamId::new(1));
        assert_eq!(second, StreamId::new(3));
        assert_eq!(field(&first_fields, ":path"), Some(&b"/one"[..]));
        assert_eq!(field(&second_fields, ":path"), Some(&b"/two"[..]));

        // Complete the later stream first, then the earlier stream.
        server.send_response(second, 201, &[], &[b"second".as_slice()], true);
        server.send_response(first, 202, &[], &[b"first".as_slice()], true);
        // One connection WINDOW_UPDATE per terminal DATA frame.
        server.drain_window_updates(2);
    });

    let mut client = connect(address);
    let first = client
        .send_request(&H2Request::get("one.test", "/one"))
        .expect("send first");
    let second = client
        .send_request(&H2Request::get("two.test", "/two"))
        .expect("send second");
    assert_eq!(first, StreamId::new(1));
    assert_eq!(second, StreamId::new(3));

    let first_response = client.read_response(first).expect("first response");
    assert_eq!(first_response.status, 202);
    assert_eq!(first_response.body, b"first");

    let second_response = client.read_response(second).expect("second response");
    assert_eq!(second_response.status, 201);
    assert_eq!(second_response.body, b"second");

    server.join().expect("server thread");
}

#[test]
fn request_body_waits_for_stream_window_updates() {
    let body = b"twelve bytes";
    let (address, server) = spawn_server(move |stream| {
        let mut server = TestServer::new(stream);
        // Advertise a deliberately tiny stream window (5 octets).
        server.handshake(&[Setting::new(SettingId::InitialWindowSize, 5)]);
        let (stream_id, end_stream, fields) = server.read_request();
        assert!(!end_stream);
        assert_eq!(field(&fields, ":method"), Some(&b"POST"[..]));
        assert_eq!(
            field(&fields, "content-length"),
            Some(body.len().to_string().as_bytes())
        );
        let (received, updates) = server.read_request_body();
        assert_eq!(received, body);
        assert_eq!(updates, vec![5, 5]);
        server.send_response(stream_id, 204, &[], &[], false);
    });

    let mut client = connect(address);
    let request = H2Request::new("POST", "http", "upload.test", "/upload").with_body(body.to_vec());
    let stream_id = client.send_request(&request).expect("send POST");
    let response = client.read_response(stream_id).expect("read 204");
    assert_eq!(response.status, 204);
    assert!(response.body.is_empty());
    server.join().expect("server thread");
}

#[test]
fn rejects_invalid_request_fields_before_writing() {
    let error = H2Connection::connect(std::io::Cursor::new(Vec::new()), H2Config::default())
        .err()
        .expect("handshake cannot complete on an empty stream");
    assert_eq!(error, H2Error::ConnectionClosed);
    let error =
        validate_request(&H2Request::get("bad.test", "/").with_header("X-Uppercase", "value"))
            .expect_err("uppercase field name");
    assert!(matches!(error, H2Error::InvalidRequest(_)));

    let error =
        validate_request(&H2Request::get("bad.test", "/").with_header("connection", "close"))
            .expect_err("connection-specific header");
    assert!(matches!(error, H2Error::InvalidRequest(_)));

    let error = validate_request(&H2Request::new("GET", "ftp", "bad.test", "/"))
        .expect_err("unsupported scheme");
    assert!(matches!(error, H2Error::InvalidRequest(_)));

    let error = validate_request(&H2Request::new("CONNECT", "http", "bad.test", "/"))
        .expect_err("CONNECT is not modelled");
    assert_eq!(error, H2Error::Unsupported(UnsupportedFeature::Connect));
}
