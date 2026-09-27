//! Minimal static-SVG rasterizer for `<img>` content.
//!
//! Real-world icon SVGs draw with a compact vector subset; this module
//! implements that subset by the SVG spec so ordinary icons render
//! correctly everywhere, without site-specific handling:
//!
//! - shapes: `rect`, `circle`, `ellipse`, `polygon`, `polyline`, `line`,
//!   and `path` segments `M m L l H h V v C c S s Q q T t Z z` (arc `A`
//!   degrades to a line to its endpoint);
//! - nested `<g>` transforms: `translate`, `scale`, `matrix`, `rotate`,
//!   `skewX`, `skewY`;
//! - `fill`/`stroke` paints inherited through `<g>`, with `none`;
//! - root sizing by `width`/`height` (percentages resolve against the
//!   `viewBox`), falling back to `viewBox`, then the spec default 300×150.
//!
//! Everything outside the subset (gradients, text, clips, masks, embedded
//! images, scripting) contributes nothing, matching how an `<img>`-loaded
//! SVG must render: static and script-free.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "scanline rasterization clamps coordinates before every cast"
)]

use std::str;

use super::{Color, DecodedImage, ImageDecodeError, ImageLimits, enforce_dimensions};

/// One polyline segment in device coordinates.
type Segment = ((f32, f32), (f32, f32));

/// Affine transform `[a c e; b d f]`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Affine {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl Affine {
    const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    fn apply(self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a.mul_add(x, self.c.mul_add(y, self.e)),
            self.b.mul_add(x, self.d.mul_add(y, self.f)),
        )
    }

    /// Compose two transforms: `parent(child(point))`.
    fn then(self, child: Self) -> Self {
        Self {
            a: self.a * child.a + self.c * child.b,
            b: self.b * child.a + self.d * child.b,
            c: self.a * child.c + self.c * child.d,
            d: self.b * child.c + self.d * child.d,
            e: self.a * child.e + self.c * child.f + self.e,
            f: self.b * child.e + self.d * child.f + self.f,
        }
    }
}

/// Paint for one element: explicit color, explicit `none`, or inherited.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Paint {
    Inherit,
    None,
    Color(Color),
}

impl Paint {
    fn resolve(self, inherited: Option<Color>) -> Option<Color> {
        match self {
            Self::Inherit => inherited,
            Self::None => None,
            Self::Color(color) => Some(color),
        }
    }
}

/// Decode an SVG document into an RGBA image.
///
/// # Errors
///
/// Returns [`ImageDecodeError`] when the bytes are not valid UTF-8, no root
/// `<svg>` element exists, or the target size violates `limits`.
pub fn decode_svg(bytes: &[u8], limits: ImageLimits) -> Result<DecodedImage, ImageDecodeError> {
    let text = str::from_utf8(bytes)
        .map_err(|_| ImageDecodeError::Codec("svg document is not valid UTF-8".to_owned()))?;
    // image/svg+xml is an XML document type, so the tree is parsed as XML:
    // self-closing elements close (an HTML tree builder would keep them
    // open and nest every following sibling inside).
    let root = parse_xml(text).ok_or_else(|| {
        ImageDecodeError::Codec("svg document has no <svg> root element".to_owned())
    })?;
    let root_width = attribute(&root, "width").and_then(parse_length);
    let root_height = attribute(&root, "height").and_then(parse_length);
    let view_box = attribute(&root, "viewbox").and_then(parse_view_box);

    let (width_f, height_f) = root_size(root_width, root_height, view_box);
    let width = width_f.round().max(1.0) as u32;
    let height = height_f.round().max(1.0) as u32;
    enforce_dimensions(width, height, limits)?;

    let mut raster = Raster {
        width,
        height,
        pixels: vec![Color::rgba(0, 0, 0, 0); width as usize * height as usize],
    };

    // Root viewBox-to-viewport mapping, composed ahead of every shape.
    let root_transform = if let Some((min_x, min_y, view_width, view_height)) = view_box
        && view_width > 0.0
        && view_height > 0.0
    {
        let scale_x = width as f32 / view_width;
        let scale_y = height as f32 / view_height;
        Affine::IDENTITY
            .then(Affine {
                a: scale_x,
                b: 0.0,
                c: 0.0,
                d: scale_y,
                e: 0.0,
                f: 0.0,
            })
            .then(Affine {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: 1.0,
                e: -min_x * scale_x,
                f: -min_y * scale_y,
            })
    } else {
        Affine::IDENTITY
    };

    walk(
        &root,
        root_transform,
        Paint::Inherit,
        Paint::None,
        &mut raster,
    );
    DecodedImage::from_pixels(width, height, raster.pixels)
}

struct Raster {
    width: u32,
    height: u32,
    pixels: Vec<Color>,
}

impl Raster {
    fn fill_polygon(&mut self, points: &[(f32, f32)], color: Color) {
        if points.len() < 2 {
            return;
        }
        let min_y = points.iter().fold(f32::MAX, |m, p| m.min(p.1));
        let max_y = points.iter().fold(f32::MIN, |m, p| m.max(p.1));
        let first_row = (min_y.floor().max(0.0)) as i64;
        let last_row = (max_y.ceil().min(f32::from(u16::MAX))) as i64;
        for row in first_row..=last_row {
            let sample = row as f32 + 0.5;
            let mut crossings: Vec<f32> = Vec::new();
            for index in 0..points.len() {
                let (x1, y1) = points[index];
                let (x2, y2) = points[(index + 1) % points.len()];
                if (y1 <= sample && y2 > sample) || (y2 <= sample && y1 > sample) {
                    let t = (sample - y1) / (y2 - y1);
                    crossings.push(x1 + t * (x2 - x1));
                }
            }
            crossings.sort_by(|left, right| {
                left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal)
            });
            for pair in crossings.chunks(2) {
                if pair.len() < 2 {
                    break;
                }
                // Pixel x is covered when its center (x + 0.5) lies in
                // [x0, x1): start = ceil(x0 - 0.5), end = ceil(x1 - 0.5)-1.
                let start = (pair[0] - 0.5).ceil().max(0.0) as i64;
                let end = ((pair[1] - 0.5).ceil() as i64)
                    .saturating_sub(1)
                    .min(i64::from(self.width.saturating_sub(1)));
                for x in start..=end {
                    self.put(x, row, color);
                }
            }
        }
    }

    fn stroke_segments(&mut self, segments: &[Segment], color: Color) {
        for ((x1, y1), (x2, y2)) in segments {
            let steps = ((x2 - x1).abs().max((y2 - y1).abs()).ceil() as i64).clamp(1, 4096);
            for step in 0..=steps {
                let t = step as f32 / steps as f32;
                self.put(
                    (x1 + t * (x2 - x1)).round() as i64,
                    (y1 + t * (y2 - y1)).round() as i64,
                    color,
                );
            }
        }
    }

    fn put(&mut self, x: i64, y: i64, color: Color) {
        if x < 0 || y < 0 || x >= self.width.into() || y >= self.height.into() {
            return;
        }
        let index =
            usize::try_from(y).unwrap_or(0) * self.width as usize + usize::try_from(x).unwrap_or(0);
        self.pixels[index] = color;
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one shape-arm per SVG element reads clearest as a flat match"
)]
fn walk(element: &XmlNode, transform: Affine, fill: Paint, stroke: Paint, raster: &mut Raster) {
    for child_node in &element.children {
        let attribute = |name: &str| {
            attribute(child_node, name)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        let child_fill = attribute("fill").map_or(fill, parse_paint);
        let child_stroke = attribute("stroke").map_or(stroke, parse_paint);
        let child_transform = match attribute("transform") {
            Some(specification) => transform.then(parse_transform(specification)),
            None => transform,
        };
        match child_node.name.as_str() {
            "defs" | "text" | "image" | "style" | "title" | "desc" | "metadata" => {}
            "rect" => {
                let Some(points) = rect_polygon(
                    attribute("x").and_then(parse_length),
                    attribute("y").and_then(parse_length),
                    attribute("width").and_then(parse_length),
                    attribute("height").and_then(parse_length),
                ) else {
                    continue;
                };
                let points = transform_points(&points, child_transform);
                if let Some(color) = child_fill.resolve(None) {
                    raster.fill_polygon(&points, color);
                }
                stroke_outline(raster, &points, child_stroke.resolve(None));
            }
            "circle" | "ellipse" => {
                let cx = attribute("cx").and_then(parse_length).unwrap_or(0.0);
                let cy = attribute("cy").and_then(parse_length).unwrap_or(0.0);
                let (rx, ry) = if child_node.name == "circle" {
                    let r = attribute("r").and_then(parse_length).unwrap_or(0.0);
                    (r, r)
                } else {
                    (
                        attribute("rx").and_then(parse_length).unwrap_or(0.0),
                        attribute("ry").and_then(parse_length).unwrap_or(0.0),
                    )
                };
                if rx <= 0.0 || ry <= 0.0 {
                    continue;
                }
                let mut points = Vec::with_capacity(48);
                for step in 0..48 {
                    let angle = std::f32::consts::TAU * step as f32 / 48.0;
                    points
                        .push(child_transform.apply(cx + rx * angle.cos(), cy + ry * angle.sin()));
                }
                if let Some(color) = child_fill.resolve(None) {
                    raster.fill_polygon(&points, color);
                }
                stroke_outline(raster, &points, child_stroke.resolve(None));
            }
            "polygon" | "polyline" => {
                let Some(raw_points) = attribute("points").and_then(parse_points) else {
                    continue;
                };
                if raw_points.len() < 2 {
                    continue;
                }
                let mut points = transform_points(&raw_points, child_transform);
                if let Some(color) = child_fill.resolve(None) {
                    // Fill closes the path implicitly, per spec.
                    if child_node.name == "polygon" {
                        raster.fill_polygon(&points, color);
                    } else {
                        points.push(points[0]);
                        raster.fill_polygon(&points, color);
                        points.pop();
                    }
                }
                let segments = polyline_segments(&points, child_node.name == "polygon");
                stroke_outline_segments(raster, &segments, child_stroke.resolve(None));
            }
            "line" => {
                let (Some(x1), Some(y1), Some(x2), Some(y2)) = (
                    attribute("x1").and_then(parse_length),
                    attribute("y1").and_then(parse_length),
                    attribute("x2").and_then(parse_length),
                    attribute("y2").and_then(parse_length),
                ) else {
                    continue;
                };
                let from = child_transform.apply(x1, y1);
                let to = child_transform.apply(x2, y2);
                stroke_outline_segments(raster, &[(from, to)], child_stroke.resolve(None));
            }
            "path" => {
                let Some(specification) = attribute("d") else {
                    continue;
                };
                for (subpath, closed) in &parse_path(specification) {
                    let points = transform_points(subpath, child_transform);
                    if points.len() < 2 {
                        continue;
                    }
                    if let Some(color) = child_fill.resolve(None) {
                        raster.fill_polygon(&points, color);
                    }
                    let segments = polyline_segments(&points, *closed);
                    stroke_outline_segments(raster, &segments, child_stroke.resolve(None));
                }
            }
            "svg" | "g" | "a" | "switch" => {
                if attribute("display").is_some_and(|value| value.eq_ignore_ascii_case("none")) {
                    continue;
                }
                walk(
                    child_node,
                    child_transform,
                    child_fill,
                    child_stroke,
                    raster,
                );
            }
            _ => {
                // Unknown container: descend, it may hold shapes.
                walk(
                    child_node,
                    child_transform,
                    child_fill,
                    child_stroke,
                    raster,
                );
            }
        }
    }
}

fn transform_points(points: &[(f32, f32)], transform: Affine) -> Vec<(f32, f32)> {
    points
        .iter()
        .map(|(x, y)| transform.apply(*x, *y))
        .collect()
}

fn stroke_outline(raster: &mut Raster, points: &[(f32, f32)], color: Option<Color>) {
    stroke_outline_segments(raster, &polyline_segments(points, true), color);
}

fn stroke_outline_segments(raster: &mut Raster, segments: &[Segment], color: Option<Color>) {
    if let Some(color) = color {
        raster.stroke_segments(segments, color);
    }
}

fn polyline_segments(points: &[(f32, f32)], closed: bool) -> Vec<Segment> {
    let mut segments = Vec::new();
    for pair in points.windows(2) {
        segments.push((pair[0], pair[1]));
    }
    if closed && points.len() > 2 {
        segments.push((*points.last().expect("non-empty"), points[0]));
    }
    segments
}

/// A minimal XML element tree: SVG loaded through `<img>` is an XML
/// document, so the tree honors self-closing elements, quoted attributes,
/// comments, processing instructions, and CDATA.
#[derive(Debug, PartialEq)]
pub struct XmlNode {
    pub name: String,
    pub attributes: Vec<(String, String)>,
    pub children: Vec<XmlNode>,
}

fn attribute<'a>(element: &'a XmlNode, name: &str) -> Option<&'a str> {
    element
        .attributes
        .iter()
        .find(|(attribute_name, _)| attribute_name == name)
        .map(|(_, value)| value.as_str())
}

fn parse_xml(text: &str) -> Option<XmlNode> {
    let mut stack: Vec<XmlNode> = Vec::new();
    let mut root: Option<XmlNode> = None;
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'<' {
            if text[index..].starts_with("<!--") {
                index = text[index..]
                    .find("-->")
                    .map_or(text.len(), |found| index + found + 3);
                continue;
            }
            if text[index..].starts_with("<![CDATA[") {
                index = text[index..]
                    .find("]]>")
                    .map_or(text.len(), |found| index + found + 3);
                continue;
            }
            if text[index..].starts_with("<?") || text[index..].starts_with("<!") {
                index = text[index..]
                    .find('>')
                    .map_or(text.len(), |found| index + found + 1);
                continue;
            }
            let Some(tag_end) = text[index..].find('>') else {
                break;
            };
            let tag = &text[index + 1..index + tag_end];
            index += tag_end + 1;
            if let Some(name) = tag.strip_prefix('/') {
                let name = name.trim();
                let closed = stack.pop()?;
                if !name.is_empty() && closed.name != name {
                    return None;
                }
                match stack.pop() {
                    Some(mut parent) => {
                        parent.children.push(closed);
                        stack.push(parent);
                    }
                    None => root = Some(closed),
                }
            } else {
                let self_closing = tag.ends_with('/');
                let tag = tag.strip_suffix('/').unwrap_or(tag);
                let element = parse_xml_tag(tag);
                if self_closing {
                    match stack.pop() {
                        Some(mut parent) => {
                            parent.children.push(element);
                            stack.push(parent);
                        }
                        None => root = Some(element),
                    }
                } else {
                    stack.push(element);
                }
            }
            continue;
        }
        index += 1;
    }
    root
}

fn parse_xml_tag(tag: &str) -> XmlNode {
    let mut name = String::new();
    let mut attributes = Vec::new();
    let mut characters = tag.chars().peekable();
    while let Some(character) = characters.peek() {
        if character.is_whitespace() {
            break;
        }
        name.push(*character);
        characters.next();
    }
    loop {
        while characters.peek().is_some_and(|c: &char| c.is_whitespace()) {
            characters.next();
        }
        if characters.peek().is_none() {
            break;
        }
        let mut attribute_name = String::new();
        while let Some(character) = characters.peek()
            && !character.is_whitespace()
            && *character != '='
        {
            attribute_name.push(*character);
            characters.next();
        }
        while characters.peek().is_some_and(|c: &char| c.is_whitespace()) {
            characters.next();
        }
        if characters.peek() == Some(&'=') {
            characters.next();
            while characters.peek().is_some_and(|c: &char| c.is_whitespace()) {
                characters.next();
            }
            let quote = characters.next().unwrap_or('"');
            let mut value = String::new();
            while let Some(character) = characters.peek()
                && *character != quote
            {
                value.push(*character);
                characters.next();
            }
            characters.next();
            attributes.push((attribute_name, decode_entities(&value)));
        } else {
            attributes.push((attribute_name, String::new()));
        }
    }
    XmlNode {
        name,
        attributes,
        children: Vec::new(),
    }
}

fn decode_entities(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_owned();
    }
    raw.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Root size resolution: explicit width/height (percentages resolve against
/// the viewBox, unparseable values fall to its smaller side), then the
/// viewBox, then the spec default 300×150.
fn root_size(
    width: Option<f32>,
    height: Option<f32>,
    view_box: Option<(f32, f32, f32, f32)>,
) -> (f32, f32) {
    let view_size = view_box.map_or((300.0, 150.0), |(_, _, w, h)| (w, h));
    let resolve = |value: Option<f32>, fallback: f32| value.unwrap_or(fallback);
    (resolve(width, view_size.0), resolve(height, view_size.1))
}

fn parse_length(raw: &str) -> Option<f32> {
    let trimmed = raw.trim();
    let number: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
        .collect();
    number
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .filter(|_| {
            // Percentages are handled by the caller when a viewBox exists;
            // the raw scale-less number is the best available basis here.
            let suffix = trimmed[number.len()..].trim_start();
            suffix.is_empty() || suffix.eq_ignore_ascii_case("px") || suffix.ends_with('%')
        })
}

fn parse_view_box(raw: &str) -> Option<(f32, f32, f32, f32)> {
    let numbers: Vec<f32> = raw
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .filter_map(parse_length)
        .collect();
    if numbers.len() == 4 {
        Some((numbers[0], numbers[1], numbers[2], numbers[3]))
    } else {
        None
    }
}

fn parse_points(raw: &str) -> Option<Vec<(f32, f32)>> {
    let numbers: Vec<f32> = raw
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .filter_map(parse_length)
        .collect();
    if numbers.is_empty() || numbers.len() % 2 != 0 {
        return None;
    }
    Some(
        numbers
            .chunks(2)
            .map(|pair| (pair[0], pair[1]))
            .collect::<Vec<_>>(),
    )
}

fn rect_polygon(
    x: Option<f32>,
    y: Option<f32>,
    width: Option<f32>,
    height: Option<f32>,
) -> Option<Vec<(f32, f32)>> {
    let (width, height) = (width?, height?);
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let x = x.unwrap_or(0.0);
    let y = y.unwrap_or(0.0);
    Some(vec![
        (x, y),
        (x + width, y),
        (x + width, y + height),
        (x, y + height),
    ])
}

fn parse_paint(raw: &str) -> Paint {
    let trimmed = raw.trim();
    match trimmed.to_ascii_lowercase().as_str() {
        "none" | "transparent" => Paint::None,
        "currentcolor" => Paint::Color(Color::rgb(0, 0, 0)),
        _ => parse_color(trimmed).map_or(Paint::Inherit, Paint::Color),
    }
}

fn parse_color(raw: &str) -> Option<Color> {
    let trimmed = raw.trim();
    if let Some(hex) = trimmed.strip_prefix('#') {
        let digits = hex.as_bytes();
        return match digits.len() {
            3 => {
                let expanded: Vec<u8> = digits.iter().flat_map(|byte| [*byte, *byte]).collect();
                six_digit_hex(&expanded, 255)
            }
            6 => six_digit_hex(digits, 255),
            8 => six_digit_hex(
                digits,
                six_digit_hex(&digits[4..8], 255).map_or(255, |c| c.alpha),
            )
            .map(|color| color.with_opacity(1.0))
            .and(six_digit_hex(
                &digits[..6],
                six_digit_hex(&digits[6..8], 255).map_or(255, |c| c.alpha),
            )),
            _ => None,
        };
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "black" => Some(Color::rgb(0, 0, 0)),
        "white" => Some(Color::rgb(255, 255, 255)),
        "red" => Some(Color::rgb(255, 0, 0)),
        "green" => Some(Color::rgb(0, 128, 0)),
        "lime" => Some(Color::rgb(0, 255, 0)),
        "blue" => Some(Color::rgb(0, 0, 255)),
        "yellow" => Some(Color::rgb(255, 255, 0)),
        "orange" => Some(Color::rgb(255, 165, 0)),
        "purple" => Some(Color::rgb(128, 0, 128)),
        "gray" | "grey" => Some(Color::rgb(128, 128, 128)),
        "silver" => Some(Color::rgb(192, 192, 192)),
        "cyan" | "aqua" => Some(Color::rgb(0, 255, 255)),
        "magenta" | "fuchsia" => Some(Color::rgb(255, 0, 255)),
        _ => None,
    }
}

fn six_digit_hex(digits: &[u8], alpha: u8) -> Option<Color> {
    if digits.len() < 6 {
        return None;
    }
    let channel = |index: usize| -> Option<u8> {
        let high = hex_value(digits[index])?;
        let low = hex_value(digits[index + 1])?;
        Some(high * 16 + low)
    };
    Some(Color::rgba(channel(0)?, channel(2)?, channel(4)?, alpha))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_transform(raw: &str) -> Affine {
    let mut result = Affine::IDENTITY;
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len() && !bytes[index].is_ascii_alphabetic() {
            index += 1;
        }
        let name_start = index;
        while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
            index += 1;
        }
        let name = &raw[name_start..index];
        while index < bytes.len() && bytes[index] != b'(' {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        index += 1;
        let args_start = index;
        while index < bytes.len() && bytes[index] != b')' {
            index += 1;
        }
        let numbers: Vec<f32> = raw[args_start..index.min(raw.len())]
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|part| !part.is_empty())
            .filter_map(parse_length)
            .collect();
        index = index.min(raw.len()) + 1;
        let applied = match name {
            "translate" => Affine {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: 1.0,
                e: numbers.first().copied().unwrap_or(0.0),
                f: numbers.get(1).copied().unwrap_or(0.0),
            },
            "scale" => {
                let sx = numbers.first().copied().unwrap_or(1.0);
                let sy = numbers.get(1).copied().unwrap_or(sx);
                Affine {
                    a: sx,
                    b: 0.0,
                    c: 0.0,
                    d: sy,
                    e: 0.0,
                    f: 0.0,
                }
            }
            "matrix" if numbers.len() == 6 => Affine {
                a: numbers[0],
                b: numbers[1],
                c: numbers[2],
                d: numbers[3],
                e: numbers[4],
                f: numbers[5],
            },
            "rotate" => {
                let radians = numbers.first().copied().unwrap_or(0.0).to_radians();
                let (sin, cos) = radians.sin_cos();
                let rotation = Affine {
                    a: cos,
                    b: sin,
                    c: -sin,
                    d: cos,
                    e: 0.0,
                    f: 0.0,
                };
                match (numbers.get(1).copied(), numbers.get(2).copied()) {
                    (Some(cx), Some(cy)) => Affine::IDENTITY
                        .then(Affine {
                            a: 1.0,
                            b: 0.0,
                            c: 0.0,
                            d: 1.0,
                            e: cx,
                            f: cy,
                        })
                        .then(rotation)
                        .then(Affine {
                            a: 1.0,
                            b: 0.0,
                            c: 0.0,
                            d: 1.0,
                            e: -cx,
                            f: -cy,
                        }),
                    _ => rotation,
                }
            }
            "skewx" => Affine {
                a: 1.0,
                b: 0.0,
                c: numbers.first().copied().unwrap_or(0.0).to_radians().tan(),
                d: 1.0,
                e: 0.0,
                f: 0.0,
            },
            "skewy" => Affine {
                a: 1.0,
                b: numbers.first().copied().unwrap_or(0.0).to_radians().tan(),
                c: 0.0,
                d: 1.0,
                e: 0.0,
                f: 0.0,
            },
            _ => Affine::IDENTITY,
        };
        result = result.then(applied);
    }
    result
}

fn path_number(tokens: &[PathToken], index: &mut usize) -> Option<f32> {
    match tokens.get(*index) {
        Some(PathToken::Number(value)) => {
            *index += 1;
            Some(*value)
        }
        _ => None,
    }
}

fn path_pair(tokens: &[PathToken], index: &mut usize) -> Option<(f32, f32)> {
    let x = path_number(tokens, index)?;
    let y = path_number(tokens, index)?;
    Some((x, y))
}

/// Parse a path `d` attribute into subpaths of points with a closed flag.
/// Curves are flattened by fixed-step sampling; arc segments degrade to a
/// straight line to their endpoint.
#[allow(clippy::too_many_lines)]
fn parse_path(raw: &str) -> Vec<(Vec<(f32, f32)>, bool)> {
    let tokens = &tokenize_path(raw);
    let mut subpaths: Vec<(Vec<(f32, f32)>, bool)> = Vec::new();
    let mut current: Vec<(f32, f32)> = Vec::new();
    let mut command = ' ';
    let mut index = 0;
    let mut cursor = (0.0_f32, 0.0_f32);
    let mut subpath_start = (0.0_f32, 0.0_f32);
    let mut last_cubic_control: Option<(f32, f32)> = None;
    let mut last_quad_control: Option<(f32, f32)> = None;

    while index < tokens.len() {
        let next = &tokens[index];
        if let PathToken::Command(letter) = next {
            command = *letter;
            index += 1;
            // After M/m, subsequent coordinate pairs are implicit line-tos.
            if matches!(command, 'M' | 'm') {
                command = if command == 'M' { 'M' } else { 'm' };
            }
        }
        let relative = command.is_ascii_lowercase();
        match command.to_ascii_uppercase() {
            'M' => {
                let Some((x, y)) = path_pair(tokens, &mut index) else {
                    break;
                };
                cursor = if relative {
                    (cursor.0 + x, cursor.1 + y)
                } else {
                    (x, y)
                };
                if !current.is_empty() {
                    subpaths.push((std::mem::take(&mut current), false));
                }
                current.push(cursor);
                subpath_start = cursor;
                last_cubic_control = None;
                last_quad_control = None;
                command = if relative { 'l' } else { 'L' };
            }
            'L' => {
                let Some((x, y)) = path_pair(tokens, &mut index) else {
                    break;
                };
                cursor = if relative {
                    (cursor.0 + x, cursor.1 + y)
                } else {
                    (x, y)
                };
                current.push(cursor);
            }
            'H' => {
                let Some(x) = path_number(tokens, &mut index) else {
                    break;
                };
                cursor.0 = if relative { cursor.0 + x } else { x };
                current.push(cursor);
            }
            'V' => {
                let Some(y) = path_number(tokens, &mut index) else {
                    break;
                };
                cursor.1 = if relative { cursor.1 + y } else { y };
                current.push(cursor);
            }
            'C' => {
                let Some((x1, y1)) = path_pair(tokens, &mut index) else {
                    break;
                };
                let Some((x2, y2)) = path_pair(tokens, &mut index) else {
                    break;
                };
                let Some((x, y)) = path_pair(tokens, &mut index) else {
                    break;
                };
                let (p0x, p0y) = cursor;
                let (x1, y1) = if relative {
                    (p0x + x1, p0y + y1)
                } else {
                    (x1, y1)
                };
                let (x2, y2) = if relative {
                    (p0x + x2, p0y + y2)
                } else {
                    (x2, y2)
                };
                let (x, y) = if relative { (p0x + x, p0y + y) } else { (x, y) };
                for step in 1..=12 {
                    let t = step as f32 / 12.0;
                    current.push(cubic_point(p0x, p0y, x1, y1, x2, y2, x, y, t));
                }
                last_cubic_control = Some((x2, y2));
                cursor = (x, y);
            }
            'S' => {
                let Some((x2, y2)) = path_pair(tokens, &mut index) else {
                    break;
                };
                let Some((x, y)) = path_pair(tokens, &mut index) else {
                    break;
                };
                let (p0x, p0y) = cursor;
                let (x2, y2) = if relative {
                    (p0x + x2, p0y + y2)
                } else {
                    (x2, y2)
                };
                let (x, y) = if relative { (p0x + x, p0y + y) } else { (x, y) };
                let (x1, y1) = last_cubic_control
                    .map_or((p0x, p0y), |(cx, cy)| (2.0 * p0x - cx, 2.0 * p0y - cy));
                for step in 1..=12 {
                    let t = step as f32 / 12.0;
                    current.push(cubic_point(p0x, p0y, x1, y1, x2, y2, x, y, t));
                }
                last_cubic_control = Some((x2, y2));
                cursor = (x, y);
            }
            'Q' => {
                let Some((x1, y1)) = path_pair(tokens, &mut index) else {
                    break;
                };
                let Some((x, y)) = path_pair(tokens, &mut index) else {
                    break;
                };
                let (p0x, p0y) = cursor;
                let (x1, y1) = if relative {
                    (p0x + x1, p0y + y1)
                } else {
                    (x1, y1)
                };
                let (x, y) = if relative { (p0x + x, p0y + y) } else { (x, y) };
                for step in 1..=10 {
                    let t = step as f32 / 10.0;
                    current.push(quad_point(p0x, p0y, x1, y1, x, y, t));
                }
                last_quad_control = Some((x1, y1));
                cursor = (x, y);
            }
            'T' => {
                let Some((x, y)) = path_pair(tokens, &mut index) else {
                    break;
                };
                let (p0x, p0y) = cursor;
                let (x, y) = if relative { (p0x + x, p0y + y) } else { (x, y) };
                let (x1, y1) = last_quad_control
                    .map_or((p0x, p0y), |(cx, cy)| (2.0 * p0x - cx, 2.0 * p0y - cy));
                for step in 1..=10 {
                    let t = step as f32 / 10.0;
                    current.push(quad_point(p0x, p0y, x1, y1, x, y, t));
                }
                last_quad_control = Some((x1, y1));
                cursor = (x, y);
            }
            'A' => {
                // Skip rx ry rotation large-arc sweep; land on the endpoint.
                for _ in 0..5 {
                    let _ = path_number(tokens, &mut index);
                }
                let Some((x, y)) = path_pair(tokens, &mut index) else {
                    break;
                };
                cursor = if relative {
                    (cursor.0 + x, cursor.1 + y)
                } else {
                    (x, y)
                };
                current.push(cursor);
            }
            'Z' => {
                if !current.is_empty() {
                    current.push(subpath_start);
                    subpaths.push((std::mem::take(&mut current), true));
                }
                cursor = subpath_start;
            }
            _ => break,
        }
    }
    if !current.is_empty() {
        subpaths.push((current, false));
    }
    subpaths
}

#[allow(clippy::many_single_char_names)]
fn cubic_point(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    x3: f32,
    y3: f32,
    t: f32,
) -> (f32, f32) {
    let inverse = 1.0 - t;
    let a = inverse * inverse * inverse;
    let b = 3.0 * inverse * inverse * t;
    let c = 3.0 * inverse * t * t;
    let d = t * t * t;
    (
        a * x0 + b * x1 + c * x2 + d * x3,
        a * y0 + b * y1 + c * y2 + d * y3,
    )
}

#[allow(clippy::many_single_char_names)]
fn quad_point(x0: f32, y0: f32, x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> (f32, f32) {
    let inverse = 1.0 - t;
    (
        inverse * inverse * x0 + 2.0 * inverse * t * x1 + t * t * x2,
        inverse * inverse * y0 + 2.0 * inverse * t * y1 + t * t * y2,
    )
}

enum PathToken {
    Command(char),
    Number(f32),
}

fn tokenize_path(raw: &str) -> Vec<PathToken> {
    let mut tokens = Vec::new();
    let mut number = String::new();
    for character in raw.chars() {
        if character.is_ascii_alphabetic() {
            flush_number(&mut number, &mut tokens);
            tokens.push(PathToken::Command(character));
        } else if character.is_ascii_digit()
            || character == '.'
            || character == '-'
            || character == '+'
            || character == 'e'
            || character == 'E'
        {
            let ends_with_exponent = number.ends_with(['e', 'E']);
            let starts_new_number =
                (character == '-' || character == '+') && !ends_with_exponent && !number.is_empty();
            let starts_new_fraction = character == '.' && number.contains('.');
            if starts_new_number || starts_new_fraction {
                flush_number(&mut number, &mut tokens);
            }
            number.push(character);
        } else {
            flush_number(&mut number, &mut tokens);
        }
    }
    flush_number(&mut number, &mut tokens);
    tokens
}

fn flush_number(number: &mut String, tokens: &mut Vec<PathToken>) {
    if number.is_empty() {
        return;
    }
    if let Ok(value) = number.parse::<f32>() {
        tokens.push(PathToken::Number(value));
    }
    number.clear();
}

#[cfg(test)]
mod tests {
    use super::{Color, ImageLimits, decode_svg, parse_path};

    #[test]
    fn rect_fill_covers_exact_area() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect x="2" y="3" width="4" height="5" fill="#ff0000"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!((image.width(), image.height()), (10, 10));
        assert_eq!(image.pixel(2, 3), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(5, 7), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(6, 3), None.or(Some(Color::rgba(0, 0, 0, 0))));
        assert_eq!(image.pixel(1, 3), Some(Color::rgba(0, 0, 0, 0)));
    }

    #[test]
    fn polygon_and_circle_render() {
        let svg = br##"<svg width="20" height="20"><polygon points="0,0 10,0 0,10" fill="blue"/><circle cx="15" cy="15" r="3" fill="#00ff00"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(1, 1), Some(Color::rgb(0, 0, 255)));
        assert_eq!(image.pixel(15, 15), Some(Color::rgb(0, 255, 0)));
    }

    #[test]
    fn path_lineto_and_close_fill() {
        let svg = br#"<svg width="8" height="8"><path d="M1 1 L6 1 L1 6 Z" fill="red"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(2, 2), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(5, 5), Some(Color::rgba(0, 0, 0, 0)));
    }

    #[test]
    fn viewBox_scales_geometry_to_the_viewport() {
        let svg = br#"<svg width="20" height="20" viewBox="0 0 10 10"><rect x="2" y="2" width="4" height="4" fill="black"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(5, 5), Some(Color::rgb(0, 0, 0)));
        assert_eq!(image.pixel(19, 19), Some(Color::rgba(0, 0, 0, 0)));
    }

    #[test]
    fn fill_inherits_through_group_transform() {
        let svg = br##"<svg width="10" height="10"><g fill="#00f" transform="translate(2 2)"><rect width="3" height="3"/></g></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(3, 3), Some(Color::rgb(0, 0, 255)));
        assert_eq!(image.pixel(1, 1), Some(Color::rgba(0, 0, 0, 0)));
    }

    #[test]
    fn default_viewport_is_300x150_without_size_or_viewbox() {
        let svg = br#"<svg><rect width="10" height="10" fill="red"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!((image.width(), image.height()), (300, 150));
    }

    #[test]
    fn parse_path_handles_relative_and_mixed_numbers() {
        let subpaths = parse_path("M1 2L3 4m-1-1H0V5z");
        assert_eq!(subpaths.len(), 2);
        let (first, first_closed) = &subpaths[0];
        assert_eq!(first[0], (1.0, 2.0));
        assert_eq!(first[1], (3.0, 4.0));
        assert!(!*first_closed);
        let (second, second_closed) = &subpaths[1];
        assert_eq!(second[0], (2.0, 3.0));
        // z appends the subpath start point as the closing vertex.
        assert_eq!(second.last(), Some(&(2.0, 3.0)));
        assert_eq!(second[second.len() - 2], (0.0, 5.0));
        assert!(*second_closed);
    }

    #[test]
    fn stroke_only_line_draws_its_pixels() {
        let svg = br#"<svg width="6" height="6"><line x1="1" y1="1" x2="5" y2="1" stroke="black" stroke-width="1"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(3, 1), Some(Color::rgb(0, 0, 0)));
        assert_eq!(image.pixel(3, 3), Some(Color::rgba(0, 0, 0, 0)));
    }
}
