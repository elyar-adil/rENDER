//! CSS cascade winner selection.
//!
//! The result is a cascaded (not computed or used) value map. Inheritance,
//! CSS-wide keywords, custom-property substitution, and property grammars are
//! intentionally separate stages.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::dom::{Dom, NodeId};

use super::properties::{expand_flex_shorthand, expand_gap_shorthand};
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

fn expanded_declaration(name: &str, value: &str) -> Vec<(String, String)> {
    if name.eq_ignore_ascii_case("background") {
        return expand_background_shorthand(value);
    }
    if matches!(name, "margin" | "padding") {
        return expand_box_shorthand(name, value);
    }
    if name == "border" {
        return expand_border_shorthand(value);
    }
    if name != "gap" && name != "flex" && name != "overflow" {
        return vec![(name.to_owned(), value.to_owned())];
    }
    if css_wide_keyword(value).is_some() {
        let longhands: &[&str] = if name == "gap" {
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
        "gap" => expand_gap_shorthand(value).map_or_else(
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
    let image = extract_css_url(value).map_or_else(|| "none".to_owned(), |url| format!("url({url})"));
    let repeat = ["no-repeat", "repeat-x", "repeat-y", "repeat"]
        .into_iter()
        .find(|keyword| lower.split_ascii_whitespace().any(|part| part == *keyword))
        .unwrap_or("repeat")
        .to_owned();
    let size = value
        .split_once('/')
        .map(|(_, tail)| tail.split_ascii_whitespace().next().unwrap_or("auto"))
        .filter(|part| matches!(part.to_ascii_lowercase().as_str(), "cover" | "contain" | "auto"))
        .unwrap_or("auto")
        .to_owned();
    let position = if lower.contains("center") {
        "center center"
    } else if lower.contains("right") {
        "right center"
    } else {
        "0% 0%"
    };
    vec![
        ("background-image".to_owned(), image),
        ("background-repeat".to_owned(), repeat),
        ("background-position".to_owned(), position.to_owned()),
        ("background-size".to_owned(), size),
    ]
}

fn extract_css_url(value: &str) -> Option<&str> {
    let start = value.to_ascii_lowercase().find("url(")?.saturating_add(4);
    let tail = &value[start..];
    let end = tail.find(')')?;
    Some(tail[..end].trim().trim_matches(['\'', '"']))
}

fn expand_box_shorthand(name: &str, value: &str) -> Vec<(String, String)> {
    let values: Vec<&str> = value.split_ascii_whitespace().collect();
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

fn expand_border_shorthand(value: &str) -> Vec<(String, String)> {
    let values: Vec<&str> = value.split_ascii_whitespace().collect();
    let mut result = Vec::new();
    for token in values {
        let property =
            if token.ends_with("px") || token == "thin" || token == "medium" || token == "thick" {
                "border-width"
            } else if matches!(
                token,
                "none" | "hidden" | "dotted" | "dashed" | "solid" | "double"
            ) {
                "border-style"
            } else {
                "border-color"
            };
        for edge in ["top", "right", "bottom", "left"] {
            let suffix = match property {
                "border-width" => "width",
                "border-style" => "style",
                _ => "color",
            };
            result.push((format!("border-{edge}-{suffix}"), token.to_owned()));
        }
    }
    if result.is_empty() {
        vec![("border".to_owned(), value.to_owned())]
    } else {
        result
    }
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
    use crate::css::selector::{MatchContext, parse_selector_list, select_all};
    use crate::css::stylesheet::parse_stylesheet;
    use crate::html::parse_document;

    fn document_and_target() -> (crate::dom::Dom, crate::dom::NodeId) {
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
}
