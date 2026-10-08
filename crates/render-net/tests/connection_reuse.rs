//! HTTP connection reuse for compressed same-origin subresources.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use render_net::{CancelToken, FetchConfig, FetchRequest, HttpTransport};

/// Brotli encoding of "hello\n" (raw stream bytes, no container).
const BROTLI_HELLO: &[u8] = b"\x8f\x02\x80hello\n\x03";

fn gzip_hello() -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write as _;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(b"hello\n").unwrap();
    encoder.finish().unwrap()
}

/// Serve responses honoring the request's Accept-Encoding, keep-alive capable,
/// and count the number of accepted connections plus observed encodings.
fn spawn_negotiating_server() -> (
    std::net::SocketAddr,
    Arc<AtomicUsize>,
    Arc<Mutex<Vec<String>>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let encodings = Arc::new(Mutex::new(Vec::new()));
    let connections_clone = Arc::clone(&connections);
    let encodings_clone = Arc::clone(&encodings);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            connections_clone.fetch_add(1, Ordering::SeqCst);
            let encodings_clone = Arc::clone(&encodings_clone);
            std::thread::spawn(move || {
                let _ = handle_connection(stream, &encodings_clone);
            });
        }
    });
    (address, connections, encodings)
}

fn handle_connection(mut stream: TcpStream, encodings: &Mutex<Vec<String>>) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        loop {
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
            let count = stream.read(&mut chunk)?;
            if count == 0 {
                return Ok(());
            }
            buffer.extend_from_slice(&chunk[..count]);
        }
        let request = String::from_utf8_lossy(&buffer).into_owned();
        buffer.clear();
        let accept_encoding = request
            .to_ascii_lowercase()
            .lines()
            .find_map(|line| line.strip_prefix("accept-encoding:"))
            .unwrap_or("")
            .trim()
            .to_owned();
        encodings.lock().unwrap().push(accept_encoding.clone());
        let (body, encoding): (Vec<u8>, &str) = if accept_encoding.contains("br") {
            (BROTLI_HELLO.to_vec(), "br")
        } else if accept_encoding.contains("gzip") {
            (gzip_hello(), "gzip")
        } else {
            (b"hello\n".to_vec(), "identity")
        };
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nContent-Encoding: {encoding}\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes())?;
        stream.write_all(&body)?;
        stream.flush()?;
    }
}

fn fetch_twice(url: &url::Url) {
    let transport = HttpTransport::new(FetchConfig::default());
    let cancel = CancelToken::default();
    for _ in 0..2 {
        let response = transport
            .fetch(&FetchRequest::get(url.clone()), &cancel)
            .expect("fetch");
        assert_eq!(response.body, b"hello\n");
    }
}

#[test]
fn compressed_responses_keep_the_same_origin_connection_alive() {
    let (address, connections, encodings) = spawn_negotiating_server();
    let url = url::Url::parse(&format!("http://{address}/one")).unwrap();
    fetch_twice(&url);
    assert_eq!(encodings.lock().unwrap()[0], "gzip");
    assert_eq!(encodings.lock().unwrap()[1], "gzip");
    assert_eq!(
        connections.load(Ordering::SeqCst),
        1,
        "gzip should reuse conn"
    );
}

#[test]
fn the_transport_advertises_only_encodings_it_can_pool() {
    // Connection reuse is the whole reason the transport pins its
    // `Accept-Encoding`. Three requests to one origin must cost one connection,
    // not three handshakes: a page pulling forty same-origin assets over
    // HTTP/1.1 otherwise pays TCP and TLS forty times.
    let (address, connections, encodings) = spawn_negotiating_server();
    let url = url::Url::parse(&format!("http://{address}/one")).unwrap();
    let transport = HttpTransport::new(FetchConfig::default());
    let cancel = CancelToken::default();

    let mut bodies = Vec::new();
    for _ in 0..3 {
        bodies.push(
            transport
                .fetch(&FetchRequest::get(url.clone()), &cancel)
                .expect("fetch")
                .body,
        );
    }

    assert!(
        encodings
            .lock()
            .unwrap()
            .iter()
            .all(|value| value == "gzip"),
        "only gzip may be advertised: {:?}",
        encodings.lock().unwrap()
    );
    assert_eq!(bodies, vec![b"hello\n".to_vec(); 3]);
    assert_eq!(
        connections.load(Ordering::SeqCst),
        1,
        "three requests to one origin must share one connection"
    );
}

/// A local origin serving `count` distinct keep-alive resources, counting every
/// connection it accepts.
fn spawn_asset_origin(count: usize) -> (url::Url, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&connections);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            counter.fetch_add(1, Ordering::SeqCst);
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(30)));
            std::thread::spawn(move || {
                let mut buffer = Vec::new();
                let mut chunk = [0_u8; 1024];
                loop {
                    while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => return,
                            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
                        }
                    }
                    buffer.clear();
                    // Every response is deliberately unique so a cross-attributed
                    // body would be visible, and every one keeps the connection
                    // alive so the pool is what decides reuse.
                    let body = b"body-for-this-request-only";
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nContent-Length: {}\r\n\
                         Connection: keep-alive\r\n\r\n",
                        body.len()
                    );
                    let mut response = head.into_bytes();
                    response.extend_from_slice(body);
                    if stream.write_all(&response).is_err() {
                        return;
                    }
                }
            });
        }
    });
    let _ = count;
    (
        url::Url::parse(&format!("http://{address}/")).unwrap(),
        connections,
    )
}

#[test]
fn a_forty_asset_page_uses_one_connection_per_in_flight_slot() {
    // The shape of every stylesheet corpus in the real-site evidence: one page
    // pulling forty assets from a single origin over HTTP/1.1, which can have
    // `DEFAULT_PER_ORIGIN_CONCURRENCY` requests in flight at a time.
    //
    // Six connections is the floor for that shape - you cannot have six requests
    // in flight over fewer than six sockets - so this asserts the pool keeps
    // enough idle connections per origin to reach it, and no test-only ceiling.
    const ASSETS: usize = 40;
    let (base, connections) = spawn_asset_origin(ASSETS);
    let per_origin = render_net::DEFAULT_PER_ORIGIN_CONCURRENCY;
    let requests = (0..ASSETS)
        .map(|index| {
            render_net::FetchRequest::get(base.join(&format!("asset-{index}.css")).unwrap())
        })
        .collect();
    let transport = HttpTransport::new(FetchConfig::default());

    let results = transport.fetch_batch(
        requests,
        &render_net::BatchOptions::default(),
        &CancelToken::default(),
    );

    assert_eq!(results.len(), ASSETS, "input order is retained");
    for result in &results {
        assert_eq!(
            result.as_ref().expect("asset").body,
            b"body-for-this-request-only"
        );
    }
    // A socket that finishes early can carry a later request, so the pool may
    // open fewer than `per_origin` connections when the waves overlap unevenly.
    // The property is the ceiling: a handshake per wave would open about forty.
    let opened = connections.load(Ordering::SeqCst);
    assert!(
        (1..=per_origin).contains(&opened),
        "forty assets from one origin must cost at most one connection per in-flight \
         slot, not a handshake per wave; opened {opened}"
    );
}

#[test]
fn a_pooled_connection_the_origin_closed_is_discarded_not_failed() {
    // Why `max_idle_age` is documented as inert rather than emulated here.
    // ureq 3.3's `Connection::age()` returns zero, so its `max_idle_age` never
    // evicts anything. That is survivable because a pooled connection is probed
    // before it is handed out: an origin that closed an idle socket costs one
    // wasted pool slot and a new handshake, not a failed request. This test is
    // the evidence for that claim.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&connections);
    std::thread::spawn(move || {
        for stream in listener.incoming().take(2) {
            let Ok(mut stream) = stream else { break };
            counter.fetch_add(1, Ordering::SeqCst);
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
            std::thread::spawn(move || {
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(count) => request.extend_from_slice(&chunk[..count]),
                    }
                }
                // Serve, then drop the socket: the client's pooled connection is
                // now dead while idle.
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok",
                );
            });
        }
    });
    let url = url::Url::parse(&format!("http://{address}/asset.css")).unwrap();
    let transport = HttpTransport::new(FetchConfig::default());
    let cancel = CancelToken::default();

    let first = transport
        .fetch(&render_net::FetchRequest::get(url.clone()), &cancel)
        .expect("first fetch");
    assert_eq!(first.body, b"ok");
    let second = transport
        .fetch(&render_net::FetchRequest::get(url), &cancel)
        .expect("a dead pooled connection must not fail the request");

    assert_eq!(second.body, b"ok");
    assert_eq!(
        connections.load(Ordering::SeqCst),
        2,
        "the closed connection is discarded and replaced, not handed to the request"
    );
}

#[test]
fn advertising_brotli_is_what_breaks_the_pool_in_ureq_3_3() {
    // This test documents a third-party defect, not behaviour we want. ureq
    // 3.3's brotli reader reaches the end of the *decoded* stream without
    // draining the length-delimited wire body, so the connection is never
    // returned to the pool: every request pays a fresh TCP connect and TLS
    // handshake. That is what made an earlier measurement read "135ms then
    // 134ms, no reuse" for a transport whose pool was in fact working.
    //
    // If this ever starts failing because ureq *does* pool brotli responses,
    // the defect is fixed upstream and the transport can advertise `br` again
    // (see `accept_encoding` in `HttpTransport::with_proxy`).
    let (address, connections, _encodings) = spawn_negotiating_server();
    let url = url::Url::parse(&format!("http://{address}/one")).unwrap();
    let transport = HttpTransport::new(FetchConfig::default());
    let cancel = CancelToken::default();

    for _ in 0..3 {
        let request = FetchRequest::get(url.clone()).with_header("Accept-Encoding", "gzip, br");
        let response = transport.fetch(&request, &cancel).expect("fetch");
        assert_eq!(
            response.body, b"hello\n",
            "the body must still decode correctly; only pooling is lost"
        );
    }

    assert_eq!(
        connections.load(Ordering::SeqCst),
        3,
        "brotli is expected to cost one connection per request in ureq 3.3"
    );
}
