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

const LOCATION_FIRST: &str = r#"<!doctype html>
<html><head><title>first</title></head>
<body><p id="doc">first document</p>
<script>location.href = '/next';</script>
</body></html>"#;

const LOCATION_NEXT: &str = r#"<!doctype html>
<html><head><title>next</title></head>
<body><p id="doc">next document</p>
<script>document.getElementById('doc').textContent = 'second-script-ran';</script>
</body></html>"#;

const META_FIRST: &str = r#"<!doctype html>
<html><head><meta http-equiv="refresh" content="0; url=/next-meta"></head>
<body><p id="doc">meta refresh source</p></body></html>"#;

const META_NEXT: &str = r#"<!doctype html>
<html><head><title>meta target</title></head>
<body><p id="doc">meta target</p></body></html>"#;

const DOMCONTENTLOADED_FIRST: &str = r#"<!doctype html>
<html><body><p id="doc">loading</p>
<script>
document.addEventListener('DOMContentLoaded', function () {
  location.assign('/next-dcl');
});
</script></body></html>"#;

const DOMCONTENTLOADED_NEXT: &str = r#"<!doctype html>
<html><body><p id="doc">dcl target</p></body></html>"#;

const TIMER_NAV_FIRST: &str = r#"<!doctype html>
<html><body><p id="doc">timer source</p>
<script>setTimeout(function () { location.replace('/next-timer'); }, 0);</script>
</body></html>"#;

const TIMER_NAV_NEXT: &str = r#"<!doctype html>
<html><body><p id="doc">timer target</p></body></html>"#;

const FORM_FIRST: &str = r#"<!doctype html>
<html><body>
<form id="search" action="/next-form" method="get">
  <input name="q" value="rust">
</form>
<script>document.getElementById('search').submit();</script>
</body></html>"#;

const FORM_NEXT: &str = r#"<!doctype html>
<html><body><p id="doc">form target</p></body></html>"#;

const OPEN_FIRST: &str = r#"<!doctype html>
<html><body><p id="doc">open source</p>
<script>window.open('/next-open', '_self');</script>
</body></html>"#;

const OPEN_NEXT: &str = r#"<!doctype html>
<html><body><p id="doc">open target</p></body></html>"#;

const META_LOOP: &str = r#"<!doctype html>
<html><head><meta http-equiv="refresh" content="0; url=/loop"></head>
<body><p id="doc">loop</p></body></html>"#;

const SLOW_TIMER: &str = r#"<!doctype html>
<html><body><p id="out">before</p>
<script>
setTimeout(function () {
  document.getElementById('out').textContent = 'after-two-seconds';
}, 2000);
</script></body></html>"#;

const IMAGE_AFTER_DEADLINE: &str = r#"<!doctype html>
<html><body style="margin:0">
<img id="dot" src="/dot.svg" width="16" height="16">
<script>
var start = Date.now();
while (Date.now() - start < 300) {}
</script>
</body></html>"#;

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

fn png_pixel(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let image = image_codec::load_from_memory_with_format(bytes, image_codec::ImageFormat::Png)
        .expect("output decodes as PNG");
    let rgba = image.to_rgba8();
    rgba.get_pixel(x, y).0
}

fn health_number(json: &str, key: &str) -> Option<u64> {
    let marker = format!("\"{key}\":");
    json.split(&marker)
        .nth(1)?
        .split([',', '}'])
        .next()?
        .trim()
        .parse()
        .ok()
}

struct FixtureRun {
    stdout: String,
    stderr: String,
    dom: String,
    png: Vec<u8>,
    directory: PathBuf,
}

fn run_fixture(
    name: &str,
    routes: HashMap<&'static str, Response>,
    path: &str,
    wait_ms: u64,
) -> FixtureRun {
    let server = TestServer::start(routes);
    let directory = scratch_directory(name);
    let png_path = directory.join("page.png");
    let dom_path = directory.join("page.html");
    let base = server.url();
    let url = if path == "/" {
        base
    } else {
        format!("{}{path}", base.trim_end_matches('/'))
    };
    let output = Command::new(env!("CARGO_BIN_EXE_render-headless"))
        .args([
            "--url",
            &url,
            "--out",
            png_path.to_str().expect("png path"),
            "--width",
            "400",
            "--height",
            "300",
            "--wait-ms",
            &wait_ms.to_string(),
            "--dump-dom",
            dom_path.to_str().expect("dom path"),
            "--print-health",
        ])
        .output()
        .expect("run render-headless");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "render-headless failed for {url}:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    FixtureRun {
        dom: fs::read_to_string(&dom_path).expect("read DOM dump"),
        png: fs::read(&png_path).expect("read PNG"),
        stdout,
        stderr,
        directory,
    }
}

impl FixtureRun {
    fn finish(self) {
        let _ = fs::remove_dir_all(self.directory);
    }
}

#[test]
fn headless_follows_script_location_navigation_and_resets_document_state() {
    let run = run_fixture(
        "location-nav",
        HashMap::from([
            (
                "/first",
                Response::ok("text/html; charset=utf-8", LOCATION_FIRST.as_bytes()),
            ),
            (
                "/next",
                Response::ok("text/html; charset=utf-8", LOCATION_NEXT.as_bytes()),
            ),
        ]),
        "/first",
        5_000,
    );
    assert!(
        run.dom.contains("<p id=\"doc\">second-script-ran</p>"),
        "the new document's own script must run after a navigation; DOM:\n{}",
        run.dom
    );
    assert!(
        !run.dom.contains("first document"),
        "the abandoned document must not remain in the frame; DOM:\n{}",
        run.dom
    );
    assert!(
        run.stdout.contains("\"navigations\":1"),
        "health: {}",
        run.stdout
    );
    assert!(
        run.stdout.contains("\"final_url\":\"http://127.0.0.1:") && run.stdout.contains("/next\""),
        "final URL must be the navigated document: {}",
        run.stdout
    );
    assert!(
        run.stdout.contains("\"timed_out\":false"),
        "health: {}",
        run.stdout
    );
    run.finish();
}

#[test]
fn headless_follows_meta_refresh_navigation() {
    let run = run_fixture(
        "meta-nav",
        HashMap::from([
            (
                "/meta",
                Response::ok("text/html; charset=utf-8", META_FIRST.as_bytes()),
            ),
            (
                "/next-meta",
                Response::ok("text/html; charset=utf-8", META_NEXT.as_bytes()),
            ),
        ]),
        "/meta",
        5_000,
    );
    assert!(
        run.dom.contains("meta target"),
        "meta refresh must load its target; DOM:\n{}",
        run.dom
    );
    assert!(
        run.stdout.contains("\"navigations\":1") && run.stdout.contains("/next-meta\""),
        "health: {}",
        run.stdout
    );
    run.finish();
}

#[test]
fn headless_follows_domcontentloaded_and_timer_navigation() {
    let dcl = run_fixture(
        "dcl-nav",
        HashMap::from([
            (
                "/dcl",
                Response::ok(
                    "text/html; charset=utf-8",
                    DOMCONTENTLOADED_FIRST.as_bytes(),
                ),
            ),
            (
                "/next-dcl",
                Response::ok("text/html; charset=utf-8", DOMCONTENTLOADED_NEXT.as_bytes()),
            ),
        ]),
        "/dcl",
        5_000,
    );
    assert!(
        dcl.dom.contains("dcl target") && dcl.stdout.contains("/next-dcl\""),
        "DOMContentLoaded navigation must commit:\nDOM:\n{}\nhealth:\n{}",
        dcl.dom,
        dcl.stdout
    );
    dcl.finish();

    let timer = run_fixture(
        "timer-nav",
        HashMap::from([
            (
                "/timer",
                Response::ok("text/html; charset=utf-8", TIMER_NAV_FIRST.as_bytes()),
            ),
            (
                "/next-timer",
                Response::ok("text/html; charset=utf-8", TIMER_NAV_NEXT.as_bytes()),
            ),
        ]),
        "/timer",
        5_000,
    );
    assert!(
        timer.dom.contains("timer target") && timer.stdout.contains("/next-timer\""),
        "timer navigation must commit:\nDOM:\n{}\nhealth:\n{}",
        timer.dom,
        timer.stdout
    );
    timer.finish();
}

#[test]
fn headless_follows_form_submit_and_same_tab_window_open() {
    let form = run_fixture(
        "form-nav",
        HashMap::from([
            (
                "/form",
                Response::ok("text/html; charset=utf-8", FORM_FIRST.as_bytes()),
            ),
            (
                "/next-form?q=rust",
                Response::ok("text/html; charset=utf-8", FORM_NEXT.as_bytes()),
            ),
        ]),
        "/form",
        5_000,
    );
    assert!(
        form.dom.contains("form target") && form.stdout.contains("/next-form?q=rust\""),
        "form.submit navigation must commit the encoded query:\nDOM:\n{}\nhealth:\n{}",
        form.dom,
        form.stdout
    );
    form.finish();

    let opened = run_fixture(
        "open-nav",
        HashMap::from([
            (
                "/open",
                Response::ok("text/html; charset=utf-8", OPEN_FIRST.as_bytes()),
            ),
            (
                "/next-open",
                Response::ok("text/html; charset=utf-8", OPEN_NEXT.as_bytes()),
            ),
        ]),
        "/open",
        5_000,
    );
    assert!(
        opened.dom.contains("open target") && opened.stdout.contains("/next-open\""),
        "same-tab window.open must navigate:\nDOM:\n{}\nhealth:\n{}",
        opened.dom,
        opened.stdout
    );
    opened.finish();
}

#[test]
fn headless_bounds_meta_refresh_redirects() {
    let run = run_fixture(
        "meta-loop",
        HashMap::from([(
            "/loop",
            Response::ok("text/html; charset=utf-8", META_LOOP.as_bytes()),
        )]),
        "/loop",
        5_000,
    );
    assert!(
        run.stdout.contains("\"navigations\":10"),
        "the runner must stop at ten followed redirects: {}",
        run.stdout
    );
    assert!(
        run.stderr.contains("navigation limit 10 reached"),
        "the limit must be reported: {}",
        run.stderr
    );
    run.finish();
}

#[test]
fn headless_runs_a_timer_that_is_due_before_the_wait_deadline() {
    let run = run_fixture(
        "slow-timer",
        HashMap::from([(
            "/timer",
            Response::ok("text/html; charset=utf-8", SLOW_TIMER.as_bytes()),
        )]),
        "/timer",
        5_000,
    );
    assert!(
        run.dom.contains("<p id=\"out\">after-two-seconds</p>"),
        "a 2s timer inside the 5s budget must run before the final frame; DOM:\n{}",
        run.dom
    );
    assert!(
        run.stdout.contains("\"timed_out\":false"),
        "health: {}",
        run.stdout
    );
    assert!(
        health_number(&run.stdout, "elapsed_ms").is_some_and(|millis| millis >= 1_900),
        "the runner must actually wait for the timer: {}",
        run.stdout
    );
    run.finish();
}

#[test]
fn headless_final_pass_paints_an_image_discovered_after_timeout() {
    let run = run_fixture(
        "image-final-pass",
        HashMap::from([
            (
                "/slow",
                Response::ok("text/html; charset=utf-8", IMAGE_AFTER_DEADLINE.as_bytes()),
            ),
            ("/dot.svg", Response::ok("image/svg+xml", IMAGE.as_bytes())),
        ]),
        "/slow",
        150,
    );
    assert!(
        run.stdout.contains("\"timed_out\":true"),
        "the fixture must exercise the timed-out finalization path: {}",
        run.stdout
    );
    assert!(
        run.stdout.contains("\"images_loaded\":1"),
        "the final pass must load the image: {}",
        run.stdout
    );
    assert_eq!(
        png_pixel(&run.png, 2, 2),
        [0x00, 0xaa, 0x00, 0xff],
        "the final frame must paint the image fetched by the final apply pass"
    );
    run.finish();
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
