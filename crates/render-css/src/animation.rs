//! Sampling for the CSS Animations `@keyframes` timeline.
//!
//! The engine does not drive a compositor clock here; it computes one frame for
//! a caller-supplied time. That is enough to make `animation-*` properties and
//! `@keyframes` observable in layout and paint, and it keeps the sampler pure so
//! tests can ask for any instant.

use std::collections::BTreeMap;

use crate::computed::ComputedStyle;
use crate::properties::{CssColor, TypedPropertyValue, parse_typed_property};
use crate::stylesheet::KeyframesRule;

/// The declarations one element's animations contribute at `time` seconds.
///
/// Values are CSS text suitable for [`ComputedStyle::set_animated_property`].
/// Only the first `animation-name` is sampled; a comma-separated list of
/// independent animations remains a documented gap.
#[must_use]
pub fn sample_animations(
    keyframes: &BTreeMap<String, KeyframesRule>,
    style: &ComputedStyle,
    time: f32,
) -> Vec<(String, String)> {
    let Some(name) = animation_name(style) else {
        return Vec::new();
    };
    let Some(rule) = keyframes.get(&name) else {
        return Vec::new();
    };
    let duration = animation_time(style, "animation-duration").unwrap_or(0.0);
    if duration <= 0.0 {
        return Vec::new();
    }
    let delay = animation_time(style, "animation-delay").unwrap_or(0.0);
    let iterations = animation_iterations(style).unwrap_or(1.0);
    let fill = property_text(style, "animation-fill-mode").unwrap_or_else(|| "none".to_owned());
    let direction =
        property_text(style, "animation-direction").unwrap_or_else(|| "normal".to_owned());
    let Some(progress) = animation_progress(time, delay, duration, iterations, &fill, &direction)
    else {
        return Vec::new();
    };
    let progress = apply_timing(style, progress);

    let mut properties = BTreeMap::<String, ()>::new();
    for frame in &rule.keyframes {
        for declaration in &frame.declarations {
            if !declaration.important {
                properties.insert(declaration.name.clone(), ());
            }
        }
    }
    let mut sampled = Vec::new();
    for property in properties.keys() {
        if property.starts_with("animation") {
            continue;
        }
        let base = style.get(property).map(|value| value.css_text().to_owned());
        if let Some(value) = sample_property(&rule.keyframes, property, progress, base.as_deref()) {
            sampled.push((property.clone(), value));
        }
    }
    sampled
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
    let Some(_name) = animation_name(style) else {
        return false;
    };
    if property_text(style, "animation-play-state")
        .is_some_and(|value| value.eq_ignore_ascii_case("paused"))
    {
        return false;
    }
    let Some(duration) = animation_time(style, "animation-duration") else {
        return false;
    };
    if duration <= 0.0 {
        return false;
    }
    let delay = animation_time(style, "animation-delay").unwrap_or(0.0);
    let iterations = animation_iterations(style).unwrap_or(1.0);
    let total = delay + duration * iterations;
    if total.is_finite() {
        (time as f64) < f64::from(total)
    } else {
        true
    }
}

fn property_text(style: &ComputedStyle, name: &str) -> Option<String> {
    style
        .get(name)
        .map(|value| value.css_text().trim().to_owned())
}

fn animation_name(style: &ComputedStyle) -> Option<String> {
    let text = property_text(style, "animation-name")?;
    let name = text.split(',').next()?.trim();
    if name.is_empty() || name.eq_ignore_ascii_case("none") {
        return None;
    }
    Some(name.trim_matches(['\'', '"']).to_owned())
}

fn animation_time(style: &ComputedStyle, name: &str) -> Option<f32> {
    let text = property_text(style, name)?;
    let first = text.split(',').next()?.trim();
    parse_time(first)
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

fn animation_iterations(style: &ComputedStyle) -> Option<f32> {
    let text = property_text(style, "animation-iteration-count")?;
    let first = text.split(',').next()?.trim();
    if first.eq_ignore_ascii_case("infinite") {
        return Some(f32::INFINITY);
    }
    first.parse::<f32>().ok().filter(|value| *value >= 0.0)
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

fn apply_timing(style: &ComputedStyle, progress: f32) -> f32 {
    let text =
        property_text(style, "animation-timing-function").unwrap_or_else(|| "ease".to_owned());
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
    if let Some(value) = interpolate(property, before_value, after_value, local) {
        return Some(value);
    }
    Some(if local < 0.5 {
        before_value.clone()
    } else {
        after_value.clone()
    })
}

fn interpolate(property: &str, from: &str, to: &str, progress: f32) -> Option<String> {
    if (property.eq_ignore_ascii_case("color") || property.ends_with("-color"))
        && let (Some(from), Some(to)) = (parse_color(from), parse_color(to))
    {
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
    use super::{animation_progress, interpolate_numbers, sample_property, split_numbers};
    use crate::stylesheet::{Declaration, Keyframe};

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
}
