//! CSS Transitions: detecting changed computed values, sampling the transition
//! timeline, and reporting `transitionend`.
//!
//! The sampler owns the state a single-shot renderer cannot hold itself: the
//! previous target value of every transitionable property and the active
//! transitions. A caller keeps one [`TransitionState`] per document and passes
//! it to every render, at the document timeline's current time in seconds.

use std::collections::{BTreeMap, BTreeSet};

use render_dom::NodeId;

use crate::animation::{interpolate_property, timing_function};
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
    /// The reversing shortening factor. 1 for a normal transition, smaller
    /// when an interrupted transition is reversed back toward its start.
    shortening: f32,
    /// The logical start state used to recognize a later reversal. For a
    /// normal transition this is `from`; for a reversed transition it is the
    /// old transition's end value (CSS Transitions 1 §3.1).
    reversing_adjusted_start: String,
    /// Whether a property with no numeric interpolator may step discretely.
    allow_discrete: bool,
}

/// Which transition event this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionEventKind {
    End,
    Cancel,
}

/// One `transitionend` or `transitioncancel` the sampler observed.
#[derive(Clone, Debug, PartialEq)]
pub struct TransitionEvent {
    pub kind: TransitionEventKind,
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

    /// Cancel every running transition, as a document switch requires, and
    /// return the `transitioncancel` events for the old document's runtime.
    #[must_use]
    pub fn cancel_all(&mut self, time: f32) -> Vec<TransitionEvent> {
        let mut events = Vec::new();
        for (node, node_state) in &self.nodes {
            for (property, active) in &node_state.active {
                events.push(transition_event(
                    *node,
                    property,
                    active,
                    time,
                    TransitionEventKind::Cancel,
                ));
            }
        }
        self.nodes.clear();
        events
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
                events.push(transition_event(
                    *node,
                    &property,
                    active,
                    time,
                    TransitionEventKind::Cancel,
                ));
                if covered {
                    let duration = plan.duration_for(&property);
                    if duration > 0.0 {
                        let from = sample_active(&property, active, time)
                            .unwrap_or_else(|| active.from.clone());
                        let reversing = target == active.reversing_adjusted_start;
                        let shortening = if reversing {
                            reversing_factor(active, time)
                        } else {
                            1.0
                        };
                        if shortening > 0.001 {
                            let delay = plan.delay_for(&property);
                            *active = ActiveTransition {
                                from: from.clone(),
                                to: target,
                                start: time,
                                delay: if delay < 0.0 {
                                    delay * shortening
                                } else {
                                    delay
                                },
                                duration: duration * shortening,
                                timing: plan.timing_for(&property),
                                shortening,
                                reversing_adjusted_start: if reversing {
                                    active.to.clone()
                                } else {
                                    from.clone()
                                },
                                allow_discrete: plan.allow_discrete(&property),
                            };
                            continue;
                        }
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
            let allow_discrete = plan.allow_discrete(&property);
            if duration <= 0.0
                || (!allow_discrete
                    && interpolate_property(&property, &previous, &target, 0.0).is_none())
            {
                continue;
            }
            node_state.active.insert(
                property.clone(),
                ActiveTransition {
                    from: previous.clone(),
                    to: target,
                    start: time,
                    delay: plan.delay_for(&property),
                    duration,
                    timing: plan.timing_for(&property),
                    shortening: 1.0,
                    reversing_adjusted_start: previous.clone(),
                    allow_discrete,
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
                    kind: TransitionEventKind::End,
                    node: *node,
                    property: property.clone(),
                    elapsed_time: active.duration,
                });
                ended.push(property.clone());
            } else if let Some(value) = sample_active(property, active, time) {
                style.set_animated_property(property, &value);
            }
        }
        for property in ended {
            node_state.active.remove(&property);
        }
    }
    let removed: Vec<(NodeId, Vec<(String, TransitionEvent)>)> = state
        .nodes
        .iter()
        .filter(|(node, _)| !live.contains(node))
        .map(|(node, node_state)| {
            let cancellations = node_state
                .active
                .iter()
                .map(|(property, active)| {
                    (
                        property.clone(),
                        transition_event(
                            *node,
                            property,
                            active,
                            time,
                            TransitionEventKind::Cancel,
                        ),
                    )
                })
                .collect::<Vec<_>>();
            (*node, cancellations)
        })
        .collect();
    for (_, cancellations) in removed {
        events.extend(cancellations.into_iter().map(|(_, event)| event));
    }
    state.nodes.retain(|node, _| live.contains(node));
    events
}

/// The portion of the value space an interrupted transition traversed.
///
/// CSS Transitions 1 §3.1: the timing output at the interruption, scaled by the
/// old transition's own shortening factor, plus the unshortened remainder.
fn reversing_factor(active: &ActiveTransition, time: f32) -> f32 {
    let progress = ((time - active.start - active.delay) / active.duration).clamp(0.0, 1.0);
    let output = timing_function(&active.timing, progress);
    (output.mul_add(active.shortening, 1.0 - active.shortening))
        .abs()
        .clamp(0.0, 1.0)
}

fn sample_active(property: &str, active: &ActiveTransition, time: f32) -> Option<String> {
    let local = time - active.start - active.delay;
    let progress = (local / active.duration).clamp(0.0, 1.0);
    let eased = timing_function(&active.timing, progress);
    interpolate_property(property, &active.from, &active.to, eased).or_else(|| {
        active
            .allow_discrete
            .then(|| discrete_value(property, &active.from, &active.to, eased))
    })
}

fn discrete_value(property: &str, from: &str, to: &str, progress: f32) -> String {
    if property.eq_ignore_ascii_case("display") {
        // CSS Transitions 2: content must be visible for the whole transition.
        if from.eq_ignore_ascii_case("none") {
            return to.to_owned();
        }
        if to.eq_ignore_ascii_case("none") {
            return from.to_owned();
        }
    }
    if progress < 0.5 {
        from.to_owned()
    } else {
        to.to_owned()
    }
}

fn transition_event(
    node: NodeId,
    property: &str,
    active: &ActiveTransition,
    time: f32,
    kind: TransitionEventKind,
) -> TransitionEvent {
    TransitionEvent {
        kind,
        node,
        property: property.to_owned(),
        elapsed_time: (time - active.start - active.delay).clamp(0.0, active.duration),
    }
}

struct TransitionPlan {
    properties: PropertyList,
    durations: Vec<f32>,
    delays: Vec<f32>,
    timings: Vec<String>,
    behaviors: Vec<String>,
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
        let mut behaviors = split_top_level_commas(
            &style
                .get("transition-behavior")
                .map_or_else(|| "normal".to_owned(), |value| value.css_text().to_owned()),
        );
        if behaviors.is_empty() {
            behaviors.push("normal".to_owned());
        }
        Self {
            properties,
            durations,
            delays,
            timings,
            behaviors,
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
        list_value(&self.timings, self.index_for(property)).unwrap_or_else(|| "ease".to_owned())
    }

    fn allow_discrete(&self, property: &str) -> bool {
        list_value(&self.behaviors, self.index_for(property))
            .is_some_and(|value| value.eq_ignore_ascii_case("allow-discrete"))
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

fn list_value<T: Clone>(values: &[T], index: usize) -> Option<T> {
    if values.is_empty() {
        return None;
    }
    values.get(index.min(values.len() - 1)).cloned()
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
    use super::{TransitionEventKind, TransitionState, sample_transitions};
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
    #[test]
    fn reversing_an_interrupted_transition_shortens_its_duration() {
        let node = NodeId::from_u64(4);
        let base = |width: &str| {
            style_with(&[
                ("width", width),
                ("transition-property", "width"),
                ("transition-duration", "1s"),
                ("transition-timing-function", "linear"),
            ])
        };
        let mut styles = BTreeMap::new();
        styles.insert(node, base("0px"));
        let mut state = TransitionState::default();
        sample_transitions(&mut styles, &mut state, 0.0);
        styles.insert(node, base("100px"));
        sample_transitions(&mut styles, &mut state, 0.0);
        styles.insert(node, base("100px"));
        sample_transitions(&mut styles, &mut state, 0.5);
        assert_eq!(
            styles[&node].get("width").map(|value| value.css_text()),
            Some("50px")
        );

        // Reversing at 50% must take the shortened 0.5s back, not another 1s.
        styles.insert(node, base("0px"));
        sample_transitions(&mut styles, &mut state, 0.5);
        styles.insert(node, base("0px"));
        sample_transitions(&mut styles, &mut state, 0.75);
        assert_eq!(
            styles[&node].get("width").map(|value| value.css_text()),
            Some("25px")
        );
        styles.insert(node, base("0px"));
        let events = sample_transitions(&mut styles, &mut state, 1.0);
        assert_eq!(
            styles[&node].get("width").map(|value| value.css_text()),
            Some("0px")
        );
        assert_eq!(events.len(), 1);
        assert!((events[0].elapsed_time - 0.5).abs() < 1.0e-6);
    }
    #[test]
    fn an_interrupted_third_value_cancels_the_old_transition() {
        let node = NodeId::from_u64(5);
        let base = |width: &str| {
            style_with(&[
                ("width", width),
                ("transition-property", "width"),
                ("transition-duration", "1s"),
                ("transition-timing-function", "linear"),
            ])
        };
        let mut styles = BTreeMap::new();
        styles.insert(node, base("0px"));
        let mut state = TransitionState::default();
        sample_transitions(&mut styles, &mut state, 0.0);
        styles.insert(node, base("100px"));
        sample_transitions(&mut styles, &mut state, 0.0);
        styles.insert(node, base("100px"));
        sample_transitions(&mut styles, &mut state, 0.5);

        // A third value replaces the running transition and cancels it.
        styles.insert(node, base("75px"));
        let events = sample_transitions(&mut styles, &mut state, 0.5);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, TransitionEventKind::Cancel);
        assert_eq!(events[0].property, "width");
        assert!((events[0].elapsed_time - 0.5).abs() < 1.0e-6);
        assert_eq!(
            styles[&node].get("width").map(|value| value.css_text()),
            Some("50px")
        );
    }

    #[test]
    fn allow_discrete_steps_display_at_the_endpoints() {
        let node = NodeId::from_u64(6);
        let base = |display: &str| {
            style_with(&[
                ("display", display),
                ("transition-property", "display"),
                ("transition-duration", "1s"),
                ("transition-timing-function", "linear"),
                ("transition-behavior", "allow-discrete"),
            ])
        };
        let mut styles = BTreeMap::new();
        styles.insert(node, base("block"));
        let mut state = TransitionState::default();
        sample_transitions(&mut styles, &mut state, 0.0);
        styles.insert(node, base("none"));
        sample_transitions(&mut styles, &mut state, 0.0);
        styles.insert(node, base("none"));
        sample_transitions(&mut styles, &mut state, 0.5);
        assert_eq!(
            styles[&node].get("display").map(|value| value.css_text()),
            Some("block")
        );
        styles.insert(node, base("none"));
        let events = sample_transitions(&mut styles, &mut state, 1.0);
        assert_eq!(
            styles[&node].get("display").map(|value| value.css_text()),
            Some("none")
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, TransitionEventKind::End);
    }

    #[test]
    fn without_allow_discrete_a_discrete_change_is_immediate() {
        let node = NodeId::from_u64(7);
        let base = |display: &str| {
            style_with(&[
                ("display", display),
                ("transition-property", "display"),
                ("transition-duration", "1s"),
            ])
        };
        let mut styles = BTreeMap::new();
        styles.insert(node, base("block"));
        let mut state = TransitionState::default();
        sample_transitions(&mut styles, &mut state, 0.0);
        styles.insert(node, base("none"));
        let events = sample_transitions(&mut styles, &mut state, 0.5);
        assert!(events.is_empty());
        assert!(!state.has_active());
        assert_eq!(
            styles[&node].get("display").map(|value| value.css_text()),
            Some("none")
        );
    }
    #[test]
    fn repeated_reversals_use_the_adjusted_start_value() {
        let node = NodeId::from_u64(8);
        let base = |width: &str| {
            style_with(&[
                ("width", width),
                ("transition-property", "width"),
                ("transition-duration", "1s"),
                ("transition-timing-function", "linear"),
            ])
        };
        let mut styles = BTreeMap::new();
        styles.insert(node, base("0px"));
        let mut state = TransitionState::default();
        sample_transitions(&mut styles, &mut state, 0.0);
        styles.insert(node, base("100px"));
        sample_transitions(&mut styles, &mut state, 0.0);
        styles.insert(node, base("100px"));
        sample_transitions(&mut styles, &mut state, 0.5);
        styles.insert(node, base("0px"));
        let first_cancel = sample_transitions(&mut styles, &mut state, 0.5);
        assert_eq!(first_cancel[0].kind, TransitionEventKind::Cancel);
        styles.insert(node, base("0px"));
        sample_transitions(&mut styles, &mut state, 0.75);
        styles.insert(node, base("100px"));
        let second_cancel = sample_transitions(&mut styles, &mut state, 0.75);
        assert_eq!(second_cancel[0].kind, TransitionEventKind::Cancel);
        styles.insert(node, base("100px"));
        let events = sample_transitions(&mut styles, &mut state, 1.5);
        assert_eq!(
            styles[&node].get("width").map(|value| value.css_text()),
            Some("100px")
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, TransitionEventKind::End);
        assert!((events[0].elapsed_time - 0.75).abs() < 1.0e-6);
    }

    #[test]
    fn removing_a_node_cancels_its_running_transition() {
        let node = NodeId::from_u64(9);
        let mut styles = BTreeMap::new();
        styles.insert(
            node,
            style_with(&[
                ("opacity", "1"),
                ("transition-property", "opacity"),
                ("transition-duration", "1s"),
            ]),
        );
        let mut state = TransitionState::default();
        sample_transitions(&mut styles, &mut state, 0.0);
        styles.insert(
            node,
            style_with(&[
                ("opacity", "0"),
                ("transition-property", "opacity"),
                ("transition-duration", "1s"),
            ]),
        );
        sample_transitions(&mut styles, &mut state, 0.0);
        styles.clear();
        let events = sample_transitions(&mut styles, &mut state, 0.5);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, TransitionEventKind::Cancel);
        assert_eq!(events[0].node, node);
        assert_eq!(events[0].property, "opacity");
        assert!(!state.has_active());
    }

    #[test]
    fn a_document_switch_cancels_all_running_transitions() {
        let node = NodeId::from_u64(10);
        let mut styles = BTreeMap::new();
        styles.insert(
            node,
            style_with(&[
                ("width", "0px"),
                ("transition-property", "width"),
                ("transition-duration", "1s"),
            ]),
        );
        let mut state = TransitionState::default();
        sample_transitions(&mut styles, &mut state, 0.0);
        styles.insert(
            node,
            style_with(&[
                ("width", "100px"),
                ("transition-property", "width"),
                ("transition-duration", "1s"),
            ]),
        );
        sample_transitions(&mut styles, &mut state, 0.0);
        let events = state.cancel_all(0.25);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, TransitionEventKind::Cancel);
        assert!(!state.has_active());
    }
}
