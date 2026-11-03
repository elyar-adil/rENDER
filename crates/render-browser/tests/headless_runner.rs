//! End-to-end test for the offscreen page runner.
//!
//! The fixture is served by a loopback HTTP server, so the same run proves
//! navigation, external CSS, an external classic script, a script-issued
//! `fetch()`, and an external image. The runner is executed as a process; the
//! assertions read the PNG header and the dumped DOM, so nothing inspects an
//! internal shortcut instead of the delivered artifact.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PAGE: &str = r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<title>headless fixture</title>
<link rel="stylesheet" href="/style.css">
</head>
<body>
<h1 id="title">headless fixture</h1>
<p id="status">no-js</p>
<img id="dot" src="/dot.svg" width="8" height="8">
<div id="box"></div>
<script src="/app.js"></script>
</body>
</html>"#;

const STYLE: &str = r"
@font-face { font-family: MissingFace; src: url('/missing.woff2'); }
html, body { margin: 0; padding: 0; }
#box { position: absolute; left: 10px; top: 100px; width: 40px; height: 20px; background-color: #123456; }
#title { color: #aa0000; font-size: 20px; font-family: MissingFace; }
";

const SCRIPT: &str = r"
document.getElementById('status').textContent = 'js-start';
fetch('/data.json').then(function (response) { return response.text(); }).then(function (text) {
  var value = JSON.parse(text).value;
  document.getElementById('status').textContent = 'js-' + value;
});
";

const DATA: &str = r#"{"value":"ok"}"#;

const IMAGE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><rect x="0" y="0" width="8" height="8" fill="#00aa00"/></svg>"##;

struct Response {
    status: &'static str,
    content_type: &'static str,
    body: &'static [u8],
}

impl Response {
    const fn ok(content_type: &'static str, body: &'static [u8]) -> Self {
        Self {
            status: "200 OK",
            content_type,
            body,
        }
    }
}

struct TestServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl TestServer {
    fn start(routes: HashMap<&'static str, Response>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
        let address = listener.local_addr().expect("fixture address");
        listener
            .set_nonblocking(true)
            .expect("fixture listener non-blocking");
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => serve(stream, &routes),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            address,
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self) -> String {
        format!("http://{}/", self.address)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(mut stream: TcpStream, routes: &HashMap<&'static str, Response>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1_024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => request.extend_from_slice(&buffer[..count]),
        }
    }
    let path = request
        .split(|byte| *byte == b'\n')
        .next()
        .and_then(|line| std::str::from_utf8(line).ok())
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_owned();
    let response = routes.get(path.as_str()).unwrap_or(&Response {
        status: "404 Not Found",
        content_type: "text/plain",
        body: b"not found",
    });
    let headers = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        response.content_type,
        response.body.len()
    );
    let _ = stream.write_all(headers.as_bytes());
    let _ = stream.write_all(response.body);
}

fn fixture_routes() -> HashMap<&'static str, Response> {
    HashMap::from([
        (
            "/",
            Response::ok("text/html; charset=utf-8", PAGE.as_bytes()),
        ),
        ("/style.css", Response::ok("text/css", STYLE.as_bytes())),
        (
            "/app.js",
            Response::ok("text/javascript", SCRIPT.as_bytes()),
        ),
        (
            "/data.json",
            Response::ok("application/json", DATA.as_bytes()),
        ),
        ("/dot.svg", Response::ok("image/svg+xml", IMAGE.as_bytes())),
    ])
}

fn scratch_directory(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "render-headless-{name}-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("scratch directory");
    path
}

fn png_dimensions(bytes: &[u8]) -> (u32, u32) {
    assert!(
        bytes.len() > 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "output is a PNG"
    );
    assert_eq!(&bytes[12..16], b"IHDR", "PNG starts with an IHDR chunk");
    let width = u32::from_be_bytes(bytes[16..20].try_into().expect("width bytes"));
    let height = u32::from_be_bytes(bytes[20..24].try_into().expect("height bytes"));
    (width, height)
}

#[test]
fn headless_fixture_loads_css_scripts_fetch_and_image_at_device_scale() {
    let server = TestServer::start(fixture_routes());
    let directory = scratch_directory("fixture");
    let png = directory.join("page.png");
    let dom = directory.join("page.html");

    let output = Command::new(env!("CARGO_BIN_EXE_render-headless"))
        .args([
            "--url",
            &server.url(),
            "--out",
            png.to_str().expect("png path"),
            "--width",
            "400",
            "--height",
            "300",
            "--dpr",
            "2",
            "--wait-ms",
            "5000",
            "--dump-dom",
            dom.to_str().expect("dom path"),
            "--print-health",
        ])
        .output()
        .expect("run render-headless");

    assert!(
        output.status.success(),
        "render-headless failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let png_bytes = fs::read(&png).expect("read PNG");
    assert_eq!(
        png_dimensions(&png_bytes),
        (800, 600),
        "CSS viewport 400x300 at dpr 2 is 800x600 device pixels"
    );

    let serialized = fs::read_to_string(&dom).expect("read DOM dump");
    assert!(
        serialized.contains("js-ok"),
        "the external script and its fetch() have run; DOM was:\n{serialized}"
    );

    let health = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "\"ready_state\":\"complete\"",
        "\"surface\":{\"width\":800,\"height\":600}",
        "\"css_errors\":0",
        "\"scripts_failed\":0",
        "\"scripts_threw\":0",
        "\"images_failed\":0",
        "\"images_loaded\":1",
        "\"stylesheets_loaded\":1",
        "\"fonts_loaded\":0",
        "\"font_failures\":1",
        "\"page_fetches\":1",
        "\"timed_out\":false",
        "\"id\":\"box\"",
        "\"x\":10.000",
        "\"y\":100.000",
        "\"width\":40.000",
        "\"height\":20.000",
    ] {
        assert!(
            health.contains(expected),
            "health JSON lacks {expected}:\n{health}"
        );
    }

    let _ = fs::remove_dir_all(directory);
}

#[test]
fn headless_bin_and_session_have_no_native_window_dependency() {
    let sources = [
        include_str!("../src/bin/render-headless.rs"),
        include_str!("../src/headless.rs"),
    ];
    for source in sources {
        let lower = source.to_ascii_lowercase();
        for forbidden in [
            "winit::",
            "softbuffer",
            "wgpu",
            "nswindow",
            "nsapplication",
            "appkit",
            "platform_presenter",
            "gpu_presenter",
            "arboard",
        ] {
            assert!(
                !lower.contains(forbidden),
                "headless source must not reference {forbidden}"
            );
        }
    }
}
