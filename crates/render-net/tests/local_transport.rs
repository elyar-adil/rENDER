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
    FixedOriginLimit, HttpTransport, NetworkWorker, Url,
};

#[derive(Clone, Debug)]
struct WireResponse {
    status: &'static str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    delay: Duration,
}

impl WireResponse {
    fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: "200 OK",
            headers: Vec::new(),
            body: body.into(),
            delay: Duration::ZERO,
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
    stream.write_all(&response.body).expect("write body");
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
fn rejects_non_http_schemes_before_transport() {
    let url = Url::parse("file:///tmp/index.html").expect("file URL");
    let error = transport(|_| {})
        .fetch(&FetchRequest::get(url), &CancelToken::default())
        .expect_err("scheme must be rejected");
    assert_eq!(error, FetchError::UnsupportedScheme("file".into()));
}
