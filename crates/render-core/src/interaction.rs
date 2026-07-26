//! Selection, focus navigation, and activation primitives independent of GUI.
//!
//! The data types in this module expose browser interaction state without
//! dispatching platform events or performing default actions. This keeps GUI,
//! headless, and Agent frontends on the same DOM semantics.

use std::cmp::Ordering;
use std::error::Error;
use std::fmt;

use crate::dom::{Dom, MutationBatch, MutationKind, Namespace, NodeId, NodeKind};

pub mod hit_test;

/// A DOM Range boundary. Character-data offsets are counted in UTF-16 code
/// units, as required by the DOM Standard; other offsets index child nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundaryPoint {
    pub container: NodeId,
    pub offset: usize,
}

impl BoundaryPoint {
    #[must_use]
    pub const fn new(container: NodeId, offset: usize) -> Self {
        Self { container, offset }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InteractionErrorKind {
    UnknownNode,
    InvalidBoundaryContainer,
    OffsetOutsideNode,
    DisconnectedBoundary,
    DifferentTrees,
    MissingSelection,
    NotFocusable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InteractionError {
    kind: InteractionErrorKind,
    message: String,
}

impl InteractionError {
    #[must_use]
    pub const fn kind(&self) -> InteractionErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    fn new(kind: InteractionErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for InteractionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl Error for InteractionError {}

/// A normalized Range whose start is never after its end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DomRange {
    start: BoundaryPoint,
    end: BoundaryPoint,
}

impl DomRange {
    /// Construct and normalize two boundary points in one connected tree.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid containers or offsets, detached nodes, or
    /// points from different trees.
    pub fn new(
        dom: &Dom,
        first: BoundaryPoint,
        second: BoundaryPoint,
    ) -> Result<Self, InteractionError> {
        validate_pair(dom, first, second)?;
        if compare_boundary_points(dom, first, second)? == Ordering::Greater {
            Ok(Self {
                start: second,
                end: first,
            })
        } else {
            Ok(Self {
                start: first,
                end: second,
            })
        }
    }

    #[must_use]
    pub const fn start(&self) -> BoundaryPoint {
        self.start
    }

    #[must_use]
    pub const fn end(&self) -> BoundaryPoint {
        self.end
    }

    #[must_use]
    pub fn collapsed(&self) -> bool {
        self.start.container == self.end.container && self.start.offset == self.end.offset
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelectionDirection {
    #[default]
    Directionless,
    Forward,
    Backward,
}

/// One Selection with explicit anchor/focus direction and a normalized Range.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    anchor: Option<BoundaryPoint>,
    focus: Option<BoundaryPoint>,
    direction: SelectionDirection,
    range: Option<DomRange>,
}

impl Selection {
    #[must_use]
    pub const fn anchor(&self) -> Option<BoundaryPoint> {
        self.anchor
    }

    #[must_use]
    pub const fn focus(&self) -> Option<BoundaryPoint> {
        self.focus
    }

    #[must_use]
    pub const fn direction(&self) -> SelectionDirection {
        self.direction
    }

    #[must_use]
    pub const fn range(&self) -> Option<DomRange> {
        self.range
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.range.is_none()
    }

    pub fn clear(&mut self) {
        self.anchor = None;
        self.focus = None;
        self.range = None;
        self.direction = SelectionDirection::Directionless;
    }

    /// Collapse to one valid point.
    ///
    /// # Errors
    ///
    /// Returns an error when the point is invalid or disconnected.
    pub fn collapse(&mut self, dom: &Dom, point: BoundaryPoint) -> Result<(), InteractionError> {
        validate_boundary(dom, point)?;
        require_connected(dom, point.container)?;
        self.anchor = Some(point);
        self.focus = Some(point);
        self.range = Some(DomRange {
            start: point,
            end: point,
        });
        self.direction = SelectionDirection::Directionless;
        Ok(())
    }

    /// Move focus while preserving the anchor.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing selection or an invalid/new-tree point.
    pub fn extend(&mut self, dom: &Dom, focus: BoundaryPoint) -> Result<(), InteractionError> {
        let anchor = self.anchor.ok_or_else(|| {
            InteractionError::new(
                InteractionErrorKind::MissingSelection,
                "cannot extend an empty selection",
            )
        })?;
        validate_pair(dom, anchor, focus)?;
        let ordering = compare_boundary_points(dom, anchor, focus)?;
        self.focus = Some(focus);
        self.direction = match ordering {
            Ordering::Less => SelectionDirection::Forward,
            Ordering::Greater => SelectionDirection::Backward,
            Ordering::Equal => SelectionDirection::Directionless,
        };
        self.range = Some(DomRange::new(dom, anchor, focus)?);
        Ok(())
    }

    /// Select the contents of `node` using that node's DOM length.
    ///
    /// # Errors
    ///
    /// Returns an error when `node` cannot contain a Range boundary or is not
    /// connected.
    pub fn select_all_children(&mut self, dom: &Dom, node: NodeId) -> Result<(), InteractionError> {
        let length = boundary_length(dom, node)?;
        require_connected(dom, node)?;
        let anchor = BoundaryPoint::new(node, 0);
        let focus = BoundaryPoint::new(node, length);
        self.anchor = Some(anchor);
        self.focus = Some(focus);
        self.range = Some(DomRange {
            start: anchor,
            end: focus,
        });
        self.direction = if length == 0 {
            SelectionDirection::Directionless
        } else {
            SelectionDirection::Forward
        };
        Ok(())
    }

    /// Reconcile boundaries after an already-applied mutation batch.
    ///
    /// Child-list records do not currently contain insertion indices, so a
    /// boundary in the changed container is cleared instead of guessed.
    pub fn apply_mutations(&mut self, dom: &Dom, batch: &MutationBatch) -> SelectionRepair {
        let (Some(mut anchor), Some(mut focus)) = (self.anchor, self.focus) else {
            return SelectionRepair::Unchanged;
        };
        for record in &batch.records {
            if let MutationKind::ChildList { target, .. } = &record.kind
                && (*target == anchor.container || *target == focus.container)
            {
                self.clear();
                return SelectionRepair::Cleared(SelectionClearReason::AmbiguousChildList);
            }
        }
        if !dom.is_connected(anchor.container) || !dom.is_connected(focus.container) {
            self.clear();
            return SelectionRepair::Cleared(SelectionClearReason::DetachedBoundary);
        }

        let original_anchor = anchor;
        let original_focus = focus;
        for record in &batch.records {
            if let MutationKind::CharacterData { target } = record.kind {
                if target == anchor.container {
                    anchor.offset = anchor.offset.min(boundary_length(dom, target).unwrap_or(0));
                }
                if target == focus.container {
                    focus.offset = focus.offset.min(boundary_length(dom, target).unwrap_or(0));
                }
            }
        }
        let Ok(range) = DomRange::new(dom, anchor, focus) else {
            self.clear();
            return SelectionRepair::Cleared(SelectionClearReason::InvalidBoundary);
        };
        self.anchor = Some(anchor);
        self.focus = Some(focus);
        self.range = Some(range);
        if anchor == focus {
            self.direction = SelectionDirection::Directionless;
        }
        if anchor == original_anchor && focus == original_focus {
            SelectionRepair::Unchanged
        } else {
            SelectionRepair::Adjusted
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionClearReason {
    DetachedBoundary,
    AmbiguousChildList,
    InvalidBoundary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionRepair {
    Unchanged,
    Adjusted,
    Cleared(SelectionClearReason),
}

/// Compare two validated boundary points in document order.
///
/// # Errors
///
/// Returns an error for invalid or disconnected/different-tree points.
pub fn compare_boundary_points(
    dom: &Dom,
    first: BoundaryPoint,
    second: BoundaryPoint,
) -> Result<Ordering, InteractionError> {
    validate_pair(dom, first, second)?;
    if first.container == second.container {
        return Ok(first.offset.cmp(&second.offset));
    }
    if is_ancestor(dom, first.container, second.container) {
        let child = child_below(dom, first.container, second.container).ok_or_else(|| {
            InteractionError::new(
                InteractionErrorKind::DifferentTrees,
                "could not resolve descendant branch",
            )
        })?;
        let index = child_index(dom, first.container, child)?;
        return Ok(if first.offset <= index {
            Ordering::Less
        } else {
            Ordering::Greater
        });
    }
    if is_ancestor(dom, second.container, first.container) {
        return compare_boundary_points(dom, second, first).map(Ordering::reverse);
    }
    let first_path = path_from_root(dom, first.container);
    let second_path = path_from_root(dom, second.container);
    let shared = first_path
        .iter()
        .zip(&second_path)
        .take_while(|(left, right)| left == right)
        .count();
    if shared == 0 || shared >= first_path.len() || shared >= second_path.len() {
        return Err(InteractionError::new(
            InteractionErrorKind::DifferentTrees,
            "boundary points do not share a connected root",
        ));
    }
    let parent = first_path[shared - 1];
    let first_index = child_index(dom, parent, first_path[shared])?;
    let second_index = child_index(dom, parent, second_path[shared])?;
    Ok(first_index.cmp(&second_index))
}

fn validate_pair(
    dom: &Dom,
    first: BoundaryPoint,
    second: BoundaryPoint,
) -> Result<(), InteractionError> {
    validate_boundary(dom, first)?;
    validate_boundary(dom, second)?;
    require_connected(dom, first.container)?;
    require_connected(dom, second.container)?;
    if root(dom, first.container) != root(dom, second.container) {
        return Err(InteractionError::new(
            InteractionErrorKind::DifferentTrees,
            "boundary points must be in the same connected tree",
        ));
    }
    Ok(())
}

fn validate_boundary(dom: &Dom, point: BoundaryPoint) -> Result<(), InteractionError> {
    let length = boundary_length(dom, point.container)?;
    if point.offset > length {
        return Err(InteractionError::new(
            InteractionErrorKind::OffsetOutsideNode,
            format!(
                "boundary offset {} exceeds node length {length}",
                point.offset
            ),
        ));
    }
    Ok(())
}

fn boundary_length(dom: &Dom, node: NodeId) -> Result<usize, InteractionError> {
    let node = dom.node(node).ok_or_else(|| {
        InteractionError::new(
            InteractionErrorKind::UnknownNode,
            "boundary node does not exist",
        )
    })?;
    match node.kind() {
        NodeKind::Document | NodeKind::DocumentFragment | NodeKind::Element(_) => {
            Ok(node.children().len())
        }
        NodeKind::Text(data)
        | NodeKind::Comment(data)
        | NodeKind::ProcessingInstruction { data, .. } => Ok(data.encode_utf16().count()),
        NodeKind::DocumentType(_) => Err(InteractionError::new(
            InteractionErrorKind::InvalidBoundaryContainer,
            "DocumentType cannot contain a Range boundary",
        )),
    }
}

fn require_connected(dom: &Dom, node: NodeId) -> Result<(), InteractionError> {
    if dom.is_connected(node) {
        Ok(())
    } else {
        Err(InteractionError::new(
            InteractionErrorKind::DisconnectedBoundary,
            "selection boundaries must be connected",
        ))
    }
}

fn root(dom: &Dom, node: NodeId) -> NodeId {
    let mut candidate = node;
    while let Some(parent) = dom.parent(candidate) {
        candidate = parent;
    }
    candidate
}

fn is_ancestor(dom: &Dom, ancestor: NodeId, node: NodeId) -> bool {
    let mut candidate = dom.parent(node);
    while let Some(current) = candidate {
        if current == ancestor {
            return true;
        }
        candidate = dom.parent(current);
    }
    false
}

fn child_below(dom: &Dom, ancestor: NodeId, descendant: NodeId) -> Option<NodeId> {
    let mut candidate = descendant;
    while let Some(parent) = dom.parent(candidate) {
        if parent == ancestor {
            return Some(candidate);
        }
        candidate = parent;
    }
    None
}

fn child_index(dom: &Dom, parent: NodeId, child: NodeId) -> Result<usize, InteractionError> {
    dom.children(parent)
        .and_then(|children| children.iter().position(|candidate| *candidate == child))
        .ok_or_else(|| {
            InteractionError::new(
                InteractionErrorKind::DifferentTrees,
                "node is not a child of the expected ancestor",
            )
        })
}

fn path_from_root(dom: &Dom, node: NodeId) -> Vec<NodeId> {
    let mut path = vec![node];
    let mut candidate = node;
    while let Some(parent) = dom.parent(candidate) {
        path.push(parent);
        candidate = parent;
    }
    path.reverse();
    path
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusNavigationDirection {
    Forward,
    Backward,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusCause {
    Programmatic,
    Sequential(FocusNavigationDirection),
    RemovedOrDisabled,
    Cleared,
}

/// State transition from which a frontend can synthesize blur/focus events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FocusTransition {
    pub previous: Option<NodeId>,
    pub current: Option<NodeId>,
    pub cause: FocusCause,
}

impl FocusTransition {
    #[must_use]
    pub fn changed(&self) -> bool {
        self.previous != self.current
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FocusManager {
    focused: Option<NodeId>,
}

impl FocusManager {
    #[must_use]
    pub const fn focused(&self) -> Option<NodeId> {
        self.focused
    }

    /// Focus a programmatically focusable element.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown, hidden, disabled, or non-focusable
    /// target.
    pub fn focus(
        &mut self,
        dom: &Dom,
        target: NodeId,
    ) -> Result<FocusTransition, InteractionError> {
        if dom.node(target).is_none() {
            return Err(InteractionError::new(
                InteractionErrorKind::UnknownNode,
                "focus target does not exist",
            ));
        }
        if !is_programmatically_focusable(dom, target) {
            return Err(InteractionError::new(
                InteractionErrorKind::NotFocusable,
                "target is not programmatically focusable",
            ));
        }
        Ok(self.transition_to(Some(target), FocusCause::Programmatic))
    }

    #[must_use]
    pub fn clear(&mut self) -> FocusTransition {
        self.transition_to(None, FocusCause::Cleared)
    }

    /// Advance through the sequential focus order, wrapping at either end.
    #[must_use]
    pub fn advance(&mut self, dom: &Dom, direction: FocusNavigationDirection) -> FocusTransition {
        let order = sequential_focus_order(dom);
        let next = if order.is_empty() {
            None
        } else {
            let current_index = self
                .focused
                .and_then(|focused| order.iter().position(|candidate| *candidate == focused));
            let index = match (direction, current_index) {
                (FocusNavigationDirection::Forward, Some(index)) => (index + 1) % order.len(),
                (FocusNavigationDirection::Backward, Some(0) | None) => order.len() - 1,
                (FocusNavigationDirection::Backward, Some(index)) => index - 1,
                (FocusNavigationDirection::Forward, None) => 0,
            };
            Some(order[index])
        };
        self.transition_to(next, FocusCause::Sequential(direction))
    }

    /// Clear focus if mutations detached, disabled, or hid the focused node.
    #[must_use]
    pub fn apply_mutations(&mut self, dom: &Dom, _batch: &MutationBatch) -> FocusTransition {
        let previous = self.focused;
        if previous.is_some_and(|node| !is_programmatically_focusable(dom, node)) {
            self.focused = None;
        }
        FocusTransition {
            previous,
            current: self.focused,
            cause: FocusCause::RemovedOrDisabled,
        }
    }

    fn transition_to(&mut self, current: Option<NodeId>, cause: FocusCause) -> FocusTransition {
        let previous = self.focused;
        self.focused = current;
        FocusTransition {
            previous,
            current,
            cause,
        }
    }
}

/// Compute positive `tabindex` elements first (ascending, stable DOM order),
/// followed by zero/default-tabindex elements in DOM order.
#[must_use]
pub fn sequential_focus_order(dom: &Dom) -> Vec<NodeId> {
    let mut positive = Vec::new();
    let mut normal = Vec::new();
    let mut pending = vec![dom.document()];
    let mut ordinal = 0usize;
    while let Some(node) = pending.pop() {
        if let Some(children) = dom.children(node) {
            pending.extend(children.iter().rev());
        }
        let Some(tab_index) = sequential_tab_index(dom, node) else {
            ordinal = ordinal.saturating_add(1);
            continue;
        };
        if tab_index > 0 {
            positive.push((tab_index, ordinal, node));
        } else {
            normal.push((ordinal, node));
        }
        ordinal = ordinal.saturating_add(1);
    }
    positive.sort_by_key(|(tab_index, order, _)| (*tab_index, *order));
    positive
        .into_iter()
        .map(|(_, _, node)| node)
        .chain(normal.into_iter().map(|(_, node)| node))
        .collect()
}

fn sequential_tab_index(dom: &Dom, node: NodeId) -> Option<i32> {
    if !is_programmatically_focusable(dom, node) {
        return None;
    }
    match parsed_tab_index(dom, node) {
        Some(value) if value < 0 => None,
        Some(value) => Some(value),
        None if is_natively_focusable(dom, node) => Some(0),
        None => None,
    }
}

fn is_programmatically_focusable(dom: &Dom, node: NodeId) -> bool {
    if !dom.is_connected(node) || is_hidden_by_html(dom, node) || is_disabled_control(dom, node) {
        return false;
    }
    parsed_tab_index(dom, node).is_some() || is_natively_focusable(dom, node)
}

fn is_natively_focusable(dom: &Dom, node: NodeId) -> bool {
    let Some(element) = html_element(dom, node) else {
        return false;
    };
    match element.local_name.as_str() {
        "a" => has_attribute(dom, node, "href"),
        "button" | "select" | "textarea" => true,
        "input" => !attribute_equals_ascii_case(dom, node, "type", "hidden"),
        _ => false,
    }
}

fn is_disabled_control(dom: &Dom, node: NodeId) -> bool {
    let Some(element) = html_element(dom, node) else {
        return false;
    };
    matches!(
        element.local_name.as_str(),
        "button" | "input" | "select" | "textarea"
    ) && has_attribute(dom, node, "disabled")
}

fn is_hidden_by_html(dom: &Dom, node: NodeId) -> bool {
    let mut candidate = Some(node);
    while let Some(current) = candidate {
        if has_attribute(dom, current, "hidden") {
            return true;
        }
        candidate = dom.parent(current);
    }
    false
}

fn parsed_tab_index(dom: &Dom, node: NodeId) -> Option<i32> {
    dom.attribute(node, "tabindex")
        .ok()
        .flatten()
        .map(str::trim)
        .and_then(|value| value.parse::<i32>().ok())
}

fn html_element(dom: &Dom, node: NodeId) -> Option<&crate::dom::ElementData> {
    let NodeKind::Element(element) = dom.node(node)?.kind() else {
        return None;
    };
    (element.namespace == Namespace::Html).then_some(element)
}

fn has_attribute(dom: &Dom, node: NodeId, name: &str) -> bool {
    dom.attribute(node, name)
        .is_ok_and(|attribute| attribute.is_some())
}

fn attribute_equals_ascii_case(dom: &Dom, node: NodeId, name: &str, expected: &str) -> bool {
    dom.attribute(node, name)
        .ok()
        .flatten()
        .is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InteractiveKind {
    Link,
    Button,
    TextInput,
    Checkbox,
    Radio,
    Select,
    Textarea,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonBehavior {
    Submit,
    Reset,
    Button,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefaultActionKind {
    FollowHyperlink { href: String },
    InvokeButton(ButtonBehavior),
    BeginTextEditing,
    ToggleCheckbox,
    SelectRadio { group: Option<String> },
    OpenSelectPicker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultActionStatus {
    NotImplemented,
}

/// Typed activation description. It intentionally does not claim that a
/// default action happened; frontends can inspect `status` explicitly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivationPlan {
    pub element: NodeId,
    pub kind: InteractiveKind,
    pub default_action: DefaultActionKind,
    pub status: DefaultActionStatus,
}

/// Classify a common HTML interactive element and describe its pending default
/// action. Unknown/noninteractive elements return `None`.
#[must_use]
pub fn activation_plan(dom: &Dom, node: NodeId) -> Option<ActivationPlan> {
    let element = html_element(dom, node)?;
    let (kind, default_action) = match element.local_name.as_str() {
        "a" => {
            let href = dom.attribute(node, "href").ok().flatten()?.to_owned();
            (
                InteractiveKind::Link,
                DefaultActionKind::FollowHyperlink { href },
            )
        }
        "button" => (
            InteractiveKind::Button,
            DefaultActionKind::InvokeButton(button_behavior(dom, node)),
        ),
        "input" => input_activation(dom, node)?,
        "select" => (InteractiveKind::Select, DefaultActionKind::OpenSelectPicker),
        "textarea" => (
            InteractiveKind::Textarea,
            DefaultActionKind::BeginTextEditing,
        ),
        _ => return None,
    };
    Some(ActivationPlan {
        element: node,
        kind,
        default_action,
        status: DefaultActionStatus::NotImplemented,
    })
}

fn input_activation(dom: &Dom, node: NodeId) -> Option<(InteractiveKind, DefaultActionKind)> {
    let input_type = dom
        .attribute(node, "type")
        .ok()
        .flatten()
        .unwrap_or("text")
        .to_ascii_lowercase();
    match input_type.as_str() {
        "checkbox" => Some((InteractiveKind::Checkbox, DefaultActionKind::ToggleCheckbox)),
        "radio" => Some((
            InteractiveKind::Radio,
            DefaultActionKind::SelectRadio {
                group: dom
                    .attribute(node, "name")
                    .ok()
                    .flatten()
                    .map(str::to_owned),
            },
        )),
        "button" | "reset" | "submit" => Some((
            InteractiveKind::Button,
            DefaultActionKind::InvokeButton(button_behavior(dom, node)),
        )),
        "text" | "search" | "email" | "password" | "tel" | "url" | "number" => Some((
            InteractiveKind::TextInput,
            DefaultActionKind::BeginTextEditing,
        )),
        _ => None,
    }
}

fn button_behavior(dom: &Dom, node: NodeId) -> ButtonBehavior {
    match dom
        .attribute(node, "type")
        .ok()
        .flatten()
        .unwrap_or("submit")
        .to_ascii_lowercase()
        .as_str()
    {
        "reset" => ButtonBehavior::Reset,
        "button" => ButtonBehavior::Button,
        _ => ButtonBehavior::Submit,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BoundaryPoint, DefaultActionKind, DefaultActionStatus, DomRange, FocusManager,
        FocusNavigationDirection, InteractionErrorKind, InteractiveKind, Selection,
        SelectionDirection, SelectionRepair, activation_plan, sequential_focus_order,
    };
    use crate::dom::{Dom, NodeId, NodeKind};
    use crate::html::parse_document;

    fn by_id(dom: &Dom, id: &str) -> NodeId {
        let mut pending = vec![dom.document()];
        while let Some(node) = pending.pop() {
            if matches!(
                dom.node(node).map(crate::dom::Node::kind),
                Some(NodeKind::Element(_))
            ) && dom.attribute(node, "id").ok().flatten() == Some(id)
            {
                return node;
            }
            pending.extend(dom.children(node).unwrap_or_default().iter().rev());
        }
        panic!("test node #{id} should exist");
    }

    fn first_text(dom: &Dom, parent: NodeId) -> NodeId {
        dom.children(parent)
            .unwrap_or_default()
            .iter()
            .copied()
            .find(|child| {
                matches!(
                    dom.node(*child).map(crate::dom::Node::kind),
                    Some(NodeKind::Text(_))
                )
            })
            .expect("test parent should have text")
    }

    #[test]
    fn range_uses_utf16_offsets_and_normalizes_direction() {
        let mut parsed = parse_document("<!doctype html><p id='p'>a😀b</p>");
        let paragraph = by_id(&parsed.dom, "p");
        let text = first_text(&parsed.dom, paragraph);
        let range = DomRange::new(
            &parsed.dom,
            BoundaryPoint::new(text, 4),
            BoundaryPoint::new(text, 1),
        )
        .expect("four UTF-16 units should be valid and normalize");
        assert_eq!(range.start(), BoundaryPoint::new(text, 1));
        assert_eq!(range.end(), BoundaryPoint::new(text, 4));

        let mut selection = Selection::default();
        selection
            .collapse(&parsed.dom, BoundaryPoint::new(text, 4))
            .expect("connected point should collapse");
        selection
            .extend(&parsed.dom, BoundaryPoint::new(text, 1))
            .expect("earlier focus should produce a backward selection");
        assert_eq!(selection.direction(), SelectionDirection::Backward);

        let revision = parsed.dom.revision();
        parsed
            .dom
            .set_character_data(text, "x")
            .expect("text mutation should succeed");
        let batch = parsed
            .dom
            .mutations_since(revision)
            .expect("mutation should remain available");
        assert_eq!(
            selection.apply_mutations(&parsed.dom, &batch),
            SelectionRepair::Adjusted
        );
        assert_eq!(selection.anchor(), Some(BoundaryPoint::new(text, 1)));
        assert_eq!(selection.focus(), Some(BoundaryPoint::new(text, 1)));
    }

    #[test]
    fn range_rejects_invalid_offsets_and_detached_boundaries() {
        let mut parsed = parse_document("<!doctype html><p id='p'>ok</p>");
        let paragraph = by_id(&parsed.dom, "p");
        let text = first_text(&parsed.dom, paragraph);
        let offset_error = DomRange::new(
            &parsed.dom,
            BoundaryPoint::new(text, 3),
            BoundaryPoint::new(text, 3),
        )
        .expect_err("two UTF-16 units cannot accept offset three");
        assert_eq!(offset_error.kind(), InteractionErrorKind::OffsetOutsideNode);

        let detached = parsed.dom.create_element("span");
        let detached_error = DomRange::new(
            &parsed.dom,
            BoundaryPoint::new(detached, 0),
            BoundaryPoint::new(detached, 0),
        )
        .expect_err("detached boundaries are not selection roots");
        assert_eq!(
            detached_error.kind(),
            InteractionErrorKind::DisconnectedBoundary
        );
    }

    #[test]
    fn ambiguous_child_list_change_clears_selection() {
        let mut parsed = parse_document("<!doctype html><p id='p'>x</p>");
        let paragraph = by_id(&parsed.dom, "p");
        let mut selection = Selection::default();
        selection
            .select_all_children(&parsed.dom, paragraph)
            .expect("element contents should be selectable");
        let revision = parsed.dom.revision();
        let span = parsed.dom.create_element("span");
        parsed
            .dom
            .append_child(paragraph, span)
            .expect("append should succeed");
        let batch = parsed
            .dom
            .mutations_since(revision)
            .expect("mutation should remain available");
        assert!(matches!(
            selection.apply_mutations(&parsed.dom, &batch),
            SelectionRepair::Cleared(_)
        ));
        assert!(selection.is_empty());
    }

    #[test]
    fn sequential_focus_order_obeys_tabindex_and_exclusions() {
        let parsed = parse_document(
            "<!doctype html><body>
                <button id='natural'>N</button>
                <a id='link' href='/'>L</a>
                <div id='two' tabindex='2'></div>
                <input id='one' tabindex='1'>
                <button id='disabled' disabled>D</button>
                <div hidden><input id='hidden'></div>
                <div id='negative' tabindex='-1'></div>
            </body>",
        );
        let order = sequential_focus_order(&parsed.dom);
        assert_eq!(
            order,
            vec![
                by_id(&parsed.dom, "one"),
                by_id(&parsed.dom, "two"),
                by_id(&parsed.dom, "natural"),
                by_id(&parsed.dom, "link"),
            ]
        );

        let mut focus = FocusManager::default();
        let first = focus.advance(&parsed.dom, FocusNavigationDirection::Forward);
        assert_eq!(first.current, Some(by_id(&parsed.dom, "one")));
        focus
            .focus(&parsed.dom, by_id(&parsed.dom, "negative"))
            .expect("negative tabindex remains programmatically focusable");
        let next = focus.advance(&parsed.dom, FocusNavigationDirection::Forward);
        assert_eq!(next.current, Some(by_id(&parsed.dom, "one")));
    }

    #[test]
    fn activation_is_typed_but_default_behavior_is_explicitly_pending() {
        let parsed = parse_document(
            "<!doctype html><body>
                <a id='link' href='/next'>next</a>
                <button id='button' type='reset'>reset</button>
                <input id='text'>
                <input id='check' type='checkbox'>
                <input id='radio' type='radio' name='group'>
                <select id='select'></select>
                <textarea id='editor'></textarea>
            </body>",
        );
        let link =
            activation_plan(&parsed.dom, by_id(&parsed.dom, "link")).expect("link should classify");
        assert_eq!(link.kind, InteractiveKind::Link);
        assert_eq!(
            link.default_action,
            DefaultActionKind::FollowHyperlink {
                href: "/next".to_owned()
            }
        );
        assert_eq!(link.status, DefaultActionStatus::NotImplemented);
        assert_eq!(
            activation_plan(&parsed.dom, by_id(&parsed.dom, "button"))
                .expect("button should classify")
                .kind,
            InteractiveKind::Button
        );
        assert_eq!(
            activation_plan(&parsed.dom, by_id(&parsed.dom, "text"))
                .expect("text input should classify")
                .kind,
            InteractiveKind::TextInput
        );
        assert_eq!(
            activation_plan(&parsed.dom, by_id(&parsed.dom, "check"))
                .expect("checkbox should classify")
                .kind,
            InteractiveKind::Checkbox
        );
        assert_eq!(
            activation_plan(&parsed.dom, by_id(&parsed.dom, "radio"))
                .expect("radio should classify")
                .kind,
            InteractiveKind::Radio
        );
        assert_eq!(
            activation_plan(&parsed.dom, by_id(&parsed.dom, "select"))
                .expect("select should classify")
                .kind,
            InteractiveKind::Select
        );
        assert_eq!(
            activation_plan(&parsed.dom, by_id(&parsed.dom, "editor"))
                .expect("textarea should classify")
                .kind,
            InteractiveKind::Textarea
        );
    }
}
