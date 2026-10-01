//! Read-only queries over a parsed fixture and its layout, used by the
//! acceptance assertions.
//!
//! Every helper here is structural: it asks the DOM and the fragment tree what
//! they contain, and it never asks which site it is looking at.

use std::collections::{BTreeMap, BTreeSet};

use render_core::css::selector::{MatchContext, parse_selector_list, select_all};
use render_core::dom::{Dom, Namespace, NodeId, NodeKind};
use render_core::layout::{FragmentId, FragmentKind, FragmentTree, PhysicalRect};

/// Elements matching `selector`, in document order.
#[must_use]
pub fn select(dom: &Dom, selector: &str) -> Vec<NodeId> {
    let Ok(selectors) = parse_selector_list(selector) else {
        return Vec::new();
    };
    select_all(dom, dom.document(), &selectors, &MatchContext::default())
}

/// The first element matching `selector`, in document order.
#[must_use]
pub fn select_one(dom: &Dom, selector: &str) -> Option<NodeId> {
    select(dom, selector).into_iter().next()
}

/// Every element, in document order.
#[must_use]
pub fn elements(dom: &Dom) -> Vec<NodeId> {
    let mut found = Vec::new();
    walk_elements(dom, dom.document(), &mut found);
    found
}

fn walk_elements(dom: &Dom, node: NodeId, found: &mut Vec<NodeId>) {
    for child in dom.children(node).unwrap_or_default() {
        let Some(current) = dom.node(*child) else {
            continue;
        };
        if matches!(current.kind(), NodeKind::Element(_)) {
            found.push(*child);
        }
        walk_elements(dom, *child, found);
    }
}

/// The HTML local name of `node`, when it is an element in the HTML namespace.
#[must_use]
pub fn tag(dom: &Dom, node: NodeId) -> Option<&str> {
    let NodeKind::Element(element) = dom.node(node)?.kind() else {
        return None;
    };
    (element.namespace == Namespace::Html).then_some(element.local_name.as_str())
}

/// The namespace of `node`, when it is an element.
#[must_use]
pub fn namespace(dom: &Dom, node: NodeId) -> Option<Namespace> {
    match dom.node(node)?.kind() {
        NodeKind::Element(element) => Some(element.namespace.clone()),
        _ => None,
    }
}

/// An attribute value, ignoring any namespace prefix.
#[must_use]
pub fn attribute<'a>(dom: &'a Dom, node: NodeId, name: &str) -> Option<&'a str> {
    dom.attribute(node, name).ok().flatten()
}

/// Space-separated class tokens on `node`.
#[must_use]
pub fn class_tokens(dom: &Dom, node: NodeId) -> Vec<&str> {
    attribute(dom, node, "class")
        .map(|value| value.split_ascii_whitespace().collect())
        .unwrap_or_default()
}

/// Whether the node carries every token in `class`.
#[must_use]
pub fn has_class(dom: &Dom, node: NodeId, class: &str) -> bool {
    class_tokens(dom, node).contains(&class)
}

/// Concatenated text of the whole subtree rooted at `node`.
#[must_use]
pub fn subtree_text(dom: &Dom, node: NodeId) -> String {
    let mut text = String::new();
    let mut pending = vec![node];
    while let Some(current) = pending.pop() {
        let Some(current) = dom.node(current) else {
            continue;
        };
        match current.kind() {
            NodeKind::Text(data) => text.push_str(data),
            _ => pending.extend(current.children().iter().rev().copied()),
        }
    }
    text
}

/// Text with runs of whitespace collapsed and the ends trimmed.
#[must_use]
pub fn normalized_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The document title: the text of the first `title` element inside `head`.
#[must_use]
pub fn document_title(dom: &Dom) -> Option<String> {
    select_one(dom, "head title").map(|node| normalized_text(&subtree_text(dom, node)))
}

/// Whether `node` is `ancestor` or sits inside it, in the DOM.
#[must_use]
pub fn is_descendant(dom: &Dom, node: NodeId, ancestor: NodeId) -> bool {
    let mut current = node;
    let mut guard = 0_usize;
    while let Some(parent) = dom.parent(current) {
        guard += 1;
        if guard > 1_024 {
            return false;
        }
        if parent == ancestor {
            return true;
        }
        current = parent;
    }
    false
}

/// The box a node laid out, with its margin, border, and content rectangles.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LaidOutBox {
    pub margin: PhysicalRect,
    pub border: PhysicalRect,
    pub content: PhysicalRect,
}

/// The box a node laid out, if the reference layout produced one for it.
#[must_use]
pub fn box_of(fragments: &FragmentTree, node: NodeId) -> Option<LaidOutBox> {
    fragments.iter().find_map(|fragment| {
        if fragment.source != Some(node) {
            return None;
        }
        match &fragment.kind {
            FragmentKind::Box(geometry) => Some(LaidOutBox {
                margin: geometry.margin_rect(),
                border: geometry.border_rect(),
                content: geometry.content_rect,
            }),
            FragmentKind::Text(_) => None,
        }
    })
}

/// One laid-out line of text.
#[derive(Clone, Debug, PartialEq)]
pub struct TextLine {
    /// Document-space line box.
    pub rect: PhysicalRect,
    pub text: String,
    pub font_size: f32,
}

/// Every text line the reference layout produced for exactly `node`.
///
/// Note that a text fragment is attributed to the DOM *text node* it came from,
/// not to the element containing it, so this returns lines only when `node` is
/// itself a text node. Use [`subtree_text_node_lines`] to ask about an element.
#[must_use]
pub fn text_lines(fragments: &FragmentTree, node: NodeId) -> Vec<TextLine> {
    fragments
        .iter()
        .filter(|fragment| fragment.source == Some(node))
        .filter_map(|fragment| match &fragment.kind {
            FragmentKind::Text(data) => Some(TextLine {
                rect: fragment.rect,
                text: data.text.clone(),
                font_size: data.font_size,
            }),
            FragmentKind::Box(_) => None,
        })
        .collect()
}

/// Every laid-out line of text that belongs to a text node inside `node`.
///
/// This is how you ask "does this element's own content occupy space": the
/// layout attributes each line to the text node, and an *inline* element gets no
/// fragment of its own at all, so the text nodes are where the answer lives.
#[must_use]
pub fn subtree_text_node_lines(dom: &Dom, fragments: &FragmentTree, node: NodeId) -> Vec<TextLine> {
    text_nodes(dom, node)
        .into_iter()
        .flat_map(|text_node| text_lines(fragments, text_node))
        .collect()
}

/// The text nodes inside the subtree rooted at `node`, in document order.
#[must_use]
pub fn text_nodes(dom: &Dom, node: NodeId) -> Vec<NodeId> {
    let mut found = Vec::new();
    let mut pending = vec![node];
    while let Some(current) = pending.pop() {
        let Some(found_node) = dom.node(current) else {
            continue;
        };
        match found_node.kind() {
            NodeKind::Text(_) => found.push(current),
            _ => pending.extend(found_node.children().iter().rev().copied()),
        }
    }
    found
}

/// Every text line anywhere inside the fragment subtree of `node`.
///
/// A node may own several fragments when it is split across lines, so every one
/// of them is treated as a root of the search.
#[must_use]
pub fn subtree_text_lines(fragments: &FragmentTree, node: NodeId) -> Vec<TextLine> {
    let roots: Vec<FragmentId> = fragments
        .iter()
        .filter(|fragment| fragment.source == Some(node))
        .map(|fragment| fragment.id)
        .collect();
    let mut lines = Vec::new();
    let mut visited = BTreeSet::new();
    let mut pending = roots;
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        let Some(fragment) = fragments.get(id) else {
            continue;
        };
        if let FragmentKind::Text(data) = &fragment.kind {
            lines.push(TextLine {
                rect: fragment.rect,
                text: data.text.clone(),
                font_size: data.font_size,
            });
        }
        pending.extend(fragment.children.iter().copied());
    }
    lines
}

/// One text block of a scroll region: a child element and where its text landed.
#[derive(Clone, Debug, PartialEq)]
pub struct ScrollBlock {
    pub node: NodeId,
    /// Topmost document-space `y` of any line inside the block.
    pub top: f32,
    /// Bottom-most document-space `y` of any line inside the block.
    pub bottom: f32,
    pub text: String,
}

/// The ordered text blocks owned by the direct element children of `node`.
///
/// A child counts as a text block when it owns at least one laid-out line.
#[must_use]
pub fn text_blocks(dom: &Dom, fragments: &FragmentTree, node: NodeId) -> Vec<ScrollBlock> {
    dom.children(node)
        .unwrap_or_default()
        .iter()
        .copied()
        .filter(|child| tag(dom, *child).is_some())
        .filter_map(|child| {
            let lines = subtree_text_lines(fragments, child);
            if lines.is_empty() {
                return None;
            }
            let top = lines
                .iter()
                .map(|line| line.rect.origin.y)
                .fold(f32::INFINITY, f32::min);
            let bottom = lines
                .iter()
                .map(|line| line.rect.origin.y + line.rect.size.height)
                .fold(f32::NEG_INFINITY, f32::max);
            Some(ScrollBlock {
                node: child,
                top,
                bottom,
                text: normalized_text(&subtree_text(dom, child)),
            })
        })
        .collect()
}

/// The element holding a fixture's long feed, plus its ordered text blocks.
///
/// The region is found structurally: inside `main`, the element with the most
/// direct element children that each own their own text. That is a property of
/// the page shape, not of any site's class names.
#[must_use]
pub fn scroll_region(
    dom: &Dom,
    fragments: &FragmentTree,
    minimum_blocks: usize,
) -> Option<(NodeId, Vec<ScrollBlock>)> {
    let roots = {
        let mains = select(dom, "main");
        if mains.is_empty() {
            elements(dom)
        } else {
            mains
        }
    };
    let candidates = elements(dom);
    let mut best: Option<(NodeId, Vec<ScrollBlock>)> = None;
    for candidate in candidates {
        if !roots
            .iter()
            .any(|root| is_descendant(dom, candidate, *root))
        {
            continue;
        }
        let blocks = text_blocks(dom, fragments, candidate);
        if blocks.len() < minimum_blocks {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|(_, current)| blocks.len() > current.len())
        {
            best = Some((candidate, blocks));
        }
    }
    best
}

/// A text line together with the DOM position of the node that produced it.
#[derive(Clone, Debug)]
pub struct OrderedLine {
    /// Position of the producing node among all elements, in document order.
    pub dom_rank: usize,
    pub line: TextLine,
}

/// Every text line in the tree, ordered the way a reader sees it: top to
/// bottom, then left to right.
///
/// A fragment is attributed to the DOM node it came from, and a text fragment's
/// source is a **text node**, not the element containing it. So the document
/// rank of a line is the rank of its own text node, and the ranks come from
/// [`document_order_ranks`] over the whole tree rather than from
/// [`elements`]: ranking elements only would give every text node rank zero and
/// make the ordering assertion vacuous.
#[must_use]
pub fn lines_in_reading_order(dom: &Dom, fragments: &FragmentTree) -> Vec<OrderedLine> {
    let ranks = document_order_ranks(dom);
    let mut lines: Vec<OrderedLine> = fragments
        .iter()
        .filter_map(|fragment| {
            let source = fragment.source?;
            let FragmentKind::Text(data) = &fragment.kind else {
                return None;
            };
            Some(OrderedLine {
                dom_rank: *ranks.get(&source)?,
                line: TextLine {
                    rect: fragment.rect,
                    text: data.text.clone(),
                    font_size: data.font_size,
                },
            })
        })
        .collect();
    lines.sort_by(|left, right| {
        left.line
            .rect
            .origin
            .y
            .total_cmp(&right.line.rect.origin.y)
            .then(left.line.rect.origin.x.total_cmp(&right.line.rect.origin.x))
            .then(left.dom_rank.cmp(&right.dom_rank))
    });
    lines
}

/// Position of every node in the document - elements *and* text nodes - in
/// tree order.
///
/// Element-only ranking is the obvious mistake and a silent one: a text
/// fragment's source is a text node, so ranking elements alone leaves every
/// line's rank undefined, the ordering check sees no lines, and it passes
/// vacuously. The [`crate::contract`] counting check is what catches it, and
/// this function is the one place the ranking is built.
#[must_use]
pub fn document_order_ranks(dom: &Dom) -> BTreeMap<NodeId, usize> {
    let mut found = Vec::new();
    collect_tree_nodes(dom, dom.document(), &mut found);
    found
        .into_iter()
        .enumerate()
        .map(|(index, node)| (node, index))
        .collect()
}

fn collect_tree_nodes(dom: &Dom, node: NodeId, found: &mut Vec<NodeId>) {
    for child in dom.children(node).unwrap_or_default() {
        // A child id the DOM cannot resolve is a hole in the arena, not a
        // reason to abandon the rest of the subtree, so the walk records what it
        // can and carries on.
        let _ = dom.node(*child);
        found.push(*child);
        collect_tree_nodes(dom, *child, found);
    }
}

/// The document-space bottom edge of everything the layout produced.
#[must_use]
pub fn document_bottom(fragments: &FragmentTree) -> f32 {
    fragments
        .iter()
        .map(|fragment| fragment.rect.origin.y + fragment.rect.size.height)
        .fold(0.0_f32, f32::max)
}

/// The document-space right edge of everything the layout produced.
#[must_use]
pub fn document_right(fragments: &FragmentTree) -> f32 {
    fragments
        .iter()
        .map(|fragment| fragment.rect.origin.x + fragment.rect.size.width)
        .fold(0.0_f32, f32::max)
}
