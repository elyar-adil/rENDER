//! CSS cascade winner selection.
//!
//! The result is a cascaded (not computed or used) value map. Inheritance,
//! CSS-wide keywords, custom-property substitution, and property grammars are
//! intentionally separate stages.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use render_dom::{Dom, Node, NodeId, NodeKind};

use super::container::{NoStyleQueries, StyleQueryEnvironment};
use super::length::{LengthContext, resolve_length_expr};
use super::properties::{
    expand_columns_shorthand, expand_flex_shorthand, expand_gap_shorthand,
    expand_grid_axis_shorthand, parse_typed_property,
};
use super::selector::{
    ColorGamut, ColorScheme, ContrastPreference, DisplayMode, DynamicRange, Hover, MatchContext,
    OverflowBlock, OverflowInline, Pointer, ReducedPreference, Scripting, Specificity, SubjectKey,
    SubjectPseudo, UpdateFrequency, matching_specificity,
};
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

/// Per-source buckets of rules keyed by their selector's rightmost compound.
///
/// Without this, every element is tested against every rule in every sheet:
/// GitHub's 28 stylesheets put tens of thousands of rules in front of ~2,600
/// elements, which took 42 seconds in a release build. A rule can only match
/// when the element carries at least one id, class or type named by the
/// selector's rightmost compound, so the buckets cut the candidate set before
/// any combinator or `:has`/`:not` work happens.
#[derive(Debug, Default)]
pub(crate) struct CascadeRuleIndex {
    sources: Vec<RuleBuckets>,
    rules: usize,
    keyed_entries: usize,
    universal_entries: usize,
    debug: bool,
    elements: AtomicU64,
    candidate_rules: AtomicU64,
    selectors_tried: AtomicU64,
    matched_rules: AtomicU64,
}

#[derive(Clone, Debug, Default)]
struct RuleBuckets {
    /// Keyed by the pseudo-element context (`""` for ordinary elements).
    contexts: HashMap<String, ContextBuckets>,
}

#[derive(Clone, Debug, Default)]
struct ContextBuckets {
    keyed: HashMap<SubjectKey, Vec<usize>>,
    /// Rules whose subject has no id, class, type or attribute key (`*`,
    /// `:root`, a bare `:not(...)`).
    universal: Vec<usize>,
}

impl CascadeRuleIndex {
    pub(crate) fn new(sources: &[CascadeInput<'_>]) -> Self {
        let mut indexed = Self {
            sources: Vec::with_capacity(sources.len()),
            debug: std::env::var_os("RENDER_DEBUG_CSS_INDEX").is_some(),
            ..Self::default()
        };
        for source in sources {
            let mut buckets = RuleBuckets::default();
            indexed.rules += source.sheet.rules.len();
            for (rule_index, rule) in source.sheet.rules.iter().enumerate() {
                let mut inserts: Vec<(String, Vec<SubjectKey>)> = Vec::new();
                for selector in rule.selectors.selectors() {
                    let pseudo = match selector.subject_pseudo() {
                        SubjectPseudo::Never => continue,
                        SubjectPseudo::None => String::new(),
                        SubjectPseudo::Named(name) => name.to_ascii_lowercase(),
                    };
                    let mut keys = Vec::new();
                    selector.subject_keys(&mut keys);
                    inserts.push((pseudo, keys));
                }
                for (pseudo, keys) in inserts {
                    let context = buckets.contexts.entry(pseudo).or_default();
                    if keys.is_empty() {
                        if context.universal.last() != Some(&rule_index) {
                            context.universal.push(rule_index);
                            indexed.universal_entries += 1;
                        }
                        continue;
                    }
                    for key in keys {
                        let list = context.keyed.entry(key).or_default();
                        // Rules arrive in ascending order, so a duplicate from
                        // another selector in the same rule is always the tail.
                        if list.last() != Some(&rule_index) {
                            list.push(rule_index);
                            indexed.keyed_entries += 1;
                        }
                    }
                }
            }
            indexed.sources.push(buckets);
        }
        indexed
    }

    pub(crate) fn stats(&self) -> (usize, usize, usize, u64, u64, u64, u64) {
        (
            self.rules,
            self.keyed_entries,
            self.universal_entries,
            self.elements.load(Ordering::Relaxed),
            self.candidate_rules.load(Ordering::Relaxed),
            self.selectors_tried.load(Ordering::Relaxed),
            self.matched_rules.load(Ordering::Relaxed),
        )
    }

    /// Appends every rule of `source_index` whose subject could match `keys` in
    /// the `pseudo` context (`""` for ordinary elements).
    fn candidates(
        &self,
        source_index: usize,
        pseudo: &str,
        keys: &[SubjectKey],
        out: &mut Vec<usize>,
    ) {
        let Some(buckets) = self.sources.get(source_index) else {
            return;
        };
        let Some(context) = buckets.contexts.get(pseudo) else {
            return;
        };
        out.extend_from_slice(&context.universal);
        for key in keys {
            if let Some(list) = context.keyed.get(key) {
                out.extend_from_slice(list);
            }
        }
    }
}

/// The id, class and type keys carried by one element.
fn element_subject_keys(dom: &Dom, element: NodeId) -> Vec<SubjectKey> {
    let mut keys = Vec::new();
    let Some(NodeKind::Element(data)) = dom.node(element).map(Node::kind) else {
        return keys;
    };
    keys.push(SubjectKey::Type(data.local_name.to_ascii_lowercase()));
    if let Ok(Some(id)) = dom.attribute(element, "id")
        && !id.is_empty()
    {
        keys.push(SubjectKey::Id(id.to_ascii_lowercase()));
    }
    if let Ok(Some(class)) = dom.attribute(element, "class") {
        for name in class.split_ascii_whitespace() {
            keys.push(SubjectKey::Class(name.to_ascii_lowercase()));
        }
    }
    for attribute in &data.attributes {
        keys.push(SubjectKey::Attribute(
            attribute.local_name.to_ascii_lowercase(),
        ));
    }
    keys
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
    cascade_element_with_origins_impl(
        dom,
        element,
        sources,
        context,
        ua_declarations,
        inline_declarations,
        &CascadeLookup {
            rule_index: None,
            style_queries: &NoStyleQueries,
        },
    )
}

/// The per-document lookup state a cascade pass reuses for every element.
///
/// [`compute_document_styles_with_hints`](super::computed::compute_document_styles_with_hints)
/// builds the rule index once per document; building it inside the cascade
/// would repeat that work for every element, and skipping it entirely would
/// restore the quadratic scan the index exists to avoid. The style environment
/// is the partially built top-down style map, so a `style()` query can be
/// answered by the same pass that consumes it.
pub(crate) struct CascadeLookup<'a> {
    pub(crate) rule_index: Option<&'a CascadeRuleIndex>,
    pub(crate) style_queries: &'a dyn StyleQueryEnvironment,
}

/// [`cascade_element_with_origins`] with a prebuilt rule index and a source of
/// computed styles for `style()` container queries.
pub(crate) fn cascade_element_with_origins_indexed(
    dom: &Dom,
    element: NodeId,
    sources: &[CascadeInput<'_>],
    context: &MatchContext,
    ua_declarations: &[Declaration],
    inline_declarations: &[Declaration],
    lookup: &CascadeLookup<'_>,
) -> CascadedStyle {
    cascade_element_with_origins_impl(
        dom,
        element,
        sources,
        context,
        ua_declarations,
        inline_declarations,
        lookup,
    )
}

fn cascade_element_with_origins_impl(
    dom: &Dom,
    element: NodeId,
    sources: &[CascadeInput<'_>],
    context: &MatchContext,
    ua_declarations: &[Declaration],
    inline_declarations: &[Declaration],
    lookup: &CascadeLookup<'_>,
) -> CascadedStyle {
    let layer_orders = collect_layer_orders(sources);
    let mut candidates: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
    let mut source_order = 0_u64;

    // An author stylesheet is scoped to the tree it belongs to, so the
    // document's sheets never reach an element inside a shadow tree. The
    // user-agent and user origins still apply there. Shadow-tree sheets are not
    // collected yet, so no author rule applies inside a shadow tree.
    let in_shadow_tree = dom.shadow_host(dom.root_node(element, false)).is_some();
    let subject_keys = lookup
        .rule_index
        .map(|_| element_subject_keys(dom, element));
    if let Some(index) = lookup.rule_index
        && index.debug
    {
        index.elements.fetch_add(1, Ordering::Relaxed);
    }
    for (source_index, source) in sources.iter().enumerate() {
        if in_shadow_tree && source.origin == CascadeOrigin::Author {
            continue;
        }
        let mut candidate_rules = Vec::new();
        if let (Some(index), Some(keys)) = (lookup.rule_index, subject_keys.as_ref()) {
            index.candidates(
                source_index,
                context.pseudo_element.as_deref().unwrap_or(""),
                keys,
                &mut candidate_rules,
            );
            candidate_rules.sort_unstable();
            candidate_rules.dedup();
            if let Some(index) = lookup.rule_index
                && index.debug
            {
                index.candidate_rules.fetch_add(
                    u64::try_from(candidate_rules.len()).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
            }
        } else {
            candidate_rules.extend(0..source.sheet.rules.len());
        }
        for candidate_rule in candidate_rules {
            let rule = &source.sheet.rules[candidate_rule];
            if !rule
                .media
                .iter()
                .all(|query| media_query_list_matches(query, context))
            {
                continue;
            }
            // CSS Conditional 5 §3.1: a container condition is evaluated
            // against the element's nearest eligible ancestor container. Size
            // geometry arrives from the previous layout pass; style features
            // read the ancestor styles already computed by this top-down pass.
            // An empty context answers false for every size query, never
            // "maybe".
            if !rule.container.iter().all(|condition| {
                context
                    .containers
                    .matches_with_style(dom, element, condition, lookup.style_queries)
            }) {
                continue;
            }
            if let Some(index) = lookup.rule_index
                && index.debug
            {
                index.selectors_tried.fetch_add(
                    u64::try_from(rule.selectors.selectors().len()).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
            }
            let Some(specificity) = matching_specificity(dom, element, &rule.selectors, context)
            else {
                continue;
            };
            if let Some(index) = lookup.rule_index
                && index.debug
            {
                index.matched_rules.fetch_add(1, Ordering::Relaxed);
            }
            for declaration in &rule.declarations {
                source_order = source_order.saturating_add(1);
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
/// from the same evaluation the cascade uses, so the two cannot disagree. A
/// query reporting `true` here is one whose result came from a real evaluation
/// rather than from the "unknown means false" fallback of Media Queries 4
/// §2.1.1.
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
        .all(|query| evaluate_media_query(query, &MatchContext::default()).supported)
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
    const fn matches(self) -> bool {
        match self {
            Self::Evaluated(value) => value,
            Self::Unsupported | Self::Unknown => false,
        }
    }

    /// Three-valued negation: an unknown condition stays unknown, so a
    /// `not (general-enclosed)` query is false rather than accidentally true.
    const fn negate(self) -> Self {
        match self {
            Self::Evaluated(value) => Self::Evaluated(!value),
            Self::Unsupported | Self::Unknown => Self::Unknown,
        }
    }

    fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::Evaluated(false), _) | (_, Self::Evaluated(false)) => Self::Evaluated(false),
            (Self::Evaluated(true), Self::Evaluated(true)) => Self::Evaluated(true),
            _ => Self::Unknown,
        }
    }

    fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::Evaluated(true), _) | (_, Self::Evaluated(true)) => Self::Evaluated(true),
            (Self::Evaluated(false), Self::Evaluated(false)) => Self::Evaluated(false),
            _ => Self::Unknown,
        }
    }
}

/// A condition's value together with whether every construct in it was one the
/// evaluator understood. Keeping support separate from the three-valued result
/// is what lets `or` stay short-circuit-correct without hiding an unsupported
/// feature from [`media_query_list_is_supported`].
#[derive(Clone, Copy, Debug)]
struct ConditionResult {
    value: MediaCondition,
    supported: bool,
}

impl ConditionResult {
    const fn evaluated(value: bool) -> Self {
        Self {
            value: MediaCondition::Evaluated(value),
            supported: true,
        }
    }

    const fn unknown() -> Self {
        Self {
            value: MediaCondition::Unknown,
            supported: true,
        }
    }

    const fn unsupported() -> Self {
        Self {
            value: MediaCondition::Unsupported,
            supported: false,
        }
    }

    const fn negate(self) -> Self {
        Self {
            value: self.value.negate(),
            supported: self.supported,
        }
    }

    fn and(self, other: Self) -> Self {
        Self {
            value: self.value.and(other.value),
            supported: self.supported && other.supported,
        }
    }

    fn or(self, other: Self) -> Self {
        Self {
            value: self.value.or(other.value),
            supported: self.supported && other.supported,
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
    evaluate_media_query(query, context).value.matches()
}

/// Evaluate one `<media-query>` (Media Queries 4 §3).
fn evaluate_media_query(query: &str, context: &MatchContext) -> ConditionResult {
    let lowered = query.trim().to_ascii_lowercase();
    let query = lowered.trim();
    if query.is_empty() {
        return ConditionResult::evaluated(true);
    }
    // A leading `not` followed by parentheses is the condition-level operator;
    // a leading `not` followed by a media type negates the whole query.
    if let Some(rest) = strip_keyword(query, "not") {
        if rest.starts_with('(') {
            return evaluate_media_condition(query, context);
        }
        return evaluate_media_query_body(rest, context).negate();
    }
    let query = strip_keyword(query, "only").unwrap_or(query);
    evaluate_media_query_body(query, context)
}

fn evaluate_media_query_body(query: &str, context: &MatchContext) -> ConditionResult {
    if query.starts_with('(') {
        return evaluate_media_condition(query, context);
    }
    let (media_type, rest) =
        match query.find(|character: char| character.is_ascii_whitespace() || character == '(') {
            Some(index) => (&query[..index], query[index..].trim()),
            None => (query, ""),
        };
    // A media type is valid syntax even when it is not one this engine
    // produces: `print` and any future type simply do not match.
    let result = ConditionResult::evaluated(matches!(media_type, "all" | "screen"));
    if rest.is_empty() {
        return result;
    }
    let Some(condition) = strip_keyword(rest, "and") else {
        return ConditionResult::unsupported();
    };
    result.and(evaluate_media_condition(condition, context))
}

/// Strip a leading keyword when it is followed by a token boundary. Media
/// query keywords are ASCII case-insensitive, and minified sheets write
/// `)and(` with no whitespace, so the boundary check accepts `)` and `(` as
/// well as whitespace.
fn strip_keyword<'a>(input: &'a str, keyword: &str) -> Option<&'a str> {
    let input = input.trim_start();
    let prefix = input.get(..keyword.len())?;
    if !prefix.eq_ignore_ascii_case(keyword) {
        return None;
    }
    let rest = &input[keyword.len()..];
    if let Some(next) = rest.chars().next()
        && (next.is_ascii_alphanumeric() || next == '-' || next == '_')
    {
        return None;
    }
    Some(rest.trim_start())
}

fn evaluate_media_condition(condition: &str, context: &MatchContext) -> ConditionResult {
    let condition = condition.trim();
    if condition.is_empty() {
        return ConditionResult::unsupported();
    }
    if let Some(rest) = strip_keyword(condition, "not") {
        if !rest.starts_with('(') {
            return ConditionResult::unsupported();
        }
        return evaluate_media_in_parens(rest, context).negate();
    }
    if let Some(parts) = split_top_level_keyword(condition, "or") {
        let mut result = evaluate_media_in_parens(parts[0], context);
        for part in &parts[1..] {
            result = result.or(evaluate_media_in_parens(part, context));
        }
        return result;
    }
    if let Some(parts) = split_top_level_keyword(condition, "and") {
        let mut result = evaluate_media_in_parens(parts[0], context);
        for part in &parts[1..] {
            result = result.and(evaluate_media_in_parens(part, context));
        }
        return result;
    }
    evaluate_media_in_parens(condition, context)
}

/// Split on a top-level keyword (`and`/`or`) at parenthesis depth zero.
fn split_top_level_keyword<'a>(input: &'a str, keyword: &str) -> Option<Vec<&'a str>> {
    let lowered = input.to_ascii_lowercase();
    let bytes = lowered.as_bytes();
    let mut parts = Vec::new();
    let mut depth = 0_u32;
    let mut start = 0_usize;
    let mut index = 0_usize;
    while index < bytes.len() {
        match bytes[index] {
            b'(' => depth = depth.saturating_add(1),
            b')' => depth = depth.saturating_sub(1),
            _ if depth == 0 && lowered[index..].starts_with(keyword) => {
                let before = index == 0
                    || bytes
                        .get(index.wrapping_sub(1))
                        .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b')');
                let after = bytes
                    .get(index + keyword.len())
                    .is_none_or(|byte| byte.is_ascii_whitespace() || *byte == b'(');
                if before && after {
                    parts.push(input[start..index].trim());
                    index += keyword.len();
                    start = index;
                    continue;
                }
            }
            _ => {}
        }
        index += 1;
    }
    if parts.is_empty() {
        None
    } else {
        parts.push(input[start..].trim());
        Some(parts)
    }
}

fn evaluate_media_in_parens(value: &str, context: &MatchContext) -> ConditionResult {
    let value = value.trim();
    let Some(inner) = value
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
    else {
        return ConditionResult::unsupported();
    };
    let inner = inner.trim();
    if inner.is_empty() {
        return ConditionResult::unsupported();
    }
    if inner.starts_with('(') || strip_keyword(inner, "not").is_some() {
        return evaluate_media_condition(inner, context);
    }
    evaluate_media_feature(inner, context)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Comparison {
    Equal,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

impl Comparison {
    const fn reverse(self) -> Self {
        match self {
            Self::Equal => Self::Equal,
            Self::Less => Self::Greater,
            Self::LessOrEqual => Self::GreaterOrEqual,
            Self::Greater => Self::Less,
            Self::GreaterOrEqual => Self::LessOrEqual,
        }
    }

    const fn same_direction(self, other: Self) -> bool {
        matches!(
            (self, other),
            (
                Self::Less | Self::LessOrEqual,
                Self::Less | Self::LessOrEqual
            ) | (
                Self::Greater | Self::GreaterOrEqual,
                Self::Greater | Self::GreaterOrEqual
            )
        )
    }
}

fn evaluate_media_feature(feature: &str, context: &MatchContext) -> ConditionResult {
    let feature = feature.trim();
    if let Some((name, value)) = feature.split_once(':') {
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || value.is_empty() {
            return ConditionResult::unsupported();
        }
        return evaluate_legacy_media_feature(name, value, context);
    }
    let operators = split_comparisons(feature);
    match operators.as_slice() {
        [] => evaluate_boolean_media_feature(feature, context),
        [(position, length, comparison)] => {
            let left = feature[..*position].trim();
            let right = feature[position + length..].trim();
            if left.is_empty() || right.is_empty() {
                return ConditionResult::unsupported();
            }
            if is_feature_name(left) && !is_feature_name(right) {
                evaluate_media_feature_value(left, right, *comparison, context)
            } else if is_feature_name(right) && !is_feature_name(left) {
                evaluate_media_feature_value(right, left, comparison.reverse(), context)
            } else {
                ConditionResult::unsupported()
            }
        }
        [
            (first_position, first_length, first_comparison),
            (second_position, second_length, second_comparison),
        ] => {
            let left = feature[..*first_position].trim();
            let name = feature[first_position + first_length..*second_position].trim();
            let right = feature[second_position + second_length..].trim();
            if left.is_empty() || right.is_empty() || !is_feature_name(name) {
                return ConditionResult::unsupported();
            }
            if !first_comparison.same_direction(*second_comparison) {
                return ConditionResult::unsupported();
            }
            evaluate_media_feature_value(name, left, first_comparison.reverse(), context).and(
                evaluate_media_feature_value(name, right, *second_comparison, context),
            )
        }
        _ => ConditionResult::unsupported(),
    }
}

/// The comparison operators in a range context, at parenthesis depth zero.
fn split_comparisons(input: &str) -> Vec<(usize, usize, Comparison)> {
    let bytes = input.as_bytes();
    let mut result = Vec::new();
    let mut depth = 0_u32;
    let mut index = 0_usize;
    while index < bytes.len() {
        match bytes[index] {
            b'(' => depth = depth.saturating_add(1),
            b')' => depth = depth.saturating_sub(1),
            b'<' | b'>' | b'=' if depth == 0 => {
                let comparison = match bytes[index] {
                    b'<' => {
                        if bytes.get(index + 1) == Some(&b'=') {
                            Comparison::LessOrEqual
                        } else {
                            Comparison::Less
                        }
                    }
                    b'>' => {
                        if bytes.get(index + 1) == Some(&b'=') {
                            Comparison::GreaterOrEqual
                        } else {
                            Comparison::Greater
                        }
                    }
                    _ => Comparison::Equal,
                };
                let length = if matches!(
                    comparison,
                    Comparison::LessOrEqual | Comparison::GreaterOrEqual
                ) {
                    2
                } else {
                    1
                };
                result.push((index, length, comparison));
                index += length;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    result
}

fn is_feature_name(value: &str) -> bool {
    let mut characters = value.chars();
    if value.starts_with('-') {
        characters.next();
    }
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic())
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContinuousFeature {
    Width,
    Height,
    AspectRatio,
    Resolution,
    Color,
    Monochrome,
    ColorIndex,
}

fn continuous_feature(name: &str) -> Option<ContinuousFeature> {
    Some(match name {
        "width" => ContinuousFeature::Width,
        "height" => ContinuousFeature::Height,
        "aspect-ratio" => ContinuousFeature::AspectRatio,
        "resolution" | "-webkit-device-pixel-ratio" => ContinuousFeature::Resolution,
        "color" => ContinuousFeature::Color,
        "monochrome" => ContinuousFeature::Monochrome,
        "color-index" => ContinuousFeature::ColorIndex,
        _ => return None,
    })
}

fn media_feature_actual(feature: ContinuousFeature, context: &MatchContext) -> Option<f32> {
    match feature {
        ContinuousFeature::Width => context.viewport_width,
        ContinuousFeature::Height => context.viewport_height,
        ContinuousFeature::AspectRatio => {
            let width = context.viewport_width?;
            let height = context.viewport_height?;
            (height > 0.0).then_some(width / height)
        }
        ContinuousFeature::Resolution => context.resolution,
        ContinuousFeature::Color => Some(f32::from(context.color_bits)),
        ContinuousFeature::Monochrome => Some(f32::from(context.monochrome_bits)),
        ContinuousFeature::ColorIndex => Some(context.color_index as f32),
    }
}

fn parse_media_feature_number(
    feature: ContinuousFeature,
    value: &str,
    context: &MatchContext,
) -> Option<f32> {
    match feature {
        ContinuousFeature::Width | ContinuousFeature::Height => parse_media_length(value, context),
        ContinuousFeature::AspectRatio => parse_media_ratio(value),
        ContinuousFeature::Resolution => parse_media_resolution(value),
        ContinuousFeature::Color
        | ContinuousFeature::Monochrome
        | ContinuousFeature::ColorIndex => {
            value.trim().parse::<u32>().ok().map(|number| number as f32)
        }
    }
}

fn evaluate_legacy_media_feature(
    name: &str,
    value: &str,
    context: &MatchContext,
) -> ConditionResult {
    for (prefix, comparison) in [
        ("min-", Comparison::GreaterOrEqual),
        ("max-", Comparison::LessOrEqual),
    ] {
        if let Some(base) = name.strip_prefix(prefix)
            && continuous_feature(base).is_some()
        {
            return evaluate_media_feature_value(base, value, comparison, context);
        }
    }
    evaluate_media_feature_value(name, value, Comparison::Equal, context)
}

fn evaluate_media_feature_value(
    name: &str,
    value: &str,
    comparison: Comparison,
    context: &MatchContext,
) -> ConditionResult {
    if let Some(feature) = continuous_feature(name) {
        let Some(expected) = parse_media_feature_number(feature, value, context) else {
            return ConditionResult::unsupported();
        };
        let Some(actual) = media_feature_actual(feature, context) else {
            return ConditionResult::unknown();
        };
        return ConditionResult::evaluated(compare(actual, comparison, expected));
    }
    if comparison != Comparison::Equal {
        return ConditionResult::unsupported();
    }
    evaluate_discrete_media_feature(name, value, context)
}

fn compare(left: f32, comparison: Comparison, right: f32) -> bool {
    match comparison {
        Comparison::Equal => (left - right).abs() <= f32::EPSILON,
        Comparison::Less => left < right,
        Comparison::LessOrEqual => left <= right,
        Comparison::Greater => left > right,
        Comparison::GreaterOrEqual => left >= right,
    }
}

fn evaluate_boolean_media_feature(name: &str, context: &MatchContext) -> ConditionResult {
    if let Some(feature) = continuous_feature(name) {
        let Some(actual) = media_feature_actual(feature, context) else {
            return ConditionResult::unknown();
        };
        return ConditionResult::evaluated(actual > 0.0);
    }
    match name {
        "pointer" => ConditionResult::evaluated(context.pointer != Pointer::None),
        "any-pointer" => ConditionResult::evaluated(context.any_pointer != Pointer::None),
        "hover" => ConditionResult::evaluated(context.hover == Hover::Hover),
        "any-hover" => ConditionResult::evaluated(context.any_hover == Hover::Hover),
        "forced-colors" => ConditionResult::evaluated(context.forced_colors),
        "inverted-colors" => ConditionResult::evaluated(context.inverted_colors),
        "update" => ConditionResult::evaluated(context.update != UpdateFrequency::None),
        "overflow-block" => {
            ConditionResult::evaluated(context.overflow_block != OverflowBlock::None)
        }
        "overflow-inline" => {
            ConditionResult::evaluated(context.overflow_inline != OverflowInline::None)
        }
        "orientation"
        | "prefers-color-scheme"
        | "prefers-reduced-motion"
        | "prefers-reduced-transparency"
        | "prefers-reduced-data"
        | "prefers-contrast"
        | "scripting"
        | "display-mode"
        | "color-gamut"
        | "dynamic-range"
        | "video-dynamic-range" => ConditionResult::evaluated(true),
        _ => ConditionResult::unsupported(),
    }
}

fn evaluate_discrete_media_feature(
    name: &str,
    value: &str,
    context: &MatchContext,
) -> ConditionResult {
    match name {
        "orientation" => {
            let Some((width, height)) = context.viewport_width.zip(context.viewport_height) else {
                return ConditionResult::unknown();
            };
            match value {
                "landscape" => ConditionResult::evaluated(width >= height),
                "portrait" => ConditionResult::evaluated(height > width),
                _ => ConditionResult::unsupported(),
            }
        }
        "prefers-color-scheme" => match value {
            "light" => {
                ConditionResult::evaluated(context.prefers_color_scheme == ColorScheme::Light)
            }
            "dark" => ConditionResult::evaluated(context.prefers_color_scheme == ColorScheme::Dark),
            _ => ConditionResult::unsupported(),
        },
        "prefers-reduced-motion" => {
            evaluate_reduced_preference(value, context.prefers_reduced_motion)
        }
        "prefers-reduced-transparency" => {
            evaluate_reduced_preference(value, context.prefers_reduced_transparency)
        }
        "prefers-reduced-data" => evaluate_reduced_preference(value, context.prefers_reduced_data),
        "prefers-contrast" => match value {
            "no-preference" => ConditionResult::evaluated(
                context.prefers_contrast == ContrastPreference::NoPreference,
            ),
            "more" => {
                ConditionResult::evaluated(context.prefers_contrast == ContrastPreference::More)
            }
            "less" => {
                ConditionResult::evaluated(context.prefers_contrast == ContrastPreference::Less)
            }
            "custom" => {
                ConditionResult::evaluated(context.prefers_contrast == ContrastPreference::Custom)
            }
            _ => ConditionResult::unsupported(),
        },
        "forced-colors" => match value {
            "none" => ConditionResult::evaluated(!context.forced_colors),
            "active" => ConditionResult::evaluated(context.forced_colors),
            _ => ConditionResult::unsupported(),
        },
        "inverted-colors" => match value {
            "none" => ConditionResult::evaluated(!context.inverted_colors),
            "inverted" => ConditionResult::evaluated(context.inverted_colors),
            _ => ConditionResult::unsupported(),
        },
        "pointer" => evaluate_pointer(value, context.pointer),
        "any-pointer" => evaluate_pointer(value, context.any_pointer),
        "hover" => evaluate_hover(value, context.hover),
        "any-hover" => evaluate_hover(value, context.any_hover),
        "scripting" => match value {
            "none" => ConditionResult::evaluated(context.scripting == Scripting::None),
            "initial-only" => {
                ConditionResult::evaluated(context.scripting == Scripting::InitialOnly)
            }
            "enabled" => ConditionResult::evaluated(context.scripting == Scripting::Enabled),
            _ => ConditionResult::unsupported(),
        },
        "display-mode" => match value {
            "browser" => ConditionResult::evaluated(context.display_mode == DisplayMode::Browser),
            "fullscreen" => {
                ConditionResult::evaluated(context.display_mode == DisplayMode::Fullscreen)
            }
            "standalone" => {
                ConditionResult::evaluated(context.display_mode == DisplayMode::Standalone)
            }
            "minimal-ui" => {
                ConditionResult::evaluated(context.display_mode == DisplayMode::MinimalUi)
            }
            "window-controls-overlay" => ConditionResult::evaluated(
                context.display_mode == DisplayMode::WindowControlsOverlay,
            ),
            "picture-in-picture" => {
                ConditionResult::evaluated(context.display_mode == DisplayMode::PictureInPicture)
            }
            _ => ConditionResult::unsupported(),
        },
        "color-gamut" => match value {
            "srgb" => ConditionResult::evaluated(true),
            "p3" => ConditionResult::evaluated(matches!(
                context.color_gamut,
                ColorGamut::P3 | ColorGamut::Rec2020
            )),
            "rec2020" => ConditionResult::evaluated(context.color_gamut == ColorGamut::Rec2020),
            _ => ConditionResult::unsupported(),
        },
        "dynamic-range" => evaluate_dynamic_range(value, context.dynamic_range),
        "video-dynamic-range" => evaluate_dynamic_range(value, context.video_dynamic_range),
        "update" => match value {
            "none" => ConditionResult::evaluated(context.update == UpdateFrequency::None),
            "slow" => ConditionResult::evaluated(context.update == UpdateFrequency::Slow),
            "fast" => ConditionResult::evaluated(context.update == UpdateFrequency::Fast),
            _ => ConditionResult::unsupported(),
        },
        "overflow-block" => match value {
            "none" => ConditionResult::evaluated(context.overflow_block == OverflowBlock::None),
            "scroll" => ConditionResult::evaluated(context.overflow_block == OverflowBlock::Scroll),
            "paged" => ConditionResult::evaluated(context.overflow_block == OverflowBlock::Paged),
            _ => ConditionResult::unsupported(),
        },
        "overflow-inline" => match value {
            "none" => ConditionResult::evaluated(context.overflow_inline == OverflowInline::None),
            "scroll" => {
                ConditionResult::evaluated(context.overflow_inline == OverflowInline::Scroll)
            }
            _ => ConditionResult::unsupported(),
        },
        _ => ConditionResult::unsupported(),
    }
}

fn evaluate_reduced_preference(value: &str, actual: ReducedPreference) -> ConditionResult {
    match value {
        "no-preference" => ConditionResult::evaluated(actual == ReducedPreference::NoPreference),
        "reduce" => ConditionResult::evaluated(actual == ReducedPreference::Reduce),
        _ => ConditionResult::unsupported(),
    }
}

fn evaluate_pointer(value: &str, actual: Pointer) -> ConditionResult {
    match value {
        "none" => ConditionResult::evaluated(actual == Pointer::None),
        "coarse" => ConditionResult::evaluated(actual == Pointer::Coarse),
        "fine" => ConditionResult::evaluated(actual == Pointer::Fine),
        _ => ConditionResult::unsupported(),
    }
}

fn evaluate_hover(value: &str, actual: Hover) -> ConditionResult {
    match value {
        "none" => ConditionResult::evaluated(actual == Hover::None),
        "hover" => ConditionResult::evaluated(actual == Hover::Hover),
        _ => ConditionResult::unsupported(),
    }
}

fn evaluate_dynamic_range(value: &str, actual: DynamicRange) -> ConditionResult {
    match value {
        "standard" => ConditionResult::evaluated(true),
        "high" => ConditionResult::evaluated(actual == DynamicRange::High),
        _ => ConditionResult::unsupported(),
    }
}

/// Resolve a media-query length to CSS pixels.
///
/// Media Queries 4 makes `em`/`rem` relative to the initial font size, not the
/// element's, so the context is deliberately viewport-and-initial-font only.
fn parse_media_length(value: &str, context: &MatchContext) -> Option<f32> {
    let value = value.trim();
    if value == "0" {
        return Some(0.0);
    }
    let length_context = LengthContext {
        percentage_base: None,
        em_base: 16.0,
        rem_base: 16.0,
        viewport_width: f64::from(context.viewport_width.unwrap_or(1.0)),
        viewport_height: f64::from(context.viewport_height.unwrap_or(1.0)),
        ..LengthContext::default()
    };
    let pixels = resolve_length_expr(value, &length_context).ok()?;
    if pixels.is_finite() && pixels >= 0.0 {
        Some(pixels as f32)
    } else {
        None
    }
}

fn parse_media_ratio(value: &str) -> Option<f32> {
    let value = value.trim();
    if let Some((numerator, denominator)) = value.split_once('/') {
        let numerator = numerator.trim().parse::<f32>().ok()?;
        let denominator = denominator.trim().parse::<f32>().ok()?;
        if !numerator.is_finite() || !denominator.is_finite() || denominator == 0.0 {
            return None;
        }
        if numerator < 0.0 || denominator < 0.0 {
            return None;
        }
        Some(numerator / denominator)
    } else {
        let value = value.parse::<f32>().ok()?;
        if value.is_finite() && value >= 0.0 {
            Some(value)
        } else {
            None
        }
    }
}

fn parse_media_resolution(value: &str) -> Option<f32> {
    let value = value.trim();
    for (unit, factor) in [
        ("dppx", 1.0_f32),
        ("x", 1.0),
        ("dpi", 1.0 / 96.0),
        ("dpcm", 2.54 / 96.0),
    ] {
        if let Some(number) = value.strip_suffix(unit) {
            let number = number.trim().parse::<f32>().ok()?;
            if number.is_finite() && number >= 0.0 {
                return Some(number * factor);
            }
            return None;
        }
    }
    // A bare number is the legacy `-webkit-device-pixel-ratio` spelling.
    let number = value.parse::<f32>().ok()?;
    if number.is_finite() && number >= 0.0 {
        Some(number)
    } else {
        None
    }
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
    if name.eq_ignore_ascii_case("container") {
        return expand_container_shorthand(value);
    }
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

/// Expand the `container` shorthand (CSS Containment 3 §3.3).
///
/// `container: card / inline-size` sets both longhands; omitting the type
/// resets it to `normal`, and `none` names nothing. A value the shorthand
/// grammar rejects is left unexpanded at its original name, so the declaration
/// simply does not apply and no invented longhand value reaches the cascade.
fn expand_container_shorthand(value: &str) -> Vec<(String, String)> {
    let unexpanded = || vec![("container".to_owned(), value.to_owned())];
    if css_wide_keyword(value).is_some() {
        return vec![
            ("container-name".to_owned(), value.to_owned()),
            ("container-type".to_owned(), value.to_owned()),
        ];
    }
    let mut parts = value.split('/');
    let name_part = parts.next().unwrap_or_default().trim();
    let type_part = parts.next().map(str::trim);
    if parts.next().is_some() {
        return unexpanded();
    }
    let container_type = match type_part {
        None => "normal".to_owned(),
        Some(part) => {
            let lower = part.to_ascii_lowercase();
            if part.is_empty() || !matches!(lower.as_str(), "normal" | "size" | "inline-size") {
                return unexpanded();
            }
            lower
        }
    };
    if name_part.is_empty() {
        return unexpanded();
    }
    let names = if name_part.eq_ignore_ascii_case("none") {
        "none".to_owned()
    } else {
        let mut names = Vec::new();
        for name in name_part.split_ascii_whitespace() {
            if !super::container::is_container_name(name) {
                return unexpanded();
            }
            names.push(name.to_owned());
        }
        names.join(" ")
    };
    vec![
        ("container-name".to_owned(), names),
        ("container-type".to_owned(), container_type),
    ]
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
        && name != "columns"
        && name != "animation"
        && name != "transition"
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
        } else if name == "columns" {
            vec!["column-width", "column-count"]
        } else if name == "animation" {
            vec![
                "animation-name",
                "animation-duration",
                "animation-timing-function",
                "animation-delay",
                "animation-iteration-count",
                "animation-direction",
                "animation-fill-mode",
                "animation-play-state",
            ]
        } else if name == "transition" {
            vec![
                "transition-property",
                "transition-duration",
                "transition-timing-function",
                "transition-delay",
                "transition-behavior",
            ]
        } else {
            vec!["overflow-x", "overflow-y"]
        };
        return longhands
            .into_iter()
            .map(|longhand| (longhand.to_owned(), value.to_owned()))
            .collect();
    }
    match name {
        "columns" => expand_columns_shorthand(value).map_or_else(
            || vec![(name.to_owned(), value.to_owned())],
            |(width, count)| {
                vec![
                    ("column-width".to_owned(), width),
                    ("column-count".to_owned(), count),
                ]
            },
        ),
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
        "animation" => expand_animation_shorthand(value)
            .unwrap_or_else(|| vec![(name.to_owned(), value.to_owned())]),
        "transition" => expand_transition_shorthand(value)
            .unwrap_or_else(|| vec![(name.to_owned(), value.to_owned())]),
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

/// Expand the `animation` shorthand into its eight longhands.
///
/// The first `<time>` is the duration and the second is the delay (CSS
/// Animations 1 §3.1). A function component (`cubic-bezier(...)`,
/// `steps(...)`) is one token, which is why the splitter is bracket-aware.
fn expand_animation_shorthand(value: &str) -> Option<Vec<(String, String)>> {
    let layers = split_top_level_commas(value);
    if layers.is_empty() {
        return None;
    }
    let mut names = Vec::new();
    let mut durations = Vec::new();
    let mut timings = Vec::new();
    let mut delays = Vec::new();
    let mut iterations = Vec::new();
    let mut directions = Vec::new();
    let mut fills = Vec::new();
    let mut plays = Vec::new();
    for layer in layers {
        let (name, duration, timing, delay, iteration, direction, fill, play) =
            expand_animation_layer(&layer)?;
        names.push(name);
        durations.push(duration);
        timings.push(timing);
        delays.push(delay);
        iterations.push(iteration);
        directions.push(direction);
        fills.push(fill);
        plays.push(play);
    }
    Some(vec![
        ("animation-name".to_owned(), names.join(", ")),
        ("animation-duration".to_owned(), durations.join(", ")),
        ("animation-timing-function".to_owned(), timings.join(", ")),
        ("animation-delay".to_owned(), delays.join(", ")),
        (
            "animation-iteration-count".to_owned(),
            iterations.join(", "),
        ),
        ("animation-direction".to_owned(), directions.join(", ")),
        ("animation-fill-mode".to_owned(), fills.join(", ")),
        ("animation-play-state".to_owned(), plays.join(", ")),
    ])
}

#[allow(
    clippy::type_complexity,
    reason = "one animation layer expands to the same eight longhand values in order"
)]
fn expand_animation_layer(
    value: &str,
) -> Option<(
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
)> {
    let mut duration = None;
    let mut delay = None;
    let mut timing = None;
    let mut iterations = None;
    let mut direction = None;
    let mut fill = None;
    let mut play = None;
    let mut name = None;
    for component in split_css_components(value) {
        let lower = component.to_ascii_lowercase();
        if is_animation_time(component) {
            if duration.is_none() {
                duration = Some(component);
            } else if delay.is_none() {
                delay = Some(component);
            } else {
                return None;
            }
        } else if is_animation_timing(&lower) {
            timing = Some(component);
        } else if lower == "infinite"
            || lower
                .parse::<f32>()
                .is_ok_and(|number| number.is_finite() && number >= 0.0)
        {
            iterations = Some(component);
        } else if matches!(
            lower.as_str(),
            "normal" | "reverse" | "alternate" | "alternate-reverse"
        ) {
            direction = Some(component);
        } else if matches!(lower.as_str(), "none" | "forwards" | "backwards" | "both") {
            fill = Some(component);
        } else if matches!(lower.as_str(), "running" | "paused") {
            play = Some(component);
        } else if name.is_none() {
            name = Some(component);
        } else {
            return None;
        }
    }
    Some((
        name.unwrap_or("none").to_owned(),
        duration.unwrap_or("0s").to_owned(),
        timing.unwrap_or("ease").to_owned(),
        delay.unwrap_or("0s").to_owned(),
        iterations.unwrap_or("1").to_owned(),
        direction.unwrap_or("normal").to_owned(),
        fill.unwrap_or("none").to_owned(),
        play.unwrap_or("running").to_owned(),
    ))
}

fn is_animation_time(value: &str) -> bool {
    let value = value.trim();
    let number = value.strip_suffix("ms").or_else(|| value.strip_suffix('s'));
    number.is_some_and(|number| {
        number
            .trim()
            .parse::<f32>()
            .is_ok_and(|number| number.is_finite())
    })
}

fn is_animation_timing(value: &str) -> bool {
    matches!(
        value,
        "linear" | "ease" | "ease-in" | "ease-out" | "ease-in-out" | "step-start" | "step-end"
    ) || value.starts_with("cubic-bezier(")
        || value.starts_with("steps(")
        || value.starts_with("linear(")
}

/// Expand `transition: <property> <duration> <timing> <delay>, ...` into its
/// four longhand lists.
///
/// The grammar is order-independent for property/timing, and the first time is
/// the duration while the second is the delay (CSS Transitions 1 §2.4). Each
/// comma-separated layer contributes one entry per longhand, so a later layer
/// is not silently collapsed onto the first.
fn expand_transition_shorthand(value: &str) -> Option<Vec<(String, String)>> {
    let mut properties = Vec::new();
    let mut durations = Vec::new();
    let mut timings = Vec::new();
    let mut delays = Vec::new();
    let mut behaviors = Vec::new();
    for layer in split_top_level_commas(value) {
        let mut property = None;
        let mut duration = None;
        let mut delay = None;
        let mut timing = None;
        let mut behavior = None;
        for component in split_css_components(&layer) {
            let lower = component.to_ascii_lowercase();
            if is_animation_time(component) {
                if duration.is_none() {
                    duration = Some(component);
                } else if delay.is_none() {
                    delay = Some(component);
                } else {
                    return None;
                }
            } else if is_animation_timing(&lower) {
                timing = Some(component);
            } else if matches!(lower.as_str(), "normal" | "allow-discrete") {
                behavior = Some(component);
            } else if property.is_none() {
                property = Some(component);
            } else {
                return None;
            }
        }
        properties.push(property.unwrap_or("all").to_owned());
        durations.push(duration.unwrap_or("0s").to_owned());
        timings.push(timing.unwrap_or("ease").to_owned());
        delays.push(delay.unwrap_or("0s").to_owned());
        behaviors.push(behavior.unwrap_or("normal").to_owned());
    }
    if properties.is_empty() {
        return None;
    }
    Some(vec![
        ("transition-property".to_owned(), properties.join(", ")),
        ("transition-duration".to_owned(), durations.join(", ")),
        ("transition-timing-function".to_owned(), timings.join(", ")),
        ("transition-delay".to_owned(), delays.join(", ")),
        ("transition-behavior".to_owned(), behaviors.join(", ")),
    ])
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
    while let Some((start, open)) = gradient_function_start(&lower, search_from) {
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

/// Find the earliest gradient function at or after `from`, returning the
/// function's start and the byte index of its opening parenthesis.
fn gradient_function_start(value: &str, from: usize) -> Option<(usize, usize)> {
    const FUNCTIONS: [&str; 6] = [
        "repeating-linear-gradient(",
        "repeating-radial-gradient(",
        "repeating-conic-gradient(",
        "linear-gradient(",
        "radial-gradient(",
        "conic-gradient(",
    ];
    FUNCTIONS
        .iter()
        .filter_map(|function| {
            value[from..]
                .find(function)
                .map(|offset| (from + offset, from + offset + function.len() - 1))
        })
        .min_by_key(|(start, _)| *start)
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
            "(width >= 768px)",
            "(768px <= width)",
            "(400px <= width <= 700px)",
            "(400px < width <= 700px)",
            "(width >= 40em)",
            "(min-width: 10vw)",
            "(calc(100px + 5vw) < width)",
            "(aspect-ratio: 16/9)",
            "(aspect-ratio > 1)",
            "(min-resolution: 2dppx)",
            "(resolution >= 192dpi)",
            "(-webkit-device-pixel-ratio: 2)",
            "(prefers-color-scheme: dark)",
            "(prefers-reduced-motion: reduce)",
            "(prefers-contrast: more)",
            "(forced-colors: active)",
            "(hover: hover)",
            "(any-pointer: coarse)",
            "(scripting: enabled)",
            "(display-mode: standalone)",
            "(color-gamut: p3)",
            "(dynamic-range: high)",
            "(update: fast)",
            "(overflow-block: scroll)",
            "(width > 600px) or (height > 400px)",
            "not (hover: none)",
            "((width > 600px) and (height > 400px))",
        ] {
            assert!(
                media_query_list_is_supported(query),
                "{query:?} is evaluated by the cascade and must not be reported unsupported"
            );
        }
    }

    /// A feature the engine genuinely does not evaluate is reported as
    /// unsupported, which is what makes the diagnostic worth reading.
    #[test]
    fn media_features_the_engine_cannot_evaluate_report_as_unsupported() {
        for query in [
            "(device-min-pixel-ratio: 2)",
            "(unknown-feature: 1)",
            "(min-width: 10furlong)",
            "(aspect-ratio: wide)",
            "(hover: sometimes)",
            "(resolution: fast)",
        ] {
            assert!(
                !media_query_list_is_supported(query),
                "{query:?} names something the engine does not evaluate"
            );
        }
        // One unsupported conjunct makes the whole list unsupported even when
        // the other conjunct alone would match.
        assert!(!media_query_list_is_supported(
            "screen and (min-width: 700px) and (unknown-feature: 1)"
        ));
    }

    /// Media Queries 4 §2.4 range syntax, including the value-first and
    /// two-sided forms, compares against the same viewport the colon form does.
    #[test]
    fn media_range_syntax_compares_the_viewport() {
        let context = MatchContext {
            viewport_width: Some(800.0),
            viewport_height: Some(600.0),
            resolution: Some(2.0),
            ..MatchContext::default()
        };
        for (query, expected) in [
            ("(width >= 800px)", true),
            ("(width > 800px)", false),
            ("(width <= 800px)", true),
            ("(width < 800px)", false),
            ("(800px <= width)", true),
            ("(900px <= width)", false),
            ("(400px <= width <= 900px)", true),
            ("(400px <= width <= 700px)", false),
            ("(900px >= width >= 400px)", true),
            ("(700px >= width >= 900px)", false),
            ("(width >= 600px) and (height <= 600px)", true),
            ("(width < 400px) or (height >= 600px)", true),
            ("not (width > 900px)", true),
            ("(aspect-ratio > 1)", true),
            ("(16/9 > aspect-ratio)", true),
            ("(min-resolution: 2dppx)", true),
            ("(resolution >= 192dpi)", true),
            ("(resolution = 2x)", true),
            ("(width >= 40em)", true),
        ] {
            assert_eq!(
                media_query_list_matches(query, &context),
                expected,
                "{query:?}"
            );
        }
    }

    /// Preference and capability features read the environment carried by the
    /// match context instead of assuming a desktop browser.
    #[test]
    fn media_environment_features_read_the_match_context() {
        use crate::selector::{
            ColorScheme, ContrastPreference, DisplayMode, Hover, Pointer, ReducedPreference,
            Scripting,
        };

        let context = MatchContext {
            prefers_color_scheme: ColorScheme::Dark,
            prefers_reduced_motion: ReducedPreference::Reduce,
            prefers_contrast: ContrastPreference::More,
            forced_colors: true,
            pointer: Pointer::Coarse,
            any_pointer: Pointer::Fine,
            hover: Hover::None,
            any_hover: Hover::Hover,
            scripting: Scripting::None,
            display_mode: DisplayMode::Standalone,
            ..MatchContext::default()
        };
        for (query, expected) in [
            ("(prefers-color-scheme: dark)", true),
            ("(prefers-color-scheme: light)", false),
            ("(prefers-reduced-motion: reduce)", true),
            ("(prefers-reduced-motion: no-preference)", false),
            ("(prefers-contrast: more)", true),
            ("(forced-colors: active)", true),
            ("(forced-colors: none)", false),
            ("(pointer: coarse)", true),
            ("(pointer: fine)", false),
            ("(any-pointer: fine)", true),
            ("(hover: none)", true),
            ("(any-hover: hover)", true),
            ("(scripting: none)", true),
            ("(scripting: enabled)", false),
            ("(display-mode: standalone)", true),
            ("(display-mode: browser)", false),
        ] {
            assert_eq!(
                media_query_list_matches(query, &context),
                expected,
                "{query:?}"
            );
        }
    }

    /// Range syntax is supported even when the host has not supplied a
    /// viewport yet; it simply cannot match until one exists.
    #[test]
    fn media_range_syntax_is_supported_before_the_viewport_is_known() {
        for query in [
            "(width >= 768px)",
            "(400px <= width <= 700px)",
            "(aspect-ratio > 1)",
            "(resolution >= 2dppx)",
        ] {
            assert!(media_query_list_is_supported(query), "{query:?}");
            assert!(
                !media_query_list_matches(query, &MatchContext::default()),
                "{query:?} must not match without the environment it compares against"
            );
        }
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
    fn background_shorthand_keeps_conic_and_repeating_gradients() {
        let (dom, target) = document_and_target();
        let sheet = parse_stylesheet(
            "#target { background: conic-gradient(from 45deg at 25% 25%, red, blue) }",
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
                .get("background-image")
                .map(|value| value.value.as_str()),
            Some("conic-gradient(from 45deg at 25% 25%, red, blue)")
        );

        let sheet = parse_stylesheet(
            "#target { background: repeating-linear-gradient(90deg, red 10% 20%, blue) }",
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
                .get("background-image")
                .map(|value| value.value.as_str()),
            Some("repeating-linear-gradient(90deg, red 10% 20%, blue)")
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
    #[test]
    fn animation_shorthand_expands_duration_delay_and_easing() {
        let expansion: std::collections::BTreeMap<_, _> = super::expand_shorthand(
            "animation",
            "wave .5s cubic-bezier(.1, .2, .3, 1) 100ms alternate infinite both",
        )
        .into_iter()
        .collect();
        assert_eq!(expansion["animation-name"], "wave");
        assert_eq!(expansion["animation-duration"], ".5s");
        assert_eq!(expansion["animation-delay"], "100ms");
        assert_eq!(
            expansion["animation-timing-function"],
            "cubic-bezier(.1, .2, .3, 1)"
        );
        assert_eq!(expansion["animation-direction"], "alternate");
        assert_eq!(expansion["animation-iteration-count"], "infinite");
        assert_eq!(expansion["animation-fill-mode"], "both");
        assert_eq!(expansion["animation-play-state"], "running");
    }
    #[test]
    fn transition_shorthand_expands_each_layer() {
        let expansion: std::collections::BTreeMap<_, _> = super::expand_shorthand(
            "transition",
            "opacity 200ms linear allow-discrete, transform 1s cubic-bezier(.1, .2, .3, 1) 100ms",
        )
        .into_iter()
        .collect();
        assert_eq!(expansion["transition-property"], "opacity, transform");
        assert_eq!(expansion["transition-duration"], "200ms, 1s");
        assert_eq!(
            expansion["transition-timing-function"],
            "linear, cubic-bezier(.1, .2, .3, 1)"
        );
        assert_eq!(expansion["transition-delay"], "0s, 100ms");
        assert_eq!(expansion["transition-behavior"], "allow-discrete, normal");
    }
    #[test]
    fn animation_shorthand_expands_each_layer() {
        let expansion: std::collections::BTreeMap<_, _> = super::expand_shorthand(
            "animation",
            "fade 1s linear both, slide 2s ease-in 100ms infinite alternate",
        )
        .into_iter()
        .collect();
        assert_eq!(expansion["animation-name"], "fade, slide");
        assert_eq!(expansion["animation-duration"], "1s, 2s");
        assert_eq!(expansion["animation-timing-function"], "linear, ease-in");
        assert_eq!(expansion["animation-delay"], "0s, 100ms");
        assert_eq!(expansion["animation-iteration-count"], "1, infinite");
        assert_eq!(expansion["animation-direction"], "normal, alternate");
        assert_eq!(expansion["animation-fill-mode"], "both, none");
    }
}
