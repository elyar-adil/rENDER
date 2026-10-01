//! CSS cascade winner selection.
//!
//! The result is a cascaded (not computed or used) value map. Inheritance,
//! CSS-wide keywords, custom-property substitution, and property grammars are
//! intentionally separate stages.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};

use render_dom::{Dom, NodeId};

use super::properties::{
    expand_flex_shorthand, expand_gap_shorthand, expand_grid_axis_shorthand, parse_typed_property,
};
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
    /// Whether the document itself supplied this declaration, as opposed to
    /// the user agent. See [`CascadedValue::is_authored`].
    pub authored: bool,
    pub layer: Option<LayerName>,
    pub specificity: Specificity,
    pub source_order: u64,
}

impl CascadedValue {
    /// Whether a declaration from the document produced this value, rather
    /// than the user agent's own styling.
    ///
    /// `origin` cannot answer this on its own, because HTML's presentational
    /// hints cascade at the user-agent origin (HTML5 rendering §15.3) yet come
    /// from markup the document's author wrote. A `<td valign=bottom>` is
    /// author intent that happens to arrive through the user-agent origin, and
    /// a consumer asking "did the document ask for this?" should hear yes.
    ///
    /// What is deliberately *not* distinguished here is who wrote it: an author
    /// stylesheet, a `style` attribute and a presentational hint all count, and
    /// a user-agent stylesheet rule does not. That is the distinction CSS
    /// Cascade 5 §6.1.1 draws between the UA origin and everything above it,
    /// and it is the one a "did the document say so" question needs.
    #[must_use]
    pub const fn is_authored(&self) -> bool {
        self.authored
    }
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
    cascade_element_with_origins(dom, element, sources, context, &[], &[])
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
    cascade_element_with_origins(dom, element, sources, context, &[], inline_declarations)
}

/// Select cascaded declaration winners including per-element declarations at
/// the user-agent origin (HTML presentational hints) and at the author
/// origin (the `style` attribute).
#[must_use]
pub fn cascade_element_with_origins(
    dom: &Dom,
    element: NodeId,
    sources: &[CascadeInput<'_>],
    context: &MatchContext,
    ua_declarations: &[Declaration],
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
                        // A rule from the user-agent stylesheet is the engine
                        // styling the document, not the document asking for
                        // something. Everything else came from a stylesheet the
                        // document brought with it.
                        authored: source.origin != CascadeOrigin::UserAgent,
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

    // HTML presentational hints sit at the user-agent origin with zero
    // specificity, so every author rule wins over them while they still
    // apply when no author rule targets the property (HTML5 rendering §15.3).
    let ua_specificity = Specificity {
        ids: 0,
        classes: 0,
        types: 0,
    };
    for declaration in ua_declarations {
        source_order = source_order.saturating_add(1);
        let priority = Priority {
            important: false,
            origin: origin_rank(CascadeOrigin::UserAgent, false),
            layer: layer_rank(
                &layer_orders,
                CascadeOrigin::UserAgent,
                sources.len(),
                None,
                false,
            ),
            specificity: ua_specificity,
            source_order,
        };
        for (name, specified_value) in expanded_declaration(&declaration.name, &declaration.value) {
            candidates.entry(name).or_default().push(Candidate {
                priority,
                value: CascadedValue {
                    value: specified_value,
                    important: false,
                    origin: CascadeOrigin::UserAgent,
                    // A presentational hint is an attribute the document's
                    // author wrote, so it counts as authored even though it
                    // cascades at the user-agent origin. Laying it on the UA
                    // origin is what makes every author rule beat it, which is
                    // a precedence decision and says nothing about who wanted
                    // the value.
                    authored: true,
                    layer: None,
                    specificity: ua_specificity,
                    source_order,
                },
                layer_key: None,
            });
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
                    // A `style` attribute is the document speaking directly.
                    authored: true,
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

/// Whether every construct in a media query list is one this engine's
/// evaluator can answer on its own.
///
/// This is deliberately *not* a second parse with weaker rules: it is derived
/// from the same [`media_condition_matches`] the cascade uses, so the two
/// cannot disagree. A query reporting `true` here is one whose result came
/// from a real evaluation rather than from the "unknown means false" fallback
/// of Media Queries 4 §2.1.1.
///
/// A consumer that reports a media query as unsupported must use this, or the
/// engine ends up claiming to lack support for something it evaluates on every
/// element it styles —which is how a supported feature gets reported as a gap
/// and sends someone hunting a bug that is not there.
///
/// Note the difference from [`media_query_list_matches`]: a feature the engine
/// knows but cannot answer *right now* (a viewport-dependent `orientation` with
/// no viewport yet) is still supported syntax, so it is not reported here. An
/// unsupported *feature* is.
#[must_use]
pub fn media_query_list_is_supported(query: &str) -> bool {
    split_media_list(query)
        .into_iter()
        .all(media_query_is_supported)
}

fn media_query_is_supported(query: &str) -> bool {
    let query = strip_media_modifiers(query);
    split_media_conjunctions(&query)
        .into_iter()
        .all(|condition| {
            media_condition_evaluate(condition, &MatchContext::default()).is_supported()
        })
}

/// The `not`/`only` prefix stripped, shared by evaluation and the support
/// check so the two cannot drift.
fn strip_media_modifiers(query: &str) -> String {
    let query = query.trim().to_ascii_lowercase();
    let query = query.strip_prefix("not ").unwrap_or(&query);
    query.strip_prefix("only ").unwrap_or(query).to_owned()
}

/// The outcome of evaluating one media condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MediaCondition {
    /// The engine reached a real comparison and knows the answer.
    Evaluated(bool),
    /// The condition names a feature or a unit this engine does not implement.
    /// Media Queries 4 §2.1.1 makes such a query false, so the cascade applies
    /// the rule as not matching.
    Unsupported,
    /// A feature the engine implements, whose value the environment does not
    /// provide yet: a viewport-dependent feature queried before the viewport
    /// is known. That is not a capability gap, so it is not reported as one.
    Unknown,
}

impl MediaCondition {
    /// Whether this engine can answer the condition by itself, as opposed to
    /// falling back to the "unknown means false" rule.
    const fn is_supported(self) -> bool {
        !matches!(self, Self::Unsupported)
    }

    const fn matches(self) -> bool {
        match self {
            Self::Evaluated(value) => value,
            Self::Unsupported | Self::Unknown => false,
        }
    }
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
        .all(|condition| media_condition_evaluate(condition, context).matches());
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

/// The one place a media condition is interpreted. Both the cascade's
/// [`media_query_list_matches`] and the [`media_query_list_is_supported`]
/// diagnostic read their answer from here, so a query cannot be evaluated one
/// way and described another.
fn media_condition_evaluate(condition: &str, context: &MatchContext) -> MediaCondition {
    match condition {
        "" | "all" | "screen" => return MediaCondition::Evaluated(true),
        // Known media types this engine does not produce. The engine answers
        // these correctly, so they are not a capability gap.
        "print" | "speech" => return MediaCondition::Evaluated(false),
        _ => {}
    }
    let Some(feature) = condition
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
    else {
        return MediaCondition::Unsupported;
    };
    // A range context such as `(400px <= width <= 700px)` has no colon, and
    // evaluating one would need the full Media Queries 4 §2.4 grammar.
    let Some((name, value)) = feature.split_once(':') else {
        return MediaCondition::Unsupported;
    };
    let name = name.trim();
    let value = value.trim();
    let is_horizontal = match name {
        "width" | "min-width" | "max-width" => true,
        "height" | "min-height" | "max-height" => false,
        "orientation" => {
            let Some((width, height)) = context.viewport_width.zip(context.viewport_height) else {
                return MediaCondition::Unknown;
            };
            return match value {
                "landscape" => MediaCondition::Evaluated(width >= height),
                "portrait" => MediaCondition::Evaluated(height > width),
                // A known feature asked a question this engine cannot answer.
                _ => MediaCondition::Unsupported,
            };
        }
        _ => return MediaCondition::Unsupported,
    };
    // The value's grammar is checked before the environment, so a length this
    // engine cannot express is reported as unsupported whether or not a
    // viewport happens to be available. Otherwise the support predicate would
    // answer "supported" for any length feature simply because no viewport had
    // been supplied yet.
    let Some(expected) = parse_media_length(value) else {
        return MediaCondition::Unsupported;
    };
    let actual = if is_horizontal {
        context.viewport_width
    } else {
        context.viewport_height
    };
    let Some(actual) = actual else {
        return MediaCondition::Unknown;
    };
    MediaCondition::Evaluated(match name {
        "min-width" | "min-height" => actual >= expected,
        "max-width" | "max-height" => actual <= expected,
        _ => (actual - expected).abs() < f32::EPSILON,
    })
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

/// The longhand declarations one authored declaration resolves to.
///
/// Shorthand expansion is what lets a page's `text-decoration: none` compete
/// with a user-agent stylesheet that sets `text-decoration-line` directly, so
/// the mapping is observable behaviour rather than an internal detail: a
/// consumer that wants to know what a declaration actually contributes to the
/// cascade has to be able to ask. Returns a single `(name, value)` pair
/// unchanged when the property is not a shorthand this engine expands, or when
/// its value cannot be read as one.
#[must_use]
pub fn expand_shorthand(name: &str, value: &str) -> Vec<(String, String)> {
    expanded_declaration(name, value)
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
        "text-decoration" => expand_text_decoration_shorthand(value),
        // `grid-gap` and its longhands are the legacy spellings real sheets
        // still ship; they expand exactly like their modern counterparts.
        _ => expand_legacy_longhands(name, value),
    }
}

fn expand_legacy_longhands(name: &str, value: &str) -> Vec<(String, String)> {
    let gap_alias = name == "gap" || name == "grid-gap";
    let grid_axis = match name {
        "grid-column" => Some(("grid-column-start", "grid-column-end")),
        "grid-row" => Some(("grid-row-start", "grid-row-end")),
        _ => None,
    };
    let gap_longhand = match name {
        "grid-row-gap" => Some("row-gap"),
        "grid-column-gap" => Some("column-gap"),
        _ => None,
    };
    if let Some(longhand) = gap_longhand {
        return vec![(longhand.to_owned(), value.to_owned())];
    }
    if !gap_alias
        && grid_axis.is_none()
        && name != "flex"
        && name != "flex-flow"
        && name != "overflow"
    {
        return vec![(name.to_owned(), value.to_owned())];
    }
    if css_wide_keyword(value).is_some() {
        let longhands: Vec<&str> = if gap_alias {
            vec!["row-gap", "column-gap"]
        } else if let Some((start, end)) = grid_axis {
            vec![start, end]
        } else if name == "flex" {
            vec!["flex-grow", "flex-shrink", "flex-basis"]
        } else if name == "flex-flow" {
            vec!["flex-direction", "flex-wrap"]
        } else {
            vec!["overflow-x", "overflow-y"]
        };
        return longhands
            .into_iter()
            .map(|longhand| (longhand.to_owned(), value.to_owned()))
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
        "grid-column" | "grid-row" => expand_grid_axis_shorthand(value).map_or_else(
            || vec![(name.to_owned(), value.to_owned())],
            |(start, end)| {
                let Some((start_name, end_name)) = grid_axis else {
                    unreachable!("grid axis longhands checked above")
                };
                vec![(start_name.to_owned(), start), (end_name.to_owned(), end)]
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
        "flex-flow" => {
            let lower = value.to_ascii_lowercase();
            let mut direction = None;
            let mut wrap = None;
            for part in lower.split_ascii_whitespace() {
                match part {
                    "row" | "row-reverse" | "column" | "column-reverse" if direction.is_none() => {
                        direction = Some(part)
                    }
                    "nowrap" | "wrap" | "wrap-reverse" if wrap.is_none() => wrap = Some(part),
                    _ => return vec![(name.to_owned(), value.to_owned())],
                }
            }
            if direction.is_none() && wrap.is_none() {
                return vec![(name.to_owned(), value.to_owned())];
            }
            vec![
                (
                    "flex-direction".to_owned(),
                    direction.unwrap_or("row").to_owned(),
                ),
                ("flex-wrap".to_owned(), wrap.unwrap_or("nowrap").to_owned()),
            ]
        }
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

/// The `text-decoration` shorthand. Three grammars are in play and the union
/// of them is accepted, because real sheets mix all three:
///
/// - Text Decoration 4 §2.6, the current one:
///   `<'text-decoration-line'> || <'text-decoration-thickness'> ||
///   <'text-decoration-style'> || <'text-decoration-color'>`
/// - Text Decoration 3 §2.4, the same without `thickness`.
/// - CSS 2.1 §16.3.1, which is line keywords only:
///   `none | [ underline || overline || line-through || blink ]`
///
/// §2.6 also settles a question the CSS 2.1 form leaves open: "Omitted values
/// are set to their initial values." So *all four* longhands are always
/// emitted, and a value that mentions only `underline` also resets style,
/// colour and thickness to their initials. That is not tidiness, it is the
/// whole point: the user-agent stylesheet puts `text-decoration-line:
/// underline` on `a:link`, and an author's `a { text-decoration: none }` only
/// beats it if the shorthand writes `text-decoration-line: none` explicitly
/// rather than leaving the UA's value standing.
///
/// `none` is a `text-decoration-line` keyword, not an unknown one, so it
/// expands to `line: none` plus the three initials. It is deliberately *not*
/// treated as a parse failure: the overwhelmingly common declaration on the
/// web is `text-decoration: none`, and a value that silently failed to expand
/// would leave the UA underline in place, which is the bug being fixed here.
fn expand_text_decoration_shorthand(value: &str) -> Vec<(String, String)> {
    const LINE_KEYWORDS: [&str; 6] = [
        "underline",
        "overline",
        "line-through",
        "blink",
        "spelling-error",
        "grammar-error",
    ];
    const STYLE_KEYWORDS: [&str; 5] = ["solid", "double", "dotted", "dashed", "wavy"];
    const THICKNESS_KEYWORDS: [&str; 5] = ["auto", "from-font", "thin", "medium", "thick"];

    let components = split_css_components(value);
    if components.is_empty() {
        return vec![("text-decoration".to_owned(), value.to_owned())];
    }
    let mut line: Option<String> = None;
    let mut style: Option<String> = None;
    let mut thickness: Option<String> = None;
    let mut color: Option<String> = None;

    for component in components {
        let lowered = component.to_ascii_lowercase();
        // `none` belongs to the line slot: `text-decoration-style` has no
        // `none` in any of the three grammars above.
        if lowered == "none" || LINE_KEYWORDS.contains(&lowered.as_str()) {
            // The `||` combinator allows each component at most once, and the
            // line keywords accumulate into a single value.
            match &mut line {
                None => {
                    line = Some(if lowered == "none" {
                        "none".to_owned()
                    } else {
                        lowered
                    })
                }
                Some(existing) if existing == "none" || lowered == "none" => {
                    return unexpanded(value);
                }
                Some(existing) => {
                    if split_css_components(existing)
                        .iter()
                        .any(|part| part.eq_ignore_ascii_case(&lowered))
                    {
                        return unexpanded(value);
                    }
                    existing.push(' ');
                    existing.push_str(&lowered);
                }
            }
            continue;
        }
        if STYLE_KEYWORDS.contains(&lowered.as_str()) {
            if style.replace(lowered).is_some() {
                return unexpanded(value);
            }
            continue;
        }
        if THICKNESS_KEYWORDS.contains(&lowered.as_str()) {
            if thickness.replace(lowered).is_some() {
                return unexpanded(value);
            }
            continue;
        }
        // A colour is the only remaining possibility. Test it against the real
        // grammar rather than guessing, so a typo falls through to the
        // unexpanded form instead of being written into the longhands as a
        // colour the engine would then fail to understand.
        if parse_typed_property("text-decoration-color", component)
            .is_some_and(|result| result.is_ok())
        {
            if color.replace(component.to_owned()).is_some() {
                return unexpanded(value);
            }
            continue;
        }
        // `<length-percentage>` and `<line-width>` for the thickness slot. A
        // colour never starts with a digit or a sign, so numeric-leading is
        // unambiguous. `calc()` and `var()` are not recognised here; they fall
        // through to the unexpanded form rather than being misfiled.
        if is_decoration_thickness_token(&lowered) {
            if thickness.replace(component.to_owned()).is_some() {
                return unexpanded(value);
            }
            continue;
        }
        return unexpanded(value);
    }

    vec![
        (
            "text-decoration-line".to_owned(),
            line.unwrap_or_else(|| "none".to_owned()),
        ),
        (
            "text-decoration-thickness".to_owned(),
            thickness.unwrap_or_else(|| "auto".to_owned()),
        ),
        (
            "text-decoration-style".to_owned(),
            style.unwrap_or_else(|| "solid".to_owned()),
        ),
        (
            "text-decoration-color".to_owned(),
            color.unwrap_or_else(|| "currentcolor".to_owned()),
        ),
    ]
}

/// Leave a declaration that cannot be read as a shorthand under its own name,
/// so a consumer that still understands the unexpanded form keeps working. A
/// `text-decoration` that reaches paint unexpanded reads as "no line
/// keywords", i.e. no decoration, which is what an invalid value becomes at
/// computed-value time anyway.
fn unexpanded(value: &str) -> Vec<(String, String)> {
    vec![("text-decoration".to_owned(), value.to_owned())]
}

/// A bare number or a dimension: `0`, `2px`, `.5em`, `50%`. Colours and all the
/// keyword slots are excluded by the caller before this is reached.
fn is_decoration_thickness_token(token: &str) -> bool {
    token
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit() || matches!(character, '.' | '+' | '-'))
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
    use super::{
        CascadeInput, CascadeOrigin, cascade_element, media_query_list_is_supported,
        media_query_list_matches,
    };
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

    /// Resolve the winning `color` for one element in a document.
    fn winning_color(html: &str, source: &str) -> Option<String> {
        let output = parse_document(html);
        let sheet = parse_stylesheet(source);
        let selectors = parse_selector_list("[data-probe]").expect("valid test selector");
        let probe = select_all(
            &output.dom,
            output.dom.document(),
            &selectors,
            &MatchContext::default(),
        )[0];
        let style = cascade_element(
            &output.dom,
            probe,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );
        style.get("color").map(|value| value.value.clone())
    }

    /// CSS Nesting §4's worked example: `&` takes the specificity of the
    /// *largest* selector in the parent list, not of the one that matched, so
    /// `#a, b { & c { ... } }` is (1,0,1) and beats `.foo c` at (0,1,1).
    #[test]
    fn nesting_selector_takes_the_largest_parent_specificity() {
        let color = winning_color(
            "<!doctype html><b class='foo'><c data-probe>x</c></b>",
            "#a, b { & c { color: blue } } .foo c { color: red }",
        );
        assert_eq!(color.as_deref(), Some("blue"));
    }

    /// CSS Nesting §3.1: a relative selector without `&` implies one, so it
    /// inherits the parent's specificity. The universal selector contributes
    /// nothing, so `#target *` is (1,0,0) and the implied `&` is what makes the
    /// nested rule's `:is(#target) .x`, at (1,1,0), outrank it.
    #[test]
    fn a_relative_selector_implies_the_parent_specificity() {
        let color = winning_color(
            "<!doctype html><div id='target'><span class='x' data-probe></span></div>",
            "#target { color: green } #target { .x { color: red } } #target * { color: blue }",
        );
        assert_eq!(color.as_deref(), Some("red"));
    }

    /// CSS Nesting §3.4: `:where(&)` reduces the nesting selector to zero, so
    /// the nested rule now loses to its parent on specificity even though it
    /// comes later in source order.
    #[test]
    fn where_reduces_the_nesting_selector_to_zero() {
        let color = winning_color(
            "<!doctype html><div id='target' data-probe></div>",
            "#target { color: blue } #target { :where(&) { color: red } }",
        );
        assert_eq!(color.as_deref(), Some("blue"));
    }

    /// CSS Nesting §3.4: a nested rule is considered to come after its parent
    /// rule, so with equal specificity the nested declaration wins, and a
    /// declaration written after the nested rule wins over it.
    #[test]
    fn nested_rules_come_after_their_parent_in_source_order() {
        let color = winning_color(
            "<!doctype html><article data-probe></article>",
            "article { color: green; & { color: blue } }",
        );
        assert_eq!(color.as_deref(), Some("blue"));

        // §3.4's example: the trailing declarations become a nested
        // declarations rule, which is ordered after the nested style rule.
        let color = winning_color(
            "<!doctype html><article data-probe></article>",
            "article { color: green; & { color: blue } color: red }",
        );
        assert_eq!(color.as_deref(), Some("red"));
    }

    /// CSS Nesting §3.3: a nested `@media` gates the parent selector's
    /// declarations on the query, and the query is still evaluated.
    #[test]
    fn nested_media_rules_are_gated_by_their_query() {
        let html = "<!doctype html><div class='foo' data-probe></div>";
        let source = ".foo { @media screen and (min-width: 700px) { color: green } }";
        let narrow = parse_document(html);

        let sheet = parse_stylesheet(source);
        let probe = {
            let selectors = parse_selector_list("[data-probe]").expect("valid test selector");
            select_all(
                &narrow.dom,
                narrow.dom.document(),
                &selectors,
                &MatchContext::default(),
            )[0]
        };
        let context = MatchContext {
            viewport_width: Some(800.0),
            viewport_height: Some(600.0),
            ..MatchContext::default()
        };
        let style = cascade_element(
            &narrow.dom,
            probe,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &context,
        );
        assert_eq!(
            style.get("color").map(|value| value.value.as_str()),
            Some("green")
        );

        let wide = MatchContext {
            viewport_width: Some(400.0),
            viewport_height: Some(600.0),
            ..MatchContext::default()
        };
        let style = cascade_element(
            &narrow.dom,
            probe,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &wide,
        );
        assert_eq!(style.get("color"), None);
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

    /// The predicate exists so that a media query the cascade *does* evaluate
    /// is never reported as unsupported. These are the forms a real page
    /// ships, including the minified conjunction with no spaces around `and`.
    #[test]
    fn every_media_query_the_cascade_evaluates_reports_as_supported() {
        for query in [
            "",
            "all",
            "screen",
            "only screen",
            "print",
            "not print",
            "speech",
            "(min-width: 768px)",
            "(max-width: 768px)",
            "(width: 1024px)",
            "(min-height: 480px)",
            "(max-height: 480px)",
            "(orientation: landscape)",
            "(orientation: portrait)",
            "screen and (min-width: 700px)",
            "(min-width:1560px)and (max-width:2059.9px)",
            "screen, print",
            "not screen and (min-width: 400px)",
        ] {
            assert!(
                media_query_list_is_supported(query),
                "{query:?} is evaluated by the cascade and must not be reported unsupported"
            );
        }
    }

    /// A feature the engine genuinely does not evaluate is reported as
    /// unsupported, which is what makes the diagnostic worth reading. These
    /// are the features the corpus gates rules out on.
    #[test]
    fn media_features_the_engine_cannot_evaluate_report_as_unsupported() {
        for query in [
            "(prefers-reduced-motion: reduce)",
            "(prefers-color-scheme: dark)",
            "(min-resolution: 2dppx)",
            "(device-min-pixel-ratio: 2)",
            "(color-gamut: p3)",
            "(min-width: 10vw)",
            "(400px <= width <= 700px)",
            "(unknown-feature: 1)",
            "screen and (hover: hover)",
        ] {
            assert!(
                !media_query_list_is_supported(query),
                "{query:?} names something the engine does not evaluate"
            );
        }
        // One unsupported conjunct makes the whole list unsupported.
        assert!(!media_query_list_is_supported(
            "screen and (min-width: 700px) and (hover: hover)"
        ));
    }

    /// A viewport-dependent feature asked before the viewport is known is not a
    /// capability gap, so it is supported even though it cannot match yet.
    #[test]
    fn a_viewport_dependent_feature_is_supported_before_the_viewport_is_known() {
        assert!(media_query_list_is_supported("(orientation: landscape)"));
        assert!(media_query_list_is_supported("(min-width: 768px)"));
        // ... and it still does not match without a viewport, which is what
        // `media_query_list_matches` reports.
        assert!(!media_query_list_matches(
            "(orientation: landscape)",
            &MatchContext::default()
        ));
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
    fn flex_flow_resets_both_longhands_and_accepts_case_insensitive_keywords() {
        let (dom, target) = document_and_target();
        let sheet = parse_stylesheet(
            "#target { flex-direction: column; flex-wrap: wrap-reverse; flex-flow: ROW WRAP }",
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
            style
                .get("flex-direction")
                .map(|value| value.value.as_str()),
            Some("row")
        );
        assert_eq!(
            style.get("flex-wrap").map(|value| value.value.as_str()),
            Some("wrap")
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

    /// The reported bug: "underlines are everywhere; many pages remove them and
    /// they still render."
    ///
    /// The user-agent stylesheet underlines links (`a:link { text-decoration-
    /// line: underline }`). A page that writes `a { text-decoration: none }`
    /// should win, because the author origin outranks the user-agent origin.
    /// It did not, for two reasons that had to be fixed together: the
    /// shorthand never expanded, so the author's `none` was stored under the
    /// literal key `text-decoration` and the longhand was never overridden at
    /// all; and because the longhand *was* set by the UA sheet, the
    /// unexpanded fallback could never fire.
    ///
    /// The selectors are `#target` rather than `a` so the shared fixture
    /// matches, but the cascade shape - a user-agent-origin longhand against an
    /// author-origin shorthand - is the one from the report.
    #[test]
    fn an_author_text_decoration_none_beats_the_user_agent_underline() {
        let (dom, target) = document_and_target();
        let ua = parse_stylesheet("#target { text-decoration-line: underline }");
        let author = parse_stylesheet("#target { text-decoration: none }");
        let style = cascade_element(
            &dom,
            target,
            &[
                CascadeInput {
                    sheet: &ua,
                    origin: CascadeOrigin::UserAgent,
                },
                CascadeInput {
                    sheet: &author,
                    origin: CascadeOrigin::Author,
                },
            ],
            &MatchContext::default(),
        );

        // The author's rule wins on the longhand, not merely on the shorthand.
        assert_eq!(
            style
                .get("text-decoration-line")
                .map(|value| value.value.as_str()),
            Some("none"),
            "text-decoration: none must expand to text-decoration-line: none so \
             it can outrank the UA sheet"
        );
        // And the longhand is what the declaration was recorded under, so a
        // consumer reading only the longhand sees the author's intent.
        assert!(style.get("text-decoration").is_none());
    }

    /// The same two sheets without an author override must still underline, or
    /// the test above would pass for a reason that has nothing to do with the
    /// cascade.
    #[test]
    fn the_user_agent_underline_survives_when_the_author_says_nothing() {
        let (dom, target) = document_and_target();
        let ua = parse_stylesheet("#target { text-decoration-line: underline }");
        let author = parse_stylesheet("");
        let style = cascade_element(
            &dom,
            target,
            &[
                CascadeInput {
                    sheet: &ua,
                    origin: CascadeOrigin::UserAgent,
                },
                CascadeInput {
                    sheet: &author,
                    origin: CascadeOrigin::Author,
                },
            ],
            &MatchContext::default(),
        );

        assert_eq!(
            style
                .get("text-decoration-line")
                .map(|value| value.value.as_str()),
            Some("underline")
        );
    }

    /// Text Decoration 4 §2.6: "Omitted values are set to their initial
    /// values." A shorthand that mentions only a line must still reset the
    /// other three, which is what makes the `none` case above work.
    #[test]
    fn text_decoration_none_expands_to_the_line_none_and_three_initials() {
        let expanded = super::expanded_declaration("text-decoration", "none");
        assert_eq!(
            expanded,
            vec![
                ("text-decoration-line".to_owned(), "none".to_owned()),
                ("text-decoration-thickness".to_owned(), "auto".to_owned()),
                ("text-decoration-style".to_owned(), "solid".to_owned()),
                (
                    "text-decoration-color".to_owned(),
                    "currentcolor".to_owned()
                ),
            ]
        );
    }

    /// The same §2.6 rule for the *other* three initials, and specifically for a
    /// shorthand value that omits the line slot entirely.
    ///
    /// `text-decoration: none` does not reach the omitted-value path: `none` is
    /// itself a `text-decoration-line` value, so it fills that slot and the
    /// `unwrap_or_else` is never asked for it. A shorthand that mentions only a
    /// style, a thickness or a colour is what does, and the line it has to write
    /// is `none` - the initial of `text-decoration-line` - not any line keyword.
    ///
    /// This is the slot that makes the project work: `a { text-decoration:
    /// none }` in the reported bug had to beat a user-agent
    /// `text-decoration-line: underline`, and a shorthand that wrote a line
    /// keyword of its own into an omitted slot would underline whatever it
    /// touched.
    #[test]
    fn a_shorthand_that_omits_the_line_slot_writes_the_line_initial() {
        for (value, thickness, style, color) in [
            ("solid", "auto", "solid", "currentcolor"),
            ("wavy", "auto", "wavy", "currentcolor"),
            ("2px", "2px", "solid", "currentcolor"),
            ("blue", "auto", "solid", "blue"),
            ("2px wavy blue", "2px", "wavy", "blue"),
        ] {
            assert_eq!(
                super::expanded_declaration("text-decoration", value),
                vec![
                    ("text-decoration-line".to_owned(), "none".to_owned()),
                    ("text-decoration-thickness".to_owned(), thickness.to_owned()),
                    ("text-decoration-style".to_owned(), style.to_owned()),
                    ("text-decoration-color".to_owned(), color.to_owned()),
                ],
                "{value:?}: an omitted text-decoration-line is its initial, `none`"
            );
        }
    }

    /// Text Decoration 4 §2.6's `||` production, all four slots in one value
    /// and in an order that differs from the grammar's. `||` permits each slot
    /// at most once, so a second style keyword would be invalid.
    #[test]
    fn text_decoration_expands_all_four_slots_in_any_order() {
        assert_eq!(
            super::expanded_declaration("text-decoration", "2px wavy blue underline"),
            vec![
                ("text-decoration-line".to_owned(), "underline".to_owned()),
                ("text-decoration-thickness".to_owned(), "2px".to_owned()),
                ("text-decoration-style".to_owned(), "wavy".to_owned()),
                ("text-decoration-color".to_owned(), "blue".to_owned()),
            ]
        );
    }

    /// The `||` in §2.1 means the line keywords accumulate:
    /// `underline overline` is one legal value, not two declarations.
    #[test]
    fn text_decoration_accumulates_multiple_line_keywords() {
        assert_eq!(
            super::expanded_declaration("text-decoration", "underline overline"),
            vec![
                (
                    "text-decoration-line".to_owned(),
                    "underline overline".to_owned()
                ),
                ("text-decoration-thickness".to_owned(), "auto".to_owned()),
                ("text-decoration-style".to_owned(), "solid".to_owned()),
                (
                    "text-decoration-color".to_owned(),
                    "currentcolor".to_owned()
                ),
            ]
        );
    }

    /// Text Decoration 3 §2.4 omits `thickness` from the shorthand, so
    /// `dotted red line-through` is a valid Level 3 value. It must expand, and
    /// the omitted thickness must take its Level 4 initial.
    #[test]
    fn text_decoration_accepts_the_level_3_three_slot_grammar() {
        assert_eq!(
            super::expanded_declaration("text-decoration", "dotted red line-through"),
            vec![
                ("text-decoration-line".to_owned(), "line-through".to_owned()),
                ("text-decoration-thickness".to_owned(), "auto".to_owned()),
                ("text-decoration-style".to_owned(), "dotted".to_owned()),
                ("text-decoration-color".to_owned(), "red".to_owned()),
            ]
        );
    }

    /// CSS 2.1 §16.3.1: `none | [ underline || overline || line-through ||
    /// blink ]`, line keywords only. These are the oldest declarations still in
    /// circulation and must keep working.
    #[test]
    fn text_decoration_accepts_the_css21_line_only_grammar() {
        assert_eq!(
            super::expanded_declaration("text-decoration", "blink"),
            vec![
                ("text-decoration-line".to_owned(), "blink".to_owned()),
                ("text-decoration-thickness".to_owned(), "auto".to_owned()),
                ("text-decoration-style".to_owned(), "solid".to_owned()),
                (
                    "text-decoration-color".to_owned(),
                    "currentcolor".to_owned()
                ),
            ]
        );
    }

    /// A value that is not in any of the three grammars must not be written
    /// into the longhands as though it were. Leaving the declaration under its
    /// own name keeps the existing unexpanded consumer working, which reads it
    /// as "no line keywords" - the same thing an invalid declaration becomes
    /// at computed-value time.
    #[test]
    fn an_unreadable_text_decoration_is_left_unexpanded() {
        for value in [
            "bogus",
            "underline underline",
            "underline dotted dotted",
            "underline calc(2px)",
            "underline 2px 3px",
        ] {
            assert_eq!(
                super::expanded_declaration("text-decoration", value),
                vec![("text-decoration".to_owned(), value.to_owned())],
                "{value:?}"
            );
        }
    }

    /// Text Decoration 4 §2.3 makes `currentcolor` a legal
    /// `text-decoration-color`, so the shorthand must accept it as an explicit
    /// colour rather than choking on it.
    #[test]
    fn text_decoration_color_accepts_currentcolor() {
        assert_eq!(
            super::expanded_declaration("text-decoration", "underline currentcolor"),
            vec![
                ("text-decoration-line".to_owned(), "underline".to_owned()),
                ("text-decoration-thickness".to_owned(), "auto".to_owned()),
                ("text-decoration-style".to_owned(), "solid".to_owned()),
                (
                    "text-decoration-color".to_owned(),
                    "currentcolor".to_owned()
                ),
            ]
        );
    }

    /// The colour slot must be tested against the real grammar, not guessed,
    /// so a functional or hash colour keeps its own text instead of being
    /// misfiled as a thickness.
    #[test]
    fn text_decoration_color_keeps_functional_and_hash_colours() {
        for (value, expected) in [
            ("underline rgb(1, 2, 3)", "rgb(1, 2, 3)"),
            ("underline #0f0", "#0f0"),
            ("underline transparent", "transparent"),
        ] {
            let expanded = super::expanded_declaration("text-decoration", value);
            assert_eq!(
                expanded
                    .iter()
                    .find(|(name, _)| name == "text-decoration-color")
                    .map(|(_, color)| color.as_str()),
                Some(expected),
                "{value:?}"
            );
            assert_eq!(
                expanded
                    .iter()
                    .find(|(name, _)| name == "text-decoration-thickness")
                    .map(|(_, thickness)| thickness.as_str()),
                Some("auto"),
                "{value:?}: a colour must not be read as a thickness"
            );
        }
    }

    /// Text Decoration 4 §2: a decoration originates at the element that
    /// specifies it and propagates *down* the box tree. Nothing propagates
    /// upward, so a descendant's value cannot switch off an ancestor's
    /// decoration - CSS 2.1 §16.3.1 says so outright ("The 'text-decoration'
    /// property on descendant elements cannot have any effect on the decoration
    /// of the ancestor"), and §2.2/§2.3 restate it for style and colour
    /// ("affects all decorations originating from this element even if
    /// descendant boxes specify a different style").
    ///
    /// This is pinned because the opposite is a natural mistake, and a
    /// consumer that treats a descendant's `none` as a switch-off will
    /// un-underline text that the spec says must stay underlined. The two
    /// elements keep their own independent values, which is what lets a
    /// consumer find the ancestor's decoration by looking upward.
    #[test]
    fn a_descendants_none_does_not_reach_the_ancestors_decoration() {
        let output =
            parse_document("<!doctype html><a id='link' href='#'><span id='inner'>text</span></a>");
        let sheet = parse_stylesheet(
            "#link { text-decoration: underline } #inner { text-decoration: none }",
        );
        let dom = &output.dom;
        let link = select_all(
            dom,
            dom.document(),
            &parse_selector_list("#link").expect("valid selector"),
            &MatchContext::default(),
        )[0];
        let inner = select_all(
            dom,
            dom.document(),
            &parse_selector_list("#inner").expect("valid selector"),
            &MatchContext::default(),
        )[0];
        let link_style = cascade_element(
            dom,
            link,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );
        let inner_style = cascade_element(
            dom,
            inner,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );

        // Each element holds its own value, and each authored its own.
        assert_eq!(
            link_style
                .get("text-decoration-line")
                .map(|value| value.value.as_str()),
            Some("underline")
        );
        assert!(
            link_style
                .get("text-decoration-line")
                .is_some_and(super::CascadedValue::is_authored)
        );
        assert_eq!(
            inner_style
                .get("text-decoration-line")
                .map(|value| value.value.as_str()),
            Some("none")
        );
        assert!(
            inner_style
                .get("text-decoration-line")
                .is_some_and(super::CascadedValue::is_authored)
        );
        // The ancestor's value is untouched by the descendant's declaration, so
        // a consumer walking up from the inner span and taking the first
        // non-`none` line finds `underline` - which is what the spec requires.
        // It is not switched off, and it is not a missing value either.
    }

    /// Shorthand expansion happens before the winner is chosen, so a longhand
    /// written later in the same block must beat the shorthand, exactly as it
    /// does for `border`.
    #[test]
    fn a_text_decoration_longhand_after_the_shorthand_wins() {
        let (dom, target) = document_and_target();
        let sheet =
            parse_stylesheet("#target { text-decoration: underline; text-decoration-line: none }");
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
                .get("text-decoration-line")
                .map(|value| value.value.as_str()),
            Some("none")
        );
    }

    /// The reverse direction: a shorthand after a longhand must win, or a page
    /// that only ever sets `text-decoration-line: overline` and then resets
    /// with `text-decoration: none` would keep the overline.
    #[test]
    fn the_text_decoration_shorthand_resets_an_earlier_longhand() {
        let (dom, target) = document_and_target();
        let sheet =
            parse_stylesheet("#target { text-decoration-line: overline; text-decoration: none }");
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
                .get("text-decoration-line")
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
