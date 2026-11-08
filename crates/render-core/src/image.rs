//! Image discovery, bounded decoding, and decoded-resource identity.
//!
//! This module performs no I/O. Browser/document coordinators turn discovery
//! entries into network requests, then insert successfully decoded resources
//! into [`ImageResources`]. Resource keys retain the selected source value so
//! a response for an older dynamic attribute value can be rejected.

#![allow(clippy::cast_precision_loss)]

pub mod animation;
pub mod inline_svg;
pub mod svg;

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::io::Cursor;
use std::sync::Arc;

use image_codec::{ImageFormat as CodecImageFormat, ImageReader};
use url::Url;

use self::animation::AnimatedImage;
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
    /// The frame a `<video>` element currently presents. Managed by the
    /// video pipeline (installed through [`ImageResources::set_video_frame`]),
    /// not by URL-keyed image loads.
    VideoFrame,
    CssBackground,
    /// A `mask-image: url(...)` layer on an HTML element.
    CssMask,
    /// The bitmap of a `<canvas>` element, published by the canvas engine
    /// (installed through [`ImageResources::set_canvas_bitmap`]), not by a
    /// URL-keyed load.
    Canvas,
    /// The rasterised viewport of the nested document an `<iframe>` displays,
    /// installed through [`ImageResources::set_frame_bitmap`]. It is an image
    /// resource like a canvas bitmap, so the same display command paints it,
    /// but it belongs to the frame's own painting pipeline.
    FrameDocument,
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
    let (mut elements, mut diagnostics) = collect_elements(dom, limits.max_discovery_nodes);
    if let Some(diagnostic) =
        collect_detached_images(dom, &mut elements, limits.max_discovery_nodes)
    {
        diagnostics.push(diagnostic);
    }
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
    for &node in &elements {
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
    for &node in &elements {
        if discovery.resources.len() >= limits.max_resources {
            break;
        }
        let Some(value) = styles
            .get(&node)
            .and_then(|style| style.get("mask-image"))
            .map(|value| value.css_text().to_owned())
        else {
            continue;
        };
        let Some(reference) = background_url(&value) else {
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
                && resource.key.source == ImageSource::CssMask
        }) {
            continue;
        }
        discovery.resources.push(DiscoveredImage {
            source_order: discovery.resources.len(),
            key: ImageResourceKey {
                owner: node,
                requested_url,
                source_snapshot: value.clone(),
                source: ImageSource::CssMask,
                selection_context: context,
            },
        });
    }
    discovery
}

/// The first fetchable URL in a `background-image` or `mask-image` value.
///
/// CSS Backgrounds 3 §3.2 makes the value a comma-separated layer list, and
/// `image-set()` nests another candidate list inside one layer. Only the first
/// referenced image is returned because paint currently selects one bitmap per
/// element; callers still need discovery to understand the surrounding syntax
/// rather than rejecting the whole declaration.
#[must_use]
pub fn background_url(value: &str) -> Option<&str> {
    split_top_level(value, ',')
        .into_iter()
        .find_map(background_layer_url)
}

fn background_layer_url(layer: &str) -> Option<&str> {
    image_token_url(first_css_token(layer.trim())?)
}

fn image_token_url(token: &str) -> Option<&str> {
    let token = token.trim();
    let lower = token.to_ascii_lowercase();
    if lower.starts_with("url(") && token.ends_with(')') {
        return Some(unquote(token.get(4..token.len() - 1)?.trim()));
    }
    if (lower.starts_with("image-set(") || lower.starts_with("-webkit-image-set("))
        && token.ends_with(')')
    {
        let open = token.find('(')? + 1;
        let candidates = token.get(open..token.len() - 1)?;
        return split_top_level(candidates, ',')
            .into_iter()
            .find_map(background_layer_url);
    }
    if let Some(inner) = token
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            token
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
    {
        return Some(inner);
    }
    None
}

fn unquote(value: &str) -> &str {
    value.trim_matches(['\'', '"'])
}

/// The first component of a CSS function argument, stopping at top-level
/// whitespace while preserving nested functions and quoted strings.
fn first_css_token(value: &str) -> Option<&str> {
    let mut depth = 0_i32;
    let mut quote = None;
    let mut start = None;
    for (index, character) in value.char_indices() {
        match (quote, character) {
            (Some(expected), character) if character == expected => quote = None,
            (None, '\'' | '"') => {
                quote = Some(character);
                start.get_or_insert(index);
            }
            (None, '(') => {
                depth += 1;
                start.get_or_insert(index);
            }
            (None, ')') => depth = (depth - 1).max(0),
            (None, character) if character.is_whitespace() && depth == 0 => {
                if let Some(start) = start {
                    return Some(&value[start..index]);
                }
            }
            (None, _) => {
                start.get_or_insert(index);
            }
            _ => {}
        }
    }
    start.map(|start| &value[start..])
}

fn split_top_level(value: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0_i32;
    let mut quote = None;
    for (index, character) in value.char_indices() {
        match (quote, character) {
            (Some(expected), character) if character == expected => quote = None,
            (None, '\'' | '"') => quote = Some(character),
            (None, '(') => depth += 1,
            (None, ')') => depth = (depth - 1).max(0),
            (None, character) if character == separator && depth == 0 => {
                parts.push(&value[start..index]);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&value[start..]);
    parts
}

/// Whether a completed request still represents the element's current image
/// source and current document-base resolution.
#[must_use]
pub fn image_key_is_current(dom: &Dom, document_url: &Url, key: &ImageResourceKey) -> bool {
    if matches!(
        key.source,
        ImageSource::CssBackground | ImageSource::CssMask
    ) {
        return true;
    }
    // Presented video frames are owned by the video pipeline's lifecycle,
    // not by URL revalidation.
    if matches!(
        key.source,
        ImageSource::VideoFrame | ImageSource::Canvas | ImageSource::FrameDocument
    ) {
        return false;
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
    Avif,
    Svg,
}

impl ImageFormat {
    #[must_use]
    pub fn from_media_type(media_type: &str) -> Option<Self> {
        match media_type.to_ascii_lowercase().as_str() {
            "image/png" => Some(Self::Png),
            "image/jpeg" | "image/pjpeg" => Some(Self::Jpeg),
            "image/gif" => Some(Self::Gif),
            "image/webp" => Some(Self::WebP),
            "image/avif" => Some(Self::Avif),
            "image/svg+xml" | "image/svg" => Some(Self::Svg),
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
            Self::Avif => "image/avif",
            Self::Svg => "image/svg+xml",
        }
    }

    /// The raster-codec backend for this format; `None` for SVG, which the
    /// engine rasterizes itself instead of via the image crate.
    const fn codec(self) -> Option<CodecImageFormat> {
        match self {
            Self::Png => Some(CodecImageFormat::Png),
            Self::Jpeg => Some(CodecImageFormat::Jpeg),
            Self::Gif => Some(CodecImageFormat::Gif),
            Self::WebP => Some(CodecImageFormat::WebP),
            Self::Avif => Some(CodecImageFormat::Avif),
            Self::Svg => None,
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
        CodecImageFormat::Avif => Some(ImageFormat::Avif),
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

    /// Construct from tightly packed 8-bit RGBA bytes (the layout decoded
    /// video frames arrive in).
    ///
    /// # Errors
    ///
    /// Returns a dimension error when `bytes` is not exactly
    /// `width * height * 4`.
    pub fn from_rgba8(width: u32, height: u32, bytes: &[u8]) -> Result<Self, ImageDecodeError> {
        let expected = pixel_len(width, height)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(ImageDecodeError::DimensionOverflow)?;
        if bytes.len() != expected {
            return Err(ImageDecodeError::PixelBufferLength {
                actual: bytes.len(),
                expected,
            });
        }
        let pixels = bytes
            .chunks_exact(4)
            .map(|pixel| Color::rgba(pixel[0], pixel[1], pixel[2], pixel[3]))
            .collect();
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
    FrameLimit { actual: usize, limit: usize },
    EmptyAnimation,
    UnsupportedAnimation(String),
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
            Self::FrameLimit { actual, limit } => {
                write!(
                    formatter,
                    "animation has at least {actual} frames; limit is {limit}"
                )
            }
            Self::EmptyAnimation => formatter.write_str("animation contains no frames"),
            Self::UnsupportedAnimation(format) => {
                write!(formatter, "animation decoding is unsupported for {format}")
            }
            Self::Codec(message) => write!(formatter, "image decode failed: {message}"),
        }
    }
}

impl Error for ImageDecodeError {}

/// Decode one still image. Animated GIF/WebP inputs deliberately expose only
/// the codec's first composited frame through this still-image API; use
/// [`animation::decode_animation`] when the frame sequence is needed.
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
    if format == ImageFormat::Svg {
        return svg::decode_svg(bytes, limits);
    }
    let Some(codec_format) = format.codec() else {
        return Err(ImageDecodeError::Codec(
            "format has no raster codec".to_owned(),
        ));
    };
    let reader = ImageReader::with_format(Cursor::new(bytes), codec_format);
    let (width, height) = reader
        .into_dimensions()
        .map_err(|error| ImageDecodeError::Codec(error.to_string()))?;
    enforce_dimensions(width, height, limits)?;

    let decoded = ImageReader::with_format(Cursor::new(bytes), codec_format)
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
    /// The revision whose pixels a live surface presents. Decoded images are
    /// immutable and leave this `None`; a nested document's rendered viewport
    /// carries the child DOM revision so a display-list diff sees child-only
    /// content changes.
    pub content_revision: Option<DomRevision>,
    /// The decoded frame sequence when the source is animated. `None` for
    /// still resources. The first frame shares its pixel buffer with `image`.
    pub animation: Option<Arc<AnimatedImage>>,
}

impl LoadedImage {
    /// Bytes retained by this resource. An animation already includes its
    /// first frame, which shares `image`'s allocation.
    #[must_use]
    pub fn decoded_bytes(&self) -> usize {
        self.animation.as_ref().map_or_else(
            || self.image.decoded_bytes(),
            |animation| animation.decoded_bytes(),
        )
    }
}

/// The requested-URL component of a canvas bitmap's key. A canvas has no URL,
/// so every bitmap carries this fixed one; the owner node tells them apart.
fn canvas_bitmap_url() -> Url {
    Url::parse("about:canvas").expect("about:canvas is a valid URL")
}

/// The requested-URL component of a frame bitmap's key. A nested document's
/// rendered viewport has no URL of its own here, so the iframe node tells the
/// entries apart.
fn frame_document_url() -> Url {
    Url::parse("about:iframe").expect("about:iframe is a valid URL")
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
        // A presented video frame outranks the element's poster: while a
        // frame is on screen the poster is hidden, exactly like browsers.
        self.by_node_url
            .values()
            .find(|loaded| {
                loaded.key.owner == node && matches!(loaded.key.source, ImageSource::FrameDocument)
            })
            .or_else(|| {
                self.by_node_url.values().find(|loaded| {
                    loaded.key.owner == node && matches!(loaded.key.source, ImageSource::VideoFrame)
                })
            })
            .or_else(|| {
                self.by_node_url.values().find(|loaded| {
                    loaded.key.owner == node && matches!(loaded.key.source, ImageSource::Canvas)
                })
            })
            .or_else(|| {
                self.by_node_url.values().find(|loaded| {
                    loaded.key.owner == node
                        && matches!(
                            loaded.key.source,
                            ImageSource::Element | ImageSource::VideoPoster
                        )
                })
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

    /// The loaded `mask-image: url(...)` layer attached to `node`.
    #[must_use]
    pub fn get_css_mask(&self, node: NodeId, snapshot: &str) -> Option<&LoadedImage> {
        self.by_node_url.values().find(|loaded| {
            loaded.key.owner == node
                && loaded.key.source == ImageSource::CssMask
                && loaded.key.source_snapshot == snapshot
        })
    }

    #[must_use]
    pub fn get(&self, id: ImageResourceId) -> Option<&DecodedImage> {
        self.by_id.get(&id).map(Arc::as_ref)
    }

    /// The decoded image as a shared handle, for the rasterizer's scaled-image
    /// cache. The cache keeps this handle so its pointer identity cannot be
    /// reused by a different image while an entry is alive.
    #[must_use]
    pub fn get_shared(&self, id: ImageResourceId) -> Option<Arc<DecodedImage>> {
        self.by_id.get(&id).cloned()
    }

    /// The decoded frame sequence attached to a resource, when it is animated.
    #[must_use]
    pub fn get_animation(&self, id: ImageResourceId) -> Option<&AnimatedImage> {
        self.by_node_url
            .values()
            .find(|loaded| loaded.id == id)
            .and_then(|loaded| loaded.animation.as_deref())
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
        self.insert_entry(key, Arc::new(image), None, limits)
    }

    /// Copy every entry of `other` into this store, allocating fresh resource
    /// IDs. The embedder uses this to combine the network image store with
    /// the live canvas and video-frame store before handing a document
    /// snapshot to another thread: the display list resolves images by node,
    /// so both stores must be visible to that render.
    ///
    /// # Errors
    ///
    /// Returns the store-limit error of the first entry that does not fit.
    /// Entries copied before it stay, exactly as a sequence of inserts would.
    pub fn overlay(&mut self, other: &Self, limits: ImageLimits) -> Result<(), ImageStoreError> {
        for loaded in other.by_node_url.values() {
            self.insert_entry(
                loaded.key.clone(),
                Arc::clone(&loaded.image),
                loaded.animation.clone(),
                limits,
            )?;
        }
        Ok(())
    }

    /// Store a decoded animation while keeping its first frame as the ordinary
    /// still bitmap in [`LoadedImage::image`].
    ///
    /// The document clock is deliberately not wired in this layer. Before each
    /// paint, the coordinator should choose
    /// [`AnimatedImage::frame_at`](animation::AnimatedImage::frame_at) at the
    /// page's monotonic/document time and publish that frame through the
    /// ordinary image-resource seam so display-list diffs and paint observe it.
    ///
    /// # Errors
    ///
    /// Returns a store-limit error without changing the existing resource.
    pub fn insert_animated(
        &mut self,
        key: ImageResourceKey,
        animation: AnimatedImage,
        limits: ImageLimits,
    ) -> Result<ImageResourceId, ImageStoreError> {
        let image = animation.first_frame_image_arc();
        self.insert_entry(key, image, Some(Arc::new(animation)), limits)
    }

    fn insert_entry(
        &mut self,
        key: ImageResourceKey,
        image: Arc<DecodedImage>,
        animation: Option<Arc<AnimatedImage>>,
        limits: ImageLimits,
    ) -> Result<ImageResourceId, ImageStoreError> {
        let map_key = (key.owner, key.requested_url.to_string());
        let retained_bytes = animation.as_ref().map_or_else(
            || image.decoded_bytes(),
            |animation| animation.decoded_bytes(),
        );
        let replaced_bytes = self
            .by_node_url
            .get(&map_key)
            .map_or(0, LoadedImage::decoded_bytes);
        let new_total = self
            .decoded_bytes
            .saturating_sub(replaced_bytes)
            .checked_add(retained_bytes)
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
        if let Some(previous) = self.by_node_url.remove(&map_key) {
            self.by_id.remove(&previous.id);
        }
        self.by_id.insert(id, Arc::clone(&image));
        self.by_node_url.insert(
            map_key,
            LoadedImage {
                id,
                key,
                image,
                content_revision: None,
                animation,
            },
        );
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
        self.decoded_bytes = self.decoded_bytes.saturating_sub(loaded.decoded_bytes());
        Some(loaded)
    }

    /// Install or replace the frame a `<video>` element currently presents.
    ///
    /// The entry is keyed by the element node, so layout sizes the video by
    /// the frame's intrinsic dimensions and paint draws it through the same
    /// `Image` display command as ordinary decoded images; a new resource id
    /// per replacement keeps display-list diffs honest about content changes.
    /// Any earlier frame for the node is dropped first, whatever URL it was
    /// published under.
    ///
    /// # Errors
    ///
    /// Returns a store-limit error without changing the existing frame.
    pub fn set_video_frame(
        &mut self,
        node: NodeId,
        media_url: &Url,
        frame: DecodedImage,
        limits: ImageLimits,
    ) -> Result<ImageResourceId, ImageStoreError> {
        self.remove_video_frame(node);
        let key = ImageResourceKey {
            owner: node,
            requested_url: media_url.clone(),
            source_snapshot: String::new(),
            source: ImageSource::VideoFrame,
            selection_context: ImageSelectionContext::default(),
        };
        self.insert(key, frame, limits)
    }

    /// Publish the bitmap of the `<canvas>` element at `node`, replacing the
    /// one it had.
    ///
    /// # Errors
    ///
    /// Returns a store-limit error without changing the existing bitmap.
    pub fn set_canvas_bitmap(
        &mut self,
        node: NodeId,
        bitmap: DecodedImage,
        limits: ImageLimits,
    ) -> Result<ImageResourceId, ImageStoreError> {
        self.remove_canvas_bitmap(node);
        let key = ImageResourceKey {
            owner: node,
            requested_url: canvas_bitmap_url(),
            source_snapshot: String::new(),
            source: ImageSource::Canvas,
            selection_context: ImageSelectionContext::default(),
        };
        self.insert(key, bitmap, limits)
    }

    /// Publish the rendered viewport of the nested document displayed by the
    /// `<iframe>` element at `node`.
    ///
    /// The bitmap is composited into the iframe's content box by the ordinary
    /// image display command, so the child document stays a separate surface:
    /// its nodes never join the parent display list. Replacing the bitmap
    /// allocates a new resource id, exactly as a canvas or video frame does, so
    /// display-list diffs observe child-only content changes.
    ///
    /// # Errors
    ///
    /// Returns a store-limit error without changing the existing bitmap.
    pub fn set_frame_bitmap(
        &mut self,
        node: NodeId,
        content_revision: DomRevision,
        bitmap: DecodedImage,
        limits: ImageLimits,
    ) -> Result<ImageResourceId, ImageStoreError> {
        self.remove_frame_bitmap(node);
        let key = ImageResourceKey {
            owner: node,
            requested_url: frame_document_url(),
            source_snapshot: String::new(),
            source: ImageSource::FrameDocument,
            selection_context: ImageSelectionContext::default(),
        };
        let map_key = (node, key.requested_url.to_string());
        let id = self.insert(key, bitmap, limits)?;
        if let Some(loaded) = self.by_node_url.get_mut(&map_key) {
            loaded.content_revision = Some(content_revision);
        }
        Ok(id)
    }

    /// Drop the rendered viewport of the nested document displayed by the
    /// `<iframe>` element at `node`.
    pub fn remove_frame_bitmap(&mut self, node: NodeId) -> Option<LoadedImage> {
        let map_key = self
            .by_node_url
            .iter()
            .find(|((owner, _), loaded)| {
                *owner == node && loaded.key.source == ImageSource::FrameDocument
            })
            .map(|((owner, url), _)| (*owner, url.clone()))?;
        let loaded = self.by_node_url.remove(&map_key)?;
        self.by_id.remove(&loaded.id);
        self.decoded_bytes = self.decoded_bytes.saturating_sub(loaded.decoded_bytes());
        Some(loaded)
    }

    /// Drop the bitmap of the `<canvas>` element at `node`.
    pub fn remove_canvas_bitmap(&mut self, node: NodeId) -> Option<LoadedImage> {
        let map_key = self
            .by_node_url
            .iter()
            .find(|((owner, _), loaded)| *owner == node && loaded.key.source == ImageSource::Canvas)
            .map(|((owner, url), _)| (*owner, url.clone()))?;
        let loaded = self.by_node_url.remove(&map_key)?;
        self.by_id.remove(&loaded.id);
        self.decoded_bytes = self.decoded_bytes.saturating_sub(loaded.decoded_bytes());
        Some(loaded)
    }

    /// Drop the presented frame of the `<video>` element at `node` (element
    /// removed, media unloaded, or element collected).
    pub fn remove_video_frame(&mut self, node: NodeId) -> Option<LoadedImage> {
        let map_key = self
            .by_node_url
            .iter()
            .find(|((owner, _), loaded)| {
                *owner == node && loaded.key.source == ImageSource::VideoFrame
            })
            .map(|((owner, url), _)| (*owner, url.clone()))?;
        let loaded = self.by_node_url.remove(&map_key)?;
        self.by_id.remove(&loaded.id);
        self.decoded_bytes = self.decoded_bytes.saturating_sub(loaded.decoded_bytes());
        Some(loaded)
    }

    /// Nodes with a presented video frame, for lifecycle pruning.
    #[must_use]
    pub fn video_frame_nodes(&self) -> Vec<NodeId> {
        self.by_node_url
            .iter()
            .filter(|(_, loaded)| loaded.key.source == ImageSource::VideoFrame)
            .map(|((owner, _), _)| *owner)
            .collect()
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

/// Adds script-created `img` elements that are not in the document. The
/// browser starts their load when `src` is set, so the preload pattern
/// `new Image().src = ...` must reach the fetch plan even though the element
/// is never inserted.
fn collect_detached_images(
    dom: &Dom,
    elements: &mut Vec<NodeId>,
    limit: usize,
) -> Option<ImageDiscoveryDiagnostic> {
    let attached: std::collections::BTreeSet<NodeId> = elements.iter().copied().collect();
    for (node, candidate) in dom.nodes() {
        if elements.len() >= limit {
            return Some(ImageDiscoveryDiagnostic {
                node: Some(node),
                code: ImageDiscoveryDiagnosticCode::NodeLimit,
                message: format!("image discovery node limit ({limit}) exceeded"),
            });
        }
        if attached.contains(&node) {
            continue;
        }
        if let NodeKind::Element(element) = candidate.kind()
            && element.local_name == "img"
        {
            elements.push(node);
        }
    }
    None
}

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
        // The shadow-including walk: a host's shadow root is visited before its
        // light children, so it is pushed last and popped first. An image in a
        // shadow tree loads whether or not a slot renders it.
        if let Some(root) = dom.shadow_root(node) {
            pending.push(root);
        }
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
        resolution: Some(context.device_pixel_ratio_milli as f32 / 1_000.0),
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
        "image/png"
            | "image/jpeg"
            | "image/pjpeg"
            | "image/gif"
            | "image/webp"
            | "image/svg+xml"
            | "image/svg"
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
    // A comma separates candidates, but a `data:` URL contains one of its
    // own. Splitting the whole attribute on commas truncates every data URL
    // at `base64,`; Apple uses such placeholders in `<source srcset>` and
    // every one of them failed to load. Walk the string instead: a data URL
    // keeps consuming characters (including commas) until whitespace, which
    // is where its descriptor starts.
    let bytes = srcset.as_bytes();
    let mut candidates = Vec::new();
    let mut index = 0_usize;
    while index < bytes.len() {
        while index < bytes.len() && (bytes[index] == b',' || bytes[index].is_ascii_whitespace()) {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        let url_start = index;
        let data_url = srcset[url_start..].starts_with("data:");
        while index < bytes.len() {
            let byte = bytes[index];
            if byte.is_ascii_whitespace() || (byte == b',' && !data_url) {
                break;
            }
            index += 1;
        }
        let url = &srcset[url_start..index];
        let descriptor_start = index;
        while index < bytes.len() && bytes[index] != b',' {
            index += 1;
        }
        let descriptor = srcset[descriptor_start..index].trim();
        index += usize::from(index < bytes.len());
        if url.is_empty() {
            continue;
        }
        let mut parts = descriptor.split_ascii_whitespace();
        let descriptor = match parts.next() {
            None => SrcsetDescriptor::Density(1.0),
            Some(value) if value.ends_with('x') => {
                let Some(density) = value
                    .strip_suffix('x')
                    .and_then(|value| value.parse::<f32>().ok())
                    .filter(|value| *value > 0.0)
                else {
                    continue;
                };
                SrcsetDescriptor::Density(density)
            }
            Some(value) if value.ends_with('w') => {
                let Some(width) = value
                    .strip_suffix('w')
                    .and_then(|value| value.parse::<u32>().ok())
                    .filter(|value| *value > 0)
                else {
                    continue;
                };
                SrcsetDescriptor::Width(width)
            }
            Some(_) => continue,
        };
        if parts.next().is_some() {
            continue;
        }
        candidates.push(SrcsetCandidate { url, descriptor });
    }
    candidates
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

    const AVIF_PROBE: &str = "AAAAIGZ0eXBhdmlmAAAAAGF2aWZtaWYxbWlhZk1BMUEAAADrbWV0YQAAAAAAAAAhaGRscgAAAAAAAAAAcGljdAAAAAAAAAAAAAAAAAAAAAAOcGl0bQAAAAAAAQAAAB5pbG9jAAAAAEQAAAEAAQAAAAEAAAETAAAAIgAAAChpaW5mAAAAAAABAAAAGmluZmUCAAAAAAEAAGF2MDFDb2xvcgAAAABqaXBycAAAAEtpcGNvAAAAFGlzcGUAAAAAAAAAEAAAABAAAAAQcGl4aQAAAAADCAgIAAAADGF2MUOBIAAAAAAAE2NvbHJuY2x4AAEADQAGgAAAABdpcG1hAAAAAAAAAAEAAQQBAoMEAAAAKm1kYXQSAAoIOAz/2EBDQaQyFBAAAAAUuboKM00UXyTHSo5Ou3Yw";

    use image_codec::{DynamicImage, ImageFormat as CodecImageFormat, Rgba, RgbaImage};
    use url::Url;

    use crate::html::parse_document;

    use super::{
        DecodedImage, ImageDecodeError, ImageFormat, ImageLimits, ImageResources,
        ImageSelectionContext, SrcsetDescriptor, background_url, decode_image, discover_images,
        discover_images_with_context, image_key_is_current, parse_srcset, sniff_image_format,
    };
    use crate::dom::{ShadowRootMode, SlotAssignmentMode};
    use crate::paint::Color;

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
    fn background_image_layers_and_image_set_discover_the_first_url() {
        for (value, expected) in [
            ("url(a.png)", Some("a.png")),
            ("url('a.png')", Some("a.png")),
            ("url(a.png), url(b.png)", Some("a.png")),
            ("none, url(b.png)", Some("b.png")),
            ("linear-gradient(red, blue), url(c.png)", Some("c.png")),
            ("image-set(url(a.png) 1x, url(b.png) 2x)", Some("a.png")),
            ("image-set('a.png' 1x, 'b.png' 2x)", Some("a.png")),
            ("-webkit-image-set(url(a.png) 1x)", Some("a.png")),
            ("none", None),
        ] {
            assert_eq!(background_url(value), expected, "{value}");
        }
    }

    #[test]
    fn discovery_accepts_a_multi_layer_background_image() {
        let document = crate::document::Document::parse(
            "<!doctype html><style>\
             html, body { display:block; margin:0 }\
             #box { display:block; width:20px; height:20px;\
                    background-image:url(a.png),url(b.png);\
                    background-repeat:no-repeat }\
             </style><div id=box></div>",
        );
        let initial = document.render_reference(crate::document::DocumentRenderOptions::default());
        let discovery = super::discover_images_with_styles(
            document.dom(),
            &initial.styles,
            &Url::parse("https://example.test/page.html").unwrap(),
            ImageLimits::default(),
        );
        assert_eq!(discovery.resources.len(), 1);
        assert_eq!(
            discovery.resources[0].key.requested_url.as_str(),
            "https://example.test/a.png"
        );
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
    fn keeps_commas_inside_a_data_url_srcset_candidate() {
        let candidates = parse_srcset(
            "data:image/gif;base64,R0lGODlhAQABAHAAACH5BAEAAAAALAAAAAABAAEAAAICRAEAOw== 1x, \
             hero@2x.png 2x",
        );
        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates[0].url,
            "data:image/gif;base64,R0lGODlhAQABAHAAACH5BAEAAAAALAAAAAABAAEAAAICRAEAOw=="
        );
        assert_eq!(candidates[1].url, "hero@2x.png");
        assert!(matches!(
            candidates[1].descriptor,
            SrcsetDescriptor::Density(density) if (density - 2.0).abs() < 0.001
        ));
    }

    #[test]
    fn discovers_images_in_a_shadow_tree_before_the_light_children() {
        let mut parsed = parse_document("<div id=host><img src='light.png'></div>");
        let host = by_id(&parsed.dom, "host");
        let root = parsed
            .dom
            .attach_shadow(host, ShadowRootMode::Open, false, SlotAssignmentMode::Named)
            .expect("a div can host a shadow root");
        let shadow_image = parsed.dom.create_element("img");
        parsed
            .dom
            .set_attribute(shadow_image, "src", "shadow.png")
            .expect("set the shadow image's src");
        parsed
            .dom
            .append_child(root, shadow_image)
            .expect("append the image to the shadow root");
        let document_url = Url::parse("https://example.test/page/index.html").unwrap();
        let discovery = discover_images(&parsed.dom, &document_url, ImageLimits::default());

        let urls: Vec<&str> = discovery
            .resources
            .iter()
            .map(|image| image.key.requested_url.as_str())
            .collect();
        assert_eq!(
            urls,
            [
                "https://example.test/page/shadow.png",
                "https://example.test/page/light.png",
            ]
        );
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
    fn a_detached_script_created_image_is_discovered() {
        // `new Image(); image.src = ...` is the standard preload pattern. The
        // element is never inserted, but the browser still starts the load and
        // fires load/error on it.
        let mut parsed = parse_document("<p>page</p>");
        let image = parsed.dom.create_element("img");
        parsed
            .dom
            .set_attribute(image, "src", "/preload.png")
            .unwrap();
        let document_url = Url::parse("https://example.test/page/").unwrap();
        let discovery = discover_images(&parsed.dom, &document_url, ImageLimits::default());
        assert_eq!(discovery.resources.len(), 1);
        assert_eq!(discovery.resources[0].key.owner, image);
        assert_eq!(
            discovery.resources[0].key.requested_url.as_str(),
            "https://example.test/preload.png"
        );
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
    fn presented_video_frame_outranks_the_poster_and_releases_cleanly() {
        let parsed = parse_document("<video id=player poster='poster.png'></video>");
        let document_url = Url::parse("https://example.test/watch").unwrap();
        let discovery = discover_images(&parsed.dom, &document_url, ImageLimits::default());
        let poster_key = discovery.resources[0].key.clone();
        let node = poster_key.owner;

        let mut images = ImageResources::default();
        let poster = DecodedImage::from_pixels(4, 4, vec![Color::rgb(1, 2, 3); 16]).unwrap();
        let poster_id = images
            .insert(poster_key, poster, ImageLimits::default())
            .unwrap();
        assert_eq!(
            images.get_for_node(node).map(|loaded| loaded.id),
            Some(poster_id),
            "before playback the poster shows"
        );

        let media_url = Url::parse("https://example.test/movie.mp4").unwrap();
        let mut frame_bytes = Vec::with_capacity(2 * 4 * 4);
        for _ in 0..2 * 4 {
            frame_bytes.extend_from_slice(&[9, 8, 7, 255]);
        }
        let frame = DecodedImage::from_rgba8(2, 4, &frame_bytes).unwrap();
        let frame_id = images
            .set_video_frame(node, &media_url, frame, ImageLimits::default())
            .unwrap();
        assert_ne!(frame_id, poster_id);
        assert_eq!(
            images.get_for_node(node).map(|loaded| loaded.id),
            Some(frame_id),
            "a presented frame hides the poster"
        );
        assert_eq!(images.get_for_node(node).unwrap().image.width(), 2);
        assert_eq!(images.video_frame_nodes(), vec![node]);

        // Replacing under a different URL drops the earlier frame; removing
        // releases the node back to the poster.
        let other_url = Url::parse("https://example.test/other.mp4").unwrap();
        let frame = DecodedImage::from_rgba8(2, 4, &[255; 2 * 4 * 4]).unwrap();
        let replaced = images
            .set_video_frame(node, &other_url, frame, ImageLimits::default())
            .unwrap();
        assert_eq!(images.video_frame_nodes(), vec![node]);
        assert_eq!(
            images.get_for_node(node).map(|loaded| loaded.id),
            Some(replaced)
        );
        assert_eq!(
            images.decoded_bytes(),
            poster_and_frame_bytes(4 * 4, 2 * 4),
            "byte accounting covers poster plus one frame"
        );

        images.remove_video_frame(node);
        assert!(images.video_frame_nodes().is_empty());
        assert_eq!(
            images.get_for_node(node).map(|loaded| loaded.id),
            Some(poster_id),
            "the poster returns after the frame is gone"
        );
    }

    fn poster_and_frame_bytes(poster_pixels: usize, frame_pixels: usize) -> usize {
        poster_pixels.saturating_mul(4) + frame_pixels.saturating_mul(4)
    }

    #[test]
    fn from_rgba8_validates_the_byte_length() {
        assert!(DecodedImage::from_rgba8(2, 2, &[0; 16]).is_ok());
        assert_eq!(
            DecodedImage::from_rgba8(2, 2, &[0; 15]).unwrap_err(),
            super::ImageDecodeError::PixelBufferLength {
                actual: 15,
                expected: 16
            }
        );
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
    fn sniffs_and_decodes_avif() {
        // A 16x16 red AVIF encoded by avifenc. Decoding it is what makes
        // `images_failed` stop counting a browser as AVIF-incapable.
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(AVIF_PROBE)
            .expect("AVIF base64");
        assert_eq!(sniff_image_format(&bytes), Some(ImageFormat::Avif));
        assert_eq!(
            ImageFormat::from_media_type("image/avif"),
            Some(ImageFormat::Avif)
        );
        let decoded =
            decode_image(&bytes, ImageFormat::Avif, ImageLimits::default()).expect("AVIF decodes");
        assert_eq!(decoded.intrinsic_size(), (16, 16));
        let pixel = decoded.pixel(0, 0).expect("decoded pixel");
        assert!(
            pixel.red >= 250 && pixel.green < 5 && pixel.blue < 5,
            "{pixel:?}"
        );
        assert_eq!(pixel.alpha, 255);
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
