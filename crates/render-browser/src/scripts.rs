//! Browser-level loading policy for classic scripts.
//!
//! Discovery and JavaScript execution remain in `render-core`; this adapter
//! turns revision-bound script metadata into ordered network requests, validates
//! responses, decodes classic-script bytes as UTF-8, and compiles every source
//! before an embedding queues any of them for execution.

use std::borrow::Cow;
use std::collections::HashSet;
use std::hash::BuildHasher;

use render_core::document::Document;
use render_core::dom::{DomRevision, NodeId};
use render_core::js::{CompiledScript, JsError, RuntimeLimits};
use render_core::page::PreparedPageScript;
use render_core::script::{
    ScriptDiagnostic, ScriptDiscoveryLimits, ScriptScheduling, ScriptSource, discover_scripts,
};
use render_net::{FetchRequest, FetchResponse, FetchResult, Url};

pub const SCRIPT_ACCEPT: &str = "*/*";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptFetch {
    pub owner: NodeId,
    pub source_order: usize,
    pub request: FetchRequest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PlannedSource {
    Inline(String),
    External { result_index: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PlannedScript {
    owner: NodeId,
    source_order: usize,
    scheduling: ScriptScheduling,
    source: PlannedSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptFetchPlan {
    pub revision: DomRevision,
    pub resources: Vec<ScriptFetch>,
    pub discovery_diagnostics: Vec<ScriptDiagnostic>,
    scripts: Vec<PlannedScript>,
}

impl ScriptFetchPlan {
    #[must_use]
    pub fn requests(&self) -> Vec<FetchRequest> {
        self.resources
            .iter()
            .map(|resource| resource.request.clone())
            .collect()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.scripts.is_empty()
    }

    pub fn owners(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.scripts.iter().map(|script| script.owner)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptResourceSeverity {
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptResourceDiagnosticCode {
    StalePlan,
    MissingBatchResult,
    ExtraBatchResult,
    Transport,
    UnexpectedResponseUrl,
    HttpStatus,
    MissingContentType,
    UnsupportedContentType,
    DecodeReplacement,
    Compile,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScriptResourceDiagnostic {
    pub owner: Option<NodeId>,
    pub source_order: Option<usize>,
    pub requested_url: Option<Url>,
    pub severity: ScriptResourceSeverity,
    pub code: ScriptResourceDiagnosticCode,
    pub message: String,
    pub compile_error: Option<JsError>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PreparedClassicScript {
    pub owner: NodeId,
    pub source_order: usize,
    pub scheduling: ScriptScheduling,
    pub final_url: Option<Url>,
    pub byte_len: usize,
    pub compiled: CompiledScript,
}

impl From<PreparedClassicScript> for PreparedPageScript {
    fn from(script: PreparedClassicScript) -> Self {
        Self {
            owner: script.owner,
            source_order: script.source_order,
            scheduling: script.scheduling,
            compiled: script.compiled,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScriptBatchPreparation {
    pub revision: DomRevision,
    pub scripts: Vec<PreparedClassicScript>,
    pub discovery_diagnostics: Vec<ScriptDiagnostic>,
    pub diagnostics: Vec<ScriptResourceDiagnostic>,
}

#[must_use]
pub fn plan_classic_scripts(
    document: &Document,
    base_url: &Url,
    limits: ScriptDiscoveryLimits,
) -> ScriptFetchPlan {
    plan_unstarted_classic_scripts(document, base_url, limits, &HashSet::new(), false)
}

/// Plans scripts that have not already started in this page lifecycle.
///
/// Follow-up scans represent DOM-inserted scripts. External classic scripts
/// inserted after parsing default to async even when they omit the attribute.
#[must_use]
pub fn plan_unstarted_classic_scripts<S: BuildHasher>(
    document: &Document,
    base_url: &Url,
    limits: ScriptDiscoveryLimits,
    started: &HashSet<NodeId, S>,
    follow_up_scan: bool,
) -> ScriptFetchPlan {
    let discovery = discover_scripts(document, base_url, limits);
    let mut resources = Vec::new();
    let scripts = discovery
        .scripts
        .into_iter()
        .filter(|script| !started.contains(&script.owner))
        .map(|script| {
            let scheduling =
                if follow_up_scan && matches!(&script.source, ScriptSource::External { .. }) {
                    ScriptScheduling::Async
                } else {
                    script.scheduling
                };
            let source = match script.source {
                ScriptSource::Inline { source } => PlannedSource::Inline(source),
                ScriptSource::External { resolved_url, .. } => {
                    let result_index = resources.len();
                    resources.push(ScriptFetch {
                        owner: script.owner,
                        source_order: script.source_order,
                        request: FetchRequest::get(resolved_url).with_accept(SCRIPT_ACCEPT),
                    });
                    PlannedSource::External { result_index }
                }
            };
            PlannedScript {
                owner: script.owner,
                source_order: script.source_order,
                scheduling,
                source,
            }
        })
        .collect();
    ScriptFetchPlan {
        revision: discovery.revision,
        resources,
        discovery_diagnostics: discovery.diagnostics,
        scripts,
    }
}

#[must_use]
pub fn prepare_script_batch(
    document: &Document,
    plan: &ScriptFetchPlan,
    results: Vec<FetchResult>,
    limits: &RuntimeLimits,
) -> ScriptBatchPreparation {
    let mut preparation = ScriptBatchPreparation {
        revision: plan.revision,
        scripts: Vec::new(),
        discovery_diagnostics: plan.discovery_diagnostics.clone(),
        diagnostics: Vec::new(),
    };
    let current_revision = document.dom().revision();
    if current_revision != plan.revision {
        // Resource completion and DOM mutation are concurrent from the
        // embedding's perspective.  A script plan remains valid when the
        // document changed while bytes were in flight; queue it against the
        // current revision and let the per-script owner checks decide what is
        // still present.  Rejecting the whole batch strands unrelated page
        // bootstrap scripts after a harmless image or style mutation.
        preparation.revision = current_revision;
    }

    let result_count = results.len();
    let mut responses = results.into_iter().map(Some).collect::<Vec<_>>();
    for script in &plan.scripts {
        match &script.source {
            PlannedSource::Inline(source) => {
                compile_source(script, source, None, source.len(), limits, &mut preparation);
            }
            PlannedSource::External { result_index } => {
                let resource = &plan.resources[*result_index];
                let Some(result) = responses.get_mut(*result_index).and_then(Option::take) else {
                    preparation.diagnostics.push(resource_diagnostic(
                        resource,
                        ScriptResourceSeverity::Error,
                        ScriptResourceDiagnosticCode::MissingBatchResult,
                        "ordered script batch did not return a result for this request",
                    ));
                    continue;
                };
                match result {
                    Ok(response) => {
                        apply_response(script, resource, response, limits, &mut preparation);
                    }
                    Err(error) => preparation.diagnostics.push(resource_diagnostic(
                        resource,
                        ScriptResourceSeverity::Error,
                        ScriptResourceDiagnosticCode::Transport,
                        format!("script transfer failed: {error}"),
                    )),
                }
            }
        }
    }
    if result_count > plan.resources.len() {
        preparation.diagnostics.push(general_diagnostic(
            ScriptResourceDiagnosticCode::ExtraBatchResult,
            format!(
                "ordered script batch returned {} extra result(s)",
                result_count - plan.resources.len()
            ),
        ));
    }
    preparation
}

fn apply_response(
    script: &PlannedScript,
    resource: &ScriptFetch,
    response: FetchResponse,
    limits: &RuntimeLimits,
    preparation: &mut ScriptBatchPreparation,
) {
    if response.requested_url != resource.request.url {
        preparation.diagnostics.push(resource_diagnostic(
            resource,
            ScriptResourceSeverity::Error,
            ScriptResourceDiagnosticCode::UnexpectedResponseUrl,
            format!(
                "batch response was for {}, expected {}",
                response.requested_url, resource.request.url
            ),
        ));
        return;
    }
    if !response.status.is_success() {
        preparation.diagnostics.push(resource_diagnostic(
            resource,
            ScriptResourceSeverity::Error,
            ScriptResourceDiagnosticCode::HttpStatus,
            format!(
                "script server returned HTTP status {}",
                response.status.as_u16()
            ),
        ));
        return;
    }
    match response.content_type.as_ref() {
        Some(content_type) if !is_javascript_mime(&content_type.media_type) => {
            preparation.diagnostics.push(resource_diagnostic(
                resource,
                ScriptResourceSeverity::Error,
                ScriptResourceDiagnosticCode::UnsupportedContentType,
                format!(
                    "script response has unsupported content type '{}'",
                    content_type.media_type
                ),
            ));
            return;
        }
        None => preparation.diagnostics.push(resource_diagnostic(
            resource,
            ScriptResourceSeverity::Warning,
            ScriptResourceDiagnosticCode::MissingContentType,
            "script response omitted Content-Type; decoding its bytes as UTF-8",
        )),
        Some(_) => {}
    }

    let source = String::from_utf8_lossy(&response.body);
    if matches!(source, Cow::Owned(_)) {
        preparation.diagnostics.push(resource_diagnostic(
            resource,
            ScriptResourceSeverity::Warning,
            ScriptResourceDiagnosticCode::DecodeReplacement,
            "invalid UTF-8 in classic script was replaced with U+FFFD",
        ));
    }
    compile_source(
        script,
        &source,
        Some(response.final_url),
        response.body.len(),
        limits,
        preparation,
    );
}

fn compile_source(
    script: &PlannedScript,
    source: &str,
    final_url: Option<Url>,
    byte_len: usize,
    limits: &RuntimeLimits,
    preparation: &mut ScriptBatchPreparation,
) {
    match CompiledScript::compile(source, limits) {
        Ok(compiled) => preparation.scripts.push(PreparedClassicScript {
            owner: script.owner,
            source_order: script.source_order,
            scheduling: script.scheduling,
            final_url,
            byte_len,
            compiled,
        }),
        Err(error) => preparation.diagnostics.push(ScriptResourceDiagnostic {
            owner: Some(script.owner),
            source_order: Some(script.source_order),
            requested_url: final_url,
            severity: ScriptResourceSeverity::Error,
            code: ScriptResourceDiagnosticCode::Compile,
            message: format!("classic script compilation failed: {error}"),
            compile_error: Some(error),
        }),
    }
}

fn is_javascript_mime(media_type: &str) -> bool {
    matches!(
        media_type,
        "text/javascript"
            | "application/javascript"
            | "text/ecmascript"
            | "application/ecmascript"
            | "application/x-javascript"
            // A number of production sites still serve generated bootstrap
            // code as text/plain. With no nosniff policy this is accepted by
            // mainstream browsers' compatibility path.
            | "text/plain"
    )
}

fn resource_diagnostic(
    resource: &ScriptFetch,
    severity: ScriptResourceSeverity,
    code: ScriptResourceDiagnosticCode,
    message: impl Into<String>,
) -> ScriptResourceDiagnostic {
    ScriptResourceDiagnostic {
        owner: Some(resource.owner),
        source_order: Some(resource.source_order),
        requested_url: Some(resource.request.url.clone()),
        severity,
        code,
        message: message.into(),
        compile_error: None,
    }
}

fn general_diagnostic(
    code: ScriptResourceDiagnosticCode,
    message: impl Into<String>,
) -> ScriptResourceDiagnostic {
    ScriptResourceDiagnostic {
        owner: None,
        source_order: None,
        requested_url: None,
        severity: ScriptResourceSeverity::Error,
        code,
        message: message.into(),
        compile_error: None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::Duration;

    use render_core::document::Document;
    use render_core::js::{JsRuntime, RuntimeLimits};
    use render_core::script::{ScriptDiscoveryLimits, ScriptScheduling};
    use render_net::{BatchOptions, CancelToken, FetchConfig, HttpTransport, Url};

    use super::{
        SCRIPT_ACCEPT, ScriptResourceDiagnosticCode, plan_classic_scripts,
        plan_unstarted_classic_scripts, prepare_script_batch,
    };

    #[test]
    fn planning_preserves_mixed_inline_and_external_source_order() {
        let document = Document::parse(
            "<script>var order = 'I';</script>\
             <script src=one.js></script>\
             <script>order += 'J';</script>\
             <script src=/two.js></script>",
        );
        let base = Url::parse("https://example.test/path/index.html").expect("base URL");

        let plan = plan_classic_scripts(&document, &base, ScriptDiscoveryLimits::default());

        assert_eq!(
            plan.resources
                .iter()
                .map(|resource| resource.source_order)
                .collect::<Vec<_>>(),
            [1, 3]
        );
        assert_eq!(
            plan.requests()
                .iter()
                .map(|request| request.url.as_str())
                .collect::<Vec<_>>(),
            [
                "https://example.test/path/one.js",
                "https://example.test/two.js",
            ]
        );
        assert_eq!(
            plan.resources[0].request.accept.as_deref(),
            Some(SCRIPT_ACCEPT)
        );
    }

    #[test]
    fn planning_and_preparation_preserve_classic_script_scheduling() {
        let (base, server) = serve(3, |_| {
            response("200 OK", Some("text/javascript"), b"var loaded = true;")
        });
        let document = Document::parse(
            "<script src=blocking.js></script>\
             <script defer src=deferred.js></script>\
             <script async src=asynchronous.js></script>",
        );
        let plan = plan_classic_scripts(&document, &base, ScriptDiscoveryLimits::default());
        let transport = HttpTransport::new(FetchConfig {
            timeout: Duration::from_secs(2),
            ..FetchConfig::default()
        });
        let results = transport.fetch_batch(
            plan.requests(),
            &BatchOptions::default(),
            &CancelToken::default(),
        );

        let preparation =
            prepare_script_batch(&document, &plan, results, &RuntimeLimits::default());
        server.join().expect("server thread");

        assert_eq!(
            preparation
                .scripts
                .iter()
                .map(|script| script.scheduling)
                .collect::<Vec<_>>(),
            [
                ScriptScheduling::ParserBlocking,
                ScriptScheduling::Defer,
                ScriptScheduling::Async,
            ]
        );
    }

    #[test]
    fn follow_up_planning_skips_started_scripts_and_defaults_external_scripts_to_async() {
        let document = Document::parse(
            "<script id=initial>var initial = true;</script>\
             <script id=chunk src=chunk.js></script>\
             <script id=inline>var follow_up = true;</script>",
        );
        let base = Url::parse("https://example.test/index.html").expect("base URL");
        let initial = plan_classic_scripts(&document, &base, ScriptDiscoveryLimits::default());
        let owners = initial.owners().collect::<Vec<_>>();
        let started = HashSet::from([owners[0]]);

        let follow_up = plan_unstarted_classic_scripts(
            &document,
            &base,
            ScriptDiscoveryLimits::default(),
            &started,
            true,
        );

        assert_eq!(follow_up.owners().collect::<Vec<_>>(), owners[1..]);
        assert_eq!(follow_up.scripts.len(), 2);
        assert_eq!(follow_up.scripts[0].scheduling, ScriptScheduling::Async);
        assert_eq!(
            follow_up.scripts[1].scheduling,
            ScriptScheduling::ParserBlocking
        );
    }

    #[test]
    fn ordered_network_batch_decodes_compiles_and_executes_one_realm() {
        let (base, server) = serve(2, |path| match path {
            "/one.js" => response(
                "200 OK",
                Some("text/javascript; charset=shift_jis"),
                b"order += 'E';",
            ),
            "/two.js" => response("200 OK", None, b"order += 'F';"),
            _ => response("404 Not Found", Some("text/javascript"), b""),
        });
        let mut document = Document::parse(
            "<script>var order = 'I';</script>\
             <script src=one.js></script>\
             <script>order += 'J';</script>\
             <script src=two.js></script>",
        );
        let plan = plan_classic_scripts(&document, &base, ScriptDiscoveryLimits::default());
        let transport = HttpTransport::new(FetchConfig {
            timeout: Duration::from_secs(2),
            ..FetchConfig::default()
        });
        let results = transport.fetch_batch(
            plan.requests(),
            &BatchOptions::default(),
            &CancelToken::default(),
        );

        let preparation =
            prepare_script_batch(&document, &plan, results, &RuntimeLimits::default());
        server.join().expect("server thread");

        assert_eq!(
            preparation
                .scripts
                .iter()
                .map(|script| script.source_order)
                .collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
        assert!(preparation.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == ScriptResourceDiagnosticCode::MissingContentType
                && diagnostic.source_order == Some(3)
        }));
        let mut runtime = JsRuntime::new(document.dom());
        for script in preparation.scripts {
            runtime
                .execute_compiled(document.dom_mut(), &script.compiled)
                .expect("prepared scripts execute");
        }
        let outcome = runtime
            .execute(document.dom_mut(), "order;")
            .expect("shared Realm retains source order");
        assert_eq!(outcome.value.to_js_string(), "IEJF");
    }

    #[test]
    fn stale_plan_is_rebased_before_compilation() {
        let mut document = Document::parse("<script src=old.js></script>");
        let base = Url::parse("https://example.test/index.html").expect("base URL");
        let plan = plan_classic_scripts(&document, &base, ScriptDiscoveryLimits::default());
        document
            .dom_mut()
            .set_attribute(plan.resources[0].owner, "src", "new.js")
            .expect("retarget script");

        let preparation =
            prepare_script_batch(&document, &plan, Vec::new(), &RuntimeLimits::default());

        assert!(preparation.scripts.is_empty());
        assert_eq!(preparation.revision, document.dom().revision());
        assert_eq!(preparation.diagnostics.len(), 1);
        assert_eq!(
            preparation.diagnostics[0].code,
            ScriptResourceDiagnosticCode::MissingBatchResult
        );
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
