//! CSS cascade winner selection.
//!
//! The result is a cascaded (not computed or used) value map. Inheritance,
//! CSS-wide keywords, custom-property substitution, and property grammars are
//! intentionally separate stages.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};

use render_dom::{Dom, NodeId};

use super::properties::{expand_flex_shorthand, expand_gap_shorthand, parse_typed_property};
use super::selector::{MatchContext, Specificity, matching_specificity};
use super::stylesheet::{CssWideKeyword, Declaration, LayerName, StyleSheet, css_wide_keyword};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CascadeOrigin {
    UserAgent,
    User,
    Author,
}

#[derive(Clone, Copy, Debug)]
pub struct CascadeInput<'a> {
    pub sheet: &'a StyleSheet,
    pub origin: CascadeOrigin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CascadedValue {
    pub value: String,
    pub important: bool,
    pub origin: CascadeOrigin,
    pub layer: Option<LayerName>,
    pub specificity: Specificity,
    pub source_order: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CascadedStyle {
    properties: BTreeMap<String, CascadedValue>,
}

impl CascadedStyle {
    #[must_use]
    pub fn get(&self, property: &str) -> Option<&CascadedValue> {
        self.properties.get(property)
    }

    #[must_use]
    pub const fn properties(&self) -> &BTreeMap<String, CascadedValue> {
        &self.properties
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum GlobalLayerKey {
    Named(Vec<String>),
    Anonymous { source: usize, id: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Priority {
    important: bool,
    origin: u8,
    layer: usize,
    specificity: Specificity,
    source_order: u64,
}

#[derive(Clone, Debug)]
struct Candidate {
    priority: Priority,
    value: CascadedValue,
    layer_key: Option<GlobalLayerKey>,
}

/// Select cascaded declaration winners for one element.
#[must_use]
pub fn cascade_element(
    dom: &Dom,
    element: NodeId,
    sources: &[CascadeInput<'_>],
    context: &MatchContext,
) -> CascadedStyle {
    cascade_element_with_inline(dom, element, sources, context, &[])
}

/// Select cascaded declaration winners including an inline declaration list.
#[must_use]
pub fn cascade_element_with_inline(
    dom: &Dom,
    element: NodeId,
    sources: &[CascadeInput<'_>],
    context: &MatchContext,
    inline_declarations: &[Declaration],
) -> CascadedStyle {
    let layer_orders = collect_layer_orders(sources);
    let mut candidates: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
    let mut source_order = 0_u64;

    for (source_index, source) in sources.iter().enumerate() {
        for rule in &source.sheet.rules {
            if !rule
                .media
                .iter()
                .all(|query| media_query_list_matches(query, context))
            {
                continue;
            }
            let specificity = matching_specificity(dom, element, &rule.selectors, context);
            for declaration in &rule.declarations {
                source_order = source_order.saturating_add(1);
                let Some(specificity) = specificity else {
                    continue;
                };
                let priority = Priority {
                    important: declaration.important,
                    origin: origin_rank(source.origin, declaration.important),
                    layer: layer_rank(
                        &layer_orders,
                        source.origin,
                        source_index,
                        rule.layer.as_ref(),
                        declaration.important,
                    ),
                    specificity,
                    source_order,
                };
                for (name, specified_value) in
                    expanded_declaration(&declaration.name, &declaration.value)
                {
                    let value = CascadedValue {
                        value: specified_value,
                        important: declaration.important,
                        origin: source.origin,
                        layer: rule.layer.clone(),
                        specificity,
                        source_order,
                    };
                    candidates.entry(name).or_default().push(Candidate {
                        priority,
                        value,
                        layer_key: rule
                            .layer
                            .as_ref()
                            .map(|layer| global_layer_key(layer, source_index)),
                    });
                }
            }
        }
    }

    let inline_specificity = Specificity {
        ids: u32::MAX,
        classes: u32::MAX,
        types: u32::MAX,
    };
    for declaration in inline_declarations {
        source_order = source_order.saturating_add(1);
        let priority = Priority {
            important: declaration.important,
            origin: origin_rank(CascadeOrigin::Author, declaration.important),
            layer: layer_rank(
                &layer_orders,
                CascadeOrigin::Author,
                sources.len(),
                None,
                declaration.important,
            ),
            specificity: inline_specificity,
            source_order,
        };
        for (name, specified_value) in expanded_declaration(&declaration.name, &declaration.value) {
            candidates.entry(name).or_default().push(Candidate {
                priority,
                value: CascadedValue {
                    value: specified_value,
                    important: declaration.important,
                    origin: CascadeOrigin::Author,
                    layer: None,
                    specificity: inline_specificity,
                    source_order,
                },
                layer_key: None,
            });
        }
    }

    CascadedStyle {
        properties: candidates
            .into_iter()
            .filter_map(|(property, candidates)| {
                select_cascaded_candidate(candidates).map(|value| (property, value))
            })
            .collect(),
    }
}

pub fn media_query_list_matches(query: &str, context: &MatchContext) -> bool {
    split_media_list(query)
        .into_iter()
        .any(|query| media_query_matches(query, context))
}

fn split_media_list(query: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut depth = 0_u32;
    let mut start = 0;
    for (index, character) in query.char_indices() {
        match character {
            '(' => depth = depth.saturating_add(1),
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                result.push(query[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    result.push(query[start..].trim());
    result
}

fn media_query_matches(query: &str, context: &MatchContext) -> bool {
    let query = query.trim().to_ascii_lowercase();
    let (negated, query) = query
        .strip_prefix("not ")
        .map_or((false, query.as_str()), |query| (true, query));
    let query = query.strip_prefix("only ").unwrap_or(query);
    let matches = split_media_conjunctions(query)
        .into_iter()
        .all(|condition| media_condition_matches(condition, context));
    if negated { !matches } else { matches }
}

/// Split a media query on its `and` conjunctions. Real-world stylesheets
/// routinely write `(min-width:1560px)and (max-width:2059.9px)` with no
/// surrounding whitespace, so the identifier must be matched with paren
/// depth instead of a literal `" and "` split.
fn split_media_conjunctions(query: &str) -> Vec<&str> {
    let lowered = query.to_ascii_lowercase();
    let bytes = lowered.as_bytes();
    let mut result = Vec::new();
    let mut depth = 0_u32;
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'(' => depth = depth.saturating_add(1),
            b')' => depth = depth.saturating_sub(1),
            b'a' | b'A' if depth == 0 && lowered[index..].starts_with("and") => {
                let before = bytes.get(index.wrapping_sub(1));
                let after = bytes.get(index + 3);
                // Minified sheets write `)and (` with no whitespace at all;
                // a preceding `)` or following `(` still reads as a
                // conjunction.
                let boundary_before = index == 0
                    || before.is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b')');
                let boundary_after =
                    after.is_none_or(|byte| byte.is_ascii_whitespace() || *byte == b'(');
                if boundary_before && boundary_after {
                    result.push(query[start..index].trim());
                    index += 3;
                    start = index;
                    continue;
                }
            }
            _ => {}
        }
        index += 1;
    }
    result.push(query[start..].trim());
    result
}

fn media_condition_matches(condition: &str, context: &MatchContext) -> bool {
    match condition {
        "" | "all" | "screen" => return true,
        "print" | "speech" => return false,
        _ => {}
    }
    let Some(feature) = condition
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
    else {
        return false;
    };
    let Some((name, value)) = feature.split_once(':') else {
        return false;
    };
    let name = name.trim();
    let value = value.trim();
    let dimension = match name {
        "width" | "min-width" | "max-width" => context.viewport_width,
        "height" | "min-height" | "max-height" => context.viewport_height,
        "orientation" => {
            let Some((width, height)) = context.viewport_width.zip(context.viewport_height) else {
                return false;
            };
            return match value {
                "landscape" => width >= height,
                "portrait" => height > width,
                _ => false,
            };
        }
        _ => return false,
    };
    let Some(actual) = dimension else {
        return false;
    };
    let Some(expected) = parse_media_length(value) else {
        return false;
    };
    match name {
        "min-width" | "min-height" => actual >= expected,
        "max-width" | "max-height" => actual <= expected,
        _ => (actual - expected).abs() < f32::EPSILON,
    }
}

fn parse_media_length(value: &str) -> Option<f32> {
    let value = value.trim();
    if value == "0" {
        return Some(0.0);
    }
    for (unit, factor) in [("px", 1.0), ("em", 16.0), ("rem", 16.0)] {
        if let Some(number) = value.strip_suffix(unit) {
            return number
                .trim()
                .parse::<f32>()
                .ok()
                .map(|value| value * factor);
        }
    }
    None
}

fn expanded_declaration(name: &str, value: &str) -> Vec<(String, String)> {
    if name.eq_ignore_ascii_case("background") {
        return expand_background_shorthand(value);
    }
    if matches!(name, "margin" | "padding") {
        return expand_box_shorthand(name, value);
    }
    match name {
        "border" => expand_border_shorthand(value),
        "border-top" | "border-right" | "border-bottom" | "border-left" => {
            expand_border_side_shorthand(name, value)
        }
        "border-width" | "border-style" | "border-color" => {
            expand_border_part_shorthand(name, value)
        }
        "font" => expand_font_shorthand(value)
            .unwrap_or_else(|| vec![(name.to_owned(), value.to_owned())]),
        // `grid-gap` and its longhands are the legacy spellings real sheets
        // still ship; they expand exactly like their modern counterparts.
        _ => expand_legacy_longhands(name, value),
    }
}

fn expand_legacy_longhands(name: &str, value: &str) -> Vec<(String, String)> {
    let gap_alias = name == "gap" || name == "grid-gap";
    let gap_longhand = match name {
        "grid-row-gap" => Some("row-gap"),
        "grid-column-gap" => Some("column-gap"),
        _ => None,
    };
    if let Some(longhand) = gap_longhand {
        return vec![(longhand.to_owned(), value.to_owned())];
    }
    if !gap_alias && name != "flex" && name != "overflow" {
        return vec![(name.to_owned(), value.to_owned())];
    }
    if css_wide_keyword(value).is_some() {
        let longhands: &[&str] = if gap_alias {
            &["row-gap", "column-gap"]
        } else if name == "flex" {
            &["flex-grow", "flex-shrink", "flex-basis"]
        } else {
            &["overflow-x", "overflow-y"]
        };
        return longhands
            .iter()
            .map(|longhand| ((*longhand).to_owned(), value.to_owned()))
            .collect();
    }
    match name {
        "gap" | "grid-gap" => expand_gap_shorthand(value).map_or_else(
            || vec![(name.to_owned(), value.to_owned())],
            |(row, column)| {
                vec![
                    ("row-gap".to_owned(), row),
                    ("column-gap".to_owned(), column),
                ]
            },
        ),
        "flex" => expand_flex_shorthand(value).map_or_else(
            || vec![(name.to_owned(), value.to_owned())],
            |(grow, shrink, basis)| {
                vec![
                    ("flex-grow".to_owned(), grow),
                    ("flex-shrink".to_owned(), shrink),
                    ("flex-basis".to_owned(), basis),
                ]
            },
        ),
        "overflow" => {
            let values: Vec<_> = value.split_ascii_whitespace().collect();
            if values.len() == 1 || values.len() == 2 {
                let y = values[0];
                let x = values.get(1).copied().unwrap_or(y);
                vec![
                    ("overflow-x".to_owned(), x.to_owned()),
                    ("overflow-y".to_owned(), y.to_owned()),
                ]
            } else {
                vec![(name.to_owned(), value.to_owned())]
            }
        }
        _ => unreachable!(),
    }
}

fn expand_background_shorthand(value: &str) -> Vec<(String, String)> {
    let lower = value.to_ascii_lowercase();
    let image = extract_css_url(value).map_or_else(
        || extract_css_gradients(value).unwrap_or_else(|| "none".to_owned()),
        |url| format!("url({url})"),
    );
    let color = split_css_components(value)
        .into_iter()
        .find(|component| {
            parse_typed_property("background-color", component).is_some_and(|result| result.is_ok())
        })
        .unwrap_or("transparent")
        .to_owned();
    let repeat = ["no-repeat", "repeat-x", "repeat-y", "repeat"]
        .into_iter()
        .find(|keyword| lower.split_ascii_whitespace().any(|part| part == *keyword))
        .unwrap_or("repeat")
        .to_owned();
    let size = value
        .split_once('/')
        .map(|(_, tail)| tail.split_ascii_whitespace().next().unwrap_or("auto"))
        .filter(|part| {
            matches!(
                part.to_ascii_lowercase().as_str(),
                "cover" | "contain" | "auto"
            )
        })
        .unwrap_or("auto")
        .to_owned();
    let positions: Vec<_> = split_css_components(value)
        .into_iter()
        .filter(|component| {
            component.ends_with('%')
                || matches!(
                    component.to_ascii_lowercase().as_str(),
                    "left" | "center" | "right" | "top" | "bottom"
                )
        })
        .collect();
    let position = match positions.as_slice() {
        [horizontal, vertical] => format!("{horizontal} {vertical}"),
        [single] if matches!(single.to_ascii_lowercase().as_str(), "top" | "bottom") => {
            format!("center {single}")
        }
        [single] if single.eq_ignore_ascii_case("center") => "center center".to_owned(),
        [single] => format!("{single} 50%"),
        _ => "0% 0%".to_owned(),
    };
    vec![
        ("background-color".to_owned(), color),
        ("background-image".to_owned(), image),
        ("background-repeat".to_owned(), repeat),
        ("background-position".to_owned(), position),
        ("background-size".to_owned(), size),
    ]
}

fn split_css_components(value: &str) -> Vec<&str> {
    let mut components = Vec::new();
    let mut start = None;
    let mut depth = 0_u32;
    let mut quote = None;
    for (index, character) in value.char_indices() {
        match (quote, character) {
            (Some(expected), character) if character == expected => quote = None,
            (None, '\'' | '"') => quote = Some(character),
            (None, '(') => depth = depth.saturating_add(1),
            (None, ')') => depth = depth.saturating_sub(1),
            (None, character) if character.is_ascii_whitespace() && depth == 0 => {
                if let Some(start) = start.take() {
                    components.push(&value[start..index]);
                }
            }
            (None, _) if start.is_none() => start = Some(index),
            _ => {}
        }
    }
    if let Some(start) = start {
        components.push(&value[start..]);
    }
    components
}

fn extract_css_url(value: &str) -> Option<&str> {
    let start = value.to_ascii_lowercase().find("url(")?.saturating_add(4);
    let tail = &value[start..];
    let end = tail.find(')')?;
    Some(tail[..end].trim().trim_matches(['\'', '"']))
}

fn extract_css_gradients(value: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    let mut gradients = Vec::new();
    let mut search_from = 0;
    while let Some(relative_start) = lower[search_from..].find("linear-gradient(") {
        let start = search_from + relative_start;
        let open = start + "linear-gradient".len();
        let mut depth = 0_u32;
        let mut end = None;
        for (offset, character) in value[open..].char_indices() {
            match character {
                '(' => depth = depth.saturating_add(1),
                ')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        end = Some(open + offset + character.len_utf8());
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end?;
        gradients.push(value[start..end].to_owned());
        search_from = end;
    }
    (!gradients.is_empty()).then(|| gradients.join(","))
}

fn expand_box_shorthand(name: &str, value: &str) -> Vec<(String, String)> {
    // Component-aware splitting keeps math functions such as
    // `margin: calc(50% - 10px) auto` intact.
    let values: Vec<&str> = split_css_components(value);
    if values.is_empty() || values.len() > 4 {
        return vec![(name.to_owned(), value.to_owned())];
    }
    let edges = match values.len() {
        1 => [values[0], values[0], values[0], values[0]],
        2 => [values[0], values[1], values[0], values[1]],
        3 => [values[0], values[1], values[2], values[1]],
        _ => [values[0], values[1], values[2], values[3]],
    };
    ["top", "right", "bottom", "left"]
        .into_iter()
        .zip(edges)
        .map(|(edge, value)| (format!("{name}-{edge}"), value.to_owned()))
        .collect()
}

/// One component of a `border*` shorthand classified per CSS 2.1 §8.5:
/// the three value slots are order-independent and at most one of each kind
/// may appear.
enum BorderComponent {
    Width(String),
    Style(String),
    Color(String),
}

fn classify_border_component(token: &str) -> BorderComponent {
    let lowered = token.to_ascii_lowercase();
    match lowered.as_str() {
        "thin" | "medium" | "thick" => BorderComponent::Width(lowered),
        "none" | "hidden" | "dotted" | "dashed" | "solid" | "double" | "groove" | "ridge"
        | "inset" | "outset" => BorderComponent::Style(lowered),
        _ => {
            if is_border_width_token(&lowered) {
                BorderComponent::Width(token.to_owned())
            } else {
                BorderComponent::Color(token.to_owned())
            }
        }
    }
}

/// `<line-width>` accepts any `<length [0,∞]>`; a bare `0` (the extremely
/// common `border: 0` reset) and dimensioned lengths both classify as widths.
/// Colors never begin with a number or sign, so a numeric-leading token can
/// only be a width.
fn is_border_width_token(token: &str) -> bool {
    token
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit() || matches!(character, '.' | '+' | '-'))
}

/// Apply classified border components to `result`, keyed by longhand suffix.
fn push_border_longhands(
    result: &mut Vec<(String, String)>,
    prefix: &str,
    components: &[BorderComponent],
) {
    // Per CSS 2.1 §8.5.4 a border shorthand resets every longhand it does
    // not explicitly specify to its initial value.
    let mut width = "medium".to_owned();
    let mut style = "none".to_owned();
    let mut color = "currentcolor".to_owned();
    for component in components {
        match component {
            BorderComponent::Width(value) => width.clone_from(value),
            BorderComponent::Style(value) => style.clone_from(value),
            BorderComponent::Color(value) => color.clone_from(value),
        }
    }
    for (suffix, value) in [("width", width), ("style", style), ("color", color)] {
        result.push((format!("{prefix}-{suffix}"), value));
    }
}

fn expand_border_shorthand(value: &str) -> Vec<(String, String)> {
    let components: Vec<_> = split_css_components(value)
        .into_iter()
        .map(classify_border_component)
        .collect();
    if components.is_empty() {
        return vec![("border".to_owned(), value.to_owned())];
    }
    let mut result = Vec::new();
    for edge in ["top", "right", "bottom", "left"] {
        push_border_longhands(&mut result, &format!("border-{edge}"), &components);
    }
    result
}

fn expand_border_side_shorthand(name: &str, value: &str) -> Vec<(String, String)> {
    let components: Vec<_> = split_css_components(value)
        .into_iter()
        .map(classify_border_component)
        .collect();
    if components.is_empty() {
        return vec![(name.to_owned(), value.to_owned())];
    }
    let mut result = Vec::new();
    push_border_longhands(&mut result, name, &components);
    result
}

fn expand_border_part_shorthand(name: &str, value: &str) -> Vec<(String, String)> {
    let Some(suffix) = name.strip_prefix("border-") else {
        return vec![(name.to_owned(), value.to_owned())];
    };
    let values: Vec<&str> = split_css_components(value);
    if values.is_empty() || values.len() > 4 {
        return vec![(name.to_owned(), value.to_owned())];
    }
    let edges = match values.len() {
        1 => [values[0], values[0], values[0], values[0]],
        2 => [values[0], values[1], values[0], values[1]],
        3 => [values[0], values[1], values[2], values[1]],
        _ => [values[0], values[1], values[2], values[3]],
    };
    ["top", "right", "bottom", "left"]
        .into_iter()
        .zip(edges)
        .map(|(edge, value)| (format!("border-{edge}-{suffix}"), value.to_owned()))
        .collect()
}

/// The `font` shorthand per CSS Fonts: optional style/variant/weight keywords
/// in any order, a mandatory size (with optional `/line-height`), and a
/// mandatory font family.
fn expand_font_shorthand(value: &str) -> Option<Vec<(String, String)>> {
    const STYLE_KEYWORDS: [&str; 3] = ["italic", "oblique", "normal"];
    const VARIANT_KEYWORDS: [&str; 2] = ["normal", "small-caps"];
    const WEIGHT_KEYWORDS: [&str; 4] = ["normal", "bold", "bolder", "lighter"];
    let components = split_css_components(value);
    if components.is_empty() {
        return None;
    }
    let mut style = "normal".to_owned();
    let mut variant = "normal".to_owned();
    let mut weight = "normal".to_owned();
    let mut index = 0;
    while let Some(component) = components.get(index) {
        let lowered = component.to_ascii_lowercase();
        // `normal` is valid in every keyword slot; consume at most one of each.
        let consumed = match lowered.as_str() {
            keyword if STYLE_KEYWORDS.contains(&keyword) => {
                if style != "normal" {
                    break;
                }
                style = lowered;
                true
            }
            keyword if VARIANT_KEYWORDS.contains(&keyword) => {
                if variant != "normal" {
                    break;
                }
                variant = lowered;
                true
            }
            keyword if WEIGHT_KEYWORDS.contains(&keyword) || lowered.parse::<u32>().is_ok() => {
                if weight != "normal" {
                    break;
                }
                weight = lowered;
                true
            }
            _ => false,
        };
        if !consumed {
            break;
        }
        index += 1;
    }
    let size_component = (*components.get(index)?).trim();
    let (size, line_height) = size_component
        .split_once('/')
        .map_or((size_component, "normal"), |(size, height)| {
            (size, height.trim())
        });
    let family = components[index + 1..].join(" ");
    if size.is_empty() || family.is_empty() {
        return None;
    }
    Some(vec![
        ("font-style".to_owned(), style),
        ("font-variant".to_owned(), variant),
        ("font-weight".to_owned(), weight),
        ("font-size".to_owned(), size.to_owned()),
        ("line-height".to_owned(), line_height.to_owned()),
        ("font-family".to_owned(), family),
    ])
}

fn select_cascaded_candidate(mut candidates: Vec<Candidate>) -> Option<CascadedValue> {
    candidates.sort_by_key(|candidate| Reverse(candidate.priority));
    let mut reverted_origins = HashSet::new();
    let mut reverted_layers = HashSet::new();

    for candidate in candidates {
        if reverted_origins.contains(&candidate.value.origin)
            || reverted_layers.contains(&(
                candidate.value.origin,
                candidate.value.important,
                candidate.layer_key.clone(),
            ))
        {
            continue;
        }
        match css_wide_keyword(&candidate.value.value) {
            Some(CssWideKeyword::Revert) => {
                reverted_origins.insert(candidate.value.origin);
            }
            Some(CssWideKeyword::RevertLayer) => {
                // The unlayered normal bucket is also a cascade-layer step:
                // rolling it back exposes the last explicit layer in this
                // origin rather than behaving like `revert`.
                reverted_layers.insert((
                    candidate.value.origin,
                    candidate.value.important,
                    candidate.layer_key,
                ));
            }
            _ => return Some(candidate.value),
        }
    }
    None
}

fn collect_layer_orders(
    sources: &[CascadeInput<'_>],
) -> HashMap<CascadeOrigin, Vec<GlobalLayerKey>> {
    let mut orders: HashMap<CascadeOrigin, Vec<GlobalLayerKey>> = HashMap::new();
    for (source_index, source) in sources.iter().enumerate() {
        let order = orders.entry(source.origin).or_default();
        for layer in &source.sheet.layer_order {
            let key = global_layer_key(layer, source_index);
            if !order.contains(&key) {
                order.push(key);
            }
        }
    }
    orders
}

fn global_layer_key(layer: &LayerName, source_index: usize) -> GlobalLayerKey {
    match layer {
        LayerName::Named(name) => GlobalLayerKey::Named(name.clone()),
        LayerName::Anonymous(id) => GlobalLayerKey::Anonymous {
            source: source_index,
            id: *id,
        },
    }
}

fn layer_rank(
    orders: &HashMap<CascadeOrigin, Vec<GlobalLayerKey>>,
    origin: CascadeOrigin,
    source_index: usize,
    layer: Option<&LayerName>,
    important: bool,
) -> usize {
    let order = orders.get(&origin).map_or(&[][..], Vec::as_slice);
    let Some(layer) = layer else {
        return if important { 0 } else { order.len() };
    };
    let key = global_layer_key(layer, source_index);
    let index = order
        .iter()
        .position(|candidate| *candidate == key)
        .unwrap_or(order.len());
    if important {
        order.len().saturating_sub(index)
    } else {
        index
    }
}

const fn origin_rank(origin: CascadeOrigin, important: bool) -> u8 {
    match origin {
        CascadeOrigin::User => 1,
        CascadeOrigin::UserAgent => {
            if important {
                2
            } else {
                0
            }
        }
        CascadeOrigin::Author => {
            if important {
                0
            } else {
                2
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CascadeInput, CascadeOrigin, cascade_element};
    use crate::selector::{MatchContext, parse_selector_list, select_all};
    use crate::stylesheet::parse_stylesheet;
    use render_html::parse_document;

    fn document_and_target() -> (render_dom::Dom, render_dom::NodeId) {
        let output = parse_document("<!doctype html><div id='target' class='target'></div>");
        let selectors = parse_selector_list("#target").expect("valid test selector");
        let target = select_all(
            &output.dom,
            output.dom.document(),
            &selectors,
            &MatchContext::default(),
        )[0];
        (output.dom, target)
    }

    #[test]
    fn uses_specificity_of_the_selector_that_matched() {
        let (dom, target) = document_and_target();
        let sheet = parse_stylesheet("#missing, div { color: red } .target { color: blue }");
        let style = cascade_element(
            &dom,
            target,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );

        assert_eq!(
            style.get("color").map(|value| value.value.as_str()),
            Some("blue")
        );
    }

    #[test]
    fn important_reverses_origin_precedence() {
        let (dom, target) = document_and_target();
        let author = parse_stylesheet("#target { color: red !important }");
        let user = parse_stylesheet("div { color: blue !important }");
        let user_agent = parse_stylesheet("div { color: green !important }");
        let style = cascade_element(
            &dom,
            target,
            &[
                CascadeInput {
                    sheet: &user_agent,
                    origin: CascadeOrigin::UserAgent,
                },
                CascadeInput {
                    sheet: &user,
                    origin: CascadeOrigin::User,
                },
                CascadeInput {
                    sheet: &author,
                    origin: CascadeOrigin::Author,
                },
            ],
            &MatchContext::default(),
        );

        assert_eq!(
            style.get("color").map(|value| value.value.as_str()),
            Some("green")
        );
    }

    #[test]
    fn layers_follow_normal_and_reversed_important_order() {
        let (dom, target) = document_and_target();
        let normal = parse_stylesheet(
            "@layer reset, theme; \
             @layer theme { #target { color: blue } } \
             @layer reset { #target { color: red } } \
             #target { color: green }",
        );
        let important = parse_stylesheet(
            "@layer reset, theme; \
             @layer theme { #target { color: blue !important } } \
             @layer reset { #target { color: red !important } } \
             #target { color: green !important }",
        );

        let normal_style = cascade_element(
            &dom,
            target,
            &[CascadeInput {
                sheet: &normal,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );
        let important_style = cascade_element(
            &dom,
            target,
            &[CascadeInput {
                sheet: &important,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );

        assert_eq!(
            normal_style.get("color").map(|value| value.value.as_str()),
            Some("green")
        );
        assert_eq!(
            important_style
                .get("color")
                .map(|value| value.value.as_str()),
            Some("red")
        );
    }

    #[test]
    fn minified_conjunction_media_queries_without_spaces_match() {
        let (dom, target) = document_and_target();
        let sheet = parse_stylesheet(
            "@media(min-width:1140px)and (max-width:1299.9px){#target{display:grid}}",
        );
        let style = cascade_element(
            &dom,
            target,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext {
                viewport_width: Some(1180.0),
                viewport_height: Some(800.0),
                ..MatchContext::default()
            },
        );

        assert_eq!(
            style.get("display").map(|value| value.value.as_str()),
            Some("grid")
        );
    }

    #[test]
    fn media_rules_match_the_actual_layout_viewport() {
        let (dom, target) = document_and_target();
        let sheet = parse_stylesheet(
            "#target { color:black } \
             @media screen and (min-width: 700px) { #target { color:green } } \
             @media screen and (max-width: 699px) { #target { color:red } } \
             @media print { #target { color:blue } }",
        );
        let style = cascade_element(
            &dom,
            target,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext {
                viewport_width: Some(800.0),
                viewport_height: Some(600.0),
                ..MatchContext::default()
            },
        );
        assert_eq!(
            style.get("color").map(|value| value.value.as_str()),
            Some("green")
        );
    }

    #[test]
    fn custom_property_names_remain_case_sensitive() {
        let (dom, target) = document_and_target();
        let sheet = parse_stylesheet("#target { --Theme: red; --theme: blue; --Theme: green }");
        let style = cascade_element(
            &dom,
            target,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );

        assert_eq!(
            style.get("--Theme").map(|value| value.value.as_str()),
            Some("green")
        );
        assert_eq!(
            style.get("--theme").map(|value| value.value.as_str()),
            Some("blue")
        );
    }

    #[test]
    fn revert_rolls_back_the_current_origin() {
        let (dom, target) = document_and_target();
        let user_agent = parse_stylesheet("#target { color: black }");
        let user = parse_stylesheet("#target { color: blue }");
        let author = parse_stylesheet("#target { color: revert }");
        let style = cascade_element(
            &dom,
            target,
            &[
                CascadeInput {
                    sheet: &user_agent,
                    origin: CascadeOrigin::UserAgent,
                },
                CascadeInput {
                    sheet: &user,
                    origin: CascadeOrigin::User,
                },
                CascadeInput {
                    sheet: &author,
                    origin: CascadeOrigin::Author,
                },
            ],
            &MatchContext::default(),
        );

        assert_eq!(
            style.get("color").map(|value| value.value.as_str()),
            Some("blue")
        );
    }

    #[test]
    fn revert_layer_discards_every_declaration_in_the_winning_layer() {
        let (dom, target) = document_and_target();
        let sheet = parse_stylesheet(
            "@layer reset, theme; \
             @layer reset { #target { color: red } } \
             @layer theme { #target { color: blue } #target { color: revert-layer } }",
        );
        let style = cascade_element(
            &dom,
            target,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );

        assert_eq!(
            style.get("color").map(|value| value.value.as_str()),
            Some("red")
        );
    }

    #[test]
    fn unlayered_revert_layer_exposes_the_last_explicit_layer() {
        let (dom, target) = document_and_target();
        let sheet = parse_stylesheet(
            "@layer base { #target { color: red } } \
             #target { color: blue; color: revert-layer }",
        );
        let style = cascade_element(
            &dom,
            target,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );

        assert_eq!(
            style.get("color").map(|value| value.value.as_str()),
            Some("red")
        );
    }

    #[test]
    fn flex_and_gap_shorthands_participate_in_longhand_cascade_order() {
        let (dom, target) = document_and_target();
        let sheet = parse_stylesheet(
            "#target { flex-grow: 9; flex: 2 3 40px; row-gap: 1px; gap: 10px 20px; column-gap: 30px }",
        );
        let style = cascade_element(
            &dom,
            target,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );

        assert_eq!(
            style.get("flex-grow").map(|value| value.value.as_str()),
            Some("2")
        );
        assert_eq!(
            style.get("flex-shrink").map(|value| value.value.as_str()),
            Some("3")
        );
        assert_eq!(
            style.get("flex-basis").map(|value| value.value.as_str()),
            Some("40px")
        );
        assert_eq!(
            style.get("row-gap").map(|value| value.value.as_str()),
            Some("10px")
        );
        assert_eq!(
            style.get("column-gap").map(|value| value.value.as_str()),
            Some("30px")
        );
    }

    #[test]
    fn background_shorthand_resets_color_and_keeps_percentage_position() {
        let (dom, target) = document_and_target();
        let sheet =
            parse_stylesheet("#target { background: rgba(0,0,0,.6) url(icon.png) no-repeat 50% }");
        let style = cascade_element(
            &dom,
            target,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );

        assert_eq!(
            style
                .get("background-color")
                .map(|value| value.value.as_str()),
            Some("rgba(0,0,0,.6)")
        );
        assert_eq!(
            style
                .get("background-position")
                .map(|value| value.value.as_str()),
            Some("50% 50%")
        );
    }

    #[test]
    fn border_zero_shorthand_resets_every_border_longhand() {
        let (dom, target) = document_and_target();
        let sheet = parse_stylesheet(
            "#target { border-top-width: 4px; border-left-style: solid; border: 0 }",
        );
        let style = cascade_element(
            &dom,
            target,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );

        // CSS 2.1 §8.5.4: `border: 0` (the classic reset) must set the width,
        // not be misread as a color, and reset the unspecified longhands.
        assert_eq!(
            style
                .get("border-top-width")
                .map(|value| value.value.as_str()),
            Some("0")
        );
        assert_eq!(
            style
                .get("border-top-style")
                .map(|value| value.value.as_str()),
            Some("none")
        );
        assert_eq!(
            style
                .get("border-top-color")
                .map(|value| value.value.as_str()),
            Some("currentcolor")
        );
        assert_eq!(
            style
                .get("border-left-style")
                .map(|value| value.value.as_str()),
            Some("none")
        );
    }

    #[test]
    fn border_shorthand_classifies_functional_colors_with_spaces() {
        let (dom, target) = document_and_target();
        let sheet = parse_stylesheet("#target { border: 1px solid rgba(0, 0, 0, 0.5) }");
        let style = cascade_element(
            &dom,
            target,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );

        assert_eq!(
            style
                .get("border-top-width")
                .map(|value| value.value.as_str()),
            Some("1px")
        );
        assert_eq!(
            style
                .get("border-bottom-style")
                .map(|value| value.value.as_str()),
            Some("solid")
        );
        assert_eq!(
            style
                .get("border-left-color")
                .map(|value| value.value.as_str()),
            Some("rgba(0, 0, 0, 0.5)")
        );
    }

    #[test]
    fn border_side_shorthand_expands_only_that_side() {
        let (dom, target) = document_and_target();
        let sheet = parse_stylesheet("#target { border-bottom: 1px solid #eee }");
        let style = cascade_element(
            &dom,
            target,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );

        assert_eq!(
            style
                .get("border-bottom-width")
                .map(|value| value.value.as_str()),
            Some("1px")
        );
        assert_eq!(
            style
                .get("border-bottom-style")
                .map(|value| value.value.as_str()),
            Some("solid")
        );
        assert_eq!(
            style
                .get("border-bottom-color")
                .map(|value| value.value.as_str()),
            Some("#eee")
        );
        assert_eq!(style.get("border-top-width"), None);
        assert_eq!(style.get("border-top-color"), None);
    }

    #[test]
    fn border_part_shorthands_expand_per_edge() {
        let (dom, target) = document_and_target();
        let sheet = parse_stylesheet(
            "#target { border-width: 8px 6px; border-style: dashed dashed solid; border-color: transparent }",
        );
        let style = cascade_element(
            &dom,
            target,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );

        // The CSS triangle pattern: unequal widths, per-edge styles, and a
        // transparent base color that a later longhand overrides.
        assert_eq!(
            style
                .get("border-top-width")
                .map(|value| value.value.as_str()),
            Some("8px")
        );
        assert_eq!(
            style
                .get("border-right-width")
                .map(|value| value.value.as_str()),
            Some("6px")
        );
        assert_eq!(
            style
                .get("border-bottom-width")
                .map(|value| value.value.as_str()),
            Some("8px")
        );
        assert_eq!(
            style
                .get("border-left-width")
                .map(|value| value.value.as_str()),
            Some("6px")
        );
        assert_eq!(
            style
                .get("border-bottom-style")
                .map(|value| value.value.as_str()),
            Some("solid")
        );
        assert_eq!(
            style
                .get("border-top-style")
                .map(|value| value.value.as_str()),
            Some("dashed")
        );
        assert_eq!(
            style
                .get("border-top-color")
                .map(|value| value.value.as_str()),
            Some("transparent")
        );
    }

    #[test]
    fn later_border_color_longhand_overrides_the_part_shorthand() {
        let (dom, target) = document_and_target();
        let sheet =
            parse_stylesheet("#target { border-color: transparent; border-bottom-color: #f2f4f7 }");
        let style = cascade_element(
            &dom,
            target,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );

        assert_eq!(
            style
                .get("border-bottom-color")
                .map(|value| value.value.as_str()),
            Some("#f2f4f7")
        );
        assert_eq!(
            style
                .get("border-top-color")
                .map(|value| value.value.as_str()),
            Some("transparent")
        );
    }

    #[test]
    fn font_shorthand_expands_weight_size_line_height_and_family() {
        let (dom, target) = document_and_target();
        let sheet = parse_stylesheet("#target { font: bold 14px/20px Arial, sans-serif }");
        let style = cascade_element(
            &dom,
            target,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );

        assert_eq!(
            style.get("font-weight").map(|value| value.value.as_str()),
            Some("bold")
        );
        assert_eq!(
            style.get("font-size").map(|value| value.value.as_str()),
            Some("14px")
        );
        assert_eq!(
            style.get("line-height").map(|value| value.value.as_str()),
            Some("20px")
        );
        assert_eq!(
            style.get("font-family").map(|value| value.value.as_str()),
            Some("Arial, sans-serif")
        );
    }

    #[test]
    fn box_shorthand_keeps_calc_components_together() {
        let (dom, target) = document_and_target();
        let sheet = parse_stylesheet("#target { margin: calc(50% - 10px) auto }");
        let style = cascade_element(
            &dom,
            target,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );

        assert_eq!(
            style.get("margin-top").map(|value| value.value.as_str()),
            Some("calc(50% - 10px)")
        );
        assert_eq!(
            style.get("margin-right").map(|value| value.value.as_str()),
            Some("auto")
        );
        assert_eq!(
            style.get("margin-bottom").map(|value| value.value.as_str()),
            Some("calc(50% - 10px)")
        );
        assert_eq!(
            style.get("margin-left").map(|value| value.value.as_str()),
            Some("auto")
        );
    }
}
