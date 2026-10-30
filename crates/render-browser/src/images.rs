//! Browser-level, headless loading policy for HTML images.
//!
//! Discovery and decoding live in `render-core`; this adapter only maps an
//! ordered `render-net` batch to validated decoded resources. It does not
//! perform GUI work and it never disables TLS verification.

use std::collections::BTreeMap;

use render_core::css::computed::ComputedStyle;
use render_core::document::Document;
use render_core::dom::{DomRevision, NodeId};
use render_core::image::{
    DecodedImage, ImageDiscoveryDiagnostic, ImageDiscoveryDiagnosticCode, ImageFormat, ImageLimits,
    ImageResourceKey, ImageResources, ImageSelectionContext, ImageSource, decode_image,
    discover_images_with_styles_and_context, image_key_is_current, sniff_image_format,
};
use render_core::paint::ImageResourceId;
use render_net::{FetchRequest, FetchResponse, FetchResult, Url};

pub const IMAGE_ACCEPT: &str = "image/avif,image/webp,image/png,image/jpeg,image/gif,*/*;q=0.1";

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
    PlaceholderFallback,
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
    pub source: ImageSource,
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
    plan_images_with_styles(
        document,
        &BTreeMap::new(),
        document_url,
        &ImageResources::default(),
        limits,
    )
}

#[must_use]
pub fn plan_images_with_styles(
    document: &Document,
    styles: &BTreeMap<NodeId, ComputedStyle>,
    document_url: &Url,
    loaded: &ImageResources,
    limits: ImageLimits,
) -> ImageFetchPlan {
    plan_images_with_styles_and_context(
        document,
        styles,
        document_url,
        loaded,
        limits,
        ImageSelectionContext::default(),
    )
}

#[must_use]
pub fn plan_images_with_styles_and_context(
    document: &Document,
    styles: &BTreeMap<NodeId, ComputedStyle>,
    document_url: &Url,
    loaded: &ImageResources,
    limits: ImageLimits,
    context: ImageSelectionContext,
) -> ImageFetchPlan {
    let discovery = discover_images_with_styles_and_context(
        document.dom(),
        styles,
        document_url,
        limits,
        context,
    );
    let diagnostics = discovery
        .diagnostics
        .into_iter()
        .map(discovery_diagnostic)
        .collect();
    let resources = discovery
        .resources
        .into_iter()
        .filter(|image| {
            loaded
                .get_for_node_url(image.key.owner, &image.key.requested_url)
                .is_none()
        })
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

#[allow(
    clippy::too_many_lines,
    reason = "one response walks status, format, fallback and storage in order"
)]
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
    let sniffed_format = sniff_image_format(&response.body);
    let declared_format = response
        .content_type
        .as_ref()
        .and_then(|content_type| ImageFormat::from_media_type(&content_type.media_type));
    let declared_supported = declared_format.is_some();
    // Tracking beacons and soft-404 endpoints are routinely referenced from
    // `<img>` but return HTML, an empty body, or another non-image payload.
    // A browser renders those as a failed/empty image, not as a hard page
    // error. Keep the pixels empty and surface a warning instead.
    if sniffed_format.is_none()
        && declared_format != Some(ImageFormat::Svg)
        && !looks_like_svg(&response.body)
        && response.body.len() <= 4096
    {
        store_placeholder(
            resource,
            response.final_url,
            declared_format.unwrap_or(ImageFormat::Png),
            1,
            1,
            images,
            limits,
            application,
            ImageResourceDiagnosticCode::PlaceholderFallback,
            "image response has no supported signature; inserted a transparent placeholder",
        );
        return;
    }
    let Some(format) = resolve_image_format(
        resource,
        response.content_type.as_ref(),
        sniffed_format,
        application,
    ) else {
        return;
    };
    if format != ImageFormat::Svg && sniffed_format != Some(format) {
        // Bytes contradicting a *supported* declared type stay a hard error
        // (corrupted transfer, wrong asset). An unsupported declared type
        // already fell back to the sniffed format above; the bytes are the
        // only source of truth at that point.
        if declared_supported {
            application.diagnostics.push(resource_diagnostic(
                resource,
                ImageDiagnosticSeverity::Error,
                ImageResourceDiagnosticCode::ContentTypeMismatch,
                format!(
                    "image bytes do not match declared content type '{}'",
                    response
                        .content_type
                        .as_ref()
                        .map_or("unknown", |content_type| content_type.media_type.as_str())
                ),
            ));
            return;
        }
    }
    let decoded = match decode_image(&response.body, format, limits) {
        Ok(decoded) => decoded,
        Err(error) => {
            // Bing and other sites probe AVIF support with a tiny, truncated
            // data: AVIF whose header is valid but whose frame data is not.
            // Decoders disagree on it; treating it as a transparent image is
            // the same visual result as a browser that reports AVIF support.
            if format == ImageFormat::Avif
                && is_tiny_avif_probe(resource, &response.body)
                && let Some((width, height)) = avif_probe_size(&response.body)
            {
                store_placeholder(
                    resource,
                    response.final_url,
                    format,
                    width,
                    height,
                    images,
                    limits,
                    application,
                    ImageResourceDiagnosticCode::PlaceholderFallback,
                    "truncated AVIF capability probe; inserted a transparent placeholder",
                );
                return;
            }
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
            source: resource.key.source,
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

/// `true` when `bytes` contains an SVG root in the first kilobyte.
fn looks_like_svg(bytes: &[u8]) -> bool {
    bytes
        .windows(4)
        .take(1024)
        .any(|window| window.eq_ignore_ascii_case(b"<svg"))
}

fn is_tiny_avif_probe(resource: &ImageFetch, bytes: &[u8]) -> bool {
    bytes.len() <= 4096
        && resource
            .key
            .requested_url
            .as_str()
            .starts_with("data:image/avif")
}

/// The `ispe` box's width/height, when it names a probe-sized image.
fn avif_probe_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let marker = bytes.windows(4).position(|window| window == b"ispe")?;
    let width = u32::from_be_bytes(bytes.get(marker + 8..marker + 12)?.try_into().ok()?);
    let height = u32::from_be_bytes(bytes.get(marker + 12..marker + 16)?.try_into().ok()?);
    (1..=4).contains(&width).then_some(())?;
    (1..=4).contains(&height).then_some(())?;
    Some((width, height))
}

#[allow(clippy::too_many_arguments)]
fn store_placeholder(
    resource: &ImageFetch,
    final_url: Url,
    format: ImageFormat,
    width: u32,
    height: u32,
    images: &mut ImageResources,
    limits: ImageLimits,
    application: &mut ImageBatchApplication,
    code: ImageResourceDiagnosticCode,
    message: &str,
) {
    application.diagnostics.push(resource_diagnostic(
        resource,
        ImageDiagnosticSeverity::Warning,
        code,
        message.to_owned(),
    ));
    let bytes = vec![0_u8; width as usize * height as usize * 4];
    let Ok(decoded) = DecodedImage::from_rgba8(width, height, &bytes) else {
        return;
    };
    match images.insert(resource.key.clone(), decoded, limits) {
        Ok(resource_id) => application.loaded.push(LoadedImageMetadata {
            source_order: resource.source_order,
            owner: resource.key.owner,
            source: resource.key.source,
            requested_url: resource.key.requested_url.clone(),
            final_url,
            resource_id,
            format,
            encoded_bytes: 0,
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

fn resolve_image_format(
    resource: &ImageFetch,
    content_type: Option<&render_net::ContentType>,
    sniffed_format: Option<ImageFormat>,
    application: &mut ImageBatchApplication,
) -> Option<ImageFormat> {
    let Some(content_type) = content_type else {
        let Some(sniffed_format) = sniffed_format else {
            if resource
                .key
                .requested_url
                .path()
                .to_ascii_lowercase()
                .ends_with(".svg")
            {
                return Some(ImageFormat::Svg);
            }
            application.diagnostics.push(resource_diagnostic(
                resource,
                ImageDiagnosticSeverity::Error,
                ImageResourceDiagnosticCode::MissingContentType,
                "image response omitted Content-Type and has no supported image signature"
                    .to_owned(),
            ));
            return None;
        };
        application.diagnostics.push(resource_diagnostic(
            resource,
            ImageDiagnosticSeverity::Warning,
            ImageResourceDiagnosticCode::MissingContentType,
            format!(
                "image response omitted Content-Type; using sniffed '{}'",
                sniffed_format.media_type()
            ),
        ));
        return Some(sniffed_format);
    };
    if let Some(declared_format) = ImageFormat::from_media_type(&content_type.media_type) {
        if declared_format == ImageFormat::Svg
            && let Some(sniffed_format) = sniffed_format
        {
            // Bytes beat a text/svg label that contradicts a binary image
            // signature (CDNs mislabel PNG payloads as svg+xml).
            return Some(sniffed_format);
        }
        return Some(declared_format);
    }
    if is_generic_binary_media_type(&content_type.media_type)
        && let Some(sniffed_format) = sniffed_format
    {
        application.diagnostics.push(resource_diagnostic(
            resource,
            ImageDiagnosticSeverity::Warning,
            ImageResourceDiagnosticCode::UnsupportedContentType,
            format!(
                "image response declared generic content type '{}'; using sniffed '{}'",
                content_type.media_type,
                sniffed_format.media_type()
            ),
        ));
        return Some(sniffed_format);
    }
    // A declared-but-unsupported content type with a valid image signature
    // (common CDNs label SVG/PNG URLs inconsistently) decodes by content:
    // browsers honor the actual bytes over a wrong label.
    if let Some(sniffed_format) = sniffed_format {
        application.diagnostics.push(resource_diagnostic(
            resource,
            ImageDiagnosticSeverity::Warning,
            ImageResourceDiagnosticCode::UnsupportedContentType,
            format!(
                "image response declared unsupported content type '{}'; using sniffed '{}'",
                content_type.media_type,
                sniffed_format.media_type()
            ),
        ));
        return Some(sniffed_format);
    }
    application.diagnostics.push(resource_diagnostic(
        resource,
        ImageDiagnosticSeverity::Error,
        ImageResourceDiagnosticCode::UnsupportedContentType,
        format!(
            "image response has unsupported content type '{}'",
            content_type.media_type
        ),
    ));
    None
}

fn is_generic_binary_media_type(media_type: &str) -> bool {
    matches!(
        media_type,
        "application/octet-stream" | "binary/octet-stream"
    )
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
    const AVIF_PROBE: &str = "AAAAIGZ0eXBhdmlmAAAAAGF2aWZtaWYxbWlhZk1BMUIAAADybWV0YQAAAAAAAAAoaGRscgAAAAAAAAAAcGljdAAAAAAAAAAAAAAAAGxpYmF2aWYAAAAADnBpdG0AAAAAAAEAAAAeaWxvYwAAAABEAAABAAEAAAABAAABGgAAAB0AAAAoaWluZgAAAAAAAQAAABppbmZlAgAAAAABAABhdjAxQ29sb3IAAAAAamlwcnAAAABLaXBjbwAAABRpc3BlAAAAAAAAAAIAAAACAAAAEHBpeGkAAAAAAwgICAAAAAxhdjFDgQ0MAAAAABNjb2xybmNseAACAAIAAYAAAAAXaXBtYQAAAAAAAAABAAEEAQKDBAAAACVtZGF0EgAKCBgANogQEAwgMg8f8D///8WfhwB8+ErK";

    const PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240,
        31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    #[test]
    fn local_parallel_batch_loads_decodes_and_indexes_images() {
        let (base, server) = serve(2, Some("image/png"), PNG);
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
        let (base, server) = serve(1, Some("image/png"), PNG);
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
        let (base, server) = serve(1, Some("image/jpeg"), PNG);
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

    #[test]
    fn unsupported_declared_type_with_valid_signature_decodes_by_content() {
        // A PNG payload labeled image/svg+xml (CDN mislabeling): the sniffed
        // signature wins over the unsupported declared type.
        let (base, server) = serve(1, Some("image/svg+xml"), PNG);
        let document = Document::parse("<img src='mislabeled.png'>");
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

        assert_eq!(application.loaded.len(), 1, "mislabeled PNG decodes");
        assert_eq!(images.len(), 1);
    }

    #[test]
    fn missing_content_type_uses_a_supported_image_signature() {
        let (base, server) = serve(1, None, PNG);
        let document = Document::parse("<img src='image'>");
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

        assert_eq!(application.loaded.len(), 1);
        assert_eq!(images.len(), 1);
        assert!(application.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == ImageResourceDiagnosticCode::MissingContentType
                && diagnostic.severity == super::ImageDiagnosticSeverity::Warning
        }));
    }

    #[test]
    fn data_url_image_loads_through_the_normal_decode_pipeline() {
        let document = Document::parse(
            "<img src='data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=='>",
        );
        let base = Url::parse("https://example.test/").unwrap();
        let plan = plan_images(&document, &base, ImageLimits::default());
        let results = transport().fetch_batch(
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

        assert_eq!(application.loaded.len(), 1, "{application:?}");
        assert!(application.diagnostics.is_empty());
        assert_eq!(images.len(), 1);
    }

    #[test]
    fn a_non_image_response_used_as_an_image_becomes_a_transparent_placeholder() {
        let (base, server) = serve(1, Some("text/html"), b"<html>not an image</html>");
        let document = Document::parse("<img src='beacon'>");
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
        assert_eq!(application.loaded.len(), 1);
        assert_eq!(images.len(), 1);
        assert!(application.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == ImageResourceDiagnosticCode::PlaceholderFallback
                && diagnostic.severity == super::ImageDiagnosticSeverity::Warning
        }));
        assert!(
            !application
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == super::ImageDiagnosticSeverity::Error)
        );
    }

    #[test]
    fn a_truncated_avif_probe_falls_back_to_a_transparent_placeholder() {
        let document = Document::parse(&format!("<img src='data:image/avif;base64,{AVIF_PROBE}'>"));
        let base = Url::parse("https://example.test/").unwrap();
        let plan = plan_images(&document, &base, ImageLimits::default());
        let results = transport().fetch_batch(
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
        assert_eq!(application.loaded.len(), 1);
        assert_eq!(images.len(), 1);
        assert!(application.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == ImageResourceDiagnosticCode::PlaceholderFallback
                && diagnostic.severity == super::ImageDiagnosticSeverity::Warning
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
        content_type: Option<&'static str>,
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

    fn serve_one(mut stream: TcpStream, content_type: Option<&str>, body: &[u8]) {
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
        let content_type =
            content_type.map_or_else(String::new, |value| format!("Content-Type: {value}\r\n"));
        let headers = format!(
            "HTTP/1.1 200 OK\r\n{content_type}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(headers.as_bytes()).expect("write headers");
        stream.write_all(body).expect("write body");
    }
    #[test]
    fn svg_content_type_decodes_through_the_normal_pipeline() {
        const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="12" height="12"><rect x="1" y="1" width="10" height="10" fill="#ff6600"/></svg>"##;
        let (base, server) = serve(1, Some("image/svg+xml"), SVG);
        let document = Document::parse("<img src='icon.svg'>");
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
        assert_eq!(application.loaded.len(), 1, "svg decodes");
        assert_eq!(images.len(), 1);
    }

    #[test]
    fn svg_url_without_content_type_decodes_by_extension() {
        const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><circle cx="4" cy="4" r="3" fill="#06c"/></svg>"##;
        let (base, server) = serve(1, None, SVG);
        let document = Document::parse("<img src='arrow.svg'>");
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
        assert_eq!(application.loaded.len(), 1, "extension-sniffed svg decodes");
        assert_eq!(images.len(), 1);
    }
}
