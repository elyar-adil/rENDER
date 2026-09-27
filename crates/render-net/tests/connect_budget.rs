//! Bounded, observable transfers: a dead address must not eat the request
//! budget, and every request must report a terminal outcome with its phase and
//! elapsed time.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use render_net::{
    BatchOptions, CancelToken, FetchConfig, FetchError, FetchEvent, FetchObserver, FetchPhase,
    FetchRequest, FixedOriginLimit, HttpTransport, NetworkWorker, NullObserver, Url,
};

/// RFC 5737 TEST-NET-1. Reserved for documentation, so no host answers there
/// and a connect attempt to it can only run into its budget.
const DEAD_ADDRESS: &str = "192.0.2.1";

/// Upper bound on how long a silent test server keeps a connection open. It is
/// only reached if a client never gives up, which is the failure these tests
/// are about.
const SILENT_HOLD: Duration = Duration::from_secs(20);

fn transport(config: FetchConfig) -> HttpTransport {
    HttpTransport::with_proxy(config, None)
}

fn quiet(timeout: Duration, connect_timeout: Duration) -> FetchConfig {
    FetchConfig {
        timeout,
        connect_timeout,
        observer: Arc::new(NullObserver),
        ..FetchConfig::default()
    }
}

/// Serves `count` keep-alive-less responses and returns the base URL.
fn spawn_ok_server(count: usize) -> (Url, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
    let address = listener.local_addr().expect("read local address");
    let server = thread::spawn(move || {
        for _ in 0..count {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => request.extend_from_slice(&chunk[..count]),
                }
            }
            let _ignored = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\
                  Connection: close\r\n\r\nhello",
            );
        }
    });
    (
        Url::parse(&format!("http://{address}/")).expect("construct local URL"),
        server,
    )
}

/// Accepts connections, reads the request, and never answers.
///
/// The returned thread is deliberately not joined: a silent server outlives the
/// test that started it by design, and each connection is released as soon as
/// the client goes away (or after `SILENT_HOLD` at the latest).
fn spawn_silent_server() -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind silent test server");
    let address = listener.local_addr().expect("read local address");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let _ignored = stream.set_read_timeout(Some(SILENT_HOLD));
            thread::spawn(move || {
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                // Read the request, then hold the connection open without a
                // response. A closed client or the read timeout ends it.
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(count) => request.extend_from_slice(&chunk[..count]),
                    }
                }
                let mut held = Vec::new();
                while stream.read(&mut held).is_ok() {}
            });
        }
    });
    Url::parse(&format!("http://{address}/")).expect("construct local URL")
}

/// Answers the first request on a keep-alive connection, then reads the second
/// request *on that same socket* and never answers it, so the second request
/// runs on a pooled connection.
///
/// Returns the base URL and the number of connections the server accepted, so a
/// test can prove the second request really did reuse the first socket.
fn spawn_pool_then_stall_server(answering_connections: usize) -> (Url, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind pool-and-stall server");
    let address = listener.local_addr().expect("read local address");
    let accepted = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&accepted);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let connection = counted.fetch_add(1, Ordering::SeqCst) + 1;
            let _ignored = stream.set_read_timeout(Some(SILENT_HOLD));
            thread::spawn(move || {
                let mut requests = 0_usize;
                loop {
                    let mut request = Vec::new();
                    let mut chunk = [0_u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => return,
                            Ok(count) => request.extend_from_slice(&chunk[..count]),
                        }
                    }
                    requests += 1;
                    if connection <= answering_connections && requests == 1 {
                        // Keep the socket open: the connection is only reusable
                        // while the server holds it.
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\
                              Connection: keep-alive\r\n\r\nhello",
                        );
                        continue;
                    }
                    // The request arrived on a reused connection and gets
                    // nothing back: the client must still bound and name it.
                    let mut held = Vec::new();
                    while stream.read(&mut held).is_ok() {}
                    return;
                }
            });
        }
    });
    (
        Url::parse(&format!("http://{address}/")).expect("construct local URL"),
        accepted,
    )
}

#[test]
fn a_dead_address_costs_only_the_connect_budget() {
    let url = Url::parse(&format!("http://{DEAD_ADDRESS}/style.css")).expect("dead URL");
    let started = Instant::now();
    let error = transport(quiet(Duration::from_secs(30), Duration::from_millis(700)))
        .fetch(&FetchRequest::get(url), &CancelToken::default())
        .expect_err("an unreachable address must fail");
    let elapsed = started.elapsed();

    assert_eq!(
        error.phase(),
        Some(FetchPhase::TcpConnect),
        "an address that never answers belongs to the connect phase, got: {error:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "a dead address must not consume the 30s per-request budget, took {elapsed:?}"
    );
    assert!(
        error.to_string().contains("tcp connect"),
        "the error must name its phase, got: {error}"
    );
    assert!(
        matches!(error.into_inner(), FetchError::Timeout | FetchError::Io(_)),
        "a stalled connect is a timeout or a socket failure"
    );
}

#[test]
fn a_live_address_still_connects_under_a_small_connect_budget() {
    let (base, server) = spawn_ok_server(1);
    let response = transport(quiet(Duration::from_secs(10), Duration::from_millis(700)))
        .fetch(
            &FetchRequest::get(base.join("ok").expect("resource URL")),
            &CancelToken::default(),
        )
        .expect("a reachable address must not be affected by the connect budget");
    server.join().expect("server exits");
    assert_eq!(response.body, b"hello");
}

#[test]
fn a_server_that_never_answers_headers_names_the_header_phase_and_elapsed_time() {
    let base = spawn_silent_server();
    let started = Instant::now();
    let error = transport(quiet(
        Duration::from_millis(400),
        Duration::from_millis(400),
    ))
    .fetch(
        &FetchRequest::get(base.join("silent").expect("resource URL")),
        &CancelToken::default(),
    )
    .expect_err("a server that never answers must fail at its budget");
    let elapsed = started.elapsed();

    assert_eq!(error.phase(), Some(FetchPhase::ResponseHeaders));
    let reported = error
        .elapsed()
        .expect("a phase failure reports its elapsed time");
    assert!(
        reported >= Duration::from_millis(300),
        "the reported elapsed time must cover the wait, got {reported:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the request must end near its budget, took {elapsed:?}"
    );
    let message = error.to_string();
    assert!(
        message.contains("response headers") && message.contains("ms"),
        "the error must name its phase and its elapsed milliseconds, got: {message}"
    );
    assert_eq!(error.into_inner(), FetchError::Timeout);
}

/// Records what the transport reported, standing in for a log file.
#[derive(Debug, Default)]
struct Recorder {
    events: Mutex<Vec<String>>,
}

impl Recorder {
    fn lines(&self) -> Vec<String> {
        self.events.lock().expect("recorded events").clone()
    }
}

impl FetchObserver for Recorder {
    fn on_fetch_event(&self, event: &FetchEvent<'_>) {
        let line = match *event {
            FetchEvent::Completed {
                method,
                url,
                status,
                body_bytes,
                elapsed,
            } => format!(
                "ok {} {url} {} {body_bytes} {elapsed:?}",
                method.as_str(),
                status.as_u16()
            ),
            FetchEvent::Failed {
                method,
                url,
                elapsed,
                error,
            } => format!("fail {} {url} {elapsed:?} {error:?}", method.as_str()),
            other => format!("other {other:?}"),
        };
        self.events.lock().expect("recorded events").push(line);
    }
}

#[test]
fn every_request_reports_a_terminal_outcome() {
    let (base, server) = spawn_ok_server(1);
    let recorder = Arc::new(Recorder::default());
    let config = FetchConfig {
        timeout: Duration::from_secs(10),
        connect_timeout: Duration::from_millis(400),
        observer: Arc::clone(&recorder) as Arc<dyn FetchObserver>,
        ..FetchConfig::default()
    };
    let client = transport(config);

    client
        .fetch(
            &FetchRequest::get(base.join("ok").expect("resource URL")),
            &CancelToken::default(),
        )
        .expect("local response");
    let dead = Url::parse(&format!("http://{DEAD_ADDRESS}/late.css")).expect("dead URL");
    let error = client
        .fetch(&FetchRequest::get(dead), &CancelToken::default())
        .expect_err("unreachable address must fail");
    server.join().expect("server exits");

    let lines = recorder.lines();
    assert_eq!(lines.len(), 2, "both requests must be reported: {lines:?}");
    assert!(lines[0].starts_with("ok GET "), "got: {}", lines[0]);
    assert!(lines[0].contains(" 200 5 "), "got: {}", lines[0]);
    assert!(lines[1].starts_with("fail GET "), "got: {}", lines[1]);
    assert!(
        lines[1].contains("TcpConnect"),
        "the reported failure must carry its phase, got: {}",
        lines[1]
    );
    assert_eq!(error.phase(), Some(FetchPhase::TcpConnect));
}

#[test]
fn a_reused_connection_still_gets_a_named_bounded_outcome_per_request() {
    // Reuse must not cost the diagnostics: a request served on a pooled
    // connection has no connect phase of its own, so the interesting case is
    // the second request stalling on a warm socket. It must still end with a
    // named phase and its own elapsed time, and both requests must be reported.
    let (base, accepted) = spawn_pool_then_stall_server(1);
    let recorder = Arc::new(Recorder::default());
    // Generous enough that a loaded machine cannot make the *answered* request
    // fail, and still short enough that the stalling one ends inside the test.
    let budget = Duration::from_millis(1500);
    let config = FetchConfig {
        timeout: budget,
        connect_timeout: budget,
        observer: Arc::clone(&recorder) as Arc<dyn FetchObserver>,
        ..FetchConfig::default()
    };
    let client = transport(config);

    client
        .fetch(
            &FetchRequest::get(base.join("first").expect("first URL")),
            &CancelToken::default(),
        )
        .expect("the first request is answered and pooled");

    let error = client
        .fetch(
            &FetchRequest::get(base.join("second").expect("second URL")),
            &CancelToken::default(),
        )
        .expect_err("a stall on a reused connection must still fail at its budget");

    assert_eq!(
        error.phase(),
        Some(FetchPhase::ResponseHeaders),
        "a reused connection has no connect phase; the origin stopped answering, got: {error}"
    );
    let reported = error
        .elapsed()
        .expect("a phase failure reports its elapsed time");
    assert!(
        reported >= Duration::from_secs(1),
        "the pooled request must report its own wait, got {reported:?}"
    );
    assert_eq!(error.into_inner(), FetchError::Timeout);
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "the stalling request must have run on the pooled connection, not a new one"
    );

    let lines = recorder.lines();
    assert_eq!(
        lines.len(),
        2,
        "both requests must be reported independently: {lines:?}"
    );
    assert!(lines[0].starts_with("ok GET "), "got: {}", lines[0]);
    assert!(lines[1].starts_with("fail GET "), "got: {}", lines[1]);
    assert!(
        lines[1].contains("ResponseHeaders"),
        "the failure must name the phase it stalled in, got: {}",
        lines[1]
    );
}

/// An origin serving one resource per path, all keep-alive, except the path at
/// `stall_path`, which is accepted and never answered. Counts connections.
fn spawn_asset_origin(paths: usize, stall_path: usize) -> (Url, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind asset origin");
    let address = listener.local_addr().expect("read asset origin address");
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&connections);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            counter.fetch_add(1, Ordering::SeqCst);
            let _ignored = stream.set_read_timeout(Some(SILENT_HOLD));
            thread::spawn(move || {
                let mut buffer = Vec::new();
                let mut chunk = [0_u8; 1024];
                loop {
                    while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => return,
                            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
                        }
                    }
                    let request = String::from_utf8_lossy(&buffer).into_owned();
                    buffer.clear();
                    let path = request
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap_or("")
                        .to_owned();
                    let index: usize = path
                        .rsplit('-')
                        .next()
                        .and_then(|value| value.split('.').next())
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(usize::MAX);
                    if index == stall_path {
                        let mut held = Vec::new();
                        while stream.read(&mut held).is_ok() {}
                        return;
                    }
                    let body = b"asset-body-for-this-path-only";
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nContent-Length: {}\r\n\
                         Connection: keep-alive\r\n\r\n",
                        body.len()
                    );
                    if stream.write_all(head.as_bytes()).is_err() || stream.write_all(body).is_err()
                    {
                        return;
                    }
                    let _ = paths;
                }
            });
        }
    });
    (
        Url::parse(&format!("http://{address}/")).expect("construct asset origin URL"),
        connections,
    )
}

#[test]
fn a_forty_request_same_origin_burst_stays_individually_attributable() {
    // Forty concurrent requests against one origin, one of which stalls. The
    // burst is exactly the shape the raised idle ceiling speeds up, so this is
    // the test that the speed-up did not cost per-request attribution: every
    // request must still produce its own terminal outcome, and the one that
    // failed must still name the phase it failed in.
    const ASSETS: usize = 40;
    const STALL: usize = 17;
    let (base, _connections) = spawn_asset_origin(ASSETS, STALL);
    let recorder = Arc::new(Recorder::default());
    // Forty requests at once on a machine shared with other agents needs a
    // budget that scheduling noise cannot exhaust; the stalling asset still
    // ends inside the test because it never answers at all.
    let budget = Duration::from_secs(2);
    let config = FetchConfig {
        timeout: budget,
        connect_timeout: budget,
        observer: Arc::clone(&recorder) as Arc<dyn FetchObserver>,
        ..FetchConfig::default()
    };
    let client = transport(config);
    let requests = (0..ASSETS)
        .map(|index| {
            FetchRequest::get(base.join(&format!("asset-{index}.css")).expect("asset URL"))
        })
        .collect();
    // All forty at once, so the burst really is one wave rather than a queue.
    let options = BatchOptions {
        max_concurrency: ASSETS,
        origin_policy: Arc::new(FixedOriginLimit(ASSETS)),
        ..BatchOptions::default()
    };

    let results = client.fetch_batch(requests, &options, &CancelToken::default());
    assert_eq!(results.len(), ASSETS, "input order is retained");

    let mut failed = Vec::new();
    for (index, result) in results.iter().enumerate() {
        match result {
            Ok(response) => assert_eq!(
                response.body, b"asset-body-for-this-path-only",
                "asset {index} got another request's body"
            ),
            Err(error) => {
                assert_eq!(index, STALL, "only the stalling asset may fail: {error}");
                assert_eq!(
                    error.phase(),
                    Some(FetchPhase::ResponseHeaders),
                    "the stalled asset must name the phase it stalled in: {error}"
                );
                failed.push(index);
            }
        }
    }
    assert_eq!(failed, vec![STALL], "exactly one asset fails");

    let lines = recorder.lines();
    assert_eq!(
        lines.len(),
        ASSETS,
        "every request in the burst must be reported once: {} lines",
        lines.len()
    );
    let mut reported = std::collections::BTreeSet::new();
    for line in &lines {
        let url = line
            .split_whitespace()
            .nth(2)
            .expect("event line carries a URL")
            .to_owned();
        assert!(reported.insert(url.clone()), "duplicate report for {url}");
        if url.ends_with(&format!("asset-{STALL}.css")) {
            assert!(
                line.starts_with("fail ") && line.contains("ResponseHeaders"),
                "the stalled asset must be reported as a named failure: {line}"
            );
        } else {
            assert!(line.starts_with("ok "), "expected a success: {line}");
        }
    }
    assert_eq!(reported.len(), ASSETS, "all forty assets are accounted for");
}

#[test]
fn a_batch_reports_the_requests_that_outlived_its_budget() {
    let base = spawn_silent_server();
    let client = transport(quiet(Duration::from_secs(20), Duration::from_secs(2)));
    let requests = (0..4)
        .map(|index| FetchRequest::get(base.join(&index.to_string()).expect("resource URL")))
        .collect();
    let options = BatchOptions {
        max_concurrency: 2,
        timeout: Duration::from_millis(400),
        ..BatchOptions::default()
    };

    let started = Instant::now();
    let results = client.fetch_batch(requests, &options, &CancelToken::default());
    let elapsed = started.elapsed();

    assert_eq!(results.len(), 4, "input order is retained");
    assert!(
        elapsed < Duration::from_secs(5),
        "the batch must end at its own budget, took {elapsed:?}"
    );
    for result in &results {
        let error = result
            .as_ref()
            .expect_err("a silent server cannot answer")
            .clone();
        assert_eq!(
            error.phase(),
            Some(FetchPhase::Queued),
            "an unfinished request never reached the network, got: {error}"
        );
        assert_eq!(error.into_inner(), FetchError::Timeout);
    }
}

#[test]
fn a_worker_batch_reports_the_requests_that_outlived_its_budget() {
    let base = spawn_silent_server();
    let worker = NetworkWorker::start(transport(quiet(
        Duration::from_secs(20),
        Duration::from_secs(2),
    )))
    .expect("start worker");
    let requests = (0..4)
        .map(|index| FetchRequest::get(base.join(&index.to_string()).expect("resource URL")))
        .collect();
    let options = BatchOptions {
        max_concurrency: 2,
        timeout: Duration::from_millis(400),
        ..BatchOptions::default()
    };

    let started = Instant::now();
    let results = worker
        .submit_batch(requests, options)
        .recv_timeout(Duration::from_secs(5))
        .expect("a stalled batch must still report at its budget");
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "the batch must end at its own budget, took {elapsed:?}"
    );
    assert_eq!(results.len(), 4);
    for result in &results {
        let error = result.as_ref().expect_err("a silent server cannot answer");
        assert_eq!(error.phase(), Some(FetchPhase::Queued), "got: {error}");
    }
}

#[test]
fn a_request_with_a_body_that_is_never_answered_names_the_send_phase() {
    let base = spawn_silent_server();
    // A request that carries a body can legitimately still be writing when the
    // budget expires, so ureq's send-request reason stays the send phase: the
    // loopback-to-header rewrite above is deliberately limited to bodyless
    // requests.
    let request = FetchRequest::post(base.join("submit").expect("resource URL"))
        .with_body("field=value")
        .with_header("Content-Type", "application/x-www-form-urlencoded");
    let error = transport(quiet(
        Duration::from_millis(400),
        Duration::from_millis(400),
    ))
    .fetch(&request, &CancelToken::default())
    .expect_err("a server that never answers must fail at its budget");

    assert_eq!(error.phase(), Some(FetchPhase::RequestSend), "got: {error}");
    assert_eq!(error.into_inner(), FetchError::Timeout);
}

#[test]
fn a_zero_batch_budget_disables_the_bound() {
    let (base, server) = spawn_ok_server(2);
    let client = transport(quiet(Duration::from_secs(10), Duration::from_secs(2)));
    let requests = (0..2)
        .map(|index| FetchRequest::get(base.join(&index.to_string()).expect("resource URL")))
        .collect();
    let options = BatchOptions {
        max_concurrency: 2,
        timeout: Duration::ZERO,
        ..BatchOptions::default()
    };

    let results = client.fetch_batch(requests, &options, &CancelToken::default());
    server.join().expect("server exits");

    assert!(
        results.iter().all(Result::is_ok),
        "an unbounded batch must not fail requests that complete"
    );
}

#[test]
fn a_batch_budget_larger_than_the_requests_keeps_waiting() {
    let (base, server) = spawn_ok_server(2);
    let client = transport(quiet(Duration::from_secs(10), Duration::from_secs(2)));
    let requests = (0..2)
        .map(|index| FetchRequest::get(base.join(&index.to_string()).expect("resource URL")))
        .collect();
    let options = BatchOptions {
        max_concurrency: 2,
        ..BatchOptions::default()
    };

    let results = client.fetch_batch(requests, &options, &CancelToken::default());
    server.join().expect("server exits");

    assert!(
        results.iter().all(Result::is_ok),
        "an unexpired budget must not fail a request that completes"
    );
}
