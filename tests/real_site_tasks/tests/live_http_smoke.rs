use std::time::Duration;

use render_net::{CancelToken, FetchConfig, FetchError, FetchRequest, HttpTransport, Url};

#[test]
#[ignore = "requires Internet; network-unavailable errors are reported as skips"]
fn baidu_live_http_smoke() {
    smoke_url("https://www.baidu.com/");
}

#[test]
#[ignore = "requires Internet; network-unavailable errors are reported as skips"]
fn zhihu_live_http_smoke() {
    smoke_url("https://www.zhihu.com/");
}

#[test]
#[ignore = "requires Internet; network-unavailable errors are reported as skips"]
fn netease_live_http_smoke() {
    smoke_url("https://www.163.com/");
}

fn smoke_url(raw_url: &str) {
    let mut config = FetchConfig {
        timeout: Duration::from_secs(12),
        max_body_bytes: 8 * 1024 * 1024,
        ..FetchConfig::default()
    };
    config.user_agent.push_str(" offline-smoke");
    let transport = HttpTransport::new(config);
    let url = Url::parse(raw_url).expect("smoke URL");
    let request = FetchRequest::get(url.clone())
        .with_accept("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8");

    let response = match transport.fetch(&request, &CancelToken::default()) {
        Ok(response) => response,
        Err(error) if is_network_unavailable(&error) => {
            eprintln!("live HTTP smoke skipped for {url}: {error}");
            return;
        }
        Err(error) => panic!("live HTTP request failed for {url}: {error}"),
    };

    assert!(
        response.status.is_success(),
        "live HTTP response for {} was {}",
        response.final_url,
        response.status.as_u16()
    );
    assert!(
        !response.body.is_empty(),
        "live HTTP body for {url} is empty"
    );

    let content_type_is_html = response.content_type.as_ref().is_some_and(|content_type| {
        matches!(
            content_type.media_type.as_str(),
            "text/html" | "application/xhtml+xml"
        )
    });
    let body_looks_like_html = response
        .body
        .windows(5)
        .any(|window| window.eq_ignore_ascii_case(b"<html"))
        || response
            .body
            .windows(9)
            .any(|window| window.eq_ignore_ascii_case(b"<!doctype"));
    assert!(
        content_type_is_html || body_looks_like_html,
        "live HTTP response for {} is not HTML (content type: {:?})",
        response.final_url,
        response.content_type
    );
}

fn is_network_unavailable(error: &FetchError) -> bool {
    match error {
        FetchError::Dns
        | FetchError::Timeout
        | FetchError::Tls(_)
        | FetchError::Io(_)
        | FetchError::Transport(_) => true,
        _ => false,
    }
}
