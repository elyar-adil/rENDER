//! Image discovery, bounded decoding, and decoded-resource identity.
//!
//! This module performs no I/O. Browser/document coordinators turn discovery
//! entries into network requests, then insert successfully decoded resources
//! into [`ImageResources`]. Resource keys retain the selected source value so
//! a response for an older dynamic attribute value can be rejected.

#![allow(clippy::cast_precision_loss)]

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::io::Cursor;
use std::sync::Arc;

use image_codec::{ImageFormat as CodecImageFormat, ImageReader};
use url::Url;

use crate::css::cascade::media_query_list_matches;
use crate::css::computed::ComputedStyle;
use crate::css::selector::MatchContext;
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
    pub selection_context: ImageSelectionContext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageSource {
    Element,
    VideoPoster,
    CssBackground,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageSelectionContext {
    pub viewport_width: u32,
    pub viewport_height: u32,
    /// Device pixel ratio multiplied by 1,000.
    pub device_pixel_ratio_milli: u32,
}

impl Default for ImageSelectionContext {
    fn default() -> Self {
        Self {
            viewport_width: 1_024,
            viewport_height: 768,
            device_pixel_ratio_milli: 1_000,
        }
    }
}

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
    discover_images_with_context(dom, document_url, limits, ImageSelectionContext::default())
}

/// Discover the image candidate selected for a concrete viewport and device
/// pixel ratio. This covers `picture`, `srcset`, `sizes`, ordinary `img src`,
/// and `video poster` through one resource pipeline.
#[must_use]
pub fn discover_images_with_context(
    dom: &Dom,
    document_url: &Url,
    limits: ImageLimits,
    context: ImageSelectionContext,
) -> ImageDiscovery {
    let (elements, mut diagnostics) = collect_elements(dom, limits.max_discovery_nodes);
    let effective_base_url = effective_base_url(dom, &elements, document_url, &mut diagnostics);
    let mut resources = Vec::new();

    for node in elements {
        let Some(element) = html_element(dom, node) else {
            continue;
        };
        let selected = match element.local_name.as_str() {
            "img" => select_image_source(dom, node, context),
            "video" => attribute(element, "poster")
                .map(str::trim)
                .filter(|source| !source.is_empty())
                .map(|source| SelectedImageSource {
                    reference: source.to_owned(),
                    snapshot: source.to_owned(),
                    source: ImageSource::VideoPoster,
                }),
            _ => None,
        };
        let Some(selected) = selected else {
            continue;
        };
        let source = selected.reference.as_str();
        if source.len() > limits.max_url_bytes {
            diagnostics.push(ImageDiscoveryDiagnostic {
                node: Some(node),
                code: ImageDiscoveryDiagnosticCode::UrlBytesLimit,
                message: format!(
                    "image URL reference uses {} bytes; limit is {}",
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
                message: format!("image source '{source}' is not a valid URL reference"),
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
                    "image URL scheme '{}' is not supported by the image loader",
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
                source_snapshot: selected.snapshot,
                source: selected.source,
                selection_context: context,
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
    discover_images_with_styles_and_context(
        dom,
        styles,
        document_url,
        limits,
        ImageSelectionContext::default(),
    )
}

#[must_use]
pub fn discover_images_with_styles_and_context(
    dom: &Dom,
    styles: &BTreeMap<NodeId, ComputedStyle>,
    document_url: &Url,
    limits: ImageLimits,
    context: ImageSelectionContext,
) -> ImageDiscovery {
    let mut discovery = discover_images_with_context(dom, document_url, limits, context);
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
                selection_context: context,
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
    let selected = match key.source {
        ImageSource::Element if element.local_name == "img" => {
            select_image_source(dom, key.owner, key.selection_context)
        }
        ImageSource::VideoPoster if element.local_name == "video" => attribute(element, "poster")
            .map(str::trim)
            .filter(|source| !source.is_empty())
            .map(|source| SelectedImageSource {
                reference: source.to_owned(),
                snapshot: source.to_owned(),
                source: ImageSource::VideoPoster,
            }),
        _ => None,
    };
    let Some(selected) = selected else {
        return false;
    };
    if selected.snapshot != key.source_snapshot || selected.source != key.source {
        return false;
    }
    let (elements, _) = collect_elements(dom, usize::MAX);
    let mut diagnostics = Vec::new();
    let base = effective_base_url(dom, &elements, document_url, &mut diagnostics);
    base.join(&selected.reference)
        .is_ok_and(|url| url == key.requested_url)
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
        self.by_node_url.values().find(|loaded| {
            loaded.key.owner == node
                && matches!(
                    loaded.key.source,
                    ImageSource::Element | ImageSource::VideoPoster
                )
        })
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

struct SelectedImageSource {
    reference: String,
    snapshot: String,
    source: ImageSource,
}

#[derive(Clone, Copy)]
enum SrcsetDescriptor {
    Density(f32),
    Width(u32),
}

struct SrcsetCandidate<'a> {
    url: &'a str,
    descriptor: SrcsetDescriptor,
}

fn select_image_source(
    dom: &Dom,
    image: NodeId,
    context: ImageSelectionContext,
) -> Option<SelectedImageSource> {
    let element = html_element(dom, image)?;
    let snapshot = image_selection_snapshot(dom, image);
    if let Some(parent) = dom.parent(image)
        && html_element(dom, parent).is_some_and(|element| element.local_name == "picture")
    {
        for child in dom.children(parent).unwrap_or_default() {
            if *child == image {
                break;
            }
            let Some(source) = html_element(dom, *child) else {
                continue;
            };
            if source.local_name != "source"
                || attribute(source, "type").is_some_and(|value| !supported_image_type(value))
                || attribute(source, "media").is_some_and(|value| {
                    !media_query_list_matches(value, &selection_match_context(context))
                })
            {
                continue;
            }
            if let Some(reference) = attribute(source, "srcset").and_then(|srcset| {
                select_srcset_candidate(srcset, attribute(source, "sizes"), context)
            }) {
                return Some(SelectedImageSource {
                    reference: reference.to_owned(),
                    snapshot,
                    source: ImageSource::Element,
                });
            }
        }
    }
    let reference = attribute(element, "srcset")
        .and_then(|srcset| select_srcset_candidate(srcset, attribute(element, "sizes"), context))
        .or_else(|| {
            attribute(element, "src")
                .map(str::trim)
                .filter(|source| !source.is_empty())
        })?;
    Some(SelectedImageSource {
        reference: reference.to_owned(),
        snapshot,
        source: ImageSource::Element,
    })
}

fn selection_match_context(context: ImageSelectionContext) -> MatchContext {
    MatchContext {
        viewport_width: Some(context.viewport_width as f32),
        viewport_height: Some(context.viewport_height as f32),
        ..MatchContext::default()
    }
}

fn supported_image_type(value: &str) -> bool {
    matches!(
        value
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "image/png" | "image/jpeg" | "image/pjpeg" | "image/gif" | "image/webp"
    )
}

fn image_selection_snapshot(dom: &Dom, image: NodeId) -> String {
    let mut snapshot = String::new();
    if let Some(parent) = dom.parent(image)
        && html_element(dom, parent).is_some_and(|element| element.local_name == "picture")
    {
        for child in dom.children(parent).unwrap_or_default() {
            if *child == image {
                break;
            }
            let Some(source) = html_element(dom, *child) else {
                continue;
            };
            if source.local_name == "source" {
                snapshot.push_str("source[");
                for name in ["srcset", "sizes", "media", "type"] {
                    snapshot.push_str(name);
                    snapshot.push('=');
                    snapshot.push_str(attribute(source, name).unwrap_or_default());
                    snapshot.push(';');
                }
                snapshot.push(']');
            }
        }
    }
    snapshot.push_str("img[");
    for name in ["src", "srcset", "sizes"] {
        snapshot.push_str(name);
        snapshot.push('=');
        snapshot.push_str(
            html_element(dom, image)
                .and_then(|element| attribute(element, name))
                .unwrap_or_default(),
        );
        snapshot.push(';');
    }
    snapshot.push(']');
    snapshot
}

fn select_srcset_candidate<'a>(
    srcset: &'a str,
    sizes: Option<&str>,
    context: ImageSelectionContext,
) -> Option<&'a str> {
    let candidates = parse_srcset(srcset);
    let source_size = parse_sizes(sizes, context).max(1.0);
    let target_density = context.device_pixel_ratio_milli as f32 / 1_000.0;
    candidates
        .iter()
        .filter_map(|candidate| {
            let density = match candidate.descriptor {
                SrcsetDescriptor::Density(density) => density,
                SrcsetDescriptor::Width(width) => width as f32 / source_size,
            };
            density
                .is_finite()
                .then_some((candidate.url, density.max(0.0)))
        })
        .min_by(|(_, left), (_, right)| {
            let left_distance = if *left >= target_density {
                *left - target_density
            } else {
                f32::MAX / 2.0 + target_density - *left
            };
            let right_distance = if *right >= target_density {
                *right - target_density
            } else {
                f32::MAX / 2.0 + target_density - *right
            };
            left_distance.total_cmp(&right_distance)
        })
        .map(|(url, _)| url)
}

fn parse_srcset(srcset: &str) -> Vec<SrcsetCandidate<'_>> {
    srcset
        .split(',')
        .filter_map(|candidate| {
            let mut parts = candidate.split_ascii_whitespace();
            let url = parts.next()?.trim();
            if url.is_empty() {
                return None;
            }
            let descriptor = match parts.next() {
                None => SrcsetDescriptor::Density(1.0),
                Some(value) if value.ends_with('x') => SrcsetDescriptor::Density(
                    value
                        .strip_suffix('x')?
                        .parse::<f32>()
                        .ok()
                        .filter(|v| *v > 0.0)?,
                ),
                Some(value) if value.ends_with('w') => SrcsetDescriptor::Width(
                    value
                        .strip_suffix('w')?
                        .parse::<u32>()
                        .ok()
                        .filter(|v| *v > 0)?,
                ),
                Some(_) => return None,
            };
            parts
                .next()
                .is_none()
                .then_some(SrcsetCandidate { url, descriptor })
        })
        .collect()
}

fn parse_sizes(sizes: Option<&str>, context: ImageSelectionContext) -> f32 {
    let Some(sizes) = sizes else {
        return context.viewport_width as f32;
    };
    for entry in split_top_level_commas(sizes) {
        let entry = entry.trim();
        let split = entry.rfind(char::is_whitespace);
        let (media, length) = split.map_or(("", entry), |index| {
            (entry[..index].trim(), entry[index..].trim())
        });
        if !media.is_empty() && !media_query_list_matches(media, &selection_match_context(context))
        {
            continue;
        }
        if let Some(length) = parse_source_size(length, context.viewport_width) {
            return length;
        }
    }
    context.viewport_width as f32
}

fn split_top_level_commas(value: &str) -> Vec<&str> {
    let mut entries = Vec::new();
    let mut depth = 0_u32;
    let mut start = 0;
    for (index, character) in value.char_indices() {
        match character {
            '(' => depth = depth.saturating_add(1),
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                entries.push(&value[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    entries.push(&value[start..]);
    entries
}

fn parse_source_size(value: &str, viewport_width: u32) -> Option<f32> {
    let value = value.trim().to_ascii_lowercase();
    for (unit, factor) in [
        ("px", 1.0),
        ("em", 16.0),
        ("rem", 16.0),
        ("vw", viewport_width as f32 / 100.0),
    ] {
        if let Some(number) = value.strip_suffix(unit) {
            return number
                .trim()
                .parse::<f32>()
                .ok()
                .filter(|number| number.is_finite() && *number >= 0.0)
                .map(|number| number * factor);
        }
    }
    None
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
        ImageDecodeError, ImageFormat, ImageLimits, ImageResources, ImageSelectionContext,
        decode_image, discover_images, discover_images_with_context, image_key_is_current,
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
    fn discovers_srcset_against_first_base() {
        let parsed = parse_document(
            "<base href='https://cdn.example/assets/'><img src='a.png' srcset='a@2x.png 2x'><img><img src=''>",
        );
        let document_url = Url::parse("https://example.test/page/index.html").unwrap();
        let discovery = discover_images(&parsed.dom, &document_url, ImageLimits::default());

        assert_eq!(discovery.resources.len(), 1);
        assert_eq!(
            discovery.resources[0].key.requested_url.as_str(),
            "https://cdn.example/assets/a@2x.png"
        );
        assert!(discovery.diagnostics.is_empty());
    }

    #[test]
    fn nonstandard_data_attributes_wait_for_script_to_set_src() {
        let mut parsed = parse_document(
            "<img id=lazy data-src='lazy.png'><img id=plain><img id=empty src='' data-src='fallback.png'><img src='normal.png' data-src='ignored.png'>",
        );
        let document_url = Url::parse("https://example.test/page/").unwrap();
        let mut discovery = discover_images(&parsed.dom, &document_url, ImageLimits::default());

        assert_eq!(discovery.resources.len(), 1);
        let lazy = by_id(&parsed.dom, "lazy");
        parsed.dom.set_attribute(lazy, "src", "lazy.png").unwrap();
        discovery = discover_images(&parsed.dom, &document_url, ImageLimits::default());
        assert_eq!(discovery.resources.len(), 2);
        assert!(discovery.diagnostics.is_empty());
    }

    #[test]
    fn picture_density_and_video_poster_use_the_standard_image_pipeline() {
        let parsed = parse_document(
            "<picture><source media='(min-width: 700px)' srcset='wide.png 1x, wide@2x.png 2x'><img src='fallback.png'></picture><video poster='poster.webp'></video>",
        );
        let document_url = Url::parse("https://example.test/assets/").unwrap();
        let discovery = discover_images_with_context(
            &parsed.dom,
            &document_url,
            ImageLimits::default(),
            ImageSelectionContext {
                viewport_width: 800,
                viewport_height: 600,
                device_pixel_ratio_milli: 2_000,
            },
        );

        let urls = discovery
            .resources
            .iter()
            .map(|resource| resource.key.requested_url.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            urls,
            vec![
                "https://example.test/assets/wide@2x.png",
                "https://example.test/assets/poster.webp",
            ]
        );
        assert_eq!(
            discovery.resources[1].key.source,
            super::ImageSource::VideoPoster
        );
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
    fn stale_dynamic_srcset_is_rejected_without_rejecting_unrelated_mutations() {
        let mut parsed = parse_document("<img id=hero srcset='old.png 1x'><p>before</p>");
        let document_url = Url::parse("https://example.test/").unwrap();
        let discovery = discover_images(&parsed.dom, &document_url, ImageLimits::default());
        let key = discovery.resources[0].key.clone();
        let image = by_id(&parsed.dom, "hero");

        parsed.dom.set_attribute(image, "alt", "changed").unwrap();
        assert!(image_key_is_current(&parsed.dom, &document_url, &key));
        parsed
            .dom
            .set_attribute(image, "srcset", "new.png 1x")
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
