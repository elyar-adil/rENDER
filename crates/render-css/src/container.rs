//! Container queries (CSS Containment 3 §4).
//!
//! A container query is evaluated per element: the queried container is the
//! nearest ancestor that is a query container (`container-type` other than
//! `normal`), whose `container-name` matches the query's name when one is
//! written, and whose type supports every axis the condition asks about.
//!
//! The style engine cannot know a content box before layout has run, so this
//! module is deliberately split into syntax and evaluation. [`parse_container_condition`]
//! turns an `@container` prelude into a [`ContainerCondition`], which a
//! stylesheet stores on every rule the block contributes. [`ContainerContext`]
//! carries the geometry measured by a layout pass; the cascade evaluates the
//! stored condition against it on the next style pass. An empty context is the
//! first pass: a size query has no answer yet and evaluates false, exactly as
//! Media Queries 4 §2.1.1 treats an unknown value.

use std::collections::BTreeMap;

use render_dom::{Dom, NodeId};

use crate::computed::ComputedStyle;
use crate::length::{LengthContext, resolve_length_expr};
use crate::properties::LengthResolutionContext;

/// What a `container-type` declaration establishes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContainerType {
    /// Not a query container.
    #[default]
    Normal,
    /// Establishes inline-size containment; inline-axis size queries apply.
    InlineSize,
    /// Establishes size containment; both axes apply.
    Size,
}

impl ContainerType {
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "normal" => Some(Self::Normal),
            "inline-size" => Some(Self::InlineSize),
            "size" => Some(Self::Size),
            _ => None,
        }
    }

    /// The `container-type` a computed style declares.
    ///
    /// A missing or invalid declaration is the initial value `normal`, which
    /// is exactly what the property grammar's invalid-value handling produces.
    #[must_use]
    pub fn from_style(style: &ComputedStyle) -> Self {
        style
            .get("container-type")
            .and_then(|value| Self::parse(value.css_text()))
            .unwrap_or_default()
    }

    /// Whether this container can answer a query that needs the block axis.
    #[must_use]
    pub const fn supports_block_axis(self) -> bool {
        matches!(self, Self::Size)
    }

    /// Whether this container can answer a query that needs the inline axis.
    #[must_use]
    pub const fn supports_inline_axis(self) -> bool {
        matches!(self, Self::Size | Self::InlineSize)
    }
}

/// The axes a condition reads. `Both` is what `aspect-ratio` and `orientation`
/// need: a ratio cannot be answered without both sizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxisRequirement {
    pub inline: bool,
    pub block: bool,
}

impl AxisRequirement {
    const fn inline() -> Self {
        Self {
            inline: true,
            block: false,
        }
    }

    const fn block() -> Self {
        Self {
            inline: false,
            block: true,
        }
    }

    const fn both() -> Self {
        Self {
            inline: true,
            block: true,
        }
    }

    fn union(self, other: Self) -> Self {
        Self {
            inline: self.inline || other.inline,
            block: self.block || other.block,
        }
    }
}

/// A parsed `@container` prelude: an optional container name and a condition.
#[derive(Clone, Debug, PartialEq)]
pub struct ContainerCondition {
    pub name: Option<String>,
    pub query: ContainerQuery,
}

// The parsed form only ever holds finite ratios, so the `Eq` a stylesheet's
// comparison needs is sound here even though `FeatureValue::Ratio` is an f64.
impl Eq for ContainerCondition {}

impl ContainerCondition {
    pub fn parse(source: &str) -> Result<Self, String> {
        parse_container_condition(source)
    }

    /// Whether this condition can be answered by a container of `container_type`.
    #[must_use]
    pub fn is_eligible_for(&self, container_type: ContainerType) -> bool {
        let axes = self.query.axes();
        (!axes.inline || container_type.supports_inline_axis())
            && (!axes.block || container_type.supports_block_axis())
    }
}

/// One container condition. An unsupported sub-condition is kept rather than
/// dropped: CSS Conditional 5 §5 lets a query it cannot answer evaluate false,
/// and naming it lets a diagnostic explain which part was not understood.
#[derive(Clone, Debug, PartialEq)]
pub enum ContainerQuery {
    Feature(SizeFeature),
    /// `style(...)`: a declaration evaluated against the query container's
    /// computed style. Style containers need no `container-type` (CSS
    /// Conditional 5 §3): every element is a style container.
    Style(StyleFeature),
    Not(Box<Self>),
    And(Vec<Self>),
    Or(Vec<Self>),
    /// `scroll-state()` queries: recognised syntax this engine does not
    /// evaluate yet.
    Unsupported(String),
}

impl Eq for ContainerQuery {}

impl ContainerQuery {
    /// Every axis this condition reads, combined.
    #[must_use]
    pub fn axes(&self) -> AxisRequirement {
        match self {
            Self::Feature(feature) => feature.name.axes(),
            Self::Style(_) => AxisRequirement {
                inline: false,
                block: false,
            },
            Self::Not(inner) => inner.axes(),
            Self::And(parts) | Self::Or(parts) => parts.iter().map(Self::axes).fold(
                AxisRequirement {
                    inline: false,
                    block: false,
                },
                AxisRequirement::union,
            ),
            Self::Unsupported(_) => AxisRequirement {
                inline: false,
                block: false,
            },
        }
    }

    /// Whether the condition contains a `style()` feature.
    #[must_use]
    pub fn uses_style(&self) -> bool {
        match self {
            Self::Style(_) => true,
            Self::Feature(_) | Self::Unsupported(_) => false,
            Self::Not(inner) => inner.uses_style(),
            Self::And(parts) | Self::Or(parts) => parts.iter().any(Self::uses_style),
        }
    }

    /// Evaluate against a container's measured content box.
    #[must_use]
    pub fn matches(&self, geometry: &ContainerGeometry, context: &ContainerContext) -> bool {
        match self {
            Self::Feature(feature) => feature.matches(geometry, context),
            // A style feature needs a computed style, which this entry point
            // does not have. `matches_with_style` is the answering path.
            Self::Style(_) => false,
            Self::Not(inner) => !inner.matches(geometry, context),
            Self::And(parts) => parts.iter().all(|part| part.matches(geometry, context)),
            Self::Or(parts) => parts.iter().any(|part| part.matches(geometry, context)),
            Self::Unsupported(_) => false,
        }
    }

    fn matches_with_style(
        &self,
        geometry: Option<&ContainerGeometry>,
        node: NodeId,
        context: &ContainerContext,
        styles: &dyn StyleQueryEnvironment,
    ) -> bool {
        match self {
            Self::Feature(feature) => {
                geometry.is_some_and(|geometry| feature.matches(geometry, context))
            }
            Self::Style(feature) => styles.style_feature_matches(node, feature),
            Self::Not(inner) => !inner.matches_with_style(geometry, node, context, styles),
            Self::And(parts) => parts
                .iter()
                .all(|part| part.matches_with_style(geometry, node, context, styles)),
            Self::Or(parts) => parts
                .iter()
                .any(|part| part.matches_with_style(geometry, node, context, styles)),
            Self::Unsupported(_) => false,
        }
    }

    /// Whether this condition names a construct this engine does not evaluate.
    #[must_use]
    pub fn unsupported(&self) -> Option<&str> {
        match self {
            Self::Unsupported(reason) => Some(reason),
            Self::Not(inner) => inner.unsupported(),
            Self::And(parts) | Self::Or(parts) => parts.iter().find_map(Self::unsupported),
            Self::Feature(_) | Self::Style(_) => None,
        }
    }
}

/// One `style()` feature: a property declaration or a bare property name.
///
/// CSS Conditional 5 §3.1: the feature is matched against the computed value
/// of the property on the query container. A feature without a value is true
/// when the computed value differs from the property's initial value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleFeature {
    /// The property name as written. Custom properties keep their leading
    /// `--`; standard property names are case-folded to lowercase.
    pub name: String,
    /// The declared value as written, or `None` for a boolean feature.
    pub value: Option<String>,
}

/// Computed styles for elements that have already been cascaded, so a style
/// query can be answered during the same top-down style pass that reads it.
///
/// Style queries never consult descendant styles, so an element can answer
/// them before its own style is computed, provided its ancestors are done.
pub trait StyleQueryEnvironment {
    /// Whether `element` declares `container-name: name`.
    fn container_has_name(&self, element: NodeId, name: &str) -> bool;

    /// Whether `element`'s computed style matches `feature`.
    fn style_feature_matches(&self, element: NodeId, feature: &StyleFeature) -> bool;
}

/// The environment used when no computed styles are available: every style
/// feature is false, as CSS Conditional 5 requires for an unknown container.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoStyleQueries;

impl StyleQueryEnvironment for NoStyleQueries {
    fn container_has_name(&self, _element: NodeId, _name: &str) -> bool {
        false
    }

    fn style_feature_matches(&self, _element: NodeId, _feature: &StyleFeature) -> bool {
        false
    }
}

/// The size feature names CSS Containment 3 §4.2 defines for containers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeFeatureName {
    Width,
    Height,
    InlineSize,
    BlockSize,
    AspectRatio,
    Orientation,
}

impl SizeFeatureName {
    fn parse(name: &str) -> Option<(Self, Option<Comparison>)> {
        let lower = name.to_ascii_lowercase();
        let (base, prefix) = if let Some(rest) = lower.strip_prefix("min-") {
            (rest, Some(Comparison::Ge))
        } else if let Some(rest) = lower.strip_prefix("max-") {
            (rest, Some(Comparison::Le))
        } else {
            (lower.as_str(), None)
        };
        let feature = match base {
            "width" => Self::Width,
            "height" => Self::Height,
            "inline-size" => Self::InlineSize,
            "block-size" => Self::BlockSize,
            "aspect-ratio" => Self::AspectRatio,
            "orientation" => Self::Orientation,
            _ => return None,
        };
        // `min-`/`max-` prefixes are only valid in the plain `(min-width: 1px)`
        // form, where they carry the comparison. `(min-width > 1px)` is not a
        // size feature at all.
        Some((feature, prefix))
    }

    const fn axes(self) -> AxisRequirement {
        match self {
            Self::Width | Self::InlineSize => AxisRequirement::inline(),
            Self::Height | Self::BlockSize => AxisRequirement::block(),
            Self::AspectRatio | Self::Orientation => AxisRequirement::both(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comparison {
    Lt,
    Le,
    Eq,
    Ge,
    Gt,
}

impl Comparison {
    const fn reverse(self) -> Self {
        match self {
            Self::Lt => Self::Gt,
            Self::Le => Self::Ge,
            Self::Eq => Self::Eq,
            Self::Ge => Self::Le,
            Self::Gt => Self::Lt,
        }
    }

    fn compare(self, left: f64, right: f64) -> bool {
        // A NaN never compares true, which keeps an unresolved value false
        // rather than accidentally equal.
        match self {
            Self::Lt => left < right,
            Self::Le => left <= right,
            Self::Eq => (left - right).abs() <= f64::EPSILON * left.abs().max(right.abs()).max(1.0),
            Self::Ge => left >= right,
            Self::Gt => left > right,
        }
    }
}

/// The value side of a size feature.
#[derive(Clone, Debug, PartialEq)]
pub enum FeatureValue {
    /// A `<length>` (or the unitless zero), kept as written so `em` resolves
    /// against the queried container and `calc()` is handled by one parser.
    Length(String),
    /// A `<ratio>` or a bare number for `aspect-ratio`.
    Ratio(f64),
    /// `orientation`'s `portrait` or `landscape`.
    Keyword(Orientation),
}

impl Eq for FeatureValue {}

#[derive(Clone, Debug, PartialEq)]
pub struct SizeFeature {
    pub name: SizeFeatureName,
    pub comparison: Comparison,
    pub value: FeatureValue,
}

impl Eq for SizeFeature {}

impl SizeFeature {
    fn matches(&self, geometry: &ContainerGeometry, context: &ContainerContext) -> bool {
        if self.name == SizeFeatureName::Orientation {
            let FeatureValue::Keyword(expected) = self.value else {
                return false;
            };
            return orientation_matches(geometry, expected);
        }
        let measured = match self.name {
            SizeFeatureName::Width | SizeFeatureName::InlineSize => f64::from(geometry.width),
            SizeFeatureName::Height | SizeFeatureName::BlockSize => f64::from(geometry.height),
            SizeFeatureName::AspectRatio => {
                if geometry.height <= 0.0 {
                    return false;
                }
                f64::from(geometry.width) / f64::from(geometry.height)
            }
            SizeFeatureName::Orientation => unreachable!(),
        };
        let value = match &self.value {
            FeatureValue::Length(text) => resolve_length_expr(
                text,
                &LengthContext {
                    em_base: f64::from(geometry.font_size),
                    rem_base: f64::from(context.root_font_size),
                    viewport_width: f64::from(context.viewport_width),
                    viewport_height: f64::from(context.viewport_height),
                    percentage_base: None,
                    ..LengthContext::default()
                },
            ),
            FeatureValue::Ratio(value) => Ok(*value),
            FeatureValue::Keyword(_) => return false,
        };
        value.is_ok_and(|value| self.comparison.compare(measured, value))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    Portrait,
    Landscape,
}

fn orientation_matches(geometry: &ContainerGeometry, expected: Orientation) -> bool {
    let actual = if geometry.width > geometry.height {
        Orientation::Landscape
    } else {
        Orientation::Portrait
    };
    actual == expected
}

/// One query container's measured geometry.
#[derive(Clone, Debug, PartialEq)]
pub struct ContainerGeometry {
    pub container_type: ContainerType,
    pub names: Vec<String>,
    /// Content-box width and height in CSS pixels.
    pub width: f32,
    pub height: f32,
    /// The container's used font size, which is the `em` base for its queries.
    pub font_size: f32,
}

/// The four container-relative lengths the `cq*` units resolve against
/// (CSS Containment 3 §5.1).
///
/// Each axis selects its own query container: `cqi` reads the nearest ancestor
/// that accepts inline-axis size queries and `cqb` the nearest ancestor that
/// accepts block-axis size queries. Those can be different elements, which is
/// why `cqmin`/`cqmax` cannot be written as a single lookup. A `None` side is
/// answered by the corresponding small viewport dimension, exactly as
/// [`LengthResolutionContext`] already treats an absent container size.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ContainerSizes {
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub inline_size: Option<f32>,
    pub block_size: Option<f32>,
}

/// Every query container the current layout produced, plus the environment a
/// query's `vw`/`rem` units resolve against.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContainerContext {
    pub containers: BTreeMap<NodeId, ContainerGeometry>,
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub root_font_size: f32,
}

impl ContainerContext {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.containers.is_empty()
    }

    /// The nearest ancestor query container eligible for `condition`.
    #[must_use]
    pub fn query_container<'a>(
        &'a self,
        dom: &Dom,
        element: NodeId,
        condition: &ContainerCondition,
    ) -> Option<&'a ContainerGeometry> {
        let mut current = dom.parent(element);
        while let Some(node) = current {
            if let Some(geometry) = self.containers.get(&node)
                && condition
                    .name
                    .as_ref()
                    .is_none_or(|name| geometry.names.iter().any(|candidate| candidate == name))
                && condition.is_eligible_for(geometry.container_type)
            {
                return Some(geometry);
            }
            current = dom.parent(node);
        }
        None
    }

    /// Whether `condition` holds for `element`'s nearest eligible container.
    /// No context, no eligible container, or an unsupported sub-condition all
    /// answer false, as CSS Conditional 5 §3.1 requires.
    #[must_use]
    pub fn matches(&self, dom: &Dom, element: NodeId, condition: &ContainerCondition) -> bool {
        self.matches_with_style(dom, element, condition, &NoStyleQueries)
    }

    /// Style-aware container query evaluation.
    ///
    /// A style feature is answered from the candidate ancestor's computed
    /// style. Size and style features are answered by the *same* query
    /// container: the nearest ancestor that matches the optional name and can
    /// answer every axis the condition names. Style-only conditions need no
    /// geometry and therefore need no `container-type`; CSS Conditional 5 §3.1
    /// makes every element a style container. The queried element itself is
    /// not a container (CSS Conditional 5 §5).
    #[must_use]
    pub fn matches_with_style(
        &self,
        dom: &Dom,
        element: NodeId,
        condition: &ContainerCondition,
        styles: &dyn StyleQueryEnvironment,
    ) -> bool {
        self.matches_with_style_from(dom, element, condition, styles, false)
    }

    /// Style-aware evaluation for a generated-content pseudo-element.
    ///
    /// CSS Conditional 5 §5 makes `::before` and `::after` query their
    /// originating element, so the lookup starts at that element rather than
    /// at its parent.
    #[must_use]
    pub fn matches_with_style_inclusive(
        &self,
        dom: &Dom,
        element: NodeId,
        condition: &ContainerCondition,
        styles: &dyn StyleQueryEnvironment,
    ) -> bool {
        self.matches_with_style_from(dom, element, condition, styles, true)
    }

    fn matches_with_style_from(
        &self,
        dom: &Dom,
        element: NodeId,
        condition: &ContainerCondition,
        styles: &dyn StyleQueryEnvironment,
        include_self: bool,
    ) -> bool {
        let axes = condition.query.axes();
        let needs_geometry = axes.inline || axes.block;
        let mut current = if include_self {
            Some(element)
        } else {
            dom.parent(element)
        };
        while let Some(node) = current {
            let geometry = self.containers.get(&node);
            let name_matches = condition.name.as_ref().is_none_or(|name| {
                styles.container_has_name(node, name)
                    || geometry
                        .is_some_and(|geometry| geometry.names.iter().any(|item| item == name))
            });
            if name_matches
                && (!needs_geometry
                    || geometry
                        .is_some_and(|geometry| condition.is_eligible_for(geometry.container_type)))
            {
                return condition
                    .query
                    .matches_with_style(geometry, node, self, styles);
            }
            current = dom.parent(node);
        }
        false
    }

    /// The nearest ancestor that accepts size queries on one axis.
    ///
    /// The element itself is never its own query container (CSS Containment 3
    /// §4.1): `container-type` on a box establishes containment for its
    /// descendants, and a `cqw` in that box's own `width` would otherwise
    /// depend on itself.
    fn nearest_for_axis(
        &self,
        dom: &Dom,
        element: NodeId,
        inline: bool,
    ) -> Option<&ContainerGeometry> {
        let mut current = dom.parent(element);
        while let Some(node) = current {
            if let Some(geometry) = self.containers.get(&node) {
                let eligible = if inline {
                    geometry.container_type.supports_inline_axis()
                } else {
                    geometry.container_type.supports_block_axis()
                };
                if eligible {
                    return Some(geometry);
                }
            }
            current = dom.parent(node);
        }
        None
    }

    /// The `cqw`/`cqh`/`cqi`/`cqb` bases for `element`.
    ///
    /// CSS Containment 3 §5.1 gives each axis its own nearest eligible
    /// container. `width` and `height` are the physical axes, so which logical
    /// axis answers them depends on the writing mode: in a vertical writing
    /// mode a `cqw` reads the block-axis container's width. `cqmin`/`cqmax`
    /// combine `inline_size` and `block_size`, which may therefore come from
    /// two different containers.
    #[must_use]
    pub fn sizes_for_element(
        &self,
        dom: &Dom,
        element: NodeId,
        inline_axis_is_horizontal: bool,
    ) -> ContainerSizes {
        let inline = self.nearest_for_axis(dom, element, true);
        let block = self.nearest_for_axis(dom, element, false);
        let inline_size = inline.map(|geometry| {
            if inline_axis_is_horizontal {
                geometry.width
            } else {
                geometry.height
            }
        });
        let block_size = block.map(|geometry| {
            if inline_axis_is_horizontal {
                geometry.height
            } else {
                geometry.width
            }
        });
        let (width, height) = if inline_axis_is_horizontal {
            (
                inline.map(|geometry| geometry.width),
                block.map(|geometry| geometry.height),
            )
        } else {
            (
                block.map(|geometry| geometry.width),
                inline.map(|geometry| geometry.height),
            )
        };
        ContainerSizes {
            width,
            height,
            inline_size,
            block_size,
        }
    }

    /// Fill the container-relative fields of a length resolution context.
    ///
    /// Leaves fields `None` when no eligible ancestor exists, so the resolver
    /// falls back to the small viewport size as §5.1 requires.
    pub fn apply_container_sizes(
        &self,
        dom: &Dom,
        element: NodeId,
        inline_axis_is_horizontal: bool,
        context: &mut LengthResolutionContext,
    ) {
        let sizes = self.sizes_for_element(dom, element, inline_axis_is_horizontal);
        context.container_width = sizes.width;
        context.container_height = sizes.height;
        context.container_inline_size = sizes.inline_size;
        context.container_block_size = sizes.block_size;
        context.inline_axis_is_horizontal = inline_axis_is_horizontal;
    }
}

/// The `container-type` and `container-name` a computed style declares.
///
/// An unparsable declared value falls back to the initial value, which is what
/// the property grammar would have done: `container-type` is not inherited, so
/// `normal` is the answer and the element is not a query container.
#[must_use]
pub fn container_geometry_for_style(
    style: &ComputedStyle,
    width: f32,
    height: f32,
) -> Option<ContainerGeometry> {
    let container_type = ContainerType::from_style(style);
    if container_type == ContainerType::Normal {
        return None;
    }
    let names = style
        .get("container-name")
        .map(|value| parse_container_names(value.css_text()))
        .unwrap_or_default();
    let font_size = style
        .get("font-size")
        .and_then(|value| resolve_length_expr(value.css_text(), &LengthContext::default()).ok())
        .map_or(16.0, |value| value as f32);
    Some(ContainerGeometry {
        container_type,
        names,
        width,
        height,
        font_size,
    })
}

/// Parse a `container-name` value into its custom identifiers.
#[must_use]
pub fn parse_container_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for part in text.split_ascii_whitespace() {
        let lower = part.to_ascii_lowercase();
        if lower == "none" {
            return Vec::new();
        }
        if is_custom_ident(part) && !names.iter().any(|name| name == part) {
            names.push(part.to_owned());
        }
    }
    names
}

/// Whether `text` is a valid `container-name` custom identifier.
#[must_use]
pub fn is_container_name(text: &str) -> bool {
    is_custom_ident(text)
}

fn is_custom_ident(text: &str) -> bool {
    let mut characters = text.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '-') {
        return false;
    }
    if !characters
        .clone()
        .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
    {
        return false;
    }
    !matches!(
        text.to_ascii_lowercase().as_str(),
        "none"
            | "normal"
            | "default"
            | "and"
            | "or"
            | "not"
            | "initial"
            | "inherit"
            | "unset"
            | "revert"
            | "revert-layer"
    )
}

/// Parse an `@container` prelude.
///
/// The grammar is `@container <container-name>? <container-condition>`, where a
/// condition is `not (...)` or a chain of parenthesised terms joined by one of
/// `and`/`or`. Mixing the two at one level without parentheses is invalid, and
/// returning that as an error is what lets the stylesheet report the author's
/// mistake instead of silently applying nothing.
pub fn parse_container_condition(source: &str) -> Result<ContainerCondition, String> {
    let tokens = tokenize(source)?;
    let mut cursor = Cursor::new(tokens);
    cursor.skip_whitespace();
    let name = match cursor.peek() {
        Some(Token::Ident(identifier)) if !is_condition_keyword(identifier) => {
            let name = identifier.clone();
            cursor.advance();
            Some(name)
        }
        _ => None,
    };
    let query = cursor.parse_condition()?;
    cursor.expect_end()?;
    Ok(ContainerCondition { name, query })
}

fn is_condition_keyword(text: &str) -> bool {
    matches!(
        text.to_ascii_lowercase().as_str(),
        "not" | "and" | "or" | "style" | "scroll-state"
    )
}

/// Parse the contents of a `style(...)` query into one style feature.
///
/// CSS Conditional 5 §3.1 defines a style feature as a declaration or a bare
/// property name. A declaration is split at the first top-level colon, so
/// values containing functions (`style(--theme: var(--x))`) and strings
/// (`content: "a:b"`) stay intact.
fn parse_style_function(text: &str) -> Result<StyleFeature, String> {
    let (_, rest) = text
        .split_once('(')
        .ok_or_else(|| "style() is missing its opening parenthesis".to_owned())?;
    let inner = rest
        .strip_suffix(')')
        .ok_or_else(|| "style() is missing its closing parenthesis".to_owned())?
        .trim();
    if inner.is_empty() {
        return Err("style() requires a property name or declaration".to_owned());
    }
    let (name, value) = split_style_declaration(inner);
    let name = name.trim();
    let value = value.map(str::trim);
    if !is_style_property_name(name) {
        return Err(format!("'{name}' is not a property name"));
    }
    if value.is_some_and(str::is_empty) {
        return Err("style() declaration is missing its value".to_owned());
    }
    Ok(StyleFeature {
        name: if name.starts_with("--") {
            name.to_owned()
        } else {
            name.to_ascii_lowercase()
        },
        value: value.map(str::to_owned),
    })
}

/// Split a style feature at its first top-level colon.
fn split_style_declaration(source: &str) -> (&str, Option<&str>) {
    let mut depth = 0_usize;
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in source.char_indices() {
        if let Some(closing) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == closing {
                quote = None;
            }
            continue;
        }
        match character {
            '"' | '\'' => quote = Some(character),
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => return (&source[..index], Some(&source[index + 1..])),
            _ => {}
        }
    }
    (source, None)
}

/// Whether `name` is an ordinary property name or a custom property name.
///
/// Ordinary property names are CSS identifiers, which may carry a single
/// leading `-` for a vendor prefix (`-webkit-transform`); the doubled prefix
/// belongs to custom properties and is handled first.
fn is_style_property_name(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix("--") {
        let mut characters = rest.chars();
        return characters.next().is_some_and(is_ident_start) && characters.all(is_ident_continue);
    }
    let name = name.strip_prefix('-').unwrap_or(name);
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    is_ident_start(first)
        && characters.all(|character| is_ident_continue(character) || character == '-')
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Ident(String),
    Number(String),
    Dimension(String),
    Function(String),
    Open,
    Close,
    Colon,
    Slash,
    Comparison(Comparison),
}

struct Cursor {
    tokens: Vec<Token>,
    position: usize,
}

impl Cursor {
    fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            position: 0,
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn advance(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.position).cloned();
        self.position += usize::from(token.is_some());
        token
    }

    fn skip_whitespace(&mut self) {
        // Whitespace is not tokenized; this exists so the parse code reads the
        // same whether or not the tokenizer later keeps trivia.
    }

    fn at_end(&self) -> bool {
        self.position >= self.tokens.len()
    }

    fn expect_end(&self) -> Result<(), String> {
        if self.at_end() {
            Ok(())
        } else {
            Err("unexpected trailing tokens in container condition".to_owned())
        }
    }

    fn parse_condition(&mut self) -> Result<ContainerQuery, String> {
        self.skip_whitespace();
        if self.consume_ident("not") {
            let inner = self.parse_term()?;
            return Ok(ContainerQuery::Not(Box::new(inner)));
        }
        let first = self.parse_term()?;
        let mut terms = vec![first];
        let mut operator: Option<bool> = None;
        while let Some(Token::Ident(word)) = self.peek() {
            let is_and = if word.eq_ignore_ascii_case("and") {
                true
            } else if word.eq_ignore_ascii_case("or") {
                false
            } else {
                break;
            };
            self.advance();
            if let Some(previous) = operator
                && previous != is_and
            {
                return Err("'and' and 'or' cannot be mixed without parentheses".to_owned());
            }
            operator = Some(is_and);
            terms.push(self.parse_term()?);
        }
        if terms.len() == 1 {
            return Ok(terms.remove(0));
        }
        Ok(if operator == Some(true) {
            ContainerQuery::And(terms)
        } else {
            ContainerQuery::Or(terms)
        })
    }

    /// One `<query-in-parens>`: a parenthesised feature or nested condition, or
    /// a functional query this engine recognises but cannot evaluate yet.
    fn parse_term(&mut self) -> Result<ContainerQuery, String> {
        if let Some(Token::Function(text)) = self.peek().cloned() {
            let name = text
                .split_once('(')
                .map_or_else(|| text.clone(), |(name, _)| name.to_ascii_lowercase());
            if name == "style" {
                self.advance();
                return parse_style_function(&text).map(ContainerQuery::Style);
            }
            if name == "scroll-state" {
                let reason = format!("@{name} container queries are not implemented");
                self.advance();
                return Ok(ContainerQuery::Unsupported(reason));
            }
            return Err(format!("unsupported container query function '{name}'"));
        }
        match self.peek() {
            Some(Token::Open) => {
                let inner = self.take_group()?;
                parse_paren_contents(inner)
            }
            Some(Token::Ident(word)) if word.eq_ignore_ascii_case("not") => {
                self.advance();
                Ok(ContainerQuery::Not(Box::new(self.parse_term()?)))
            }
            _ => Err("container condition needs a parenthesised query".to_owned()),
        }
    }

    fn consume_ident(&mut self, expected: &str) -> bool {
        if let Some(Token::Ident(word)) = self.peek()
            && word.eq_ignore_ascii_case(expected)
        {
            self.advance();
            return true;
        }
        false
    }

    fn take_group(&mut self) -> Result<Vec<Token>, String> {
        if !matches!(self.advance(), Some(Token::Open)) {
            return Err("expected '('".to_owned());
        }
        let mut depth = 1_usize;
        let mut inner = Vec::new();
        while let Some(token) = self.advance() {
            match token {
                Token::Open => {
                    depth += 1;
                    inner.push(Token::Open);
                }
                Token::Close => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(inner);
                    }
                    inner.push(Token::Close);
                }
                other => inner.push(other),
            }
        }
        Err("unclosed '(' in container condition".to_owned())
    }
}

fn parse_paren_contents(tokens: Vec<Token>) -> Result<ContainerQuery, String> {
    // A size feature is one expression; if reading one consumes all the tokens
    // this parenthesis is a feature and not a nested condition.
    let mut feature_cursor = Cursor::new(tokens.clone());
    if let Ok(feature) = feature_cursor.parse_size_feature()
        && feature_cursor.at_end()
    {
        return Ok(feature);
    }
    let mut cursor = Cursor::new(tokens);
    let query = cursor.parse_condition()?;
    cursor.expect_end()?;
    Ok(query)
}

impl Cursor {
    /// Parse a size feature in any of its three shapes:
    ///
    /// - `width: 600px` and the `min-`/`max-` aliases
    /// - `width >= 600px`
    /// - `400px <= width <= 700px`
    fn parse_size_feature(&mut self) -> Result<ContainerQuery, String> {
        if let Some(Token::Ident(name)) = self.peek().cloned()
            && let Some((feature, prefix)) = SizeFeatureName::parse(&name)
        {
            self.advance();
            if matches!(self.peek(), Some(Token::Colon)) {
                self.advance();
                if prefix.is_some() {
                    // Plain form: the prefix itself carries the comparison.
                }
                let comparison = prefix.unwrap_or(Comparison::Eq);
                let value_tokens = self.rest();
                let value = parse_feature_value(feature, &value_tokens)?;
                return Ok(ContainerQuery::Feature(SizeFeature {
                    name: feature,
                    comparison,
                    value,
                }));
            }
            if let Some(comparison) = self.consume_comparison() {
                if prefix.is_some() {
                    return Err(format!(
                        "'{name}' is not a size feature with a range comparison"
                    ));
                }
                let value_tokens = self.rest();
                let value = parse_feature_value(feature, &value_tokens)?;
                return Ok(ContainerQuery::Feature(SizeFeature {
                    name: feature,
                    comparison,
                    value,
                }));
            }
            return Err(format!("'{name}' is not followed by a comparison"));
        }

        // Value-first form: `400px <= width` or `400px <= width <= 700px`.
        let mut value_tokens = Vec::new();
        while let Some(token) = self.peek() {
            if matches!(token, Token::Comparison(_)) {
                break;
            }
            value_tokens.push(self.advance().expect("peeked token exists"));
        }
        if value_tokens.is_empty() {
            return Err("container condition has no feature".to_owned());
        }
        let first_comparison = self
            .consume_comparison()
            .ok_or_else(|| "expected a comparison operator".to_owned())?;
        let Some(Token::Ident(name)) = self.advance() else {
            return Err("expected a size feature name after the comparison".to_owned());
        };
        let Some((feature, prefix)) = SizeFeatureName::parse(&name) else {
            return Err(format!("unknown container size feature '{name}'"));
        };
        if prefix.is_some() {
            return Err(format!(
                "'{name}' is not a size feature with a range comparison"
            ));
        }
        let first_value = parse_feature_value(feature, &value_tokens)?;
        if self.at_end() {
            return Ok(ContainerQuery::Feature(SizeFeature {
                name: feature,
                comparison: first_comparison.reverse(),
                value: first_value,
            }));
        }
        let second_comparison = self
            .consume_comparison()
            .ok_or_else(|| "expected a second comparison operator".to_owned())?;
        let second_tokens = self.rest();
        let second_value = parse_feature_value(feature, &second_tokens)?;
        Ok(ContainerQuery::And(vec![
            ContainerQuery::Feature(SizeFeature {
                name: feature,
                comparison: first_comparison.reverse(),
                value: first_value,
            }),
            ContainerQuery::Feature(SizeFeature {
                name: feature,
                comparison: second_comparison,
                value: second_value,
            }),
        ]))
    }

    fn consume_comparison(&mut self) -> Option<Comparison> {
        if let Some(Token::Comparison(comparison)) = self.peek() {
            let comparison = *comparison;
            self.advance();
            return Some(comparison);
        }
        None
    }

    fn rest(&mut self) -> Vec<Token> {
        let rest = self.tokens.split_off(self.position);
        self.position = self.tokens.len();
        rest
    }
}

fn parse_feature_value(feature: SizeFeatureName, tokens: &[Token]) -> Result<FeatureValue, String> {
    match feature {
        SizeFeatureName::Orientation => match tokens {
            [Token::Ident(value)] if value.eq_ignore_ascii_case("portrait") => {
                Ok(FeatureValue::Keyword(Orientation::Portrait))
            }
            [Token::Ident(value)] if value.eq_ignore_ascii_case("landscape") => {
                Ok(FeatureValue::Keyword(Orientation::Landscape))
            }
            _ => Err("orientation takes 'portrait' or 'landscape'".to_owned()),
        },
        SizeFeatureName::AspectRatio => match tokens {
            [Token::Number(value)] => parse_ratio_number(value).map(FeatureValue::Ratio),
            [
                Token::Number(numerator),
                Token::Slash,
                Token::Number(denominator),
            ] => {
                let numerator = parse_ratio_number(numerator)?;
                let denominator = parse_ratio_number(denominator)?;
                if denominator == 0.0 {
                    return Err("aspect-ratio cannot divide by zero".to_owned());
                }
                Ok(FeatureValue::Ratio(numerator / denominator))
            }
            _ => Err("aspect-ratio takes a number or a ratio".to_owned()),
        },
        _ => match tokens {
            [Token::Dimension(value)] => Ok(FeatureValue::Length(value.clone())),
            [Token::Number(value)] if value == "0" => Ok(FeatureValue::Length(value.clone())),
            [Token::Function(value)] => Ok(FeatureValue::Length(value.clone())),
            _ => Err("a size feature takes one length value".to_owned()),
        },
    }
}

fn parse_ratio_number(text: &str) -> Result<f64, String> {
    text.parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
        .ok_or_else(|| "aspect-ratio components must be non-negative numbers".to_owned())
}

fn tokenize(source: &str) -> Result<Vec<Token>, String> {
    let mut tokenizer = Tokenizer {
        input: source,
        offset: 0,
    };
    let mut tokens = Vec::new();
    while let Some(token) = tokenizer.next_token()? {
        tokens.push(token);
    }
    Ok(tokens)
}

struct Tokenizer<'a> {
    input: &'a str,
    offset: usize,
}

impl<'a> Tokenizer<'a> {
    fn next_token(&mut self) -> Result<Option<Token>, String> {
        self.skip_trivia()?;
        let Some(character) = self.peek_char() else {
            return Ok(None);
        };
        match character {
            '(' => {
                self.offset += 1;
                Ok(Some(Token::Open))
            }
            ')' => {
                self.offset += 1;
                Ok(Some(Token::Close))
            }
            ':' => {
                self.offset += 1;
                Ok(Some(Token::Colon))
            }
            '/' => {
                self.offset += 1;
                Ok(Some(Token::Slash))
            }
            '<' => {
                self.offset += 1;
                if self.consume_char('=') {
                    Ok(Some(Token::Comparison(Comparison::Le)))
                } else {
                    Ok(Some(Token::Comparison(Comparison::Lt)))
                }
            }
            '>' => {
                self.offset += 1;
                if self.consume_char('=') {
                    Ok(Some(Token::Comparison(Comparison::Ge)))
                } else {
                    Ok(Some(Token::Comparison(Comparison::Gt)))
                }
            }
            '=' => {
                self.offset += 1;
                Ok(Some(Token::Comparison(Comparison::Eq)))
            }
            '+' | '-' | '.' if self.number_starts_here() => self.number_or_dimension(),
            character if is_ident_start(character) => self.identifier_or_function(),
            character if character.is_ascii_digit() => self.number_or_dimension(),
            other => Err(format!(
                "unsupported character '{other}' in container condition"
            )),
        }
    }

    fn skip_trivia(&mut self) -> Result<(), String> {
        loop {
            while self.peek_char().is_some_and(char::is_whitespace) {
                self.offset += 1;
            }
            if self.input[self.offset..].starts_with("/*") {
                let end = self.input[self.offset + 2..]
                    .find("*/")
                    .ok_or_else(|| "unclosed comment in container condition".to_owned())?;
                self.offset += end + 4;
                continue;
            }
            return Ok(());
        }
    }

    fn identifier_or_function(&mut self) -> Result<Option<Token>, String> {
        let name_start = self.offset;
        while self.peek_char().is_some_and(is_ident_continue) {
            self.offset += 1;
        }
        let identifier = &self.input[name_start..self.offset];
        if self.peek_char() == Some('(') {
            let end = self.take_function_text(name_start)?;
            return Ok(Some(Token::Function(end)));
        }
        Ok(Some(Token::Ident(identifier.to_owned())))
    }

    /// Consume a function from its name through its matching `)`, returning the
    /// whole callsite so `calc(100px + 1em)` reaches the length parser intact.
    fn take_function_text(&mut self, start: usize) -> Result<String, String> {
        debug_assert_eq!(self.peek_char(), Some('('));
        self.offset += 1;
        let mut depth = 1_usize;
        while let Some(character) = self.peek_char() {
            self.offset += 1;
            match character {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(self.input[start..self.offset].to_owned());
                    }
                }
                '"' | '\'' => self.take_string(character)?,
                _ => {}
            }
        }
        Err("unclosed function in container condition".to_owned())
    }

    fn take_string(&mut self, quote: char) -> Result<(), String> {
        while let Some(character) = self.peek_char() {
            self.offset += 1;
            if character == '\\' {
                if self.peek_char().is_some() {
                    self.offset += 1;
                }
            } else if character == quote {
                return Ok(());
            }
        }
        Err("unclosed string in container condition".to_owned())
    }

    fn number_starts_here(&self) -> bool {
        let mut characters = self.input[self.offset..].chars();
        match characters.next() {
            Some('+' | '-') => {
                let next = characters.next();
                next.is_some_and(|character| {
                    character.is_ascii_digit()
                        || (character == '.' && self.input[self.offset..].len() > 2)
                })
            }
            Some('.') => characters.next().is_some_and(|c| c.is_ascii_digit()),
            _ => false,
        }
    }

    fn number_or_dimension(&mut self) -> Result<Option<Token>, String> {
        let start = self.offset;
        if matches!(self.peek_char(), Some('+' | '-')) {
            self.offset += 1;
        }
        let mut digits = false;
        while self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
            digits = true;
            self.offset += 1;
        }
        if self.peek_char() == Some('.') {
            self.offset += 1;
            while self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
                digits = true;
                self.offset += 1;
            }
        }
        if !digits {
            return Err("invalid number in container condition".to_owned());
        }
        if matches!(self.peek_char(), Some('e' | 'E')) {
            let exponent_start = self.offset;
            self.offset += 1;
            if matches!(self.peek_char(), Some('+' | '-')) {
                self.offset += 1;
            }
            let exponent_digits = self.offset;
            while self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
                self.offset += 1;
            }
            if self.offset == exponent_digits {
                self.offset = exponent_start;
            }
        }
        let number = &self.input[start..self.offset];
        let unit_start = self.offset;
        if self.peek_char() == Some('%') {
            self.offset += 1;
        } else {
            while self.peek_char().is_some_and(is_ident_continue) {
                self.offset += 1;
            }
        }
        if self.offset == unit_start {
            Ok(Some(Token::Number(number.to_owned())))
        } else {
            Ok(Some(Token::Dimension(
                self.input[start..self.offset].to_owned(),
            )))
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.input[self.offset..].chars().next()
    }

    fn consume_char(&mut self, expected: char) -> bool {
        if self.peek_char() == Some(expected) {
            self.offset += expected.len_utf8();
            true
        } else {
            false
        }
    }
}

fn is_ident_start(character: char) -> bool {
    character.is_ascii_alphabetic() || character == '_' || character == '-'
}

fn is_ident_continue(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '-' || character == '_'
}

#[cfg(test)]
mod tests {
    use super::{
        Comparison, ContainerCondition, ContainerGeometry, ContainerQuery, ContainerType,
        FeatureValue, Orientation, SizeFeatureName, parse_container_condition,
        parse_container_names,
    };

    fn geometry(width: f32, height: f32) -> ContainerGeometry {
        ContainerGeometry {
            container_type: ContainerType::Size,
            names: vec!["card".to_owned()],
            width,
            height,
            font_size: 16.0,
        }
    }

    fn matches(source: &str, width: f32, height: f32) -> bool {
        let condition = parse_container_condition(source).expect("condition parses");
        let context = super::ContainerContext {
            viewport_width: 800.0,
            viewport_height: 600.0,
            root_font_size: 16.0,
            ..super::ContainerContext::default()
        };
        condition.query.matches(&geometry(width, height), &context)
    }

    #[test]
    fn plain_and_range_size_queries_compare_both_directions() {
        assert!(matches("(width >= 600px)", 600.0, 100.0));
        assert!(!matches("(width >= 600px)", 599.0, 100.0));
        assert!(matches("(400px <= width <= 700px)", 500.0, 100.0));
        assert!(!matches("(400px <= width <= 700px)", 300.0, 100.0));
        assert!(matches("(min-width: 40em)", 640.0, 100.0));
        assert!(matches("(max-height: 200px)", 100.0, 200.0));
        assert!(!matches("(max-height: 200px)", 100.0, 201.0));
    }

    #[test]
    fn orientation_and_aspect_ratio_read_both_axes() {
        assert!(matches("(orientation: landscape)", 200.0, 100.0));
        assert!(matches("(orientation: portrait)", 100.0, 200.0));
        assert!(matches("(aspect-ratio > 1)", 200.0, 100.0));
        assert!(matches("(aspect-ratio: 16/9)", 160.0, 90.0));
        assert!(!matches("(aspect-ratio: 16/9)", 160.0, 91.0));
    }

    #[test]
    fn boolean_combinators_nest_and_mixing_needs_parentheses() {
        let condition =
            parse_container_condition("(width > 100px) and (height > 100px)").expect("parses");
        assert!(matches!(
            &condition.query,
            ContainerQuery::And(parts) if parts.len() == 2
        ));
        assert!(
            parse_container_condition("(width > 1px) and (height > 1px) or (width > 2px)").is_err()
        );
        assert!(
            parse_container_condition("((width > 1px) and (height > 1px)) or (width > 2px)")
                .is_ok()
        );
        let not = parse_container_condition("not (width < 100px)").expect("parses");
        assert!(matches!(not.query, ContainerQuery::Not(_)));
    }

    #[test]
    fn a_leading_ident_is_the_container_name() {
        let condition = ContainerCondition::parse("card (width > 300px)").expect("parses");
        assert_eq!(condition.name.as_deref(), Some("card"));
        assert_eq!(
            condition.query,
            ContainerQuery::Feature(super::SizeFeature {
                name: SizeFeatureName::Width,
                comparison: Comparison::Gt,
                value: FeatureValue::Length("300px".to_owned()),
            })
        );
        let unnamed = parse_container_condition("(width > 300px)").expect("parses");
        assert!(unnamed.name.is_none());
    }

    #[test]
    fn style_queries_parse_as_features_and_stay_false_without_computed_styles() {
        let condition = parse_container_condition("style(--theme: dark)").expect("parses");
        assert!(matches!(
            condition.query,
            ContainerQuery::Style(super::StyleFeature { ref name, ref value })
                if name == "--theme" && value.as_deref() == Some("dark")
        ));
        assert!(condition.query.unsupported().is_none());
        // Size-only evaluation has no computed style to read, so the feature
        // answers false rather than guessing.
        assert!(!matches("style(--theme: dark)", 100.0, 100.0));
    }

    #[test]
    fn style_features_accept_boolean_declarations_and_functions() {
        let boolean = parse_container_condition("style(--theme)").expect("parses");
        assert!(matches!(
            boolean.query,
            ContainerQuery::Style(super::StyleFeature { ref name, value: None }) if name == "--theme"
        ));
        let function =
            parse_container_condition("style(--theme: var(--fallback))").expect("parses");
        assert!(matches!(
            function.query,
            ContainerQuery::Style(super::StyleFeature { value: Some(ref value), .. })
                if value == "var(--fallback)"
        ));
        // A standard property name is case-folded and a colon inside a string
        // does not split the declaration.
        let standard = parse_container_condition("style(Font-Weight: bold)").expect("parses");
        assert!(matches!(
            standard.query,
            ContainerQuery::Style(super::StyleFeature { ref name, .. }) if name == "font-weight"
        ));
        let quoted = parse_container_condition("style(content: \"a:b\")").expect("parses");
        assert!(matches!(
            quoted.query,
            ContainerQuery::Style(super::StyleFeature { value: Some(ref value), .. })
                if value == "\"a:b\""
        ));
        // A vendor prefix is a single leading hyphen, unlike the double prefix
        // of a custom property.
        let prefixed = parse_container_condition("style(-webkit-transform: none)").expect("parses");
        assert!(matches!(
            prefixed.query,
            ContainerQuery::Style(super::StyleFeature { ref name, .. })
                if name == "-webkit-transform"
        ));
    }

    #[test]
    fn container_names_ignore_none_and_duplicates() {
        assert_eq!(
            parse_container_names("card sidebar card"),
            ["card", "sidebar"]
        );
        assert!(parse_container_names("none").is_empty());
    }

    #[test]
    fn container_type_eligibility_is_axis_specific() {
        let inline = parse_container_condition("(width > 1px)").expect("parses");
        assert!(inline.is_eligible_for(ContainerType::InlineSize));
        let block = parse_container_condition("(height > 1px)").expect("parses");
        assert!(!block.is_eligible_for(ContainerType::InlineSize));
        assert!(block.is_eligible_for(ContainerType::Size));
        assert_eq!(parse_container_names("a b"), ["a", "b"]);
        assert_eq!(
            super::SizeFeatureName::parse("min-width"),
            Some((SizeFeatureName::Width, Some(Comparison::Ge)))
        );
        assert_eq!(Orientation::Landscape, Orientation::Landscape);
    }

    fn unit_tree() -> (render_dom::Dom, render_dom::NodeId, render_dom::NodeId) {
        let mut dom = render_dom::Dom::new();
        let outer = dom.create_element("div");
        let inner = dom.create_element("div");
        let target = dom.create_element("div");
        dom.append_child(dom.document(), outer)
            .expect("outer attaches to the document");
        dom.append_child(outer, inner)
            .expect("inner attaches to outer");
        dom.append_child(inner, target)
            .expect("target attaches to inner");
        (dom, outer, target)
    }

    fn size_container(width: f32, height: f32) -> ContainerGeometry {
        ContainerGeometry {
            container_type: ContainerType::Size,
            names: Vec::new(),
            width,
            height,
            font_size: 16.0,
        }
    }

    fn inline_container(width: f32, height: f32) -> ContainerGeometry {
        ContainerGeometry {
            container_type: ContainerType::InlineSize,
            names: Vec::new(),
            width,
            height,
            font_size: 16.0,
        }
    }

    #[test]
    fn container_units_pick_each_axis_nearest_eligible_ancestor() {
        let (dom, outer, target) = unit_tree();
        let inner = dom.parent(target).expect("target has a parent");
        let context = super::ContainerContext {
            containers: [
                (outer, size_container(600.0, 400.0)),
                (inner, inline_container(500.0, 200.0)),
            ]
            .into_iter()
            .collect(),
            viewport_width: 1_000.0,
            viewport_height: 800.0,
            root_font_size: 16.0,
        };

        // The inner inline-size container answers the inline axis; the outer
        // size container is the nearest one that accepts the block axis.
        assert_eq!(
            context.sizes_for_element(&dom, target, true),
            super::ContainerSizes {
                width: Some(500.0),
                height: Some(400.0),
                inline_size: Some(500.0),
                block_size: Some(400.0),
            }
        );

        // A container's own `cq*` units read its ancestors, never itself.
        assert_eq!(
            context.sizes_for_element(&dom, inner, true),
            super::ContainerSizes {
                width: Some(600.0),
                height: Some(400.0),
                inline_size: Some(600.0),
                block_size: Some(400.0),
            }
        );

        // The outermost container has no eligible ancestor; the resolver's
        // small-viewport fallback applies.
        assert_eq!(
            context.sizes_for_element(&dom, outer, true),
            super::ContainerSizes::default()
        );
    }

    #[test]
    fn physical_container_units_follow_the_writing_mode() {
        let (dom, outer, target) = unit_tree();
        let inner = dom.parent(target).expect("target has a parent");
        let context = super::ContainerContext {
            containers: [
                (outer, size_container(600.0, 400.0)),
                (inner, inline_container(500.0, 200.0)),
            ]
            .into_iter()
            .collect(),
            viewport_width: 1_000.0,
            viewport_height: 800.0,
            root_font_size: 16.0,
        };

        // In a vertical writing mode inline is the physical height, so `cqw`
        // reads the block-axis container's width and `cqh` the inline
        // container's height.
        assert_eq!(
            context.sizes_for_element(&dom, target, false),
            super::ContainerSizes {
                width: Some(600.0),
                height: Some(200.0),
                inline_size: Some(200.0),
                block_size: Some(600.0),
            }
        );
    }

    #[test]
    fn container_sizes_fill_the_length_resolution_context() {
        use crate::properties::{Length, LengthPercentage, LengthResolutionContext, LengthUnit};

        let (dom, outer, target) = unit_tree();
        let inner = dom.parent(target).expect("target has a parent");
        let context = super::ContainerContext {
            containers: [
                (outer, size_container(600.0, 400.0)),
                (inner, inline_container(500.0, 200.0)),
            ]
            .into_iter()
            .collect(),
            ..super::ContainerContext::default()
        };
        let mut resolution = LengthResolutionContext {
            // The fallback when no eligible container exists is the small
            // viewport size, so make it distinguishable from both containers.
            small_viewport_width: Some(1_000.0),
            small_viewport_height: Some(800.0),
            ..LengthResolutionContext::default()
        };
        context.apply_container_sizes(&dom, target, true, &mut resolution);

        let resolve = |resolution: &LengthResolutionContext, unit| {
            LengthPercentage::Length(Length { value: 10.0, unit })
                .resolve(resolution)
                .expect("container lengths resolve")
        };
        assert_eq!(resolve(&resolution, LengthUnit::Cqw), 50.0);
        assert_eq!(resolve(&resolution, LengthUnit::Cqh), 40.0);
        assert_eq!(resolve(&resolution, LengthUnit::Cqi), 50.0);
        assert_eq!(resolve(&resolution, LengthUnit::Cqb), 40.0);
        assert_eq!(resolve(&resolution, LengthUnit::Cqmin), 40.0);
        assert_eq!(resolve(&resolution, LengthUnit::Cqmax), 50.0);

        // No eligible ancestor: every axis falls back to the small viewport.
        context.apply_container_sizes(&dom, outer, true, &mut resolution);
        assert_eq!(resolution.container_width, None);
        assert_eq!(resolve(&resolution, LengthUnit::Cqw), 100.0);
        assert_eq!(resolve(&resolution, LengthUnit::Cqh), 80.0);
    }
}
