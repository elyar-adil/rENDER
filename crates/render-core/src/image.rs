//! Image discovery, bounded decoding, and decoded-resource identity.
//!
//! This module performs no I/O. Browser/document coordinators turn discovery
//! entries into network requests, then insert successfully decoded resources
//! into [`ImageResources`]. Resource keys retain the selected source value so
//! a response for an older dynamic attribute value can be rejected.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::io::Cursor;
use std::sync::Arc;

use image_codec::{ImageFormat as CodecImageFormat, ImageReader};
use url::Url;

use crate::css::computed::ComputedStyle;
use crate::dom::{Dom, DomRevision, ElementData, Namespace, NodeId, NodeKind};
use crate::paint::{Color, ImageResourceId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageLimits {
    pub max_discovery_nodes: usize,
    pub max_resources: usize,
    pub max_url_bytes: usize,
    pub max_encoded_bytes: usize,
    pub max_width: u32,
    pub max_height: u32,
    pub max_pixels: u64,
    pub max_decoded_bytes: usize,
    pub max_total_decoded_bytes: usize,
}

impl Default for ImageLimits {
    fn default() -> Self {
        Self {
            max_discovery_nodes: 1_000_000,
            max_resources: 4_096,
            max_url_bytes: 64 * 1_024,
            max_encoded_bytes: 32 * 1_024 * 1_024,
            max_width: 32_768,
            max_height: 32_768,
            max_pixels: 128 * 1_024 * 1_024,
            max_decoded_bytes: 512 * 1_024 * 1_024,
            max_total_decoded_bytes: 1024 * 1_024 * 1_024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageDiscoveryDiagnosticCode {
    NodeLimit,
    ResourceLimit,
    UrlBytesLimit,
    InvalidBaseUrl,
    MissingSource,
    EmptySource,
    InvalidSourceUrl,
    UnsupportedScheme,
    SrcsetUnsupported,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageDiscoveryDiagnostic {
    pub node: Option<NodeId>,
    pub code: ImageDiscoveryDiagnosticCode,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImageResourceKey {
    pub owner: NodeId,
    pub requested_url: Url,
    pub source_snapshot: String,
    pub source: ImageSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageSource {
    Element,
    CssBackground,
}

// These attributes are common compatibility hooks on Chinese news and portal
// sites. They are only fallbacks when `src` is absent or empty; this is not a
// general lazy-loading script or `srcset` implementation.
const LAZY_IMAGE_SOURCE_ATTRIBUTES: &[&str] = &[
    "data-src",
    "data-original",
    "data-lazy-src",
    "data-actualsrc",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredImage {
    pub source_order: usize,
    pub key: ImageResourceKey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageDiscovery {
    pub revision: DomRevision,
    pub effective_base_url: Url,
    pub resources: Vec<DiscoveredImage>,
    pub diagnostics: Vec<ImageDiscoveryDiagnostic>,
}

/// Find fetchable HTML `img` sources in tree order and resolve them against
/// the document's first valid `base[href]`, or the caller-provided URL.
#[must_use]
pub fn discover_images(dom: &Dom, document_url: &Url, limits: ImageLimits) -> ImageDiscovery {
    let (elements, mut diagnostics) = collect_elements(dom, limits.max_discovery_nodes);
    let effective_base_url = effective_base_url(dom, &elements, document_url, &mut diagnostics);
    let mut resources = Vec::new();

    for node in elements {
        let Some(element) = html_element(dom, node) else {
            continue;
        };
        if element.local_name != "img" {
            continue;
        }
        if attribute(element, "srcset").is_some() {
            diagnostics.push(ImageDiscoveryDiagnostic {
                node: Some(node),
                code: ImageDiscoveryDiagnosticCode::SrcsetUnsupported,
                message: "img srcset candidate selection is not implemented; using src or a supported lazy-source fallback"
                    .to_owned(),
            });
        }
        let Some(source) = element_image_source(element) else {
            continue;
        };
        if source.len() > limits.max_url_bytes {
            diagnostics.push(ImageDiscoveryDiagnostic {
                node: Some(node),
                code: ImageDiscoveryDiagnosticCode::UrlBytesLimit,
                message: format!(
                    "img src uses {} bytes; limit is {}",
                    source.len(),
                    limits.max_url_bytes
                ),
            });
            continue;
        }
        let Ok(requested_url) = effective_base_url.join(source) else {
            diagnostics.push(ImageDiscoveryDiagnostic {
                node: Some(node),
                code: ImageDiscoveryDiagnosticCode::InvalidSourceUrl,
                message: format!("img src '{source}' is not a valid URL reference"),
            });
            continue;
        };
        if requested_url.as_str().len() > limits.max_url_bytes {
            diagnostics.push(ImageDiscoveryDiagnostic {
                node: Some(node),
                code: ImageDiscoveryDiagnosticCode::UrlBytesLimit,
                message: format!(
                    "resolved img URL uses {} bytes; limit is {}",
                    requested_url.as_str().len(),
                    limits.max_url_bytes
                ),
            });
            continue;
        }
        if !matches!(requested_url.scheme(), "http" | "https" | "data") {
            diagnostics.push(ImageDiscoveryDiagnostic {
                node: Some(node),
                code: ImageDiscoveryDiagnosticCode::UnsupportedScheme,
                message: format!(
                    "img URL scheme '{}' is not supported by the image loader",
                    requested_url.scheme()
                ),
            });
            continue;
        }
        if resources.len() >= limits.max_resources {
            diagnostics.push(ImageDiscoveryDiagnostic {
                node: Some(node),
                code: ImageDiscoveryDiagnosticCode::ResourceLimit,
                message: format!("image resource limit ({}) exceeded", limits.max_resources),
            });
            continue;
        }
        resources.push(DiscoveredImage {
            source_order: resources.len(),
            key: ImageResourceKey {
                owner: node,
                requested_url,
                source_snapshot: source.to_owned(),
                source: ImageSource::Element,
            },
        });
    }

    ImageDiscovery {
        revision: dom.revision(),
        effective_base_url,
        resources,
        diagnostics,
    }
}

/// Add fetchable computed `background-image: url(...)` values to ordinary
/// HTML image discovery. Computed URLs are resolved against the document's
/// effective base URL.
#[must_use]
pub fn discover_images_with_styles(
    dom: &Dom,
    styles: &BTreeMap<NodeId, ComputedStyle>,
    document_url: &Url,
    limits: ImageLimits,
) -> ImageDiscovery {
    let mut discovery = discover_images(dom, document_url, limits);
    let (elements, _) = collect_elements(dom, limits.max_discovery_nodes);
    for node in elements {
        if discovery.resources.len() >= limits.max_resources {
            break;
        }
        let Some(value) = styles
            .get(&node)
            .and_then(|style| style.typed("background-image"))
            .and_then(|value| match value {
                crate::css::properties::TypedPropertyValue::BackgroundImage(value) => Some(value),
                _ => None,
            })
        else {
            continue;
        };
        let Some(reference) = background_url(value) else {
            continue;
        };
        let Ok(requested_url) = discovery.effective_base_url.join(reference) else {
            continue;
        };
        if !matches!(requested_url.scheme(), "http" | "https" | "data")
            || requested_url.as_str().len() > limits.max_url_bytes
        {
            continue;
        }
        if discovery.resources.iter().any(|resource| {
            resource.key.owner == node
                && resource.key.requested_url == requested_url
                && resource.key.source == ImageSource::CssBackground
        }) {
            continue;
        }
        discovery.resources.push(DiscoveredImage {
            source_order: discovery.resources.len(),
            key: ImageResourceKey {
                owner: node,
                requested_url,
                source_snapshot: value.clone(),
                source: ImageSource::CssBackground,
            },
        });
    }
    discovery
}

#[must_use]
pub fn background_url(value: &str) -> Option<&str> {
    let value = value.trim();
    let inner = value.strip_prefix("url(")?.strip_suffix(')')?.trim();
    Some(inner.trim_matches(['\'', '"']))
}

/// Whether a completed request still represents the element's current image
/// source and current document-base resolution.
#[must_use]
pub fn image_key_is_current(dom: &Dom, document_url: &Url, key: &ImageResourceKey) -> bool {
    if key.source == ImageSource::CssBackground {
        return true;
    }
    let Some(element) = html_element(dom, key.owner) else {
        return false;
    };
    let Some(source) = element_image_source(element) else {
        return false;
    };
    if element.local_name != "img" || source != key.source_snapshot {
        return false;
    }
    let (elements, _) = collect_elements(dom, usize::MAX);
    let mut diagnostics = Vec::new();
    let base = effective_base_url(dom, &elements, document_url, &mut diagnostics);
    base.join(source).is_ok_and(|url| url == key.requested_url)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    WebP,
}

impl ImageFormat {
    #[must_use]
    pub fn from_media_type(media_type: &str) -> Option<Self> {
        match media_type.to_ascii_lowercase().as_str() {
            "image/png" => Some(Self::Png),
            "image/jpeg" | "image/pjpeg" => Some(Self::Jpeg),
            "image/gif" => Some(Self::Gif),
            "image/webp" => Some(Self::WebP),
            _ => None,
        }
    }

    #[must_use]
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::WebP => "image/webp",
        }
    }

    const fn codec(self) -> CodecImageFormat {
        match self {
            Self::Png => CodecImageFormat::Png,
            Self::Jpeg => CodecImageFormat::Jpeg,
            Self::Gif => CodecImageFormat::Gif,
            Self::WebP => CodecImageFormat::WebP,
        }
    }
}

/// Recognize a supported encoded-image signature without decoding pixels.
#[must_use]
pub fn sniff_image_format(bytes: &[u8]) -> Option<ImageFormat> {
    match image_codec::guess_format(bytes).ok()? {
        CodecImageFormat::Png => Some(ImageFormat::Png),
        CodecImageFormat::Jpeg => Some(ImageFormat::Jpeg),
        CodecImageFormat::Gif => Some(ImageFormat::Gif),
        CodecImageFormat::WebP => Some(ImageFormat::WebP),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedImage {
    width: u32,
    height: u32,
    pixels: Vec<Color>,
}

impl DecodedImage {
    /// Construct an already-decoded straight-alpha RGBA image.
    ///
    /// # Errors
    ///
    /// Returns a dimension error when `pixels` is not exactly `width * height`.
    pub fn from_pixels(
        width: u32,
        height: u32,
        pixels: Vec<Color>,
    ) -> Result<Self, ImageDecodeError> {
        let expected = pixel_len(width, height).ok_or(ImageDecodeError::DimensionOverflow)?;
        if pixels.len() != expected {
            return Err(ImageDecodeError::PixelBufferLength {
                actual: pixels.len(),
                expected,
            });
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    #[must_use]
    pub const fn intrinsic_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    #[must_use]
    pub fn pixels(&self) -> &[Color] {
        &self.pixels
    }

    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<Color> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let index = usize::try_from(y)
            .ok()?
            .checked_mul(usize::try_from(self.width).ok()?)?
            .checked_add(usize::try_from(x).ok()?)?;
        self.pixels.get(index).copied()
    }

    #[must_use]
    pub fn decoded_bytes(&self) -> usize {
        self.pixels.len().saturating_mul(4)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageDecodeError {
    EncodedBytesLimit { actual: usize, limit: usize },
    WidthLimit { actual: u32, limit: u32 },
    HeightLimit { actual: u32, limit: u32 },
    PixelLimit { actual: u64, limit: u64 },
    DecodedBytesLimit { actual: usize, limit: usize },
    DimensionOverflow,
    PixelBufferLength { actual: usize, expected: usize },
    Codec(String),
}

impl fmt::Display for ImageDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EncodedBytesLimit { actual, limit } => {
                write!(
                    formatter,
                    "encoded image uses {actual} bytes; limit is {limit}"
                )
            }
            Self::WidthLimit { actual, limit } => {
                write!(formatter, "image width is {actual}; limit is {limit}")
            }
            Self::HeightLimit { actual, limit } => {
                write!(formatter, "image height is {actual}; limit is {limit}")
            }
            Self::PixelLimit { actual, limit } => {
                write!(formatter, "image has {actual} pixels; limit is {limit}")
            }
            Self::DecodedBytesLimit { actual, limit } => {
                write!(
                    formatter,
                    "decoded image uses {actual} bytes; limit is {limit}"
                )
            }
            Self::DimensionOverflow => formatter.write_str("image dimensions overflow capacity"),
            Self::PixelBufferLength { actual, expected } => write!(
                formatter,
                "decoded pixel buffer has {actual} pixels; expected {expected}"
            ),
            Self::Codec(message) => write!(formatter, "image decode failed: {message}"),
        }
    }
}

impl Error for ImageDecodeError {}

/// Decode one still image. Animated GIF/WebP inputs deliberately expose only
/// the codec's first composited frame through this still-image API.
///
/// # Errors
///
/// Returns an explicit resource-limit or codec error before retaining pixels.
pub fn decode_image(
    bytes: &[u8],
    format: ImageFormat,
    limits: ImageLimits,
) -> Result<DecodedImage, ImageDecodeError> {
    if bytes.len() > limits.max_encoded_bytes {
        return Err(ImageDecodeError::EncodedBytesLimit {
            actual: bytes.len(),
            limit: limits.max_encoded_bytes,
        });
    }
    let reader = ImageReader::with_format(Cursor::new(bytes), format.codec());
    let (width, height) = reader
        .into_dimensions()
        .map_err(|error| ImageDecodeError::Codec(error.to_string()))?;
    enforce_dimensions(width, height, limits)?;

    let decoded = ImageReader::with_format(Cursor::new(bytes), format.codec())
        .decode()
        .map_err(|error| ImageDecodeError::Codec(error.to_string()))?
        .into_rgba8();
    if decoded.dimensions() != (width, height) {
        return Err(ImageDecodeError::Codec(
            "decoded dimensions differ from header dimensions".to_owned(),
        ));
    }
    let pixels = decoded
        .pixels()
        .map(|pixel| Color::rgba(pixel[0], pixel[1], pixel[2], pixel[3]))
        .collect();
    DecodedImage::from_pixels(width, height, pixels)
}

fn enforce_dimensions(
    width: u32,
    height: u32,
    limits: ImageLimits,
) -> Result<(), ImageDecodeError> {
    if width > limits.max_width {
        return Err(ImageDecodeError::WidthLimit {
            actual: width,
            limit: limits.max_width,
        });
    }
    if height > limits.max_height {
        return Err(ImageDecodeError::HeightLimit {
            actual: height,
            limit: limits.max_height,
        });
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(ImageDecodeError::DimensionOverflow)?;
    if pixels > limits.max_pixels {
        return Err(ImageDecodeError::PixelLimit {
            actual: pixels,
            limit: limits.max_pixels,
        });
    }
    let decoded_bytes = usize::try_from(pixels)
        .ok()
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(ImageDecodeError::DimensionOverflow)?;
    if decoded_bytes > limits.max_decoded_bytes {
        return Err(ImageDecodeError::DecodedBytesLimit {
            actual: decoded_bytes,
            limit: limits.max_decoded_bytes,
        });
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct LoadedImage {
    pub id: ImageResourceId,
    pub key: ImageResourceKey,
    pub image: Arc<DecodedImage>,
}

#[derive(Clone, Debug, Default)]
pub struct ImageResources {
    by_node_url: BTreeMap<(NodeId, String), LoadedImage>,
    by_id: BTreeMap<ImageResourceId, Arc<DecodedImage>>,
    next_id: u64,
    decoded_bytes: usize,
}

impl ImageResources {
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_node_url.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_node_url.is_empty()
    }

    #[must_use]
    pub const fn decoded_bytes(&self) -> usize {
        self.decoded_bytes
    }

    #[must_use]
    pub fn get_for_node(&self, node: NodeId) -> Option<&LoadedImage> {
        self.by_node_url
            .values()
            .find(|loaded| loaded.key.owner == node && loaded.key.source == ImageSource::Element)
    }

    #[must_use]
    pub fn get_for_node_url(&self, node: NodeId, requested_url: &Url) -> Option<&LoadedImage> {
        self.by_node_url.get(&(node, requested_url.to_string()))
    }

    #[must_use]
    pub fn get_css_background(&self, node: NodeId, snapshot: &str) -> Option<&LoadedImage> {
        self.by_node_url.values().find(|loaded| {
            loaded.key.owner == node
                && loaded.key.source == ImageSource::CssBackground
                && loaded.key.source_snapshot == snapshot
        })
    }

    #[must_use]
    pub fn get(&self, id: ImageResourceId) -> Option<&DecodedImage> {
        self.by_id.get(&id).map(Arc::as_ref)
    }

    /// Replace the image for one DOM node with a newly allocated resource ID.
    /// New IDs ensure display-list diffs observe same-node content changes.
    ///
    /// # Errors
    ///
    /// Returns a store-limit error without changing the existing resource.
    pub fn insert(
        &mut self,
        key: ImageResourceKey,
        image: DecodedImage,
        limits: ImageLimits,
    ) -> Result<ImageResourceId, ImageStoreError> {
        let map_key = (key.owner, key.requested_url.to_string());
        let replaced_bytes = self
            .by_node_url
            .get(&map_key)
            .map_or(0, |loaded| loaded.image.decoded_bytes());
        let new_total = self
            .decoded_bytes
            .saturating_sub(replaced_bytes)
            .checked_add(image.decoded_bytes())
            .ok_or(ImageStoreError::TotalDecodedBytesLimit {
                actual: usize::MAX,
                limit: limits.max_total_decoded_bytes,
            })?;
        if new_total > limits.max_total_decoded_bytes {
            return Err(ImageStoreError::TotalDecodedBytesLimit {
                actual: new_total,
                limit: limits.max_total_decoded_bytes,
            });
        }
        if !self.by_node_url.contains_key(&map_key)
            && self.by_node_url.len() >= limits.max_resources
        {
            return Err(ImageStoreError::ResourceLimit {
                limit: limits.max_resources,
            });
        }
        let next = self
            .next_id
            .checked_add(1)
            .ok_or(ImageStoreError::IdentifierExhausted)?;
        let id = ImageResourceId(next);
        let image = Arc::new(image);
        if let Some(previous) = self.by_node_url.remove(&map_key) {
            self.by_id.remove(&previous.id);
        }
        self.by_id.insert(id, Arc::clone(&image));
        self.by_node_url
            .insert(map_key, LoadedImage { id, key, image });
        self.next_id = next;
        self.decoded_bytes = new_total;
        Ok(id)
    }

    pub fn remove_node(&mut self, node: NodeId) -> Option<LoadedImage> {
        let key = self
            .by_node_url
            .keys()
            .find(|(owner, _)| *owner == node)
            .cloned()?;
        let loaded = self.by_node_url.remove(&key)?;
        self.by_id.remove(&loaded.id);
        self.decoded_bytes = self
            .decoded_bytes
            .saturating_sub(loaded.image.decoded_bytes());
        Some(loaded)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageStoreError {
    ResourceLimit { limit: usize },
    TotalDecodedBytesLimit { actual: usize, limit: usize },
    IdentifierExhausted,
}

impl fmt::Display for ImageStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResourceLimit { limit } => {
                write!(formatter, "decoded image resource limit ({limit}) exceeded")
            }
            Self::TotalDecodedBytesLimit { actual, limit } => write!(
                formatter,
                "decoded images use {actual} bytes in total; limit is {limit}"
            ),
            Self::IdentifierExhausted => formatter.write_str("image resource IDs exhausted"),
        }
    }
}

impl Error for ImageStoreError {}

fn collect_elements(dom: &Dom, limit: usize) -> (Vec<NodeId>, Vec<ImageDiscoveryDiagnostic>) {
    let mut elements = Vec::new();
    let mut diagnostics = Vec::new();
    let mut pending = vec![dom.document()];
    let mut visited = 0_usize;
    while let Some(node) = pending.pop() {
        if visited >= limit {
            diagnostics.push(ImageDiscoveryDiagnostic {
                node: Some(node),
                code: ImageDiscoveryDiagnosticCode::NodeLimit,
                message: format!("image discovery node limit ({limit}) exceeded"),
            });
            break;
        }
        visited += 1;
        let Some(current) = dom.node(node) else {
            continue;
        };
        if matches!(current.kind(), NodeKind::Element(_)) {
            elements.push(node);
        }
        pending.extend(current.children().iter().rev());
    }
    (elements, diagnostics)
}

fn effective_base_url(
    dom: &Dom,
    elements: &[NodeId],
    document_url: &Url,
    diagnostics: &mut Vec<ImageDiscoveryDiagnostic>,
) -> Url {
    for node in elements {
        let Some(element) = html_element(dom, *node) else {
            continue;
        };
        if element.local_name != "base" {
            continue;
        }
        let Some(href) = attribute(element, "href") else {
            continue;
        };
        match document_url.join(href) {
            Ok(url) => return url,
            Err(error) => diagnostics.push(ImageDiscoveryDiagnostic {
                node: Some(*node),
                code: ImageDiscoveryDiagnosticCode::InvalidBaseUrl,
                message: format!("base href '{href}' is invalid: {error}"),
            }),
        }
    }
    document_url.clone()
}

fn html_element(dom: &Dom, node: NodeId) -> Option<&ElementData> {
    let NodeKind::Element(element) = dom.node(node)?.kind() else {
        return None;
    };
    (element.namespace == Namespace::Html).then_some(element)
}

fn attribute<'a>(element: &'a ElementData, name: &str) -> Option<&'a str> {
    element
        .attributes
        .iter()
        .find(|attribute| attribute.namespace.is_none() && attribute.local_name == name)
        .map(|attribute| attribute.value.as_str())
}

fn element_image_source(element: &ElementData) -> Option<&str> {
    attribute(element, "src")
        .map(str::trim)
        .filter(|source| !source.is_empty())
        .or_else(|| {
            LAZY_IMAGE_SOURCE_ATTRIBUTES.iter().find_map(|name| {
                attribute(element, name)
                    .map(str::trim)
                    .filter(|source| !source.is_empty())
            })
        })
}

fn pixel_len(width: u32, height: u32) -> Option<usize> {
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image_codec::{DynamicImage, ImageFormat as CodecImageFormat, Rgba, RgbaImage};
    use url::Url;

    use crate::html::parse_document;

    use super::{
        ImageDecodeError, ImageDiscoveryDiagnosticCode, ImageFormat, ImageLimits, ImageResources,
        decode_image, discover_images, image_key_is_current,
    };

    fn png_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        let image = RgbaImage::from_fn(2, 1, |x, _| {
            if x == 0 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 255, 128])
            }
        });
        DynamicImage::ImageRgba8(image)
            .write_to(&mut Cursor::new(&mut bytes), CodecImageFormat::Png)
            .expect("encode test PNG");
        bytes
    }

    fn by_id(dom: &crate::dom::Dom, id: &str) -> crate::dom::NodeId {
        let mut pending = vec![dom.document()];
        while let Some(node) = pending.pop() {
            if dom.attribute(node, "id").ok().flatten() == Some(id) {
                return node;
            }
            pending.extend(dom.children(node).unwrap_or_default().iter().rev());
        }
        panic!("missing test element #{id}");
    }

    #[test]
    fn discovers_src_against_first_base_and_diagnoses_srcset() {
        let parsed = parse_document(
            "<base href='https://cdn.example/assets/'><img src='a.png' srcset='a@2x.png 2x'><img><img src=''>",
        );
        let document_url = Url::parse("https://example.test/page/index.html").unwrap();
        let discovery = discover_images(&parsed.dom, &document_url, ImageLimits::default());

        assert_eq!(discovery.resources.len(), 1);
        assert_eq!(
            discovery.resources[0].key.requested_url.as_str(),
            "https://cdn.example/assets/a.png"
        );
        let codes = discovery
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();
        assert!(codes.contains(&ImageDiscoveryDiagnosticCode::SrcsetUnsupported));
        assert!(!codes.contains(&ImageDiscoveryDiagnosticCode::MissingSource));
        assert!(!codes.contains(&ImageDiscoveryDiagnosticCode::EmptySource));
    }

    #[test]
    fn discovers_data_src_when_src_is_missing_or_empty_without_warning() {
        let parsed = parse_document(
            "<img id=lazy data-src='lazy.png'><img id=plain><img id=empty src='' data-src='fallback.png'><img src='normal.png' data-src='ignored.png'>",
        );
        let document_url = Url::parse("https://example.test/page/").unwrap();
        let discovery = discover_images(&parsed.dom, &document_url, ImageLimits::default());

        let urls = discovery
            .resources
            .iter()
            .map(|resource| resource.key.requested_url.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            urls,
            vec![
                "https://example.test/page/lazy.png",
                "https://example.test/page/fallback.png",
                "https://example.test/page/normal.png",
            ]
        );
        assert!(discovery.diagnostics.is_empty());
    }

    #[test]
    fn discovers_data_url_images_without_an_unsupported_scheme_diagnostic() {
        let parsed = parse_document("<img src='data:image/png;base64,AA=='>");
        let document_url = Url::parse("https://example.test/page/").unwrap();
        let discovery = discover_images(&parsed.dom, &document_url, ImageLimits::default());

        assert_eq!(discovery.resources.len(), 1);
        assert_eq!(
            discovery.resources[0].key.requested_url.as_str(),
            "data:image/png;base64,AA=="
        );
        assert!(discovery.diagnostics.is_empty());
    }

    #[test]
    fn stale_dynamic_src_is_rejected_without_rejecting_unrelated_mutations() {
        let mut parsed = parse_document("<img id=hero src='old.png'><p>before</p>");
        let document_url = Url::parse("https://example.test/").unwrap();
        let discovery = discover_images(&parsed.dom, &document_url, ImageLimits::default());
        let key = discovery.resources[0].key.clone();
        let image = by_id(&parsed.dom, "hero");

        parsed.dom.set_attribute(image, "alt", "changed").unwrap();
        assert!(image_key_is_current(&parsed.dom, &document_url, &key));
        parsed.dom.set_attribute(image, "src", "new.png").unwrap();
        assert!(!image_key_is_current(&parsed.dom, &document_url, &key));
    }

    #[test]
    fn stale_dynamic_lazy_source_is_rejected_without_rejecting_unrelated_mutations() {
        let mut parsed = parse_document("<img id=hero data-src='old.png'><p>before</p>");
        let document_url = Url::parse("https://example.test/").unwrap();
        let discovery = discover_images(&parsed.dom, &document_url, ImageLimits::default());
        let key = discovery.resources[0].key.clone();
        let image = by_id(&parsed.dom, "hero");

        parsed.dom.set_attribute(image, "alt", "changed").unwrap();
        assert!(image_key_is_current(&parsed.dom, &document_url, &key));
        parsed
            .dom
            .set_attribute(image, "data-src", "new.png")
            .unwrap();
        assert!(!image_key_is_current(&parsed.dom, &document_url, &key));
    }

    #[test]
    fn decodes_rgba_and_enforces_header_limits_before_retaining_pixels() {
        let bytes = png_bytes();
        let decoded = decode_image(&bytes, ImageFormat::Png, ImageLimits::default()).unwrap();
        assert_eq!(decoded.intrinsic_size(), (2, 1));
        assert_eq!(
            decoded.pixel(0, 0),
            Some(crate::paint::Color::rgb(255, 0, 0))
        );
        assert_eq!(
            decoded.pixel(1, 0),
            Some(crate::paint::Color::rgba(0, 0, 255, 128))
        );

        let error = decode_image(
            &bytes,
            ImageFormat::Png,
            ImageLimits {
                max_width: 1,
                ..ImageLimits::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error,
            ImageDecodeError::WidthLimit {
                actual: 2,
                limit: 1
            }
        );
    }

    #[test]
    fn replacing_a_node_allocates_a_new_display_resource_identity() {
        let parsed = parse_document("<img src='a.png'>");
        let document_url = Url::parse("https://example.test/").unwrap();
        let discovery = discover_images(&parsed.dom, &document_url, ImageLimits::default());
        let key = discovery.resources[0].key.clone();
        let image = decode_image(&png_bytes(), ImageFormat::Png, ImageLimits::default()).unwrap();
        let mut resources = ImageResources::default();
        let first = resources
            .insert(key.clone(), image.clone(), ImageLimits::default())
            .unwrap();
        let second = resources
            .insert(key, image, ImageLimits::default())
            .unwrap();
        assert_ne!(first, second);
        assert!(resources.get(first).is_none());
        assert_eq!(resources.get(second).unwrap().intrinsic_size(), (2, 1));
    }
}
