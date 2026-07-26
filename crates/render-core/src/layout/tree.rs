//! DOM-to-formatting-tree construction.

use std::collections::BTreeMap;

use crate::css::computed::ComputedStyle;
use crate::css::properties::{
    Display, DisplayBox, DisplayInside, DisplayOutside, TypedPropertyValue,
};
use crate::dom::{Dom, DomRevision, NodeId, NodeKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FormattingNodeId(u32);

impl FormattingNodeId {
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    fn from_index(index: usize) -> Self {
        Self(u32::try_from(index).expect("formatting arena exceeded u32 capacity"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormattingContextKind {
    Block,
    Flex,
    Grid,
    Table,
    Ruby,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormattingNodeKind {
    Root,
    BlockContainer { context: FormattingContextKind },
    AnonymousBlock,
    Inline,
    Text(String),
}

impl FormattingNodeKind {
    const fn accepts_inline_children(&self) -> bool {
        matches!(
            self,
            Self::Root | Self::BlockContainer { .. } | Self::AnonymousBlock
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormattingNode {
    pub id: FormattingNodeId,
    pub source: Option<NodeId>,
    /// Element whose computed style applies to this box or text. Anonymous
    /// boxes and text nodes therefore remain styleable without copying styles.
    pub style_source: Option<NodeId>,
    pub kind: FormattingNodeKind,
    pub children: Vec<FormattingNodeId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormattingLimits {
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_text_bytes: usize,
}

impl Default for FormattingLimits {
    fn default() -> Self {
        Self {
            max_nodes: 1_000_000,
            max_depth: 4_096,
            max_text_bytes: 64 * 1_024 * 1_024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormattingDiagnosticCode {
    NodeLimit,
    DepthLimit,
    TextLimit,
    MissingComputedStyle,
    BlockInsideInline,
    RunInNotImplemented,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormattingDiagnostic {
    pub node: Option<NodeId>,
    pub code: FormattingDiagnosticCode,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormattingWorkUnit {
    pub root: FormattingNodeId,
    pub context: FormattingContextKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormattingTree {
    pub dom_revision: DomRevision,
    root: FormattingNodeId,
    nodes: Vec<FormattingNode>,
    diagnostics: Vec<FormattingDiagnostic>,
}

impl FormattingTree {
    #[must_use]
    pub const fn root(&self) -> FormattingNodeId {
        self.root
    }

    #[must_use]
    pub fn get(&self, id: FormattingNodeId) -> Option<&FormattingNode> {
        usize::try_from(id.0)
            .ok()
            .and_then(|index| self.nodes.get(index))
    }

    pub fn iter(&self) -> impl Iterator<Item = &FormattingNode> {
        self.nodes.iter()
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[FormattingDiagnostic] {
        &self.diagnostics
    }

    /// Formatting-context roots are explicit immutable work units. A scheduler
    /// may run units in parallel once containing-block and intrinsic-size
    /// dependencies are satisfied.
    #[must_use]
    pub fn work_units(&self) -> Vec<FormattingWorkUnit> {
        self.nodes
            .iter()
            .filter_map(|node| {
                let FormattingNodeKind::BlockContainer { context } = node.kind else {
                    return None;
                };
                Some(FormattingWorkUnit {
                    root: node.id,
                    context,
                })
            })
            .collect()
    }
}

/// Build the immutable CSS formatting structure for one DOM revision.
#[must_use]
pub fn build_formatting_tree(
    dom: &Dom,
    styles: &BTreeMap<NodeId, ComputedStyle>,
    limits: &FormattingLimits,
) -> FormattingTree {
    let mut builder = Builder {
        dom,
        styles,
        limits,
        nodes: Vec::new(),
        diagnostics: Vec::new(),
        text_bytes: 0,
        limit_reported: false,
    };
    let root = builder
        .allocate(None, None, FormattingNodeKind::Root)
        .unwrap_or_else(|| {
            // max_nodes == 0 still needs a stable empty tree root.
            builder.nodes.push(FormattingNode {
                id: FormattingNodeId(0),
                source: None,
                style_source: None,
                kind: FormattingNodeKind::Root,
                children: Vec::new(),
            });
            FormattingNodeId(0)
        });
    builder.append_dom_children(dom.document(), root, None, 0);
    FormattingTree {
        dom_revision: dom.revision(),
        root,
        nodes: builder.nodes,
        diagnostics: builder.diagnostics,
    }
}

struct Builder<'a> {
    dom: &'a Dom,
    styles: &'a BTreeMap<NodeId, ComputedStyle>,
    limits: &'a FormattingLimits,
    nodes: Vec<FormattingNode>,
    diagnostics: Vec<FormattingDiagnostic>,
    text_bytes: usize,
    limit_reported: bool,
}

impl Builder<'_> {
    fn append_dom_children(
        &mut self,
        dom_parent: NodeId,
        format_parent: FormattingNodeId,
        text_style_source: Option<NodeId>,
        depth: usize,
    ) {
        if depth > self.limits.max_depth {
            self.diagnostics.push(FormattingDiagnostic {
                node: Some(dom_parent),
                code: FormattingDiagnosticCode::DepthLimit,
                message: "formatting-tree depth limit exceeded".to_owned(),
            });
            return;
        }
        let children = self.dom.children(dom_parent).unwrap_or_default().to_vec();
        let parent_accepts_inline = self
            .get(format_parent)
            .is_some_and(|node| node.kind.accepts_inline_children());
        let independent_children = self.get(format_parent).is_some_and(|node| {
            matches!(
                node.kind,
                FormattingNodeKind::BlockContainer {
                    context: FormattingContextKind::Flex | FormattingContextKind::Grid
                }
            )
        });
        let mut anonymous = None;
        for child in children {
            if independent_children {
                anonymous = None;
            }
            self.append_dom_node(
                child,
                format_parent,
                text_style_source,
                parent_accepts_inline,
                independent_children,
                &mut anonymous,
                depth,
            );
        }
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn append_dom_node(
        &mut self,
        dom_node: NodeId,
        format_parent: FormattingNodeId,
        text_style_source: Option<NodeId>,
        parent_accepts_inline: bool,
        independent_children: bool,
        anonymous: &mut Option<FormattingNodeId>,
        depth: usize,
    ) {
        let Some(node) = self.dom.node(dom_node) else {
            return;
        };
        match node.kind() {
            NodeKind::Text(text) => {
                if text.is_empty()
                    || (independent_children && text.chars().all(char::is_whitespace))
                {
                    return;
                }
                if self.text_bytes.saturating_add(text.len()) > self.limits.max_text_bytes {
                    self.diagnostics.push(FormattingDiagnostic {
                        node: Some(dom_node),
                        code: FormattingDiagnosticCode::TextLimit,
                        message: "formatting-tree text byte limit exceeded".to_owned(),
                    });
                    return;
                }
                self.text_bytes += text.len();
                let Some(id) = self.allocate(
                    Some(dom_node),
                    text_style_source,
                    FormattingNodeKind::Text(text.clone()),
                ) else {
                    return;
                };
                self.append_with_anonymous_if_needed(
                    format_parent,
                    id,
                    if independent_children {
                        None
                    } else {
                        text_style_source
                    },
                    parent_accepts_inline,
                    anonymous,
                );
            }
            NodeKind::Element(_) => {
                let Some(style) = self.styles.get(&dom_node) else {
                    self.diagnostics.push(FormattingDiagnostic {
                        node: Some(dom_node),
                        code: FormattingDiagnosticCode::MissingComputedStyle,
                        message: "element has no computed style".to_owned(),
                    });
                    return;
                };
                let display = display(style);
                if display == Display::Box(DisplayBox::None) {
                    return;
                }
                if display == Display::Box(DisplayBox::Contents) {
                    let children = self.dom.children(dom_node).unwrap_or_default().to_vec();
                    for child in children {
                        self.append_dom_node(
                            child,
                            format_parent,
                            Some(dom_node),
                            parent_accepts_inline,
                            independent_children,
                            anonymous,
                            depth.saturating_add(1),
                        );
                    }
                    return;
                }

                let (mut kind, inline_level) = self.formatting_kind(dom_node, &display);
                if independent_children && matches!(kind, FormattingNodeKind::Inline) {
                    // Flex and grid items are blockified for layout, while
                    // retaining their computed display value for the cascade.
                    kind = FormattingNodeKind::BlockContainer {
                        context: FormattingContextKind::Block,
                    };
                }
                let Some(id) = self.allocate(Some(dom_node), Some(dom_node), kind) else {
                    return;
                };
                if independent_children {
                    *anonymous = None;
                    self.append_child(format_parent, id);
                } else if parent_accepts_inline && inline_level {
                    self.append_with_anonymous_if_needed(
                        format_parent,
                        id,
                        text_style_source,
                        true,
                        anonymous,
                    );
                } else {
                    if !parent_accepts_inline && !inline_level {
                        self.diagnostics.push(FormattingDiagnostic {
                            node: Some(dom_node),
                            code: FormattingDiagnosticCode::BlockInsideInline,
                            message: "block-in-inline splitting is not implemented yet".to_owned(),
                        });
                    }
                    *anonymous = None;
                    self.append_child(format_parent, id);
                }
                self.append_dom_children(dom_node, id, Some(dom_node), depth.saturating_add(1));
            }
            NodeKind::Document | NodeKind::DocumentFragment => {
                self.append_dom_children(
                    dom_node,
                    format_parent,
                    text_style_source,
                    depth.saturating_add(1),
                );
            }
            NodeKind::DocumentType(_)
            | NodeKind::Comment(_)
            | NodeKind::ProcessingInstruction { .. } => {}
        }
    }

    fn formatting_kind(&mut self, node: NodeId, display: &Display) -> (FormattingNodeKind, bool) {
        match display {
            Display::Normal {
                outside, inside, ..
            } => {
                if *outside == DisplayOutside::RunIn {
                    self.diagnostics.push(FormattingDiagnostic {
                        node: Some(node),
                        code: FormattingDiagnosticCode::RunInNotImplemented,
                        message: "run-in box merging is not implemented yet".to_owned(),
                    });
                }
                let inline = *outside == DisplayOutside::Inline;
                if inline && matches!(inside, DisplayInside::Flow | DisplayInside::FlowRoot) {
                    (FormattingNodeKind::Inline, true)
                } else {
                    (
                        FormattingNodeKind::BlockContainer {
                            context: context_for_inside(*inside),
                        },
                        inline,
                    )
                }
            }
            Display::Internal(_) => (
                FormattingNodeKind::BlockContainer {
                    context: FormattingContextKind::Table,
                },
                false,
            ),
            Display::Box(_) => unreachable!("box display values are handled before allocation"),
        }
    }

    fn append_with_anonymous_if_needed(
        &mut self,
        parent: FormattingNodeId,
        child: FormattingNodeId,
        style_source: Option<NodeId>,
        parent_accepts_inline: bool,
        anonymous: &mut Option<FormattingNodeId>,
    ) {
        if !parent_accepts_inline {
            self.append_child(parent, child);
            return;
        }
        let wrapper = if let Some(wrapper) = *anonymous {
            wrapper
        } else {
            let Some(wrapper) =
                self.allocate(None, style_source, FormattingNodeKind::AnonymousBlock)
            else {
                return;
            };
            self.append_child(parent, wrapper);
            *anonymous = Some(wrapper);
            wrapper
        };
        self.append_child(wrapper, child);
    }

    fn allocate(
        &mut self,
        source: Option<NodeId>,
        style_source: Option<NodeId>,
        kind: FormattingNodeKind,
    ) -> Option<FormattingNodeId> {
        if self.nodes.len() >= self.limits.max_nodes {
            if !self.limit_reported {
                self.limit_reported = true;
                self.diagnostics.push(FormattingDiagnostic {
                    node: source,
                    code: FormattingDiagnosticCode::NodeLimit,
                    message: "formatting-tree node limit exceeded".to_owned(),
                });
            }
            return None;
        }
        let id = FormattingNodeId::from_index(self.nodes.len());
        self.nodes.push(FormattingNode {
            id,
            source,
            style_source,
            kind,
            children: Vec::new(),
        });
        Some(id)
    }

    fn append_child(&mut self, parent: FormattingNodeId, child: FormattingNodeId) {
        if let Some(parent) = self.get_mut(parent) {
            parent.children.push(child);
        }
    }

    fn get(&self, id: FormattingNodeId) -> Option<&FormattingNode> {
        usize::try_from(id.0)
            .ok()
            .and_then(|index| self.nodes.get(index))
    }

    fn get_mut(&mut self, id: FormattingNodeId) -> Option<&mut FormattingNode> {
        usize::try_from(id.0)
            .ok()
            .and_then(|index| self.nodes.get_mut(index))
    }
}

fn display(style: &ComputedStyle) -> Display {
    match style.typed("display") {
        Some(TypedPropertyValue::Display(display)) => display.clone(),
        _ => Display::Normal {
            outside: DisplayOutside::Inline,
            inside: DisplayInside::Flow,
            list_item: false,
        },
    }
}

const fn context_for_inside(inside: DisplayInside) -> FormattingContextKind {
    match inside {
        DisplayInside::Flow | DisplayInside::FlowRoot => FormattingContextKind::Block,
        DisplayInside::Table => FormattingContextKind::Table,
        DisplayInside::Flex => FormattingContextKind::Flex,
        DisplayInside::Grid => FormattingContextKind::Grid,
        DisplayInside::Ruby => FormattingContextKind::Ruby,
    }
}

#[cfg(test)]
mod tests {
    use crate::css::cascade::{CascadeInput, CascadeOrigin};
    use crate::css::computed::{ComputationLimits, PropertyRegistry, compute_document_styles};
    use crate::css::selector::{MatchContext, parse_selector_list, select_all};
    use crate::css::stylesheet::parse_stylesheet;
    use crate::dom::NodeKind;
    use crate::html::parse_document;

    use super::{
        FormattingContextKind, FormattingLimits, FormattingNodeKind, build_formatting_tree,
    };

    fn styles(
        dom: &crate::dom::Dom,
        css: &str,
    ) -> std::collections::BTreeMap<crate::dom::NodeId, crate::css::computed::ComputedStyle> {
        let sheet = parse_stylesheet(css);
        compute_document_styles(
            dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        )
    }

    fn find(dom: &crate::dom::Dom, selector: &str) -> crate::dom::NodeId {
        let selector = parse_selector_list(selector).unwrap();
        select_all(dom, dom.document(), &selector, &MatchContext::default())[0]
    }

    #[test]
    fn block_containers_wrap_consecutive_inline_content_in_anonymous_blocks() {
        let output = parse_document(
            "<!doctype html><html><head></head><body><div id='box'>before<span>inside</span><p>block</p>after</div></body></html>",
        );
        let styles = styles(
            &output.dom,
            "html, body, div, p { display: block } head { display: none } span { display: inline }",
        );
        let tree = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let box_node = find(&output.dom, "#box");
        let formatting_box = tree
            .iter()
            .find(|node| node.source == Some(box_node))
            .unwrap();
        assert_eq!(formatting_box.children.len(), 3);
        assert!(matches!(
            tree.get(formatting_box.children[0]).unwrap().kind,
            FormattingNodeKind::AnonymousBlock
        ));
        assert!(matches!(
            tree.get(formatting_box.children[1]).unwrap().kind,
            FormattingNodeKind::BlockContainer {
                context: FormattingContextKind::Block
            }
        ));
        assert!(matches!(
            tree.get(formatting_box.children[2]).unwrap().kind,
            FormattingNodeKind::AnonymousBlock
        ));
    }

    #[test]
    fn display_none_suppresses_subtrees_and_contents_is_box_transparent() {
        let output = parse_document(
            "<!doctype html><body><div id='hidden'><b></b></div><section id='contents'><em id='kept'>x</em></section></body>",
        );
        let styles = styles(
            &output.dom,
            "body { display:block } #hidden { display:none } #contents { display:contents } em { display:inline }",
        );
        let tree = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let hidden = find(&output.dom, "#hidden");
        let contents = find(&output.dom, "#contents");
        let kept = find(&output.dom, "#kept");
        assert!(!tree.iter().any(|node| node.source == Some(hidden)));
        assert!(!tree.iter().any(|node| node.source == Some(contents)));
        assert!(tree.iter().any(|node| node.source == Some(kept)));
    }

    #[test]
    fn rebuilt_tree_consumes_the_revision_created_by_dynamic_dom_updates() {
        let mut output = parse_document("<!doctype html><body><main id='app'></main></body>");
        let css = "body, main, p { display:block }";
        let before_styles = styles(&output.dom, css);
        let before =
            build_formatting_tree(&output.dom, &before_styles, &FormattingLimits::default());

        let app = find(&output.dom, "#app");
        let paragraph = output.dom.create_element("p");
        let text = output.dom.create_text("added by script");
        output.dom.append_child(paragraph, text).unwrap();
        output.dom.append_child(app, paragraph).unwrap();
        let after_styles = styles(&output.dom, css);
        let after = build_formatting_tree(&output.dom, &after_styles, &FormattingLimits::default());

        assert!(after.dom_revision > before.dom_revision);
        assert!(after.iter().any(|node| {
            node.source == Some(text)
                && matches!(node.kind, FormattingNodeKind::Text(ref value) if value == "added by script")
        }));
        assert!(matches!(
            output.dom.node(text).unwrap().kind(),
            NodeKind::Text(_)
        ));
    }

    #[test]
    fn independent_formatting_contexts_are_exposed_as_work_units() {
        let output = parse_document(
            "<!doctype html><body><div id='flex'></div><div id='grid'></div></body>",
        );
        let styles = styles(
            &output.dom,
            "body { display:block } #flex { display:flex } #grid { display:grid }",
        );
        let tree = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let units = tree.work_units();
        assert!(
            units
                .iter()
                .any(|unit| unit.context == FormattingContextKind::Flex)
        );
        assert!(
            units
                .iter()
                .any(|unit| unit.context == FormattingContextKind::Grid)
        );
    }

    #[test]
    fn flex_children_are_independent_blockified_items_and_whitespace_is_suppressed() {
        let output = parse_document(
            "<!doctype html><body><div id='flex'>\n<span id='a'>A</span> <button id='b'>B</button>\n</div></body>",
        );
        let styles = styles(
            &output.dom,
            "body { display:block } #flex { display:flex } span, button { display:inline }",
        );
        let tree = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let flex = find(&output.dom, "#flex");
        let flex = tree.iter().find(|node| node.source == Some(flex)).unwrap();
        assert_eq!(flex.children.len(), 2);
        assert!(flex.children.iter().all(|child| matches!(
            tree.get(*child).unwrap().kind,
            FormattingNodeKind::BlockContainer {
                context: FormattingContextKind::Block
            }
        )));
    }

    #[test]
    fn grid_children_are_independent_blockified_items_and_whitespace_is_suppressed() {
        let output = parse_document(
            "<!doctype html><body><div id='grid'>\n<span id='a'>A</span> <button id='b'>B</button>\n</div></body>",
        );
        let styles = styles(
            &output.dom,
            "body { display:block } #grid { display:grid } span, button { display:inline }",
        );
        let tree = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let grid = find(&output.dom, "#grid");
        let grid = tree.iter().find(|node| node.source == Some(grid)).unwrap();
        assert_eq!(grid.children.len(), 2);
        assert!(grid.children.iter().all(|child| matches!(
            tree.get(*child).unwrap().kind,
            FormattingNodeKind::BlockContainer {
                context: FormattingContextKind::Block
            }
        )));
    }
}
