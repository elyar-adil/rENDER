//! Cross-origin rules for page-initiated requests (`fetch()` and XHR).
//!
//! A page reaches another origin only through the CORS protocol (Fetch
//! standard, section 3.2 and 4.8). This module holds the protocol's decisions
//! as functions over headers, so they can be tested without a network. The
//! shell applies them: it sends a preflight before a request that is not
//! simple, refuses a response the page's origin may not read, and exposes only
//! the response headers the page may see.
//!
//! Credentials: `fetch()` and XHR have no credentials mode yet
//! (`withCredentials` is always false), so a cross-origin request never carries
//! cookies. That is the protocol's "omit" mode, and under it the wildcard
//! `Access-Control-Allow-Origin: *` is allowed.
//!
//! Redirects: the transport follows them and these checks run on the final
//! response. The standard checks every hop, so a redirect that crosses origins
//! is judged more leniently here than the standard requires.

use render_net::{FetchRequest, Header, HttpMethod, Url};

/// What a page's promise rejects with when CORS refuses a response.
pub(super) const BLOCKED_MESSAGE: &str = "network: blocked by the CORS policy";

/// Request headers that pages cannot set (Fetch standard, section 2.2.2). They
/// are dropped rather than sent, so a page cannot attach its own cookies or
/// spoof the origin it is running in.
pub(super) fn is_forbidden_request_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    if name.starts_with("proxy-") || name.starts_with("sec-") {
        return true;
    }
    matches!(
        name.as_str(),
        "accept-charset"
            | "accept-encoding"
            | "access-control-request-headers"
            | "access-control-request-method"
            | "connection"
            | "content-length"
            | "cookie"
            | "cookie2"
            | "date"
            | "dnt"
            | "expect"
            | "host"
            | "keep-alive"
            | "origin"
            | "referer"
            | "set-cookie"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "via"
    )
}

/// The request headers a page may send, in the order it set them.
pub(super) fn author_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(name, _)| !is_forbidden_request_header(name))
        .cloned()
        .collect()
}

/// Whether `request` leaves the page's origin. An opaque origin, such as a
/// `file:` document, is never the same origin as anything, including itself.
pub(super) fn is_cross_origin(page: &Url, request: &Url) -> bool {
    page.origin() != request.origin()
}

/// The page's origin as the `Origin` header writes it. An opaque origin is
/// `null`.
pub(super) fn page_origin(page: &Url) -> String {
    page.origin().ascii_serialization()
}

/// Whether a request is "simple" (Fetch standard, section 2.2.2): one a browser
/// sends without a preflight. Such a request uses GET, HEAD or POST and only
/// headers whose values a cross-origin form could also have sent.
pub(super) fn is_simple(method: HttpMethod, headers: &[(String, String)]) -> bool {
    matches!(
        method,
        HttpMethod::Get | HttpMethod::Head | HttpMethod::Post
    ) && headers
        .iter()
        .all(|(name, value)| is_safelisted_request_header(name, value))
}

fn is_safelisted_request_header(name: &str, value: &str) -> bool {
    match name.to_ascii_lowercase().as_str() {
        "accept" | "accept-language" | "content-language" => true,
        "content-type" => matches!(
            media_type(value).as_str(),
            "text/plain" | "multipart/form-data" | "application/x-www-form-urlencoded"
        ),
        _ => false,
    }
}

fn media_type(value: &str) -> String {
    value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// The lowercase names of the headers that make a request non-simple, sorted and
/// without duplicates. They are the `Access-Control-Request-Headers` value.
fn preflight_header_names(headers: &[(String, String)]) -> Vec<String> {
    let mut names: Vec<String> = headers
        .iter()
        .filter(|(name, value)| !is_safelisted_request_header(name, value))
        .map(|(name, _)| name.to_ascii_lowercase())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The preflight for a cross-origin request that is not simple (Fetch standard,
/// section 4.8). It asks the target whether the origin, method and headers are
/// allowed, and carries no cookies and no body.
pub(super) fn preflight_request(
    url: &Url,
    origin: &str,
    method: HttpMethod,
    headers: &[(String, String)],
) -> FetchRequest {
    let mut request = FetchRequest::new(HttpMethod::Options, url.clone())
        .with_header("Origin", origin)
        .with_header("Access-Control-Request-Method", method.as_str());
    let names = preflight_header_names(headers);
    if !names.is_empty() {
        request = request.with_header("Access-Control-Request-Headers", names.join(","));
    }
    request
}

/// Whether a preflight response lets the actual request go ahead.
pub(super) fn preflight_allows(
    status: u16,
    headers: &[Header],
    origin: &str,
    method: HttpMethod,
    request_headers: &[(String, String)],
) -> bool {
    if !(200..300).contains(&status) || !allows_origin(headers, origin) {
        return false;
    }
    let method_allowed = matches!(
        method,
        HttpMethod::Get | HttpMethod::Head | HttpMethod::Post
    ) || list_allows(
        headers,
        "access-control-allow-methods",
        method.as_str(),
        false,
    );
    method_allowed
        && preflight_header_names(request_headers)
            .iter()
            .all(|name| list_allows(headers, "access-control-allow-headers", name, true))
}

/// Whether a response to a cross-origin request may be read by the page.
pub(super) fn response_allows(headers: &[Header], origin: &str) -> bool {
    allows_origin(headers, origin)
}

/// The response headers a page may read. A same-origin response exposes all of
/// its headers. A cross-origin one exposes the CORS-safelisted set, plus what
/// `Access-Control-Expose-Headers` names.
pub(super) fn exposed_headers(headers: &[Header], cross_origin: bool) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|header| {
            !cross_origin
                || is_safelisted_response_header(&header.name)
                || list_allows(headers, "access-control-expose-headers", &header.name, true)
        })
        .map(|header| {
            (
                header.name.clone(),
                String::from_utf8_lossy(&header.value).into_owned(),
            )
        })
        .collect()
}

fn is_safelisted_response_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "cache-control"
            | "content-language"
            | "content-length"
            | "content-type"
            | "expires"
            | "last-modified"
            | "pragma"
    )
}

/// `Access-Control-Allow-Origin` must be present exactly once and name either
/// the page's origin or `*`. A response that names no origin, or names two,
/// is refused.
fn allows_origin(headers: &[Header], origin: &str) -> bool {
    let values: Vec<String> = headers
        .iter()
        .filter(|header| {
            header
                .name
                .eq_ignore_ascii_case("access-control-allow-origin")
        })
        .map(|header| String::from_utf8_lossy(&header.value).trim().to_owned())
        .collect();
    matches!(values.as_slice(), [value] if value == "*" || value == origin)
}

/// Whether a comma-separated header list allows `item`. Header names compare
/// case-insensitively and methods exactly. `*` allows any item, except that it
/// never covers `authorization`, which the standard keeps out of the wildcard.
fn list_allows(headers: &[Header], list_name: &str, item: &str, case_insensitive: bool) -> bool {
    let same = |candidate: &str| {
        if case_insensitive {
            candidate.eq_ignore_ascii_case(item)
        } else {
            candidate == item
        }
    };
    let mut wildcard = false;
    for header in headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case(list_name))
    {
        for candidate in String::from_utf8_lossy(&header.value)
            .split(',')
            .map(str::trim)
        {
            if candidate == "*" {
                wildcard = true;
            } else if same(candidate) {
                return true;
            }
        }
    }
    wildcard && !(case_insensitive && item.eq_ignore_ascii_case("authorization"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(name: &str, value: &str) -> Header {
        Header {
            name: name.to_owned(),
            value: value.as_bytes().to_vec(),
        }
    }

    fn pairs(values: &[(&str, &str)]) -> Vec<(String, String)> {
        values
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn origins_compare_by_scheme_host_and_port() {
        let page = Url::parse("https://app.example.test/home").expect("page");
        assert!(!is_cross_origin(
            &page,
            &Url::parse("https://app.example.test:443/api").expect("same")
        ));
        assert!(is_cross_origin(
            &page,
            &Url::parse("http://app.example.test/api").expect("other scheme")
        ));
        assert!(is_cross_origin(
            &page,
            &Url::parse("https://app.example.test:8443/api").expect("other port")
        ));
        assert!(is_cross_origin(
            &page,
            &Url::parse("https://api.example.test/api").expect("other host")
        ));
        let file_page = Url::parse("file:///tmp/index.html").expect("file page");
        assert!(is_cross_origin(&file_page, &file_page));
        assert_eq!(page_origin(&file_page), "null");
    }

    #[test]
    fn only_safelisted_methods_and_headers_are_simple() {
        assert!(is_simple(HttpMethod::Get, &[]));
        assert!(is_simple(
            HttpMethod::Post,
            &pairs(&[("Content-Type", "text/plain; charset=utf-8")])
        ));
        assert!(is_simple(
            HttpMethod::Get,
            &pairs(&[("Accept", "application/json")])
        ));
        assert!(!is_simple(HttpMethod::Put, &[]));
        assert!(!is_simple(HttpMethod::Options, &[]));
        assert!(!is_simple(
            HttpMethod::Post,
            &pairs(&[("Content-Type", "application/json")])
        ));
        assert!(!is_simple(HttpMethod::Get, &pairs(&[("X-Token", "t")])));
    }

    #[test]
    fn the_preflight_names_each_non_simple_header_once_in_order() {
        let url = Url::parse("https://api.example.test/items").expect("url");
        let headers = pairs(&[
            ("X-Token", "t"),
            ("content-type", "application/json"),
            ("x-token", "again"),
            ("Accept", "*/*"),
        ]);
        let preflight =
            preflight_request(&url, "https://app.example.test", HttpMethod::Put, &headers);
        assert_eq!(preflight.method, HttpMethod::Options);
        assert!(preflight.body.is_none());
        assert_eq!(preflight.cookie, None);
        let names = preflight
            .headers
            .iter()
            .find(|(name, _)| name == "Access-Control-Request-Headers")
            .map(|(_, value)| value.as_str());
        assert_eq!(names, Some("content-type,x-token"));
    }

    #[test]
    fn a_preflight_must_allow_the_origin_method_and_headers() {
        let origin = "https://app.example.test";
        let headers = pairs(&[("X-Token", "t")]);
        let allowed = [
            header("Access-Control-Allow-Origin", origin),
            header("Access-Control-Allow-Methods", "GET, PUT"),
            header("Access-Control-Allow-Headers", "X-Token"),
        ];
        assert!(preflight_allows(
            204,
            &allowed,
            origin,
            HttpMethod::Put,
            &headers
        ));

        assert!(!preflight_allows(
            204,
            &[header("Access-Control-Allow-Origin", "https://evil.test")],
            origin,
            HttpMethod::Put,
            &headers
        ));
        assert!(!preflight_allows(
            403,
            &allowed,
            origin,
            HttpMethod::Put,
            &headers
        ));
        assert!(!preflight_allows(
            204,
            &[
                header("Access-Control-Allow-Origin", origin),
                header("Access-Control-Allow-Methods", "GET"),
                header("Access-Control-Allow-Headers", "X-Token"),
            ],
            origin,
            HttpMethod::Put,
            &headers
        ));
        assert!(!preflight_allows(
            204,
            &[
                header("Access-Control-Allow-Origin", origin),
                header("Access-Control-Allow-Methods", "PUT"),
            ],
            origin,
            HttpMethod::Put,
            &headers
        ));
    }

    #[test]
    fn a_wildcard_allows_headers_but_never_authorization() {
        let origin = "https://app.example.test";
        let wildcard = [
            header("Access-Control-Allow-Origin", "*"),
            header("Access-Control-Allow-Methods", "*"),
            header("Access-Control-Allow-Headers", "*"),
        ];
        assert!(preflight_allows(
            200,
            &wildcard,
            origin,
            HttpMethod::Delete,
            &pairs(&[("X-Token", "t")])
        ));
        assert!(!preflight_allows(
            200,
            &wildcard,
            origin,
            HttpMethod::Delete,
            &pairs(&[("Authorization", "Bearer t")])
        ));
    }

    #[test]
    fn a_response_names_exactly_one_allowed_origin() {
        let origin = "https://app.example.test";
        assert!(response_allows(
            &[header("Access-Control-Allow-Origin", origin)],
            origin
        ));
        assert!(response_allows(
            &[header("access-control-allow-origin", "*")],
            origin
        ));
        assert!(!response_allows(&[], origin));
        assert!(!response_allows(
            &[header("Access-Control-Allow-Origin", "https://evil.test")],
            origin
        ));
        assert!(!response_allows(
            &[
                header("Access-Control-Allow-Origin", origin),
                header("Access-Control-Allow-Origin", origin),
            ],
            origin
        ));
        assert!(!response_allows(
            &[header("Access-Control-Allow-Origin", "null")],
            origin
        ));
    }

    #[test]
    fn a_cross_origin_page_reads_only_the_safelisted_and_exposed_headers() {
        let headers = [
            header("Content-Type", "application/json"),
            header("Set-Cookie", "session=stolen"),
            header("X-Request-Id", "abc"),
            header("Access-Control-Expose-Headers", "X-Request-Id"),
        ];
        let cross: Vec<String> = exposed_headers(&headers, true)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(cross, ["Content-Type", "X-Request-Id"]);

        let same: Vec<String> = exposed_headers(&headers, false)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(same.len(), headers.len());
    }

    #[test]
    fn a_page_cannot_set_the_headers_the_standard_forbids() {
        let sent = author_headers(&pairs(&[
            ("Cookie", "session=attacker"),
            ("Origin", "https://bank.example.test"),
            ("Host", "bank.example.test"),
            ("Sec-Fetch-Site", "same-origin"),
            ("Proxy-Authorization", "x"),
            ("X-Token", "t"),
        ]));
        assert_eq!(sent, pairs(&[("X-Token", "t")]));
    }
}
