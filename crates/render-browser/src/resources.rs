//! Browser-level loading policy for external author stylesheets.
//!
//! This module deliberately does not own a [`render_net::NetworkWorker`]. The
//! browser event loop can submit [`StylesheetFetchPlan::requests`] as one
//! ordered batch and later pass the results to [`apply_stylesheet_batch`].

use encoding_rs::{Encoding, UTF_8};
use render_core::css::stylesheet::parse_stylesheet;
use render_core::document::{
    AuthorStyleSource, Document, DocumentDiagnosticCode, DocumentLimits, ExternalStyleSheetKey,
    ExternalStyleSheets,
};
use render_core::dom::{DomRevision, NodeId};
use render_net::{FetchRequest, FetchResponse, FetchResult, Url};

/// `Accept` value used for stylesheet requests.
pub const CSS_ACCEPT: &str = "text/css,*/*;q=0.1";

/// One eligible external stylesheet request, in DOM source order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StylesheetFetch {
    pub source_order: usize,
    pub key: ExternalStyleSheetKey,
    pub request: FetchRequest,
}

/// A pure, revision-bound description of the stylesheet network work to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StylesheetFetchPlan {
    pub revision: DomRevision,
    pub resources: Vec<StylesheetFetch>,
    pub diagnostics: Vec<StylesheetResourceDiagnostic>,
}

impl StylesheetFetchPlan {
    /// Clones the ordered requests for [`render_net::NetworkWorker::submit_batch`].
    #[must_use]
    pub fn requests(&self) -> Vec<FetchRequest> {
        self.resources
            .iter()
            .map(|resource| resource.request.clone())
            .collect()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StylesheetDiagnosticSeverity {
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StylesheetDiagnosticCode {
    Discovery(DocumentDiagnosticCode),
    StalePlan,
    MissingBatchResult,
    ExtraBatchResult,
    Transport,
    UnexpectedResponseUrl,
    HttpStatus,
    MissingContentType,
    UnsupportedContentType,
    UnsupportedCharset,
    DecodeReplacement,
    CssSyntax,
}

/// A planning, transfer, decoding, or parsing problem tied to a stylesheet
/// slot whenever that identity is available.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StylesheetResourceDiagnostic {
    pub owner: Option<NodeId>,
    pub source_order: Option<usize>,
    pub requested_url: Option<Url>,
    pub severity: StylesheetDiagnosticSeverity,
    pub code: StylesheetDiagnosticCode,
    pub message: String,
}

/// Metadata for a stylesheet successfully decoded, parsed, and injected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedStylesheet {
    pub source_order: usize,
    pub key: ExternalStyleSheetKey,
    pub final_url: Url,
    pub encoding: String,
    pub byte_len: usize,
    pub parser_diagnostic_count: usize,
}

/// Result of applying an ordered network batch to a fetch plan.
#[derive(Clone, Debug)]
pub struct StylesheetBatchApplication {
    pub revision: DomRevision,
    pub style_sheets: ExternalStyleSheets,
    pub loaded: Vec<LoadedStylesheet>,
    pub diagnostics: Vec<StylesheetResourceDiagnostic>,
}

/// Discovers eligible external author stylesheets and creates their ordered
/// GET requests without performing I/O.
#[must_use]
pub fn plan_external_style_sheets(
    document: &Document,
    base_url: &Url,
    limits: DocumentLimits,
) -> StylesheetFetchPlan {
    let discovery = document.discover_author_style_slots(base_url, limits);
    let diagnostics = discovery
        .diagnostics
        .iter()
        .map(|diagnostic| {
            let slot = diagnostic
                .node
                .and_then(|owner| discovery.slots.iter().find(|slot| slot.owner == owner));
            StylesheetResourceDiagnostic {
                owner: diagnostic.node,
                source_order: slot.map(|slot| slot.source_order),
                requested_url: slot.and_then(slot_url).cloned(),
                severity: discovery_severity(diagnostic.code),
                code: StylesheetDiagnosticCode::Discovery(diagnostic.code),
                message: diagnostic.message.clone(),
            }
        })
        .collect::<Vec<_>>();
    let mut resources = discovery
        .slots
        .iter()
        .filter(|slot| slot.eligibility.is_eligible())
        .filter_map(|slot| {
            let AuthorStyleSource::External {
                resolved_url: Some(requested_url),
                ..
            } = &slot.source
            else {
                return None;
            };
            let key = ExternalStyleSheetKey::new(slot.owner, requested_url.clone());
            Some(StylesheetFetch {
                source_order: slot.source_order,
                key,
                request: FetchRequest::get(requested_url.clone()).with_accept(CSS_ACCEPT),
            })
        })
        .collect::<Vec<_>>();

    // Discovery is already ordered; retain a defensive sort so this adapter's
    // public ordering contract does not depend on that implementation detail.
    resources.sort_by_key(|resource| resource.source_order);
    StylesheetFetchPlan {
        revision: discovery.revision,
        resources,
        diagnostics,
    }
}

/// Validates and applies an ordered `render-net` batch to a stylesheet plan.
///
/// A plan is rejected as a whole if the DOM revision changed while its
/// requests were pending. Individual failed responses do not prevent other
/// source-order slots from being injected.
#[must_use]
pub fn apply_stylesheet_batch(
    document: &Document,
    plan: &StylesheetFetchPlan,
    results: Vec<FetchResult>,
) -> StylesheetBatchApplication {
    let mut application = StylesheetBatchApplication {
        revision: plan.revision,
        style_sheets: ExternalStyleSheets::default(),
        loaded: Vec::new(),
        diagnostics: plan.diagnostics.clone(),
    };

    let current_revision = document.dom().revision();
    if current_revision != plan.revision {
        application.diagnostics.push(StylesheetResourceDiagnostic {
            owner: None,
            source_order: None,
            requested_url: None,
            severity: StylesheetDiagnosticSeverity::Error,
            code: StylesheetDiagnosticCode::StalePlan,
            message: format!(
                "stylesheet plan targets DOM revision {}, but the document is at revision {}",
                plan.revision.as_u64(),
                current_revision.as_u64()
            ),
        });
        return application;
    }

    let result_count = results.len();
    for (resource, result) in plan.resources.iter().zip(results) {
        match result {
            Ok(response) => apply_response(resource, response, &mut application),
            Err(error) => application.diagnostics.push(resource_diagnostic(
                resource,
                StylesheetDiagnosticSeverity::Error,
                StylesheetDiagnosticCode::Transport,
                format!("stylesheet transfer failed: {error}"),
            )),
        }
    }

    for resource in plan.resources.iter().skip(result_count) {
        application.diagnostics.push(resource_diagnostic(
            resource,
            StylesheetDiagnosticSeverity::Error,
            StylesheetDiagnosticCode::MissingBatchResult,
            "ordered stylesheet batch did not return a result for this request".to_owned(),
        ));
    }
    if result_count > plan.resources.len() {
        application.diagnostics.push(StylesheetResourceDiagnostic {
            owner: None,
            source_order: None,
            requested_url: None,
            severity: StylesheetDiagnosticSeverity::Error,
            code: StylesheetDiagnosticCode::ExtraBatchResult,
            message: format!(
                "ordered stylesheet batch returned {} extra result(s)",
                result_count - plan.resources.len()
            ),
        });
    }

    application
}

fn apply_response(
    resource: &StylesheetFetch,
    response: FetchResponse,
    application: &mut StylesheetBatchApplication,
) {
    if response.requested_url != resource.key.requested_url {
        application.diagnostics.push(resource_diagnostic(
            resource,
            StylesheetDiagnosticSeverity::Error,
            StylesheetDiagnosticCode::UnexpectedResponseUrl,
            format!(
                "batch response was for {}, expected {}",
                response.requested_url, resource.key.requested_url
            ),
        ));
        return;
    }
    if !response.status.is_success() {
        application.diagnostics.push(resource_diagnostic(
            resource,
            StylesheetDiagnosticSeverity::Error,
            StylesheetDiagnosticCode::HttpStatus,
            format!(
                "stylesheet server returned HTTP status {}",
                response.status.as_u16()
            ),
        ));
        return;
    }

    match response.content_type.as_ref() {
        Some(content_type) if !content_type.media_type.eq_ignore_ascii_case("text/css") => {
            application.diagnostics.push(resource_diagnostic(
                resource,
                StylesheetDiagnosticSeverity::Error,
                StylesheetDiagnosticCode::UnsupportedContentType,
                format!(
                    "stylesheet response has unsupported content type '{}'",
                    content_type.media_type
                ),
            ));
            return;
        }
        None => application.diagnostics.push(resource_diagnostic(
            resource,
            StylesheetDiagnosticSeverity::Warning,
            StylesheetDiagnosticCode::MissingContentType,
            "stylesheet response omitted Content-Type; interpreting its bytes as CSS".to_owned(),
        )),
        Some(_) => {}
    }

    let transport_charset = response
        .content_type
        .as_ref()
        .and_then(|content_type| content_type.charset.as_deref());
    let decoded = decode_css_bytes(&response.body, transport_charset);
    for issue in decoded.issues {
        application.diagnostics.push(resource_diagnostic(
            resource,
            StylesheetDiagnosticSeverity::Warning,
            issue.code,
            issue.message,
        ));
    }

    let mut sheet = parse_stylesheet(&decoded.text);
    absolutize_background_urls(&mut sheet, &response.final_url);
    for diagnostic in &sheet.diagnostics {
        application.diagnostics.push(resource_diagnostic(
            resource,
            StylesheetDiagnosticSeverity::Warning,
            StylesheetDiagnosticCode::CssSyntax,
            format!(
                "CSS parse diagnostic at {}:{}: {}",
                diagnostic.line, diagnostic.column, diagnostic.message
            ),
        ));
    }
    let parser_diagnostic_count = sheet.diagnostics.len();
    application.style_sheets.insert(resource.key.clone(), sheet);
    application.loaded.push(LoadedStylesheet {
        source_order: resource.source_order,
        key: resource.key.clone(),
        final_url: response.final_url,
        encoding: decoded.encoding.name().to_owned(),
        byte_len: response.body.len(),
        parser_diagnostic_count,
    });
}

fn absolutize_background_urls(
    sheet: &mut render_core::css::stylesheet::StyleSheet,
    base_url: &Url,
) {
    for rule in &mut sheet.rules {
        for declaration in &mut rule.declarations {
            if declaration.name.eq_ignore_ascii_case("background-image")
                || declaration.name.eq_ignore_ascii_case("background")
            {
                declaration.value = absolutize_css_urls(&declaration.value, base_url);
            }
        }
    }
}

fn absolutize_css_urls(value: &str, base_url: &Url) -> String {
    let lower = value.to_ascii_lowercase();
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(relative_start) = lower[cursor..].find("url(") {
        let start = cursor + relative_start;
        output.push_str(&value[cursor..start]);
        let content_start = start + 4;
        let Some(relative_end) = value[content_start..].find(')') else {
            output.push_str(&value[start..]);
            return output;
        };
        let end = content_start + relative_end;
        let reference = value[content_start..end]
            .trim()
            .trim_matches(['\'', '"']);
        if let Ok(url) = base_url.join(reference) {
            output.push_str("url(\"");
            output.push_str(url.as_str());
            output.push_str("\")");
        } else {
            output.push_str(&value[start..=end]);
        }
        cursor = end + 1;
    }
    output.push_str(&value[cursor..]);
    output
}

fn slot_url(slot: &render_core::document::AuthorStyleSlot) -> Option<&Url> {
    match &slot.source {
        AuthorStyleSource::External { resolved_url, .. } => resolved_url.as_ref(),
        AuthorStyleSource::Embedded => None,
    }
}

const fn discovery_severity(code: DocumentDiagnosticCode) -> StylesheetDiagnosticSeverity {
    match code {
        DocumentDiagnosticCode::ExternalStyleSheetUnresolved
        | DocumentDiagnosticCode::StyleDiscoveryNodeLimit
        | DocumentDiagnosticCode::AuthorStyleSlotLimit
        | DocumentDiagnosticCode::ExternalStyleSheetLimit
        | DocumentDiagnosticCode::ExternalStyleSheetUrlBytesLimit => {
            StylesheetDiagnosticSeverity::Error
        }
        DocumentDiagnosticCode::ExternalStyleSheetUnsupported
        | DocumentDiagnosticCode::InlineStyleUnsupported
        | DocumentDiagnosticCode::MediaQueryUnsupported
        | DocumentDiagnosticCode::NonCssStyleType
        | DocumentDiagnosticCode::QuirksModeUnsupported
        | DocumentDiagnosticCode::EmbeddedStyleLimit
        | DocumentDiagnosticCode::EmbeddedStyleBytesLimit => StylesheetDiagnosticSeverity::Warning,
    }
}

fn resource_diagnostic(
    resource: &StylesheetFetch,
    severity: StylesheetDiagnosticSeverity,
    code: StylesheetDiagnosticCode,
    message: String,
) -> StylesheetResourceDiagnostic {
    StylesheetResourceDiagnostic {
        owner: Some(resource.key.owner),
        source_order: Some(resource.source_order),
        requested_url: Some(resource.key.requested_url.clone()),
        severity,
        code,
        message,
    }
}

struct DecodedCss {
    text: String,
    encoding: &'static Encoding,
    issues: Vec<DecodeIssue>,
}

struct DecodeIssue {
    code: StylesheetDiagnosticCode,
    message: String,
}

fn decode_css_bytes(bytes: &[u8], transport_charset: Option<&str>) -> DecodedCss {
    let (encoding, offset, mut issues) = if let Some((encoding, bom_len)) = Encoding::for_bom(bytes)
    {
        (encoding, bom_len, Vec::new())
    } else if let Some(label) = transport_charset {
        encoding_for_label(label, "HTTP charset")
    } else if let Some(label) = css_charset_label(bytes) {
        encoding_for_label(label, "@charset")
    } else {
        (UTF_8, 0, Vec::new())
    };

    let (text, had_errors) = encoding.decode_without_bom_handling(&bytes[offset..]);
    if had_errors {
        issues.push(DecodeIssue {
            code: StylesheetDiagnosticCode::DecodeReplacement,
            message: format!(
                "stylesheet contained malformed {} bytes; invalid sequences were replaced",
                encoding.name()
            ),
        });
    }
    DecodedCss {
        text: text.into_owned(),
        encoding,
        issues,
    }
}

fn encoding_for_label(label: &str, source: &str) -> (&'static Encoding, usize, Vec<DecodeIssue>) {
    if let Some(encoding) = Encoding::for_label(label.trim().as_bytes()) {
        return (encoding, 0, Vec::new());
    }
    (
        UTF_8,
        0,
        vec![DecodeIssue {
            code: StylesheetDiagnosticCode::UnsupportedCharset,
            message: format!("unsupported {source} label '{label}'; falling back to UTF-8"),
        }],
    )
}

fn css_charset_label(bytes: &[u8]) -> Option<&str> {
    const PREFIX: &[u8] = b"@charset \"";
    let declaration = bytes.get(..bytes.len().min(1_024))?;
    let tail = declaration.strip_prefix(PREFIX)?;
    let quote = tail.iter().position(|byte| *byte == b'\"')?;
    if tail.get(quote + 1) != Some(&b';') {
        return None;
    }
    std::str::from_utf8(&tail[..quote]).ok()
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::Duration;

    use encoding_rs::SHIFT_JIS;
    use render_core::document::{Document, DocumentLimits};
    use render_net::{BatchOptions, CancelToken, FetchConfig, FetchError, HttpTransport, Url};

    use super::{
        CSS_ACCEPT, StylesheetDiagnosticCode, apply_stylesheet_batch, decode_css_bytes,
        plan_external_style_sheets,
    };

    #[test]
    fn planning_keeps_only_eligible_external_slots_in_source_order() {
        let document = Document::parse(
            "<style>p { color: red }</style>\
             <link rel=stylesheet href='../css/a.css?theme=1'>\
             <link rel=stylesheet href=print.css media=print>\
             <link rel=stylesheet href=plain.css type=text/plain>\
             <link rel=stylesheet href='https://cdn.example/x.css'>",
        );
        let base = Url::parse("https://example.test/pages/deep/index.html").expect("base URL");

        let plan = plan_external_style_sheets(&document, &base, DocumentLimits::default());

        assert_eq!(plan.revision, document.dom().revision());
        assert_eq!(
            plan.resources
                .iter()
                .map(|resource| resource.source_order)
                .collect::<Vec<_>>(),
            vec![1, 4]
        );
        assert_eq!(
            plan.resources[0].request.url.as_str(),
            "https://example.test/pages/css/a.css?theme=1"
        );
        assert_eq!(
            plan.resources[1].request.url.as_str(),
            "https://cdn.example/x.css"
        );
        assert_eq!(
            plan.resources[0].request.accept.as_deref(),
            Some(CSS_ACCEPT)
        );
        assert!(
            plan.diagnostics.iter().any(|diagnostic| matches!(
                diagnostic.code,
                StylesheetDiagnosticCode::Discovery(_)
            ))
        );
    }

    #[test]
    fn ordered_network_batch_decodes_parses_and_injects_stylesheets() {
        let (base, server) = serve(2, |path| match path {
            "/styles/legacy.css" => {
                let (bytes, _, _) = SHIFT_JIS.encode(".日本 { color: red }");
                response("200 OK", Some("text/css; charset=shift_jis"), &bytes)
            }
            "/styles/modern.css" => response(
                "200 OK",
                Some("text/css; charset=utf-8"),
                b"p { color: blue }",
            ),
            _ => response("404 Not Found", Some("text/css"), b""),
        });
        let document = Document::parse(
            "<link rel=stylesheet href=styles/legacy.css>\
             <style>p { color: green }</style>\
             <link rel=stylesheet href=styles/modern.css>",
        );
        let plan = plan_external_style_sheets(&document, &base, DocumentLimits::default());
        let transport = HttpTransport::new(FetchConfig {
            timeout: Duration::from_secs(2),
            ..FetchConfig::default()
        });

        let results = transport.fetch_batch(
            plan.requests(),
            &BatchOptions::default(),
            &CancelToken::default(),
        );
        let application = apply_stylesheet_batch(&document, &plan, results);
        server.join().expect("server thread");

        assert_eq!(application.style_sheets.len(), 2);
        assert_eq!(
            application
                .loaded
                .iter()
                .map(|loaded| loaded.source_order)
                .collect::<Vec<_>>(),
            vec![0, 2]
        );
        assert_eq!(application.loaded[0].encoding, "Shift_JIS");
        assert!(application.diagnostics.is_empty());
        for resource in &plan.resources {
            assert!(application.style_sheets.get(&resource.key).is_some());
        }
    }

    #[test]
    fn failed_and_missing_batch_entries_are_diagnosed_without_hiding_successes() {
        let (base, server) = serve(1, |_| response("200 OK", None, b"p { color: red }"));
        let document =
            Document::parse("<link rel=stylesheet href=one.css><link rel=stylesheet href=two.css>");
        let plan = plan_external_style_sheets(&document, &base, DocumentLimits::default());
        let transport = HttpTransport::new(FetchConfig::default());
        let success = transport.fetch(&plan.resources[0].request, &CancelToken::default());
        server.join().expect("server thread");

        let application = apply_stylesheet_batch(&document, &plan, vec![success]);

        assert_eq!(application.style_sheets.len(), 1);
        assert!(
            application.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == StylesheetDiagnosticCode::MissingContentType
            })
        );
        assert!(application.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == StylesheetDiagnosticCode::MissingBatchResult
                && diagnostic.source_order == Some(1)
        }));

        let transport_failure = apply_stylesheet_batch(
            &document,
            &plan,
            vec![Err(FetchError::Timeout), Err(FetchError::Dns)],
        );
        assert_eq!(
            transport_failure
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == StylesheetDiagnosticCode::Transport)
                .count(),
            2
        );
    }

    #[test]
    fn http_status_and_non_css_mime_failures_are_structured() {
        let (base, server) = serve(2, |path| match path {
            "/missing.css" => response("404 Not Found", Some("text/css"), b"not found"),
            "/image.css" => response("200 OK", Some("image/png"), b"not really a png"),
            _ => response("500 Internal Server Error", None, b""),
        });
        let document = Document::parse(
            "<link rel=stylesheet href=missing.css><link rel=stylesheet href=image.css>",
        );
        let plan = plan_external_style_sheets(&document, &base, DocumentLimits::default());
        let transport = HttpTransport::new(FetchConfig::default());
        let results = transport.fetch_batch(
            plan.requests(),
            &BatchOptions::default(),
            &CancelToken::default(),
        );

        let application = apply_stylesheet_batch(&document, &plan, results);
        server.join().expect("server thread");

        assert!(application.style_sheets.is_empty());
        assert!(application.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == StylesheetDiagnosticCode::HttpStatus
                && diagnostic.source_order == Some(0)
        }));
        assert!(application.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == StylesheetDiagnosticCode::UnsupportedContentType
                && diagnostic.source_order == Some(1)
        }));
    }

    #[test]
    fn stale_plan_is_rejected_before_any_result_is_applied() {
        let mut document = Document::parse("<link rel=stylesheet href=old.css>");
        let base = Url::parse("https://example.test/index.html").expect("base URL");
        let plan = plan_external_style_sheets(&document, &base, DocumentLimits::default());
        document
            .dom_mut()
            .set_attribute(plan.resources[0].key.owner, "href", "new.css")
            .expect("retarget link");

        let application = apply_stylesheet_batch(&document, &plan, vec![Err(FetchError::Timeout)]);

        assert!(application.style_sheets.is_empty());
        assert_eq!(application.loaded.len(), 0);
        assert!(
            application
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == StylesheetDiagnosticCode::StalePlan })
        );
        assert!(
            !application
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == StylesheetDiagnosticCode::Transport })
        );
    }

    #[test]
    fn css_decoding_honors_bom_and_reports_bad_labels_and_bytes() {
        let with_bom = decode_css_bytes(b"\xef\xbb\xbfp { color: red }", Some("shift_jis"));
        assert_eq!(with_bom.encoding.name(), "UTF-8");
        assert_eq!(with_bom.text, "p { color: red }");

        let unsupported = decode_css_bytes(b"p { color: red }", Some("x-not-an-encoding"));
        assert!(
            unsupported
                .issues
                .iter()
                .any(|issue| { issue.code == StylesheetDiagnosticCode::UnsupportedCharset })
        );

        let malformed = decode_css_bytes(b"p { content: '\xff' }", Some("utf-8"));
        assert!(
            malformed
                .issues
                .iter()
                .any(|issue| { issue.code == StylesheetDiagnosticCode::DecodeReplacement })
        );

        let (legacy_bytes, _, _) = SHIFT_JIS.encode("@charset \"shift_jis\"; .日本 {}");
        let declared = decode_css_bytes(&legacy_bytes, None);
        assert_eq!(declared.encoding.name(), "Shift_JIS");
        assert!(declared.text.contains("日本"));
    }

    fn serve(
        request_count: usize,
        handler: impl Fn(&str) -> Vec<u8> + Send + 'static,
    ) -> (Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let handle = thread::spawn(move || {
            for _ in 0..request_count {
                let (mut stream, _) = listener.accept().expect("accept request");
                let path = request_path(&mut stream);
                stream.write_all(&handler(&path)).expect("write response");
            }
        });
        (
            Url::parse(&format!("http://{address}/index.html")).expect("server URL"),
            handle,
        )
    }

    fn request_path(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout");
        let mut bytes = [0_u8; 4_096];
        let count = stream.read(&mut bytes).expect("read request");
        let request = String::from_utf8_lossy(&bytes[..count]);
        request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_owned()
    }

    fn response(status: &str, content_type: Option<&str>, body: &[u8]) -> Vec<u8> {
        let content_type = content_type
            .map(|value| format!("Content-Type: {value}\r\n"))
            .unwrap_or_default();
        let mut response = format!(
            "HTTP/1.1 {status}\r\n{content_type}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }
}
