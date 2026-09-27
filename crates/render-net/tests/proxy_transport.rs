//! Proxy routing: an explicitly configured proxy must carry requests, and a
//! `no_proxy` exclusion must bypass it.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use render_net::{CancelToken, FetchConfig, FetchRequest, HttpTransport};
use url::Url;

/// Origin server answering one fixed body for any request.
fn spawn_origin(body: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = Vec::new();
            let mut chunk = [0_u8; 512];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => request.extend_from_slice(&chunk[..count]),
                }
            }
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    address
}

/// CONNECT tunneling proxy: counts every accepted connection, answers each
/// `CONNECT host:port` with an established response, then relays raw bytes.
fn spawn_connect_proxy(hits: Arc<AtomicUsize>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            hits.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || relay_connect(stream));
        }
    });
    address
}

fn relay_connect(mut client: TcpStream) {
    let mut head = Vec::new();
    let mut chunk = [0_u8; 512];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        match client.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(count) => head.extend_from_slice(&chunk[..count]),
        }
    }
    let request_line = String::from_utf8_lossy(&head);
    let Some(target) = request_line
        .lines()
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|authority| authority.rsplit_once(':'))
    else {
        return;
    };
    let Ok(port) = target.1.parse::<u16>() else {
        return;
    };
    let Ok(mut origin) = TcpStream::connect((target.0, port)) else {
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
    let upstream = std::thread::spawn(move || std::io::copy(&mut client, &mut origin));
    let downstream = std::thread::spawn(move || std::io::copy(&mut origin_copy, &mut client_copy));
    let _ = upstream.join();
    let _ = downstream.join();
}

#[test]
fn explicit_proxy_carries_absolute_form_requests() {
    const BODY: &str = "proxied-body";
    let origin = spawn_origin(BODY);
    let hits = Arc::new(AtomicUsize::new(0));
    let proxy_addr = spawn_connect_proxy(Arc::clone(&hits));

    let proxy = ureq::Proxy::new(&format!("http://{proxy_addr}")).unwrap();
    let transport = HttpTransport::with_proxy(FetchConfig::default(), Some(proxy));
    let url = Url::parse(&format!("http://{origin}/resource")).unwrap();
    let response = transport
        .fetch(&FetchRequest::get(url), &CancelToken::default())
        .expect("proxied fetch");
    assert_eq!(response.status.as_u16(), 200);
    assert_eq!(response.body, BODY.as_bytes());
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "request must traverse the proxy"
    );
}

#[test]
fn no_proxy_exclusion_bypasses_the_proxy() {
    const BODY: &str = "direct-body";
    let origin = spawn_origin(BODY);
    let hits = Arc::new(AtomicUsize::new(0));
    let proxy_addr = spawn_connect_proxy(Arc::clone(&hits));

    let proxy = ureq::Proxy::builder(ureq::ProxyProtocol::Http)
        .host(&proxy_addr.ip().to_string())
        .port(proxy_addr.port())
        .no_proxy("127.0.0.1")
        .build()
        .unwrap();
    let transport = HttpTransport::with_proxy(FetchConfig::default(), Some(proxy));
    let url = Url::parse(&format!("http://{origin}/resource")).unwrap();
    let response = transport
        .fetch(&FetchRequest::get(url), &CancelToken::default())
        .expect("direct fetch");
    assert_eq!(response.status.as_u16(), 200);
    assert_eq!(response.body, BODY.as_bytes());
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "excluded hosts must not reach the proxy"
    );
}

#[test]
fn proxy_without_exclusion_still_carries_excluded_style_hosts() {
    const BODY: &str = "still-proxied";
    let origin = spawn_origin(BODY);
    let hits = Arc::new(AtomicUsize::new(0));
    let proxy_addr = spawn_connect_proxy(Arc::clone(&hits));

    let proxy = ureq::Proxy::new(&format!("http://{proxy_addr}")).unwrap();
    let transport = HttpTransport::with_proxy(FetchConfig::default(), Some(proxy));
    let url = Url::parse(&format!("http://{origin}/resource")).unwrap();
    let response = transport
        .fetch(&FetchRequest::get(url), &CancelToken::default())
        .expect("proxied fetch");
    assert_eq!(response.body, BODY.as_bytes());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}
