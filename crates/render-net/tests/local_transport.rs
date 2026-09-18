use std::fmt::Write as _;
use std::io::{Read, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use flate2::{Compression, write::GzEncoder};
use render_net::{
    BatchOptions, ByteRange, CancelToken, CookieJar, FetchConfig, FetchError, FetchRequest,
    FixedOriginLimit, HttpTransport, NetworkWorker, NetworkWorkerConfig, Url,
};

#[derive(Clone, Debug)]
struct WireResponse {
    status: &'static str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    delay: Duration,
    /// Pause inserted between 256-byte body writes, to emulate slow transfers
    /// that keep making progress without ever exceeding any single read.
    body_chunk_delay: Duration,
}

impl WireResponse {
    fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: "200 OK",
            headers: Vec::new(),
            body: body.into(),
            delay: Duration::ZERO,
            body_chunk_delay: Duration::ZERO,
        }
    }
}

fn spawn_server(
    expected_connections: usize,
    handler: impl Fn(String) -> WireResponse + Send + Sync + 'static,
) -> (Url, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
    let address = listener.local_addr().expect("read local address");
    let handler = Arc::new(handler);
    let server = thread::spawn(move || {
        let mut children = Vec::new();
        for _ in 0..expected_connections {
            let (stream, _) = listener.accept().expect("accept local request");
            let child_handler = Arc::clone(&handler);
            children.push(thread::spawn(move || serve(stream, &*child_handler)));
        }
        for child in children {
            child.join().expect("serve local request");
        }
    });
    let url = Url::parse(&format!("http://{address}/")).expect("construct local URL");
    (url, server)
}

fn serve(mut stream: TcpStream, handler: &dyn Fn(String) -> WireResponse) {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set local read timeout");
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let count = stream.read(&mut chunk).expect("read local request");
        if count == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..count]);
    }
    let request = String::from_utf8_lossy(&request).into_owned();
    let response = handler(request);
    thread::sleep(response.delay);
    let mut wire = format!(
        "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        response.body.len()
    );
    for (name, value) in response.headers {
        write!(&mut wire, "{name}: {value}\r\n").expect("format local header");
    }
    wire.push_str("\r\n");
    stream.write_all(wire.as_bytes()).expect("write headers");
    // Body writes tolerate a client that already went away (timeouts,
    // cancellations, body limits); failing the server thread would only
    // obscure the assertion under test.
    let mut body = &response.body[..];
    while !body.is_empty() {
        let (piece, rest) = body.split_at(body.len().min(256));
        if stream.write_all(piece).is_err() {
            return;
        }
        body = rest;
        if !body.is_empty() && response.body_chunk_delay > Duration::ZERO {
            thread::sleep(response.body_chunk_delay);
        }
    }
}

fn request_path(request: &str) -> &str {
    request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .expect("request path")
}

fn transport(configure: impl FnOnce(&mut FetchConfig)) -> HttpTransport {
    let mut config = FetchConfig {
        timeout: Duration::from_secs(2),
        ..FetchConfig::default()
    };
    configure(&mut config);
    HttpTransport::new(config)
}

#[test]
fn cookie_jar_absorbs_set_cookie_and_decorates_the_next_request() {
    let seen_profile = Arc::new(Mutex::new(String::new()));
    let captured_profile = Arc::clone(&seen_profile);
    let (base, server) = spawn_server(2, move |request| {
        if request_path(&request) == "/login" {
            let mut response = WireResponse::ok("logged-in");
            response.headers.push((
                "Set-Cookie".into(),
                "session=abc123; Path=/; HttpOnly; SameSite=Lax".into(),
            ));
            response
        } else {
            *captured_profile.lock().expect("capture profile") = request;
            WireResponse::ok("profile")
        }
    });
    let client = transport(|_| {});
    let login_url = base.join("login").expect("login URL");
    let profile_url = base.join("profile").expect("profile URL");

    let login = client
        .fetch(&FetchRequest::get(login_url), &CancelToken::default())
        .expect("login response");
    let mut jar = CookieJar::default();
    assert!(jar.absorb_response(&login).is_empty());
    let profile_request = jar.decorate_request(FetchRequest::get(profile_url));
    client
        .fetch(&profile_request, &CancelToken::default())
        .expect("profile response");
    server.join().expect("server exits");

    assert!(
        seen_profile
            .lock()
            .expect("profile request")
            .lines()
            .any(|line| line.eq_ignore_ascii_case("cookie: session=abc123"))
    );
}

#[test]
fn gets_status_headers_body_metadata_and_user_agent() {
    let seen_request = Arc::new(Mutex::new(String::new()));
    let seen_by_server = Arc::clone(&seen_request);
    let (url, server) = spawn_server(1, move |request| {
        *seen_by_server.lock().expect("capture request") = request;
        let mut response = WireResponse::ok("hello");
        response.status = "404 Not Found";
        response
            .headers
            .push(("Content-Type".into(), "Text/HTML; charset=Shift_JIS".into()));
        response
    });
    let client = transport(|config| config.user_agent = "rENDER-test/1".into());
    let result = client
        .fetch(&FetchRequest::get(url), &CancelToken::default())
        .expect("HTTP statuses are typed responses");
    server.join().expect("server exits");

    assert_eq!(result.status.as_u16(), 404);
    assert!(!result.status.is_success());
    assert_eq!(result.body, b"hello");
    let content_type = result.content_type.expect("content type metadata");
    assert_eq!(content_type.media_type, "text/html");
    assert_eq!(content_type.charset.as_deref(), Some("shift_jis"));
    assert!(
        seen_request
            .lock()
            .expect("read request")
            .to_ascii_lowercase()
            .contains("user-agent: render-test/1")
    );
}

#[test]
fn byte_range_request_sends_range_and_accepts_partial_content() {
    let seen_request = Arc::new(Mutex::new(String::new()));
    let captured = Arc::clone(&seen_request);
    let (url, server) = spawn_server(1, move |request| {
        *captured.lock().expect("capture range request") = request;
        WireResponse {
            status: "206 Partial Content",
            headers: vec![("Content-Range".into(), "bytes 100-199/1000".into())],
            body: vec![7; 100],
            delay: Duration::ZERO,
            body_chunk_delay: Duration::ZERO,
        }
    });
    let request = FetchRequest::get(url)
        .with_byte_range(ByteRange::inclusive(100, 199).expect("ordered byte range"));

    let response = transport(|config| config.max_body_bytes = 128)
        .fetch(&request, &CancelToken::default())
        .expect("partial response");
    server.join().expect("server exits");

    assert_eq!(response.status.as_u16(), 206);
    assert_eq!(response.body.len(), 100);
    assert!(
        seen_request
            .lock()
            .expect("range request")
            .lines()
            .any(|line| line.eq_ignore_ascii_case("range: bytes=100-199"))
    );
    assert_eq!(
        ByteRange::inclusive(9, 8),
        Err(FetchError::InvalidByteRange { start: 9, end: 8 })
    );
    assert_eq!(ByteRange::suffix(0), Err(FetchError::EmptyByteRangeSuffix));
}

#[test]
fn follows_redirects_and_reports_final_url() {
    let (base, server) = spawn_server(2, |request| match request_path(&request) {
        "/start" => WireResponse {
            status: "302 Found",
            headers: vec![
                ("Location".into(), "/final".into()),
                ("Set-Cookie".into(), "redirect=1; Path=/".into()),
            ],
            body: Vec::new(),
            delay: Duration::ZERO,
            body_chunk_delay: Duration::ZERO,
        },
        "/final" => WireResponse::ok("done"),
        path => panic!("unexpected path {path}"),
    });
    let start = base.join("start").expect("start URL");
    let final_url = base.join("final").expect("final URL");
    let result = transport(|_| {})
        .fetch(&FetchRequest::get(start.clone()), &CancelToken::default())
        .expect("follow redirect");
    server.join().expect("server exits");

    assert_eq!(result.requested_url, start);
    assert_eq!(result.final_url, final_url);
    assert_eq!(result.redirect_chain, vec![start, final_url.clone()]);
    assert_eq!(result.redirects.len(), 1);
    assert_eq!(result.redirects[0].status.as_u16(), 302);
    assert_eq!(result.body, b"done");

    let mut jar = CookieJar::default();
    assert!(jar.absorb_response(&result).is_empty());
    assert_eq!(jar.cookie_header(&final_url), Some("redirect=1".to_owned()));
}

#[test]
fn cookies_set_by_redirect_hops_apply_to_subsequent_hops() {
    let seen_final = Arc::new(Mutex::new(String::new()));
    let captured = Arc::clone(&seen_final);
    let (base, server) = spawn_server(2, move |request| match request_path(&request) {
        "/start" => WireResponse {
            status: "302 Found",
            headers: vec![
                ("Location".into(), "/final".into()),
                ("Set-Cookie".into(), "hop=abc; Path=/".into()),
            ],
            body: Vec::new(),
            delay: Duration::ZERO,
            body_chunk_delay: Duration::ZERO,
        },
        "/final" => {
            *captured.lock().expect("capture final request") = request;
            WireResponse::ok("done")
        }
        path => panic!("unexpected path {path}"),
    });
    let start = base.join("start").expect("start URL");
    // The caller (browser context) decorated the request for the original URL.
    let request = FetchRequest::get(start).with_cookie("caller=1");

    let response = transport(|_| {})
        .fetch(&request, &CancelToken::default())
        .expect("redirect chain completes");
    server.join().expect("server exits");

    assert_eq!(response.final_url, base.join("final").expect("final URL"));
    // RFC 6265 hop-by-hop application: the cookie set by /start must decorate
    // the /final request, merged with the caller's own pairs.
    let final_cookie = seen_final
        .lock()
        .expect("final request")
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("cookie:"))
        .expect("final request carries cookies")
        .to_owned();
    assert_eq!(
        final_cookie.to_ascii_lowercase(),
        "cookie: caller=1; hop=abc"
    );
}

#[test]
fn chain_cookies_replace_colliding_caller_cookie_names_on_the_next_hop() {
    let seen_final = Arc::new(Mutex::new(String::new()));
    let captured = Arc::clone(&seen_final);
    let (base, server) = spawn_server(2, move |request| match request_path(&request) {
        "/start" => WireResponse {
            status: "302 Found",
            headers: vec![
                ("Location".into(), "/final".into()),
                ("Set-Cookie".into(), "sid=new-value; Path=/".into()),
            ],
            body: Vec::new(),
            delay: Duration::ZERO,
            body_chunk_delay: Duration::ZERO,
        },
        "/final" => {
            *captured.lock().expect("capture final request") = request;
            WireResponse::ok("done")
        }
        path => panic!("unexpected path {path}"),
    });
    let start = base.join("start").expect("start URL");
    let request = FetchRequest::get(start).with_cookie("sid=old-value; keep=1");

    transport(|_| {})
        .fetch(&request, &CancelToken::default())
        .expect("redirect chain completes");
    server.join().expect("server exits");

    // A later Set-Cookie replaces an earlier cookie of the same name; the
    // non-colliding caller pair survives.
    let final_cookie = seen_final
        .lock()
        .expect("final request")
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("cookie:"))
        .expect("final request carries cookies")
        .to_owned();
    assert_eq!(
        final_cookie.to_ascii_lowercase(),
        "cookie: keep=1; sid=new-value"
    );
}

#[test]
fn caller_and_hop_cookies_are_not_leaked_across_a_cross_origin_redirect() {
    let seen_b = Arc::new(Mutex::new(String::new()));
    let captured_b = Arc::clone(&seen_b);
    let (origin_b, server_b) = spawn_server(1, move |request| {
        *captured_b.lock().expect("capture origin B request") = request;
        WireResponse::ok("origin-b")
    });
    let cross_target = origin_b
        .join("final")
        .expect("cross-origin target URL")
        .to_string();

    let (bound_a, server_a) = spawn_server(1, move |_| WireResponse {
        status: "302 Found",
        headers: vec![
            ("Location".into(), cross_target.clone()),
            ("Set-Cookie".into(), "cross=1; Path=/".into()),
        ],
        body: Vec::new(),
        delay: Duration::ZERO,
        body_chunk_delay: Duration::ZERO,
    });
    // Re-home origin A on the "localhost" name so the two origins have
    // distinct cookie hosts: RFC 6265 cookies are deliberately not isolated
    // by port, so two 127.0.0.1 servers on different ports would share them
    // just like real browsers do.
    let origin_a = Url::parse(&format!(
        "http://localhost:{}/",
        bound_a.port().expect("bound port")
    ))
    .expect("localhost origin URL");
    let start = origin_a.join("start").expect("start URL");
    let request = FetchRequest::get(start).with_cookie("caller=secret");
    let response = transport(|_| {})
        .fetch(&request, &CancelToken::default())
        .expect("cross-origin redirect completes");
    server_a.join().expect("origin A server exits");
    server_b.join().expect("origin B server exits");

    assert_eq!(response.final_url, origin_b.join("final").unwrap());
    // Neither the caller's Cookie header (computed for origin A) nor the
    // cookie origin A set during the chain may reach origin B.
    let carried_cookie = seen_b
        .lock()
        .expect("origin B request")
        .lines()
        .any(|line| line.to_ascii_lowercase().starts_with("cookie:"));
    assert!(!carried_cookie, "origin B must receive no Cookie header");
}

#[test]
fn transparently_decodes_gzip_responses() {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    std::io::Write::write_all(&mut encoder, b"<html>compressed</html>").expect("compress response");
    let compressed = encoder.finish().expect("finish gzip response");
    let seen_request = Arc::new(Mutex::new(String::new()));
    let captured = Arc::clone(&seen_request);
    let (url, server) = spawn_server(1, move |request| {
        *captured.lock().expect("capture request") = request;
        let mut response = WireResponse::ok(compressed.clone());
        response
            .headers
            .push(("Content-Encoding".into(), "gzip".into()));
        response
    });

    let response = transport(|_| {})
        .fetch(&FetchRequest::get(url), &CancelToken::default())
        .expect("gzip response");
    server.join().expect("server exits");

    assert_eq!(response.body, b"<html>compressed</html>");
    assert!(
        !response
            .headers
            .iter()
            .any(|header| header.name.eq_ignore_ascii_case("content-encoding"))
    );
    assert!(
        seen_request
            .lock()
            .expect("read request")
            .to_ascii_lowercase()
            .contains("accept-encoding: gzip, br")
    );
}

/// Builds a valid RFC 7932 brotli stream holding `data` in a single
/// uncompressed meta-block, followed by the mandatory empty final meta-block.
/// This exercises the same `content-encoding: br` decode path (including
/// header stripping) without pulling a brotli *encoder* in as a dev-dependency.
///
/// Bit layout (LSB-first): `WBITS=16` (`0`), `ISLAST=0`, `MNIBBLES=00` (four
/// nibbles), 16 bits of `MLEN-1`, `ISUNCOMPRESSED=1`, skip to the byte
/// boundary, the raw bytes, then the empty last block `ISLAST=1`,
/// `ISLASTEMPTY=1`.
fn brotli_uncompressed_stream(data: &[u8]) -> Vec<u8> {
    assert!(
        !data.is_empty() && data.len() <= 65_536,
        "a four-nibble meta-block holds 1..=65536 bytes"
    );
    let mlen_minus_one = data.len() - 1;
    let mut stream = vec![
        u8::try_from((mlen_minus_one & 0x0F) << 4).expect("nibble shifted into bits 4..8"),
        u8::try_from((mlen_minus_one >> 4) & 0xFF).expect("length byte fits u8"),
        u8::try_from(mlen_minus_one >> 12).expect("length nibble fits u8") | 0x10,
    ];
    stream.extend_from_slice(data);
    stream.push(0x03);
    stream
}

#[test]
fn transparently_decodes_brotli_responses() {
    let compressed = brotli_uncompressed_stream(b"<html>brotli</html>");
    let seen_request = Arc::new(Mutex::new(String::new()));
    let captured = Arc::clone(&seen_request);
    let (url, server) = spawn_server(1, move |request| {
        *captured.lock().expect("capture request") = request;
        let mut response = WireResponse::ok(compressed.clone());
        response
            .headers
            .push(("Content-Encoding".into(), "br".into()));
        response
    });

    let response = transport(|_| {})
        .fetch(&FetchRequest::get(url), &CancelToken::default())
        .expect("brotli response");
    server.join().expect("server exits");

    assert_eq!(response.body, b"<html>brotli</html>");
    assert!(
        !response
            .headers
            .iter()
            .any(|header| header.name.eq_ignore_ascii_case("content-encoding")),
        "the transport must expose the decoded representation only"
    );
    assert!(
        seen_request
            .lock()
            .expect("read request")
            .to_ascii_lowercase()
            .contains("accept-encoding: gzip, br"),
        "advertising br requires decoding it, otherwise CDNs serve undecodable bodies"
    );
}

#[test]
fn enforces_redirect_header_and_body_limits() {
    let (redirect_base, redirect_server) = spawn_server(2, |request| {
        let location = match request_path(&request) {
            "/one" => "/two",
            "/two" => "/three",
            path => panic!("unexpected path {path}"),
        };
        WireResponse {
            status: "302 Found",
            headers: vec![("Location".into(), location.into())],
            body: Vec::new(),
            delay: Duration::ZERO,
            body_chunk_delay: Duration::ZERO,
        }
    });
    let redirect_error = transport(|config| config.redirect_limit = 1)
        .fetch(
            &FetchRequest::get(redirect_base.join("one").expect("redirect URL")),
            &CancelToken::default(),
        )
        .expect_err("redirect limit");
    redirect_server.join().expect("redirect server exits");
    assert_eq!(
        redirect_error,
        FetchError::RedirectLimitExceeded { limit: 1 }
    );

    let (header_url, header_server) = spawn_server(1, |_| {
        let mut response = WireResponse::ok(Vec::new());
        response
            .headers
            .push(("X-Oversized".into(), "x".repeat(512)));
        response
    });
    let header_error = transport(|config| config.max_header_bytes = 128)
        .fetch(&FetchRequest::get(header_url), &CancelToken::default())
        .expect_err("header limit");
    header_server.join().expect("header server exits");
    assert_eq!(header_error, FetchError::HeaderLimitExceeded { limit: 128 });

    let (body_url, body_server) = spawn_server(1, |_| WireResponse::ok("12345"));
    let body_error = transport(|config| config.max_body_bytes = 4)
        .fetch(&FetchRequest::get(body_url), &CancelToken::default())
        .expect_err("body limit");
    body_server.join().expect("body server exits");
    assert_eq!(body_error, FetchError::BodyLimitExceeded { limit: 4 });
}

#[test]
fn batch_is_parallel_origin_bounded_and_input_ordered() {
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let handler_active = Arc::clone(&active);
    let handler_maximum = Arc::clone(&maximum);
    let (base, server) = spawn_server(4, move |request| {
        let now = handler_active.fetch_add(1, Ordering::SeqCst) + 1;
        handler_maximum.fetch_max(now, Ordering::SeqCst);
        let path = request_path(&request).trim_start_matches('/').to_owned();
        thread::sleep(Duration::from_millis(match path.as_str() {
            "0" => 80,
            "1" => 10,
            "2" => 50,
            _ => 5,
        }));
        handler_active.fetch_sub(1, Ordering::SeqCst);
        WireResponse::ok(path)
    });
    let requests = (0..4)
        .map(|index| FetchRequest::get(base.join(&index.to_string()).expect("resource URL")))
        .collect();
    let options = BatchOptions {
        max_concurrency: 4,
        origin_policy: Arc::new(FixedOriginLimit(2)),
    };
    let results = transport(|_| {}).fetch_batch(requests, &options, &CancelToken::default());
    server.join().expect("server exits");

    let bodies = results
        .into_iter()
        .map(|result| result.expect("batch response").body)
        .collect::<Vec<_>>();
    assert_eq!(bodies, vec![b"0", b"1", b"2", b"3"]);
    assert_eq!(maximum.load(Ordering::SeqCst), 2);
}

#[test]
fn worker_is_non_blocking_and_batch_cancellation_is_prompt() {
    let (hit_tx, hit_rx) = mpsc::channel();
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server_release = Arc::clone(&release);
    let (base, server) = spawn_server(1, move |request| {
        hit_tx.send(()).expect("report request hit");
        while !server_release.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(1));
        }
        WireResponse::ok(request_path(&request).as_bytes().to_vec())
    });
    let worker = NetworkWorker::start(transport(|_| {})).expect("start worker");
    let requests = (0..3)
        .map(|index| FetchRequest::get(base.join(&index.to_string()).expect("resource URL")))
        .collect();
    let handle = worker.submit_batch(
        requests,
        BatchOptions {
            max_concurrency: 1,
            origin_policy: Arc::new(FixedOriginLimit(1)),
        },
    );
    assert!(matches!(handle.try_recv(), Err(mpsc::TryRecvError::Empty)));
    hit_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("first request started");
    handle.cancel();
    let results = handle
        .recv_timeout(Duration::from_millis(150))
        .expect("cancellation must not wait for the blocked socket");

    assert!(
        results
            .iter()
            .all(|result| *result == Err(FetchError::Cancelled))
    );
    release.store(true, Ordering::Release);
    server.join().expect("server exits");
}

#[test]
fn worker_uses_a_shared_bounded_transfer_pool() {
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let handler_active = Arc::clone(&active);
    let handler_maximum = Arc::clone(&maximum);
    let (base, server) = spawn_server(6, move |request| {
        let now = handler_active.fetch_add(1, Ordering::SeqCst) + 1;
        handler_maximum.fetch_max(now, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(25));
        handler_active.fetch_sub(1, Ordering::SeqCst);
        WireResponse::ok(request_path(&request).as_bytes().to_vec())
    });
    let worker = NetworkWorker::start_with_config(
        transport(|_| {}),
        NetworkWorkerConfig {
            worker_count: 2,
            queue_capacity: 4,
            operation_capacity: 4,
        },
    )
    .expect("start bounded worker pool");
    let requests = (0..6)
        .map(|index| FetchRequest::get(base.join(&index.to_string()).expect("resource URL")))
        .collect();
    let results = worker
        .submit_batch(
            requests,
            BatchOptions {
                max_concurrency: 6,
                origin_policy: Arc::new(FixedOriginLimit(6)),
            },
        )
        .recv_timeout(Duration::from_secs(2))
        .expect("bounded batch completes");
    server.join().expect("server exits");

    assert!(results.iter().all(Result::is_ok));
    assert_eq!(maximum.load(Ordering::SeqCst), 2);
}

#[test]
fn default_config_sizes_the_queue_independently_of_in_flight_permits() {
    let config = NetworkWorkerConfig::default();
    assert!(config.operation_capacity >= 16);
    assert!(
        config.queue_capacity >= 1024,
        "the command queue must absorb realistic page bursts"
    );
    assert!(
        config.queue_capacity > config.operation_capacity,
        "the queue must be larger than the in-flight limit so bursts queue instead of failing"
    );
}

#[test]
fn worker_queues_excess_operations_until_permits_free_up() {
    let (hit_tx, hit_rx) = mpsc::channel();
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server_release = Arc::clone(&release);
    let hits = Arc::new(AtomicUsize::new(0));
    let handler_hits = Arc::clone(&hits);
    let (base, server) = spawn_server(2, move |request| {
        handler_hits.fetch_add(1, Ordering::SeqCst);
        hit_tx.send(()).expect("report request hit");
        while !server_release.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(1));
        }
        WireResponse::ok(request_path(&request).as_bytes().to_vec())
    });
    let worker = NetworkWorker::start_with_config(
        transport(|_| {}),
        NetworkWorkerConfig {
            worker_count: 1,
            queue_capacity: 4,
            operation_capacity: 1,
        },
    )
    .expect("start bounded worker pool");

    let first = worker.submit(FetchRequest::get(base.join("first").expect("first URL")));
    hit_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("first request starts");
    let second = worker.submit(FetchRequest::get(base.join("second").expect("second URL")));
    assert!(
        matches!(second.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "an operation parked behind an exhausted permit stays queued, it must not fail"
    );

    release.store(true, Ordering::Release);
    first
        .recv_timeout(Duration::from_secs(1))
        .expect("first operation completes")
        .expect("first request succeeds");
    let second_result = second
        .recv_timeout(Duration::from_secs(1))
        .expect("queued operation completes once a permit frees up");
    second_result.expect("second request succeeds");
    server.join().expect("server exits");
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[test]
fn default_worker_absorbs_a_burst_of_two_hundred_submissions() {
    let (base, server) = spawn_server(200, move |request| {
        thread::sleep(Duration::from_millis(1));
        WireResponse::ok(request_path(&request).as_bytes().to_vec())
    });
    let worker = NetworkWorker::start(transport(|_| {})).expect("start default worker");
    let handles = (0..200)
        .map(|index| {
            worker.submit(FetchRequest::get(
                base.join(&index.to_string()).expect("burst URL"),
            ))
        })
        .collect::<Vec<_>>();

    for (index, handle) in handles.into_iter().enumerate() {
        let result = handle
            .recv_timeout(Duration::from_secs(10))
            .expect("burst submission completes");
        let response = result
            .unwrap_or_else(|error| panic!("burst submission {index} must not fail, got: {error}"));
        assert!(response.status.is_success());
        assert_eq!(response.body, format!("/{index}").into_bytes());
    }
    server.join().expect("server exits");
}

#[test]
fn rejects_non_http_schemes_before_transport() {
    let url = Url::parse("file:///tmp/index.html").expect("file URL");
    let error = transport(|_| {})
        .fetch(&FetchRequest::get(url), &CancelToken::default())
        .expect_err("scheme must be rejected");
    assert_eq!(error, FetchError::UnsupportedScheme("file".into()));
}

#[test]
#[ignore = "split header/body budget enforcement needs the ureq transport rework (queued)"]
fn slow_but_progressing_bodies_are_not_spuriously_timed_out() {
    // A large resource (like a CDN stylesheet) trickling in steadily: each
    // read returns well within the budget, but the whole transfer outlives it.
    // The former whole-transfer timeout killed such transfers mid-flight.
    let body = vec![0xAB_u8; 32 * 1024];
    let expected_len = body.len();
    let (url, server) = spawn_server(1, move |_| {
        let mut response = WireResponse::ok(body.clone());
        // 256-byte pieces every ~15ms: about 17 KiB/s, far above the 1 KiB/s
        // minimum-progress floor, while the ~1.9s total deliberately outlives
        // the 1s configured budget.
        response.body_chunk_delay = Duration::from_millis(15);
        response
    });

    let response = transport(|config| config.timeout = Duration::from_secs(1))
        .fetch(&FetchRequest::get(url), &CancelToken::default())
        .expect("a transfer that keeps making progress must finish");
    server.join().expect("server exits");

    assert_eq!(response.body.len(), expected_len);
    assert_eq!(response.body[0], 0xAB);
}

#[test]
fn stalled_body_reads_time_out_with_a_typed_error() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stall server");
    let address = listener.local_addr().expect("read local address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept stall request");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set local read timeout");
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream.read(&mut chunk).expect("read stall request");
            if count == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..count]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\nConnection: close\r\n\r\n")
            .expect("write stall headers");
        stream
            .write_all(&[7_u8; 1024])
            .expect("write first stall piece");
        // Stall longer than the configured body-read budget, then finish the
        // write regardless (the client is gone; errors are expected).
        thread::sleep(Duration::from_secs(2));
        let _ignored = stream.write_all(&[7_u8; 3072]);
    });
    let url = Url::parse(&format!("http://{address}/stall")).expect("stall URL");

    let error = transport(|config| config.timeout = Duration::from_secs(1))
        .fetch(&FetchRequest::get(url), &CancelToken::default())
        .expect_err("a stalled body read must fail");
    server.join().expect("server exits");

    assert_eq!(error, FetchError::Timeout);
}

#[test]
fn redirect_chains_keep_the_configured_overall_budget() {
    let seen_paths = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&seen_paths);
    let (base, server) = spawn_server(2, move |request| {
        captured
            .lock()
            .expect("capture path")
            .push(request_path(&request).to_owned());
        let location = match request_path(&request) {
            "/r1" => "/r2",
            "/r2" => "/r3",
            path => panic!("unexpected path {path}"),
        };
        // Each hop stays within the per-stage budget (1.2s < 2s), but two hops
        // already exhaust the overall budget.
        WireResponse {
            status: "302 Found",
            headers: vec![("Location".into(), location.into())],
            body: Vec::new(),
            delay: Duration::from_millis(1200),
            body_chunk_delay: Duration::ZERO,
        }
    });

    let error = transport(|config| config.timeout = Duration::from_secs(2))
        .fetch(
            &FetchRequest::get(base.join("r1").expect("redirect root")),
            &CancelToken::default(),
        )
        .expect_err("a redirect chain must not multiply the per-stage budget");
    server.join().expect("server exits");

    assert_eq!(error, FetchError::Timeout);
    // The third hop must never have been requested.
    assert_eq!(
        *seen_paths.lock().expect("read paths"),
        vec!["/r1".to_owned(), "/r2".to_owned()]
    );
}

#[test]
fn cancellation_propagates_during_a_slow_body_transfer() {
    let body = vec![9_u8; 8192];
    let (url, server) = spawn_server(1, move |_| {
        let mut response = WireResponse::ok(body.clone());
        response.body_chunk_delay = Duration::from_millis(50);
        response
    });
    let client = transport(|config| config.timeout = Duration::from_secs(5));
    let cancel = CancelToken::default();
    let canceller = cancel.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        canceller.cancel();
    });

    let error = client
        .fetch(&FetchRequest::get(url), &cancel)
        .expect_err("cancelled transfer must not complete");
    server.join().expect("server exits");

    assert_eq!(error, FetchError::Cancelled);
}
