//! CSS Transitions: detecting changed computed values, sampling the transition
//! timeline, and reporting `transitionend`.
//!
//! The sampler owns the state a single-shot renderer cannot hold itself: the
//! previous target value of every transitionable property and the active
//! transitions. A caller keeps one [`TransitionState`] per document and passes
//! it to every render, at the document timeline's current time in seconds.

use std::collections::{BTreeMap, BTreeSet};

use render_dom::NodeId;

use crate::animation::{interpolate, timing_function};
use crate::computed::ComputedStyle;

/// Per-document transition state, carried from one render to the next.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TransitionState {
    nodes: BTreeMap<NodeId, NodeTransitions>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct NodeTransitions {
    /// The computed target value of each transitionable property at the last
    /// render, before any transition sampling replaced it.
    targets: BTreeMap<String, String>,
    active: BTreeMap<String, ActiveTransition>,
}

#[derive(Clone, Debug, PartialEq)]
struct ActiveTransition {
    from: String,
    to: String,
    start: f32,
    delay: f32,
    duration: f32,
    timing: String,
}

/// One `transitionend` the sampler observed while rendering.
#[derive(Clone, Debug, PartialEq)]
pub struct TransitionEvent {
    pub node: NodeId,
    pub property: String,
    /// The transition's own duration, excluding its delay, in seconds.
    pub elapsed_time: f32,
}

impl TransitionState {
    /// Whether a transition is still being sampled.
    ///
    /// Entries stay here until a later render observes their end, so the
    /// browser clock keeps producing frames until the ``transitionend`` event
    /// can be emitted.
    #[must_use]
    pub fn has_active(&self) -> bool {
        self.nodes.values().any(|node| !node.active.is_empty())
    }

    /// The number of active transitions, for diagnostics and tests.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.nodes.values().map(|node| node.active.len()).sum()
    }
}

/// Detect value changes, advance active transitions, and report completions.
///
/// `time` is the document timeline position in seconds. A transition that ends
/// at or before `time` is painted at its final value and emits one
/// [`TransitionEvent`].
pub fn sample_transitions(
    styles: &mut BTreeMap<NodeId, ComputedStyle>,
    state: &mut TransitionState,
    time: f32,
) -> Vec<TransitionEvent> {
    let mut events = Vec::new();
    let mut live = BTreeSet::new();
    for (node, style) in styles.iter_mut() {
        live.insert(*node);
        let plan = TransitionPlan::from_style(style);
        let node_state = state.nodes.entry(*node).or_default();
        let candidates = plan.candidates(style, node_state);
        let mut cancel = Vec::new();
        for property in candidates {
            let Some(target) = style
                .get(&property)
                .map(|value| value.css_text().trim().to_owned())
            else {
                continue;
            };
            let previous = node_state.targets.insert(property.clone(), target.clone());
            let covered = plan.covers(&property);
            if let Some(active) = node_state.active.get_mut(&property) {
                if active.to == target {
                    continue;
                }
                if covered {
                    let duration = plan.duration_for(&property);
                    if duration > 0.0 {
                        let from =
                            sample_active(active, time).unwrap_or_else(|| active.from.clone());
                        *active = ActiveTransition {
                            from,
                            to: target,
                            start: time,
                            delay: plan.delay_for(&property),
                            duration,
                            timing: plan.timing_for(&property),
                        };
                        continue;
                    }
                }
                cancel.push(property.clone());
                continue;
            }
            let Some(previous) = previous else {
                continue;
            };
            if !covered || previous == target {
                continue;
            }
            let duration = plan.duration_for(&property);
            if duration <= 0.0 || interpolate(&previous, &target, 0.0).is_none() {
                continue;
            }
            node_state.active.insert(
                property.clone(),
                ActiveTransition {
                    from: previous,
                    to: target,
                    start: time,
                    delay: plan.delay_for(&property),
                    duration,
                    timing: plan.timing_for(&property),
                },
            );
        }
        for property in cancel {
            node_state.active.remove(&property);
        }

        let mut ended = Vec::new();
        for (property, active) in &node_state.active {
            let end = active.start + active.delay + active.duration;
            if time >= end {
                style.set_animated_property(property, &active.to);
                events.push(TransitionEvent {
                    node: *node,
                    property: property.clone(),
                    elapsed_time: active.duration,
                });
                ended.push(property.clone());
            } else if let Some(value) = sample_active(active, time) {
                style.set_animated_property(property, &value);
            }
        }
        for property in ended {
            node_state.active.remove(&property);
        }
    }
    state.nodes.retain(|node, _| live.contains(node));
    events
}

fn sample_active(active: &ActiveTransition, time: f32) -> Option<String> {
    let local = time - active.start - active.delay;
    let progress = (local / active.duration).clamp(0.0, 1.0);
    let eased = timing_function(&active.timing, progress);
    interpolate(&active.from, &active.to, eased)
}

struct TransitionPlan {
    properties: PropertyList,
    durations: Vec<f32>,
    delays: Vec<f32>,
    timings: Vec<String>,
}

enum PropertyList {
    All,
    None,
    Named(Vec<String>),
}

impl TransitionPlan {
    fn from_style(style: &ComputedStyle) -> Self {
        let properties = split_top_level_commas(
            &style
                .get("transition-property")
                .map_or_else(|| "all".to_owned(), |value| value.css_text().to_owned()),
        );
        let properties = if properties.len() == 1 && properties[0].eq_ignore_ascii_case("all") {
            PropertyList::All
        } else if properties.len() == 1 && properties[0].eq_ignore_ascii_case("none") {
            PropertyList::None
        } else {
            PropertyList::Named(properties)
        };
        let durations = time_list(
            &style
                .get("transition-duration")
                .map_or_else(|| "0s".to_owned(), |value| value.css_text().to_owned()),
        );
        let delays = time_list(
            &style
                .get("transition-delay")
                .map_or_else(|| "0s".to_owned(), |value| value.css_text().to_owned()),
        );
        let mut timings = split_top_level_commas(
            &style
                .get("transition-timing-function")
                .map_or_else(|| "ease".to_owned(), |value| value.css_text().to_owned()),
        );
        if timings.is_empty() {
            timings.push("ease".to_owned());
        }
        Self {
            properties,
            durations,
            delays,
            timings,
        }
    }

    fn covers(&self, property: &str) -> bool {
        match &self.properties {
            PropertyList::All => true,
            PropertyList::None => false,
            PropertyList::Named(names) => names.iter().any(|name| name == property),
        }
    }

    fn index_for(&self, property: &str) -> usize {
        match &self.properties {
            PropertyList::Named(names) => {
                names.iter().position(|name| name == property).unwrap_or(0)
            }
            _ => 0,
        }
    }

    fn duration_for(&self, property: &str) -> f32 {
        list_value(&self.durations, self.index_for(property)).unwrap_or(0.0)
    }

    fn delay_for(&self, property: &str) -> f32 {
        list_value(&self.delays, self.index_for(property)).unwrap_or(0.0)
    }

    fn timing_for(&self, property: &str) -> String {
        self.timings
            .get(
                self.index_for(property)
                    .min(self.timings.len().saturating_sub(1)),
            )
            .cloned()
            .unwrap_or_else(|| "ease".to_owned())
    }

    fn candidates(&self, style: &ComputedStyle, node_state: &NodeTransitions) -> Vec<String> {
        let mut candidates = BTreeSet::new();
        match &self.properties {
            PropertyList::All => {
                for name in style.properties().keys() {
                    if is_common_transition_property(name) {
                        candidates.insert(name.clone());
                    }
                }
            }
            PropertyList::None => {}
            PropertyList::Named(names) => {
                for name in names {
                    if !name.eq_ignore_ascii_case("all") && !name.eq_ignore_ascii_case("none") {
                        candidates.insert(name.clone());
                    }
                }
            }
        }
        // An active transition keeps being sampled even when the property list
        // changed under it, and old targets must keep tracking the base value
        // so a later re-enable has a truthful starting point.
        candidates.extend(node_state.active.keys().cloned());
        candidates.extend(node_state.targets.keys().cloned());
        candidates.into_iter().collect()
    }
}

fn list_value(values: &[f32], index: usize) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    values.get(index.min(values.len() - 1)).copied()
}

fn time_list(text: &str) -> Vec<f32> {
    split_top_level_commas(text)
        .iter()
        .filter_map(|value| parse_time(value))
        .collect()
}

fn parse_time(text: &str) -> Option<f32> {
    let text = text.trim();
    if let Some(value) = text.strip_suffix("ms") {
        return value.trim().parse::<f32>().ok().map(|value| value / 1000.0);
    }
    if let Some(value) = text.strip_suffix('s') {
        return value.trim().parse::<f32>().ok();
    }
    text.parse::<f32>().ok()
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

/// The subset of animatable properties `transition: all` tracks.
///
/// A property must have a computable value in this engine and an interpolator.
/// Discrete properties (display, z-index) are deliberately absent: without
/// `transition-behavior: allow-discrete` they must change immediately.
fn is_common_transition_property(name: &str) -> bool {
    matches!(
        name,
        "opacity"
            | "color"
            | "background-color"
            | "background-position"
            | "background-size"
            | "border-top-color"
            | "border-right-color"
            | "border-bottom-color"
            | "border-left-color"
            | "border-top-width"
            | "border-right-width"
            | "border-bottom-width"
            | "border-left-width"
            | "border-top-left-radius"
            | "border-top-right-radius"
            | "border-bottom-right-radius"
            | "border-bottom-left-radius"
            | "outline-color"
            | "outline-width"
            | "outline-offset"
            | "width"
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
            | "font-size"
            | "line-height"
            | "letter-spacing"
            | "word-spacing"
            | "text-indent"
            | "transform"
            | "filter"
            | "flex-grow"
            | "flex-shrink"
            | "flex-basis"
            | "gap"
            | "row-gap"
            | "column-gap"
            | "mask-position"
            | "mask-size"
            | "fill"
            | "stroke"
            | "fill-opacity"
            | "stroke-opacity"
            | "stroke-width"
            | "stroke-dashoffset"
            | "stop-opacity"
    )
}

#[cfg(test)]
mod tests {
    use super::{TransitionState, sample_transitions};
    use crate::computed::ComputedStyle;
    use render_dom::NodeId;
    use std::collections::BTreeMap;

    fn style_with(pairs: &[(&str, &str)]) -> ComputedStyle {
        let mut style = ComputedStyle::default();
        for (name, value) in pairs {
            style.set_animated_property(name, value);
        }
        style
    }

    #[test]
    fn a_changed_value_is_sampled_between_its_endpoints() {
        let node = NodeId::from_u64(1);
        let base = |opacity: &str| {
            style_with(&[
                ("opacity", opacity),
                ("transition-property", "opacity"),
                ("transition-duration", "1s"),
                ("transition-timing-function", "linear"),
            ])
        };
        let mut styles = BTreeMap::new();
        styles.insert(node, base("1"));
        let mut state = TransitionState::default();
        assert!(sample_transitions(&mut styles, &mut state, 0.0).is_empty());
        styles.insert(node, base("0"));
        assert!(sample_transitions(&mut styles, &mut state, 0.0).is_empty());
        styles.insert(node, base("0"));
        sample_transitions(&mut styles, &mut state, 0.5);
        assert_eq!(
            styles[&node].get("opacity").map(|value| value.css_text()),
            Some("0.5")
        );
        styles.insert(node, base("0"));
        let events = sample_transitions(&mut styles, &mut state, 1.0);
        assert_eq!(
            styles[&node].get("opacity").map(|value| value.css_text()),
            Some("0")
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].property, "opacity");
        assert!((events[0].elapsed_time - 1.0).abs() < 1.0e-6);
        assert!(!state.has_active());
    }

    #[test]
    fn a_delay_keeps_the_start_value_until_it_expires() {
        let node = NodeId::from_u64(2);
        let base = |width: &str| {
            style_with(&[
                ("width", width),
                ("transition-property", "width"),
                ("transition-duration", "1s"),
                ("transition-delay", "500ms"),
                ("transition-timing-function", "linear"),
            ])
        };
        let mut styles = BTreeMap::new();
        styles.insert(node, base("10px"));
        let mut state = TransitionState::default();
        sample_transitions(&mut styles, &mut state, 0.0);
        styles.insert(node, base("20px"));
        sample_transitions(&mut styles, &mut state, 0.25);
        assert_eq!(
            styles[&node].get("width").map(|value| value.css_text()),
            Some("10px")
        );
        styles.insert(node, base("20px"));
        sample_transitions(&mut styles, &mut state, 1.25);
        assert_eq!(
            styles[&node].get("width").map(|value| value.css_text()),
            Some("15px")
        );
    }

    #[test]
    fn a_zero_duration_change_is_immediate_and_emits_no_event() {
        let node = NodeId::from_u64(3);
        let base = |opacity: &str| {
            style_with(&[
                ("opacity", opacity),
                ("transition-property", "opacity"),
                ("transition-duration", "0s"),
            ])
        };
        let mut styles = BTreeMap::new();
        styles.insert(node, base("1"));
        let mut state = TransitionState::default();
        sample_transitions(&mut styles, &mut state, 0.0);
        styles.insert(node, base("0"));
        let events = sample_transitions(&mut styles, &mut state, 1.0);
        assert!(events.is_empty());
        assert_eq!(
            styles[&node].get("opacity").map(|value| value.css_text()),
            Some("0")
        );
    }
}
