//! Layout-backed content hit testing, hyperlink intent, and pointer selection.
//!
//! This module is deliberately GUI-free. Frontends pass viewport coordinates
//! and the scroll offset used for the corresponding paint; results remain tied
//! to the immutable [`FragmentTree`] DOM revision.

use std::cmp::Ordering;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;

use url::Url;

use crate::dom::{Dom, DomRevision, Namespace, NodeId, NodeKind};
use crate::layout::{
    FragmentId, FragmentKind, FragmentTree, PhysicalPoint, PhysicalRect, TextMeasurer, TextStyle,
};

use super::{BoundaryPoint, DomRange, InteractionError, Selection, compare_boundary_points};

/// Work bounds for one interaction query. These are independent from layout
/// limits because an embedder may use a much smaller synchronous input budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InteractionLimits {
    pub max_fragments: usize,
    pub max_fragment_depth: usize,
    pub max_text_characters: usize,
    pub max_dom_utf16_units: usize,
    pub max_ancestor_depth: usize,
    pub max_selection_rects: usize,
}

impl Default for InteractionLimits {
    fn default() -> Self {
        Self {
            max_fragments: 1_000_000,
            max_fragment_depth: 4_096,
            max_text_characters: 16 * 1_024 * 1_024,
            max_dom_utf16_units: 32 * 1_024 * 1_024,
            max_ancestor_depth: 4_096,
            max_selection_rects: 1_000_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InteractionResource {
    Fragments,
    FragmentDepth,
    TextCharacters,
    DomTextUtf16Units,
    AncestorDepth,
    SelectionRects,
}

#[derive(Debug)]
pub enum HitTestError {
    StaleFragmentTree {
        fragment_revision: DomRevision,
        dom_revision: DomRevision,
    },
    InvalidCoordinate,
    InvalidTextBoundary {
        container: NodeId,
    },
    ResourceLimit {
        resource: InteractionResource,
        limit: usize,
    },
    InvalidHyperlink {
        link: NodeId,
        href: String,
        reason: String,
    },
    Selection(InteractionError),
}

impl fmt::Display for HitTestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleFragmentTree {
                fragment_revision,
                dom_revision,
            } => write!(
                formatter,
                "fragment tree revision {} does not match DOM revision {}",
                fragment_revision.as_u64(),
                dom_revision.as_u64()
            ),
            Self::InvalidCoordinate => formatter.write_str("pointer coordinates must be finite"),
            Self::InvalidTextBoundary { container } => write!(
                formatter,
                "node {} is not a text boundary container",
                container.as_u64()
            ),
            Self::ResourceLimit { resource, limit } => {
                write!(formatter, "{resource:?} interaction limit {limit} exceeded")
            }
            Self::InvalidHyperlink { href, reason, .. } => {
                write!(formatter, "could not resolve hyperlink '{href}': {reason}")
            }
            Self::Selection(error) => error.fmt(formatter),
        }
    }
}

impl Error for HitTestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Selection(error) => Some(error),
            _ => None,
        }
    }
}

impl From<InteractionError> for HitTestError {
    fn from(error: InteractionError) -> Self {
        Self::Selection(error)
    }
}

/// The deepest sourced fragment under a viewport point in paint order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HitTestResult {
    pub node: NodeId,
    pub fragment: FragmentId,
    pub viewport_point: PhysicalPoint,
    pub document_point: PhysicalPoint,
    /// Present when the hit fragment is selectable rendered text.
    pub text_position: Option<BoundaryPoint>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavigationIntent {
    pub link: NodeId,
    pub hit_node: NodeId,
    pub href: String,
    pub destination: Url,
}

/// A typed default action request. Core describes it; the frontend decides
/// whether and how to dispatch or perform it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivationIntent {
    Navigate(NavigationIntent),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectionRect {
    pub node: NodeId,
    pub fragment: FragmentId,
    pub document_rect: PhysicalRect,
    pub viewport_rect: PhysicalRect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SelectionGeometry {
    pub dom_revision: DomRevision,
    pub rects: Vec<SelectionRect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundaryBias {
    Backward,
    Forward,
    Nearest,
}

/// Immutable browser-side wiring object for one DOM/layout snapshot.
#[derive(Clone, Copy)]
pub struct InteractionScene<'a> {
    dom: &'a Dom,
    fragments: &'a FragmentTree,
    text_measurer: &'a dyn TextMeasurer,
    scroll_offset: PhysicalPoint,
    limits: InteractionLimits,
}

impl fmt::Debug for InteractionScene<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InteractionScene")
            .field("dom_revision", &self.dom.revision())
            .field("fragment_revision", &self.fragments.dom_revision)
            .field("scroll_offset", &self.scroll_offset)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl<'a> InteractionScene<'a> {
    /// Bind input handling to exactly the snapshot that was painted.
    ///
    /// # Errors
    ///
    /// Rejects a fragment tree made for any other DOM revision.
    pub fn new(
        dom: &'a Dom,
        fragments: &'a FragmentTree,
        text_measurer: &'a dyn TextMeasurer,
        scroll_offset: PhysicalPoint,
        limits: InteractionLimits,
    ) -> Result<Self, HitTestError> {
        if fragments.dom_revision != dom.revision() {
            return Err(HitTestError::StaleFragmentTree {
                fragment_revision: fragments.dom_revision,
                dom_revision: dom.revision(),
            });
        }
        if !scroll_offset.x.is_finite() || !scroll_offset.y.is_finite() {
            return Err(HitTestError::InvalidCoordinate);
        }
        Ok(Self {
            dom,
            fragments,
            text_measurer,
            scroll_offset: fragments.clamp_scroll_offset(scroll_offset),
            limits,
        })
    }

    #[must_use]
    pub const fn dom(&self) -> &Dom {
        self.dom
    }

    #[must_use]
    pub const fn fragments(&self) -> &FragmentTree {
        self.fragments
    }

    #[must_use]
    pub const fn scroll_offset(&self) -> PhysicalPoint {
        self.scroll_offset
    }

    /// Resolve the deepest sourced fragment at `viewport_point`. Later-painted
    /// siblings win, and descendants win over their containing box.
    ///
    /// # Errors
    ///
    /// Returns a resource error if traversal exceeds the configured budget.
    pub fn hit_test(
        &self,
        viewport_point: PhysicalPoint,
    ) -> Result<Option<HitTestResult>, HitTestError> {
        validate_point(viewport_point)?;
        let document_point = self
            .fragments
            .viewport_to_document_point(viewport_point, self.scroll_offset);
        let paint_order = self.paint_order()?;
        let mut hit = None;
        for fragment_id in paint_order.iter().rev() {
            let Some(fragment) = self.fragments.get(*fragment_id) else {
                continue;
            };
            if contains_point(fragment.rect, document_point)
                && let Some(node) = fragment.source
            {
                hit = Some((node, *fragment_id));
                break;
            }
        }
        let Some((node, fragment)) = hit else {
            return Ok(None);
        };
        let maps = self.text_maps(&paint_order)?;
        let text_position = maps
            .iter()
            .find(|map| map.fragment == fragment)
            .map(|map| map.caret_at(document_point.x));
        Ok(Some(HitTestResult {
            node,
            fragment,
            viewport_point,
            document_point,
            text_position,
        }))
    }

    /// Find a text caret at a viewport point. When the deepest box is not text,
    /// the geometrically nearest rendered text fragment is used so drag
    /// selection can continue through line and element gaps.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid coordinates or exhausted traversal/text
    /// budgets.
    pub fn caret_at(
        &self,
        viewport_point: PhysicalPoint,
    ) -> Result<Option<BoundaryPoint>, HitTestError> {
        validate_point(viewport_point)?;
        let document_point = self
            .fragments
            .viewport_to_document_point(viewport_point, self.scroll_offset);
        let paint_order = self.paint_order()?;
        let maps = self.text_maps(&paint_order)?;
        if let Some(map) = maps
            .iter()
            .rev()
            .find(|map| contains_point(map.rect, document_point))
        {
            return Ok(Some(map.caret_at(document_point.x)));
        }
        Ok(maps
            .iter()
            .min_by(|left, right| {
                distance_squared(left.rect, document_point)
                    .total_cmp(&distance_squared(right.rect, document_point))
            })
            .map(|map| map.caret_at(document_point.x)))
    }

    /// Walk from a hit DOM node to its nearest HTML `a[href]` ancestor and
    /// resolve the raw href against the caller's document base URL.
    ///
    /// # Errors
    ///
    /// Returns an error when the ancestor budget is exhausted or the href
    /// cannot be resolved as a URL.
    pub fn activation_intent(
        &self,
        hit_node: NodeId,
        document_base_url: &Url,
    ) -> Result<Option<ActivationIntent>, HitTestError> {
        let mut candidate = Some(hit_node);
        let mut depth = 0_usize;
        while let Some(node) = candidate {
            if depth >= self.limits.max_ancestor_depth {
                return Err(limit(
                    InteractionResource::AncestorDepth,
                    self.limits.max_ancestor_depth,
                ));
            }
            depth += 1;
            if let Some(element) = self.dom.node(node).and_then(|node| match node.kind() {
                NodeKind::Element(element)
                    if element.namespace == Namespace::Html && element.local_name == "a" =>
                {
                    Some(element)
                }
                _ => None,
            }) {
                let _ = element;
                if let Some(href) = self.dom.attribute(node, "href").ok().flatten() {
                    let destination = document_base_url.join(href).map_err(|error| {
                        HitTestError::InvalidHyperlink {
                            link: node,
                            href: href.to_owned(),
                            reason: error.to_string(),
                        }
                    })?;
                    return Ok(Some(ActivationIntent::Navigate(NavigationIntent {
                        link: node,
                        hit_node,
                        href: href.to_owned(),
                        destination,
                    })));
                }
            }
            candidate = self.dom.parent(node);
        }
        Ok(None)
    }

    /// Convert the current DOM Selection to highlight rectangles in both
    /// document and painted viewport coordinates.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid selection boundaries or exhausted
    /// fragment, text, or rectangle budgets.
    pub fn selection_geometry(
        &self,
        selection: &Selection,
    ) -> Result<SelectionGeometry, HitTestError> {
        let Some(range) = selection.range() else {
            return Ok(SelectionGeometry {
                dom_revision: self.dom.revision(),
                rects: Vec::new(),
            });
        };
        if range.collapsed() {
            return Ok(SelectionGeometry {
                dom_revision: self.dom.revision(),
                rects: Vec::new(),
            });
        }
        let paint_order = self.paint_order()?;
        let maps = self.text_maps(&paint_order)?;
        let mut rects = Vec::new();
        for map in &maps {
            let Some((start_x, end_x)) = selected_x_span(self.dom, range, map)? else {
                continue;
            };
            if rects.len() >= self.limits.max_selection_rects {
                return Err(limit(
                    InteractionResource::SelectionRects,
                    self.limits.max_selection_rects,
                ));
            }
            let document_rect = PhysicalRect::new(
                map.rect.origin.x + start_x,
                map.rect.origin.y,
                (end_x - start_x).max(0.0),
                map.rect.size.height,
            );
            rects.push(SelectionRect {
                node: map.node,
                fragment: map.fragment,
                document_rect,
                viewport_rect: translate_rect(
                    document_rect,
                    -self.scroll_offset.x,
                    -self.scroll_offset.y,
                ),
            });
        }
        Ok(SelectionGeometry {
            dom_revision: self.dom.revision(),
            rects,
        })
    }

    /// Clamp a possibly stale/external UTF-16 text offset to a Unicode scalar
    /// boundary in the current DOM text.
    ///
    /// # Errors
    ///
    /// Returns an error when the boundary container is not a text node.
    pub fn clamp_text_boundary(
        &self,
        point: BoundaryPoint,
        bias: BoundaryBias,
    ) -> Result<BoundaryPoint, HitTestError> {
        clamp_text_boundary(self.dom, point, bias)
    }

    /// Expand a UTF-16 caret to a simple Unicode-aware word boundary. CJK
    /// ideographs and non-word symbols form individual units; alphabetic and
    /// numeric runs (plus `_`) are grouped.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-text container, invalid DOM range, or an
    /// exhausted text budget.
    pub fn word_range_at(&self, point: BoundaryPoint) -> Result<DomRange, HitTestError> {
        let point = self.clamp_text_boundary(point, BoundaryBias::Nearest)?;
        let text = text_data(self.dom, point.container)?;
        let characters = text_characters(text);
        if characters.len() > self.limits.max_text_characters {
            return Err(limit(
                InteractionResource::TextCharacters,
                self.limits.max_text_characters,
            ));
        }
        if characters.is_empty() {
            return DomRange::new(self.dom, point, point).map_err(Into::into);
        }
        let mut index = characters
            .iter()
            .position(|character| point.offset < character.utf16_end)
            .unwrap_or(characters.len() - 1);
        if point.offset == characters[index].utf16_end && index + 1 < characters.len() {
            index += 1;
        }
        let class = word_class(characters[index].character);
        let mut start = index;
        let mut end = index + 1;
        if class.is_grouped() {
            while start > 0 && word_class(characters[start - 1].character) == class {
                start -= 1;
            }
            while end < characters.len() && word_class(characters[end].character) == class {
                end += 1;
            }
        }
        DomRange::new(
            self.dom,
            BoundaryPoint::new(point.container, characters[start].utf16_start),
            BoundaryPoint::new(point.container, characters[end - 1].utf16_end),
        )
        .map_err(Into::into)
    }

    fn paint_order(&self) -> Result<Vec<FragmentId>, HitTestError> {
        let mut order = Vec::new();
        let mut stack = vec![(self.fragments.root(), 0_usize)];
        let mut seen = HashSet::new();
        while let Some((fragment_id, depth)) = stack.pop() {
            if depth > self.limits.max_fragment_depth {
                return Err(limit(
                    InteractionResource::FragmentDepth,
                    self.limits.max_fragment_depth,
                ));
            }
            if !seen.insert(fragment_id) {
                continue;
            }
            if order.len() >= self.limits.max_fragments {
                return Err(limit(
                    InteractionResource::Fragments,
                    self.limits.max_fragments,
                ));
            }
            order.push(fragment_id);
            if let Some(fragment) = self.fragments.get(fragment_id) {
                stack.extend(
                    fragment
                        .children
                        .iter()
                        .rev()
                        .map(|child| (*child, depth.saturating_add(1))),
                );
            }
        }
        Ok(order)
    }

    fn text_maps(&self, paint_order: &[FragmentId]) -> Result<Vec<TextMap>, HitTestError> {
        let mut source_characters: HashMap<NodeId, Vec<SourceCharacter>> = HashMap::new();
        let mut source_cursors = HashMap::<NodeId, usize>::new();
        let mut total_dom_utf16 = 0_usize;
        let mut total_rendered_characters = 0_usize;
        let mut maps = Vec::new();
        for fragment_id in paint_order {
            let Some(fragment) = self.fragments.get(*fragment_id) else {
                continue;
            };
            let FragmentKind::Text(text_fragment) = &fragment.kind else {
                continue;
            };
            let Some(node) = fragment.source else {
                continue;
            };
            let Some(NodeKind::Text(source)) = self.dom.node(node).map(crate::dom::Node::kind)
            else {
                continue;
            };
            if let Entry::Vacant(entry) = source_characters.entry(node) {
                total_dom_utf16 = total_dom_utf16.saturating_add(source.encode_utf16().count());
                if total_dom_utf16 > self.limits.max_dom_utf16_units {
                    return Err(limit(
                        InteractionResource::DomTextUtf16Units,
                        self.limits.max_dom_utf16_units,
                    ));
                }
                entry.insert(text_characters(source));
            }
            let rendered_count = text_fragment.text.chars().count();
            total_rendered_characters = total_rendered_characters.saturating_add(rendered_count);
            if total_rendered_characters > self.limits.max_text_characters {
                return Err(limit(
                    InteractionResource::TextCharacters,
                    self.limits.max_text_characters,
                ));
            }
            let source = &source_characters[&node];
            let cursor = source_cursors.entry(node).or_default();
            let mut characters = Vec::with_capacity(rendered_count);
            for rendered in text_fragment.text.chars() {
                let Some((source_start, source_end)) =
                    match_rendered_character(source, cursor, rendered)
                else {
                    continue;
                };
                characters.push(MappedCharacter {
                    dom_start: source_start,
                    dom_end: source_end,
                    advance: measure_character(
                        self.text_measurer,
                        rendered,
                        text_fragment.font_size,
                        fragment.rect.size.height,
                    ),
                });
            }
            normalize_advances(&mut characters, fragment.rect.size.width);
            if !characters.is_empty() {
                maps.push(TextMap {
                    fragment: *fragment_id,
                    node,
                    rect: fragment.rect,
                    characters,
                });
            }
        }
        Ok(maps)
    }
}

/// Stateful pointer gesture adapter that constructs the existing DOM
/// [`Selection`] model. The state itself has no platform event dependency.
#[derive(Clone, Debug, Default)]
pub struct PointerSelection {
    selection: Selection,
    pressed_hit: Option<NodeId>,
    pressed_link: Option<NodeId>,
    pointer_down: bool,
    dragged: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PointerOutcome {
    pub hit: Option<HitTestResult>,
    pub selection_changed: bool,
    pub activation: Option<ActivationIntent>,
}

impl PointerSelection {
    #[must_use]
    pub const fn selection(&self) -> &Selection {
        &self.selection
    }

    pub fn selection_mut(&mut self) -> &mut Selection {
        &mut self.selection
    }

    /// Start a pointer selection gesture and collapse at the nearest caret.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input, interaction resource exhaustion, or
    /// a rejected DOM selection boundary.
    pub fn pointer_down(
        &mut self,
        scene: &InteractionScene<'_>,
        point: PhysicalPoint,
    ) -> Result<PointerOutcome, HitTestError> {
        let hit = scene.hit_test(point)?;
        let caret = scene.caret_at(point)?;
        let selection_changed = if let Some(caret) = caret {
            self.selection.collapse(scene.dom, caret)?;
            true
        } else {
            false
        };
        self.pressed_hit = hit.map(|hit| hit.node);
        self.pressed_link = match self.pressed_hit {
            Some(node) => nearest_link(scene, node)?,
            None => None,
        };
        self.pointer_down = true;
        self.dragged = false;
        Ok(PointerOutcome {
            hit,
            selection_changed,
            activation: None,
        })
    }

    /// Extend an active pointer selection gesture.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input, interaction resource exhaustion, or
    /// a rejected DOM selection boundary.
    pub fn pointer_drag(
        &mut self,
        scene: &InteractionScene<'_>,
        point: PhysicalPoint,
    ) -> Result<PointerOutcome, HitTestError> {
        let hit = scene.hit_test(point)?;
        if !self.pointer_down {
            return Ok(PointerOutcome {
                hit,
                selection_changed: false,
                activation: None,
            });
        }
        let selection_changed = if let Some(caret) = scene.caret_at(point)? {
            self.selection.extend(scene.dom, caret)?;
            true
        } else {
            false
        };
        self.dragged = true;
        Ok(PointerOutcome {
            hit,
            selection_changed,
            activation: None,
        })
    }

    /// Finish a gesture and, for an un-dragged same-link click, return a typed
    /// activation intent.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input, interaction resource exhaustion, a
    /// rejected selection boundary, or an invalid hyperlink URL.
    pub fn pointer_up(
        &mut self,
        scene: &InteractionScene<'_>,
        point: PhysicalPoint,
        document_base_url: &Url,
    ) -> Result<PointerOutcome, HitTestError> {
        let hit = scene.hit_test(point)?;
        let selection_changed = if self.pointer_down {
            if let Some(caret) = scene.caret_at(point)? {
                self.selection.extend(scene.dom, caret)?;
                true
            } else {
                false
            }
        } else {
            false
        };
        let release_link = match hit {
            Some(hit) => nearest_link(scene, hit.node)?,
            None => None,
        };
        let activation = if self.pointer_down && !self.dragged && self.pressed_link == release_link
        {
            match hit {
                Some(hit) => scene.activation_intent(hit.node, document_base_url)?,
                None => None,
            }
        } else {
            None
        };
        self.pointer_down = false;
        self.pressed_hit = None;
        self.pressed_link = None;
        self.dragged = false;
        Ok(PointerOutcome {
            hit,
            selection_changed,
            activation,
        })
    }
}

/// Clamp a UTF-16 offset without ever returning the middle of a surrogate
/// pair. Offsets outside the string are clamped to its ends.
///
/// # Errors
///
/// Returns an error when `point.container` is not a text node.
pub fn clamp_text_boundary(
    dom: &Dom,
    point: BoundaryPoint,
    bias: BoundaryBias,
) -> Result<BoundaryPoint, HitTestError> {
    let text = text_data(dom, point.container)?;
    let length = text.encode_utf16().count();
    let offset = point.offset.min(length);
    if offset == 0 || offset == length {
        return Ok(BoundaryPoint::new(point.container, offset));
    }
    let characters = text_characters(text);
    if characters
        .iter()
        .any(|character| character.utf16_start == offset || character.utf16_end == offset)
    {
        return Ok(BoundaryPoint::new(point.container, offset));
    }
    let Some(character) = characters
        .iter()
        .find(|character| character.utf16_start < offset && offset < character.utf16_end)
    else {
        return Ok(BoundaryPoint::new(point.container, offset));
    };
    let offset = match bias {
        BoundaryBias::Backward => character.utf16_start,
        BoundaryBias::Forward => character.utf16_end,
        BoundaryBias::Nearest => {
            if offset - character.utf16_start <= character.utf16_end - offset {
                character.utf16_start
            } else {
                character.utf16_end
            }
        }
    };
    Ok(BoundaryPoint::new(point.container, offset))
}

fn nearest_link(
    scene: &InteractionScene<'_>,
    hit_node: NodeId,
) -> Result<Option<NodeId>, HitTestError> {
    let mut candidate = Some(hit_node);
    let mut depth = 0_usize;
    while let Some(node) = candidate {
        if depth >= scene.limits.max_ancestor_depth {
            return Err(limit(
                InteractionResource::AncestorDepth,
                scene.limits.max_ancestor_depth,
            ));
        }
        depth += 1;
        if matches!(
            scene.dom.node(node).map(crate::dom::Node::kind),
            Some(NodeKind::Element(element))
                if element.namespace == Namespace::Html
                    && element.local_name == "a"
                    && scene.dom.attribute(node, "href").ok().flatten().is_some()
        ) {
            return Ok(Some(node));
        }
        candidate = scene.dom.parent(node);
    }
    Ok(None)
}

#[derive(Clone, Copy, Debug)]
struct SourceCharacter {
    character: char,
    utf16_start: usize,
    utf16_end: usize,
}

#[derive(Clone, Copy, Debug)]
struct MappedCharacter {
    dom_start: usize,
    dom_end: usize,
    advance: f32,
}

#[derive(Clone, Debug)]
struct TextMap {
    fragment: FragmentId,
    node: NodeId,
    rect: PhysicalRect,
    characters: Vec<MappedCharacter>,
}

impl TextMap {
    fn caret_at(&self, document_x: f32) -> BoundaryPoint {
        let local_x = (document_x - self.rect.origin.x).max(0.0);
        let mut x = 0.0;
        for character in &self.characters {
            if local_x < x + character.advance / 2.0 {
                return BoundaryPoint::new(self.node, character.dom_start);
            }
            x += character.advance;
        }
        BoundaryPoint::new(
            self.node,
            self.characters
                .last()
                .map_or(0, |character| character.dom_end),
        )
    }
}

fn text_characters(text: &str) -> Vec<SourceCharacter> {
    let mut utf16_offset = 0_usize;
    text.chars()
        .map(|character| {
            let utf16_start = utf16_offset;
            utf16_offset += character.len_utf16();
            SourceCharacter {
                character,
                utf16_start,
                utf16_end: utf16_offset,
            }
        })
        .collect()
}

fn match_rendered_character(
    source: &[SourceCharacter],
    cursor: &mut usize,
    rendered: char,
) -> Option<(usize, usize)> {
    let relative = source.get(*cursor..)?;
    let found = if rendered.is_whitespace() {
        relative
            .iter()
            .position(|character| character.character.is_whitespace())?
    } else {
        relative
            .iter()
            .position(|character| character.character == rendered)?
    };
    let index = cursor.saturating_add(found);
    let start = source[index].utf16_start;
    let mut end_index = index + 1;
    if rendered.is_whitespace() {
        while end_index < source.len() && source[end_index].character.is_whitespace() {
            end_index += 1;
        }
    }
    let end = source[end_index - 1].utf16_end;
    *cursor = end_index;
    Some((start, end))
}

fn measure_character(
    measurer: &dyn TextMeasurer,
    character: char,
    font_size: f32,
    line_height: f32,
) -> f32 {
    let mut encoded = [0_u8; 4];
    let advance = measurer
        .measure(
            character.encode_utf8(&mut encoded),
            TextStyle {
                font_size,
                line_height,
            },
        )
        .advance;
    if advance.is_finite() {
        advance.max(0.0)
    } else {
        0.0
    }
}

fn normalize_advances(characters: &mut [MappedCharacter], fragment_width: f32) {
    let measured_width: f32 = characters.iter().map(|character| character.advance).sum();
    let fragment_width = finite_non_negative(fragment_width);
    if measured_width > 0.0 {
        let scale = fragment_width / measured_width;
        for character in characters {
            character.advance *= scale;
        }
    } else if !characters.is_empty() {
        #[allow(clippy::cast_precision_loss)]
        let equal_advance = fragment_width / characters.len() as f32;
        for character in characters {
            character.advance = equal_advance;
        }
    }
}

fn selected_x_span(
    dom: &Dom,
    range: DomRange,
    map: &TextMap,
) -> Result<Option<(f32, f32)>, HitTestError> {
    let mut x = 0.0;
    let mut selected_start = None;
    let mut selected_end = 0.0;
    for character in &map.characters {
        let start = BoundaryPoint::new(map.node, character.dom_start);
        let end = BoundaryPoint::new(map.node, character.dom_end);
        let ends_after_range_start =
            compare_boundary_points(dom, end, range.start())? == Ordering::Greater;
        let starts_before_range_end =
            compare_boundary_points(dom, start, range.end())? == Ordering::Less;
        if ends_after_range_start && starts_before_range_end {
            selected_start.get_or_insert(x);
            selected_end = x + character.advance;
        }
        x += character.advance;
    }
    Ok(selected_start.map(|start| (start, selected_end)))
}

fn text_data(dom: &Dom, node: NodeId) -> Result<&str, HitTestError> {
    match dom.node(node).map(crate::dom::Node::kind) {
        Some(NodeKind::Text(text)) => Ok(text),
        _ => Err(HitTestError::InvalidTextBoundary { container: node }),
    }
}

fn validate_point(point: PhysicalPoint) -> Result<(), HitTestError> {
    if point.x.is_finite() && point.y.is_finite() {
        Ok(())
    } else {
        Err(HitTestError::InvalidCoordinate)
    }
}

fn contains_point(rect: PhysicalRect, point: PhysicalPoint) -> bool {
    let left = rect.origin.x.min(rect.right());
    let right = rect.origin.x.max(rect.right());
    let top = rect.origin.y.min(rect.bottom());
    let bottom = rect.origin.y.max(rect.bottom());
    point.x >= left && point.x <= right && point.y >= top && point.y <= bottom
}

fn distance_squared(rect: PhysicalRect, point: PhysicalPoint) -> f32 {
    let left = rect.origin.x.min(rect.right());
    let right = rect.origin.x.max(rect.right());
    let top = rect.origin.y.min(rect.bottom());
    let bottom = rect.origin.y.max(rect.bottom());
    let dx = if point.x < left {
        left - point.x
    } else if point.x > right {
        point.x - right
    } else {
        0.0
    };
    let dy = if point.y < top {
        top - point.y
    } else if point.y > bottom {
        point.y - bottom
    } else {
        0.0
    };
    dx.mul_add(dx, dy * dy)
}

fn translate_rect(rect: PhysicalRect, dx: f32, dy: f32) -> PhysicalRect {
    PhysicalRect::new(
        rect.origin.x + dx,
        rect.origin.y + dy,
        rect.size.width,
        rect.size.height,
    )
}

fn finite_non_negative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn limit(resource: InteractionResource, limit: usize) -> HitTestError {
    HitTestError::ResourceLimit { resource, limit }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WordClass {
    Whitespace,
    Word,
    Cjk,
    Symbol,
}

impl WordClass {
    const fn is_grouped(self) -> bool {
        matches!(self, Self::Whitespace | Self::Word)
    }
}

fn word_class(character: char) -> WordClass {
    if character.is_whitespace() {
        WordClass::Whitespace
    } else if is_cjk(character) {
        WordClass::Cjk
    } else if character.is_alphanumeric() || character == '_' {
        WordClass::Word
    } else {
        WordClass::Symbol
    }
}

const fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x2e80..=0x2fff
            | 0x3040..=0x30ff
            | 0x31f0..=0x31ff
            | 0x3400..=0x4dbf
            | 0x4e00..=0x9fff
            | 0xac00..=0xd7af
            | 0xf900..=0xfaff
            | 0x20000..=0x3134f
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]
    use url::Url;

    use crate::document::{Document, DocumentRenderOptions};
    use crate::dom::{Dom, NodeId, NodeKind};
    use crate::interaction::{BoundaryPoint, SelectionDirection};
    use crate::layout::{FragmentKind, PhysicalPoint, PhysicalSize, SimpleTextMeasurer};

    use super::{
        ActivationIntent, BoundaryBias, HitTestError, InteractionLimits, InteractionResource,
        InteractionScene, PointerSelection,
    };

    fn find_by_id(dom: &Dom, id: &str) -> NodeId {
        dom_iter(dom)
            .find(|node| dom.attribute(*node, "id").ok().flatten() == Some(id))
            .expect("test element should exist")
    }

    fn first_text(dom: &Dom, parent: NodeId) -> NodeId {
        dom.children(parent)
            .unwrap_or_default()
            .iter()
            .copied()
            .find(|node| {
                matches!(
                    dom.node(*node).map(crate::dom::Node::kind),
                    Some(NodeKind::Text(_))
                )
            })
            .expect("test text should exist")
    }

    fn dom_iter(dom: &Dom) -> impl Iterator<Item = NodeId> + '_ {
        let mut pending = vec![dom.document()];
        std::iter::from_fn(move || {
            let node = pending.pop()?;
            pending.extend(dom.children(node).unwrap_or_default().iter().rev());
            Some(node)
        })
    }

    fn render(
        html: &str,
        viewport: PhysicalSize,
    ) -> (Document, crate::document::DocumentRenderOutput) {
        let document = Document::parse(html);
        let output = document.render_reference(DocumentRenderOptions {
            layout: crate::layout::LayoutOptions {
                viewport,
                ..crate::layout::LayoutOptions::default()
            },
            ..DocumentRenderOptions::default()
        });
        (document, output)
    }

    fn text_fragment_rect(
        output: &crate::document::DocumentRenderOutput,
        source: NodeId,
    ) -> crate::layout::PhysicalRect {
        output
            .layout
            .fragments
            .iter()
            .find_map(|fragment| {
                (fragment.source == Some(source) && matches!(fragment.kind, FragmentKind::Text(_)))
                    .then_some(fragment.rect)
            })
            .expect("text fragment should exist")
    }

    fn point_in(rect: crate::layout::PhysicalRect, fraction: f32) -> PhysicalPoint {
        PhysicalPoint {
            x: rect.origin.x + rect.size.width * fraction,
            y: rect.origin.y + rect.size.height / 2.0,
        }
    }

    #[test]
    fn deepest_text_hit_resolves_nested_relative_link() {
        let (document, output) = render(
            "<!doctype html><body><a id='link' href='../next?q=1'><span id='inner'>go</span></a></body>",
            PhysicalSize {
                width: 320.0,
                height: 100.0,
            },
        );
        let inner = find_by_id(document.dom(), "inner");
        let text = first_text(document.dom(), inner);
        let rect = text_fragment_rect(&output, text);
        let scene = InteractionScene::new(
            document.dom(),
            &output.layout.fragments,
            &SimpleTextMeasurer,
            PhysicalPoint::default(),
            InteractionLimits::default(),
        )
        .unwrap();
        let hit = scene.hit_test(point_in(rect, 0.25)).unwrap().unwrap();
        assert_eq!(hit.node, text);
        assert!(hit.text_position.is_some());

        let base = Url::parse("https://example.test/dir/page.html").unwrap();
        let intent = scene.activation_intent(hit.node, &base).unwrap().unwrap();
        let ActivationIntent::Navigate(navigation) = intent;
        assert_eq!(navigation.link, find_by_id(document.dom(), "link"));
        assert_eq!(
            navigation.destination,
            Url::parse("https://example.test/next?q=1").unwrap()
        );
    }

    #[test]
    fn cjk_and_surrogate_hits_produce_utf16_boundaries() {
        let (document, output) = render(
            "<!doctype html><body><p id='p'>a界😀b</p></body>",
            PhysicalSize {
                width: 320.0,
                height: 100.0,
            },
        );
        let text = first_text(document.dom(), find_by_id(document.dom(), "p"));
        let scene = InteractionScene::new(
            document.dom(),
            &output.layout.fragments,
            &SimpleTextMeasurer,
            PhysicalPoint::default(),
            InteractionLimits::default(),
        )
        .unwrap();
        let order = scene.paint_order().unwrap();
        let maps = scene.text_maps(&order).unwrap();
        let map = maps.iter().find(|map| map.node == text).unwrap();
        let emoji = map
            .characters
            .iter()
            .find(|character| character.dom_start == 2)
            .unwrap();
        let before_emoji_x: f32 = map
            .characters
            .iter()
            .take_while(|character| character.dom_start < 2)
            .map(|character| character.advance)
            .sum();
        assert_eq!(
            map.caret_at(map.rect.origin.x + before_emoji_x + emoji.advance * 0.25),
            BoundaryPoint::new(text, 2)
        );
        assert_eq!(
            map.caret_at(map.rect.origin.x + before_emoji_x + emoji.advance * 0.75),
            BoundaryPoint::new(text, 4)
        );
        assert_eq!(
            scene
                .clamp_text_boundary(BoundaryPoint::new(text, 3), BoundaryBias::Backward)
                .unwrap(),
            BoundaryPoint::new(text, 2)
        );
        assert_eq!(
            scene
                .clamp_text_boundary(BoundaryPoint::new(text, 3), BoundaryBias::Forward)
                .unwrap(),
            BoundaryPoint::new(text, 4)
        );
        let cjk = scene.word_range_at(BoundaryPoint::new(text, 1)).unwrap();
        assert_eq!(cjk.start(), BoundaryPoint::new(text, 1));
        assert_eq!(cjk.end(), BoundaryPoint::new(text, 2));
    }

    #[test]
    fn pointer_drag_selects_across_nodes_and_preserves_direction() {
        let (document, output) = render(
            "<!doctype html><body><p><span id='a'>ab</span><span id='b'>界😀</span></p></body>",
            PhysicalSize {
                width: 320.0,
                height: 100.0,
            },
        );
        let first = first_text(document.dom(), find_by_id(document.dom(), "a"));
        let second = first_text(document.dom(), find_by_id(document.dom(), "b"));
        let first_rect = text_fragment_rect(&output, first);
        let second_rect = text_fragment_rect(&output, second);
        let scene = InteractionScene::new(
            document.dom(),
            &output.layout.fragments,
            &SimpleTextMeasurer,
            PhysicalPoint::default(),
            InteractionLimits::default(),
        )
        .unwrap();
        let mut pointer = PointerSelection::default();
        pointer
            .pointer_down(&scene, point_in(first_rect, 0.0))
            .unwrap();
        pointer
            .pointer_drag(&scene, point_in(second_rect, 1.0))
            .unwrap();
        pointer
            .pointer_up(
                &scene,
                point_in(second_rect, 1.0),
                &Url::parse("https://example.test/").unwrap(),
            )
            .unwrap();
        assert_eq!(
            pointer.selection().anchor(),
            Some(BoundaryPoint::new(first, 0))
        );
        assert_eq!(
            pointer.selection().focus(),
            Some(BoundaryPoint::new(second, 3))
        );
        assert_eq!(pointer.selection().direction(), SelectionDirection::Forward);
        let geometry = scene.selection_geometry(pointer.selection()).unwrap();
        assert_eq!(geometry.rects.len(), 2);
        assert_eq!(geometry.rects[0].node, first);
        assert_eq!(geometry.rects[1].node, second);

        let mut backward = PointerSelection::default();
        backward
            .pointer_down(&scene, point_in(second_rect, 1.0))
            .unwrap();
        backward
            .pointer_drag(&scene, point_in(first_rect, 0.0))
            .unwrap();
        assert_eq!(
            backward.selection().direction(),
            SelectionDirection::Backward
        );
    }

    #[test]
    fn scroll_coordinates_hit_document_fragments_and_translate_geometry() {
        let (document, output) = render(
            "<!doctype html><body><p>first</p><p id='target'>target</p><p>last</p></body>",
            PhysicalSize {
                width: 240.0,
                height: 24.0,
            },
        );
        let text = first_text(document.dom(), find_by_id(document.dom(), "target"));
        let rect = text_fragment_rect(&output, text);
        let scene = InteractionScene::new(
            document.dom(),
            &output.layout.fragments,
            &SimpleTextMeasurer,
            PhysicalPoint {
                x: 0.0,
                y: rect.origin.y,
            },
            InteractionLimits::default(),
        )
        .unwrap();
        let viewport_point = PhysicalPoint {
            x: rect.origin.x + 1.0,
            y: rect.origin.y - scene.scroll_offset().y + rect.size.height / 2.0,
        };
        let hit = scene.hit_test(viewport_point).unwrap().unwrap();
        assert_eq!(hit.node, text);
        assert_eq!(
            hit.document_point.y,
            viewport_point.y + scene.scroll_offset().y
        );

        let mut pointer = PointerSelection::default();
        pointer.pointer_down(&scene, viewport_point).unwrap();
        pointer
            .pointer_drag(
                &scene,
                PhysicalPoint {
                    x: rect.right(),
                    y: viewport_point.y,
                },
            )
            .unwrap();
        let geometry = scene.selection_geometry(pointer.selection()).unwrap();
        assert_eq!(
            geometry.rects[0].viewport_rect.origin.y,
            geometry.rects[0].document_rect.origin.y - scene.scroll_offset().y
        );
    }

    #[test]
    fn non_link_has_no_activation_and_stale_layout_is_rejected() {
        let (mut document, output) = render(
            "<!doctype html><body><span id='plain'>plain</span></body>",
            PhysicalSize {
                width: 240.0,
                height: 100.0,
            },
        );
        let plain = find_by_id(document.dom(), "plain");
        let text = first_text(document.dom(), plain);
        {
            let scene = InteractionScene::new(
                document.dom(),
                &output.layout.fragments,
                &SimpleTextMeasurer,
                PhysicalPoint::default(),
                InteractionLimits::default(),
            )
            .unwrap();
            assert!(
                scene
                    .activation_intent(text, &Url::parse("https://example.test/").unwrap())
                    .unwrap()
                    .is_none()
            );
        }
        document
            .dom_mut()
            .set_attribute(plain, "class", "new")
            .unwrap();
        assert!(matches!(
            InteractionScene::new(
                document.dom(),
                &output.layout.fragments,
                &SimpleTextMeasurer,
                PhysicalPoint::default(),
                InteractionLimits::default(),
            ),
            Err(HitTestError::StaleFragmentTree { .. })
        ));
    }

    #[test]
    fn interaction_work_is_resource_bounded() {
        let (document, output) = render(
            "<!doctype html><body><a id='link' href='/'>bounded</a></body>",
            PhysicalSize {
                width: 240.0,
                height: 100.0,
            },
        );
        let text = first_text(document.dom(), find_by_id(document.dom(), "link"));
        let rect = text_fragment_rect(&output, text);
        let scene = InteractionScene::new(
            document.dom(),
            &output.layout.fragments,
            &SimpleTextMeasurer,
            PhysicalPoint::default(),
            InteractionLimits {
                max_fragments: 0,
                ..InteractionLimits::default()
            },
        )
        .unwrap();
        assert!(matches!(
            scene.hit_test(point_in(rect, 0.5)),
            Err(HitTestError::ResourceLimit {
                resource: InteractionResource::Fragments,
                limit: 0
            })
        ));

        let scene = InteractionScene::new(
            document.dom(),
            &output.layout.fragments,
            &SimpleTextMeasurer,
            PhysicalPoint::default(),
            InteractionLimits {
                max_ancestor_depth: 0,
                ..InteractionLimits::default()
            },
        )
        .unwrap();
        assert!(matches!(
            scene.activation_intent(text, &Url::parse("https://example.test/").unwrap()),
            Err(HitTestError::ResourceLimit {
                resource: InteractionResource::AncestorDepth,
                limit: 0
            })
        ));
    }
}
