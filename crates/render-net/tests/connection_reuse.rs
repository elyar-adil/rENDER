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
