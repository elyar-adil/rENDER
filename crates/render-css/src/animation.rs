//! Sampling for the CSS Animations `@keyframes` timeline.
//!
//! The engine does not drive a compositor clock here; it computes one frame for
//! a caller-supplied time. That is enough to make `animation-*` properties and
//! `@keyframes` observable in layout and paint, and it keeps the sampler pure so
//! tests can ask for any instant.

use std::collections::BTreeMap;

use crate::computed::ComputedStyle;
use crate::properties::{
    CssColor, TransformFunction, TransformList, TypedPropertyValue, parse_typed_property,
};
use crate::stylesheet::KeyframesRule;

/// The declarations one element's animations contribute at `time` seconds.
///
/// Values are CSS text suitable for [`ComputedStyle::set_animated_property`].
/// Comma-separated animation lists are sampled in source order; when two
/// animations name the same property, the later one wins.
#[must_use]
pub fn sample_animations(
    keyframes: &BTreeMap<String, KeyframesRule>,
    style: &ComputedStyle,
    time: f32,
) -> Vec<(String, String)> {
    let entries = animation_entries(style);
    if entries.is_empty() {
        return Vec::new();
    }
    let mut sampled = BTreeMap::<String, String>::new();
    for entry in entries {
        if entry.duration <= 0.0 {
            continue;
        }
        let Some(rule) = keyframes.get(&entry.name) else {
            continue;
        };
        let Some(progress) = animation_progress(
            time,
            entry.delay,
            entry.duration,
            entry.iterations,
            &entry.fill,
            &entry.direction,
        ) else {
            continue;
        };
        let progress = timing_function(&entry.timing, progress);
        let mut properties = BTreeMap::<String, ()>::new();
        for frame in &rule.keyframes {
            for declaration in &frame.declarations {
                if !declaration.important {
                    properties.insert(declaration.name.clone(), ());
                }
            }
        }
        for property in properties.keys() {
            if property.starts_with("animation") {
                continue;
            }
            let base = style.get(property).map(|value| value.css_text().to_owned());
            if let Some(value) =
                sample_property(&rule.keyframes, property, progress, base.as_deref())
            {
                sampled.insert(property.clone(), value);
            }
        }
    }
    sampled.into_iter().collect()
}

/// Whether any of `styles` still needs animation frames at `time` seconds.
///
/// A finite animation stops driving frames after its last iteration. Fill modes
/// still paint the final value, but no further sample can change it, so the
/// browser clock can stop.
#[must_use]
pub fn any_animation_active<'a>(
    styles: impl IntoIterator<Item = &'a ComputedStyle>,
    time: f32,
) -> bool {
    styles
        .into_iter()
        .any(|style| animation_is_active(style, time))
}

/// Whether one element's animation can still change at `time` seconds.
#[must_use]
pub fn animation_is_active(style: &ComputedStyle, time: f32) -> bool {
    animation_entries(style).into_iter().any(|entry| {
        if entry.paused || entry.duration <= 0.0 {
            return false;
        }
        let total = entry.delay + entry.duration * entry.iterations;
        if total.is_finite() {
            (time as f64) < f64::from(total)
        } else {
            true
        }
    })
}

fn property_text(style: &ComputedStyle, name: &str) -> Option<String> {
    style
        .get(name)
        .map(|value| value.css_text().trim().to_owned())
}

#[derive(Clone, Debug, PartialEq)]
struct AnimationEntry {
    name: String,
    duration: f32,
    delay: f32,
    timing: String,
    iterations: f32,
    direction: String,
    fill: String,
    paused: bool,
}

fn animation_entries(style: &ComputedStyle) -> Vec<AnimationEntry> {
    let Some(name_value) = style.get("animation-name") else {
        return Vec::new();
    };
    let names = split_top_level_commas(name_value.css_text());
    if names.is_empty()
        || (names.len() == 1 && (names[0].is_empty() || names[0].eq_ignore_ascii_case("none")))
    {
        return Vec::new();
    }
    let durations =
        time_list(&property_text(style, "animation-duration").unwrap_or_else(|| "0s".to_owned()));
    let delays =
        time_list(&property_text(style, "animation-delay").unwrap_or_else(|| "0s".to_owned()));
    let timings = string_list(
        &property_text(style, "animation-timing-function").unwrap_or_else(|| "ease".to_owned()),
    );
    let iterations = number_list(
        &property_text(style, "animation-iteration-count").unwrap_or_else(|| "1".to_owned()),
        1.0,
    );
    let directions = string_list(
        &property_text(style, "animation-direction").unwrap_or_else(|| "normal".to_owned()),
    );
    let fills = string_list(
        &property_text(style, "animation-fill-mode").unwrap_or_else(|| "none".to_owned()),
    );
    let play_states = string_list(
        &property_text(style, "animation-play-state").unwrap_or_else(|| "running".to_owned()),
    );
    let mut entries = Vec::new();
    for (index, raw_name) in names.iter().enumerate() {
        let name = raw_name.trim_matches(['\'', '"']);
        if name.is_empty() || name.eq_ignore_ascii_case("none") {
            continue;
        }
        entries.push(AnimationEntry {
            name: name.to_owned(),
            duration: list_value(&durations, index).unwrap_or(0.0),
            delay: list_value(&delays, index).unwrap_or(0.0),
            timing: list_value(&timings, index).unwrap_or_else(|| "ease".to_owned()),
            iterations: list_value(&iterations, index).unwrap_or(1.0),
            direction: list_value(&directions, index).unwrap_or_else(|| "normal".to_owned()),
            fill: list_value(&fills, index).unwrap_or_else(|| "none".to_owned()),
            paused: list_value(&play_states, index)
                .is_some_and(|value| value.eq_ignore_ascii_case("paused")),
        });
    }
    entries
}

fn list_value<T: Clone>(values: &[T], index: usize) -> Option<T> {
    values
        .get(index.min(values.len().saturating_sub(1)))
        .cloned()
}

fn string_list(text: &str) -> Vec<String> {
    split_top_level_commas(text)
}

fn time_list(text: &str) -> Vec<f32> {
    split_top_level_commas(text)
        .iter()
        .filter_map(|value| parse_time(value))
        .collect()
}

fn number_list(text: &str, default: f32) -> Vec<f32> {
    split_top_level_commas(text)
        .iter()
        .map(|value| {
            if value.eq_ignore_ascii_case("infinite") {
                f32::INFINITY
            } else {
                value.parse::<f32>().unwrap_or(default)
            }
        })
        .collect()
}

fn split_top_level_commas(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0_u32;
    let mut quote = None;
    for (index, character) in value.char_indices() {
        match (quote, character) {
            (Some(expected), character) if character == expected => quote = None,
            (None, '\'' | '"') => quote = Some(character),
            (None, '(') => depth = depth.saturating_add(1),
            (None, ')') => depth = depth.saturating_sub(1),
            (None, ',') if depth == 0 => {
                parts.push(value[start..index].trim().to_owned());
                start = index + 1;
            }
            _ => {}
        }
    }
    let last = value[start..].trim();
    if !last.is_empty() {
        parts.push(last.to_owned());
    }
    parts.retain(|part| !part.is_empty());
    parts
}

fn parse_time(text: &str) -> Option<f32> {
    if let Some(value) = text.strip_suffix("ms") {
        return value.trim().parse::<f32>().ok().map(|value| value / 1000.0);
    }
    if let Some(value) = text.strip_suffix('s') {
        return value.trim().parse::<f32>().ok();
    }
    text.parse::<f32>().ok()
}

fn animation_progress(
    time: f32,
    delay: f32,
    duration: f32,
    iterations: f32,
    fill: &str,
    direction: &str,
) -> Option<f32> {
    let fill = fill.to_ascii_lowercase();
    let backwards = fill.contains("backwards") || fill.contains("both");
    let forwards = fill.contains("forwards") || fill.contains("both");
    let total = delay + duration * iterations;
    if time < delay {
        return backwards.then_some(0.0);
    }
    if total.is_finite() && time >= total {
        return forwards.then_some(1.0);
    }
    let active = time - delay;
    let position = active / duration;
    let iteration = position.floor();
    let mut local = position - iteration;
    if !local.is_finite() {
        local = 0.0;
    }
    let direction = direction.to_ascii_lowercase();
    let reverse = match direction.as_str() {
        "reverse" => true,
        "alternate" => (iteration as i64) % 2 == 1,
        "alternate-reverse" => (iteration as i64) % 2 == 0,
        _ => false,
    };
    Some(if reverse { 1.0 - local } else { local })
}

/// Evaluate one CSS timing function (`linear`, `ease*`, `cubic-bezier`,
/// `steps`, `step-start`/`step-end`) at `progress`.
pub(crate) fn timing_function(text: &str, progress: f32) -> f32 {
    let first = text.split(',').next().unwrap_or("ease").trim();
    if let Some(rest) = first.strip_prefix("cubic-bezier(") {
        let rest = rest.trim_end_matches(')');
        let values: Vec<f32> = rest
            .split(',')
            .filter_map(|value| value.trim().parse::<f32>().ok())
            .collect();
        if let [x1, y1, x2, y2] = values.as_slice() {
            return cubic_bezier(*x1, *y1, *x2, *y2, progress);
        }
    }
    if let Some(rest) = first.strip_prefix("steps(") {
        let rest = rest.trim_end_matches(')');
        let mut parts = rest.split(',');
        let count = parts
            .next()
            .and_then(|value| value.trim().parse::<f32>().ok())
            .unwrap_or(1.0)
            .max(1.0);
        let position = parts.next().unwrap_or("end").trim();
        let stepped = if position.eq_ignore_ascii_case("start")
            || position.eq_ignore_ascii_case("jump-start")
        {
            (progress * count).ceil() / count
        } else {
            (progress * count).floor() / count
        };
        return stepped.clamp(0.0, 1.0);
    }
    match first.to_ascii_lowercase().as_str() {
        "linear" => progress,
        "ease-in" => cubic_bezier(0.42, 0.0, 1.0, 1.0, progress),
        "ease-out" => cubic_bezier(0.0, 0.0, 0.58, 1.0, progress),
        "ease-in-out" => cubic_bezier(0.42, 0.0, 0.58, 1.0, progress),
        _ => cubic_bezier(0.25, 0.1, 0.25, 1.0, progress),
    }
}

fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    let mut guess = x;
    for _ in 0..8 {
        let current = bezier(guess, x1, x2);
        let slope = bezier_slope(guess, x1, x2);
        if (current - x).abs() < 1.0e-5 || slope.abs() < 1.0e-5 {
            break;
        }
        guess = (guess - (current - x) / slope).clamp(0.0, 1.0);
    }
    bezier(guess, y1, y2).clamp(0.0, 1.0)
}

fn bezier(t: f32, first: f32, second: f32) -> f32 {
    let inverse = 1.0 - t;
    3.0 * inverse * inverse * t * first + 3.0 * inverse * t * t * second + t * t * t
}

fn bezier_slope(t: f32, first: f32, second: f32) -> f32 {
    let inverse = 1.0 - t;
    3.0 * inverse * inverse * first
        + 6.0 * inverse * t * (second - first)
        + 3.0 * t * t * (1.0 - second)
}

fn sample_property(
    frames: &[crate::stylesheet::Keyframe],
    property: &str,
    progress: f32,
    base: Option<&str>,
) -> Option<String> {
    let mut points = BTreeMap::<u32, String>::new();
    for frame in frames {
        for declaration in &frame.declarations {
            if !declaration.important && declaration.name.eq_ignore_ascii_case(property) {
                points.insert(frame.offset, declaration.value.clone());
            }
        }
    }
    if points.is_empty() {
        return None;
    }
    if let Some(base) = base {
        points.entry(0).or_insert_with(|| base.to_owned());
        points.entry(10_000).or_insert_with(|| base.to_owned());
    }
    let target = (progress.clamp(0.0, 1.0) * 10_000.0).round() as u32;
    let mut before = None;
    let mut after = None;
    for (offset, value) in &points {
        if *offset <= target {
            before = Some((*offset, value));
        }
        if *offset >= target {
            after = Some((*offset, value));
            break;
        }
    }
    let (before_offset, before_value) = before?;
    let (after_offset, after_value) = after.unwrap_or((before_offset, before_value));
    if before_offset == after_offset || before_value == after_value {
        return Some(before_value.clone());
    }
    let local = f32::from(u16::try_from(target - before_offset).ok()?)
        / f32::from(u16::try_from(after_offset - before_offset).ok()?);
    if let Some(value) = interpolate_property(property, before_value, after_value, local) {
        return Some(value);
    }
    Some(if local < 0.5 {
        before_value.clone()
    } else {
        after_value.clone()
    })
}

/// Interpolate a property, with transform-specific handling for `none`.
///
/// `transform: none` is the identity transform, not a discrete value. Its
/// zero-valued function list is built from the other endpoint so lengths and
/// units match and the ordinary numeric interpolator can do the work.
pub(crate) fn interpolate_property(
    property: &str,
    from: &str,
    to: &str,
    progress: f32,
) -> Option<String> {
    if property.eq_ignore_ascii_case("transform")
        && let Some(value) = interpolate_transform(from, to, progress)
    {
        return Some(value);
    }
    let from = normalize_zero_length(property, from);
    let to = normalize_zero_length(property, to);
    interpolate(&from, &to, progress)
}

/// CSS computes a unitless zero length as `0`, while the other endpoint often
/// carries a unit. Treat the zero as `0px` for length properties so the two
/// endpoints share a literal skeleton.
fn normalize_zero_length<'a>(property: &str, value: &'a str) -> std::borrow::Cow<'a, str> {
    if value.trim() == "0" && is_length_property(property) {
        std::borrow::Cow::Borrowed("0px")
    } else {
        std::borrow::Cow::Borrowed(value)
    }
}

fn is_length_property(property: &str) -> bool {
    matches!(
        property,
        "width"
            | "height"
            | "min-width"
            | "min-height"
            | "max-width"
            | "max-height"
            | "top"
            | "right"
            | "bottom"
            | "left"
            | "margin-top"
            | "margin-right"
            | "margin-bottom"
            | "margin-left"
            | "padding-top"
            | "padding-right"
            | "padding-bottom"
            | "padding-left"
            | "border-top-width"
            | "border-right-width"
            | "border-bottom-width"
            | "border-left-width"
            | "border-top-left-radius"
            | "border-top-right-radius"
            | "border-bottom-right-radius"
            | "border-bottom-left-radius"
            | "font-size"
            | "line-height"
            | "letter-spacing"
            | "word-spacing"
            | "text-indent"
            | "flex-basis"
            | "gap"
            | "row-gap"
            | "column-gap"
            | "outline-offset"
            | "background-position"
            | "background-size"
            | "mask-position"
            | "mask-size"
            | "stroke-width"
            | "stroke-dashoffset"
    )
}

fn interpolate_transform(from: &str, to: &str, progress: f32) -> Option<String> {
    let from_list = parse_transform(from)?;
    let to_list = parse_transform(to)?;
    let from_css = if from_list.0.is_empty() {
        zero_transform(&to_list)
    } else {
        from_list.to_css()
    };
    let to_css = if to_list.0.is_empty() {
        zero_transform(&from_list)
    } else {
        to_list.to_css()
    };
    if from_css == to_css {
        return Some(from_css);
    }
    interpolate_numbers(&from_css, &to_css, progress)
}

fn parse_transform(value: &str) -> Option<TransformList> {
    match parse_typed_property("transform", value) {
        Some(Ok(TypedPropertyValue::Transform(list))) => Some(list),
        _ => None,
    }
}

fn zero_transform(list: &TransformList) -> String {
    if list.0.is_empty() {
        return "none".to_owned();
    }
    list.0
        .iter()
        .map(zero_transform_function)
        .collect::<Vec<_>>()
        .join(" ")
}

fn zero_transform_function(function: &TransformFunction) -> String {
    match function {
        TransformFunction::Matrix(_) => "matrix(1, 0, 0, 1, 0, 0)".to_owned(),
        TransformFunction::Translate(x, y) => format!(
            "translate({}, {})",
            replace_numbers(&x.to_css(), 0.0),
            replace_numbers(&y.to_css(), 0.0)
        ),
        TransformFunction::Scale(x, y) => format!(
            "scale({}, {})",
            replace_numbers(&format_number(f64::from(*x)), 1.0),
            replace_numbers(&format_number(f64::from(*y)), 1.0)
        ),
        TransformFunction::Rotate(_) | TransformFunction::Skew(..) => {
            replace_numbers(&function.to_css(), 0.0)
        }
    }
}

fn replace_numbers(value: &str, replacement: f64) -> String {
    let Some((literals, numbers)) = split_numbers(value) else {
        return value.to_owned();
    };
    let mut output = String::new();
    for (index, literal) in literals.iter().enumerate() {
        output.push_str(literal);
        if index < numbers.len() {
            output.push_str(&format_number(replacement));
        }
    }
    output
}

pub(crate) fn interpolate(from: &str, to: &str, progress: f32) -> Option<String> {
    if let (Some(from), Some(to)) = (parse_color(from), parse_color(to)) {
        return Some(interpolate_color(from, to, progress).to_css());
    }
    interpolate_numbers(from, to, progress)
}

fn parse_color(value: &str) -> Option<CssColor> {
    match parse_typed_property("color", value) {
        Some(Ok(TypedPropertyValue::Color(color))) => match color {
            CssColor::Srgb { .. } => Some(color),
            _ => None,
        },
        _ => None,
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "colour channels are rounded into 0..=255"
)]
fn interpolate_color(from: CssColor, to: CssColor, progress: f32) -> CssColor {
    let (
        CssColor::Srgb {
            red: from_red,
            green: from_green,
            blue: from_blue,
            alpha: from_alpha,
        },
        CssColor::Srgb {
            red: to_red,
            green: to_green,
            blue: to_blue,
            alpha: to_alpha,
        },
    ) = (from, to)
    else {
        return to;
    };
    let channel = |from: u8, to: u8| -> u8 {
        (f32::from(from) + (f32::from(to) - f32::from(from)) * progress)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    CssColor::Srgb {
        red: channel(from_red, to_red),
        green: channel(from_green, to_green),
        blue: channel(from_blue, to_blue),
        alpha: from_alpha + (to_alpha - from_alpha) * progress,
    }
}

fn interpolate_numbers(from: &str, to: &str, progress: f32) -> Option<String> {
    let (from_literals, from_numbers) = split_numbers(from)?;
    let (to_literals, to_numbers) = split_numbers(to)?;
    if from_literals != to_literals || from_numbers.len() != to_numbers.len() {
        return None;
    }
    let mut output = String::new();
    for (index, (start, end)) in from_numbers.iter().zip(&to_numbers).enumerate() {
        output.push_str(&from_literals[index]);
        output.push_str(&format_number(start + (end - start) * f64::from(progress)));
    }
    output.push_str(from_literals.last()?);
    Some(output)
}

fn split_numbers(value: &str) -> Option<(Vec<String>, Vec<f64>)> {
    let bytes = value.as_bytes();
    let mut literals = Vec::new();
    let mut numbers = Vec::new();
    let mut literal_start = 0;
    let mut index = 0;
    while index < bytes.len() {
        let starts_number = bytes[index].is_ascii_digit()
            || (matches!(bytes[index], b'-' | b'+' | b'.')
                && bytes
                    .get(index + 1)
                    .is_some_and(|next| next.is_ascii_digit() || *next == b'.'));
        if !starts_number {
            index += 1;
            continue;
        }
        let number_start = index;
        if matches!(bytes[index], b'-' | b'+') {
            index += 1;
        }
        let mut seen_dot = false;
        while index < bytes.len() {
            match bytes[index] {
                b'0'..=b'9' => index += 1,
                b'.' if !seen_dot => {
                    seen_dot = true;
                    index += 1;
                }
                _ => break,
            }
        }
        if index < bytes.len() && matches!(bytes[index], b'e' | b'E') {
            let exponent_start = index;
            index += 1;
            if index < bytes.len() && matches!(bytes[index], b'+' | b'-') {
                index += 1;
            }
            let digits_start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            if digits_start == index {
                index = exponent_start;
            }
        }
        literals.push(value[literal_start..number_start].to_owned());
        numbers.push(value[number_start..index].parse::<f64>().ok()?);
        literal_start = index;
    }
    literals.push(value[literal_start..].to_owned());
    Some((literals, numbers))
}

fn format_number(value: f64) -> String {
    if (value - value.round()).abs() < 1.0e-4 {
        return format!("{}", value.round() as i64);
    }
    let mut text = format!("{value:.4}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{
        animation_progress, interpolate_numbers, interpolate_property, sample_animations,
        sample_property, split_numbers,
    };
    use crate::computed::ComputedStyle;
    use crate::stylesheet::{Declaration, Keyframe, KeyframesRule};

    fn declaration(name: &str, value: &str) -> Declaration {
        Declaration {
            name: name.to_owned(),
            value: value.to_owned(),
            important: false,
        }
    }

    #[test]
    fn numeric_values_interpolate_inside_transform_functions() {
        assert_eq!(
            interpolate_numbers("rotate(0deg)", "rotate(360deg)", 0.5),
            Some("rotate(180deg)".to_owned())
        );
        assert_eq!(
            interpolate_numbers("translateX(0px)", "translateX(10px)", 0.25),
            Some("translateX(2.5px)".to_owned())
        );
        assert_eq!(interpolate_numbers("none", "translateX(10px)", 0.5), None);
        assert_eq!(split_numbers("translateX(-2.5px)").unwrap().1, vec![-2.5]);
    }

    #[test]
    fn keyframe_property_sampling_interpolates_and_steps() {
        let frames = vec![
            Keyframe {
                offset: 0,
                declarations: vec![declaration("opacity", "0"), declaration("display", "block")],
            },
            Keyframe {
                offset: 10_000,
                declarations: vec![declaration("opacity", "1"), declaration("display", "none")],
            },
        ];
        assert_eq!(
            sample_property(&frames, "opacity", 0.25, None),
            Some("0.25".to_owned())
        );
        // A discrete property flips at the halfway point.
        assert_eq!(
            sample_property(&frames, "display", 0.25, None),
            Some("block".to_owned())
        );
        assert_eq!(
            sample_property(&frames, "display", 0.75, None),
            Some("none".to_owned())
        );
    }

    #[test]
    fn the_browser_clock_stops_after_a_finite_animation() {
        use crate::computed::ComputedStyle;

        let mut style = ComputedStyle::default();
        style.set_animated_property("animation-name", "fade");
        style.set_animated_property("animation-duration", "1s");
        assert!(super::animation_is_active(&style, 0.0));
        assert!(super::animation_is_active(&style, 0.5));
        assert!(!super::animation_is_active(&style, 1.0));
        assert!(!super::animation_is_active(&style, 2.0));

        style.set_animated_property("animation-iteration-count", "infinite");
        assert!(super::animation_is_active(&style, 3600.0));

        style.set_animated_property("animation-play-state", "paused");
        assert!(!super::animation_is_active(&style, 0.5));
    }

    #[test]
    fn animation_progress_honours_delay_fill_and_direction() {
        assert_eq!(
            animation_progress(0.0, 0.0, 1.0, 1.0, "none", "normal"),
            Some(0.0)
        );
        assert_eq!(
            animation_progress(1.0, 0.0, 1.0, 1.0, "none", "normal"),
            None
        );
        assert_eq!(
            animation_progress(1.0, 0.0, 1.0, 1.0, "forwards", "normal"),
            Some(1.0)
        );
        assert_eq!(
            animation_progress(0.25, 0.5, 1.0, 1.0, "both", "normal"),
            Some(0.0)
        );
        assert_eq!(
            animation_progress(1.25, 0.0, 1.0, 2.0, "none", "alternate"),
            Some(0.75)
        );
    }
    #[test]
    fn multiple_animations_compose_in_list_order() {
        let mut style = ComputedStyle::default();
        for (name, value) in [
            ("animation-name", "first, second"),
            ("animation-duration", "1s, 1s"),
            ("animation-timing-function", "linear, linear"),
            ("animation-fill-mode", "both, both"),
        ] {
            style.set_animated_property(name, value);
        }
        let keyframes = std::collections::BTreeMap::from([
            (
                "first".to_owned(),
                KeyframesRule {
                    name: "first".to_owned(),
                    keyframes: vec![
                        Keyframe {
                            offset: 0,
                            declarations: vec![declaration("opacity", "0")],
                        },
                        Keyframe {
                            offset: 10_000,
                            declarations: vec![declaration("opacity", "1")],
                        },
                    ],
                    media: Vec::new(),
                    layer: None,
                },
            ),
            (
                "second".to_owned(),
                KeyframesRule {
                    name: "second".to_owned(),
                    keyframes: vec![
                        Keyframe {
                            offset: 0,
                            declarations: vec![
                                declaration("opacity", "0.5"),
                                declaration("width", "10px"),
                            ],
                        },
                        Keyframe {
                            offset: 10_000,
                            declarations: vec![
                                declaration("opacity", "1"),
                                declaration("width", "30px"),
                            ],
                        },
                    ],
                    media: Vec::new(),
                    layer: None,
                },
            ),
        ]);
        let sampled: std::collections::BTreeMap<_, _> = sample_animations(&keyframes, &style, 0.5)
            .into_iter()
            .collect();
        assert_eq!(sampled.get("opacity").map(String::as_str), Some("0.75"));
        assert_eq!(sampled.get("width").map(String::as_str), Some("20px"));
    }

    #[test]
    fn transform_none_interpolates_from_the_identity() {
        assert_eq!(
            interpolate_property("transform", "none", "translateY(-2px)", 0.5),
            Some("translate(0px, -1px)".to_owned())
        );
        assert_eq!(
            interpolate_property("transform", "none", "scale(1.2)", 0.5),
            Some("scale(1.1, 1.1)".to_owned())
        );
        assert_eq!(
            interpolate_property("transform", "rotate(90deg)", "none", 0.5),
            Some("rotate(45deg)".to_owned())
        );
    }
    #[test]
    fn unitless_zero_interpolates_to_a_length() {
        assert_eq!(
            interpolate_property("width", "0", "100px", 0.5),
            Some("50px".to_owned())
        );
        assert_eq!(
            interpolate_property("opacity", "0", "1", 0.5),
            Some("0.5".to_owned())
        );
    }
}
