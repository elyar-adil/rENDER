//! Explicitly opt-in HTTP smoke against the three live origins.
//!
//! This is the only check in the acceptance harness that can touch the network,
//! and it is `#[ignore]`d so a default `cargo test` run stays offline. Run it by
//! name:
//!
//! ```text
//! cargo test --manifest-path tests/real_site_tasks/Cargo.toml --test live_http_smoke -- --ignored --nocapture
//! ```
//!
//! What it asserts, and what it deliberately does not:
//!
//! * A DNS failure, a timeout, a TLS failure, or any other
//!   network-availability error is a **skip**: an unavailable network must not
//!   become an ordinary test failure, and the reason is printed.
//! * A reachable endpoint that answers non-2xx, with an empty body, or with a
//!   non-HTML content type **fails**. That is a real regression signal.
//! * It does not assert page contents, layout, or rendering. Those are what the
//!   offline fixtures are for. This test only answers "can the transport still
//!   reach the origin and get a page back".
//!
//! The endpoint list is the base URL of each fixture whose page origin is worth
//! keeping reachable, plus the search endpoint. Site names appear only as URLs
//! handed to the transport; nothing here decides behaviour based on them.

use std::sync::Arc;
use std::time::Duration;

use render_net::{
    CancelToken, FetchConfig, FetchError, FetchRequest, HttpTransport, NullObserver, Url,
};

/// The origins the offline fixtures model, plus one search endpoint on the
/// Baidu origin.
///
/// These are deliberately *origins and their search endpoints*, not article or
/// channel URLs. A made-up content URL is not a transport question: an article
/// id that does not exist answers 403, which says nothing about whether the
/// transport still works, and treating that as a failure would only make this
/// test rot. What matters here is that the origin still answers at all.
const ENDPOINTS: &[(&str, &str)] = &[
    ("baidu_home", "https://www.baidu.com/"),
    (
        "baidu_results",
        "https://www.baidu.com/s?wd=%E6%B5%8F%E8%A7%88%E5%99%A8",
    ),
    ("zhihu_home", "https://www.zhihu.com/"),
    ("netease_163_home", "https://www.163.com/"),
];

#[test]
#[ignore = "explicitly opt-in network check; run with --ignored"]
fn the_modelled_origins_answer_with_a_2xx_html_document() {
    let config = FetchConfig {
        timeout: Duration::from_secs(20),
        connect_timeout: Duration::from_secs(5),
        observer: Arc::new(NullObserver),
        ..FetchConfig::default()
    };
    let transport = HttpTransport::new(config);

    let mut skipped = Vec::new();
    let mut failures = Vec::new();

    for (label, endpoint) in ENDPOINTS {
        let url = Url::parse(endpoint)
            .unwrap_or_else(|error| panic!("{label}: {endpoint} is not a URL: {error}"));
        let request = FetchRequest::get(url).with_accept("text/html,application/xhtml+xml");
        match transport.fetch(&request, &CancelToken::default()) {
            Ok(response) => {
                let media_type = response
                    .content_type
                    .as_ref()
                    .map_or("", |content_type| content_type.media_type.as_str());
                if !response.status.is_success() {
                    failures.push(format!(
                        "{label}: {} answered {}",
                        endpoint,
                        response.status.as_u16()
                    ));
                } else if response.body.is_empty() {
                    failures.push(format!("{label}: {endpoint} answered an empty body"));
                } else if !media_type.starts_with("text/html") {
                    failures.push(format!(
                        "{label}: {endpoint} answered content type {media_type:?}"
                    ));
                } else {
                    println!(
                        "{label}: {} {} {media_type:?} {} bytes",
                        endpoint,
                        response.status.as_u16(),
                        response.body.len()
                    );
                }
            }
            Err(error) if is_network_availability(&error) => {
                println!("{label}: skipped, the network is unavailable: {error}");
                skipped.push(*label);
            }
            Err(error) => failures.push(format!("{label}: {endpoint} failed: {error}")),
        }
    }

    if !skipped.is_empty() {
        println!("skipped as unavailable: {}", skipped.join(", "));
    }
    assert!(
        failures.is_empty(),
        "live HTTP smoke failures:\n  {}",
        failures.join("\n  ")
    );
}

/// Whether a transport failure means "there is no network", rather than
/// "something is wrong with the request or the origin".
fn is_network_availability(error: &FetchError) -> bool {
    matches!(
        error.clone().into_inner(),
        FetchError::Dns
            | FetchError::Timeout
            | FetchError::Tls(_)
            | FetchError::Io(_)
            | FetchError::Transport(_)
            | FetchError::WorkerStopped
    )
}
