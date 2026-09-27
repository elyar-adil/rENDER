//! Scratch connection-pool probe.
//!
//! Stands up a local origin that counts accepted connections, then fetches the
//! same URL three times under three content-coding negotiations and reports how
//! many connections each cost. This is how the brotli pooling defect in ureq
//! 3.3 was identified, and how a claim like "the pool is inert" can be checked
//! rather than believed. `REUSE_PROBE_TRACE=1` adds ureq's own
//! `Return to pool` / `Use pooled` lines.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use render_net::{BatchOptions, CancelToken, FetchConfig, FetchRequest, HttpTransport};

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

/// An origin that answers any request in the negotiated content coding, keeps
/// the connection alive, and counts how many connections it ever accepted.
fn spawn_negotiating_server() -> (SocketAddr, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local origin");
    let address = listener.local_addr().expect("read local address");
    let connections = Arc::new(AtomicUsize::new(0));
    let encodings = Arc::new(Mutex::new(Vec::new()));
    let counter = Arc::clone(&connections);
    let seen = Arc::clone(&encodings);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            counter.fetch_add(1, Ordering::SeqCst);
            let seen = Arc::clone(&seen);
            std::thread::spawn(move || {
                let _ = serve(stream, &seen);
            });
        }
    });
    (address, connections, encodings)
}

fn serve(mut stream: TcpStream, encodings: &Mutex<Vec<String>>) -> std::io::Result<()> {
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
        encodings
            .lock()
            .expect("recorded encodings")
            .push(accept_encoding.clone());
        let (body, encoding): (Vec<u8>, &str) = if accept_encoding.contains("br") {
            (BROTLI_HELLO.to_vec(), "br")
        } else if accept_encoding.contains("gzip") {
            (gzip_hello(), "gzip")
        } else {
            (b"hello\n".to_vec(), "identity")
        };
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\
             Content-Encoding: {encoding}\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes())?;
        stream.write_all(&body)?;
        stream.flush()?;
    }
}

/// A local origin serving `paths` distinct keep-alive resources, counting every
/// connection it accepts, so a page-shaped burst can be measured without
/// depending on anyone's network.
struct BurstOrigin {
    address: std::net::SocketAddr,
    connections: Arc<AtomicUsize>,
}

fn spawn_burst_origin() -> BurstOrigin {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind burst origin");
    let address = listener.local_addr().expect("read burst origin address");
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&connections);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            counter.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || {
                let _ignored = stream.set_read_timeout(Some(std::time::Duration::from_secs(30)));
                let mut buffer = Vec::new();
                let mut chunk = [0_u8; 1024];
                loop {
                    while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => return,
                            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
                        }
                    }
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nContent-Length: {ASSET_BYTES}\r\n\
                         Connection: keep-alive\r\n\r\n{}",
                        "a".repeat(ASSET_BYTES)
                    );
                    buffer.clear();
                    if stream.write_all(head.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    BurstOrigin {
        address,
        connections,
    }
}

/// Body size of one burst asset, chosen to match the ~6 KiB stylesheets in the
/// real-site evidence so the transfer cost is representative.
const ASSET_BYTES: usize = 6 * 1024;

/// Fetches `count` distinct paths from one origin in waves of `per_origin`,
/// the shape a page produces when it pulls many assets from one host over
/// HTTP/1.1. Prints wall clock and the number of connections the server saw.
fn measure_burst(per_host_idle: usize, total_idle: usize, count: usize, per_origin: usize) {
    let origin = spawn_burst_origin();
    let config = FetchConfig {
        idle_connections_per_origin: per_host_idle,
        idle_connections_total: total_idle,
        ..FetchConfig::default()
    };
    let transport = HttpTransport::new(config);
    let url: url::Url =
        url::Url::parse(&format!("http://{}/", origin.address)).expect("burst origin URL");
    let requests: Vec<FetchRequest> = (0..count)
        .map(|index| FetchRequest::get(url.join(&format!("asset-{index}.css")).expect("asset URL")))
        .collect();

    let started = std::time::Instant::now();
    let cancel = CancelToken::default();
    let results = transport.fetch_batch(requests, &BatchOptions::default(), &cancel);
    let elapsed = started.elapsed();
    let failures = results.iter().filter(|result| result.is_err()).count();

    println!(
        "idle/origin={per_host_idle:>2} idle/total={total_idle:>3} \
         assets={count} per-origin={per_origin} -> {:>8.1?} wall, {:>3} connections, \
         {failures} failures",
        elapsed,
        origin.connections.load(Ordering::SeqCst)
    );
}

/// Several origins, to show whether the *total* idle ceiling binds once the
/// per-origin one no longer does. A real page is one main origin plus CDNs.
fn measure_multi_origin(
    per_host_idle: usize,
    total_idle: usize,
    origins: usize,
    per_origin_each: usize,
) {
    let servers: Vec<BurstOrigin> = (0..origins).map(|_| spawn_burst_origin()).collect();
    let config = FetchConfig {
        idle_connections_per_origin: per_host_idle,
        idle_connections_total: total_idle,
        ..FetchConfig::default()
    };
    let transport = HttpTransport::new(config);
    let mut requests = Vec::new();
    for server in &servers {
        let url: url::Url =
            url::Url::parse(&format!("http://{}/", server.address)).expect("burst origin URL");
        for index in 0..per_origin_each {
            requests.push(FetchRequest::get(
                url.join(&format!("asset-{index}.css")).expect("asset URL"),
            ));
        }
    }

    let started = std::time::Instant::now();
    let results =
        transport.fetch_batch(requests, &BatchOptions::default(), &CancelToken::default());
    let elapsed = started.elapsed();
    let failures = results.iter().filter(|result| result.is_err()).count();
    let per_origin_connections: Vec<usize> = servers
        .iter()
        .map(|server| server.connections.load(Ordering::SeqCst))
        .collect();

    println!(
        "{origins} origins, idle/origin={per_host_idle:>2} idle/total={total_idle:>3} \
         -> {:>8.1?} wall, {:>3} connections {per_origin_connections:?}, {failures} failures",
        elapsed,
        per_origin_connections.iter().sum::<usize>()
    );
}

/// Prints ureq's connection-lifecycle lines only; the rest is noise here.
struct ScratchLogger;

impl log::Log for ScratchLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Debug
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let line = record.args().to_string();
            if line.contains("pool") || line.contains("Close") {
                eprintln!("  [ureq] {line}");
            }
        }
    }
    fn flush(&self) {}
}

fn main() {
    if std::env::var("REUSE_PROBE_TRACE").is_ok() {
        let _ = log::set_boxed_logger(Box::new(ScratchLogger));
        log::set_max_level(log::LevelFilter::Debug);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let per_origin = render_net::DEFAULT_PER_ORIGIN_CONCURRENCY;
    match args.first().map(String::as_str) {
        // A 40-asset single-origin page, the shape of every stylesheet corpus,
        // swept over the per-origin idle ceiling. The total is held high enough
        // that the per-origin value is the only thing varying.
        Some("burst") => {
            for per_host in 1..=(per_origin * 2) {
                measure_burst(per_host, per_origin * 8, 40, per_origin);
            }
        }
        // The same page with ureq 3.3's own defaults, then this crate's.
        Some("burst-compare") => {
            println!("ureq 3.3 defaults (3 per host, 10 total):");
            measure_burst(3, 10, 40, per_origin);
            println!(
                "this crate ({per_origin} per host, {} total):",
                per_origin * render_net::ORIGINS_BEFORE_TOTAL_CEILING
            );
            measure_burst(
                per_origin,
                per_origin * render_net::ORIGINS_BEFORE_TOTAL_CEILING,
                40,
                per_origin,
            );
        }
        // Whether the total idle ceiling binds once the per-origin one does not.
        Some("burst-origins") => {
            for total in [10, per_origin * render_net::ORIGINS_BEFORE_TOTAL_CEILING] {
                measure_multi_origin(per_origin, total, 3, 20);
            }
        }
        Some(other) => eprintln!("unknown mode '{other}'"),
        None => {}
    }
    if !args.is_empty() {
        return;
    }

    // `None` keeps the transport's own `Accept-Encoding`; `Some` overrides it
    // the way a caller-supplied header would.
    let cases: [(&str, Option<&str>); 3] = [
        ("gzip (what the transport advertises)", None),
        ("gzip, br", Some("gzip, br")),
        ("identity", Some("identity")),
    ];

    for (label, override_encoding) in cases {
        let (address, connections, encodings) = spawn_negotiating_server();
        let url = url::Url::parse(&format!("http://{address}/one")).expect("origin URL");
        let transport = HttpTransport::new(FetchConfig::default());
        let cancel = CancelToken::default();
        let mut bodies = Vec::new();
        for _ in 0..3 {
            let request = FetchRequest::get(url.clone());
            let request = match override_encoding {
                Some(value) => request.with_header("Accept-Encoding", value),
                None => request,
            };
            let outcome = transport.fetch(&request, &cancel);
            bodies.push(match outcome {
                Ok(response) => response.body.len(),
                Err(error) => {
                    eprintln!("  fetch failed: {error}");
                    0
                }
            });
        }
        println!(
            "{label:>40}: connections={} bodies={bodies:?} encodings={:?}",
            connections.load(Ordering::SeqCst),
            encodings.lock().expect("recorded encodings")
        );
    }
}
