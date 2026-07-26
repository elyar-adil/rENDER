//! Browser-level, headless loading policy for HTML images.
//!
//! Discovery and decoding live in `render-core`; this adapter only maps an
//! ordered `render-net` batch to validated decoded resources. It does not
//! perform GUI work and it never disables TLS verification.

use render_core::document::Document;
use render_core::dom::{DomRevision, NodeId};
use render_core::image::{
    ImageDiscoveryDiagnostic, ImageDiscoveryDiagnosticCode, ImageFormat, ImageLimits,
    ImageResourceKey, ImageResources, decode_image, discover_images, image_key_is_current,
    sniff_image_format,
};
use render_core::paint::ImageResourceId;
use render_net::{FetchRequest, FetchResponse, FetchResult, Url};

pub const IMAGE_ACCEPT: &str = "image/webp,image/png,image/jpeg,image/gif,*/*;q=0.1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageFetch {
    pub source_order: usize,
    pub key: ImageResourceKey,
    pub request: FetchRequest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageFetchPlan {
    pub revision: DomRevision,
    pub document_url: Url,
    pub resources: Vec<ImageFetch>,
    pub diagnostics: Vec<ImageResourceDiagnostic>,
}

impl ImageFetchPlan {
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
pub enum ImageDiagnosticSeverity {
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageResourceDiagnosticCode {
    Discovery(ImageDiscoveryDiagnosticCode),
    StaleSource,
    MissingBatchResult,
    ExtraBatchResult,
    Transport,
    UnexpectedResponseUrl,
    HttpStatus,
    MissingContentType,
    UnsupportedContentType,
    ContentTypeMismatch,
    Decode,
    Store,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageResourceDiagnostic {
    pub owner: Option<NodeId>,
    pub source_order: Option<usize>,
    pub requested_url: Option<Url>,
    pub severity: ImageDiagnosticSeverity,
    pub code: ImageResourceDiagnosticCode,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedImageMetadata {
    pub source_order: usize,
    pub owner: NodeId,
    pub requested_url: Url,
    pub final_url: Url,
    pub resource_id: ImageResourceId,
    pub format: ImageFormat,
    pub encoded_bytes: usize,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageBatchApplication {
    pub plan_revision: DomRevision,
    pub applied_revision: DomRevision,
    pub loaded: Vec<LoadedImageMetadata>,
    pub diagnostics: Vec<ImageResourceDiagnostic>,
}

#[must_use]
pub fn plan_images(document: &Document, document_url: &Url, limits: ImageLimits) -> ImageFetchPlan {
    let discovery = discover_images(document.dom(), document_url, limits);
    let diagnostics = discovery
        .diagnostics
        .into_iter()
        .map(discovery_diagnostic)
        .collect();
    let resources = discovery
        .resources
        .into_iter()
        .map(|image| ImageFetch {
            source_order: image.source_order,
            request: FetchRequest::get(image.key.requested_url.clone()).with_accept(IMAGE_ACCEPT),
            key: image.key,
        })
        .collect();
    ImageFetchPlan {
        revision: discovery.revision,
        document_url: document_url.clone(),
        resources,
        diagnostics,
    }
}

/// Apply an input-ordered batch. Unrelated DOM mutations do not invalidate the
/// plan, but an `img` whose current `src`/base no longer resolves to its planned
/// URL rejects that response as stale.
#[must_use]
pub fn apply_image_batch(
    document: &Document,
    plan: &ImageFetchPlan,
    results: Vec<FetchResult>,
    images: &mut ImageResources,
    limits: ImageLimits,
) -> ImageBatchApplication {
    let mut application = ImageBatchApplication {
        plan_revision: plan.revision,
        applied_revision: document.dom().revision(),
        loaded: Vec::new(),
        diagnostics: plan.diagnostics.clone(),
    };
    let result_count = results.len();
    for (resource, result) in plan.resources.iter().zip(results) {
        if !image_key_is_current(document.dom(), &plan.document_url, &resource.key) {
            application.diagnostics.push(resource_diagnostic(
                resource,
                ImageDiagnosticSeverity::Warning,
                ImageResourceDiagnosticCode::StaleSource,
                "image response ignored because img src or document base changed".to_owned(),
            ));
            continue;
        }
        match result {
            Ok(response) => apply_response(resource, response, images, limits, &mut application),
            Err(error) => application.diagnostics.push(resource_diagnostic(
                resource,
                ImageDiagnosticSeverity::Error,
                ImageResourceDiagnosticCode::Transport,
                format!("image transfer failed: {error}"),
            )),
        }
    }
    for resource in plan.resources.iter().skip(result_count) {
        application.diagnostics.push(resource_diagnostic(
            resource,
            ImageDiagnosticSeverity::Error,
            ImageResourceDiagnosticCode::MissingBatchResult,
            "ordered image batch did not return a result for this request".to_owned(),
        ));
    }
    if result_count > plan.resources.len() {
        application.diagnostics.push(ImageResourceDiagnostic {
            owner: None,
            source_order: None,
            requested_url: None,
            severity: ImageDiagnosticSeverity::Error,
            code: ImageResourceDiagnosticCode::ExtraBatchResult,
            message: format!(
                "ordered image batch returned {} extra result(s)",
                result_count - plan.resources.len()
            ),
        });
    }
    application
}

fn apply_response(
    resource: &ImageFetch,
    response: FetchResponse,
    images: &mut ImageResources,
    limits: ImageLimits,
    application: &mut ImageBatchApplication,
) {
    if response.requested_url != resource.key.requested_url {
        application.diagnostics.push(resource_diagnostic(
            resource,
            ImageDiagnosticSeverity::Error,
            ImageResourceDiagnosticCode::UnexpectedResponseUrl,
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
            ImageDiagnosticSeverity::Error,
            ImageResourceDiagnosticCode::HttpStatus,
            format!(
                "image server returned HTTP status {}",
                response.status.as_u16()
            ),
        ));
        return;
    }
    let Some(content_type) = response.content_type.as_ref() else {
        application.diagnostics.push(resource_diagnostic(
            resource,
            ImageDiagnosticSeverity::Error,
            ImageResourceDiagnosticCode::MissingContentType,
            "image response omitted Content-Type".to_owned(),
        ));
        return;
    };
    let Some(format) = ImageFormat::from_media_type(&content_type.media_type) else {
        application.diagnostics.push(resource_diagnostic(
            resource,
            ImageDiagnosticSeverity::Error,
            ImageResourceDiagnosticCode::UnsupportedContentType,
            format!(
                "image response has unsupported content type '{}'",
                content_type.media_type
            ),
        ));
        return;
    };
    if sniff_image_format(&response.body) != Some(format) {
        application.diagnostics.push(resource_diagnostic(
            resource,
            ImageDiagnosticSeverity::Error,
            ImageResourceDiagnosticCode::ContentTypeMismatch,
            format!(
                "image bytes do not match declared content type '{}'",
                content_type.media_type
            ),
        ));
        return;
    }
    let decoded = match decode_image(&response.body, format, limits) {
        Ok(decoded) => decoded,
        Err(error) => {
            application.diagnostics.push(resource_diagnostic(
                resource,
                ImageDiagnosticSeverity::Error,
                ImageResourceDiagnosticCode::Decode,
                error.to_string(),
            ));
            return;
        }
    };
    let (width, height) = decoded.intrinsic_size();
    match images.insert(resource.key.clone(), decoded, limits) {
        Ok(resource_id) => application.loaded.push(LoadedImageMetadata {
            source_order: resource.source_order,
            owner: resource.key.owner,
            requested_url: resource.key.requested_url.clone(),
            final_url: response.final_url,
            resource_id,
            format,
            encoded_bytes: response.body.len(),
            width,
            height,
        }),
        Err(error) => application.diagnostics.push(resource_diagnostic(
            resource,
            ImageDiagnosticSeverity::Error,
            ImageResourceDiagnosticCode::Store,
            error.to_string(),
        )),
    }
}

fn discovery_diagnostic(diagnostic: ImageDiscoveryDiagnostic) -> ImageResourceDiagnostic {
    let severity = match diagnostic.code {
        ImageDiscoveryDiagnosticCode::SrcsetUnsupported
        | ImageDiscoveryDiagnosticCode::MissingSource
        | ImageDiscoveryDiagnosticCode::EmptySource => ImageDiagnosticSeverity::Warning,
        ImageDiscoveryDiagnosticCode::NodeLimit
        | ImageDiscoveryDiagnosticCode::ResourceLimit
        | ImageDiscoveryDiagnosticCode::UrlBytesLimit
        | ImageDiscoveryDiagnosticCode::InvalidBaseUrl
        | ImageDiscoveryDiagnosticCode::InvalidSourceUrl
        | ImageDiscoveryDiagnosticCode::UnsupportedScheme => ImageDiagnosticSeverity::Error,
    };
    ImageResourceDiagnostic {
        owner: diagnostic.node,
        source_order: None,
        requested_url: None,
        severity,
        code: ImageResourceDiagnosticCode::Discovery(diagnostic.code),
        message: diagnostic.message,
    }
}

fn resource_diagnostic(
    resource: &ImageFetch,
    severity: ImageDiagnosticSeverity,
    code: ImageResourceDiagnosticCode,
    message: String,
) -> ImageResourceDiagnostic {
    ImageResourceDiagnostic {
        owner: Some(resource.key.owner),
        source_order: Some(resource.source_order),
        requested_url: Some(resource.key.requested_url.clone()),
        severity,
        code,
        message,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use render_core::document::Document;
    use render_core::image::{ImageLimits, ImageResources};
    use render_net::{BatchOptions, CancelToken, FetchConfig, HttpTransport, Url};

    use super::{IMAGE_ACCEPT, ImageResourceDiagnosticCode, apply_image_batch, plan_images};

    // Valid 1 x 1 RGBA PNG generated once for the local transport fixtures.
    const PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240,
        31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    #[test]
    fn local_parallel_batch_loads_decodes_and_indexes_images() {
        let (base, server) = serve(2, "image/png", PNG);
        let document = Document::parse("<base href='/assets/'><img src='a.png'><img src='b.png'>");
        let plan = plan_images(&document, &base, ImageLimits::default());
        assert_eq!(plan.resources.len(), 2);
        assert!(
            plan.resources
                .iter()
                .all(|resource| resource.request.accept.as_deref() == Some(IMAGE_ACCEPT))
        );
        let transport = transport();
        let results = transport.fetch_batch(
            plan.requests(),
            &BatchOptions::default(),
            &CancelToken::default(),
        );
        let mut images = ImageResources::default();
        let application = apply_image_batch(
            &document,
            &plan,
            results,
            &mut images,
            ImageLimits::default(),
        );
        server.join().expect("server thread");

        assert_eq!(
            application.loaded.len(),
            2,
            "diagnostics: {:?}",
            application.diagnostics
        );
        assert!(application.diagnostics.is_empty());
        assert_eq!(images.len(), 2);
        assert!(application.loaded.iter().all(|loaded| {
            loaded.width == 1 && loaded.height == 1 && images.get(loaded.resource_id).is_some()
        }));
    }

    #[test]
    fn changed_src_rejects_the_completed_response_as_stale() {
        let (base, server) = serve(1, "image/png", PNG);
        let mut document = Document::parse("<img src='old.png'>");
        let plan = plan_images(&document, &base, ImageLimits::default());
        let response = transport().fetch_batch(
            plan.requests(),
            &BatchOptions::default(),
            &CancelToken::default(),
        );
        server.join().expect("server thread");
        document
            .dom_mut()
            .set_attribute(plan.resources[0].key.owner, "src", "new.png")
            .unwrap();

        let mut images = ImageResources::default();
        let application = apply_image_batch(
            &document,
            &plan,
            response,
            &mut images,
            ImageLimits::default(),
        );
        assert!(images.is_empty());
        assert!(
            application
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == ImageResourceDiagnosticCode::StaleSource })
        );
    }

    #[test]
    fn content_type_mismatch_fails_closed() {
        let (base, server) = serve(1, "image/jpeg", PNG);
        let document = Document::parse("<img src='not-a-jpeg.jpg'>");
        let plan = plan_images(&document, &base, ImageLimits::default());
        let results = transport().fetch_batch(
            plan.requests(),
            &BatchOptions::default(),
            &CancelToken::default(),
        );
        server.join().expect("server thread");
        let mut images = ImageResources::default();
        let application = apply_image_batch(
            &document,
            &plan,
            results,
            &mut images,
            ImageLimits::default(),
        );
        assert!(images.is_empty());
        assert!(application.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == ImageResourceDiagnosticCode::ContentTypeMismatch
        }));
    }

    fn transport() -> HttpTransport {
        HttpTransport::new(FetchConfig {
            timeout: Duration::from_secs(2),
            ..FetchConfig::default()
        })
    }

    fn serve(
        expected_connections: usize,
        content_type: &'static str,
        body: &'static [u8],
    ) -> (Url, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local server");
        let address = listener.local_addr().expect("local address");
        let server = thread::spawn(move || {
            let mut children = Vec::new();
            for _ in 0..expected_connections {
                let (stream, _) = listener.accept().expect("accept request");
                children.push(thread::spawn(move || serve_one(stream, content_type, body)));
            }
            for child in children {
                child.join().expect("serve request");
            }
        });
        (
            Url::parse(&format!("http://{address}/index.html")).unwrap(),
            server,
        )
    }

    fn serve_one(mut stream: TcpStream, content_type: &str, body: &[u8]) {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream.read(&mut buffer).expect("read request");
            if count == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..count]);
        }
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(headers.as_bytes()).expect("write headers");
        stream.write_all(body).expect("write body");
    }
}
