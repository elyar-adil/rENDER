//! Selectors Level 3/4 parsing, specificity, and DOM matching.

use std::borrow::Cow;
use std::collections::HashSet;
use std::error::Error;
use std::fmt;

// This module has its own character-level `Parser`; the tokenizer's is
// aliased so the nesting-selector substitution can reach it.
use cssparser::{CowRcStr, ParseError, Parser as CssParser, ParserInput, Token};
use render_dom::{Dom, ElementData, Namespace, NodeId, NodeKind};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Specificity {
    pub ids: u32,
    pub classes: u32,
    pub types: u32,
}

impl Specificity {
    const fn id() -> Self {
        Self {
            ids: 1,
            classes: 0,
            types: 0,
        }
    }

    const fn class() -> Self {
        Self {
            ids: 0,
            classes: 1,
            types: 0,
        }
    }

    const fn type_selector() -> Self {
        Self {
            ids: 0,
            classes: 0,
            types: 1,
        }
    }

    fn add(self, other: Self) -> Self {
        Self {
            ids: self.ids.saturating_add(other.ids),
            classes: self.classes.saturating_add(other.classes),
            types: self.types.saturating_add(other.types),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectorParseError {
    pub offset: usize,
    pub message: String,
}

impl SelectorParseError {
    fn new(offset: usize, message: impl Into<String>) -> Self {
        Self {
            offset,
            message: message.into(),
        }
    }
}

impl fmt::Display for SelectorParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at byte {}", self.message, self.offset)
    }
}

impl Error for SelectorParseError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectorList {
    selectors: Vec<ComplexSelector>,
}

impl SelectorList {
    #[must_use]
    pub fn selectors(&self) -> &[ComplexSelector] {
        &self.selectors
    }

    #[must_use]
    pub fn max_specificity(&self) -> Specificity {
        self.selectors
            .iter()
            .map(ComplexSelector::specificity)
            .max()
            .unwrap_or_default()
    }

    /// Whether any selector in the list names a pseudo-element, including one
    /// nested inside a functional pseudo-class such as `:is()`.
    ///
    /// CSS Cascade 6 §3.5.2 makes a pseudo-element invalid in `<scope-start>`
    /// and `<scope-end>`: a pseudo-element is not a node, so it cannot be a
    /// scoping root or a scoping limit.
    #[must_use]
    pub fn has_any_pseudo_element(&self) -> bool {
        self.selectors
            .iter()
            .any(ComplexSelector::has_any_pseudo_element)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComplexSelector {
    compounds: Vec<CompoundSelector>,
    combinators: Vec<Combinator>,
}

impl ComplexSelector {
    /// Whether any compound in this selector names `pseudo` as a
    /// non-functional pseudo-element. Used to skip generated-content work on
    /// documents whose stylesheets never mention `::before`/`::after`.
    #[must_use]
    pub fn has_pseudo_element(&self, pseudo: &str) -> bool {
        self.compounds.iter().any(|compound| {
            compound.pseudo_element.as_ref().is_some_and(|element| {
                element.argument.is_none() && element.name.eq_ignore_ascii_case(pseudo)
            })
        })
    }

    /// Whether this selector names a pseudo-element anywhere in its compounds,
    /// including inside a functional pseudo-class.
    fn has_any_pseudo_element(&self) -> bool {
        self.compounds
            .iter()
            .any(CompoundSelector::has_any_pseudo_element)
    }

    #[must_use]
    pub fn specificity(&self) -> Specificity {
        self.compounds
            .iter()
            .fold(Specificity::default(), |total, compound| {
                total.add(compound.specificity())
            })
    }

    /// Appends the rightmost compound's id, class and type keys.
    ///
    /// An empty result means the selector has no subject key (a universal,
    /// attribute-only or pseudo-only subject) and must always be tried.
    pub(crate) fn subject_keys(&self, keys: &mut Vec<SubjectKey>) {
        let Some(compound) = self.compounds.last() else {
            return;
        };
        if let Some(name) = compound
            .type_selector
            .as_ref()
            .and_then(|selector| selector.name.as_ref())
        {
            keys.push(SubjectKey::Type(name.to_ascii_lowercase()));
        }
        for simple in &compound.simple {
            match simple {
                SimpleSelector::Id(id) => keys.push(SubjectKey::Id(id.to_ascii_lowercase())),
                SimpleSelector::Class(class) => {
                    keys.push(SubjectKey::Class(class.to_ascii_lowercase()));
                }
                SimpleSelector::Attribute(attribute) => {
                    keys.push(SubjectKey::Attribute(attribute.name.to_ascii_lowercase()));
                }
                SimpleSelector::Pseudo(
                    PseudoClass::Is(selectors) | PseudoClass::Where(selectors),
                ) => {
                    for selector in selectors.selectors() {
                        selector.subject_keys(keys);
                    }
                }
                SimpleSelector::Pseudo(
                    PseudoClass::NthChild(expression) | PseudoClass::NthLastChild(expression),
                ) => {
                    if let Some(selectors) = expression.of.as_ref() {
                        for selector in selectors.selectors() {
                            selector.subject_keys(keys);
                        }
                    }
                }
                SimpleSelector::Pseudo(_) => {}
            }
        }
        keys.sort_unstable();
        keys.dedup();
    }

    /// The id, class, type and attribute-name keys of every compound to the
    /// left of the subject.
    ///
    /// Used as a cheap necessary condition for selectors whose subject is
    /// universal: such a selector can only match an element that has an
    /// ancestor carrying each of these keys. Sibling combinators are not
    /// representable as an ancestor condition, so a selector that uses one
    /// yields no keys and stays in the unkeyed bucket.
    pub(crate) fn ancestor_keys(&self, keys: &mut Vec<SubjectKey>) {
        if self.compounds.len() < 2
            || self.combinators.iter().any(|combinator| {
                matches!(
                    combinator,
                    Combinator::NextSibling | Combinator::SubsequentSibling
                )
            })
        {
            return;
        }
        for compound in &self.compounds[..self.compounds.len() - 1] {
            if let Some(name) = compound
                .type_selector
                .as_ref()
                .and_then(|selector| selector.name.as_ref())
            {
                keys.push(SubjectKey::Type(name.to_ascii_lowercase()));
            }
            for simple in &compound.simple {
                match simple {
                    SimpleSelector::Id(id) => keys.push(SubjectKey::Id(id.to_ascii_lowercase())),
                    SimpleSelector::Class(class) => {
                        keys.push(SubjectKey::Class(class.to_ascii_lowercase()));
                    }
                    SimpleSelector::Attribute(attribute) => {
                        keys.push(SubjectKey::Attribute(attribute.name.to_ascii_lowercase()));
                    }
                    SimpleSelector::Pseudo(_) => {}
                }
            }
        }
        keys.sort_unstable();
        keys.dedup();
    }

    /// Whether this selector is exactly the `:root` pseudo-class.
    ///
    /// A bare `:root` has no id, class, type or attribute key, so the cascade
    /// index would otherwise test it against every element even though only the
    /// tree root can match. On a large stylesheet with design-token `:root`
    /// rules that is one wasted match per element per rule.
    pub(crate) fn is_bare_root(&self) -> bool {
        self.compounds.len() == 1
            && self.compounds[0]
                .type_selector
                .as_ref()
                .is_none_or(|selector| selector.name.is_none())
            && self.compounds[0].pseudo_element.is_none()
            && matches!(
                self.compounds[0].simple.as_slice(),
                [SimpleSelector::Pseudo(PseudoClass::Root)]
            )
    }

    /// Whether this selector matches every element without inspecting it:
    /// a single compound with a universal or absent type selector and no
    /// simple selectors or pseudo-element.
    pub(crate) fn is_unconditional_universal(&self) -> bool {
        self.compounds.len() == 1
            && self.compounds[0]
                .type_selector
                .as_ref()
                .is_none_or(|selector| selector.name.is_none())
            && self.compounds[0].simple.is_empty()
            && self.compounds[0].pseudo_element.is_none()
    }

    /// The pseudo-element the rightmost compound selects, if any.
    pub(crate) fn subject_pseudo(&self) -> SubjectPseudo<'_> {
        match self
            .compounds
            .last()
            .and_then(|compound| compound.pseudo_element.as_ref())
        {
            None => SubjectPseudo::None,
            // `::slotted()` is an alias for the slotted element itself, not a
            // generated box, so its declarations are collected in the ordinary
            // element context (CSS Shadow 1 §3.2.4).
            Some(element) if element.name.eq_ignore_ascii_case("slotted") => SubjectPseudo::None,
            Some(element) if element.argument.is_some() => SubjectPseudo::Never,
            Some(element) => SubjectPseudo::Named(&element.name),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combinator {
    Descendant,
    Child,
    NextSibling,
    SubsequentSibling,
}

/// A key on the rightmost compound of a selector.
///
/// The subject of a selector is its rightmost compound, so an element that
/// lacks every id, class and type mentioned there cannot match. The cascade
/// uses these keys to visit only rules that could possibly apply instead of
/// every rule in every stylesheet. Keys are lowercased: a collision only adds a
/// candidate that the real match then rejects, never removes one.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum SubjectKey {
    Id(String),
    Class(String),
    Type(String),
    Attribute(String),
}

/// The rightmost compound's pseudo-element, if any.
///
/// `None` and `Named` select differently; `Never` marks a functional
/// pseudo-element (`::view-transition-new(root)`), which this renderer never
/// generates and which therefore cannot match in any context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SubjectPseudo<'a> {
    None,
    Named(&'a str),
    Never,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CompoundSelector {
    type_selector: Option<TypeSelector>,
    simple: Vec<SimpleSelector>,
    pseudo_element: Option<PseudoElementSelector>,
}

/// A pseudo-element, optionally in functional notation such as
/// `::view-transition-new(root)` or `::highlight(name)`.
///
/// Selectors 4 §3.9: unknown pseudo-elements are syntactically valid and
/// simply match nothing. The same holds for functional pseudo-elements this
/// renderer does not generate (view transitions, custom highlights, shadow
/// DOM parts): the argument is preserved but the compound can never match,
/// because [`MatchContext::pseudo_element`] carries no argument.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PseudoElementSelector {
    name: String,
    argument: Option<String>,
    /// The parsed `<compound-selector>` of `::slotted()`. CSS Shadow 1 §3.2.4
    /// makes the argument a compound selector, so it is parsed once here
    /// rather than at match time; a selector whose argument is invalid is
    /// dropped with the rule that carried it.
    slotted: Option<SelectorList>,
}

impl CompoundSelector {
    fn has_any_pseudo_element(&self) -> bool {
        self.pseudo_element.is_some()
            || self.simple.iter().any(|simple| match simple {
                SimpleSelector::Pseudo(pseudo) => pseudo.has_any_pseudo_element(),
                _ => false,
            })
    }

    fn specificity(&self) -> Specificity {
        let mut result = if self
            .type_selector
            .as_ref()
            .is_some_and(|selector| selector.name.is_some())
        {
            Specificity::type_selector()
        } else {
            Specificity::default()
        };
        for selector in &self.simple {
            result = result.add(selector.specificity());
        }
        if let Some(pseudo_element) = &self.pseudo_element {
            result = result.add(Specificity::type_selector());
            // CSS Shadow 1 §3.2.4: the specificity of `::slotted()` is that of
            // a pseudo-element plus the specificity of its argument.
            if let Some(argument) = &pseudo_element.slotted {
                result = result.add(argument.max_specificity());
            }
        }
        result
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TypeSelector {
    name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SimpleSelector {
    Id(String),
    Class(String),
    Attribute(AttributeSelector),
    Pseudo(PseudoClass),
}

impl SimpleSelector {
    fn specificity(&self) -> Specificity {
        match self {
            Self::Id(_) => Specificity::id(),
            Self::Class(_) | Self::Attribute(_) => Specificity::class(),
            Self::Pseudo(pseudo) => pseudo.specificity(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttributeOperator {
    Exists,
    Equals,
    Includes,
    DashMatch,
    Prefix,
    Suffix,
    Substring,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaseSensitivity {
    DocumentDefault,
    AsciiInsensitive,
    Sensitive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AttributeSelector {
    name: String,
    operator: AttributeOperator,
    value: String,
    case_sensitivity: CaseSensitivity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PseudoClass {
    Root,
    Scope,
    Empty,
    FirstChild,
    LastChild,
    OnlyChild,
    FirstOfType,
    LastOfType,
    OnlyOfType,
    NthChild(NthExpression),
    NthLastChild(NthExpression),
    NthOfType(NthExpression),
    NthLastOfType(NthExpression),
    Is(SelectorList),
    Where(SelectorList),
    Not(SelectorList),
    Has(Vec<RelativeSelector>),
    Link,
    AnyLink,
    Visited,
    Enabled,
    Disabled,
    Checked,
    PlaceholderShown,
    Focus,
    FocusVisible,
    FocusWithin,
    Hover,
    Active,
    Target,
    Lang(Vec<String>),
    /// `:host` and `:host(<compound-selector>)` (CSS Shadow 1 §3.2.3). The
    /// argument is `None` for the non-functional form.
    Host(Option<SelectorList>),
    /// `:host-context(<compound-selector>)` (CSS Shadow 1 §3.2.3).
    HostContext(SelectorList),
    /// Vendor and shadow-DOM pseudo-classes that this renderer does not
    /// expose yet.  They remain syntactically valid and simply do not match
    /// in the current document tree, as a browser does outside that context.
    Unsupported,
}

impl PseudoClass {
    fn has_any_pseudo_element(&self) -> bool {
        match self {
            Self::Is(list) | Self::Where(list) | Self::Not(list) => list.has_any_pseudo_element(),
            Self::Has(relative) => relative
                .iter()
                .any(|selector| selector.selector.has_any_pseudo_element()),
            Self::NthChild(expression) | Self::NthLastChild(expression) => expression
                .of
                .as_ref()
                .is_some_and(SelectorList::has_any_pseudo_element),
            _ => false,
        }
    }

    fn specificity(&self) -> Specificity {
        match self {
            Self::Where(_) => Specificity::default(),
            Self::Is(list) | Self::Not(list) => list.max_specificity(),
            Self::Has(relative) => relative
                .iter()
                .map(|selector| selector.selector.specificity())
                .max()
                .unwrap_or_default(),
            // CSS Shadow 1 §3.2.3: `:host()` and `:host-context()` are a
            // pseudo-class plus the specificity of their argument. Unlike
            // `:is()`/`:not()` they do not discard the pseudo-class's own
            // class-level weight.
            Self::Host(argument) => Specificity::class().add(
                argument
                    .as_ref()
                    .map_or_else(Specificity::default, SelectorList::max_specificity),
            ),
            Self::HostContext(argument) => Specificity::class().add(argument.max_specificity()),
            Self::NthChild(expression) | Self::NthLastChild(expression) => Specificity::class()
                .add(
                    expression
                        .of
                        .as_ref()
                        .map_or_else(Specificity::default, SelectorList::max_specificity),
                ),
            _ => Specificity::class(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NthExpression {
    a: i32,
    b: i32,
    of: Option<SelectorList>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RelativeSelector {
    leading: Combinator,
    selector: ComplexSelector,
}

#[derive(Clone, Debug, Default)]
pub struct MatchContext {
    pub scope: Option<NodeId>,
    /// The root of the node tree whose stylesheet selectors are being matched
    /// (CSS Shadow 1 §3.2.1). `None` is the document tree; `Some(root)` is a
    /// shadow tree rooted at that shadow root.
    pub tree_root: Option<NodeId>,
    /// The shadow host when matching in the context of its shadow tree. The
    /// host is featureless in its own shadow trees (CSS Shadow 1 §3.2.2):
    /// only the `:host` family and `:scope` (when it is the scoping root) may
    /// match it, and selector ancestry crosses the shadow root to reach it.
    pub shadow_host: Option<NodeId>,
    /// The light-DOM element currently being styled through `::slotted()`,
    /// when a slotted rule from a shadow tree is being matched. The selector
    /// subject on the DOM side is the slot the element is assigned to, which
    /// is why matching runs against the slot and not against this element.
    pub slotted_element: Option<NodeId>,
    pub quirks_mode: bool,
    pub pseudo_element: Option<String>,
    pub focused: Option<NodeId>,
    /// The focused element when the user agent decides its focus ring should
    /// show. `:focus` matches on `focused` alone; `:focus-visible` needs both.
    pub focus_visible: Option<NodeId>,
    pub target: Option<NodeId>,
    pub hovered: HashSet<NodeId>,
    pub active: HashSet<NodeId>,
    pub visited_links: HashSet<NodeId>,
    pub viewport_width: Option<f32>,
    pub viewport_height: Option<f32>,
    /// Device pixel ratio in `dppx`. `None` means the host has not supplied
    /// one, which makes resolution features unknown rather than false.
    pub resolution: Option<f32>,
    pub prefers_color_scheme: ColorScheme,
    pub prefers_reduced_motion: ReducedPreference,
    pub prefers_reduced_transparency: ReducedPreference,
    pub prefers_reduced_data: ReducedPreference,
    pub prefers_contrast: ContrastPreference,
    /// `forced-colors: active`.
    pub forced_colors: bool,
    /// `inverted-colors: inverted`.
    pub inverted_colors: bool,
    pub pointer: Pointer,
    pub any_pointer: Pointer,
    pub hover: Hover,
    pub any_hover: Hover,
    pub scripting: Scripting,
    pub display_mode: DisplayMode,
    pub color_gamut: ColorGamut,
    pub dynamic_range: DynamicRange,
    pub video_dynamic_range: DynamicRange,
    pub update: UpdateFrequency,
    pub overflow_block: OverflowBlock,
    pub overflow_inline: OverflowInline,
    /// Bits per color channel available to the page.
    pub color_bits: u8,
    /// Bits per pixel of a monochrome device; zero for color devices.
    pub monochrome_bits: u8,
    /// Entries in the color palette; zero for direct-color devices.
    pub color_index: u32,
    /// Measured query containers from the previous layout pass, keyed by the
    /// element that establishes each one. Empty on the first pass, which makes
    /// every size query false until geometry exists.
    pub containers: std::sync::Arc<crate::container::ContainerContext>,
}

/// The user's preferred color scheme (`prefers-color-scheme`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorScheme {
    #[default]
    Light,
    Dark,
}

/// A binary user preference such as reduced motion or reduced transparency.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReducedPreference {
    #[default]
    NoPreference,
    Reduce,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContrastPreference {
    #[default]
    NoPreference,
    More,
    Less,
    Custom,
}

/// The primary pointing device's accuracy (`pointer`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pointer {
    None,
    Coarse,
    #[default]
    Fine,
}

/// The primary pointing device's hover ability (`hover`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hover {
    None,
    #[default]
    Hover,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scripting {
    None,
    InitialOnly,
    #[default]
    Enabled,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DisplayMode {
    #[default]
    Browser,
    Fullscreen,
    Standalone,
    MinimalUi,
    WindowControlsOverlay,
    PictureInPicture,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorGamut {
    #[default]
    Srgb,
    P3,
    Rec2020,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DynamicRange {
    #[default]
    Standard,
    High,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpdateFrequency {
    None,
    Slow,
    #[default]
    Fast,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverflowBlock {
    None,
    #[default]
    Scroll,
    Paged,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverflowInline {
    None,
    #[default]
    Scroll,
}

/// The host's user-interaction state for dynamic pseudo-classes.
///
/// This is deliberately narrower than [`MatchContext`]: a frontend supplies
/// only what it knows about the user, and the render pipeline derives the
/// viewport, quirks mode, and stylesheet scope itself. `hovered` and `active`
/// are sets rather than single nodes because those pseudo-classes also match
/// the element's ancestors while the pointer is over a descendant.
/// `focus_visible` points at the element the user agent would draw a focus
/// ring for: the focused element after keyboard navigation, or a text-entry
/// control that was focused by a pointer press.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InteractionState {
    pub hovered: HashSet<NodeId>,
    pub active: HashSet<NodeId>,
    pub focused: Option<NodeId>,
    pub focus_visible: Option<NodeId>,
    pub target: Option<NodeId>,
}

/// Parse a strict selector list. Any invalid selector invalidates the list.
///
/// # Errors
///
/// Returns [`SelectorParseError`] for invalid syntax or unsupported
/// pseudo-classes. Forgiving parsing is used internally only where Selectors 4
/// requires it, such as `:is()` and `:where()`.
pub fn parse_selector_list(input: &str) -> Result<SelectorList, SelectorParseError> {
    parse_list(input, false)
}

/// A nested style rule's prelude with its nesting selector already resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NestedSelectors {
    /// The absolutized selector text. Every `&` is gone, so a deeper nesting
    /// level can use this as its own parent and the levels stack.
    pub css: String,
    pub selectors: SelectorList,
}

/// Resolve a nested style rule's `<relative-selector-list>` against the
/// enclosing style rule's selector text (`CSS Nesting §3.1`).
///
/// §4 defines the nesting selector as "the parent style rule's selector,
/// wrapped in an `:is()` selector", and §4 gives it `:is()`'s specificity: the
/// largest specificity in the parent list. Substituting `:is(<parent>)` for
/// every `&` therefore gets both the matching and the specificity right by
/// reusing the existing `:is()` machinery, and it does not desugar to a
/// cross product, which is what §4's note about selector explosion is about.
///
/// §3.1 also makes a relative selector imply a leading `&`, while a selector
/// that names `&` itself is absolute and gets no implied one. So `> .bar`
/// becomes `:is(parent) > .bar`, `& > .bar` becomes the same thing, and
/// `+ .bar + &` keeps its trailing `&` *and* gains the implied leading one,
/// exactly as §3.2 requires.
///
/// # Errors
///
/// Returns [`SelectorParseError`] when the resolved selector list is invalid,
/// which drops the nested rule and its contents without affecting the parent.
pub fn parse_nested_selector_list(
    input: &str,
    parent: &str,
) -> Result<NestedSelectors, SelectorParseError> {
    let nesting = format!(":is({parent})");
    let mut resolved = String::with_capacity(input.len() + nesting.len());
    let mut named_nesting = false;
    let mut source = ParserInput::new(input);
    let mut parser = CssParser::new(&mut source);
    substitute_nesting(&mut parser, &nesting, &mut resolved, &mut named_nesting)?;
    // A selector that names `&` is absolute unless it also starts with a
    // combinator, in which case §3.1 still implies the leading `&`.
    let css = if named_nesting && !starts_with_combinator(input) {
        resolved
    } else {
        format!("{nesting} {resolved}")
    };
    Ok(NestedSelectors {
        selectors: parse_selector_list(&css)?,
        css,
    })
}

/// Parse the prelude of a style rule inside an `@scope` block.
///
/// CSS Cascade 6 §3.5.3 makes those selectors relative to the scoping root by
/// default. A selector that does not begin with a combinator and does not name
/// `:scope` or `&` gets an implicit `:where(:scope)` descendant prefix; the
/// `:where()` keeps the scoping root at zero specificity, which is what makes
/// `@scope (#hero) { img {} }` as specific as `:where(#hero) img`. A relative
/// selector beginning with a combinator gets the same zero-specificity anchor,
/// and `&` is the scoping root exactly as §3.5.3 defines it.
pub fn parse_scoped_selector_list(input: &str) -> Result<NestedSelectors, SelectorParseError> {
    let anchored = ":where(:scope)";
    if !contains_scope_pseudo(input) || starts_with_combinator(input) {
        return parse_nested_selector_list(input, anchored);
    }
    let mut resolved = String::with_capacity(input.len() + anchored.len());
    let mut named_nesting = false;
    let mut source = ParserInput::new(input);
    let mut parser = CssParser::new(&mut source);
    substitute_nesting(&mut parser, anchored, &mut resolved, &mut named_nesting)?;
    Ok(NestedSelectors {
        selectors: parse_selector_list(&resolved)?,
        css: resolved,
    })
}

/// Parse a `<scope-start>` for an `@scope` rule nested inside a style rule.
///
/// CSS Cascade 6 §3.5.5 makes the nested rule's prelude relative to its nesting
/// context, so the enclosing style rule's selector is the implied anchor. A
/// selector that names `:scope` suppresses the implied anchor and keeps
/// `:scope` for the enclosing `@scope` rule, exactly as §3.5.3 treats
/// `:scope` in a scoped style rule.
pub fn parse_scope_start_in_style(
    input: &str,
    parent: &str,
) -> Result<SelectorList, SelectorParseError> {
    if !contains_scope_pseudo(input) || starts_with_combinator(input) {
        return parse_nested_selector_list(input, parent).map(|nested| nested.selectors);
    }
    let nesting = format!(":where({parent})");
    let mut resolved = String::with_capacity(input.len() + nesting.len());
    let mut named_nesting = false;
    let mut source = ParserInput::new(input);
    let mut parser = CssParser::new(&mut source);
    substitute_nesting(&mut parser, &nesting, &mut resolved, &mut named_nesting)?;
    parse_selector_list(&resolved)
}

/// Whether a selector names `:scope` outside a string or an attribute selector.
///
/// A `:scope` inside `[data-x=":scope"]` is an attribute value, not a selector,
/// so treating it as one would suppress the implicit scoping prefix for a rule
/// that never asked for it.
fn contains_scope_pseudo(input: &str) -> bool {
    let bytes = input.as_bytes();
    let mut index = 0;
    let mut quote = None;
    let mut bracket_depth = 0_u32;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                index = index.saturating_add(2);
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' | b'\'' => quote = Some(byte),
            b'[' => bracket_depth = bracket_depth.saturating_add(1),
            b']' => bracket_depth = bracket_depth.saturating_sub(1),
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                // A comment is not a selector, and `/* :scope */` must not
                // suppress the implicit scoping prefix.
                let rest = &bytes[index + 2..];
                let end = rest
                    .windows(2)
                    .position(|window| window == b"*/")
                    .map_or(bytes.len(), |offset| index + 2 + offset + 2);
                index = end;
                continue;
            }
            b':' if bracket_depth == 0 => {
                let rest = &input[index..];
                let lower = rest.get(..6).unwrap_or(rest);
                if lower.eq_ignore_ascii_case(":scope") {
                    let next = rest.as_bytes().get(6).copied();
                    if next.is_none_or(|byte| !is_name_character(char::from(byte))) {
                        return true;
                    }
                }
            }
            b'\\' => index = index.saturating_add(1),
            _ => {}
        }
        index += 1;
    }
    false
}

/// One substitution step, holding no borrow of the parser so the block forms can
/// recurse into it.
enum NestingStep<'i> {
    Replace,
    Function(CowRcStr<'i>),
    /// A block that is opaque to selector substitution, still to be consumed.
    Block,
    Literal,
}

fn substitute_nesting<'i, 't>(
    parser: &mut CssParser<'i, 't>,
    nesting: &str,
    output: &mut String,
    named_nesting: &mut bool,
) -> Result<(), SelectorParseError> {
    let mut start = parser.position();
    while let Ok(token) = parser.next_including_whitespace_and_comments() {
        let step = match token {
            Token::Delim('&') => NestingStep::Replace,
            // A functional pseudo-class takes selector arguments, so a `&`
            // inside one is still the nesting selector: `:not(&)` is
            // `:not(:is(parent))`.
            Token::Function(name) => NestingStep::Function(name.clone()),
            // An attribute selector's brackets and a `{}` block are opaque: a
            // `&` inside them is a literal character, not the nesting selector.
            // These are the only tokens `parse_nested_block` accepts, because
            // they are the only ones the tokenizer hands back whole.
            Token::ParenthesisBlock | Token::SquareBracketBlock | Token::CurlyBracketBlock => {
                NestingStep::Block
            }
            // A string and a url are opaque for the same reason - a `&` inside
            // one is a literal character - but they are *not* blocks. The
            // tokenizer has already consumed their contents, so there is
            // nothing inside to substitute and nothing to open. Routing them
            // through `parse_nested_block` asks for a nested parser at a token
            // that did not open one, and cssparser panics on that: so
            // `a { b:not("x") { color: red } }`, a nested rule with an ordinary
            // attribute selector in it, took the process down.
            Token::QuotedString(_) | Token::UnquotedUrl(_) | Token::BadUrl(_) => {
                NestingStep::Literal
            }
            _ => NestingStep::Literal,
        };
        let next = parser.position();
        match step {
            NestingStep::Replace => {
                output.push_str(nesting);
                *named_nesting = true;
            }
            NestingStep::Literal => output.push_str(parser.slice(start..next)),
            NestingStep::Function(name) => {
                output.push_str(&name);
                output.push('(');
                let recursed: Result<(), ParseError<SelectorParseError>> = parser
                    .parse_nested_block(|nested| {
                        substitute_nesting(nested, nesting, output, named_nesting)
                            .map_err(|error| nested.new_custom_error(error))
                    });
                recursed.map_err(|_| {
                    SelectorParseError::new(0, "unterminated functional pseudo-class")
                })?;
                output.push(')');
            }
            NestingStep::Block => {
                let drained: Result<(), ParseError<SelectorParseError>> = parser
                    .parse_nested_block(|nested| {
                        while nested.next_including_whitespace_and_comments().is_ok() {}
                        Ok(())
                    });
                drained.map_err(|_| {
                    SelectorParseError::new(0, "unterminated block in a nested selector")
                })?;
                output.push_str(parser.slice(start..parser.position()));
            }
        }
        start = next;
    }
    Ok(())
}

/// Whether a selector list begins with a combinator, which is what makes it
/// relative (`CSS Selectors 4 §Relative Selectors`).
fn starts_with_combinator(input: &str) -> bool {
    let mut rest = input.trim_start();
    while let Some(after) = rest.strip_prefix("/*") {
        let Some(end) = after.find("*/") else {
            return false;
        };
        rest = after[end + 2..].trim_start();
    }
    rest.starts_with(['>', '+', '~'])
}

/// Return whether an element matches any selector in a parsed list.
#[must_use]
pub fn matches_selector_list(
    dom: &Dom,
    element: NodeId,
    selectors: &SelectorList,
    context: &MatchContext,
) -> bool {
    if !is_element(dom, element) {
        return false;
    }
    selectors
        .selectors
        .iter()
        .any(|selector| matches_complex(dom, element, selector, context))
}

/// Return the greatest specificity among selectors in the list that actually
/// match the element.
///
/// A cascade must not use [`SelectorList::max_specificity`]: a more-specific
/// selector in the same list may not match this element.
#[must_use]
pub fn matching_specificity(
    dom: &Dom,
    element: NodeId,
    selectors: &SelectorList,
    context: &MatchContext,
) -> Option<Specificity> {
    if !is_element(dom, element) {
        return None;
    }
    selectors
        .selectors
        .iter()
        .filter(|selector| matches_complex(dom, element, selector, context))
        .map(ComplexSelector::specificity)
        .max()
}

/// Select matching descendants in tree order. The root itself is not included,
/// matching `Document`/`Element.querySelectorAll()` descendant semantics.
#[must_use]
pub fn select_all(
    dom: &Dom,
    root: NodeId,
    selectors: &SelectorList,
    context: &MatchContext,
) -> Vec<NodeId> {
    let mut result = Vec::new();
    collect_matches(dom, root, selectors, context, &mut result);
    result
}

fn collect_matches(
    dom: &Dom,
    root: NodeId,
    selectors: &SelectorList,
    context: &MatchContext,
    result: &mut Vec<NodeId>,
) {
    for child in dom.children(root).unwrap_or_default() {
        if is_element(dom, *child) && matches_selector_list(dom, *child, selectors, context) {
            result.push(*child);
        }
        collect_matches(dom, *child, selectors, context, result);
    }
}

fn parse_list(input: &str, forgiving: bool) -> Result<SelectorList, SelectorParseError> {
    let mut selectors = Vec::new();
    for (offset, fragment) in split_top_level(input, ',')? {
        let leading = fragment.len() - fragment.trim_start().len();
        let fragment = fragment.trim();
        if fragment.is_empty() {
            if forgiving {
                continue;
            }
            return Err(SelectorParseError::new(
                offset,
                "empty selector in selector list",
            ));
        }
        match Parser::new(fragment, offset + leading).parse_complex(false) {
            Ok(selector) => selectors.push(selector),
            Err(_) if forgiving => {}
            Err(error) => return Err(error),
        }
    }
    if selectors.is_empty() {
        return Err(SelectorParseError::new(
            0,
            "selector list contains no valid selectors",
        ));
    }
    Ok(SelectorList { selectors })
}

/// Parse a single `<compound-selector>` argument, as `:host()`,
/// `:host-context()` and `::slotted()` require (CSS Shadow 1 §3.2.3-§3.2.4).
///
/// The grammar is singular: a comma here is an error rather than a list, and a
/// combinator is an error rather than part of the argument. Returning a
/// [`SelectorList`] keeps specificity and nested `:is()`/`:not()` handling on
/// the same code path as every other selector argument.
fn parse_compound_selector_argument(
    input: &str,
    function: &str,
) -> Result<SelectorList, SelectorParseError> {
    let fragments = split_top_level(input, ',')?;
    if fragments.len() != 1 {
        return Err(SelectorParseError::new(
            0,
            format!("{function} requires exactly one compound selector"),
        ));
    }
    let (offset, fragment) = fragments[0];
    let leading = fragment.len() - fragment.trim_start().len();
    let fragment = fragment.trim();
    if fragment.is_empty() {
        return Err(SelectorParseError::new(
            offset,
            format!("{function} requires a compound selector"),
        ));
    }
    let mut parser = Parser::new(fragment, offset + leading);
    parser.skip_whitespace_and_comments()?;
    let compound = parser.parse_compound()?;
    parser.skip_whitespace_and_comments()?;
    if !parser.is_eof() {
        return Err(parser.error(format!("{function} requires a compound selector")));
    }
    Ok(SelectorList {
        selectors: vec![ComplexSelector {
            compounds: vec![compound],
            combinators: Vec::new(),
        }],
    })
}

fn parse_relative_list(input: &str) -> Result<Vec<RelativeSelector>, SelectorParseError> {
    let mut selectors = Vec::new();
    for (offset, fragment) in split_top_level(input, ',')? {
        let leading = fragment.len() - fragment.trim_start().len();
        let fragment = fragment.trim();
        if fragment.is_empty() {
            continue;
        }
        if let Ok(selector) = Parser::new(fragment, offset + leading).parse_relative() {
            selectors.push(selector);
        }
    }
    if selectors.is_empty() {
        return Err(SelectorParseError::new(
            0,
            ":has() contains no valid relative selectors",
        ));
    }
    Ok(selectors)
}

struct Parser<'a> {
    input: &'a str,
    offset: usize,
    base_offset: usize,
}

impl<'a> Parser<'a> {
    const fn new(input: &'a str, base_offset: usize) -> Self {
        Self {
            input,
            offset: 0,
            base_offset,
        }
    }

    fn parse_relative(mut self) -> Result<RelativeSelector, SelectorParseError> {
        self.skip_whitespace_and_comments()?;
        let leading = match self.peek_char() {
            Some('>') => {
                self.bump_char();
                Combinator::Child
            }
            Some('+') => {
                self.bump_char();
                Combinator::NextSibling
            }
            Some('~') => {
                self.bump_char();
                Combinator::SubsequentSibling
            }
            _ => Combinator::Descendant,
        };
        self.skip_whitespace_and_comments()?;
        let selector = self.parse_complex_from_current()?;
        Ok(RelativeSelector { leading, selector })
    }

    fn parse_complex(mut self, allow_leading: bool) -> Result<ComplexSelector, SelectorParseError> {
        if allow_leading {
            return self.parse_relative().map(|relative| relative.selector);
        }
        self.skip_whitespace_and_comments()?;
        self.parse_complex_from_current()
    }

    fn parse_complex_from_current(&mut self) -> Result<ComplexSelector, SelectorParseError> {
        let mut compounds = vec![self.parse_compound()?];
        let mut combinators = Vec::new();
        loop {
            let had_whitespace = self.skip_whitespace_and_comments()?;
            if self.is_eof() {
                break;
            }
            let combinator = match self.peek_char() {
                Some('>') => {
                    self.bump_char();
                    Combinator::Child
                }
                Some('+') => {
                    self.bump_char();
                    Combinator::NextSibling
                }
                Some('~') => {
                    self.bump_char();
                    Combinator::SubsequentSibling
                }
                _ if had_whitespace => Combinator::Descendant,
                _ => return Err(self.error("expected a selector combinator")),
            };
            self.skip_whitespace_and_comments()?;
            combinators.push(combinator);
            compounds.push(self.parse_compound()?);
        }
        Ok(ComplexSelector {
            compounds,
            combinators,
        })
    }

    fn parse_compound(&mut self) -> Result<CompoundSelector, SelectorParseError> {
        let mut type_selector = None;
        let mut simple = Vec::new();
        let mut pseudo_element = None;

        if self.consume_char('*') {
            type_selector = Some(TypeSelector { name: None });
        } else if self.starts_identifier() {
            type_selector = Some(TypeSelector {
                name: Some(self.parse_identifier()?),
            });
        }

        loop {
            // A comment between two simple selectors is removed during
            // preprocessing, so `.a/*c*/.b` is one compound selector rather
            // than a descendant combinator.
            self.skip_comments()?;
            match self.peek_char() {
                Some('#') => {
                    self.bump_char();
                    simple.push(SimpleSelector::Id(self.parse_identifier()?));
                }
                Some('.') => {
                    self.bump_char();
                    simple.push(SimpleSelector::Class(self.parse_identifier()?));
                }
                Some('[') => simple.push(SimpleSelector::Attribute(self.parse_attribute()?)),
                Some(':') => {
                    self.bump_char();
                    if self.consume_char(':') {
                        if pseudo_element.is_some() {
                            return Err(
                                self.error("compound selector has multiple pseudo-elements")
                            );
                        }
                        pseudo_element = Some(self.parse_pseudo_element()?);
                    } else {
                        let saved = self.offset;
                        let legacy_name = self.parse_identifier()?.to_ascii_lowercase();
                        if self.peek_char() != Some('(') && is_legacy_pseudo_element(&legacy_name) {
                            if pseudo_element.is_some() {
                                return Err(
                                    self.error("compound selector has multiple pseudo-elements")
                                );
                            }
                            pseudo_element = Some(PseudoElementSelector {
                                name: legacy_name,
                                argument: None,
                                slotted: None,
                            });
                        } else {
                            self.offset = saved;
                            simple.push(SimpleSelector::Pseudo(self.parse_pseudo_class()?));
                        }
                    }
                }
                _ => break,
            }
        }

        if type_selector.is_none() && simple.is_empty() && pseudo_element.is_none() {
            return Err(self.error("expected a compound selector"));
        }
        Ok(CompoundSelector {
            type_selector,
            simple,
            pseudo_element,
        })
    }

    fn parse_pseudo_element(&mut self) -> Result<PseudoElementSelector, SelectorParseError> {
        let name = self.parse_identifier()?.to_ascii_lowercase();
        let argument = if self.consume_char('(') {
            let contents = self.consume_function_contents()?;
            if contents.trim().is_empty() {
                return Err(self.error("functional pseudo-element requires an argument"));
            }
            Some(contents.trim().to_owned())
        } else {
            None
        };
        // CSS Shadow 1 §3.2.4: `::slotted()` takes a `<compound-selector>`.
        // Parse it now so an invalid argument drops the rule instead of
        // becoming a selector that silently matches nothing.
        let slotted = match (name.as_str(), argument.as_deref()) {
            ("slotted", Some(contents)) => {
                Some(parse_compound_selector_argument(contents, "::slotted")?)
            }
            _ => None,
        };
        Ok(PseudoElementSelector {
            name,
            argument,
            slotted,
        })
    }

    fn parse_attribute(&mut self) -> Result<AttributeSelector, SelectorParseError> {
        self.expect_char('[')?;
        self.skip_whitespace_and_comments()?;
        let name = self.parse_identifier()?;
        self.skip_whitespace_and_comments()?;
        if self.consume_char(']') {
            return Ok(AttributeSelector {
                name,
                operator: AttributeOperator::Exists,
                value: String::new(),
                case_sensitivity: CaseSensitivity::DocumentDefault,
            });
        }

        let operator = if self.consume_char('=') {
            AttributeOperator::Equals
        } else if self.consume_pair('~', '=') {
            AttributeOperator::Includes
        } else if self.consume_pair('|', '=') {
            AttributeOperator::DashMatch
        } else if self.consume_pair('^', '=') {
            AttributeOperator::Prefix
        } else if self.consume_pair('$', '=') {
            AttributeOperator::Suffix
        } else if self.consume_pair('*', '=') {
            AttributeOperator::Substring
        } else {
            return Err(self.error("expected an attribute selector operator or ']'"));
        };
        self.skip_whitespace_and_comments()?;
        let value = match self.peek_char() {
            Some('"' | '\'') => self.parse_string()?,
            _ if self.starts_identifier() => self.parse_identifier()?,
            _ => return Err(self.error("expected an attribute selector value")),
        };
        self.skip_whitespace_and_comments()?;
        let case_sensitivity = if self.starts_identifier() {
            match self.parse_identifier()?.to_ascii_lowercase().as_str() {
                "i" => CaseSensitivity::AsciiInsensitive,
                "s" => CaseSensitivity::Sensitive,
                _ => return Err(self.error("attribute selector flag must be 'i' or 's'")),
            }
        } else {
            CaseSensitivity::DocumentDefault
        };
        self.skip_whitespace_and_comments()?;
        self.expect_char(']')?;
        Ok(AttributeSelector {
            name,
            operator,
            value,
            case_sensitivity,
        })
    }

    fn parse_pseudo_class(&mut self) -> Result<PseudoClass, SelectorParseError> {
        let name = self.parse_identifier()?.to_ascii_lowercase();
        let argument = if self.consume_char('(') {
            Some(self.consume_function_contents()?)
        } else {
            None
        };
        match (name.as_str(), argument.as_deref()) {
            ("root", None) => Ok(PseudoClass::Root),
            ("scope", None) => Ok(PseudoClass::Scope),
            ("empty", None) => Ok(PseudoClass::Empty),
            ("first-child", None) => Ok(PseudoClass::FirstChild),
            ("last-child", None) => Ok(PseudoClass::LastChild),
            ("only-child", None) => Ok(PseudoClass::OnlyChild),
            ("first-of-type", None) => Ok(PseudoClass::FirstOfType),
            ("last-of-type", None) => Ok(PseudoClass::LastOfType),
            ("only-of-type", None) => Ok(PseudoClass::OnlyOfType),
            ("nth-child", Some(value)) => Ok(PseudoClass::NthChild(parse_nth(value)?)),
            ("nth-last-child", Some(value)) => Ok(PseudoClass::NthLastChild(parse_nth(value)?)),
            ("nth-of-type", Some(value)) => Ok(PseudoClass::NthOfType(parse_nth_of_type(value)?)),
            ("nth-last-of-type", Some(value)) => {
                Ok(PseudoClass::NthLastOfType(parse_nth_of_type(value)?))
            }
            ("is", Some(value)) => Ok(PseudoClass::Is(parse_list(value, true)?)),
            ("where", Some(value)) => Ok(PseudoClass::Where(parse_list(value, true)?)),
            ("not", Some(value)) => Ok(PseudoClass::Not(parse_list(value, false)?)),
            ("has", Some(value)) => Ok(PseudoClass::Has(parse_relative_list(value)?)),
            ("link", None) => Ok(PseudoClass::Link),
            ("any-link", None) => Ok(PseudoClass::AnyLink),
            ("visited", None) => Ok(PseudoClass::Visited),
            ("enabled", None) => Ok(PseudoClass::Enabled),
            ("disabled", None) => Ok(PseudoClass::Disabled),
            ("checked", None) => Ok(PseudoClass::Checked),
            ("placeholder-shown", None) => Ok(PseudoClass::PlaceholderShown),
            ("focus", None) => Ok(PseudoClass::Focus),
            ("focus-visible", None) => Ok(PseudoClass::FocusVisible),
            ("focus-within", None) => Ok(PseudoClass::FocusWithin),
            ("hover", None) => Ok(PseudoClass::Hover),
            ("active", None) => Ok(PseudoClass::Active),
            ("target", None) => Ok(PseudoClass::Target),
            ("lang", Some(value)) => Ok(PseudoClass::Lang(parse_lang_ranges(value)?)),
            ("host", None) => Ok(PseudoClass::Host(None)),
            ("host", Some(value)) => Ok(PseudoClass::Host(Some(parse_compound_selector_argument(
                value, ":host",
            )?))),
            ("host-context", Some(value)) => Ok(PseudoClass::HostContext(
                parse_compound_selector_argument(value, ":host-context")?,
            )),
            ("host-context", None) => {
                Err(self.error(":host-context requires a compound selector argument"))
            }
            _ => Ok(PseudoClass::Unsupported),
        }
    }

    fn consume_function_contents(&mut self) -> Result<String, SelectorParseError> {
        let start = self.offset;
        let mut depth = 1_u32;
        let mut quote = None;
        while let Some(character) = self.peek_char() {
            if let Some(active_quote) = quote {
                self.bump_char();
                if character == '\\' {
                    self.bump_char();
                } else if character == active_quote {
                    quote = None;
                }
                continue;
            }
            match character {
                '"' | '\'' => {
                    quote = Some(character);
                    self.bump_char();
                }
                '\\' => {
                    self.bump_char();
                    self.bump_char();
                }
                '(' => {
                    depth += 1;
                    self.bump_char();
                }
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        let result = self.input[start..self.offset].to_owned();
                        self.bump_char();
                        return Ok(result);
                    }
                    self.bump_char();
                }
                _ => {
                    self.bump_char();
                }
            }
        }
        Err(self.error("unterminated functional pseudo-class"))
    }

    fn parse_identifier(&mut self) -> Result<String, SelectorParseError> {
        let start = self.offset;
        if !self.starts_identifier() {
            return Err(SelectorParseError::new(
                self.base_offset + start,
                "expected a CSS identifier",
            ));
        }
        let mut result = String::new();
        if self.peek_char() == Some('-') {
            result.push('-');
            self.bump_char();
        }
        while let Some(character) = self.peek_char() {
            if is_name_character(character) {
                result.push(character);
                self.bump_char();
            } else if character == '\\' {
                result.push(self.consume_escape()?);
            } else {
                break;
            }
        }
        if result.is_empty() || result == "-" {
            return Err(SelectorParseError::new(
                self.base_offset + start,
                "expected a CSS identifier",
            ));
        }
        Ok(result)
    }

    fn parse_string(&mut self) -> Result<String, SelectorParseError> {
        let quote = self
            .bump_char()
            .ok_or_else(|| self.error("expected a CSS string"))?;
        let mut result = String::new();
        while let Some(character) = self.peek_char() {
            if character == quote {
                self.bump_char();
                return Ok(result);
            }
            if matches!(character, '\n' | '\r' | '\u{000c}') {
                return Err(self.error("newline in CSS string"));
            }
            if character == '\\' {
                self.bump_char();
                if matches!(self.peek_char(), Some('\n' | '\r' | '\u{000c}')) {
                    self.consume_newline();
                } else if self.peek_char().is_some() {
                    result.push(self.consume_escape_after_backslash()?);
                }
            } else {
                result.push(character);
                self.bump_char();
            }
        }
        Err(self.error("unterminated CSS string"))
    }

    fn consume_escape(&mut self) -> Result<char, SelectorParseError> {
        self.expect_char('\\')?;
        self.consume_escape_after_backslash()
    }

    fn consume_escape_after_backslash(&mut self) -> Result<char, SelectorParseError> {
        let Some(character) = self.peek_char() else {
            return Err(self.error("escape at end of selector"));
        };
        if matches!(character, '\n' | '\r' | '\u{000c}') {
            return Err(self.error("escaped newline is not valid in an identifier"));
        }
        if character.is_ascii_hexdigit() {
            let mut value = 0_u32;
            for _ in 0..6 {
                let Some(digit) = self.peek_char().and_then(|next| next.to_digit(16)) else {
                    break;
                };
                value = value.saturating_mul(16).saturating_add(digit);
                self.bump_char();
            }
            if self.peek_char().is_some_and(is_css_whitespace) {
                self.consume_newline_or_space();
            }
            return Ok(char::from_u32(value)
                .filter(|scalar| *scalar != '\0' && !(0xd800..=0xdfff).contains(&value))
                .unwrap_or('\u{fffd}'));
        }
        self.bump_char();
        Ok(character)
    }

    fn starts_identifier(&self) -> bool {
        match self.peek_char() {
            Some(character) if is_name_start(character) || character == '\\' => true,
            Some('-') => self.peek_second_char().is_some_and(|character| {
                is_name_start(character) || character == '-' || character == '\\'
            }),
            _ => false,
        }
    }

    fn skip_comments(&mut self) -> Result<bool, SelectorParseError> {
        let mut skipped = false;
        while self.remaining().starts_with("/*") {
            self.offset += 2;
            let Some(end) = self.remaining().find("*/") else {
                return Err(self.error("unterminated CSS comment"));
            };
            self.offset += end + 2;
            skipped = true;
        }
        Ok(skipped)
    }

    /// Skip comments and whitespace, reporting whether any *whitespace* was
    /// skipped.
    ///
    /// CSS Syntax §4.3.1 removes comments from the token stream while
    /// preprocessing, so a comment is not a descendant combinator (Selectors 4
    /// §4.2 defines descendant as an actual `<whitespace>` token) and it does
    /// not split a compound selector either.
    fn skip_whitespace_and_comments(&mut self) -> Result<bool, SelectorParseError> {
        let mut saw_whitespace = false;
        loop {
            while self.peek_char().is_some_and(is_css_whitespace) {
                self.bump_char();
                saw_whitespace = true;
            }
            if !self.skip_comments()? {
                return Ok(saw_whitespace);
            }
        }
    }

    fn consume_newline(&mut self) {
        if self.consume_char('\r') {
            self.consume_char('\n');
        } else {
            self.bump_char();
        }
    }

    fn consume_newline_or_space(&mut self) {
        if matches!(self.peek_char(), Some('\r' | '\n' | '\u{000c}')) {
            self.consume_newline();
        } else {
            self.bump_char();
        }
    }

    fn expect_char(&mut self, expected: char) -> Result<(), SelectorParseError> {
        if self.consume_char(expected) {
            Ok(())
        } else {
            Err(self.error(format!("expected '{expected}'")))
        }
    }

    fn consume_pair(&mut self, first: char, second: char) -> bool {
        let saved = self.offset;
        if self.consume_char(first) && self.consume_char(second) {
            true
        } else {
            self.offset = saved;
            false
        }
    }

    fn consume_char(&mut self, expected: char) -> bool {
        if self.peek_char() == Some(expected) {
            self.bump_char();
            true
        } else {
            false
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.remaining().chars().next()
    }

    fn peek_second_char(&self) -> Option<char> {
        self.remaining().chars().nth(1)
    }

    fn bump_char(&mut self) -> Option<char> {
        let character = self.peek_char()?;
        self.offset += character.len_utf8();
        Some(character)
    }

    fn remaining(&self) -> &'a str {
        &self.input[self.offset..]
    }

    fn is_eof(&self) -> bool {
        self.offset >= self.input.len()
    }

    fn error(&self, message: impl Into<String>) -> SelectorParseError {
        SelectorParseError::new(self.base_offset + self.offset, message)
    }
}

fn split_top_level(input: &str, delimiter: char) -> Result<Vec<(usize, &str)>, SelectorParseError> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut depth = 0_u32;
    let mut quote = None;
    let mut iterator = input.char_indices().peekable();
    while let Some((offset, character)) = iterator.next() {
        if let Some(active_quote) = quote {
            if character == '\\' {
                iterator.next();
            } else if character == active_quote {
                quote = None;
            }
            continue;
        }
        match character {
            '"' | '\'' => quote = Some(character),
            '\\' => {
                iterator.next();
            }
            '(' | '[' => depth += 1,
            ')' | ']' => {
                if depth == 0 {
                    return Err(SelectorParseError::new(
                        offset,
                        "unmatched closing delimiter",
                    ));
                }
                depth -= 1;
            }
            value if value == delimiter && depth == 0 => {
                result.push((start, &input[start..offset]));
                start = offset + character.len_utf8();
            }
            _ => {}
        }
    }
    if quote.is_some() || depth != 0 {
        return Err(SelectorParseError::new(
            input.len(),
            "unterminated string or block in selector",
        ));
    }
    result.push((start, &input[start..]));
    Ok(result)
}

fn parse_nth(input: &str) -> Result<NthExpression, SelectorParseError> {
    let (formula, of_selector) = split_nth_of(input);
    let normalized: String = formula
        .chars()
        .filter(|character| !is_css_whitespace(*character))
        .flat_map(char::to_lowercase)
        .collect();
    let (a, b) = if normalized == "odd" {
        (2, 1)
    } else if normalized == "even" {
        (2, 0)
    } else if let Some(n_offset) = normalized.find('n') {
        if normalized[n_offset + 1..].contains('n') {
            return Err(SelectorParseError::new(0, "invalid An+B expression"));
        }
        let a = match &normalized[..n_offset] {
            "" | "+" => 1,
            "-" => -1,
            value => value
                .parse::<i32>()
                .map_err(|_| SelectorParseError::new(0, "invalid An coefficient"))?,
        };
        let b = if n_offset + 1 == normalized.len() {
            0
        } else {
            normalized[n_offset + 1..]
                .parse::<i32>()
                .map_err(|_| SelectorParseError::new(0, "invalid B offset"))?
        };
        (a, b)
    } else {
        (
            0,
            normalized
                .parse::<i32>()
                .map_err(|_| SelectorParseError::new(0, "invalid nth index"))?,
        )
    };
    let of = of_selector.map(parse_selector_list).transpose()?;
    Ok(NthExpression { a, b, of })
}

fn parse_nth_of_type(input: &str) -> Result<NthExpression, SelectorParseError> {
    let expression = parse_nth(input)?;
    if expression.of.is_some() {
        return Err(SelectorParseError::new(
            0,
            ":nth-of-type() does not accept an 'of selector' clause",
        ));
    }
    Ok(expression)
}

fn split_nth_of(input: &str) -> (&str, Option<&str>) {
    let bytes = input.as_bytes();
    let mut offset = 0;
    while offset + 1 < bytes.len() {
        if (bytes[offset] == b'o' || bytes[offset] == b'O')
            && (bytes[offset + 1] == b'f' || bytes[offset + 1] == b'F')
            && offset > 0
            && is_css_whitespace(char::from(bytes[offset - 1]))
            && offset + 2 < bytes.len()
            && is_css_whitespace(char::from(bytes[offset + 2]))
        {
            return (&input[..offset], Some(input[offset + 2..].trim()));
        }
        offset += 1;
    }
    (input, None)
}

fn parse_lang_ranges(input: &str) -> Result<Vec<String>, SelectorParseError> {
    let mut ranges = Vec::new();
    for (offset, fragment) in split_top_level(input, ',')? {
        let fragment = fragment.trim();
        if fragment.is_empty() {
            return Err(SelectorParseError::new(offset, "empty :lang() range"));
        }
        let range = if matches!(fragment.chars().next(), Some('"' | '\'')) {
            let mut parser = Parser::new(fragment, offset);
            let value = parser.parse_string()?;
            if !parser.is_eof() {
                return Err(parser.error("trailing input in :lang()"));
            }
            value
        } else {
            let mut parser = Parser::new(fragment, offset);
            let value = parser.parse_identifier()?;
            if !parser.is_eof() {
                return Err(parser.error("trailing input in :lang()"));
            }
            value
        };
        ranges.push(range);
    }
    Ok(ranges)
}

fn matches_complex(
    dom: &Dom,
    element: NodeId,
    selector: &ComplexSelector,
    context: &MatchContext,
) -> bool {
    if !subject_may_match(dom, element, selector, context) {
        return false;
    }
    matches_complex_at(
        dom,
        element,
        selector,
        selector.compounds.len() - 1,
        context,
    )
}

/// Cheap rejection before walking a selector's combinators.
///
/// The rightmost compound names the subject, so a mismatching type, id or
/// class cannot match regardless of the ancestors. This avoids the expensive
/// `:has`/`:not`/descendant walk for the overwhelming majority of rules on a
/// large page; every rejection here is exact (no speculative exclusions).
fn subject_may_match(
    dom: &Dom,
    element: NodeId,
    selector: &ComplexSelector,
    context: &MatchContext,
) -> bool {
    let Some(compound) = selector.compounds.last() else {
        return false;
    };
    let Some(data) = element_data(dom, element) else {
        return false;
    };
    if let Some(type_selector) = &compound.type_selector
        && let Some(expected) = &type_selector.name
    {
        let matches = if data.namespace == Namespace::Html {
            data.local_name.eq_ignore_ascii_case(expected)
        } else {
            data.local_name == *expected
        };
        if !matches {
            return false;
        }
    }
    for simple in &compound.simple {
        match simple {
            SimpleSelector::Id(expected) => {
                let matches = attribute_value(data, "id").is_some_and(|actual| {
                    if context.quirks_mode {
                        actual.eq_ignore_ascii_case(expected)
                    } else {
                        actual == expected
                    }
                });
                if !matches {
                    return false;
                }
            }
            SimpleSelector::Class(expected) => {
                let matches = attribute_value(data, "class").is_some_and(|classes| {
                    classes.split_ascii_whitespace().any(|actual| {
                        if context.quirks_mode {
                            actual.eq_ignore_ascii_case(expected)
                        } else {
                            actual == expected
                        }
                    })
                });
                if !matches {
                    return false;
                }
            }
            SimpleSelector::Attribute(_) | SimpleSelector::Pseudo(_) => {}
        }
    }
    true
}

fn matches_complex_at(
    dom: &Dom,
    element: NodeId,
    selector: &ComplexSelector,
    index: usize,
    context: &MatchContext,
) -> bool {
    let subject = index == selector.compounds.len() - 1;
    if !matches_compound(dom, element, &selector.compounds[index], context, subject) {
        return false;
    }
    if index == 0 {
        return true;
    }
    match selector.combinators[index - 1] {
        Combinator::Child => selector_parent(dom, element, context)
            .is_some_and(|parent| matches_complex_at(dom, parent, selector, index - 1, context)),
        Combinator::Descendant => {
            let mut ancestor = selector_parent(dom, element, context);
            while let Some(candidate) = ancestor {
                if matches_complex_at(dom, candidate, selector, index - 1, context) {
                    return true;
                }
                ancestor = selector_parent(dom, candidate, context);
            }
            false
        }
        Combinator::NextSibling => previous_element_sibling(dom, element)
            .is_some_and(|sibling| matches_complex_at(dom, sibling, selector, index - 1, context)),
        Combinator::SubsequentSibling => {
            let mut sibling = previous_element_sibling(dom, element);
            while let Some(candidate) = sibling {
                if matches_complex_at(dom, candidate, selector, index - 1, context) {
                    return true;
                }
                sibling = previous_element_sibling(dom, candidate);
            }
            false
        }
    }
}

fn matches_compound(
    dom: &Dom,
    element: NodeId,
    selector: &CompoundSelector,
    context: &MatchContext,
    subject: bool,
) -> bool {
    let Some(data) = element_data(dom, element) else {
        return false;
    };
    if context.shadow_host == Some(element) {
        // CSS Shadow 1 §3.2.2: within its own shadow tree the host is
        // featureless, and only `:host`, `:host()`, `:host-context()` (plus
        // `:scope` when it is the scoping root) may match it.
        if !matches_featureless_host(dom, element, data, selector, context) {
            return false;
        }
    } else {
        if let Some(type_selector) = &selector.type_selector
            && let Some(expected) = &type_selector.name
        {
            let matches = if data.namespace == Namespace::Html {
                data.local_name.eq_ignore_ascii_case(expected)
            } else {
                data.local_name == *expected
            };
            if !matches {
                return false;
            }
        }
        if selector
            .simple
            .iter()
            .any(|simple| !matches_simple(dom, element, data, simple, context))
        {
            return false;
        }
    }
    // Only the subject compound (the rightmost one) may carry a
    // pseudo-element; the ancestor compounds of `#host .item::after` match
    // ordinary elements even while the subject is being matched for `::after`.
    if !subject {
        return true;
    }
    if context.slotted_element.is_some() {
        // While a shadow tree's `::slotted()` rule is being evaluated, the
        // selector subject is the *slot*. Only the slotted pseudo-element may
        // match; ordinary shadow rules must not reach the assigned element.
        return matches_slotted_subject(dom, selector, context);
    }
    match (&selector.pseudo_element, &context.pseudo_element) {
        (None, None) => true,
        (Some(expected), Some(actual)) => {
            // Functional pseudo-elements carry an argument this renderer never
            // generates, so they remain valid selectors that match nothing.
            expected.argument.is_none() && expected.name.eq_ignore_ascii_case(actual)
        }
        _ => false,
    }
}

/// Whether `selector` may match a featureless shadow host (CSS Shadow 1
/// §3.2.2). The host is matched by the `:host` family only; `:is()`/`:where()`
/// are unwrapped because the engine generates `:where(:scope)` for implicit
/// `@scope` preludes, and `:scope` is allowed when it is the scoping root
/// (CSS Cascade 6 §3.5.1).
fn matches_featureless_host(
    dom: &Dom,
    element: NodeId,
    data: &ElementData,
    selector: &CompoundSelector,
    context: &MatchContext,
) -> bool {
    if selector.type_selector.is_some() || selector.simple.is_empty() {
        return false;
    }
    selector.simple.iter().all(|simple| match simple {
        SimpleSelector::Pseudo(
            pseudo @ (PseudoClass::Host(_)
            | PseudoClass::HostContext(_)
            | PseudoClass::Scope
            | PseudoClass::Is(_)
            | PseudoClass::Where(_)),
        ) => matches_pseudo(dom, element, data, pseudo, context),
        _ => false,
    })
}

/// Match the subject of a `::slotted(<compound-selector>)` rule.
///
/// `element` is the originating slot (the selector before the pseudo-element
/// matched it), and the argument is evaluated against the light-DOM element
/// the slot is currently styling.
fn matches_slotted_subject(dom: &Dom, selector: &CompoundSelector, context: &MatchContext) -> bool {
    if context.pseudo_element.is_some() {
        return false;
    }
    let Some(expected) = &selector.pseudo_element else {
        return false;
    };
    if !expected.name.eq_ignore_ascii_case("slotted") {
        return false;
    }
    let (Some(slotted_argument), Some(slotted_element)) =
        (&expected.slotted, context.slotted_element)
    else {
        return false;
    };
    let outer = outer_selector_context(context);
    matches_selector_list(dom, slotted_element, slotted_argument, &outer)
}

fn matches_simple(
    dom: &Dom,
    element: NodeId,
    data: &ElementData,
    selector: &SimpleSelector,
    context: &MatchContext,
) -> bool {
    match selector {
        SimpleSelector::Id(expected) => attribute_value(data, "id").is_some_and(|actual| {
            if context.quirks_mode {
                actual.eq_ignore_ascii_case(expected)
            } else {
                actual == expected
            }
        }),
        SimpleSelector::Class(expected) => attribute_value(data, "class").is_some_and(|classes| {
            classes.split_ascii_whitespace().any(|actual| {
                if context.quirks_mode {
                    actual.eq_ignore_ascii_case(expected)
                } else {
                    actual == expected
                }
            })
        }),
        SimpleSelector::Attribute(attribute) => matches_attribute(data, attribute),
        SimpleSelector::Pseudo(pseudo) => matches_pseudo(dom, element, data, pseudo, context),
    }
}

fn matches_attribute(data: &ElementData, selector: &AttributeSelector) -> bool {
    let actual = data.attributes.iter().find(|attribute| {
        if data.namespace == Namespace::Html {
            attribute.local_name.eq_ignore_ascii_case(&selector.name)
        } else {
            attribute.local_name == selector.name
        }
    });
    if selector.operator == AttributeOperator::Exists {
        return actual.is_some();
    }
    let Some(actual) = actual.map(|attribute| attribute.value.as_str()) else {
        return false;
    };
    if selector.value.is_empty()
        && matches!(
            selector.operator,
            AttributeOperator::Prefix | AttributeOperator::Suffix | AttributeOperator::Substring
        )
    {
        return false;
    }
    let equals = |left: &str, right: &str| match selector.case_sensitivity {
        CaseSensitivity::AsciiInsensitive => left.eq_ignore_ascii_case(right),
        CaseSensitivity::DocumentDefault | CaseSensitivity::Sensitive => left == right,
    };
    match selector.operator {
        AttributeOperator::Exists => true,
        AttributeOperator::Equals => equals(actual, &selector.value),
        AttributeOperator::Includes => actual
            .split_ascii_whitespace()
            .any(|part| equals(part, &selector.value)),
        AttributeOperator::DashMatch => {
            equals(actual, &selector.value)
                || actual
                    .get(..selector.value.len())
                    .is_some_and(|prefix| equals(prefix, &selector.value))
                    && actual.as_bytes().get(selector.value.len()) == Some(&b'-')
        }
        AttributeOperator::Prefix => match selector.case_sensitivity {
            CaseSensitivity::AsciiInsensitive => actual
                .get(..selector.value.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&selector.value)),
            _ => actual.starts_with(&selector.value),
        },
        AttributeOperator::Suffix => match selector.case_sensitivity {
            CaseSensitivity::AsciiInsensitive => actual
                .get(actual.len().saturating_sub(selector.value.len())..)
                .is_some_and(|suffix| suffix.eq_ignore_ascii_case(&selector.value)),
            _ => actual.ends_with(&selector.value),
        },
        AttributeOperator::Substring => match selector.case_sensitivity {
            CaseSensitivity::AsciiInsensitive => actual
                .to_ascii_lowercase()
                .contains(&selector.value.to_ascii_lowercase()),
            _ => actual.contains(&selector.value),
        },
    }
}

/// The matching context for a selector list nested inside a functional
/// pseudo-class such as `:is()`, `:where()`, `:not()`, `:has()` or `:nth-child(… of …)`.
///
/// Those lists select ordinary elements, and only the outermost selector's
/// subject may carry the generated pseudo-element. Inheriting the outer
/// pseudo-element context is what made `:is(.host) p::before` unable to see the
/// `p`: the `:is()` argument has no pseudo-element of its own.
fn element_match_context(context: &MatchContext) -> Cow<'_, MatchContext> {
    if context.pseudo_element.is_none() && context.slotted_element.is_none() {
        Cow::Borrowed(context)
    } else {
        let mut element = context.clone();
        element.pseudo_element = None;
        // A nested selector list selects an ordinary element: in a
        // `::slotted()` rule that is the originating slot, never the slotted
        // element whose alias is currently being matched.
        element.slotted_element = None;
        Cow::Owned(element)
    }
}

/// The context a selector argument is evaluated in when the spec says it is
/// evaluated "in its normal context": outside the shadow tree whose host is
/// being tested. `:host(.foo)` tests `.foo` against the host's own tree, and
/// `:host-context(.theme)` tests each shadow-including ancestor in that
/// ancestor's own tree.
fn outer_selector_context(context: &MatchContext) -> Cow<'_, MatchContext> {
    if context.tree_root.is_none()
        && context.shadow_host.is_none()
        && context.slotted_element.is_none()
        && context.pseudo_element.is_none()
    {
        Cow::Borrowed(context)
    } else {
        let mut outer = context.clone();
        outer.tree_root = None;
        outer.shadow_host = None;
        outer.slotted_element = None;
        outer.pseudo_element = None;
        Cow::Owned(outer)
    }
}

fn matches_nested_selector_list(
    dom: &Dom,
    element: NodeId,
    selectors: &SelectorList,
    context: &MatchContext,
) -> bool {
    let element_context = element_match_context(context);
    matches_selector_list(dom, element, selectors, &element_context)
}

#[allow(clippy::too_many_lines)]
fn matches_pseudo(
    dom: &Dom,
    element: NodeId,
    data: &ElementData,
    pseudo: &PseudoClass,
    context: &MatchContext,
) -> bool {
    match pseudo {
        PseudoClass::Root => dom.parent(element) == Some(dom.document()),
        PseudoClass::Scope => context
            .scope
            .or_else(|| document_element(dom))
            .is_some_and(|scope| scope == element),
        PseudoClass::Empty => {
            dom.children(element)
                .unwrap_or_default()
                .iter()
                .all(|child| match dom.node(*child).map(render_dom::Node::kind) {
                    Some(NodeKind::Element(_)) => false,
                    Some(NodeKind::Text(data)) => data.is_empty(),
                    _ => true,
                })
        }
        PseudoClass::FirstChild => {
            element_index(dom, element, None, context).is_some_and(|(index, _)| index == 1)
        }
        PseudoClass::LastChild => {
            element_index(dom, element, None, context).is_some_and(|(index, count)| index == count)
        }
        PseudoClass::OnlyChild => element_index(dom, element, None, context) == Some((1, 1)),
        PseudoClass::FirstOfType => {
            type_index(dom, element, false).is_some_and(|(index, _)| index == 1)
        }
        PseudoClass::LastOfType => {
            type_index(dom, element, false).is_some_and(|(index, count)| index == count)
        }
        PseudoClass::OnlyOfType => type_index(dom, element, false) == Some((1, 1)),
        PseudoClass::NthChild(expression) => {
            element_index(dom, element, expression.of.as_ref(), context)
                .is_some_and(|(index, _)| matches_an_plus_b(expression, index))
        }
        PseudoClass::NthLastChild(expression) => {
            element_index(dom, element, expression.of.as_ref(), context)
                .is_some_and(|(index, count)| matches_an_plus_b(expression, count - index + 1))
        }
        PseudoClass::NthOfType(expression) => type_index(dom, element, false)
            .is_some_and(|(index, _)| matches_an_plus_b(expression, index)),
        PseudoClass::NthLastOfType(expression) => type_index(dom, element, false)
            .is_some_and(|(index, count)| matches_an_plus_b(expression, count - index + 1)),
        PseudoClass::Is(list) | PseudoClass::Where(list) => {
            matches_nested_selector_list(dom, element, list, context)
        }
        PseudoClass::Not(list) => !matches_nested_selector_list(dom, element, list, context),
        PseudoClass::Has(relative) => {
            let element_context = element_match_context(context);
            relative
                .iter()
                .any(|selector| matches_relative(dom, element, selector, &element_context))
        }
        PseudoClass::Link | PseudoClass::AnyLink => {
            matches!(data.local_name.as_str(), "a" | "area" | "link")
                && attribute_value(data, "href").is_some()
        }
        PseudoClass::Visited => context.visited_links.contains(&element),
        PseudoClass::Enabled => is_form_control(data) && !is_disabled(dom, element, data),
        PseudoClass::Disabled => is_disabled(dom, element, data),
        PseudoClass::Checked => {
            (data.local_name == "input" && attribute_value(data, "checked").is_some())
                || (data.local_name == "option" && attribute_value(data, "selected").is_some())
        }
        PseudoClass::PlaceholderShown => {
            matches!(data.local_name.as_str(), "input" | "textarea")
                && attribute_value(data, "placeholder").is_some()
                && attribute_value(data, "value").is_none_or(str::is_empty)
        }
        PseudoClass::Focus => context.focused == Some(element),
        PseudoClass::FocusVisible => context.focus_visible == Some(element),
        PseudoClass::FocusWithin => {
            context.focused == Some(element)
                || context
                    .focused
                    .is_some_and(|focused| is_descendant_of(dom, focused, element))
        }
        PseudoClass::Hover => context.hovered.contains(&element),
        PseudoClass::Active => context.active.contains(&element),
        PseudoClass::Target => context.target == Some(element),
        PseudoClass::Lang(ranges) => element_language(dom, element).is_some_and(|language| {
            ranges.iter().any(|range| {
                range == "*"
                    || language.eq_ignore_ascii_case(range)
                    || language
                        .get(..range.len())
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(range))
                        && language.as_bytes().get(range.len()) == Some(&b'-')
            })
        }),
        // CSS Shadow 1 §3.2.3: these match only in the context of a shadow
        // tree, and only the tree's own host.
        PseudoClass::Host(argument) => {
            if context.shadow_host != Some(element) {
                return false;
            }
            let outer = outer_selector_context(context);
            argument
                .as_ref()
                .is_none_or(|selectors| matches_selector_list(dom, element, selectors, &outer))
        }
        PseudoClass::HostContext(selectors) => {
            if context.shadow_host != Some(element) {
                return false;
            }
            // The argument tests the host and every shadow-including ancestor,
            // which deliberately crosses shadow boundaries upward.
            let outer = outer_selector_context(context);
            let mut current = Some(element);
            while let Some(candidate) = current {
                if matches_selector_list(dom, candidate, selectors, &outer) {
                    return true;
                }
                current = shadow_including_parent_element(dom, candidate);
            }
            false
        }
        PseudoClass::Unsupported => false,
    }
}

fn matches_relative(
    dom: &Dom,
    anchor: NodeId,
    relative: &RelativeSelector,
    context: &MatchContext,
) -> bool {
    let mut candidates = Vec::new();
    match relative.leading {
        Combinator::Descendant | Combinator::Child => {
            collect_element_descendants(dom, anchor, &mut candidates);
        }
        Combinator::NextSibling | Combinator::SubsequentSibling => {
            let mut sibling = next_element_sibling(dom, anchor);
            while let Some(candidate) = sibling {
                candidates.push(candidate);
                collect_element_descendants(dom, candidate, &mut candidates);
                if relative.leading == Combinator::NextSibling {
                    break;
                }
                sibling = next_element_sibling(dom, candidate);
            }
        }
    }
    candidates.into_iter().any(|candidate| {
        matches_relative_at(
            dom,
            anchor,
            candidate,
            relative,
            relative.selector.compounds.len() - 1,
            context,
        )
    })
}

fn matches_relative_at(
    dom: &Dom,
    anchor: NodeId,
    element: NodeId,
    relative: &RelativeSelector,
    index: usize,
    context: &MatchContext,
) -> bool {
    let subject = index == relative.selector.compounds.len() - 1;
    if !matches_compound(
        dom,
        element,
        &relative.selector.compounds[index],
        context,
        subject,
    ) {
        return false;
    }
    if index == 0 {
        return match relative.leading {
            Combinator::Descendant => is_descendant_of(dom, element, anchor),
            Combinator::Child => element_parent(dom, element) == Some(anchor),
            Combinator::NextSibling => previous_element_sibling(dom, element) == Some(anchor),
            Combinator::SubsequentSibling => {
                let mut previous = previous_element_sibling(dom, element);
                while let Some(candidate) = previous {
                    if candidate == anchor {
                        return true;
                    }
                    previous = previous_element_sibling(dom, candidate);
                }
                false
            }
        };
    }
    match relative.selector.combinators[index - 1] {
        Combinator::Child => element_parent(dom, element).is_some_and(|parent| {
            matches_relative_at(dom, anchor, parent, relative, index - 1, context)
        }),
        Combinator::Descendant => {
            let mut parent = element_parent(dom, element);
            while let Some(candidate) = parent {
                if matches_relative_at(dom, anchor, candidate, relative, index - 1, context) {
                    return true;
                }
                parent = element_parent(dom, candidate);
            }
            false
        }
        Combinator::NextSibling => previous_element_sibling(dom, element).is_some_and(|previous| {
            matches_relative_at(dom, anchor, previous, relative, index - 1, context)
        }),
        Combinator::SubsequentSibling => {
            let mut previous = previous_element_sibling(dom, element);
            while let Some(candidate) = previous {
                if matches_relative_at(dom, anchor, candidate, relative, index - 1, context) {
                    return true;
                }
                previous = previous_element_sibling(dom, candidate);
            }
            false
        }
    }
}

fn element_index(
    dom: &Dom,
    element: NodeId,
    filter: Option<&SelectorList>,
    context: &MatchContext,
) -> Option<(i32, i32)> {
    let parent = dom.parent(element)?;
    let element_context = element_match_context(context);
    let siblings: Vec<_> = dom
        .children(parent)?
        .iter()
        .copied()
        .filter(|candidate| {
            is_element(dom, *candidate)
                && filter.is_none_or(|selectors| {
                    matches_selector_list(dom, *candidate, selectors, &element_context)
                })
        })
        .collect();
    let index = siblings
        .iter()
        .position(|candidate| *candidate == element)?;
    Some((
        i32::try_from(index + 1).ok()?,
        i32::try_from(siblings.len()).ok()?,
    ))
}

fn type_index(dom: &Dom, element: NodeId, _from_end: bool) -> Option<(i32, i32)> {
    let parent = dom.parent(element)?;
    let data = element_data(dom, element)?;
    let siblings: Vec<_> = dom
        .children(parent)?
        .iter()
        .copied()
        .filter(|candidate| {
            element_data(dom, *candidate).is_some_and(|candidate_data| {
                candidate_data.namespace == data.namespace
                    && candidate_data.local_name == data.local_name
            })
        })
        .collect();
    let index = siblings
        .iter()
        .position(|candidate| *candidate == element)?;
    Some((
        i32::try_from(index + 1).ok()?,
        i32::try_from(siblings.len()).ok()?,
    ))
}

fn matches_an_plus_b(expression: &NthExpression, index: i32) -> bool {
    if expression.a == 0 {
        return index == expression.b;
    }
    let difference = index - expression.b;
    difference % expression.a == 0 && difference / expression.a >= 0
}

fn element_data(dom: &Dom, node: NodeId) -> Option<&ElementData> {
    match dom.node(node)?.kind() {
        NodeKind::Element(data) => Some(data),
        _ => None,
    }
}

fn is_element(dom: &Dom, node: NodeId) -> bool {
    element_data(dom, node).is_some()
}

fn document_element(dom: &Dom) -> Option<NodeId> {
    dom.children(dom.document())?
        .iter()
        .copied()
        .find(|node| is_element(dom, *node))
}

fn element_parent(dom: &Dom, node: NodeId) -> Option<NodeId> {
    dom.parent(node).filter(|parent| is_element(dom, *parent))
}

/// The parent used by combinator matching.
///
/// CSS Shadow 1 §3.2.2 makes the shadow host appear in its own shadow tree
/// with the tree's contents as children, so the root elements of the tree have
/// the host as their ancestor. The host is featureless there, which
/// [`matches_compound`] enforces: only `:host` compounds can match it, so
/// ordinary shadow selectors still cannot see outside the tree.
fn selector_parent(dom: &Dom, node: NodeId, context: &MatchContext) -> Option<NodeId> {
    // The host is the top of its shadow tree's match list. Its own ancestors
    // belong to the outer tree and a shadow-tree selector may not see them.
    if context.shadow_host == Some(node) {
        return None;
    }
    let parent = dom.parent(node)?;
    if is_element(dom, parent) {
        return Some(parent);
    }
    if context.tree_root == Some(parent) {
        return context.shadow_host;
    }
    None
}

/// The next element on the shadow-including ancestor chain (DOM §4.4): the
/// parent, or the host when the parent is a shadow root. Used only by
/// `:host-context()`, whose argument is specified to pierce shadow boundaries.
fn shadow_including_parent_element(dom: &Dom, node: NodeId) -> Option<NodeId> {
    let parent = dom.parent(node)?;
    if is_element(dom, parent) {
        return Some(parent);
    }
    dom.shadow_host(parent)
}

fn previous_element_sibling(dom: &Dom, node: NodeId) -> Option<NodeId> {
    let mut sibling = dom.previous_sibling(node);
    while let Some(candidate) = sibling {
        if is_element(dom, candidate) {
            return Some(candidate);
        }
        sibling = dom.previous_sibling(candidate);
    }
    None
}

fn next_element_sibling(dom: &Dom, node: NodeId) -> Option<NodeId> {
    let mut sibling = dom.next_sibling(node);
    while let Some(candidate) = sibling {
        if is_element(dom, candidate) {
            return Some(candidate);
        }
        sibling = dom.next_sibling(candidate);
    }
    None
}

fn collect_element_descendants(dom: &Dom, node: NodeId, result: &mut Vec<NodeId>) {
    for child in dom.children(node).unwrap_or_default() {
        if is_element(dom, *child) {
            result.push(*child);
        }
        collect_element_descendants(dom, *child, result);
    }
}

fn is_descendant_of(dom: &Dom, node: NodeId, ancestor: NodeId) -> bool {
    let mut parent = dom.parent(node);
    while let Some(candidate) = parent {
        if candidate == ancestor {
            return true;
        }
        parent = dom.parent(candidate);
    }
    false
}

fn attribute_value<'a>(data: &'a ElementData, name: &str) -> Option<&'a str> {
    data.attributes
        .iter()
        .find(|attribute| {
            attribute.namespace.is_none() && attribute.local_name.eq_ignore_ascii_case(name)
        })
        .map(|attribute| attribute.value.as_str())
}

fn is_form_control(data: &ElementData) -> bool {
    matches!(
        data.local_name.as_str(),
        "button" | "fieldset" | "input" | "optgroup" | "option" | "select" | "textarea"
    )
}

fn is_disabled(dom: &Dom, element: NodeId, data: &ElementData) -> bool {
    if !is_form_control(data) {
        return false;
    }
    if attribute_value(data, "disabled").is_some() {
        return true;
    }
    if data.local_name == "option"
        && element_parent(dom, element)
            .and_then(|parent| element_data(dom, parent))
            .is_some_and(|parent| {
                parent.local_name == "optgroup" && attribute_value(parent, "disabled").is_some()
            })
    {
        return true;
    }
    false
}

fn element_language(dom: &Dom, element: NodeId) -> Option<&str> {
    let mut current = Some(element);
    while let Some(candidate) = current {
        if let Some(data) = element_data(dom, candidate)
            && let Some(language) = attribute_value(data, "lang")
        {
            return Some(language);
        }
        current = element_parent(dom, candidate);
    }
    None
}

const fn is_css_whitespace(character: char) -> bool {
    matches!(character, '\t' | '\n' | '\u{000c}' | '\r' | ' ')
}

const fn is_name_start(character: char) -> bool {
    character.is_ascii_alphabetic() || character == '_' || !character.is_ascii()
}

const fn is_name_character(character: char) -> bool {
    is_name_start(character) || character.is_ascii_digit() || character == '-'
}

fn is_legacy_pseudo_element(name: &str) -> bool {
    matches!(name, "before" | "after" | "first-line" | "first-letter")
}

#[cfg(test)]
mod tests {
    use render_html::parse_document;

    use super::{
        MatchContext, Specificity, matches_selector_list, matching_specificity,
        parse_nested_selector_list, parse_selector_list, select_all,
    };

    /// A string and a `url()` are opaque to nesting substitution for the same
    /// reason `[]` and `{}` are - a `&` inside one is a literal character - but
    /// they are not blocks, and the substitution used to route them through the
    /// block path. That asked `cssparser` for a nested parser at a token that
    /// had not opened one, and it panics there rather than returning an error,
    /// so `a { b:not("x") { color: red } }` took the process down. A nested
    /// rule with a string in its selector is neither malformed CSS nor even a
    /// discard: whether the selector is one this engine can evaluate is a
    /// separate question, and it has to be answerable without ending the run.
    ///
    /// So the property pinned here is that these inputs are *decided*, one way
    /// or the other, and that a string stays opaque when the surrounding
    /// selector is one this engine does accept - which is the part that shows
    /// the token is being read as a literal and not as a block boundary.
    #[test]
    fn strings_and_urls_in_a_nested_selector_are_decided_rather_than_panicking() {
        for input in [
            r#"b:not("x")"#,
            "b:has('x')",
            "b:has(url(x))",
            r#"b[href="&"]"#,
            "b:not(&)",
        ] {
            // No panic, and the answer is one the caller can act on.
            let _ = parse_nested_selector_list(input, ".foo");
        }
        // The two this crate's selector grammar accepts, checked for the
        // substitution itself: a `&` inside a string is a literal character,
        // and a `&` as a pseudo-class argument is the nesting selector.
        assert_eq!(
            parse_nested_selector_list(r#"b[href="&"]"#, ".foo")
                .unwrap()
                .css,
            r#":is(.foo) b[href="&"]"#
        );
        assert_eq!(
            parse_nested_selector_list("b:not(&)", ".foo").unwrap().css,
            "b:not(:is(.foo))"
        );
    }

    /// CSS Nesting §3.1: `&` desugars to `:is(<parent>)`, and §3.2's examples
    /// pin each form the spec spells out.
    #[test]
    fn nesting_selectors_resolve_to_an_is_of_the_parent() {
        let cases = [
            // `&` on its own.
            ("&", ".foo", ":is(.foo)"),
            // In a compound selector, refining the parent's.
            ("&.bar", ".foo", ":is(.foo).bar"),
            // With an explicit combinator.
            ("& > .bar", ".foo", ":is(.foo) > .bar"),
            ("&:hover", ".foo", ":is(.foo):hover"),
            // A relative selector implies a leading `&`.
            ("> .bar", ".foo", ":is(.foo) > .bar"),
            ("+ .bar", ".foo", ":is(.foo) + .bar"),
            ("~ .bar", ".foo", ":is(.foo) ~ .bar"),
            // ... and does so even when it also names `&` later on.
            ("+ .bar + &", ".foo", ":is(.foo) + .bar + :is(.foo)"),
            // A descendant selector without `&` is a relative descendant.
            (".bar", ".foo", ":is(.foo) .bar"),
            // `&` need not be at the start.
            (".parent &", ".foo", ".parent :is(.foo)"),
            (":not(&)", ".foo", ":not(:is(.foo))"),
            // Doubled `&` matches the parent twice.
            ("&&", ".foo", ":is(.foo):is(.foo)"),
            // A parent list stays one `:is()` argument, not a cross product.
            ("& c", "#a, b", ":is(#a, b) c"),
        ];
        for (input, parent, expected) in cases {
            let resolved = parse_nested_selector_list(input, parent)
                .unwrap_or_else(|error| panic!("{input}: {error}"));
            assert_eq!(resolved.css, expected, "{input}");
            // The resolved text is what a deeper level resolves against, so it
            // has to be a selector list in its own right.
            parse_selector_list(&resolved.css)
                .unwrap_or_else(|error| panic!("{input} resolved to an invalid list: {error}"));
        }
    }

    /// CSS Nesting §4: `&` and `:where(&)` take `:is()`'s specificity, and a
    /// relative selector that implies `&` gets the same treatment.
    #[test]
    fn nesting_selectors_take_the_largest_parent_specificity() {
        let specificity = |nested: &str, parent: &str| {
            parse_nested_selector_list(nested, parent)
                .unwrap_or_else(|error| panic!("{nested}: {error}"))
                .selectors
                .max_specificity()
        };
        // `#a, b`: the largest in the parent list, exactly as `:is()` does.
        assert_eq!(
            specificity("&", "#a, b"),
            Specificity {
                ids: 1,
                classes: 0,
                types: 0
            }
        );
        // `& c` adds the type selector on top of it.
        assert_eq!(
            specificity("& c", "#a, b"),
            Specificity {
                ids: 1,
                classes: 0,
                types: 1
            }
        );
        // An implied `&` counts the same way.
        assert_eq!(
            specificity(".c", "#a, b"),
            Specificity {
                ids: 1,
                classes: 1,
                types: 0
            }
        );
        // `:where()` reduces the nesting selector to zero, as §3.4 requires.
        assert_eq!(
            specificity(":where(&)", "#a, b"),
            Specificity {
                ids: 0,
                classes: 0,
                types: 0
            }
        );
        // Nesting levels stack: the inner `:is()` already holds the outer one.
        assert_eq!(
            specificity("&", ":is(#a, b) c"),
            Specificity {
                ids: 1,
                classes: 0,
                types: 1
            }
        );
    }

    /// Selectors 4 requires a type selector to come first in a compound, so
    /// `&div` stays invalid, and an attribute selector's `&` is a literal.
    #[test]
    fn nesting_selector_boundaries_are_respected() {
        assert!(parse_nested_selector_list("&div", ".foo").is_err());
        assert!(parse_nested_selector_list("&.foo", ".foo").is_ok());
        // A `&` inside an attribute selector or a string is a literal, so the
        // result keeps it and does not gain an implied `&`.
        assert_eq!(
            parse_nested_selector_list("[data-q='&']", ".foo")
                .expect("valid")
                .css,
            ":is(.foo) [data-q='&']"
        );
    }

    fn query(html: &str, selector: &str) -> Vec<String> {
        let output = parse_document(html);
        let selectors = parse_selector_list(selector).unwrap();
        let context = MatchContext {
            quirks_mode: output.quirks_mode.as_str() == "quirks",
            ..MatchContext::default()
        };
        select_all(&output.dom, output.dom.document(), &selectors, &context)
            .iter()
            .map(|node| {
                output
                    .dom
                    .attribute(*node, "id")
                    .unwrap()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn matches_compounds_and_all_four_combinators() {
        let html = "<!doctype html><main id=m><section class=card><h2 id=h></h2><p id=p class='lead hot'></p><p id=q></p></section></main>";
        assert_eq!(query(html, "main > .card h2 + p.hot"), vec!["p"]);
        assert_eq!(query(html, "h2 ~ p"), vec!["p", "q"]);
        assert_eq!(query(html, "main section > p"), vec!["p", "q"]);
    }

    #[test]
    fn matches_attribute_operators_and_case_flags() {
        let html = "<!doctype html><a id=x data-tags='one two' lang=en-US href='HTTPS://EXAMPLE.COM/Page.HTML'></a>";
        assert_eq!(query(html, "[data-tags~=two][lang|=en]"), vec!["x"]);
        assert_eq!(query(html, "[href^='https' i][href$='.html' i]"), vec!["x"]);
        assert!(query(html, "[href^='https' s]").is_empty());
        assert!(query(html, "[href*='']").is_empty());
    }

    #[test]
    fn structural_pseudos_count_elements_and_preserve_whitespace_for_empty() {
        let html = "<!doctype html><ul><li id=a class=x></li>text<li id=b></li><li id=c class=x> </li></ul><div id=e><!--comment--></div>";
        assert_eq!(query(html, "li:nth-child(2)"), vec!["b"]);
        assert_eq!(query(html, "li:nth-last-child(1)"), vec!["c"]);
        assert_eq!(query(html, "li:nth-child(2 of .x)"), vec!["c"]);
        assert_eq!(query(html, "#e:empty"), vec!["e"]);
        assert!(query(html, "#c:empty").is_empty());
        assert_eq!(query(html, "li:first-child"), vec!["a"]);
        assert_eq!(query(html, "li:first-of-type"), vec!["a"]);
        assert!(query(html, "li:only-child").is_empty());
    }

    #[test]
    fn selector_list_pseudos_match_and_compute_level_four_specificity() {
        let html =
            "<!doctype html><article id=a class=card><h2></h2></article><article id=b></article>";
        assert_eq!(
            query(html, "article:is(.card, #missing):has(> h2)"),
            vec!["a"]
        );
        assert_eq!(query(html, "article:not(.card)"), vec!["b"]);
        assert_eq!(
            parse_selector_list("div:where(#id, .class)")
                .unwrap()
                .max_specificity(),
            Specificity {
                ids: 0,
                classes: 0,
                types: 1
            }
        );
        assert_eq!(
            parse_selector_list("div:is(.class, #id)")
                .unwrap()
                .max_specificity(),
            Specificity {
                ids: 1,
                classes: 0,
                types: 1
            }
        );
    }

    #[test]
    fn relative_has_supports_sibling_and_descendant_anchors() {
        let html =
            "<!doctype html><div id=a><span><b></b></span></div><div id=b></div><p id=p></p>";
        assert_eq!(query(html, "div:has(span b)"), vec!["a"]);
        assert_eq!(query(html, "div:has(+ p)"), vec!["b"]);
        assert_eq!(query(html, "div:has(~ p)"), vec!["a", "b"]);
    }

    #[test]
    fn pseudo_elements_do_not_match_dom_elements_without_explicit_host_context() {
        let output = parse_document("<!doctype html><p id=x></p>");
        let element = select_all(
            &output.dom,
            output.dom.document(),
            &parse_selector_list("#x").unwrap(),
            &MatchContext::default(),
        )[0];
        assert!(!matches_selector_list(
            &output.dom,
            element,
            &parse_selector_list("p::before").unwrap(),
            &MatchContext::default()
        ));
        assert!(matches_selector_list(
            &output.dom,
            element,
            &parse_selector_list("p::before").unwrap(),
            &MatchContext {
                pseudo_element: Some("before".to_owned()),
                ..MatchContext::default()
            }
        ));
        assert_eq!(
            parse_selector_list("p:before").unwrap().max_specificity(),
            Specificity {
                ids: 0,
                classes: 0,
                types: 2
            }
        );
    }

    #[test]
    fn css_escapes_and_forgiving_is_lists_are_supported() {
        let html = "<!doctype html><div id='123' class='a+b'></div>";
        assert_eq!(query(html, "#\\31 23.a\\+b"), vec!["123"]);
        assert_eq!(query(html, ":is(.a\\+b, :unsupported(), div)"), vec!["123"]);
        assert!(parse_selector_list("div, :unsupported()").is_ok());
        assert!(parse_selector_list("#123").is_err());
        assert!(parse_selector_list(":nth-of-type(2 of .x)").is_err());
    }

    #[test]
    fn a_pseudo_element_on_the_subject_matches_through_ancestor_compounds() {
        // Selectors 4 §3.9: only the last compound may carry the pseudo-element.
        // Matching `#host .item::after` must not require `#host` itself to be an
        // `::after` subject; requiring that made every descendant pseudo rule
        // unreachable.
        let output = parse_document("<!doctype html><div id=host><p class=item id=x></p></div>");
        let element = select_all(
            &output.dom,
            output.dom.document(),
            &parse_selector_list("#x").unwrap(),
            &MatchContext::default(),
        )[0];
        let after = MatchContext {
            pseudo_element: Some("after".to_owned()),
            ..MatchContext::default()
        };
        assert!(matches_selector_list(
            &output.dom,
            element,
            &parse_selector_list("#host .item::after").unwrap(),
            &after
        ));
        assert!(!matches_selector_list(
            &output.dom,
            element,
            &parse_selector_list("#host .item::after").unwrap(),
            &MatchContext::default()
        ));
        // The same holds for a structural pseudo-class in an ancestor
        // compound: its own argument list selects elements, so the generated
        // pseudo-element must not leak into `:is()`, `:where()`, `:not()`,
        // `:has()` or `:nth-child(... of ...)`.
        for selector in [
            ":is(#host) .item::after",
            ":where(#host) .item::after",
            ":not(.missing) .item::after",
            ":has(p) .item::after",
            ":nth-child(1 of .item)::after",
        ] {
            assert!(
                matches_selector_list(
                    &output.dom,
                    element,
                    &parse_selector_list(selector).unwrap(),
                    &after
                ),
                "{selector} must match the element's ::after"
            );
        }
    }

    #[test]
    fn functional_pseudo_elements_parse_and_never_match_dom_elements() {
        // The exact selectors from qq.com's index.css (View Transitions).
        let selectors =
            parse_selector_list("html::view-transition-new(root),html::view-transition-old(root)")
                .unwrap();
        assert_eq!(selectors.selectors().len(), 2);
        assert!(
            parse_selector_list("html[data-theme-transition=fade]::view-transition-new(root)")
                .is_ok()
        );
        assert!(
            parse_selector_list("html[data-theme-transition=top-right]::view-transition-new(root)")
                .is_ok()
        );
        assert!(parse_selector_list("div::view-transition-group(*)").is_ok());
        assert!(parse_selector_list("::highlight(sel-1)").is_ok());

        // A pseudo-element with an argument still counts once toward
        // specificity (Selectors 4 §3.9), same as `::before`.
        assert_eq!(
            parse_selector_list("html::view-transition-new(root)")
                .unwrap()
                .max_specificity(),
            Specificity {
                ids: 0,
                classes: 0,
                types: 2
            }
        );

        // Valid but matching nothing: the engine never surfaces a
        // view-transition pseudo tree, and functional notation in a
        // non-functional pseudo-element (`::before(x)`) matches nothing too.
        let html = "<!doctype html><p id=x></p>";
        assert!(query(html, "p::view-transition-new(root)").is_empty());
        assert!(query(html, "p::before(x)").is_empty());

        // A plain `::before` still matches under its explicit host context,
        // while its functional spelling never does.
        let output = parse_document(html);
        let element = select_all(
            &output.dom,
            output.dom.document(),
            &parse_selector_list("p").unwrap(),
            &MatchContext::default(),
        )[0];
        let before = MatchContext {
            pseudo_element: Some("before".to_owned()),
            ..MatchContext::default()
        };
        assert!(matches_selector_list(
            &output.dom,
            element,
            &parse_selector_list("p::before").unwrap(),
            &before
        ));
        assert!(!matches_selector_list(
            &output.dom,
            element,
            &parse_selector_list("p::before(x)").unwrap(),
            &before
        ));

        // Malformed functional notation is still rejected.
        assert!(parse_selector_list("div::highlight()").is_err());
        assert!(parse_selector_list("div::highlight(unclosed").is_err());
        assert!(parse_selector_list("div::highlight(a)extra").is_err());
    }

    #[test]
    fn dynamic_pseudos_use_explicit_match_context() {
        let output = parse_document("<!doctype html><a id=x href=/></a>");
        let selector = parse_selector_list("a:any-link:focus:hover").unwrap();
        let element = select_all(
            &output.dom,
            output.dom.document(),
            &parse_selector_list("#x").unwrap(),
            &MatchContext::default(),
        )[0];
        let mut context = MatchContext {
            focused: Some(element),
            ..MatchContext::default()
        };
        context.hovered.insert(element);
        assert!(matches_selector_list(
            &output.dom,
            element,
            &selector,
            &context
        ));
    }

    #[test]
    fn focus_visible_needs_its_own_state_on_top_of_focus() {
        let output = parse_document("<!doctype html><button id=x>b</button>");
        let element = select_all(
            &output.dom,
            output.dom.document(),
            &parse_selector_list("#x").unwrap(),
            &MatchContext::default(),
        )[0];
        let selector = parse_selector_list("button:focus-visible").unwrap();
        let focused = MatchContext {
            focused: Some(element),
            ..MatchContext::default()
        };

        // A pointer press focuses the button without showing a focus ring.
        assert!(!matches_selector_list(
            &output.dom,
            element,
            &selector,
            &focused
        ));

        let visible = MatchContext {
            focus_visible: Some(element),
            ..focused.clone()
        };
        assert!(matches_selector_list(
            &output.dom,
            element,
            &selector,
            &visible
        ));

        // `:focus` still matches on focus alone.
        assert!(matches_selector_list(
            &output.dom,
            element,
            &parse_selector_list("button:focus").unwrap(),
            &focused
        ));
    }

    /// Selectors 4 §6.4.1 / css-conditional: with no interactive state, a
    /// dynamic pseudo-class matches nothing. An engine that answered
    /// `:hover`, `:focus` and `:active` unconditionally would paint every
    /// hovered-looking stylesheet rule at once.
    #[test]
    fn dynamic_pseudos_never_match_without_explicit_state() {
        let html = "<!doctype html><a id=x href=/ target=_self>t</a><input id=i>";
        let output = parse_document(html);
        for selector in [
            ":hover",
            ":focus",
            ":focus-visible",
            ":active",
            ":focus-within",
            ":visited",
            ":target",
        ] {
            let parsed = parse_selector_list(selector).unwrap();
            for element in [
                select_all(
                    &output.dom,
                    output.dom.document(),
                    &parse_selector_list("#x").unwrap(),
                    &MatchContext::default(),
                )[0],
                select_all(
                    &output.dom,
                    output.dom.document(),
                    &parse_selector_list("#i").unwrap(),
                    &MatchContext::default(),
                )[0],
            ] {
                assert!(
                    !matches_selector_list(&output.dom, element, &parsed, &MatchContext::default()),
                    "{selector} matched with a default MatchContext"
                );
                assert!(
                    matching_specificity(&output.dom, element, &parsed, &MatchContext::default())
                        .is_none()
                );
            }
        }
    }

    /// Selectors 4 §B: the specificity of `:is()`, `:not()` and `:has()` is
    /// the specificity of the most specific selector in their argument list,
    /// while `:where()` is always zero. `CompositeSelector::specificity`
    /// folds each compound, so the pseudo-classes must not add their own
    /// class-level weight on top.
    #[test]
    fn specificity_bearing_pseudo_classes_use_their_argument_list() {
        let id = Specificity {
            ids: 1,
            classes: 0,
            types: 0,
        };
        let class = Specificity {
            ids: 0,
            classes: 1,
            types: 0,
        };
        for (selector, expected) in [
            (":is(#a)", id),
            (":not(#a)", id),
            (":is(.a, #b)", id),
            (":not(.a, #b)", id),
            (":is(div, .a)", class),
            (":not(div, .a)", class),
            // The legacy single-argument spelling still folds in the type.
            (
                ":not(div)",
                Specificity {
                    ids: 0,
                    classes: 0,
                    types: 1,
                },
            ),
            (":where(#a)", Specificity::default()),
            (":where(div, #a, .b)", Specificity::default()),
        ] {
            assert_eq!(
                parse_selector_list(selector).unwrap().max_specificity(),
                expected,
                "{selector}"
            );
        }
        // `:has()` takes the max of its relative-selector list and adds
        // nothing of its own; `:nth-child(An+B of S)` is a class plus the max
        // of `S`.
        assert_eq!(
            parse_selector_list(":has(> #a)").unwrap().max_specificity(),
            id
        );
        assert_eq!(
            parse_selector_list(":has(> .a, + #b)")
                .unwrap()
                .max_specificity(),
            id
        );
        assert_eq!(
            parse_selector_list(":nth-child(2 of #a)")
                .unwrap()
                .max_specificity(),
            Specificity {
                ids: 1,
                classes: 1,
                types: 0
            }
        );
        assert_eq!(
            parse_selector_list(":nth-child(2 of .a, #b)")
                .unwrap()
                .max_specificity(),
            Specificity {
                ids: 1,
                classes: 1,
                types: 0
            }
        );
    }

    /// Selectors 4 §4.3 attribute grammar: escaped strings, unquoted
    /// identifiers, and the `i`/`s` flags.
    #[test]
    fn attribute_selector_grammar_accepts_the_standard_forms() {
        let html = "<!doctype html><a id=x data-bc='Yes' lang='ZH-Hans'></a>";
        // Escaped strings and unquoted identifiers with a case flag.
        assert_eq!(query(html, r#"[data-bc="Yes" i]"#), vec!["x"]);
        assert_eq!(query(html, r#"[data-bc="yes" s]"#), Vec::<String>::new());
        assert_eq!(query(html, "[data-bc=Yes i]"), vec!["x"]);
        // Dash matching is case-sensitive by default, and the `i` flag makes
        // it insensitive (Selectors 4 §4.3.1).
        assert_eq!(query(html, r#"[lang|="ZH"]"#), vec!["x"]);
        assert_eq!(query(html, r#"[lang|="zh"]"#), Vec::<String>::new());
        assert_eq!(query(html, r#"[lang|="zh" i]"#), vec!["x"]);
        assert_eq!(query(html, r#"[lang|="hant"]"#), Vec::<String>::new());
        // Whitespace and comments are allowed around every component.
        assert_eq!(query(html, "[ data-bc = 'Yes' i ]"), vec!["x"]);
        assert_eq!(query(html, "[data-bc/*c*/=/*c*/'Yes'/*c*/i]"), vec!["x"]);
        for invalid in ["[a=b c]", "[a|]", "[a='unterminated]", "[a=b i s]"] {
            assert!(
                parse_selector_list(invalid).is_err(),
                "{invalid} must not parse"
            );
        }
    }

    /// Selectors 4 §4.2: a combinator is a single token or real whitespace.
    /// CSS Syntax §4.3.1 deletes comments during preprocessing, so a comment
    /// is neither a descendant combinator nor a compound-selector separator.
    #[test]
    fn comments_never_act_as_combinators() {
        let html =
            "<!doctype html><main id=m class='a b'><p id=a class='x y'></p><p id=b></p></main>";
        // A comment inside a compound selector keeps it one compound.
        assert_eq!(query(html, "#m.a/*c*/.b"), vec!["m"]);
        assert_eq!(query(html, "p.x/*c*/.y"), vec!["a"]);
        assert_eq!(query(html, "main/*c*/#m"), vec!["m"]);
        assert_eq!(query(html, "main#m/*c*/.a/*c*/.b"), vec!["m"]);
        // A comment around a real combinator is transparent.
        assert_eq!(query(html, "main/*c*/>/*c*/p"), vec!["a", "b"]);
        assert_eq!(query(html, "main /*c*/ p"), vec!["a", "b"]);
        assert_eq!(query(html, "main /*c*/p"), vec!["a", "b"]);
        assert_eq!(query(html, "main/*c*/ p"), vec!["a", "b"]);
        // Without any whitespace there is no combinator at all, so the
        // selector is invalid and the stylesheet rule that carried it is
        // dropped rather than silently over-matched.
        assert!(parse_selector_list("main/*c*/p").is_err());
        assert_eq!(query(html, "mainp"), Vec::<String>::new());
        // A leading comment is transparent too.
        assert_eq!(query(html, "/*c*/#m"), vec!["m"]);
    }

    /// Selectors 4 §3.9: shadow-DOM pseudo-classes and pseudo-elements are
    /// valid syntax outside a shadow tree and simply match nothing there.
    #[test]
    fn shadow_dom_pseudo_classes_parse_and_match_nothing() {
        for selector in [
            ":host",
            ":host(.a)",
            ":host-context(.a)",
            ":host-context(.a) .b",
            "::part(button)",
            "::slotted(span)",
            "::cue(.b)",
            "::marker",
        ] {
            let parsed = parse_selector_list(selector)
                .unwrap_or_else(|error| panic!("{selector} must parse: {error}"));
            let html = "<!doctype html><span id=x></span>";
            let output = parse_document(html);
            let element = select_all(
                &output.dom,
                output.dom.document(),
                &parse_selector_list("#x").unwrap(),
                &MatchContext::default(),
            )[0];
            assert!(
                !matches_selector_list(&output.dom, element, &parsed, &MatchContext::default()),
                "{selector} must not match a light-DOM element"
            );
        }
        // CSS Shadow 1 §3.2.3-§3.2.4: the arguments are `<compound-selector>`
        // singular, so a comma list or a combinator is invalid.
        for invalid in [
            ":host(.a, .b)",
            ":host(.a .b)",
            ":host(.a > .b)",
            ":host()",
            ":host-context()",
            ":host-context(.a, .b)",
            "::slotted(.a, .b)",
            "::slotted(.a .b)",
        ] {
            assert!(
                parse_selector_list(invalid).is_err(),
                "{invalid} must not parse"
            );
        }
    }

    /// CSS Shadow 1 §3.2.2-§3.2.4: within a shadow tree the host is
    /// featureless and only `:host`/`:host-context()` match it; a shadow
    /// selector can cross the root only through those pseudo-classes, and
    /// `::slotted()` aliases an assigned light-DOM element.
    #[test]
    fn host_and_slotted_selectors_match_in_a_shadow_tree_context() {
        use render_dom::{ShadowRootMode, SlotAssignmentMode};

        let output = parse_document(
            "<!doctype html><section class=theme>\
             <div id=h class='card'><span id=light class=x>light</span></div>\
             </section>",
        );
        let mut dom = output.dom;
        let query = |dom: &render_dom::Dom, selector: &str| {
            select_all(
                dom,
                dom.document(),
                &parse_selector_list(selector).unwrap(),
                &MatchContext::default(),
            )[0]
        };
        let host = query(&dom, "#h");
        let light = query(&dom, "#light");
        let root = dom
            .attach_shadow(host, ShadowRootMode::Open, false, SlotAssignmentMode::Named)
            .unwrap();
        let inner = dom.create_element("span");
        dom.set_attribute(inner, "class", "inner").unwrap();
        dom.append_child(root, inner).unwrap();
        let slot = dom.create_element("slot");
        dom.append_child(root, slot).unwrap();

        let shadow = MatchContext {
            tree_root: Some(root),
            shadow_host: Some(host),
            ..MatchContext::default()
        };
        let matches = |element, selector: &str, context: &MatchContext| {
            matches_selector_list(
                &dom,
                element,
                &parse_selector_list(selector).unwrap(),
                context,
            )
        };

        // Outside a shadow-tree context `:host` matches nothing.
        assert!(!matches(host, ":host", &MatchContext::default()));
        // Inside it, `:host` and `:host(<compound>)` match the host.
        assert!(matches(host, ":host", &shadow));
        assert!(matches(host, ":host(.card)", &shadow));
        assert!(!matches(host, ":host(.missing)", &shadow));
        // The host is featureless: a type or class selector in the shadow
        // context cannot reach it, and `.card:host` is therefore no match.
        assert!(!matches(host, "div", &shadow));
        assert!(!matches(host, ".card", &shadow));
        assert!(!matches(host, ".card:host", &shadow));
        // `:host() ` also composes with a descendant combinator.
        assert!(matches(inner, ":host(.card) .inner", &shadow));
        // An ordinary shadow selector does not pick up the host's class as an
        // ancestor match, since the host is only reachable through :host.
        assert!(!matches(inner, ".card .inner", &shadow));
        // Nor can it see an ancestor outside the shadow tree, even though
        // `:host-context(.theme)` may look at that same element.
        assert!(!matches(inner, ".theme .inner", &shadow));
        // `:host-context()` sees shadow-including ancestors.
        assert!(matches(host, ":host-context(.theme)", &shadow));
        assert!(!matches(host, ":host-context(.missing)", &shadow));

        // `::slotted()` matches the assigned light-DOM element through its
        // originating slot, and not an element that is not assigned.
        let slotted = MatchContext {
            tree_root: Some(root),
            shadow_host: Some(host),
            slotted_element: Some(light),
            ..MatchContext::default()
        };
        assert!(matches(slot, "::slotted(.x)", &slotted));
        assert!(matches(slot, "slot::slotted(.x)", &slotted));
        assert!(!matches(slot, "::slotted(.missing)", &slotted));
        // While evaluating a slotted rule, ordinary shadow selectors must not
        // match the slot.
        assert!(!matches(slot, "slot", &slotted));
        // Without a slotted element the pseudo-element matches nothing.
        assert!(!matches(slot, "::slotted(.x)", &shadow));
        // In the slot's own context it is an ordinary slot element.
        assert!(matches(slot, "slot", &shadow));

        // Specificity: a pseudo-class plus its argument for the host family,
        // and a pseudo-element plus its argument for ::slotted.
        assert_eq!(
            parse_selector_list(":host").unwrap().max_specificity(),
            Specificity {
                ids: 0,
                classes: 1,
                types: 0
            }
        );
        assert_eq!(
            parse_selector_list(":host(.card)")
                .unwrap()
                .max_specificity(),
            Specificity {
                ids: 0,
                classes: 2,
                types: 0
            }
        );
        assert_eq!(
            parse_selector_list(":host(#card)")
                .unwrap()
                .max_specificity(),
            Specificity {
                ids: 1,
                classes: 1,
                types: 0
            }
        );
        assert_eq!(
            parse_selector_list(":host-context(.theme)")
                .unwrap()
                .max_specificity(),
            Specificity {
                ids: 0,
                classes: 2,
                types: 0
            }
        );
        assert_eq!(
            parse_selector_list("::slotted(.x)")
                .unwrap()
                .max_specificity(),
            Specificity {
                ids: 0,
                classes: 1,
                types: 1
            }
        );
        assert_eq!(
            parse_selector_list("::slotted(#x)")
                .unwrap()
                .max_specificity(),
            Specificity {
                ids: 1,
                classes: 0,
                types: 1
            }
        );
    }
}
