//! Token-level computed-value resolution.
//!
//! Custom properties are inherited and resolved before ordinary properties.
//! Property-specific grammars and canonical computed-value conversions remain
//! separate; this layer preserves token boundaries so later parsers cannot
//! accidentally turn `var(--n)px` into a dimension token.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::Arc;

use cssparser::{ParseError, Parser, ParserInput, Token};

use render_dom::{Dom, Node, NodeId, NodeKind};

use super::cascade::{
    CascadeInput, CascadeLookup, CascadeRuleIndex, CascadedStyle,
    cascade_element_with_origins_indexed, media_query_list_matches,
};
use super::container::{StyleFeature, StyleQueryEnvironment, parse_container_names};
use super::properties::{
    Direction, TypedPropertyValue, parse_typed_property, physical_longhand_for_logical,
};
use super::pseudo::{self, PseudoElement};
use super::registered::{PropertyRegistration, RegisteredSyntax, SyntaxKind, SyntaxMultiplier};
use super::selector::MatchContext;
use super::stylesheet::{CssWideKeyword, Declaration, css_wide_keyword, parse_declaration_list};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropertyDefinition {
    pub inherited: bool,
    pub initial_value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PropertyRegistry {
    definitions: BTreeMap<String, PropertyDefinition>,
}

impl PropertyRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Define ordinary-property inheritance metadata and its initial value.
    ///
    /// # Panics
    ///
    /// Panics when `name` is a custom property. Unregistered custom properties
    /// always use the inheritance rules from CSS Variables instead.
    pub fn define(
        &mut self,
        name: impl Into<String>,
        inherited: bool,
        initial_value: impl Into<String>,
    ) {
        let name = name.into().to_ascii_lowercase();
        assert!(
            !name.starts_with("--"),
            "custom properties are not ordinary property definitions"
        );
        self.definitions.insert(
            name,
            PropertyDefinition {
                inherited,
                initial_value: initial_value.into(),
            },
        );
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&PropertyDefinition> {
        self.definitions.get(name)
    }

    #[must_use]
    pub fn standard_baseline() -> Self {
        let mut registry = Self::new();
        for (name, initial) in [
            ("color", "canvastext"),
            ("color-scheme", "normal"),
            ("direction", "ltr"),
            ("font-family", "system-ui"),
            ("font-size", "medium"),
            ("font-style", "normal"),
            ("font-weight", "normal"),
            ("letter-spacing", "normal"),
            ("line-height", "normal"),
            ("text-align", "start"),
            ("text-indent", "0px"),
            ("text-transform", "none"),
            ("visibility", "visible"),
            ("white-space", "normal"),
            // CSS Text 3 §5.1 and §5.2, both `Inherited: yes`. Their initial
            // values are `normal` and `auto` respectively, and §5.2's `auto` is
            // the one whose restrictions "may vary based on the length of the
            // line", so a reader cannot infer `normal` from the initial value
            // alone and must look at the property.
            ("word-break", "normal"),
            ("line-break", "auto"),
            ("word-spacing", "normal"),
            // The CSS 2.1 table properties that *inherit*: `border-spacing`
            // (§17.6.1), `border-collapse` (§17.6), `empty-cells` (§17.6.2)
            // and `caption-side` (§17.4.1). The `Inherited` flag is stated in
            // each property definition and repeated in Appendix F's full
            // property table, which is the quick place to check it. Appendix F
            // says of itself that it is informative, not normative, so the
            // definition section is the citation that carries the requirement
            // and the table is only a cross-check.
            //
            // Registering them also means an unspecified property reports its
            // initial value to layout instead of `None`, which is what makes
            // `ComputedStyle::specified` necessary: presence in the computed
            // map no longer implies an author wrote anything.
            ("border-spacing", "0px"),
            ("border-collapse", "separate"),
            ("empty-cells", "show"),
            ("caption-side", "top"),
            ("clip-rule", "nonzero"),
        ] {
            registry.define(name, true, initial);
        }
        for (name, initial) in [
            ("content", "normal"),
            ("clip-path", "none"),
            ("filter", "none"),
            ("mask-image", "none"),
            ("mask-size", "auto"),
            ("mask-position", "0% 0%"),
            ("mask-repeat", "repeat"),
            ("animation-name", "none"),
            ("animation-duration", "0s"),
            ("animation-delay", "0s"),
            ("animation-timing-function", "ease"),
            ("animation-iteration-count", "1"),
            ("animation-direction", "normal"),
            ("animation-fill-mode", "none"),
            ("animation-play-state", "running"),
            ("transition-property", "all"),
            ("transition-duration", "0s"),
            ("transition-timing-function", "ease"),
            ("transition-delay", "0s"),
            ("transition-behavior", "normal"),
            ("background-color", "transparent"),
            ("background-image", "none"),
            ("background-repeat", "repeat"),
            ("background-position", "0% 0%"),
            ("background-size", "auto"),
            ("object-fit", "fill"),
            ("display", "inline"),
            ("position", "static"),
            ("float", "none"),
            ("clear", "none"),
            ("width", "auto"),
            ("height", "auto"),
            ("min-width", "auto"),
            ("min-height", "auto"),
            ("max-width", "none"),
            ("max-height", "none"),
            ("margin-top", "0px"),
            ("margin-right", "0px"),
            ("margin-bottom", "0px"),
            ("margin-left", "0px"),
            ("margin-inline-start", "0px"),
            ("margin-inline-end", "0px"),
            ("margin-block-start", "0px"),
            ("margin-block-end", "0px"),
            ("padding-top", "0px"),
            ("padding-right", "0px"),
            ("padding-bottom", "0px"),
            ("padding-left", "0px"),
            ("padding-inline-start", "0px"),
            ("padding-inline-end", "0px"),
            ("padding-block-start", "0px"),
            ("padding-block-end", "0px"),
            ("border-top-width", "medium"),
            ("border-right-width", "medium"),
            ("border-bottom-width", "medium"),
            ("border-left-width", "medium"),
            ("border-inline-start-width", "medium"),
            ("border-inline-end-width", "medium"),
            ("border-block-start-width", "medium"),
            ("border-block-end-width", "medium"),
            ("border-top-style", "none"),
            ("border-right-style", "none"),
            ("border-bottom-style", "none"),
            ("border-left-style", "none"),
            ("border-inline-start-style", "none"),
            ("border-inline-end-style", "none"),
            ("border-block-start-style", "none"),
            ("border-block-end-style", "none"),
            ("border-top-color", "currentcolor"),
            ("border-right-color", "currentcolor"),
            ("border-bottom-color", "currentcolor"),
            ("border-left-color", "currentcolor"),
            ("border-inline-start-color", "currentcolor"),
            ("border-inline-end-color", "currentcolor"),
            ("border-block-start-color", "currentcolor"),
            ("border-block-end-color", "currentcolor"),
            ("top", "auto"),
            ("right", "auto"),
            ("bottom", "auto"),
            ("left", "auto"),
            ("inset-inline-start", "auto"),
            ("inset-inline-end", "auto"),
            ("inset-block-start", "auto"),
            ("inset-block-end", "auto"),
            ("opacity", "1"),
            ("overflow-x", "visible"),
            ("overflow-y", "visible"),
            ("box-sizing", "content-box"),
            ("z-index", "auto"),
            ("flex-direction", "row"),
            ("flex-wrap", "nowrap"),
            ("flex-basis", "auto"),
            ("flex-grow", "0"),
            ("flex-shrink", "1"),
            ("justify-content", "normal"),
            ("align-items", "stretch"),
            ("align-self", "auto"),
            ("align-content", "normal"),
            ("order", "0"),
            ("row-gap", "normal"),
            ("column-gap", "normal"),
            ("column-count", "auto"),
            ("column-width", "auto"),
            ("column-fill", "balance"),
            ("column-span", "none"),
            ("grid-template-columns", "none"),
            ("grid-template-rows", "none"),
            ("grid-column-start", "auto"),
            ("grid-column-end", "auto"),
            ("grid-row-start", "auto"),
            ("grid-row-end", "auto"),
            ("aspect-ratio", "auto"),
            ("transform", "none"),
            ("transform-origin", "50% 50%"),
            // The two that do *not* inherit, so they belong with the ordinary
            // properties. `table-layout` is CSS 2.1 §17.5.2.1; `vertical-align`
            // is §10.8.3 for inline-level boxes and §17.5.3 for the table
            // alignment it also drives, and its definition gives
            // `Inherited: no`. That matters: inheriting `vertical-align` would
            // align a cell with its ancestor's cell instead of its own, and
            // §17.5.3 distributes a *table's* alignment to its row groups
            // explicitly rather than by inheritance.
            ("table-layout", "auto"),
            ("vertical-align", "baseline"),
            // The four `text-decoration` longhands. `text-decoration` is a
            // shorthand over exactly these four (Text Decoration 4 §2.6), and
            // all four are `Inherited: no` - §2.1 says so and adds "but see
            // prose, above", §2.2, §2.3 and §2.4 say so flatly. CSS 2.1
            // Appendix F records the shorthand as `no (see prose)` too, and
            // notes of itself that it is informative rather than normative.
            //
            // They do not inherit, and that is not the same as not reaching
            // descendants: decorations *propagate* down the box tree from the
            // element that originated them (§2), which is a separate mechanism
            // from inheritance and is not the registry's business. So a
            // descendant's value says nothing about the ancestor, and the
            // ancestor's decoration still paints through the descendant.
            //
            // Registering them matters for more than completeness: the
            // user-agent stylesheet sets `text-decoration-line: underline` on
            // `a:link`, so an author writing `a { text-decoration: none }` only
            // wins if the shorthand actually expands into this longhand. Before
            // it did, that rule was dead on every page.
            ("text-decoration-line", "none"),
            ("text-decoration-style", "solid"),
            ("text-decoration-color", "currentcolor"),
            ("text-decoration-thickness", "auto"),
            // CSS Containment 3 §3.1 and §3.2: `container-type` and
            // `container-name` do not inherit. Registering them gives every
            // element the initial `normal`/`none` that keeps it out of the
            // query container chain until it says otherwise.
            ("container-type", "normal"),
            ("container-name", "none"),
            // CSS Containment 2 §2: `contain` does not inherit. Every element
            // starts with `none` so the containment helpers can read one
            // typed value without special-casing absence.
            ("contain", "none"),
            ("content-visibility", "visible"),
        ] {
            registry.define(name, false, initial);
        }
        registry
    }

    /// Every property this registry has metadata for, which is the declared
    /// set of properties the engine knows about as more than an untyped token
    /// stream. Public because the CSS feature query oracle in
    /// [`crate::supports`] answers `at-rule`-independent support questions from
    /// this same set, and a second private copy of it is how two answers to the
    /// same question would drift.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &PropertyDefinition)> {
        self.definitions.iter()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComputationLimits {
    pub max_custom_properties: usize,
    pub max_dependency_depth: usize,
    pub max_component_depth: usize,
    pub max_component_values: usize,
    pub max_value_bytes: usize,
}

impl Default for ComputationLimits {
    fn default() -> Self {
        Self {
            max_custom_properties: 4_096,
            max_dependency_depth: 128,
            max_component_depth: 128,
            max_component_values: 65_536,
            max_value_bytes: 1_048_576,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComputationDiagnostic {
    pub property: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComputedValue {
    css_text: String,
    tokens: TokenStream,
}

impl ComputedValue {
    fn new(tokens: TokenStream) -> Self {
        Self {
            css_text: tokens.to_css().trim().to_owned(),
            tokens,
        }
    }

    fn within_limits(tokens: TokenStream, limits: &ComputationLimits) -> Option<Self> {
        if tokens.serialized_len_exceeds(limits.max_value_bytes)
            || tokens.component_count_exceeds(limits.max_component_values)
        {
            None
        } else {
            Some(Self::new(tokens))
        }
    }

    #[must_use]
    pub fn css_text(&self) -> &str {
        &self.css_text
    }

    /// A serialization safe to feed back into a tokenizer. Synthetic comments
    /// preserve component boundaries introduced by `var()` substitution.
    #[must_use]
    pub fn parseable_css(&self) -> String {
        self.tokens.to_parseable_css().trim().to_owned()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ComputedStyle {
    // These maps are shared behind `Arc` because layout copies a style when it
    // needs one alive across a recursive `&mut self` call. Cloning every
    // computed property (hundreds of entries per element) made GitHub's flex
    // intrinsic sizing spend seconds in `BTreeMap::clone`; an `Arc` bump keeps
    // those copies cheap. Mutation goes through `Arc::make_mut`, so a style
    // that is not shared is still updated in place.
    properties: Arc<BTreeMap<String, ComputedValue>>,
    typed_properties: Arc<BTreeMap<String, TypedPropertyValue>>,
    custom_properties: Arc<CustomProperties>,
    invalid_custom_properties: Arc<BTreeSet<String>>,
    /// Properties whose value came from a declaration in the document itself,
    /// as opposed to the initial value, an inherited value, or the user-agent
    /// stylesheet. See [`ComputedStyle::specified`].
    specified: Arc<BTreeSet<String>>,
    /// Initial values shared by every style built from one registry. An
    /// unspecified non-inherited property is not copied into `properties`; the
    /// accessors fall back to this table. That avoids hundreds of `BTreeMap`
    /// inserts and value clones per element on large pages.
    initial: Option<Arc<InitialValues>>,
    diagnostics: Arc<Vec<ComputationDiagnostic>>,
}

/// Custom properties as a persistent overlay over the parent's map.
///
/// A design-system stylesheet can declare a thousand custom properties on
/// `:root`, and a page can have thousands of elements. Materializing the full
/// inherited map on every element made each computed style copy the whole
/// table (and its token vectors), which on GitHub's 2,600-element page cost
/// seconds and hundreds of megabytes per style pass. An element that declares
/// nothing shares the parent layer; one that declares a few names stores only
/// those overrides.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CustomProperties {
    /// The parent element's resolved custom properties.
    inherited: Option<Arc<CustomProperties>>,
    /// Names this element declares with a valid computed value.
    overrides: Option<Arc<BTreeMap<String, ComputedValue>>>,
    /// Names whose declaration on this element produced the guaranteed-invalid
    /// value, so the parent's value must not show through.
    removed: Option<Arc<BTreeSet<String>>>,
    /// Registered `inherits: false` properties and their initial (or
    /// guaranteed-invalid) value. They are document-wide and therefore shared
    /// by every element instead of being copied into each override map.
    non_inheriting: Option<Arc<BTreeMap<String, Option<ComputedValue>>>>,
}

impl CustomProperties {
    fn get(&self, name: &str) -> Option<&ComputedValue> {
        if let Some(values) = &self.overrides
            && let Some(value) = values.get(name)
        {
            return Some(value);
        }
        if let Some(values) = &self.non_inheriting
            && let Some(value) = values.get(name)
        {
            return value.as_ref();
        }
        if let Some(removed) = &self.removed
            && removed.contains(name)
        {
            return None;
        }
        self.inherited.as_ref().and_then(|parent| parent.get(name))
    }
}

impl ComputedStyle {
    /// The computed value of `property` on this element, or `None` when there
    /// is none to read.
    ///
    /// # `None` is not an answer about support
    ///
    /// Two different questions have to be told apart, and this method cannot
    /// tell either of them:
    ///
    /// - **"Does this declaration apply here?"** - the cascade's own question.
    ///   The registry installs an initial value for every property it defines,
    ///   so `get` returns `Some` for all of them and the `Option` is `Some`
    ///   far more often than "the document wrote something" would suggest. Use
    ///   [`Self::specified`] for author intent; see `docs/visual_fidelity_gaps.md`
    ///   S22, which is the recorded instance of this going wrong.
    /// - **"Does this engine have such a property at all?"** - a question about
    ///   the engine, identical for every element, and the one a diagnostic or a
    ///   support oracle has to ask. `None` here means *both* "this element has
    ///   no value" and "no such property exists in this engine", and `Some`
    ///   covers everything from a fully implemented property to one the registry
    ///   defines with an initial value and no grammar at all. Use
    ///   [`crate::supports::property_support`] for that, which has three states
    ///   precisely so that "defined but not implemented" is not collapsed into
    ///   "implemented".
    ///
    /// The two questions are deliberately separate queries rather than one
    /// tri-state return, because support is a constant and this type is
    /// per-element: a single return value would put a constant inside a varying
    /// answer, and would change the meaning of a method that roughly a hundred
    /// call sites already read as "give me the value".
    #[must_use]
    pub fn get(&self, property: &str) -> Option<&ComputedValue> {
        if property.starts_with("--") {
            self.custom_properties.get(property)
        } else {
            self.properties.get(property).or_else(|| {
                self.initial
                    .as_ref()
                    .and_then(|initial| initial.values.get(property))
                    .filter(|value| !value.rejected)
                    .and_then(|value| value.value.as_ref())
            })
        }
    }

    /// Every property name this style can answer, including the shared
    /// initial values it does not store per element.
    pub fn property_names(&self) -> impl Iterator<Item = &str> {
        self.properties
            .keys()
            .map(String::as_str)
            .chain(self.initial.iter().flat_map(|initial| {
                initial
                    .values
                    .iter()
                    .filter(|(name, value)| !value.rejected && !self.properties.contains_key(*name))
                    .map(|(name, _)| name.as_str())
            }))
    }

    /// The number of distinct properties [`Self::property_names`] yields.
    #[must_use]
    pub fn property_count(&self) -> usize {
        self.properties.len()
            + self.initial.as_ref().map_or(0, |initial| {
                initial
                    .values
                    .iter()
                    .filter(|(name, value)| !value.rejected && !self.properties.contains_key(*name))
                    .count()
            })
    }

    /// Layout-facing values whose property grammars are implemented by this
    /// migration slice. Unsupported properties remain available only through
    /// the token-level map and are never mistaken for typed values.
    /// Replace one property with an animated value.
    ///
    /// The animation sampler works in CSS text because keyframes may name any
    /// property, including ones this engine keeps only token-level. The typed
    /// map is updated when the property has a grammar, so layout and paint see
    /// the same value the cascade would have produced. A value the grammar
    /// rejects drops the typed entry rather than leaving a stale one behind.
    pub fn set_animated_property(&mut self, property: &str, css: &str) {
        let Ok(tokens) = tokenize(css, &ComputationLimits::default()) else {
            return;
        };
        let value = ComputedValue::new(tokens);
        let parsed = parse_typed_property(property, css);
        // An animated logical longhand has to reach the physical slot layout
        // reads, exactly as a cascaded one does. `direction` itself is never
        // logical, so reading it before the mutation is safe even when the
        // property being animated is `direction`.
        let direction =
            self.get("direction")
                .map(ComputedValue::css_text)
                .map_or(Direction::Ltr, |value| {
                    if value.trim().eq_ignore_ascii_case("rtl") {
                        Direction::Rtl
                    } else {
                        Direction::Ltr
                    }
                });
        let physical = physical_longhand_for_logical(property, direction);
        Arc::make_mut(&mut self.properties).insert(property.to_owned(), value.clone());
        match parsed {
            Some(Ok(typed)) => {
                Arc::make_mut(&mut self.typed_properties)
                    .insert(property.to_owned(), typed.clone());
                if let Some(physical) = physical {
                    Arc::make_mut(&mut self.typed_properties).insert(physical.to_owned(), typed);
                }
            }
            _ => {
                Arc::make_mut(&mut self.typed_properties).remove(property);
                if let Some(physical) = physical {
                    Arc::make_mut(&mut self.typed_properties).remove(physical);
                }
            }
        }
        if let Some(physical) = physical {
            Arc::make_mut(&mut self.properties).insert(physical.to_owned(), value);
        }
    }

    #[must_use]
    pub fn typed_properties(&self) -> &BTreeMap<String, TypedPropertyValue> {
        &self.typed_properties
    }

    #[must_use]
    pub fn typed(&self, property: &str) -> Option<&TypedPropertyValue> {
        self.typed_properties.get(property).or_else(|| {
            self.initial
                .as_ref()
                .and_then(|initial| initial.values.get(property))
                .and_then(|value| value.typed.as_ref())
        })
    }

    #[must_use]
    pub fn invalid_custom_properties(&self) -> &BTreeSet<String> {
        &self.invalid_custom_properties
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[ComputationDiagnostic] {
        &self.diagnostics
    }

    /// Whether a declaration in the document itself set this property on this
    /// element, as opposed to the value merely being *present*.
    ///
    /// [`Self::get`] answers a different question. Because the registry
    /// installs an initial value for every property it defines, `get` returns
    /// `Some` for all of them, so "is this property set?" silently means "is
    /// this property registered?" once a property is registered at all. Code
    /// that needs to know whether the document said anything cannot use it.
    ///
    /// A few real rules depend on the difference. CSS 2.1 §17.5.3 has a
    /// row group with no `vertical-align` of its own take the table's, which
    /// is only expressible if "the row group said nothing" is distinguishable
    /// from "the row group's value happens to be the initial one".
    ///
    /// The boundary, which is CSS Cascade 5 §6.1.1's UA-origin boundary:
    ///
    /// - An author or user stylesheet rule, or a `style` attribute, counts.
    ///   The document asked for it.
    /// - An HTML presentational hint (`bgcolor`, `valign`, `align`, ...) counts.
    ///   It cascades at the user-agent origin, but it is an attribute the
    ///   document's author wrote, so it is author intent.
    /// - A user-agent stylesheet rule does **not** count. The engine styling
    ///   the document is not the document asking for something. This is the
    ///   distinction that matters in practice: `sub { vertical-align: sub }` in
    ///   a UA sheet must not read as the author having aligned anything.
    /// - An inherited value does **not** count for the descendant. The
    ///   ancestor's declaration is recorded on the ancestor, so a consumer
    ///   asking about the descendant still learns that the descendant itself
    ///   said nothing.
    /// - The initial value does not count, by definition.
    ///
    /// Shorthand expansion needs no special case: `margin: 1px` is expanded
    /// into its longhands by the cascade before the winner is chosen, so
    /// `margin-top` is recorded as specified exactly when `margin` was.
    ///
    /// `inherit` and the other CSS-wide keywords *do* count, because the
    /// document wrote them; a consumer that wanted the inherited value can
    /// read it with `get`. Note that `inherit` on a non-inherited property
    /// such as `vertical-align` takes the parent's value while still counting
    /// as specified here, which is the honest reading: the document did say
    /// something about this element.
    #[must_use]
    pub fn specified(&self, property: &str) -> bool {
        self.specified.contains(property)
    }

    /// Every property the document set on this element, in name order.
    #[must_use]
    pub fn specified_properties(&self) -> &BTreeSet<String> {
        &self.specified
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TokenClass {
    Whitespace,
    Comment,
    Comma,
    Ident(String),
    Other,
}

impl TokenClass {
    const fn insignificant(&self) -> bool {
        matches!(self, Self::Whitespace | Self::Comment)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ComponentValue {
    Token {
        class: TokenClass,
        source: String,
    },
    Function {
        name: String,
        prefix: String,
        values: TokenStream,
        suffix: String,
    },
    Block {
        prefix: String,
        values: TokenStream,
        suffix: String,
    },
    Substitution(Box<TokenStream>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TokenStream(Vec<ComponentValue>);

impl TokenStream {
    fn to_css(&self) -> String {
        let mut output = String::new();
        self.write_css(&mut output, false);
        output
    }

    fn to_parseable_css(&self) -> String {
        let mut output = String::new();
        self.write_css(&mut output, true);
        output
    }

    fn write_css(&self, output: &mut String, preserve_substitution_boundaries: bool) {
        for value in &self.0 {
            match value {
                ComponentValue::Token { source, .. } => output.push_str(source),
                ComponentValue::Function {
                    prefix,
                    values,
                    suffix,
                    ..
                }
                | ComponentValue::Block {
                    prefix,
                    values,
                    suffix,
                } => {
                    output.push_str(prefix);
                    values.write_css(output, preserve_substitution_boundaries);
                    output.push_str(suffix);
                }
                ComponentValue::Substitution(values) => {
                    if preserve_substitution_boundaries {
                        output.push_str("/**/");
                    }
                    values.write_css(output, preserve_substitution_boundaries);
                    if preserve_substitution_boundaries {
                        output.push_str("/**/");
                    }
                }
            }
        }
    }

    fn serialized_len_exceeds(&self, limit: usize) -> bool {
        fn add_stream(stream: &TokenStream, total: &mut usize, limit: usize) -> bool {
            for value in &stream.0 {
                let exceeded = match value {
                    ComponentValue::Token { source, .. } => add(total, source.len(), limit),
                    ComponentValue::Function {
                        prefix,
                        values,
                        suffix,
                        ..
                    }
                    | ComponentValue::Block {
                        prefix,
                        values,
                        suffix,
                    } => {
                        add(total, prefix.len(), limit)
                            || add_stream(values, total, limit)
                            || add(total, suffix.len(), limit)
                    }
                    ComponentValue::Substitution(values) => add_stream(values, total, limit),
                };
                if exceeded {
                    return true;
                }
            }
            false
        }

        fn add(total: &mut usize, amount: usize, limit: usize) -> bool {
            *total = total.saturating_add(amount);
            *total > limit
        }

        add_stream(self, &mut 0, limit)
    }

    fn component_count_exceeds(&self, limit: usize) -> bool {
        self.component_count_up_to(limit).is_none()
    }

    fn component_count_up_to(&self, limit: usize) -> Option<usize> {
        fn count_stream(stream: &TokenStream, total: &mut usize, limit: usize) -> bool {
            for value in &stream.0 {
                *total = total.saturating_add(1);
                if *total > limit {
                    return true;
                }
                let nested = match value {
                    ComponentValue::Function { values, .. }
                    | ComponentValue::Block { values, .. } => Some(values),
                    ComponentValue::Substitution(values) => Some(values.as_ref()),
                    ComponentValue::Token { .. } => None,
                };
                if nested.is_some_and(|values| count_stream(values, total, limit)) {
                    return true;
                }
            }
            false
        }

        let mut total = 0;
        if count_stream(self, &mut total, limit) {
            None
        } else {
            Some(total)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TokenStreamError {
    InvalidToken,
    ComponentLimit,
    NestingLimit,
}

impl fmt::Display for TokenStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidToken => formatter.write_str("invalid CSS component value"),
            Self::ComponentLimit => formatter.write_str("CSS component-value limit exceeded"),
            Self::NestingLimit => formatter.write_str("CSS component nesting limit exceeded"),
        }
    }
}

fn tokenize(source: &str, limits: &ComputationLimits) -> Result<TokenStream, String> {
    let mut input = ParserInput::new(source);
    let mut parser = Parser::new(&mut input);
    let mut count = 0;
    parse_component_values(&mut parser, source, &mut count, limits, 0)
        .map_err(|error| error.kind.to_string())
}

fn parse_component_values<'i>(
    input: &mut Parser<'i, '_>,
    source: &'i str,
    count: &mut usize,
    limits: &ComputationLimits,
    depth: usize,
) -> Result<TokenStream, ParseError<'i, TokenStreamError>> {
    let mut values = Vec::new();
    loop {
        let start = input.position().byte_index();
        let Ok(token) = input.next_including_whitespace_and_comments().cloned() else {
            break;
        };
        *count = count.saturating_add(1);
        if *count > limits.max_component_values {
            return Err(input.new_custom_error(TokenStreamError::ComponentLimit));
        }
        if token.is_parse_error() {
            return Err(input.new_custom_error(TokenStreamError::InvalidToken));
        }

        match token {
            Token::Function(name) => {
                if depth >= limits.max_component_depth {
                    return Err(input.new_custom_error(TokenStreamError::NestingLimit));
                }
                let prefix_end = input.position().byte_index();
                let (nested, nested_end) = input.parse_nested_block(|nested| {
                    let values = parse_component_values(
                        nested,
                        source,
                        count,
                        limits,
                        depth.saturating_add(1),
                    )?;
                    Ok((values, nested.position().byte_index()))
                })?;
                let end = input.position().byte_index();
                values.push(ComponentValue::Function {
                    name: name.to_string(),
                    prefix: source[start..prefix_end].to_owned(),
                    values: nested,
                    suffix: source[nested_end..end].to_owned(),
                });
            }
            Token::ParenthesisBlock | Token::SquareBracketBlock | Token::CurlyBracketBlock => {
                if depth >= limits.max_component_depth {
                    return Err(input.new_custom_error(TokenStreamError::NestingLimit));
                }
                let prefix_end = input.position().byte_index();
                let (nested, nested_end) = input.parse_nested_block(|nested| {
                    let values = parse_component_values(
                        nested,
                        source,
                        count,
                        limits,
                        depth.saturating_add(1),
                    )?;
                    Ok((values, nested.position().byte_index()))
                })?;
                let end = input.position().byte_index();
                values.push(ComponentValue::Block {
                    prefix: source[start..prefix_end].to_owned(),
                    values: nested,
                    suffix: source[nested_end..end].to_owned(),
                });
            }
            other => {
                let end = input.position().byte_index();
                let class = match other {
                    Token::WhiteSpace(_) => TokenClass::Whitespace,
                    Token::Comment(_) => TokenClass::Comment,
                    Token::Comma => TokenClass::Comma,
                    Token::Ident(name) => TokenClass::Ident(name.to_string()),
                    _ => TokenClass::Other,
                };
                values.push(ComponentValue::Token {
                    class,
                    source: source[start..end].to_owned(),
                });
            }
        }
    }
    Ok(TokenStream(values))
}

#[derive(Clone, Debug)]
enum CustomSource {
    Resolved(TokenStream),
    Specified(TokenStream),
}

struct VarArguments {
    name: String,
    fallback: Option<TokenStream>,
}

fn var_arguments(values: &TokenStream) -> Option<VarArguments> {
    let first = values.0.iter().position(|value| !is_insignificant(value))?;
    let name = match &values.0[first] {
        ComponentValue::Token {
            class: TokenClass::Ident(name),
            ..
        } if name.starts_with("--") => name.clone(),
        _ => return None,
    };
    let next = values.0[first + 1..]
        .iter()
        .position(|value| !is_insignificant(value))
        .map(|offset| first + 1 + offset);
    match next {
        None => Some(VarArguments {
            name,
            fallback: None,
        }),
        Some(index)
            if matches!(
                values.0[index],
                ComponentValue::Token {
                    class: TokenClass::Comma,
                    ..
                }
            ) =>
        {
            Some(VarArguments {
                name,
                fallback: Some(TokenStream(values.0[index + 1..].to_vec())),
            })
        }
        Some(_) => None,
    }
}

fn is_insignificant(value: &ComponentValue) -> bool {
    matches!(
        value,
        ComponentValue::Token { class, .. } if class.insignificant()
    )
}

fn collect_dependencies(stream: &TokenStream, output: &mut BTreeSet<String>) -> bool {
    for value in &stream.0 {
        match value {
            ComponentValue::Function { name, values, .. } if name.eq_ignore_ascii_case("var") => {
                let Some(arguments) = var_arguments(values) else {
                    return false;
                };
                output.insert(arguments.name);
                if let Some(fallback) = arguments.fallback
                    && !collect_dependencies(&fallback, output)
                {
                    return false;
                }
            }
            ComponentValue::Function { values, .. } | ComponentValue::Block { values, .. } => {
                if !collect_dependencies(values, output) {
                    return false;
                }
            }
            ComponentValue::Substitution(values) => {
                if !collect_dependencies(values, output) {
                    return false;
                }
            }
            ComponentValue::Token { .. } => {}
        }
    }
    true
}

fn find_cycles(
    graph: &BTreeMap<String, Vec<String>>,
    max_depth: usize,
) -> (BTreeSet<String>, BTreeSet<String>) {
    fn visit(
        name: &str,
        graph: &BTreeMap<String, Vec<String>>,
        states: &mut HashMap<String, u8>,
        path: &mut Vec<String>,
        cycles: &mut BTreeSet<String>,
        limited: &mut BTreeSet<String>,
        max_depth: usize,
    ) {
        if path.len() >= max_depth {
            limited.extend(path.iter().cloned());
            limited.insert(name.to_owned());
            return;
        }
        match states.get(name).copied().unwrap_or_default() {
            2 => return,
            1 => {
                if let Some(start) = path.iter().position(|candidate| candidate == name) {
                    cycles.extend(path[start..].iter().cloned());
                }
                return;
            }
            _ => {}
        }
        states.insert(name.to_owned(), 1);
        path.push(name.to_owned());
        if let Some(dependencies) = graph.get(name) {
            for dependency in dependencies {
                visit(dependency, graph, states, path, cycles, limited, max_depth);
            }
        }
        path.pop();
        states.insert(name.to_owned(), 2);
    }

    let mut states = HashMap::new();
    let mut path = Vec::new();
    let mut cycles = BTreeSet::new();
    let mut limited = BTreeSet::new();
    for name in graph.keys() {
        visit(
            name,
            graph,
            &mut states,
            &mut path,
            &mut cycles,
            &mut limited,
            max_depth,
        );
    }
    (cycles, limited)
}

struct CustomResolver<'a> {
    sources: &'a BTreeMap<String, CustomSource>,
    invalid: &'a BTreeSet<String>,
    /// Resolved custom properties inherited from the parent. When the element
    /// declares no custom properties of its own, these are consulted directly
    /// instead of cloning the whole inherited map into `sources`.
    inherited: Option<&'a CustomProperties>,
    inherited_invalid: Option<&'a BTreeSet<String>>,
    /// The document's registrations, so a substituted value is checked against
    /// its syntax before another property can use it.
    registered: Option<&'a RegisteredProperties>,
    /// The unset value for each registered property, precomputed once per
    /// element. An invalid value falls back to this before substitution.
    registered_fallbacks: &'a BTreeMap<String, Option<TokenStream>>,
    memo: BTreeMap<String, Option<TokenStream>>,
    limits: &'a ComputationLimits,
    diagnostics: &'a mut Vec<ComputationDiagnostic>,
    diagnosed: BTreeSet<String>,
}

impl CustomResolver<'_> {
    fn resolve(&mut self, name: &str, depth: usize) -> Option<TokenStream> {
        if let Some(value) = self.memo.get(name) {
            return value.clone();
        }
        if self.invalid.contains(name) || depth >= self.limits.max_dependency_depth {
            // A registered property whose own value is invalid at computed-value
            // time computes to its unset value, so references to it still have
            // something to substitute.
            if let Some(Some(fallback)) = self.registered_fallbacks.get(name) {
                self.memo.insert(name.to_owned(), Some(fallback.clone()));
                return Some(fallback.clone());
            }
            if depth >= self.limits.max_dependency_depth && self.diagnosed.insert(name.to_owned()) {
                self.diagnostics.push(ComputationDiagnostic {
                    property: Some(name.to_owned()),
                    message: "custom-property dependency depth exceeded".to_owned(),
                });
            }
            self.memo.insert(name.to_owned(), None);
            return None;
        }
        if !self.sources.contains_key(name) {
            // A registered non-inheriting property never reads the parent's
            // value when this element did not declare it. Its initial value
            // (or guaranteed-invalid value) is the computed value instead.
            let non_inheriting = self
                .registered
                .and_then(|registered| registered.definition(name))
                .is_some_and(|registration| !registration.inherits);
            if non_inheriting {
                let resolved = self.registered_fallbacks.get(name).cloned().flatten();
                self.memo.insert(name.to_owned(), resolved.clone());
                return resolved;
            }
            let resolved = if self
                .inherited_invalid
                .is_some_and(|invalid| invalid.contains(name))
            {
                None
            } else {
                self.inherited
                    .and_then(|values| values.get(name))
                    .map(|value| value.tokens.clone())
            };
            self.memo.insert(name.to_owned(), resolved.clone());
            return resolved;
        }
        let source = self.sources.get(name)?.clone();
        let mut resolved = match source {
            CustomSource::Resolved(value) => Some(value),
            CustomSource::Specified(value) => self.substitute(&value, depth.saturating_add(1)),
        };
        resolved = resolved.filter(|value| {
            if !value.serialized_len_exceeds(self.limits.max_value_bytes)
                && !value.component_count_exceeds(self.limits.max_component_values)
            {
                true
            } else {
                if self.diagnosed.insert(name.to_owned()) {
                    self.diagnostics.push(ComputationDiagnostic {
                        property: Some(name.to_owned()),
                        message: "computed custom-property value exceeds byte limit".to_owned(),
                    });
                }
                false
            }
        });
        if let Some(value) = resolved.as_ref()
            && let Some(registration) = self
                .registered
                .and_then(|registered| registered.definition(name))
        {
            let css = ComputedValue::new(value.clone()).parseable_css();
            if !registration.syntax.matches(&css) {
                if self.diagnosed.insert(name.to_owned()) {
                    self.diagnostics.push(ComputationDiagnostic {
                        property: Some(name.to_owned()),
                        message: format!(
                            "value does not match the registered syntax for {name}; \
                             using the unset fallback"
                        ),
                    });
                }
                resolved = self.registered_fallbacks.get(name).cloned().flatten();
            }
        }
        self.memo.insert(name.to_owned(), resolved.clone());
        resolved
    }

    fn substitute(&mut self, stream: &TokenStream, depth: usize) -> Option<TokenStream> {
        if depth >= self.limits.max_dependency_depth {
            return None;
        }
        let mut output = Vec::new();
        let mut output_count = 0_usize;
        for value in &stream.0 {
            let substituted = match value {
                ComponentValue::Function { name, values, .. }
                    if name.eq_ignore_ascii_case("var") =>
                {
                    let arguments = var_arguments(values)?;
                    let replacement = self
                        .resolve(&arguments.name, depth.saturating_add(1))
                        .or_else(|| {
                            arguments.fallback.as_ref().and_then(|fallback| {
                                self.substitute(fallback, depth.saturating_add(1))
                            })
                        })?;
                    ComponentValue::Substitution(Box::new(replacement))
                }
                ComponentValue::Function {
                    name,
                    prefix,
                    values,
                    suffix,
                } => ComponentValue::Function {
                    name: name.clone(),
                    prefix: prefix.clone(),
                    values: self.substitute(values, depth.saturating_add(1))?,
                    suffix: suffix.clone(),
                },
                ComponentValue::Block {
                    prefix,
                    values,
                    suffix,
                } => ComponentValue::Block {
                    prefix: prefix.clone(),
                    values: self.substitute(values, depth.saturating_add(1))?,
                    suffix: suffix.clone(),
                },
                ComponentValue::Substitution(values) => {
                    ComponentValue::Substitution(values.clone())
                }
                ComponentValue::Token { .. } => value.clone(),
            };
            let nested_count = match &substituted {
                ComponentValue::Function { values, .. } | ComponentValue::Block { values, .. } => {
                    values.component_count_up_to(
                        self.limits
                            .max_component_values
                            .saturating_sub(output_count.saturating_add(1)),
                    )?
                }
                ComponentValue::Substitution(values) => values.component_count_up_to(
                    self.limits
                        .max_component_values
                        .saturating_sub(output_count.saturating_add(1)),
                )?,
                ComponentValue::Token { .. } => 0,
            };
            output_count = output_count.saturating_add(1).saturating_add(nested_count);
            if output_count > self.limits.max_component_values {
                return None;
            }
            output.push(substituted);
        }
        let output = TokenStream(output);
        if output.component_count_exceeds(self.limits.max_component_values)
            || output.serialized_len_exceeds(self.limits.max_value_bytes)
        {
            None
        } else {
            Some(output)
        }
    }
}

/// Absolute pixel size of the default user font (`medium`, 16px). The
/// computation stage has no viewport-independent root-font override; layout
/// uses the same default in `LengthResolutionContext`.
const DEFAULT_ROOT_FONT_SIZE: f32 = 16.0;

/// The computed and typed forms of every non-inherited registered property's
/// initial value.
///
/// An element that declares nothing for such a property computes exactly this
/// value, whatever its parent or ancestors are, so the work of producing it
/// (tokenising the initial text and parsing its grammar) is done once per
/// document rather than once per element.
#[derive(Debug, PartialEq)]
struct InitialValues {
    values: BTreeMap<String, InitialValue>,
    /// Registry properties that still need per-element work when the element
    /// does not declare them: inherited properties take the parent's computed
    /// value, and a non-inherited property without a usable shared initial has
    /// to run its fallback and diagnostics. Everything else is answered by
    /// [`ComputedStyle::initial`] at read time.
    per_element: Vec<String>,
}

#[derive(Debug, PartialEq)]
struct InitialValue {
    value: Option<ComputedValue>,
    typed: Option<TypedPropertyValue>,
    /// The grammar rejects the initial value. Such a property keeps the full
    /// per-element path, whose fallback and diagnostic depend on the element.
    rejected: bool,
}

impl InitialValues {
    fn new(registry: &PropertyRegistry, limits: &ComputationLimits) -> Self {
        let values: BTreeMap<String, InitialValue> = registry
            .iter()
            .filter(|(_, definition)| !definition.inherited)
            .map(|(name, definition)| {
                let value = tokenize(&definition.initial_value, limits)
                    .ok()
                    .and_then(|tokens| ComputedValue::within_limits(tokens, limits));
                let parsed = value
                    .as_ref()
                    .and_then(|value| parse_typed_property(name, &value.parseable_css()));
                let (typed, rejected) = match parsed {
                    Some(Ok(typed)) => (Some(typed), false),
                    Some(Err(_)) => (None, true),
                    None => (None, false),
                };
                (
                    name.clone(),
                    InitialValue {
                        value,
                        typed,
                        rejected,
                    },
                )
            })
            .collect();
        let per_element = registry
            .iter()
            .filter(|(name, definition)| {
                definition.inherited || values.get(*name).is_none_or(|initial| initial.rejected)
            })
            .map(|(name, _)| name.clone())
            .collect();
        Self {
            values,
            per_element,
        }
    }
}

/// The document's `@property` registrations, with their initial values
/// tokenized once.
///
/// A registration is document-wide rather than per-element, so building this
/// table once keeps `@property --x { syntax: "<length>"; ... }` from
/// re-tokenizing `0px` for every element that does not declare `--x`.
#[derive(Debug, Default)]
struct RegisteredProperties {
    definitions: BTreeMap<String, PropertyRegistration>,
    initials: BTreeMap<String, ComputedValue>,
    /// `inherits: false` registrations and their initial value (`None` for the
    /// guaranteed-invalid value). Shared by every computed style in a document
    /// so these names never have to be copied into per-element maps.
    non_inheriting: Option<Arc<BTreeMap<String, Option<ComputedValue>>>>,
}

impl RegisteredProperties {
    fn new(
        definitions: BTreeMap<String, PropertyRegistration>,
        limits: &ComputationLimits,
    ) -> Self {
        let initials: BTreeMap<String, ComputedValue> = definitions
            .iter()
            .filter_map(|(name, registration)| {
                let source = registration.initial_value.as_deref()?;
                let tokens = tokenize(source, limits).ok()?;
                Some((name.clone(), ComputedValue::within_limits(tokens, limits)?))
            })
            .collect();
        let non_inheriting: BTreeMap<String, Option<ComputedValue>> = definitions
            .iter()
            .filter(|(_, registration)| !registration.inherits)
            .map(|(name, _)| (name.clone(), initials.get(name).cloned()))
            .collect();
        Self {
            definitions,
            initials,
            non_inheriting: (!non_inheriting.is_empty()).then(|| Arc::new(non_inheriting)),
        }
    }

    fn initial(&self, name: &str) -> Option<&ComputedValue> {
        self.initials.get(name)
    }

    fn definition(&self, name: &str) -> Option<&PropertyRegistration> {
        self.definitions.get(name)
    }
}

/// Answers `style()` container features from the top-down style pass.
///
/// The style map only ever contains ancestors of the element being cascaded:
/// `compute_document_styles_with_hints` walks parent-before-child, so a style
/// query cannot observe a descendant and cannot form a cycle.
struct ComputedStyleQueries<'a> {
    styles: &'a BTreeMap<NodeId, ComputedStyle>,
    /// The originating element's style while its generated-content
    /// pseudo-element is cascaded. `styles` does not contain it yet: the
    /// element's own entry is inserted after the pseudo-element loop.
    current: Option<(NodeId, &'a ComputedStyle)>,
    registered: &'a RegisteredProperties,
    limits: &'a ComputationLimits,
}

impl ComputedStyleQueries<'_> {
    fn style_for(&self, element: NodeId) -> Option<&ComputedStyle> {
        match self.current {
            Some((node, style)) if node == element => Some(style),
            _ => self.styles.get(&element),
        }
    }
}

impl StyleQueryEnvironment for ComputedStyleQueries<'_> {
    fn container_has_name(&self, element: NodeId, name: &str) -> bool {
        self.style_for(element).is_some_and(|style| {
            style.get("container-name").is_some_and(|value| {
                parse_container_names(value.css_text())
                    .iter()
                    .any(|candidate| candidate == name)
            })
        })
    }

    fn style_feature_matches(&self, element: NodeId, feature: &StyleFeature) -> bool {
        self.style_for(element).is_some_and(|style| {
            style_feature_matches(style, feature, self.registered, self.limits)
        })
    }
}

/// CSS Conditional 5 §3.1: match one `style()` feature against the query
/// container's computed style.
fn style_feature_matches(
    style: &ComputedStyle,
    feature: &StyleFeature,
    registered: &RegisteredProperties,
    limits: &ComputationLimits,
) -> bool {
    let custom = feature.name.starts_with("--");
    let Some(value) = feature.value.as_deref() else {
        return boolean_style_feature_matches(style, feature, registered, custom);
    };
    if is_cascade_dependent_style_value(value) {
        return false;
    }
    let Some(computed) = style.get(&feature.name) else {
        return false;
    };
    let Ok(query_tokens) = tokenize(value, limits) else {
        return false;
    };
    if !custom {
        return token_streams_match(&computed.tokens, &query_tokens, true);
    }
    match registered.definition(&feature.name) {
        Some(registration) if !matches!(registration.syntax, RegisteredSyntax::Universal) => {
            // A registered property compares its computed value with the
            // declaration value. Both must match the registered syntax; then
            // token equality is the common case, and a color registration also
            // compares equal colors written differently.
            if !registration.syntax.matches(value)
                || !registration.syntax.matches(computed.css_text())
            {
                return false;
            }
            token_streams_match(&computed.tokens, &query_tokens, false)
                || (syntax_is_color(&registration.syntax)
                    && color_values_match(computed.css_text(), value))
        }
        // Unregistered custom properties compare as token streams; CSS
        // Properties and Values 1 gives them no canonicalization.
        _ => token_streams_match(&computed.tokens, &query_tokens, false),
    }
}

/// Boolean style feature: true when the computed value differs from the
/// property's initial value. An unregistered custom property has no initial
/// value, so any computed value satisfies it.
fn boolean_style_feature_matches(
    style: &ComputedStyle,
    feature: &StyleFeature,
    registered: &RegisteredProperties,
    custom: bool,
) -> bool {
    if custom {
        let Some(computed) = style.get(&feature.name) else {
            return false;
        };
        if let Some(initial) = registered.initial(&feature.name) {
            return !token_streams_match(&computed.tokens, &initial.tokens, false);
        }
        return true;
    }
    let Some(computed) = style.get(&feature.name) else {
        return false;
    };
    let Some(initial) = style
        .initial
        .as_ref()
        .and_then(|initial| initial.values.get(&feature.name))
        .filter(|initial| !initial.rejected)
        .and_then(|initial| initial.value.as_ref())
    else {
        return false;
    };
    !token_streams_match(&computed.tokens, &initial.tokens, true)
}

/// The CSS-wide keywords are not declarations a style feature may match, and
/// `!important` is invalid inside a feature.
fn is_cascade_dependent_style_value(value: &str) -> bool {
    let value = value.trim();
    if value
        .to_ascii_lowercase()
        .split_ascii_whitespace()
        .any(|part| part == "!important")
    {
        return true;
    }
    matches!(
        value.to_ascii_lowercase().as_str(),
        "initial" | "inherit" | "unset" | "revert" | "revert-layer"
    )
}

/// Whether every alternative of a registration is a single `<color>`.
fn syntax_is_color(syntax: &RegisteredSyntax) -> bool {
    let RegisteredSyntax::Alternatives(alternatives) = syntax else {
        return false;
    };
    !alternatives.is_empty()
        && alternatives.iter().all(|alternative| {
            alternative.terms.len() == 1
                && alternative.terms[0].kind == SyntaxKind::Color
                && alternative.terms[0].multiplier == SyntaxMultiplier::Single
        })
}

/// Compare two colors after parsing them with the engine's color grammar, so a
/// registered `<color>` property matches `blue`, `#00f`, and `rgb(0 0 255)`.
fn color_values_match(left: &str, right: &str) -> bool {
    matches!(
        (
            parse_typed_property("color", left),
            parse_typed_property("color", right)
        ),
        (
            Some(Ok(TypedPropertyValue::Color(left))),
            Some(Ok(TypedPropertyValue::Color(right)))
        ) if left == right
    )
}

/// Compare two token streams, ignoring whitespace and comments. Custom
/// properties are case-sensitive; ordinary property names fold identifiers.
fn token_streams_match(left: &TokenStream, right: &TokenStream, fold_identifiers: bool) -> bool {
    let mut left = left.0.iter().filter(|value| !value.is_insignificant());
    let mut right = right.0.iter().filter(|value| !value.is_insignificant());
    loop {
        match (left.next(), right.next()) {
            (None, None) => return true,
            (Some(left), Some(right)) if component_values_match(left, right, fold_identifiers) => {}
            _ => return false,
        }
    }
}

fn component_values_match(
    left: &ComponentValue,
    right: &ComponentValue,
    fold_identifiers: bool,
) -> bool {
    match (left, right) {
        (
            ComponentValue::Token {
                class: left_class,
                source: left,
            },
            ComponentValue::Token {
                class: right_class,
                source: right,
            },
        ) => {
            if fold_identifiers
                && matches!(left_class, TokenClass::Ident(_))
                && matches!(right_class, TokenClass::Ident(_))
            {
                left.eq_ignore_ascii_case(right)
            } else {
                left == right
            }
        }
        (
            ComponentValue::Function {
                name: left_name,
                values: left,
                ..
            },
            ComponentValue::Function {
                name: right_name,
                values: right,
                ..
            },
        ) => {
            left_name.eq_ignore_ascii_case(right_name)
                && token_streams_match(left, right, fold_identifiers)
        }
        (
            ComponentValue::Block {
                prefix: left_prefix,
                values: left,
                ..
            },
            ComponentValue::Block {
                prefix: right_prefix,
                values: right,
                ..
            },
        ) => left_prefix == right_prefix && token_streams_match(left, right, fold_identifiers),
        (ComponentValue::Substitution(left), ComponentValue::Substitution(right)) => {
            token_streams_match(left, right, fold_identifiers)
        }
        _ => false,
    }
}

impl ComponentValue {
    fn is_insignificant(&self) -> bool {
        matches!(self, Self::Token { class, .. } if class.insignificant())
    }
}

/// Resolve token-level computed values for one element.
#[must_use]
pub fn compute_style(
    cascaded: &CascadedStyle,
    parent: Option<&ComputedStyle>,
    registry: &PropertyRegistry,
    limits: &ComputationLimits,
) -> ComputedStyle {
    compute_style_with_initials(
        cascaded,
        parent,
        registry,
        limits,
        &Arc::new(InitialValues::new(registry, limits)),
        &RegisteredProperties::default(),
    )
}

/// Apply one registry property's computed value to an element's maps.
///
/// The registry loop and the declared-property loop both end here so the
/// grammar fallback, authored tracking and typed-map maintenance stay in one
/// place.
#[allow(clippy::too_many_arguments)]
fn apply_registered_property(
    name: &str,
    definition: &PropertyDefinition,
    cascaded: &CascadedStyle,
    parent: Option<&ComputedStyle>,
    resolver: &mut CustomResolver<'_>,
    limits: &ComputationLimits,
    properties: &mut BTreeMap<String, ComputedValue>,
    typed_properties: &mut BTreeMap<String, TypedPropertyValue>,
    specified: &mut BTreeSet<String>,
) {
    let mut computed =
        compute_registered_property(name, definition, cascaded, parent, resolver, limits);
    // Whether the document supplied this property at all, decided before the
    // grammar check below can discard the value.
    let authored = cascaded
        .get(name)
        .is_some_and(super::cascade::CascadedValue::is_authored);
    let mut accepted = true;
    if let Some(value) = computed.as_ref()
        && let Some(typed) = parse_typed_property(name, &value.parseable_css())
    {
        match typed {
            Ok(value) => {
                typed_properties.insert(name.to_owned(), value);
            }
            Err(error) => {
                resolver.diagnostics.push(ComputationDiagnostic {
                    property: Some(name.to_owned()),
                    message: format!("{error}; using the unset fallback"),
                });
                computed = registered_unset_value(name, definition, parent, limits);
                // A value the grammar rejected is not a value the document
                // successfully set, so it does not count as specified.
                accepted = false;
                if let Some(fallback) = computed.as_ref()
                    && let Some(Ok(value)) = parse_typed_property(name, &fallback.parseable_css())
                {
                    typed_properties.insert(name.to_owned(), value);
                }
            }
        }
    }
    if let Some(value) = computed {
        properties.insert(name.to_owned(), value);
    }
    if authored && accepted {
        specified.insert(name.to_owned());
    }
}

#[allow(clippy::too_many_lines)] // Kept linear to mirror the ordered CSS computed-value stages.
fn compute_style_with_initials(
    cascaded: &CascadedStyle,
    parent: Option<&ComputedStyle>,
    registry: &PropertyRegistry,
    limits: &ComputationLimits,
    initials: &Arc<InitialValues>,
    registered: &RegisteredProperties,
) -> ComputedStyle {
    let mut diagnostics = Vec::new();
    let cascaded_custom = cascaded
        .properties()
        .iter()
        .filter(|(name, _)| name.starts_with("--"))
        .collect::<Vec<_>>();
    // An element that declares no custom properties shares the parent's layer
    // unchanged. A document that registers an `inherits: false` property still
    // needs a thin child layer, because that property is answered by the shared
    // `non_inheriting` table before the inherited chain is consulted; the layer
    // holds no per-element map.
    let inherits_custom =
        cascaded_custom.is_empty() && parent.is_some() && registered.non_inheriting.is_none();
    let mut invalid = if inherits_custom {
        BTreeSet::new()
    } else {
        let mut invalid = parent
            .map(|style| (*style.invalid_custom_properties).clone())
            .unwrap_or_default();
        // A registered non-inheriting property never carries the parent's
        // value (or invalidity) into this element.
        for (name, registration) in &registered.definitions {
            if !registration.inherits {
                invalid.remove(name);
            }
        }
        invalid
    };
    // Only the element's own declarations are sources. Inherited values live in
    // the parent layer, which the resolver consults without copying. At the
    // root there is no parent, so inheriting registrations take their initial
    // value as a source; non-inheriting registrations are answered by the
    // document-wide `non_inheriting` table.
    let mut sources: BTreeMap<String, CustomSource> = BTreeMap::new();
    if parent.is_none() {
        for (name, value) in &registered.initials {
            if registered
                .definition(name)
                .is_none_or(|registration| registration.inherits)
            {
                sources.insert(name.clone(), CustomSource::Resolved(value.tokens.clone()));
            }
        }
    }
    for (name, registration) in &registered.definitions {
        if !registration.inherits && !registered.initials.contains_key(name) {
            invalid.insert(name.clone());
        }
    }
    if cascaded_custom.len() > limits.max_custom_properties {
        diagnostics.push(ComputationDiagnostic {
            property: None,
            message: "custom-property count limit exceeded".to_owned(),
        });
    }
    for (name, value) in cascaded_custom
        .into_iter()
        .take(limits.max_custom_properties)
    {
        let keyword = css_wide_keyword(&value.value);
        let registration = registered.definition(name);
        let parent_value = parent.and_then(|style| style.custom_properties.get(name));
        let initial_value = registered.initial(name);
        match keyword {
            Some(CssWideKeyword::Inherit) => {
                let inherited = parent_value.or(initial_value);
                match inherited {
                    Some(inherited) => {
                        sources.insert(
                            name.clone(),
                            CustomSource::Resolved(inherited.tokens.clone()),
                        );
                        invalid.remove(name);
                    }
                    None => {
                        sources.remove(name);
                        invalid.insert(name.clone());
                    }
                }
            }
            Some(CssWideKeyword::Unset) => {
                let unset = if registration.is_none_or(|registration| registration.inherits) {
                    parent_value.or(initial_value)
                } else {
                    initial_value
                };
                match unset {
                    Some(unset) => {
                        sources.insert(name.clone(), CustomSource::Resolved(unset.tokens.clone()));
                        invalid.remove(name);
                    }
                    None => {
                        sources.remove(name);
                        invalid.insert(name.clone());
                    }
                }
            }
            Some(CssWideKeyword::Initial) => match initial_value {
                Some(initial) => {
                    sources.insert(name.clone(), CustomSource::Resolved(initial.tokens.clone()));
                    invalid.remove(name);
                }
                None => {
                    sources.remove(name);
                    invalid.insert(name.clone());
                }
            },
            Some(CssWideKeyword::Revert | CssWideKeyword::RevertLayer) => {
                sources.remove(name);
                invalid.insert(name.clone());
                diagnostics.push(ComputationDiagnostic {
                    property: Some(name.clone()),
                    message: "unresolved cascade rollback keyword reached computed-value stage"
                        .to_owned(),
                });
            }
            None => match tokenize(&value.value, limits) {
                Ok(tokens) => {
                    sources.insert(name.clone(), CustomSource::Specified(tokens));
                    invalid.remove(name);
                }
                Err(message) => {
                    sources.remove(name);
                    invalid.insert(name.clone());
                    diagnostics.push(ComputationDiagnostic {
                        property: Some(name.clone()),
                        message,
                    });
                }
            },
        }
    }

    let mut custom_overrides = BTreeMap::new();
    let mut resolution_invalid = BTreeSet::new();
    if !inherits_custom {
        let mut graph = BTreeMap::new();
        for (name, source) in &sources {
            if let CustomSource::Specified(tokens) = source {
                let mut dependencies = BTreeSet::new();
                if collect_dependencies(tokens, &mut dependencies) {
                    graph.insert(
                        name.clone(),
                        dependencies
                            .into_iter()
                            .filter(|dependency| {
                                matches!(sources.get(dependency), Some(CustomSource::Specified(_)))
                            })
                            .collect(),
                    );
                } else {
                    invalid.insert(name.clone());
                    diagnostics.push(ComputationDiagnostic {
                        property: Some(name.clone()),
                        message: "invalid var() syntax".to_owned(),
                    });
                }
            }
        }
        let (cycles, depth_limited) = find_cycles(&graph, limits.max_dependency_depth);
        for name in &cycles {
            diagnostics.push(ComputationDiagnostic {
                property: Some(name.clone()),
                message: "custom-property dependency cycle".to_owned(),
            });
        }
        for name in &depth_limited {
            diagnostics.push(ComputationDiagnostic {
                property: Some(name.clone()),
                message: "custom-property dependency depth exceeded".to_owned(),
            });
        }
        invalid.extend(cycles);
        invalid.extend(depth_limited);
    }

    // The unset value of each registered property, precomputed once per
    // element. A value that fails its registered syntax computes to this, and
    // every later `var()` reference must see it rather than the rejected value.
    let registered_fallbacks = registered
        .definitions
        .iter()
        .map(|(name, registration)| {
            let fallback = if matches!(registration.syntax, RegisteredSyntax::Universal) {
                // The universal syntax keeps the unregistered behavior: the
                // substituted value is used as-is, and a cycle is the
                // guaranteed-invalid value rather than the initial value.
                None
            } else if registration.inherits {
                parent
                    .and_then(|style| style.custom_properties.get(name))
                    .map(|value| value.tokens.clone())
                    .or_else(|| registered.initial(name).map(|value| value.tokens.clone()))
            } else {
                registered.initial(name).map(|value| value.tokens.clone())
            };
            (name.clone(), fallback)
        })
        .collect::<BTreeMap<_, _>>();

    let mut resolver = CustomResolver {
        sources: &sources,
        invalid: &invalid,
        inherited: parent.map(|style| &*style.custom_properties),
        inherited_invalid: parent.map(|style| &*style.invalid_custom_properties),
        registered: Some(registered),
        registered_fallbacks: &registered_fallbacks,
        memo: BTreeMap::new(),
        limits,
        diagnostics: &mut diagnostics,
        diagnosed: BTreeSet::new(),
    };
    if !inherits_custom {
        // Resolve only the names this element declares. Everything else keeps
        // the parent layer, so no inherited value is copied or re-substituted.
        let source_names = sources.keys().cloned().collect::<Vec<_>>();
        for name in source_names {
            let resolved = resolver
                .resolve(&name, 0)
                .and_then(|tokens| ComputedValue::within_limits(tokens, limits));
            if let Some(value) = resolved {
                custom_overrides.insert(name, value);
            } else {
                resolution_invalid.insert(name);
            }
        }
    }
    // A registered universal property with no initial value has no source at
    // all until it is declared; it is the guaranteed-invalid value.
    for (name, registration) in &registered.definitions {
        if !registration.inherits && !sources.contains_key(name) {
            resolution_invalid.insert(name.clone());
        }
    }

    let mut properties = BTreeMap::new();
    let mut typed_properties = BTreeMap::new();
    let mut specified = BTreeSet::new();
    // An element declares a handful of properties out of a 145-entry registry.
    // Walking the declarations first keeps the common path from looking every
    // registry name up in the cascade for every element; the second pass only
    // visits the inherited and fallback properties that genuinely need the
    // element's parent or grammar.
    for name in cascaded.properties().keys() {
        if name.starts_with("--") {
            continue;
        }
        let Some(definition) = registry.get(name) else {
            continue;
        };
        apply_registered_property(
            name,
            definition,
            cascaded,
            parent,
            &mut resolver,
            limits,
            &mut properties,
            &mut typed_properties,
            &mut specified,
        );
    }
    for name in &initials.per_element {
        if cascaded.get(name).is_some() {
            continue;
        }
        let Some(definition) = registry.get(name) else {
            continue;
        };
        if definition.inherited
            && let Some(parent) = parent
            && let Some(value) = parent.get(name)
        {
            properties.insert(name.clone(), value.clone());
            if let Some(typed) = parent.typed(name) {
                typed_properties.insert(name.clone(), typed.clone());
            }
            continue;
        }
        apply_registered_property(
            name,
            definition,
            cascaded,
            parent,
            &mut resolver,
            limits,
            &mut properties,
            &mut typed_properties,
            &mut specified,
        );
    }
    // CSS 2.1 §6.1.1: the computed value of `font-size` is an absolute
    // length. Absolutizing it here means descendants inherit pixels (so a
    // child's `em`-based font sizes compound against the parent's real size)
    // and layout can use this element's stored value as its `em` basis.
    let parent_font_size = parent
        .and_then(|style| style.get("font-size"))
        .and_then(|value| {
            super::properties::computed_font_size_px(
                value.css_text(),
                DEFAULT_ROOT_FONT_SIZE,
                DEFAULT_ROOT_FONT_SIZE,
            )
        })
        .unwrap_or(DEFAULT_ROOT_FONT_SIZE);
    if let Some(value) = properties.get("font-size")
        && let Some(pixels) = super::properties::computed_font_size_px(
            value.css_text(),
            parent_font_size,
            DEFAULT_ROOT_FONT_SIZE,
        )
        && let Some(tokens) = tokenize(&format!("{pixels}px"), limits).ok()
    {
        properties.insert("font-size".to_owned(), ComputedValue::new(tokens));
    }
    for (name, value) in cascaded.properties() {
        if name.starts_with("--") || registry.get(name).is_some() {
            continue;
        }
        match css_wide_keyword(&value.value) {
            Some(CssWideKeyword::Inherit) => {
                if let Some(parent_value) = parent.and_then(|style| style.properties.get(name)) {
                    properties.insert(name.clone(), parent_value.clone());
                }
                if value.is_authored() {
                    specified.insert(name.clone());
                }
            }
            Some(_) => resolver.diagnostics.push(ComputationDiagnostic {
                property: Some(name.clone()),
                message: "property metadata is required to resolve this CSS-wide keyword"
                    .to_owned(),
            }),
            None => {
                if let Ok(tokens) = tokenize(&value.value, limits)
                    && let Some(tokens) = resolver.substitute(&tokens, 0)
                    && let Some(value) = ComputedValue::within_limits(tokens, limits)
                {
                    properties.insert(name.clone(), value);
                }
                if value.is_authored() {
                    specified.insert(name.clone());
                }
            }
        }
    }
    drop(resolver);
    resolve_logical_box_properties(
        &mut properties,
        &mut typed_properties,
        &mut specified,
        cascaded,
        parent,
    );
    invalid.extend(resolution_invalid);
    // Names whose value on this element is the guaranteed-invalid value must
    // hide the parent layer. A name that was already invalid on the parent is
    // hidden by the parent layer itself unless this element redeclared it.
    let mut removed = BTreeSet::new();
    for name in &invalid {
        let inherited = parent.is_some_and(|style| style.invalid_custom_properties.contains(name));
        if !inherited || sources.contains_key(name) {
            removed.insert(name.clone());
        }
    }

    let custom_properties = if inherits_custom {
        parent.map_or_else(
            || Arc::new(CustomProperties::default()),
            |style| Arc::clone(&style.custom_properties),
        )
    } else {
        Arc::new(CustomProperties {
            inherited: parent.map(|style| Arc::clone(&style.custom_properties)),
            overrides: (!custom_overrides.is_empty()).then(|| Arc::new(custom_overrides)),
            removed: (!removed.is_empty()).then(|| Arc::new(removed)),
            non_inheriting: registered.non_inheriting.clone(),
        })
    };

    ComputedStyle {
        properties: Arc::new(properties),
        typed_properties: Arc::new(typed_properties),
        custom_properties,
        invalid_custom_properties: if inherits_custom {
            parent.map_or_else(
                || Arc::new(BTreeSet::new()),
                |style| Arc::clone(&style.invalid_custom_properties),
            )
        } else {
            Arc::new(invalid)
        },
        specified: Arc::new(specified),
        initial: Some(Arc::clone(initials)),
        diagnostics: Arc::new(diagnostics),
    }
}

/// Resolve logical box longhands onto the physical longhands layout reads.
///
/// CSS Logical Properties 1 makes a logical and a physical declaration name the
/// same side of the box, so they are not independent properties that can each
/// win their own mini-cascade. The cascade has already selected one winner per
/// *name*; this pass compares the two winners' full cascade ranks and lets the
/// higher-ranked declaration write the physical slot. That preserves source
/// order and specificity in both directions:
///
/// ```text
/// margin-left: 5px; margin-inline-start: 9px   -> 9px
/// margin-inline-start: 9px; margin-left: 5px   -> 5px
/// ```
///
/// `direction` is read after the property loop because it is inherited and may
/// itself have been declared on this element. Only `horizontal-tb` is
/// supported: the block axis is always top/bottom and `writing-mode` is not
/// parsed, so no vertical mapping is attempted.
fn resolve_logical_box_properties(
    properties: &mut BTreeMap<String, ComputedValue>,
    typed_properties: &mut BTreeMap<String, TypedPropertyValue>,
    specified: &mut BTreeSet<String>,
    cascaded: &CascadedStyle,
    parent: Option<&ComputedStyle>,
) {
    let direction = properties
        .get("direction")
        .or_else(|| parent.and_then(|style| style.get("direction")))
        .map(ComputedValue::css_text)
        .map_or(Direction::Ltr, |value| {
            if value.trim().eq_ignore_ascii_case("rtl") {
                Direction::Rtl
            } else {
                Direction::Ltr
            }
        });
    for (logical_name, logical_value) in cascaded.properties() {
        let Some(physical_name) = physical_longhand_for_logical(logical_name, direction) else {
            continue;
        };
        if let Some(physical_value) = cascaded.get(physical_name)
            && !logical_value.outranks(physical_value)
        {
            continue;
        }
        let Some(computed) = properties.get(logical_name).cloned() else {
            continue;
        };
        properties.insert(physical_name.to_owned(), computed);
        if let Some(typed) = typed_properties.get(logical_name).cloned() {
            typed_properties.insert(physical_name.to_owned(), typed);
        } else {
            typed_properties.remove(physical_name);
        }
        if specified.contains(logical_name) {
            specified.insert(physical_name.to_owned());
        }
    }
}

fn registered_unset_value(
    name: &str,
    definition: &PropertyDefinition,
    parent: Option<&ComputedStyle>,
    limits: &ComputationLimits,
) -> Option<ComputedValue> {
    if definition.inherited
        && let Some(value) = parent.and_then(|style| style.get(name))
    {
        return Some(value.clone());
    }
    tokenize(&definition.initial_value, limits)
        .ok()
        .and_then(|tokens| ComputedValue::within_limits(tokens, limits))
}

fn compute_registered_property(
    name: &str,
    definition: &PropertyDefinition,
    cascaded: &CascadedStyle,
    parent: Option<&ComputedStyle>,
    resolver: &mut CustomResolver<'_>,
    limits: &ComputationLimits,
) -> Option<ComputedValue> {
    let initial = || {
        tokenize(&definition.initial_value, limits)
            .ok()
            .and_then(|tokens| ComputedValue::within_limits(tokens, limits))
    };
    let inherited = || {
        parent
            .and_then(|style| style.properties.get(name))
            .cloned()
            .or_else(initial)
    };
    let unset = || {
        if definition.inherited {
            inherited()
        } else {
            initial()
        }
    };
    let Some(value) = cascaded.get(name) else {
        return unset();
    };
    match css_wide_keyword(&value.value) {
        Some(CssWideKeyword::Inherit) => inherited(),
        Some(CssWideKeyword::Initial) => initial(),
        Some(CssWideKeyword::Unset | CssWideKeyword::Revert | CssWideKeyword::RevertLayer) => {
            unset()
        }
        None => tokenize(&value.value, limits)
            .ok()
            .and_then(|tokens| resolver.substitute(&tokens, 0))
            .and_then(|tokens| ComputedValue::within_limits(tokens, limits))
            .or_else(unset),
    }
}

/// Collect the `@property` registrations that apply in this environment.
///
/// Document order decides between two registrations of the same name: the
/// later rule wins, as it does for a normal declaration. A registration inside
/// a false `@media` query is absent for the same reason a style rule inside it
/// is absent.
fn collect_registered_properties(
    sources: &[CascadeInput<'_>],
    context: &MatchContext,
) -> BTreeMap<String, PropertyRegistration> {
    let mut registrations = BTreeMap::new();
    for source in sources {
        for registration in &source.sheet.property_registrations {
            if registration
                .media
                .iter()
                .all(|query| media_query_list_matches(query, context))
            {
                registrations.insert(registration.name.clone(), registration.clone());
            }
        }
    }
    registrations
}

/// Whether a `::before`/`::after` cascade generates a box.
///
/// CSS Pseudo-Elements 4 §4.1: the initial `content` is `normal`, which
/// computes to `none` for these pseudo-elements, and an element with
/// `content: none` generates no pseudo-element. A universal rule that sets an
/// unrelated property on every `::before` must therefore not force a full
/// computed style for every element.
fn pseudo_generates_content(cascaded: &CascadedStyle) -> bool {
    let Some(content) = cascaded.get("content") else {
        return false;
    };
    let value = content.value.trim();
    // A `var()` or other substitution cannot be decided here; compute it.
    !(value.eq_ignore_ascii_case("normal") || value.eq_ignore_ascii_case("none"))
}

/// Cascade and compute every element in parent-before-child order.
#[must_use]
pub fn compute_document_styles(
    dom: &Dom,
    sources: &[CascadeInput<'_>],
    registry: &PropertyRegistry,
    limits: &ComputationLimits,
    context: &MatchContext,
) -> BTreeMap<NodeId, ComputedStyle> {
    compute_document_styles_with_hints(dom, sources, registry, limits, context, &|_| Vec::new())
}

/// Like [`compute_document_styles`], with a per-element source of extra
/// user-agent-origin declarations. HTML drives this with its presentational
/// attributes (`bgcolor`, `width`, `align`, ...), which must cascade at the
/// UA origin so any author rule overrides them.
pub fn compute_document_styles_with_hints(
    dom: &Dom,
    sources: &[CascadeInput<'_>],
    registry: &PropertyRegistry,
    limits: &ComputationLimits,
    context: &MatchContext,
    hints: &dyn Fn(NodeId) -> Vec<Declaration>,
) -> BTreeMap<NodeId, ComputedStyle> {
    compute_document_styles_with_hints_from_root(
        dom,
        dom.document(),
        sources,
        registry,
        limits,
        context,
        hints,
    )
}

/// [`compute_document_styles_with_hints`] for one document tree in an arena
/// that holds several documents.
///
/// A nested browsing context's document is grafted into the same [`Dom`] as
/// its embedder, so style computation must start at the nested document's root
/// instead of the arena's primary document. `root` may be any document node;
/// the walk covers only that tree (through the flat tree), and inheritance
/// begins from the root rather than from the embedder's document element.
pub fn compute_document_styles_with_hints_from_root(
    dom: &Dom,
    root: NodeId,
    sources: &[CascadeInput<'_>],
    registry: &PropertyRegistry,
    limits: &ComputationLimits,
    context: &MatchContext,
    hints: &dyn Fn(NodeId) -> Vec<Declaration>,
) -> BTreeMap<NodeId, ComputedStyle> {
    let mut styles = BTreeMap::new();
    let initials = Arc::new(InitialValues::new(registry, limits));
    let registered =
        RegisteredProperties::new(collect_registered_properties(sources, context), limits);
    let registry_size = registry.iter().count();
    let initial_count = initials.values.len();
    let debug_index = std::env::var_os("RENDER_DEBUG_CSS_INDEX").is_some();
    let index_started = debug_index.then(std::time::Instant::now);
    let rule_index = CascadeRuleIndex::new(sources);
    let index_build = index_started.map_or(std::time::Duration::ZERO, |started| started.elapsed());
    let loop_started = debug_index.then(std::time::Instant::now);
    // Pseudo-element cascade is opt-in per document: most stylesheets never
    // mention `::before`/`::after`, and walking every element twice more would
    // be a visible cost on large pages.
    let generate_before = sources.iter().any(|source| {
        source.sheet.rules.iter().any(|rule| {
            rule.selectors
                .selectors()
                .iter()
                .any(|selector| selector.has_pseudo_element("before"))
        })
    });
    let generate_after = sources.iter().any(|source| {
        source.sheet.rules.iter().any(|rule| {
            rule.selectors
                .selectors()
                .iter()
                .any(|selector| selector.has_pseudo_element("after"))
        })
    });
    let mut stack = vec![(root, None)];
    let mut cascade_time = std::time::Duration::ZERO;
    let mut compute_time = std::time::Duration::ZERO;
    let mut pseudo_cascade_time = std::time::Duration::ZERO;
    let mut pseudo_compute_time = std::time::Duration::ZERO;
    while let Some((node, parent_element)) = stack.pop() {
        let current_parent = if matches!(dom.node(node).map(Node::kind), Some(NodeKind::Element(_)))
        {
            let inline = dom
                .attribute(node, "style")
                .ok()
                .flatten()
                .map_or_else(Vec::new, |source| parse_declaration_list(source).0);
            let ua_hints = hints(node);
            let cascade_started = debug_index.then(std::time::Instant::now);
            let cascaded = cascade_element_with_origins_indexed(
                dom,
                node,
                sources,
                context,
                &ua_hints,
                &inline,
                &CascadeLookup {
                    rule_index: Some(&rule_index),
                    style_queries: &ComputedStyleQueries {
                        styles: &styles,
                        current: None,
                        registered: &registered,
                        limits,
                    },
                    include_starting_style: false,
                },
            );
            if let Some(started) = cascade_started {
                cascade_time += started.elapsed();
            }
            let compute_started = debug_index.then(std::time::Instant::now);
            let style = compute_style_with_initials(
                &cascaded,
                parent_element.and_then(|parent| styles.get(&parent)),
                registry,
                limits,
                &initials,
                &registered,
            );
            if let Some(started) = compute_started {
                compute_time += started.elapsed();
            }
            for (pseudo_element, enabled) in [
                (PseudoElement::Before, generate_before),
                (PseudoElement::After, generate_after),
            ] {
                if !enabled {
                    continue;
                }
                let mut pseudo_context = context.clone();
                pseudo_context.pseudo_element = Some(pseudo_element.name().to_owned());
                let pseudo_started = debug_index.then(std::time::Instant::now);
                let pseudo_cascaded = cascade_element_with_origins_indexed(
                    dom,
                    node,
                    sources,
                    &pseudo_context,
                    &[],
                    &[],
                    &CascadeLookup {
                        rule_index: Some(&rule_index),
                        style_queries: &ComputedStyleQueries {
                            styles: &styles,
                            current: Some((node, &style)),
                            registered: &registered,
                            limits,
                        },
                        include_starting_style: false,
                    },
                );
                if let Some(started) = pseudo_started {
                    pseudo_cascade_time += started.elapsed();
                }
                // No matched declaration of `content` means the pseudo-element
                // is not generated (CSS Pseudo-Elements 4 §4.1: `content:
                // normal` computes to `none` for ::before/::after). Universal
                // declarations such as `*, ::before, ::after { box-sizing:
                // border-box }` match every element but generate nothing, so a
                // non-empty cascaded style is not enough: computing a full
                // pseudo style for every element was the largest remaining
                // style cost on GitHub's 2,600-element page.
                if !pseudo_generates_content(&pseudo_cascaded) {
                    continue;
                }
                let pseudo_compute_started = debug_index.then(std::time::Instant::now);
                let pseudo_style = compute_style_with_initials(
                    &pseudo_cascaded,
                    Some(&style),
                    registry,
                    limits,
                    &initials,
                    &registered,
                );
                if let Some(started) = pseudo_compute_started {
                    pseudo_compute_time += started.elapsed();
                }
                if pseudo::generated_content(&pseudo_style).is_some() {
                    styles.insert(pseudo::style_source(node, pseudo_element), pseudo_style);
                }
            }
            styles.insert(node, style);
            Some(node)
        } else {
            parent_element
        };
        // The flat tree: a host's children are its shadow tree, and a slot's are
        // the nodes assigned to it. Inheritance follows the same tree.
        for child in dom.flat_children(node).into_iter().rev() {
            stack.push((child, current_parent));
        }
    }
    if debug_index {
        let (rules, keyed, universal, elements, candidates, selectors, matched) =
            rule_index.stats();
        eprintln!(
            "render-css rule index rules={rules} keyed={keyed} universal={universal} build={index_build:?} loop={:?} elements={elements} candidates={candidates} selectors={selectors} matched={matched} cascade={cascade_time:?} pseudo_cascade={pseudo_cascade_time:?} compute={compute_time:?} pseudo_compute={pseudo_compute_time:?} registry={registry_size} initials={initial_count}",
            loop_started.map_or(std::time::Duration::ZERO, |started| started.elapsed())
        );
    }
    styles
}

/// Compute the [starting style](https://drafts.csswg.org/css-transitions-2/#starting-style)
/// of `nodes` (CSS Transitions 2 §3.3).
///
/// The starting style is defined as "the after-change style with
/// `@starting-style` rules applied in addition". Those rules therefore take
/// part in a full cascade at their ordinary source position, so a normal
/// declaration that outranks them still wins; `after_change` supplies every
/// value the starting rules do not override, and inheritance uses the parent's
/// after-change style as §3.3 requires.
///
/// Only the elements that are becoming rendered need a starting style, so the
/// caller passes exactly those in `nodes`. The cost is one cascade per entering
/// element and a second rule index, which is why this is a separate pass rather
/// than something every document pays for.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn compute_starting_styles(
    dom: &Dom,
    sources: &[CascadeInput<'_>],
    registry: &PropertyRegistry,
    limits: &ComputationLimits,
    context: &MatchContext,
    hints: &dyn Fn(NodeId) -> Vec<Declaration>,
    after_change: &BTreeMap<NodeId, ComputedStyle>,
    nodes: &BTreeSet<NodeId>,
) -> BTreeMap<NodeId, ComputedStyle> {
    let mut styles = BTreeMap::new();
    if nodes.is_empty() {
        return styles;
    }
    let initials = Arc::new(InitialValues::new(registry, limits));
    let registered =
        RegisteredProperties::new(collect_registered_properties(sources, context), limits);
    let rule_index = CascadeRuleIndex::new(sources);
    for node in nodes {
        if !matches!(dom.node(*node).map(Node::kind), Some(NodeKind::Element(_))) {
            continue;
        }
        let inline = dom
            .attribute(*node, "style")
            .ok()
            .flatten()
            .map_or_else(Vec::new, |source| parse_declaration_list(source).0);
        let ua_hints = hints(*node);
        let cascaded = cascade_element_with_origins_indexed(
            dom,
            *node,
            sources,
            context,
            &ua_hints,
            &inline,
            &CascadeLookup {
                rule_index: Some(&rule_index),
                style_queries: &ComputedStyleQueries {
                    styles: after_change,
                    current: None,
                    registered: &registered,
                    limits,
                },
                include_starting_style: true,
            },
        );
        let parent = closest_element_style(dom, *node, after_change);
        let style = compute_style_with_initials(
            &cascaded,
            parent,
            registry,
            limits,
            &initials,
            &registered,
        );
        styles.insert(*node, style);
    }
    styles
}

/// The computed style of the nearest ancestor element, which is what
/// inheritance consumes. Shadow roots are not elements, so the walk continues
/// through them to the host.
fn closest_element_style<'a>(
    dom: &Dom,
    node: NodeId,
    styles: &'a BTreeMap<NodeId, ComputedStyle>,
) -> Option<&'a ComputedStyle> {
    let mut current = dom.parent(node);
    while let Some(candidate) = current {
        if matches!(
            dom.node(candidate).map(Node::kind),
            Some(NodeKind::Element(_))
        ) && let Some(style) = styles.get(&candidate)
        {
            return Some(style);
        }
        current = dom.parent(candidate);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        ComputationLimits, PropertyRegistry, compute_document_styles,
        compute_document_styles_with_hints, compute_starting_styles, compute_style,
    };
    use crate::cascade::{CascadeInput, CascadeOrigin, cascade_element};
    use crate::container::{ContainerContext, ContainerGeometry, ContainerType};
    use crate::properties::{
        BorderSpacing, Length, LengthPercentage, LengthUnit, TypedPropertyValue,
    };
    use crate::selector::{MatchContext, parse_selector_list, select_all};
    use crate::stylesheet::parse_stylesheet;
    use render_html::parse_document;
    use std::collections::BTreeSet;

    fn target_id(dom: &render_dom::Dom, selector: &str) -> render_dom::NodeId {
        let selector = parse_selector_list(selector).expect("valid test selector");
        select_all(dom, dom.document(), &selector, &MatchContext::default())[0]
    }

    #[test]
    fn the_rule_index_preserves_selector_list_universal_and_type_matches() {
        let output = parse_document(
            "<!doctype html><div id='root'>\
             <span class='foo' data-x='1'></span>\
             <span class='bar'></span>\
             <p></p>\
             </div>",
        );
        let sheet = parse_stylesheet(
            "span.foo, span.bar { color: red }\
             [data-x] { background-color: blue }\
             :is(.foo, p) { font-weight: 700 }\
             p { color: green }\
             * { border-top-width: 3px }",
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );

        let foo = &styles[&target_id(&output.dom, "span.foo")];
        let bar = &styles[&target_id(&output.dom, "span.bar")];
        let paragraph = &styles[&target_id(&output.dom, "p")];
        assert_eq!(
            foo.get("color").map(super::ComputedValue::css_text),
            Some("red"),
            "the second selector in the list still matches"
        );
        assert_eq!(
            bar.get("color").map(super::ComputedValue::css_text),
            Some("red")
        );
        assert_eq!(
            foo.get("background-color")
                .map(super::ComputedValue::css_text),
            Some("blue"),
            "attribute-only subjects stay in the universal bucket"
        );
        assert_eq!(
            paragraph.get("color").map(super::ComputedValue::css_text),
            Some("green")
        );
        assert_eq!(
            foo.get("font-weight").map(super::ComputedValue::css_text),
            Some("700"),
            "a key inside :is() narrows the rule without losing the match"
        );
        assert_eq!(
            paragraph
                .get("font-weight")
                .map(super::ComputedValue::css_text),
            Some("700")
        );
        assert_eq!(
            foo.get("border-top-width")
                .map(super::ComputedValue::css_text),
            Some("3px"),
            "a universal selector is not lost by the index"
        );
    }

    /// `ComputedStyle::get` cannot answer the support question, and this is the
    /// shape of the failure rather than an assertion about it.
    ///
    /// Four properties, one per corner of the space a support oracle needs:
    ///
    /// | property | engine | `get` on an element that declared nothing |
    /// | --- | --- | --- |
    /// | `display` | grammar | `Some("inline")` |
    /// | `text-indent` | metadata only, no grammar | `Some("0px")` |
    /// | `backdrop-filter` | unknown | `None` |
    /// | `-webkit-line-clamp` | unknown, and a real page asks about it | `None` |
    ///
    /// So `get(p).is_some()` is **true** for a fully implemented property and
    /// for one this engine can only hold the initial value of, and is false for
    /// one it has never heard of. The two "defined but not implemented" states
    /// collapse into `Some` and cannot be told apart, and a property the engine
    /// does not know reads as absent rather than unsupported - which is the
    /// failure `docs/visual_fidelity_gaps.md` S22 records for author intent, in
    /// the other direction.
    ///
    /// The last two rows are the sharp ones, because a *declared* unknown
    /// property is stored: the cascade has nowhere else to put it. So
    /// `get("backdrop-filter").is_some()` is true exactly when the document
    /// used a property this engine does not implement - the predicate is
    /// inverted, and an oracle built on it would report support for precisely
    /// the declarations that prove it is missing.
    #[test]
    fn get_cannot_answer_the_support_question_and_the_support_query_can() {
        use crate::supports::{PropertySupport, property_support};

        assert_eq!(property_support("display"), PropertySupport::Grammar);
        assert_eq!(
            property_support("text-indent"),
            PropertySupport::MetadataOnly
        );
        assert_eq!(
            property_support("backdrop-filter"),
            PropertySupport::Unsupported
        );
        // The name is case-insensitive in CSS, so the query is too.
        assert_eq!(property_support("DISPLAY"), PropertySupport::Grammar);

        let output = parse_document("<!doctype html><div id='a'></div><div id='b'></div>");
        let sheet = parse_stylesheet("#a { backdrop-filter: blur(2px) } #b { display: block }");
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let used = &styles[&target_id(&output.dom, "#a")];
        let silent = &styles[&target_id(&output.dom, "#b")];

        // Implemented, and metadata-only, both read as present on an element
        // that declared nothing, so `get` cannot tell them apart.
        assert_eq!(
            silent.get("display").map(super::ComputedValue::css_text),
            Some("block")
        );
        assert_eq!(
            silent
                .get("text-indent")
                .map(super::ComputedValue::css_text),
            Some("0px")
        );
        // An unknown property reads as absent on an element that said nothing
        // about it...
        assert!(silent.get("backdrop-filter").is_none());
        // ...and *present* on one that did, because the cascade has nowhere else
        // to put a declaration for a property it has no metadata for. So
        // `get(p).is_some()` is true exactly when the document used a property
        // this engine does not implement: the predicate is inverted, and an
        // oracle built on it would report support for precisely the
        // declarations that prove it is missing.
        assert_eq!(
            used.get("backdrop-filter")
                .map(super::ComputedValue::css_text),
            Some("blur(2px)")
        );
        // `property_support` gives the same honest answer either way, and it is
        // a property of the engine rather than of the element.
        assert_eq!(
            property_support("backdrop-filter"),
            PropertySupport::Unsupported
        );
    }

    /// `@supports` and [`crate::supports::property_support`] answer the same
    /// question about the same property, so they must not be able to drift.
    /// The oracle collapses [`crate::supports::PropertySupport`] to a bool, and
    /// a bool is what a feature query can carry - so "known" has to be exactly
    /// "not `Unsupported`" for both.
    #[test]
    fn the_support_query_and_the_at_supports_oracle_agree() {
        use crate::supports::{PropertySupport, property_support, supports_declaration};

        for (property, value, expected) in [
            ("display", "grid", PropertySupport::Grammar),
            ("border-collapse", "collapse", PropertySupport::Grammar),
            // Defined, with an initial value, and no grammar: the state a bool
            // cannot carry and the reason the query has three.
            ("text-indent", "1px", PropertySupport::MetadataOnly),
            ("letter-spacing", "normal", PropertySupport::MetadataOnly),
            ("z-index", "auto", PropertySupport::MetadataOnly),
            ("backdrop-filter", "blur(2px)", PropertySupport::Unsupported),
            ("font-family", "Arial", PropertySupport::MetadataOnly),
            ("text-overflow", "ellipsis", PropertySupport::Unsupported),
            ("nonesuch-property", "1px", PropertySupport::Unsupported),
        ] {
            assert_eq!(property_support(property), expected, "{property}");
            assert_eq!(
                supports_declaration(property, value),
                expected != PropertySupport::Unsupported,
                "{property}"
            );
        }
    }

    #[test]
    fn property_registrations_control_inheritance_and_initial_values() {
        let output =
            parse_document("<!doctype html><div id='parent'><span id='child'></span></div>");
        let sheet = parse_stylesheet(
            "@property --accent { syntax: \"<color>\"; inherits: false; initial-value: red; } \
             @property --size { syntax: \"<length>\"; inherits: true; initial-value: 4px; } \
             #parent { --accent: blue; --size: 10px } \
             #child { color: var(--accent); margin-left: var(--size) }",
        );
        let mut registry = PropertyRegistry::new();
        registry.define("color", true, "canvastext");
        registry.define("margin-left", false, "0px");
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &registry,
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let parent = &styles[&target_id(&output.dom, "#parent")];
        let child = &styles[&target_id(&output.dom, "#child")];

        assert_eq!(
            parent.get("--accent").map(super::ComputedValue::css_text),
            Some("blue")
        );
        assert_eq!(
            parent.get("--size").map(super::ComputedValue::css_text),
            Some("10px")
        );
        // `--accent` does not inherit, so the child starts from its registered
        // initial value rather than the parent's blue.
        assert_eq!(
            child.get("--accent").map(super::ComputedValue::css_text),
            Some("red")
        );
        // `--size` does inherit, so the child sees the parent's computed value.
        assert_eq!(
            child.get("--size").map(super::ComputedValue::css_text),
            Some("10px")
        );
        assert_eq!(
            child.get("color").map(super::ComputedValue::css_text),
            Some("red")
        );
        assert_eq!(
            child.get("margin-left").map(super::ComputedValue::css_text),
            Some("10px")
        );
    }

    #[test]
    fn a_value_outside_the_registered_syntax_falls_back_before_substitution() {
        let output = parse_document("<!doctype html><div id='target'></div>");
        let sheet = parse_stylesheet(
            "@property --gap { syntax: \"<length>\"; inherits: false; initial-value: 8px; } \
             #target { --gap: 1px 2px; --mirror: var(--gap); margin-left: var(--gap) }",
        );
        let mut registry = PropertyRegistry::new();
        registry.define("margin-left", false, "0px");
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &registry,
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let target = &styles[&target_id(&output.dom, "#target")];

        // The rejected `1px 2px` computes to the registered initial value, and
        // references to it see that value, not the rejected token stream.
        assert_eq!(
            target.get("--gap").map(super::ComputedValue::css_text),
            Some("8px")
        );
        assert_eq!(
            target.get("--mirror").map(super::ComputedValue::css_text),
            Some("8px")
        );
        assert_eq!(
            target
                .get("margin-left")
                .map(super::ComputedValue::css_text),
            Some("8px")
        );
        assert!(!target.invalid_custom_properties().contains("--gap"));
        assert!(target.diagnostics().iter().any(|diagnostic| {
            diagnostic.property.as_deref() == Some("--gap")
                && diagnostic.message.contains("registered syntax")
        }));
    }

    #[test]
    fn the_last_applicable_registration_wins_and_false_media_ones_do_not_apply() {
        let output = parse_document("<!doctype html><div id='target'></div>");
        let sheet = parse_stylesheet(
            "@property --accent { syntax: \"<color>\"; inherits: false; initial-value: red; } \
             @media (max-width: 0px) { \
                 @property --accent { syntax: \"<color>\"; inherits: false; initial-value: blue; } \
             } \
             @media (min-width: 1px) { \
                 @property --accent { syntax: \"<color>\"; inherits: false; initial-value: lime; } \
             }",
        );
        let context = MatchContext {
            viewport_width: Some(1_280.0),
            viewport_height: Some(720.0),
            ..MatchContext::default()
        };
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::new(),
            &ComputationLimits::default(),
            &context,
        );
        let target = &styles[&target_id(&output.dom, "#target")];

        assert_eq!(
            target.get("--accent").map(super::ComputedValue::css_text),
            Some("lime")
        );
    }

    #[test]
    fn a_child_without_custom_declarations_shares_the_parent_layer() {
        let output =
            parse_document("<!doctype html><div id='parent'><span id='child'></span></div>");
        let sheet = parse_stylesheet("#parent { --brand: red; --derived: var(--brand) }");
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::new(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let parent = &styles[&target_id(&output.dom, "#parent")];
        let child = &styles[&target_id(&output.dom, "#child")];

        assert!(
            std::sync::Arc::ptr_eq(&parent.custom_properties, &child.custom_properties),
            "a child that declares no custom property must not copy the inherited map"
        );
        assert_eq!(
            child.get("--derived").map(super::ComputedValue::css_text),
            Some("red")
        );
    }

    #[test]
    fn a_declaring_child_stores_only_its_own_overrides() {
        let output =
            parse_document("<!doctype html><div id='parent'><span id='child'></span></div>");
        let sheet = parse_stylesheet(
            "#parent { --brand: red; --other: 2px } \
             #child { --brand: blue; color: var(--other) }",
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::new(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let child = &styles[&target_id(&output.dom, "#child")];

        assert_eq!(
            child.get("--brand").map(super::ComputedValue::css_text),
            Some("blue")
        );
        assert_eq!(
            child.get("--other").map(super::ComputedValue::css_text),
            Some("2px")
        );
        let overrides = child
            .custom_properties
            .overrides
            .as_ref()
            .expect("the child declares one custom property");
        assert_eq!(overrides.len(), 1, "inherited names are not copied");
        assert!(overrides.contains_key("--brand"));
    }

    #[test]
    fn inherited_custom_properties_are_already_computed() {
        let output =
            parse_document("<!doctype html><div id='parent'><span id='child'></span></div>");
        let sheet = parse_stylesheet(
            "#parent { --brand: red; --derived: calc(var(--brand) + 1px); color: var(--brand) } \
             #child { --brand: blue; border-color: var(--missing, var(--brand)) }",
        );
        let mut registry = PropertyRegistry::new();
        registry.define("color", true, "canvastext");
        registry.define("border-top-color", false, "currentcolor");
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &registry,
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let child = &styles[&target_id(&output.dom, "#child")];

        assert_eq!(
            child.get("--brand").map(super::ComputedValue::css_text),
            Some("blue")
        );
        assert_eq!(
            child.get("--derived").map(super::ComputedValue::css_text),
            Some("calc(red + 1px)")
        );
        assert_eq!(
            child.get("color").map(super::ComputedValue::css_text),
            Some("red")
        );
        // The border-color shorthand expands into per-edge longhands, and the
        // var() fallback resolves against the child's own custom property.
        assert_eq!(
            child
                .get("border-top-color")
                .map(super::ComputedValue::css_text),
            Some("blue")
        );
    }

    #[test]
    fn cycles_are_invalid_even_when_the_cycle_edges_have_fallbacks() {
        let output = parse_document("<!doctype html><div id='target'></div>");
        let sheet = parse_stylesheet(
            "#target { \
                --a: var(--b, red); --b: var(--a, blue); \
                --safe: var(--a, green); color: var(--a, purple); \
             }",
        );
        let mut registry = PropertyRegistry::new();
        registry.define("color", true, "canvastext");
        let target = target_id(&output.dom, "#target");
        let cascaded = cascade_element(
            &output.dom,
            target,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );
        let style = compute_style(&cascaded, None, &registry, &ComputationLimits::default());

        assert!(style.get("--a").is_none());
        assert!(style.get("--b").is_none());
        assert_eq!(
            style.get("--safe").map(super::ComputedValue::css_text),
            Some("green")
        );
        assert_eq!(
            style.get("color").map(super::ComputedValue::css_text),
            Some("purple")
        );
    }

    #[test]
    fn css_wide_keywords_use_property_inheritance_metadata() {
        let parent_cascade =
            parse_stylesheet("#x { color: red; margin-left: 12px; --token: blue }");
        let child_cascade = parse_stylesheet(
            "#x { color: unset; margin-left: unset; --token: inherit; --gone: initial }",
        );
        let output = parse_document("<!doctype html><div id='x'></div>");
        let target = target_id(&output.dom, "#x");
        let mut registry = PropertyRegistry::new();
        registry.define("color", true, "canvastext");
        registry.define("margin-left", false, "0px");
        let parent = compute_style(
            &cascade_element(
                &output.dom,
                target,
                &[CascadeInput {
                    sheet: &parent_cascade,
                    origin: CascadeOrigin::Author,
                }],
                &MatchContext::default(),
            ),
            None,
            &registry,
            &ComputationLimits::default(),
        );
        let child = compute_style(
            &cascade_element(
                &output.dom,
                target,
                &[CascadeInput {
                    sheet: &child_cascade,
                    origin: CascadeOrigin::Author,
                }],
                &MatchContext::default(),
            ),
            Some(&parent),
            &registry,
            &ComputationLimits::default(),
        );

        assert_eq!(
            child.get("color").map(super::ComputedValue::css_text),
            Some("red")
        );
        assert_eq!(
            child.get("margin-left").map(super::ComputedValue::css_text),
            Some("0px")
        );
        assert_eq!(
            child.get("--token").map(super::ComputedValue::css_text),
            Some("blue")
        );
        assert!(child.get("--gone").is_none());
    }

    #[test]
    fn font_size_computes_to_an_absolute_length() {
        let output = parse_document(
            "<!doctype html><div id='pct'><p id='em-child'></p><span id='plain'></span></div>",
        );
        let sheet = parse_stylesheet(
            "#pct { font-size: 200% } #em-child { font-size: 1.2em } #plain { font-size: large }",
        );
        let registry = PropertyRegistry::standard_baseline();
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &registry,
            &ComputationLimits::default(),
            &MatchContext::default(),
        );

        // 200% of the 16px default is 32px, and the child's `1.2em` must
        // compound against that real inherited size (CSS 2.1 §6.1.1).
        assert_eq!(
            styles
                .get(&target_id(&output.dom, "#pct"))
                .and_then(|style| style.get("font-size"))
                .map(super::ComputedValue::css_text),
            Some("32px")
        );
        assert_eq!(
            styles
                .get(&target_id(&output.dom, "#em-child"))
                .and_then(|style| style.get("font-size"))
                .map(super::ComputedValue::css_text),
            Some("38.4px")
        );
        // Absolute-size keywords map to the fixed user-font table.
        assert_eq!(
            styles
                .get(&target_id(&output.dom, "#plain"))
                .and_then(|style| style.get("font-size"))
                .map(super::ComputedValue::css_text),
            Some("18px")
        );
    }

    #[test]
    fn unset_font_size_inherits_the_parent_absolute_length() {
        let output = parse_document("<!doctype html><div id='p'><span id='c'></span></div>");
        let sheet = parse_stylesheet("#p { font-size: 25px }");
        let registry = PropertyRegistry::standard_baseline();
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &registry,
            &ComputationLimits::default(),
            &MatchContext::default(),
        );

        assert_eq!(
            styles
                .get(&target_id(&output.dom, "#c"))
                .and_then(|style| style.get("font-size"))
                .map(super::ComputedValue::css_text),
            Some("25px")
        );
    }

    #[test]
    fn substitution_boundaries_prevent_invalid_dimension_fusion() {
        let output = parse_document("<!doctype html><div id='target'></div>");
        let sheet = parse_stylesheet("#target { --n: 1; width: var(--n)px }");
        let target = target_id(&output.dom, "#target");
        let mut registry = PropertyRegistry::new();
        registry.define("width", false, "auto");
        let style = compute_style(
            &cascade_element(
                &output.dom,
                target,
                &[CascadeInput {
                    sheet: &sheet,
                    origin: CascadeOrigin::Author,
                }],
                &MatchContext::default(),
            ),
            None,
            &registry,
            &ComputationLimits::default(),
        );

        assert_eq!(
            style.get("width").map(super::ComputedValue::css_text),
            Some("auto")
        );
        assert_eq!(
            style.typed("width").map(TypedPropertyValue::to_css),
            Some("auto".to_owned())
        );
        assert!(
            style
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.property.as_deref() == Some("width"))
        );
    }

    #[test]
    fn resource_limits_invalidate_values_and_allow_call_site_fallbacks() {
        let output = parse_document("<!doctype html><div id='target'></div>");
        let sheet = parse_stylesheet(
            "#target { --large: 12345678901234567890; --deep: f(f(f(value))); \
             color: var(--large, green); visibility: var(--deep, hidden) }",
        );
        let target = target_id(&output.dom, "#target");
        let mut registry = PropertyRegistry::new();
        registry.define("color", true, "canvastext");
        registry.define("visibility", true, "visible");
        let limits = ComputationLimits {
            max_value_bytes: 10,
            max_component_depth: 2,
            ..ComputationLimits::default()
        };
        let style = compute_style(
            &cascade_element(
                &output.dom,
                target,
                &[CascadeInput {
                    sheet: &sheet,
                    origin: CascadeOrigin::Author,
                }],
                &MatchContext::default(),
            ),
            None,
            &registry,
            &limits,
        );

        assert!(style.invalid_custom_properties().contains("--large"));
        assert!(style.invalid_custom_properties().contains("--deep"));
        assert_eq!(
            style.get("color").map(super::ComputedValue::css_text),
            Some("green")
        );
        assert_eq!(
            style.get("visibility").map(super::ComputedValue::css_text),
            Some("hidden")
        );
    }

    /// The table solver reads these six properties from
    /// `ComputedStyle::get(..).css_text()` and compares the text itself, so
    /// registering them must not change the text a specified declaration
    /// produces. It also must give an unspecified element the CSS 2.1
    /// Appendix G initial value rather than `None`, because every one of those
    /// call sites treats a missing value as the initial value anyway.
    #[test]
    fn table_properties_are_typed_and_report_css21_initial_values() {
        let output = parse_document(
            "<!doctype html><table id='t'><caption>c</caption><tr id='row'>\
             <td id='cell'>x</td></tr></table><div id='plain'></div>",
        );
        let sheet = parse_stylesheet(
            "table { display: table } \
             #t { table-layout: fixed; border-spacing: 3px 5px; vertical-align: middle } \
             #row { border-collapse: collapse; empty-cells: hide; caption-side: bottom }",
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let table = &styles[&target_id(&output.dom, "#t")];

        assert_eq!(
            table
                .get("border-spacing")
                .map(super::ComputedValue::css_text),
            Some("3px 5px")
        );
        for (selector, properties) in [
            (
                "#t",
                [
                    ("border-spacing", "3px 5px"),
                    ("table-layout", "fixed"),
                    ("vertical-align", "middle"),
                ],
            ),
            (
                "#row",
                [
                    ("border-collapse", "collapse"),
                    ("empty-cells", "hide"),
                    ("caption-side", "bottom"),
                ],
            ),
        ] {
            let style = &styles[&target_id(&output.dom, selector)];
            for (property, expected) in properties {
                assert_eq!(
                    style.get(property).map(super::ComputedValue::css_text),
                    Some(expected),
                    "{selector} {property}"
                );
                assert!(
                    style.typed(property).is_some(),
                    "{property} must be a typed value"
                );
            }
        }
        assert_eq!(
            table.typed("border-spacing"),
            Some(&TypedPropertyValue::BorderSpacing(BorderSpacing {
                horizontal: LengthPercentage::Length(Length {
                    value: 3.0,
                    unit: LengthUnit::Px,
                }),
                vertical: LengthPercentage::Length(Length {
                    value: 5.0,
                    unit: LengthUnit::Px,
                }),
            }))
        );

        // An element that specifies none of them reports each initial value, so
        // a consumer that compared against the initial keyword still agrees.
        let plain = &styles[&target_id(&output.dom, "#plain")];
        for (property, expected) in [
            ("border-spacing", "0px"),
            ("border-collapse", "separate"),
            ("empty-cells", "show"),
            ("caption-side", "top"),
            ("table-layout", "auto"),
            ("vertical-align", "baseline"),
        ] {
            assert_eq!(
                plain.get(property).map(super::ComputedValue::css_text),
                Some(expected),
                "{property}"
            );
        }

        // CSS 2.1 Appendix G: `border-collapse`, `border-spacing`, `empty-cells`
        // and `caption-side` inherit, while §17.5.2.1 `table-layout` and
        // §10.8.3 `vertical-align` do not. Inheriting `vertical-align` would
        // align a cell with its ancestor's cell instead of its own.
        let cell = &styles[&target_id(&output.dom, "#cell")];
        assert_eq!(
            cell.get("border-collapse")
                .map(super::ComputedValue::css_text),
            Some("collapse")
        );
        assert_eq!(
            cell.get("empty-cells").map(super::ComputedValue::css_text),
            Some("hide")
        );
        assert_eq!(
            cell.get("vertical-align")
                .map(super::ComputedValue::css_text),
            Some("baseline")
        );
        assert_eq!(
            cell.get("table-layout").map(super::ComputedValue::css_text),
            Some("auto")
        );
    }

    /// The sweep claim: registering a property gives every element the initial
    /// value, so a consumer that compared the *text* against a non-initial
    /// keyword is unaffected, while one that only asked "is it there?" is not.
    /// This pins the initial value each table property now reports, because
    /// that text is what the solver's keyword comparisons see.
    #[test]
    fn registering_a_property_only_changes_what_unset_elements_report() {
        let output =
            parse_document("<!doctype html><table id='t'><tr><td id='c'>x</td></tr></table>");
        let styles = compute_document_styles(
            &output.dom,
            &[],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let cell = &styles[&target_id(&output.dom, "#c")];

        // Every one of these is the CSS 2.1 initial value, so a comparison
        // against `fixed`/`collapse`/`hide`/`bottom` is still false exactly as
        // it was when the property was unregistered and `get` returned `None`.
        for (property, expected) in [
            ("table-layout", "auto"),
            ("border-collapse", "separate"),
            ("empty-cells", "show"),
            ("caption-side", "top"),
            ("border-spacing", "0px"),
            ("vertical-align", "baseline"),
        ] {
            assert_eq!(
                cell.get(property).map(super::ComputedValue::css_text),
                Some(expected),
                "{property}"
            );
            assert!(
                !cell.specified(property),
                "{property} must not be specified"
            );
        }
    }

    /// `ComputedStyle::get` reports a value for every registered property, so
    /// `specified` is the only way to ask whether the document actually set
    /// one. These pin each boundary the doc comment states.
    #[test]
    fn specified_separates_document_declarations_from_initial_values() {
        let output = parse_document(
            "<!doctype html><div id='author' style='color: red'></div>\
             <div id='plain'></div><div id='child'></div>",
        );
        let sheet = parse_stylesheet("#author { display: block }");
        let mut registry = PropertyRegistry::new();
        // `color` is inherited, `display` is not, which makes the two
        // inheritance rules distinguishable below.
        registry.define("color", true, "canvastext");
        registry.define("display", false, "inline");
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &registry,
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let author = &styles[&target_id(&output.dom, "#author")];
        let plain = &styles[&target_id(&output.dom, "#plain")];

        // A `style` attribute counts: the document said so directly.
        assert!(author.specified("color"));
        // An author rule counts.
        assert!(author.specified("display"));
        // Nothing set either, so neither is specified, even though `get`
        // reports a value for both because the registry supplies the initial.
        assert!(!plain.specified("color"));
        assert!(!plain.specified("display"));
        assert_eq!(
            plain.get("color").map(super::ComputedValue::css_text),
            Some("canvastext"),
            "get must still report the initial value; that is the whole reason \
             `specified` exists"
        );
    }

    /// A user-agent stylesheet rule is the engine styling the document, not
    /// the document asking for something. This is the boundary that keeps
    /// `sub { vertical-align: sub }` from reading as author intent.
    #[test]
    fn a_user_agent_rule_is_not_specified() {
        let output = parse_document("<!doctype html><div id='x'></div>");
        let ua = parse_stylesheet("#x { color: red }");
        let author = parse_stylesheet("");
        let registry = PropertyRegistry::new();
        let mut registry = registry;
        registry.define("color", true, "canvastext");
        let styles = compute_document_styles(
            &output.dom,
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
            &registry,
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let style = &styles[&target_id(&output.dom, "#x")];

        // The value is there...
        assert_eq!(
            style.get("color").map(super::ComputedValue::css_text),
            Some("red")
        );
        // ... but the document did not ask for it.
        assert!(!style.specified("color"));
    }

    /// HTML presentational hints cascade at the user-agent origin but are
    /// attributes the document's author wrote, so they count as specified.
    /// This is the case `CascadeOrigin` alone cannot express.
    #[test]
    fn a_presentational_hint_counts_as_specified() {
        let output = parse_document("<!doctype html><div id='x'></div>");
        let registry = PropertyRegistry::new();
        let mut registry = registry;
        registry.define("color", true, "canvastext");
        let styles = compute_document_styles_with_hints(
            &output.dom,
            &[],
            &registry,
            &ComputationLimits::default(),
            &MatchContext::default(),
            &|_| {
                vec![crate::stylesheet::Declaration {
                    name: "color".to_owned(),
                    value: "red".to_owned(),
                    important: false,
                }]
            },
        );
        let style = &styles[&target_id(&output.dom, "#x")];

        assert_eq!(
            style.get("color").map(super::ComputedValue::css_text),
            Some("red")
        );
        assert!(style.specified("color"));
    }

    /// An inherited value belongs to the ancestor. A consumer asking about the
    /// descendant must still learn that the descendant said nothing itself,
    /// which is exactly the row-group case in CSS 2.1 §17.5.3.
    #[test]
    fn an_inherited_value_is_specified_only_on_the_ancestor() {
        let output = parse_document("<!doctype html><div id='p'><span id='c'></span></div>");
        let sheet = parse_stylesheet("#p { color: red }");
        let mut registry = PropertyRegistry::new();
        registry.define("color", true, "canvastext");
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &registry,
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let parent = &styles[&target_id(&output.dom, "#p")];
        let child = &styles[&target_id(&output.dom, "#c")];

        assert!(parent.specified("color"));
        // The child holds the same computed value...
        assert_eq!(
            child.get("color").map(super::ComputedValue::css_text),
            Some("red")
        );
        // ... but did not set it.
        assert!(!child.specified("color"));
    }

    /// CSS Fonts 4 §2.2 lists `font-family` as inherited. Without it in the
    /// registry every nested element fell back to `system-ui`, so a parent's
    /// family stopped at its direct text and icon fonts or webfonts vanished
    /// inside ordinary `<span>`/`<b>` wrappers.
    #[test]
    fn font_family_inherits_to_nested_elements() {
        let output = parse_document("<!doctype html><div id='p'><span id='c'>nested</span></div>");
        let sheet = parse_stylesheet("#p { font-family: Arial, sans-serif }");
        let registry = PropertyRegistry::standard_baseline();
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &registry,
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let child = &styles[&target_id(&output.dom, "#c")];

        assert_eq!(
            child.get("font-family").map(super::ComputedValue::css_text),
            Some("Arial, sans-serif")
        );
        assert!(
            !child.specified("font-family"),
            "the child inherited the value; it did not author one"
        );
    }

    /// `inherit` was written by the document, so it counts even though the
    /// value it produces came from the parent.
    #[test]
    fn inherit_counts_as_specified_because_the_document_wrote_it() {
        let output = parse_document("<!doctype html><div id='p'><span id='c'></span></div>");
        let sheet = parse_stylesheet("#p { color: red } #c { color: inherit }");
        let mut registry = PropertyRegistry::new();
        registry.define("color", true, "canvastext");
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &registry,
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let child = &styles[&target_id(&output.dom, "#c")];

        assert_eq!(
            child.get("color").map(super::ComputedValue::css_text),
            Some("red")
        );
        assert!(child.specified("color"));
    }

    /// Shorthand expansion needs no special case: the cascade expands before
    /// choosing a winner, so a longhand set by a shorthand is specified.
    #[test]
    fn a_longhand_set_by_a_shorthand_is_specified() {
        let output = parse_document("<!doctype html><div id='x' style='margin: 4px'></div>");
        let mut registry = PropertyRegistry::new();
        registry.define("margin-top", false, "0px");
        registry.define("margin-left", false, "0px");
        let styles = compute_document_styles_with_hints(
            &output.dom,
            &[],
            &registry,
            &ComputationLimits::default(),
            &MatchContext::default(),
            &|_| Vec::new(),
        );
        let style = &styles[&target_id(&output.dom, "#x")];

        assert!(style.specified("margin-top"));
        assert!(style.specified("margin-left"));
        assert_eq!(
            style.get("margin-top").map(super::ComputedValue::css_text),
            Some("4px")
        );
    }

    /// A value the grammar rejected did not successfully set the property, so
    /// the element ends up holding the initial value and the query must say
    /// no. Reporting yes would reintroduce the exact confusion the query
    /// removes.
    #[test]
    fn a_value_the_grammar_rejected_is_not_specified() {
        let output = parse_document("<!doctype html><div id='x'></div>");
        let sheet = parse_stylesheet("#x { vertical-align: center }");
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let style = &styles[&target_id(&output.dom, "#x")];

        assert_eq!(
            style
                .get("vertical-align")
                .map(super::ComputedValue::css_text),
            Some("baseline")
        );
        assert!(!style.specified("vertical-align"));
    }

    /// CSS 2.1 §17.5.3: an invalid `vertical-align` is dropped, and the
    /// computed value falls back to the initial one rather than cascading a
    /// string the solver would have to interpret.
    #[test]
    fn an_invalid_table_property_falls_back_to_its_initial_value() {
        let output = parse_document("<!doctype html><table id='t'></table>");
        let sheet = parse_stylesheet(
            "#t { vertical-align: center; border-collapse: collapsed; border-spacing: -1px }",
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let table = &styles[&target_id(&output.dom, "#t")];

        for (property, expected) in [
            ("vertical-align", "baseline"),
            ("border-collapse", "separate"),
            ("border-spacing", "0px"),
        ] {
            assert_eq!(
                table.get(property).map(super::ComputedValue::css_text),
                Some(expected),
                "{property}"
            );
            assert!(
                table
                    .diagnostics()
                    .iter()
                    .any(|diagnostic| diagnostic.property.as_deref() == Some(property)),
                "{property} must report why it was dropped"
            );
        }
    }

    /// Feed one measured container into the match context a style pass sees.
    fn context_with_container(
        node: render_dom::NodeId,
        container_type: ContainerType,
        names: &[&str],
        width: f32,
        height: f32,
    ) -> MatchContext {
        let mut context = ContainerContext {
            viewport_width: 1000.0,
            viewport_height: 800.0,
            root_font_size: 16.0,
            ..ContainerContext::default()
        };
        context.containers.insert(
            node,
            ContainerGeometry {
                container_type,
                names: names.iter().map(|name| (*name).to_owned()).collect(),
                width,
                height,
                font_size: 16.0,
            },
        );
        MatchContext {
            containers: std::sync::Arc::new(context),
            ..MatchContext::default()
        }
    }

    /// A size query reads the content box of the nearest eligible ancestor
    /// container, and an `inline-size` container cannot answer a block-axis
    /// query: the query is false, not answered from the viewport.
    #[test]
    fn container_queries_read_the_named_ancestor_and_respect_their_axis() {
        let output = parse_document("<!doctype html><div id='card'><p id='target'>text</p></div>");
        let sheet = parse_stylesheet(
            "#card { container-type: inline-size; container-name: card } \
             @container card (min-width: 400px) { #target { color: red } } \
             @container card (min-height: 100px) { #target { background-color: blue } }",
        );
        let card = target_id(&output.dom, "#card");
        let target = target_id(&output.dom, "#target");
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &context_with_container(card, ContainerType::InlineSize, &["card"], 500.0, 300.0),
        );
        let style = &styles[&target];
        assert_eq!(
            style.get("color").map(super::ComputedValue::css_text),
            Some("red")
        );
        assert_eq!(
            style
                .get("background-color")
                .map(super::ComputedValue::css_text),
            Some("transparent"),
            "an inline-size container does not answer a height query"
        );

        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &context_with_container(card, ContainerType::Size, &["card"], 500.0, 300.0),
        );
        assert_eq!(
            styles[&target]
                .get("background-color")
                .map(super::ComputedValue::css_text),
            Some("blue"),
            "a size container answers both axes"
        );
    }

    /// The queried container is chosen per name and per axis, so a nearer
    /// unnamed or ineligible container does not hide the outer one the author
    /// asked for.
    #[test]
    fn container_lookup_skips_containers_that_do_not_match_the_query() {
        let output = parse_document(
            "<!doctype html><div id='outer'><div id='inner'><p id='target'></p></div></div>",
        );
        let sheet = parse_stylesheet(
            "#outer { container-type: size; container-name: page } \
             #inner { container-type: inline-size } \
             @container page (min-height: 500px) { #target { color: green } }",
        );
        let outer = target_id(&output.dom, "#outer");
        let inner = target_id(&output.dom, "#inner");
        let target = target_id(&output.dom, "#target");
        let mut context = ContainerContext {
            viewport_width: 1000.0,
            viewport_height: 800.0,
            root_font_size: 16.0,
            ..ContainerContext::default()
        };
        context.containers.insert(
            inner,
            ContainerGeometry {
                container_type: ContainerType::InlineSize,
                names: Vec::new(),
                width: 400.0,
                height: 700.0,
                font_size: 16.0,
            },
        );
        context.containers.insert(
            outer,
            ContainerGeometry {
                container_type: ContainerType::Size,
                names: vec!["page".to_owned()],
                width: 900.0,
                height: 600.0,
                font_size: 16.0,
            },
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext {
                containers: std::sync::Arc::new(context),
                ..MatchContext::default()
            },
        );
        assert_eq!(
            styles[&target]
                .get("color")
                .map(super::ComputedValue::css_text),
            Some("green"),
            "the named eligible outer container answers the query"
        );
    }

    /// Without a measurement the query is false; it must not fall back to the
    /// viewport or to the initial value.
    #[test]
    fn a_container_query_without_geometry_does_not_match() {
        let output = parse_document("<!doctype html><div id='card'><p id='target'></p></div>");
        let sheet = parse_stylesheet(
            "#card { container-type: size } \
             @container (min-width: 1px) { #target { color: red } }",
        );
        let target = target_id(&output.dom, "#target");
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        assert_ne!(
            styles[&target]
                .get("color")
                .map(super::ComputedValue::css_text),
            Some("red")
        );
    }

    /// The `container` shorthand reaches both longhands, and the longhands are
    /// what the geometry builder reads.
    #[test]
    fn the_container_shorthand_sets_name_and_type() {
        let output = parse_document("<!doctype html><div id='card'></div>");
        let sheet = parse_stylesheet("#card { container: card / inline-size }");
        let card = target_id(&output.dom, "#card");
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let style = &styles[&card];
        assert_eq!(
            style
                .get("container-type")
                .map(super::ComputedValue::css_text),
            Some("inline-size")
        );
    }

    /// A `@property` registration inside a container query still registers:
    /// CSS Conditional 5 §5.4 exempts global name-defining at-rules from the
    /// condition, so the initial value participates in computation even when
    /// the query does not match.
    #[test]
    fn a_property_registered_inside_a_container_query_still_registers() {
        let output = parse_document("<!doctype html><div id='target'></div>");
        let sheet = parse_stylesheet(
            "@container (width: 200px) { \
                 @property --accent { syntax: \"<color>\"; inherits: false; initial-value: red } \
             } \
             #target { color: var(--accent) }",
        );
        assert_eq!(sheet.property_registrations.len(), 1);
        let target = target_id(&output.dom, "#target");
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        assert_eq!(
            styles[&target]
                .get("color")
                .map(super::ComputedValue::css_text),
            Some("red"),
            "the unconditional registration supplies its initial value"
        );
    }

    /// CSS Conditional 5 §5.1: every element is a style container, so a
    /// style-only query needs no `container-type` and no measured geometry.
    /// The nearest ancestor answers; the queried element itself does not.
    #[test]
    fn style_queries_read_ancestor_styles_without_geometry() {
        let output = parse_document(
            "<!doctype html><div id='theme'><p id='target'></p></div><span id='self'></span>",
        );
        let sheet = parse_stylesheet(
            "#theme { --theme: dark; container-name: theme } \
             #self { --theme: dark } \
             @container style(--theme: dark) { #target { color: red } } \
             @container theme style(--theme: dark) { #target { background-color: blue } } \
             @container style(--theme: dark) { #self { color: green } }",
        );
        let target = target_id(&output.dom, "#target");
        let quoted = target_id(&output.dom, "#self");
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        assert_eq!(
            styles[&target]
                .get("color")
                .map(super::ComputedValue::css_text),
            Some("red"),
            "the parent is a style container without container-type"
        );
        assert_eq!(
            styles[&target]
                .get("background-color")
                .map(super::ComputedValue::css_text),
            Some("blue"),
            "a named style container answers by name alone"
        );
        assert_ne!(
            styles[&quoted]
                .get("color")
                .map(super::ComputedValue::css_text),
            Some("green"),
            "a style query never observes the queried element itself"
        );
    }

    /// A condition that mixes a size feature and a style feature selects one
    /// query container that can answer both. A nearer style-only container
    /// cannot answer the size half, and an outer size container's own style is
    /// what the style half reads.
    #[test]
    fn style_and_size_features_share_one_query_container() {
        let output = parse_document(
            "<!doctype html><div id='outer'><div id='inner'><p id='target'></p></div></div>",
        );
        let outer = target_id(&output.dom, "#outer");
        let target = target_id(&output.dom, "#target");
        let context = context_with_container(outer, ContainerType::InlineSize, &[], 500.0, 300.0);
        let mixed = "@container (min-width: 400px) and style(--theme: dark) { \
                         #target { color: red } \
                     }";

        let matching = parse_stylesheet(&format!(
            "#outer {{ container-type: inline-size; --theme: dark }} \
             #inner {{ --theme: light }} \
             {mixed}"
        ));
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &matching,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &context,
        );
        assert_eq!(
            styles[&target]
                .get("color")
                .map(super::ComputedValue::css_text),
            Some("red"),
            "the size container is also a style container and answers both halves"
        );

        let nearer_style_only = parse_stylesheet(&format!(
            "#outer {{ container-type: inline-size; --theme: light }} \
             #inner {{ --theme: dark }} \
             {mixed}"
        ));
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &nearer_style_only,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &context,
        );
        assert_ne!(
            styles[&target]
                .get("color")
                .map(super::ComputedValue::css_text),
            Some("red"),
            "the style half is not answered by a nearer container than the size half"
        );

        let without_geometry = parse_stylesheet(&format!(
            "#outer {{ container-type: inline-size; --theme: dark }} \
             #inner {{ --theme: dark }} \
             {mixed}"
        ));
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &without_geometry,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        assert_ne!(
            styles[&target]
                .get("color")
                .map(super::ComputedValue::css_text),
            Some("red"),
            "a mixed condition needs real geometry, not just a matching style"
        );
    }

    /// A boolean style feature compares the computed value with the property's
    /// initial value. For a registered custom property the initial value is
    /// the registration's, not "guaranteed-invalid".
    #[test]
    fn boolean_style_features_compare_with_initial_values() {
        let output = parse_document("<!doctype html><div id='theme'><p id='target'></p></div>");
        let target = target_id(&output.dom, "#target");
        let registration =
            "@property --accent { syntax: \"<color>\"; inherits: false; initial-value: purple }";
        let queries = "@container style(--accent) { #target { color: red } } \
                       @container style(border-top-style) { #target { background-color: blue } }";

        let at_initial = parse_stylesheet(&format!("{registration} #theme {{ }} {queries}"));
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &at_initial,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        assert_ne!(
            styles[&target]
                .get("color")
                .map(super::ComputedValue::css_text),
            Some("red"),
            "a registered property at its initial value does not satisfy a boolean feature"
        );
        assert_ne!(
            styles[&target]
                .get("background-color")
                .map(super::ComputedValue::css_text),
            Some("blue"),
            "an unset standard property is at its initial value"
        );

        let declared = parse_stylesheet(&format!(
            "{registration} #theme {{ --accent: red; border-top-style: solid }} {queries}"
        ));
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &declared,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        assert_eq!(
            styles[&target]
                .get("color")
                .map(super::ComputedValue::css_text),
            Some("red")
        );
        assert_eq!(
            styles[&target]
                .get("background-color")
                .map(super::ComputedValue::css_text),
            Some("blue")
        );
    }

    /// A registered `<color>` property compares by computed color, so the two
    /// spellings of the same color match, while a different color does not.
    #[test]
    fn registered_color_style_features_compare_equal_colors() {
        let output = parse_document("<!doctype html><div id='theme'><p id='target'></p></div>");
        let sheet = parse_stylesheet(
            "@property --accent { syntax: \"<color>\"; inherits: false; initial-value: red } \
             #theme { --accent: #00f } \
             @container style(--accent: rgb(0 0 255)) { #target { color: red } } \
             @container style(--accent: blue) { #target { border-top-style: solid } } \
             @container style(--accent: red) { #target { border-top-color: green } }",
        );
        let target = target_id(&output.dom, "#target");
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let style = &styles[&target];
        assert_eq!(
            style.get("color").map(super::ComputedValue::css_text),
            Some("red"),
            "rgb() containing spaces matches #00f"
        );
        assert_eq!(
            style
                .get("border-top-style")
                .map(super::ComputedValue::css_text),
            Some("solid"),
            "the blue keyword matches #00f"
        );
        assert_ne!(
            style
                .get("border-top-color")
                .map(super::ComputedValue::css_text),
            Some("green"),
            "a different color does not match"
        );
    }

    /// CSS Conditional 5 §5: `::before` and `::after` query their originating
    /// element, including that element itself. The element's style is not in
    /// the map yet while its pseudo-element is cascaded, so the environment
    /// overlays it rather than looking only at ancestors.
    #[test]
    fn style_queries_on_generated_content_include_the_originating_element() {
        let output = parse_document("<!doctype html><div id='host'></div>");
        let sheet = parse_stylesheet(
            "#host { --theme: dark } \
             #host::before { content: \"x\"; color: blue } \
             @container style(--theme: dark) { #host::before { color: red } }",
        );
        let host = target_id(&output.dom, "#host");
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let pseudo = crate::pseudo::style_source(host, crate::pseudo::PseudoElement::Before);
        assert_eq!(
            styles
                .get(&pseudo)
                .and_then(|style| style.get("color"))
                .map(super::ComputedValue::css_text),
            Some("red"),
            "a style query on ::before reads its originating element"
        );
    }

    /// CSS Conditional 5 §6.2: cascade-dependent keywords and `!important` are
    /// invalid in a style feature and make the query false.
    #[test]
    fn cascade_dependent_style_feature_values_are_false() {
        let output = parse_document("<!doctype html><div id='theme'><p id='target'></p></div>");
        let sheet = parse_stylesheet(
            "#theme { color: red } \
             @container style(color: inherit) { #target { background-color: blue } } \
             @container style(color: red !important) { #target { border-top-style: solid } }",
        );
        let target = target_id(&output.dom, "#target");
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let style = &styles[&target];
        assert_ne!(
            style
                .get("background-color")
                .map(super::ComputedValue::css_text),
            Some("blue")
        );
        assert_ne!(
            style
                .get("border-top-style")
                .map(super::ComputedValue::css_text),
            Some("solid")
        );
    }

    /// CSS Transitions 2 §3.3: `@starting-style` rules never reach the
    /// after-change style, and in the starting style they cascade in their
    /// ordinary source position. A normal declaration with higher specificity
    /// therefore still wins over a matching starting declaration.
    #[test]
    fn starting_style_rules_join_only_the_starting_cascade() {
        let output = parse_document("<!doctype html><div id='x' class='x'></div>");
        let node = target_id(&output.dom, "#x");

        let sheet = parse_stylesheet(
            "#x { opacity: 1; transition: opacity 1s } \
             @starting-style { #x { opacity: 0 } }",
        );
        let inputs = [CascadeInput {
            sheet: &sheet,
            origin: CascadeOrigin::Author,
        }];
        let styles = compute_document_styles(
            &output.dom,
            &inputs,
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        assert_eq!(
            styles[&node]
                .get("opacity")
                .map(super::ComputedValue::css_text),
            Some("1"),
            "the after-change style does not see @starting-style"
        );

        let nodes = BTreeSet::from([node]);
        let starting = compute_starting_styles(
            &output.dom,
            &inputs,
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
            &|_| Vec::new(),
            &styles,
            &nodes,
        );
        assert_eq!(
            starting[&node]
                .get("opacity")
                .map(super::ComputedValue::css_text),
            Some("0"),
            "the starting style applies the starting declaration"
        );

        let sheet = parse_stylesheet("@starting-style { .x { opacity: 0 } } #x { opacity: 1 }");
        let inputs = [CascadeInput {
            sheet: &sheet,
            origin: CascadeOrigin::Author,
        }];
        let styles = compute_document_styles(
            &output.dom,
            &inputs,
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let starting = compute_starting_styles(
            &output.dom,
            &inputs,
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
            &|_| Vec::new(),
            &styles,
            &nodes,
        );
        assert_eq!(
            starting[&node]
                .get("opacity")
                .map(super::ComputedValue::css_text),
            Some("1"),
            "a higher-specificity normal declaration still beats the starting one"
        );
    }

    /// Logical and physical declarations name the same side, so both the
    /// inline start/end swap under RTL and the chosen value must be visible
    /// through the physical longhand layout reads.
    #[test]
    fn logical_margins_resolve_to_the_direction_selected_physical_side() {
        let output = parse_document("<!doctype html><div id='ltr'></div><div id='rtl'></div>");
        let sheet = parse_stylesheet(
            "#ltr, #rtl { margin-inline-start: 12px; margin-inline-end: 7px } \
             #rtl { direction: rtl }",
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let text = |selector: &str, property: &str| {
            styles[&target_id(&output.dom, selector)]
                .get(property)
                .map(super::ComputedValue::css_text)
        };

        assert_eq!(text("#ltr", "margin-left"), Some("12px"));
        assert_eq!(text("#ltr", "margin-right"), Some("7px"));
        assert_eq!(text("#rtl", "margin-right"), Some("12px"));
        assert_eq!(text("#rtl", "margin-left"), Some("7px"));
        // The logical longhands remain readable too, and the physical side
        // they won is recorded as specified author intent.
        assert_eq!(text("#rtl", "margin-inline-start"), Some("12px"));
        assert!(
            styles[&target_id(&output.dom, "#rtl")].specified("margin-right"),
            "the winning logical declaration makes the physical side specified"
        );
    }

    /// Physical and logical declarations compete as one property: the later
    /// declaration wins whichever spelling it used, and specificity beats
    /// source order as usual.
    #[test]
    fn physical_and_logical_box_declarations_compete_by_cascade_rank() {
        let output = parse_document(
            "<!doctype html>\
             <div id='logical-last'></div>\
             <div id='physical-last'></div>\
             <div id='specific' class='side'></div>",
        );
        let sheet = parse_stylesheet(
            "#logical-last { margin-left: 5px; margin-inline-start: 9px } \
             #physical-last { margin-inline-start: 9px; margin-left: 5px } \
             .side { padding-inline-start: 10px } \
             #specific { padding-left: 4px }",
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let text = |selector: &str, property: &str| {
            styles[&target_id(&output.dom, selector)]
                .get(property)
                .map(super::ComputedValue::css_text)
        };

        assert_eq!(
            text("#logical-last", "margin-left"),
            Some("9px"),
            "a later logical declaration beats an earlier physical one"
        );
        assert_eq!(
            text("#physical-last", "margin-left"),
            Some("5px"),
            "a later physical declaration beats an earlier logical one"
        );
        assert_eq!(
            text("#specific", "padding-left"),
            Some("4px"),
            "the id selector's physical rule outranks the later class rule"
        );
    }

    /// The logical shorthand families and border longhands feed the same
    /// physical typed slots, preserving calc/percentage/em forms for the
    /// length engine to resolve at layout time.
    #[test]
    fn logical_padding_inset_and_border_values_reach_physical_typed_properties() {
        let output = parse_document("<!doctype html><div id='box'></div>");
        let sheet = parse_stylesheet(
            "#box { direction: rtl; \
                    padding-inline: 1em 2%; \
                    inset-block-start: calc(10% + 3px); \
                    border-inline: 0.25rem solid blue; \
                    margin-block: 1.5em auto }",
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let style = &styles[&target_id(&output.dom, "#box")];
        let text = |property: &str| style.get(property).map(super::ComputedValue::css_text);

        assert_eq!(text("padding-right"), Some("1em"));
        assert_eq!(text("padding-left"), Some("2%"));
        assert_eq!(text("top"), Some("calc(10% + 3px)"));
        assert_eq!(text("border-right-width"), Some("0.25rem"));
        assert_eq!(text("border-right-style"), Some("solid"));
        assert_eq!(text("border-right-color"), Some("blue"));
        assert_eq!(text("border-left-width"), Some("0.25rem"));
        assert_eq!(text("margin-top"), Some("1.5em"));
        assert_eq!(text("margin-bottom"), Some("auto"));
        assert_eq!(
            style.typed("padding-right"),
            style.typed("padding-inline-start"),
            "the physical slot carries the logical typed value"
        );
    }

    /// Transition and animation sampling writes computed properties by name;
    /// a logical name has to keep the physical slot in step too.
    #[test]
    fn an_animated_logical_property_updates_the_physical_slot() {
        let output = parse_document("<!doctype html><div id='rtl'></div>");
        let sheet = parse_stylesheet("#rtl { direction: rtl }");
        let mut styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let style = styles
            .get_mut(&target_id(&output.dom, "#rtl"))
            .expect("the element has a computed style");
        style.set_animated_property("margin-inline-start", "25px");

        assert_eq!(
            style
                .get("margin-right")
                .map(super::ComputedValue::css_text),
            Some("25px")
        );
        assert_eq!(
            style.typed("margin-right"),
            style.typed("margin-inline-start")
        );
    }

    /// A logical declaration that loses to a physical one must not leave its
    /// value in the physical slot, and CSS-wide keywords resolve through the
    /// logical longhand's own inheritance metadata.
    #[test]
    fn losing_logical_declarations_leave_the_physical_value_untouched() {
        let output = parse_document("<!doctype html><div id='parent'><div id='child'></div></div>");
        let sheet = parse_stylesheet(
            "#parent { margin-left: 4px; margin-inline-start: 20px } \
             #child { margin-inline-start: inherit; margin-left: 6px }",
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let text = |selector: &str, property: &str| {
            styles[&target_id(&output.dom, selector)]
                .get(property)
                .map(super::ComputedValue::css_text)
        };

        assert_eq!(text("#parent", "margin-left"), Some("20px"));
        assert_eq!(text("#child", "margin-left"), Some("6px"));
        assert_eq!(
            text("#child", "margin-inline-start"),
            Some("20px"),
            "inherit takes the parent's computed logical value"
        );
    }
}
