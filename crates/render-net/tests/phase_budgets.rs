//! Phase budgets: connect, response and body are bounded separately, every
//! stall ends as a reported terminal outcome that names its phase, and a
//! stalled connection is never handed back to the pool as healthy.
//!
//! A transfer can die in three different ways, and each needs a different bound.
//! Measured before this work, on this crate:
//!
//! * **Response** - a server that accepts and says nothing was already bounded,
//!   but only *implicitly*: it borrowed ureq's send-request budget, had no
//!   budget of its own, and had no name. Nothing said so, and the only reason
//!   the reported phase was even correct was a rewrite in `fetch_transfer` that
//!   exists because the reason is ambiguous. It is now `response_timeout`.
//! * **Body** - a body that delivered part of a response and then went quiet was
//!   bounded only by `read_bounded_body` watching a channel, with no socket
//!   deadline at all: the pump thread stayed blocked on the dead socket and the
//!   connection stayed held. It is now `body_idle_timeout`, set both on the
//!   channel and on the socket. `a_body_timeout_releases_the_socket_instead_of
//!   _leaving_it_held` is what fails if the socket half is dropped.
//! * **Connect** - already had its own budget; see `connect_budget.rs`.
//!
//! The body budget must stay an **idle-read** bound rather than becoming a
//! whole-transfer budget, because a large stylesheet corpus on a slow link is
//! precisely what this engine struggles with. `a_slow_large_body_outlives_every
//! _total_budget_and_still_completes` is the test that holds that line, and it
//! outlives the *response* budget too, to show the two are not the same knob.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use render_net::{
    CancelToken, FetchConfig, FetchError, FetchEvent, FetchObserver, FetchPhase, FetchRequest,
    HttpTransport, Url,
};

/// Upper bound on how long a silent test server keeps a connection open. Only
/// reached if the client never gives up, which is the failure under test.
const SILENT_HOLD: Duration = Duration::from_secs(20);

/// One recorded terminal outcome.
#[derive(Clone, Debug)]
struct Recorded {
    /// The whole line, for tests that assert on the report as a whole.
    line: String,
    /// The transport's own terminal text for a failure. This is the part that
    /// names the phase and the wait, and it is the thing two paths must agree
    /// on; the line also carries the observer's own wall time, which legitimately
    /// differs between runs.
    error: Option<String>,
}

/// Records every terminal outcome the transport reports, standing in for a log
/// file.
#[derive(Debug, Default)]
struct Recorder {
    events: Mutex<Vec<Recorded>>,
}

impl Recorder {
    fn events(&self) -> Vec<Recorded> {
        self.events.lock().expect("recorded events").clone()
    }

    fn lines(&self) -> Vec<String> {
        self.events().into_iter().map(|event| event.line).collect()
    }

    /// The error text of the one recorded failure, for tests that fetch once and
    /// expect it to fail.
    fn only_failure(&self) -> String {
        let events = self.events();
        assert_eq!(
            events.len(),
            1,
            "expected exactly one terminal outcome: {events:?}"
        );
        events
            .into_iter()
            .next()
            .expect("recorded outcome")
            .error
            .expect(
                "a timeout must be reported as a failure, and that failure must carry the \
             transport's own text naming its phase and wait",
            )
    }
}

impl FetchObserver for Recorder {
    fn on_fetch_event(&self, event: &FetchEvent<'_>) {
        let (line, error) = match *event {
            FetchEvent::Completed {
                method,
                url,
                status,
                body_bytes,
                elapsed,
            } => (
                format!(
                    "ok {} {url} {} {body_bytes} {elapsed:?}",
                    method.as_str(),
                    status.as_u16()
                ),
                None,
            ),
            FetchEvent::Failed {
                method,
                url,
                elapsed,
                error,
            } => (
                format!(
                    "fail {} {url} {elapsed:?} {error}",
                    method.as_str(),
                    error = error
                ),
                Some(error.to_string()),
            ),
            other => (format!("other {other:?}"), None),
        };
        self.events
            .lock()
            .expect("recorded events")
            .push(Recorded { line, error });
    }
}

/// A config with the observer swapped out and the budgets made explicit.
fn config(
    response: Duration,
    body_idle: Duration,
    connect: Duration,
    observer: Arc<Recorder>,
) -> FetchConfig {
    FetchConfig {
        // The shared request budget, which the two phase budgets no longer
        // follow: they are set explicitly, so this value governs name
        // resolution and the redirect chain only.
        timeout: Duration::from_secs(10),
        response_timeout: Some(response),
        body_idle_timeout: Some(body_idle),
        connect_timeout: connect,
        observer,
        ..FetchConfig::default()
    }
}

fn recorder() -> Arc<Recorder> {
    Arc::new(Recorder::default())
}

fn direct(config: FetchConfig) -> HttpTransport {
    HttpTransport::with_proxy(config, None)
}

/// Reads one HTTP request off the wire and returns the request target.
fn read_request(stream: &mut TcpStream) -> String {
    let mut raw = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !raw.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => raw.extend_from_slice(&chunk[..count]),
        }
    }
    String::from_utf8_lossy(&raw)
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_owned()
}

/// Accepts connections, reads each request, and never answers. Every connection
/// is released as soon as the client goes away, or after `SILENT_HOLD`.
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
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(count) => request.extend_from_slice(&chunk[..count]),
                    }
                }
                // Hold the connection open without a response. A closed client
                // or the read timeout ends it.
                let mut held = Vec::new();
                while stream.read(&mut held).is_ok() {}
            });
        }
    });
    Url::parse(&format!("http://{address}/")).expect("construct local URL")
}

/// Answers a request with headers and part of the body, then goes silent.
fn spawn_body_stall_server() -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind body stall server");
    let address = listener.local_addr().expect("read local address");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let _ignored = stream.set_read_timeout(Some(SILENT_HOLD));
            thread::spawn(move || {
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(count) => request.extend_from_slice(&chunk[..count]),
                    }
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 65536\r\nConnection: close\r\n\r\n",
                );
                let _ = stream.write_all(&[0x5A_u8; 4096]);
                // Headers and a first piece arrived; the rest never does.
                let mut held = Vec::new();
                while stream.read(&mut held).is_ok() {}
            });
        }
    });
    Url::parse(&format!("http://{address}/")).expect("construct local URL")
}

/// Sends `body` in `piece`-sized writes with `delay` between them, so the
/// transfer keeps making progress and each individual read returns quickly.
///
/// The response is announced with a `Content-Length` and `Connection: close`,
/// and the socket is held open after the last piece, so a client that has not
/// finished reading cannot mistake the transfer for a complete one.
fn spawn_trickle_server(body: Vec<u8>, piece: usize, delay: Duration) -> (Url, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind trickle server");
    let address = listener.local_addr().expect("read local address");
    let body = Arc::new(body);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let _ignored = stream.set_read_timeout(Some(SILENT_HOLD));
            let body = Arc::clone(&body);
            thread::spawn(move || {
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(count) => request.extend_from_slice(&chunk[..count]),
                    }
                }
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                );
                for piece in body.chunks(piece) {
                    if stream.write_all(piece).is_err() {
                        return;
                    }
                    thread::sleep(delay);
                }
                let mut held = Vec::new();
                while stream.read(&mut held).is_ok() {}
            });
        }
    });
    (
        Url::parse(&format!("http://{address}/")).expect("construct local URL"),
        Arc::new(AtomicUsize::new(0)),
    )
}

/// A CONNECT proxy that establishes the tunnel and then goes silent inside it,
/// so the client is connected and has written its request but gets no response.
fn spawn_silent_connect_proxy() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind silent proxy");
    let address = listener.local_addr().expect("read proxy address");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut client) = stream else { break };
            thread::spawn(move || {
                let mut head = Vec::new();
                let mut chunk = [0_u8; 512];
                while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                    match client.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(count) => head.extend_from_slice(&chunk[..count]),
                    }
                }
                if client
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .is_err()
                {
                    return;
                }
                // The tunnelled request arrives; nothing ever comes back.
                let mut request = Vec::new();
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match client.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(count) => request.extend_from_slice(&chunk[..count]),
                    }
                }
                let mut drain = Vec::new();
                while client.read(&mut drain).is_ok() {}
            });
        }
    });
    address
}

/// A CONNECT proxy that relays to a real origin, and counts the tunnels it
/// opens. A tunnel count is the observable for "was this connection reused",
/// which is what the pool-health tests need.
fn spawn_relay_proxy(tunnels: Arc<AtomicUsize>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind relay proxy");
    let address = listener.local_addr().expect("read proxy address");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut client) = stream else { break };
            tunnels.fetch_add(1, Ordering::SeqCst);
            thread::spawn(move || {
                let mut head = Vec::new();
                let mut chunk = [0_u8; 512];
                while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                    match client.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(count) => head.extend_from_slice(&chunk[..count]),
                    }
                }
                let request = String::from_utf8_lossy(&head);
                let Some(authority) = request
                    .lines()
                    .next()
                    .and_then(|line| line.split(' ').nth(1))
                    .and_then(|authority| authority.rsplit_once(':'))
                else {
                    return;
                };
                let Ok(port) = authority.1.parse::<u16>() else {
                    return;
                };
                let Ok(mut origin) = TcpStream::connect((authority.0, port)) else {
                    return;
                };
                if client
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .is_err()
                {
                    return;
                }
                let mut client_copy = client.try_clone().expect("client socket clone");
                let mut origin_copy = origin.try_clone().expect("origin socket clone");
                let upstream = thread::spawn(move || std::io::copy(&mut client, &mut origin));
                let downstream =
                    thread::spawn(move || std::io::copy(&mut origin_copy, &mut client_copy));
                let _ = upstream.join();
                let _ = downstream.join();
            });
        }
    });
    address
}

// ---------------------------------------------------------------------------
// The three budgets are separate, named, and independently settable.
// ---------------------------------------------------------------------------

#[test]
fn the_three_phase_budgets_have_named_defaults_and_fall_back_to_the_request_budget() {
    let default = FetchConfig::default();
    assert_eq!(FetchConfig::DEFAULT_CONNECT_TIMEOUT, Duration::from_secs(5));
    assert_eq!(FetchConfig::DEFAULT_TIMEOUT, Duration::from_secs(30));
    assert_eq!(
        default.connect_timeout,
        FetchConfig::DEFAULT_CONNECT_TIMEOUT
    );
    assert_eq!(default.timeout, FetchConfig::DEFAULT_TIMEOUT);

    // Unset phase budgets follow the shared request budget, so one number still
    // bounds a whole request.
    assert_eq!(default.response_timeout, None);
    assert_eq!(default.body_idle_timeout, None);
    assert_eq!(
        default.effective_response_timeout(),
        FetchConfig::DEFAULT_TIMEOUT
    );
    assert_eq!(
        default.effective_body_idle_timeout(),
        FetchConfig::DEFAULT_TIMEOUT
    );

    // A zero connect budget is the documented opt-out and resolves to the
    // request budget, never to "unbounded".
    let zero_connect = FetchConfig {
        connect_timeout: Duration::ZERO,
        ..FetchConfig::default()
    };
    assert_eq!(
        zero_connect.effective_connect_timeout(),
        FetchConfig::DEFAULT_TIMEOUT
    );
    // And a connect budget larger than the request budget is clamped, so no
    // phase can outlive the budget the caller asked for.
    let long_connect = FetchConfig {
        connect_timeout: Duration::from_secs(600),
        ..FetchConfig::default()
    };
    assert_eq!(
        long_connect.effective_connect_timeout(),
        FetchConfig::DEFAULT_TIMEOUT
    );

    // Setting one phase budget must not disturb the others.
    let split = FetchConfig {
        response_timeout: Some(Duration::from_millis(250)),
        body_idle_timeout: Some(Duration::from_millis(500)),
        ..FetchConfig::default()
    };
    assert_eq!(
        split.effective_response_timeout(),
        Duration::from_millis(250)
    );
    assert_eq!(
        split.effective_body_idle_timeout(),
        Duration::from_millis(500)
    );
    assert_eq!(
        split.effective_connect_timeout(),
        FetchConfig::DEFAULT_CONNECT_TIMEOUT
    );
}

#[test]
fn a_response_budget_is_not_a_body_budget() {
    // The two budgets are separate knobs, and a body transfer is not killed by
    // the response budget. This is the distinction that has to hold: ureq's
    // `timeout_recv_response` would be a *total* budget over headers and body
    // together, which is why it stays unset and the response budget is carried
    // on the send side instead.
    let split = FetchConfig {
        response_timeout: Some(Duration::from_millis(100)),
        body_idle_timeout: Some(Duration::from_secs(5)),
        ..FetchConfig::default()
    };
    assert!(split.effective_response_timeout() < split.effective_body_idle_timeout());
}

// ---------------------------------------------------------------------------
// Response phase: the stall that used to hang.
// ---------------------------------------------------------------------------

#[test]
fn a_server_that_never_answers_ends_as_a_named_terminal_outcome() {
    let base = spawn_silent_server();
    let observed = recorder();
    // Generous relative to a loopback connect, short enough that the test does
    // not wait: the bound is what is under test, and it is asserted below.
    let budget = Duration::from_millis(600);
    let client = direct(config(
        budget,
        Duration::from_secs(5),
        Duration::from_secs(2),
        Arc::clone(&observed),
    ));

    let started = Instant::now();
    let error = client
        .fetch(
            &FetchRequest::get(base.join("silent.css").expect("resource URL")),
            &CancelToken::default(),
        )
        .expect_err("a server that never answers must fail at its budget");
    let elapsed = started.elapsed();

    // The stall is attributed to the response phase, not to a generic error.
    assert_eq!(
        error.phase(),
        Some(FetchPhase::ResponseHeaders),
        "a server that accepts and then says nothing stalls in the response phase: {error}"
    );
    assert_eq!(error.clone().into_inner(), FetchError::Timeout);

    // The reported wait covers the whole stall, and the request ended near its
    // budget rather than hanging or failing early.
    let reported = error
        .elapsed()
        .expect("a phase failure reports its elapsed time");
    assert!(
        reported >= budget * 2 / 3,
        "the reported wait must cover the stall, got {reported:?} for a {budget:?} budget"
    );
    // The exact terminal text an operator sees: the phase, the kind of failure,
    // and the wait in milliseconds.
    assert_eq!(
        error.to_string(),
        format!(
            "response headers: request timed out after {}ms",
            reported.as_millis()
        ),
        "the terminal text must name the phase and the wait"
    );
    assert!(
        elapsed >= budget * 2 / 3,
        "the request must not give up before its budget, took {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(4),
        "the request must end at its budget, took {elapsed:?}"
    );

    // The terminal outcome an operator sees names the phase and the wait.
    let line = observed.lines().remove(0);
    assert!(line.starts_with("fail GET "), "got: {line}");
    assert!(
        line.contains("response headers: request timed out after"),
        "the reported failure must name its phase and kind: {line}"
    );
    assert!(
        line.contains("ms"),
        "the reported failure must carry its elapsed milliseconds: {line}"
    );
}

#[test]
fn a_proxy_that_never_answers_ends_the_same_way() {
    // The bug this suite exists for was found on the proxy path, so the proxy
    // path gets its own test rather than relying on the direct one.
    let proxy_addr = spawn_silent_connect_proxy();
    let proxy = ureq::Proxy::new(&format!("http://{proxy_addr}")).expect("build proxy");
    let observed = recorder();
    let budget = Duration::from_millis(600);
    let client = HttpTransport::with_proxy(
        config(
            budget,
            Duration::from_secs(5),
            Duration::from_secs(2),
            Arc::clone(&observed),
        ),
        Some(proxy),
    );

    // A reserved-for-documentation address: nothing resolves or listens there,
    // so the only party in the request is the local proxy, which is what makes
    // this a proxy-path test rather than a DNS test.
    let target = Url::parse("http://198.51.100.9/style.css").expect("target URL");
    let started = Instant::now();
    let error = client
        .fetch(&FetchRequest::get(target), &CancelToken::default())
        .expect_err("a proxy that never answers must fail at its budget");
    let elapsed = started.elapsed();

    assert_eq!(
        error.phase(),
        Some(FetchPhase::ResponseHeaders),
        "a proxy that establishes the tunnel and then says nothing stalls in the response phase: {error}"
    );
    assert_eq!(error.into_inner(), FetchError::Timeout);
    assert!(
        elapsed >= budget * 2 / 3 && elapsed < Duration::from_secs(4),
        "the proxied request must end at its budget, took {elapsed:?}"
    );

    let reported = observed.only_failure();
    assert!(
        reported.starts_with("response headers: request timed out after"),
        "the reported failure must name its phase and kind: {reported}"
    );
    assert!(
        reported.ends_with("ms"),
        "the reported failure must carry its elapsed milliseconds: {reported}"
    );
}

#[test]
fn the_proxy_and_direct_paths_enforce_the_same_phase_budgets() {
    // Both constructors funnel through `HttpTransport::with_proxy`, so this is
    // the test that keeps them from drifting: the same stall, the same budget,
    // the same phase and the same reported wait, with and without a proxy.
    let base = spawn_silent_server();
    let proxy_addr = spawn_relay_proxy(Arc::new(AtomicUsize::new(0)));
    let proxy = ureq::Proxy::new(&format!("http://{proxy_addr}")).expect("build proxy");
    let budget = Duration::from_millis(600);

    let runs = [("direct", None), ("proxied", Some(proxy))];
    let mut outcomes = Vec::new();
    for (label, proxy) in runs {
        let observed = recorder();
        let client = HttpTransport::with_proxy(
            config(
                budget,
                Duration::from_secs(5),
                Duration::from_secs(2),
                Arc::clone(&observed),
            ),
            proxy,
        );
        let error = client
            .fetch(
                &FetchRequest::get(base.join("silent.css").expect("resource URL")),
                &CancelToken::default(),
            )
            .expect_err("a server that never answers must fail at its budget");
        outcomes.push((label, error, observed.only_failure()));
    }

    let (direct_label, direct_error, direct_reported) = &outcomes[0];
    let (proxied_label, proxied_error, proxied_reported) = &outcomes[1];
    assert_eq!(direct_error.phase(), Some(FetchPhase::ResponseHeaders));
    assert_eq!(
        direct_error.phase(),
        proxied_error.phase(),
        "{direct_label} and {proxied_label} must attribute the same stall to the same phase"
    );
    assert_eq!(
        direct_error.clone().into_inner(),
        proxied_error.clone().into_inner()
    );
    // Same phase, same kind of failure, and both waited about the configured
    // budget. Only the millisecond count may differ, because the proxy really
    // does add a hop.
    for reported in [direct_reported, proxied_reported] {
        assert!(
            reported.starts_with("response headers: request timed out after"),
            "{direct_label}/{proxied_label} must report a named response-phase timeout: {reported}"
        );
        let waited: u128 = reported
            .rsplit(' ')
            .next()
            .and_then(|value| value.trim_end_matches("ms").parse().ok())
            .unwrap_or_else(|| panic!("the reported wait must be in milliseconds: {reported}"));
        assert!(
            waited >= budget.as_millis() * 2 / 3,
            "{direct_label}/{proxied_label} must report its own wait, got {waited}ms"
        );
    }
}

// ---------------------------------------------------------------------------
// Body phase: an idle read, and a large slow download that survives it.
// ---------------------------------------------------------------------------

#[test]
fn a_body_that_stops_arriving_ends_as_a_named_terminal_outcome() {
    let base = spawn_body_stall_server();
    let observed = recorder();
    let budget = Duration::from_millis(600);
    let client = direct(config(
        Duration::from_secs(2),
        budget,
        Duration::from_secs(2),
        Arc::clone(&observed),
    ));

    let started = Instant::now();
    let error = client
        .fetch(
            &FetchRequest::get(base.join("half.css").expect("resource URL")),
            &CancelToken::default(),
        )
        .expect_err("a body that stops arriving must fail at its idle bound");
    let elapsed = started.elapsed();

    assert_eq!(
        error.phase(),
        Some(FetchPhase::BodyTransfer),
        "headers arrived, so a body that stops is a body-phase stall: {error}"
    );
    assert_eq!(error.into_inner(), FetchError::Timeout);
    assert!(
        elapsed >= budget * 2 / 3,
        "the request must not give up before its idle bound, took {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(4),
        "the request must end at its idle bound, took {elapsed:?}"
    );

    // The terminal outcome an operator sees names the phase and the wait.
    assert!(
        observed
            .only_failure()
            .starts_with("body transfer: request timed out after"),
        "the reported failure must name its phase and kind"
    );
}

#[test]
fn a_slow_large_body_outlives_every_total_budget_and_still_completes() {
    // The body budget is an idle-read bound, not a total budget. This transfer
    // deliberately outlives:
    //   * the body idle bound, by many multiples, and
    //   * the response budget, which proves the two are separate knobs - a
    //     single `timeout_recv_response`-style budget would have killed it
    //     halfway through.
    // Every individual read still returns well inside the idle bound, which is
    // what a slow link looks like, and the average rate stays above the
    // minimum-progress floor.
    const BODY: usize = 48 * 1024;
    const PIECE: usize = 2 * 1024;
    /// Gap between writes. The idle bound is ~7x this, so a machine shared with
    /// other agents has room before a read could look stalled.
    const GAP: Duration = Duration::from_millis(90);
    const IDLE_BOUND: Duration = Duration::from_millis(700);
    /// Shorter than the ~2.2s the transfer takes overall.
    const RESPONSE_BOUND: Duration = Duration::from_millis(1200);

    let pieces = u32::try_from(BODY / PIECE).expect("the fixture is a small body");
    let expected_total = GAP * pieces;
    assert!(
        expected_total > IDLE_BOUND * 2 && expected_total > RESPONSE_BOUND,
        "the fixture must genuinely outlive the budgets: {expected_total:?}"
    );

    let (base, _served) = spawn_trickle_server(vec![0xC7_u8; BODY], PIECE, GAP);
    let observed = recorder();
    let client = direct(config(
        RESPONSE_BOUND,
        IDLE_BOUND,
        Duration::from_secs(2),
        Arc::clone(&observed),
    ));

    let started = Instant::now();
    let response = client
        .fetch(&FetchRequest::get(base), &CancelToken::default())
        .expect("a body that keeps arriving must complete regardless of total time");
    let elapsed = started.elapsed();

    assert_eq!(response.body.len(), BODY);
    assert_eq!(response.body[0], 0xC7);
    assert_eq!(response.body[BODY - 1], 0xC7);
    assert!(
        elapsed >= expected_total / 2,
        "the fixture must really have taken longer than one idle bound, took {elapsed:?}"
    );
    assert!(
        elapsed > RESPONSE_BOUND,
        "the transfer must really have outlived the response budget, took {elapsed:?} against {RESPONSE_BOUND:?}"
    );
    let line = observed.lines().remove(0);
    assert!(
        line.starts_with("ok GET "),
        "a completing transfer must be reported as a success: {line}"
    );
}

/// A keep-alive origin for the pool-health tests: `/first` is answered and its
/// socket becomes pooled, `/stall` stalls (after headers and a partial body, so
/// the body read is what gives up), and `/third` is answered normally on
/// whichever connection serves it.
///
/// Returns the base URL and the number of connections accepted. That count is
/// the observable for "was this connection reused": if the stalled socket came
/// back to the pool, `/third` would be served on the connection the server has
/// stopped reading and the count would not grow.
fn spawn_pool_then_stall_origin(stalled_in_body: bool) -> (Url, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind reuse-after-stall origin");
    let address = listener.local_addr().expect("read local address");
    let accepted = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&accepted);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            // Counted on arrival, so a test can tell reuse from a fresh socket.
            counted.fetch_add(1, Ordering::SeqCst);
            let _ignored = stream.set_read_timeout(Some(SILENT_HOLD));
            thread::spawn(move || {
                loop {
                    let path = read_request(&mut stream);
                    if path.is_empty() {
                        return;
                    }
                    let answer = |stream: &mut TcpStream, body: &'static str| {
                        stream
                            .write_all(
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\
                                 Connection: keep-alive\r\n\r\n{body}",
                                    body.len()
                                )
                                .as_bytes(),
                            )
                            .is_ok()
                    };
                    match path.as_str() {
                        "/first" => {
                            if !answer(&mut stream, "first") {
                                return;
                            }
                        }
                        // Headers and a partial body, then silence, so the failure is
                        // the body read's and the socket is left mid-message.
                        "/stall" if stalled_in_body => {
                            let _ = stream.write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Length: 65536\r\n\
                              Connection: keep-alive\r\n\r\n",
                            );
                            let _ = stream.write_all(&[0x11_u8; 2048]);
                            let mut drain = Vec::new();
                            while stream.read(&mut drain).is_ok() {}
                            return;
                        }
                        // No status line at all, so the failure is the response wait's.
                        "/stall" => {
                            let mut drain = Vec::new();
                            while stream.read(&mut drain).is_ok() {}
                            return;
                        }
                        "/third" => {
                            if !answer(&mut stream, "third!") {
                                return;
                            }
                        }
                        other => panic!("unexpected request target {other}"),
                    }
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
fn a_body_stall_does_not_return_the_stalled_connection_to_the_pool() {
    // A connection whose body read timed out must not be handed back to the
    // pool as healthy: the next request to that origin has to open a fresh
    // connection rather than inherit a socket in an unknown state.
    let (base, accepted) = spawn_pool_then_stall_origin(true);
    let observed = recorder();
    let client = direct(config(
        Duration::from_secs(2),
        Duration::from_millis(600),
        Duration::from_secs(2),
        Arc::clone(&observed),
    ));

    let first = client
        .fetch(
            &FetchRequest::get(base.join("first").expect("first URL")),
            &CancelToken::default(),
        )
        .expect("the first request is answered and pooled");
    assert_eq!(first.body, b"first");

    let error = client
        .fetch(
            &FetchRequest::get(base.join("stall").expect("stall URL")),
            &CancelToken::default(),
        )
        .expect_err("the body read must fail at its idle bound");
    assert_eq!(
        error.phase(),
        Some(FetchPhase::BodyTransfer),
        "got: {error}"
    );
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "the stalling request must have run on the pooled connection"
    );

    let third = client
        .fetch(
            &FetchRequest::get(base.join("third").expect("third URL")),
            &CancelToken::default(),
        )
        .expect("a request after a body stall must still succeed");
    assert_eq!(third.body, b"third!");
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        2,
        "the stalled connection must not have been handed back to the pool: \
         the next request needs its own socket"
    );

    // All three requests reported, and the middle one as a named body-phase
    // timeout rather than a generic error.
    let lines = observed.lines();
    assert_eq!(lines.len(), 3, "every request must be reported: {lines:?}");
    assert!(lines[0].starts_with("ok GET "), "got: {}", lines[0]);
    assert!(
        lines[1].starts_with("fail GET ")
            && lines[1].contains("body transfer: request timed out after"),
        "the body stall must be reported as a named body-phase timeout: {}",
        lines[1]
    );
    assert!(lines[2].starts_with("ok GET "), "got: {}", lines[2]);
}

#[test]
fn a_response_stall_does_not_return_the_stalled_connection_to_the_pool() {
    // The same guarantee for the response phase, and on the path where the
    // original hang was found. A tunnel is established, the request is written,
    // and nothing comes back. The follow-up request must not inherit that
    // socket, so the tunnel count is the observable: it has to grow.
    let (base, _origins) = spawn_pool_then_stall_origin(false);
    let tunnels = Arc::new(AtomicUsize::new(0));
    let proxy_addr = spawn_relay_proxy(Arc::clone(&tunnels));
    let proxy = ureq::Proxy::new(&format!("http://{proxy_addr}")).expect("build proxy");
    let observed = recorder();
    let client = HttpTransport::with_proxy(
        config(
            Duration::from_secs(2),
            Duration::from_millis(600),
            Duration::from_secs(2),
            Arc::clone(&observed),
        ),
        Some(proxy),
    );

    let first = client
        .fetch(
            &FetchRequest::get(base.join("first").expect("first URL")),
            &CancelToken::default(),
        )
        .expect("the first request is answered through the tunnel");
    assert_eq!(first.body, b"first");

    let error = client
        .fetch(
            &FetchRequest::get(base.join("stall").expect("stall URL")),
            &CancelToken::default(),
        )
        .expect_err("the response wait must fail at its budget");
    assert_eq!(
        error.phase(),
        Some(FetchPhase::ResponseHeaders),
        "got: {error}"
    );
    assert_eq!(
        tunnels.load(Ordering::SeqCst),
        1,
        "the stall reused the tunnel"
    );

    let third = client
        .fetch(
            &FetchRequest::get(base.join("third").expect("third URL")),
            &CancelToken::default(),
        )
        .expect("a request after a response stall must still succeed");
    assert_eq!(third.body, b"third!");
    assert_eq!(
        tunnels.load(Ordering::SeqCst),
        2,
        "the stalled tunnel must not have been handed back to the pool"
    );

    let lines = observed.lines();
    assert_eq!(lines.len(), 3, "every request must be reported: {lines:?}");
    assert!(
        lines[1].contains("response headers: request timed out after"),
        "the response stall must be reported as a named response-phase timeout: {}",
        lines[1]
    );
}

#[test]
fn a_body_timeout_releases_the_socket_instead_of_leaving_it_held() {
    // The idle bound reaches the socket, so a stalled body read fails inside
    // ureq, the reader is dropped, and the origin sees the connection close
    // straight away. The observable is the server side: its blocking read
    // returns as soon as the client goes away. This matters because a socket
    // that is never released is not just leaked, it is indistinguishable from a
    // connection the pool still considers usable.
    let (sender, released) = std::sync::mpsc::channel::<()>();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind release origin");
    let address = listener.local_addr().expect("read local address");
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            // Far longer than the client will take, so the only way this read
            // can return is because the client closed the connection.
            let _ignored = stream.set_read_timeout(Some(SILENT_HOLD));
            let sender = sender.clone();
            thread::spawn(move || {
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => request.extend_from_slice(&chunk[..count]),
                    }
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 65536\r\nConnection: keep-alive\r\n\r\n",
                );
                let _ = stream.write_all(&[0x3C_u8; 2048]);
                // Blocks until the peer goes away, then reports it.
                let mut held = Vec::new();
                let _ = stream.read(&mut held);
                let _ = sender.send(());
            });
        }
    });
    let base = Url::parse(&format!("http://{address}/stall.css")).expect("construct local URL");

    let budget = Duration::from_millis(600);
    let client = direct(config(
        Duration::from_secs(2),
        budget,
        Duration::from_secs(2),
        recorder(),
    ));
    let error = client
        .fetch(&FetchRequest::get(base), &CancelToken::default())
        .expect_err("a stalled body read must fail at its idle bound");
    assert_eq!(
        error.phase(),
        Some(FetchPhase::BodyTransfer),
        "got: {error}"
    );

    // Well inside the server's own 20s read timeout, so this can only pass if
    // the client closed the connection as part of failing.
    assert!(
        released.recv_timeout(Duration::from_secs(3)).is_ok(),
        "a body timeout must release the connection, not leave it held"
    );
}

/// A CONNECT proxy that establishes the tunnel and then never speaks again,
/// which leaves an `https` target waiting for a TLS handshake that will not
/// arrive. This is the most hang-shaped path through a proxy: the socket is
/// open, the client has written, and nothing comes back.
fn spawn_quiet_tunnel_proxy() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind quiet tunnel proxy");
    let address = listener.local_addr().expect("read proxy address");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut client) = stream else { break };
            thread::spawn(move || {
                let mut head = Vec::new();
                let mut chunk = [0_u8; 512];
                while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                    match client.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(count) => head.extend_from_slice(&chunk[..count]),
                    }
                }
                if client
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .is_err()
                {
                    return;
                }
                // Tunnel open, then silence: no ServerHello is ever coming.
                let mut drain = Vec::new();
                while client.read(&mut drain).is_ok() {}
            });
        }
    });
    address
}

#[test]
fn an_https_target_through_a_quiet_tunnel_is_still_bounded_and_named() {
    // The handshake is lazy in ureq, so this stall happens after the connect
    // phase the connect budget covers. It must still end, and end by name.
    let proxy_addr = spawn_quiet_tunnel_proxy();
    let proxy = ureq::Proxy::new(&format!("http://{proxy_addr}")).expect("build proxy");
    let observed = recorder();
    let budget = Duration::from_millis(600);
    let client = HttpTransport::with_proxy(
        config(
            budget,
            Duration::from_secs(2),
            Duration::from_secs(2),
            Arc::clone(&observed),
        ),
        Some(proxy),
    );
    // A literal address, so this is a TLS stall and not a DNS one.
    let target = Url::parse("https://198.51.100.10/style.css").expect("target URL");

    let started = Instant::now();
    let error = client
        .fetch(&FetchRequest::get(target), &CancelToken::default())
        .expect_err("a tunnel that never handshakes must fail at its budget");
    let elapsed = started.elapsed();

    assert!(
        elapsed >= budget * 2 / 3 && elapsed < Duration::from_secs(4),
        "a dead tunnel must end at its budget, took {elapsed:?}"
    );
    // Documented limitation, asserted rather than wished away: ureq runs the
    // TLS handshake lazily on the first write, under the send budget, so this
    // stall is attributed to the phase that owns the budget rather than to
    // `FetchPhase::TlsHandshake`. It is still bounded and still named, which is
    // what matters here; the distinction would need a handshake the connect
    // budget covers, which only a non-tunnelled connection gets.
    assert_eq!(
        error.phase(),
        Some(FetchPhase::ResponseHeaders),
        "a lazy handshake over a dead tunnel is reported against the budget that \
         covers it: {error}"
    );
    let reported = observed.only_failure();
    assert!(
        reported.contains("timed out after"),
        "the failure must be a named timeout with its wait: {reported}"
    );
}

#[test]
fn a_live_transfer_through_a_proxy_still_succeeds() {
    // The budgets are a bound, not a regression: a healthy proxied transfer,
    // with a body larger than one read buffer, must still complete intact.
    let origin = TcpListener::bind("127.0.0.1:0").expect("bind live origin");
    let origin_address = origin.local_addr().expect("read origin address");
    let body = vec![0x2B_u8; 200 * 1024];
    let served = Arc::new(body.clone());
    thread::spawn(move || {
        for stream in origin.incoming().flatten() {
            let mut stream = stream;
            let _ignored = stream.set_read_timeout(Some(SILENT_HOLD));
            let served = Arc::clone(&served);
            thread::spawn(move || {
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(count) => request.extend_from_slice(&chunk[..count]),
                    }
                }
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        served.len()
                    )
                    .as_bytes(),
                );
                let _ = stream.write_all(&served);
            });
        }
    });

    let reached = Arc::new(AtomicUsize::new(0));
    let proxy_addr = spawn_relay_proxy(Arc::clone(&reached));
    let proxy = ureq::Proxy::new(&format!("http://{proxy_addr}")).expect("build proxy");
    let observed = recorder();
    let client = HttpTransport::with_proxy(
        config(
            Duration::from_secs(5),
            Duration::from_secs(2),
            Duration::from_secs(2),
            Arc::clone(&observed),
        ),
        Some(proxy),
    );
    let url = Url::parse(&format!("http://{origin_address}/big.css")).expect("construct URL");

    let response = client
        .fetch(&FetchRequest::get(url), &CancelToken::default())
        .expect("a healthy proxied transfer must succeed");
    assert_eq!(response.body, body);
    assert_eq!(reached.load(Ordering::SeqCst), 1, "the proxy must be used");
    assert!(observed.lines().remove(0).starts_with("ok GET "));
}
