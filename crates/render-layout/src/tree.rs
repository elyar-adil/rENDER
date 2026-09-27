//! DOM-to-formatting-tree construction.

use std::collections::BTreeMap;

use render_css::computed::ComputedStyle;
use render_css::properties::{
    Display, DisplayBox, DisplayInside, DisplayInternal, DisplayOutside, Float, TypedPropertyValue,
};
use render_dom::{Dom, DomRevision, NodeId, NodeKind};

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
    AtomicInline { context: FormattingContextKind },
    AnonymousBlock,
    Inline,
    Text(String),
}

/// The table structure level a display value places a box at (CSS 2.1 §17.2.1).
/// Only a wrapper box and a row constrain their children; a cell, a caption and
/// a column carry ordinary flow content.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TableLevel {
    /// A table wrapper box or a row group: children are row-level boxes.
    Rows,
    /// A table row: children are cells.
    Row,
    /// A table cell.
    Cell,
    /// A table caption, which sits beside the rows of its table.
    Caption,
    /// A column box, which takes part in the table structure but generates no
    /// box of its own.
    Column,
    /// Not part of the table structure.
    Other,
}

fn table_level(display: &Display) -> TableLevel {
    match display {
        // `display: table` and `display: inline-table` are a wrapper box, and a
        // row group sits between the wrapper and the rows.
        Display::Normal {
            outside: DisplayOutside::Block | DisplayOutside::Inline,
            inside: DisplayInside::Table,
            ..
        }
        | Display::Internal(
            DisplayInternal::TableRowGroup
            | DisplayInternal::TableHeaderGroup
            | DisplayInternal::TableFooterGroup,
        ) => TableLevel::Rows,
        Display::Internal(DisplayInternal::TableRow) => TableLevel::Row,
        Display::Internal(DisplayInternal::TableCell) => TableLevel::Cell,
        Display::Internal(DisplayInternal::TableCaption) => TableLevel::Caption,
        Display::Internal(DisplayInternal::TableColumn | DisplayInternal::TableColumnGroup) => {
            TableLevel::Column
        }
        _ => TableLevel::Other,
    }
}

/// An anonymous table row or cell. The solver tells the two apart by the level
/// of the box they sit in, and neither has a source element to look styles up
/// on, so it inherits from the enclosing table structure box.
const fn table_box() -> FormattingNodeKind {
    FormattingNodeKind::BlockContainer {
        context: FormattingContextKind::Block,
    }
}

impl FormattingNodeKind {
    const fn accepts_inline_children(&self) -> bool {
        matches!(
            self,
            Self::Root
                | Self::BlockContainer { .. }
                | Self::AtomicInline { .. }
                | Self::AnonymousBlock
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
    /// Whether this box is its parent's first child. CSS Text 3 §8.1 indents
    /// "only lines that are the first formatted line of an element", and adds
    /// that "the first line of an anonymous block box is only affected if it
    /// is the first child of its parent element", so the solver needs to know
    /// this before it can apply `text-indent`.
    pub is_first_child: bool,
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
                let (FormattingNodeKind::BlockContainer { context }
                | FormattingNodeKind::AtomicInline { context }) = node.kind
                else {
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
        table_parent: None,
        capitalize_word_start: true,
    };
    let root = builder
        .allocate(None, None, FormattingNodeKind::Root)
        .unwrap_or_else(|| {
            // max_nodes == 0 still needs a stable empty tree root.
            builder.nodes.push(FormattingNode {
                id: FormattingNodeId(0),
                source: None,
                style_source: None,
                is_first_child: false,
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
    /// The wrapping level the enclosing table structure box imposes, if the
    /// children currently being appended belong to one.
    table_parent: Option<TableLevel>,
    /// Whether the next letter read for layout begins a word, for
    /// `text-transform: capitalize`.
    capitalize_word_start: bool,
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
        if let Some(table_parent @ (TableLevel::Rows | TableLevel::Row | TableLevel::Column)) =
            self.table_parent.take()
        {
            // A column group holds nothing but column boxes (§17.2.1).
            if table_parent == TableLevel::Column {
                for child in children {
                    if self
                        .styles
                        .get(&child)
                        .is_some_and(|style| table_level(&display(style)) == TableLevel::Column)
                    {
                        self.append_column_box(child, format_parent, depth);
                    }
                }
                return;
            }
            self.append_table_children(
                &children,
                format_parent,
                table_parent,
                text_style_source,
                depth,
            );
            return;
        }
        let parent_accepts_inline = self
            .get(format_parent)
            .is_some_and(|node| node.kind.accepts_inline_children());
        let independent_children = self.get(format_parent).is_some_and(|node| {
            matches!(
                node.kind,
                FormattingNodeKind::BlockContainer {
                    context: FormattingContextKind::Flex | FormattingContextKind::Grid
                } | FormattingNodeKind::AtomicInline {
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

    /// CSS 2.1 §17.2.1: a table structure box only accepts children of the next
    /// level. Anything else is wrapped in the anonymous table boxes that lead to
    /// the required one, and whitespace-only text never generates them.
    fn append_table_children(
        &mut self,
        children: &[NodeId],
        format_parent: FormattingNodeId,
        parent: TableLevel,
        text_style_source: Option<NodeId>,
        depth: usize,
    ) {
        for child in children.iter().copied() {
            let Some(node) = self.dom.node(child) else {
                continue;
            };
            if let NodeKind::Text(text) = &node.kind()
                && (text.is_empty() || text.chars().all(char::is_whitespace))
            {
                continue;
            }
            let Some(style) = self.styles.get(&child) else {
                continue;
            };
            let display = display(style);
            let level = table_level(&display);
            // `display: none` takes no part in the table structure.
            if display == Display::Box(DisplayBox::None) {
                continue;
            }
            if level == TableLevel::Column {
                // §17.2.1: a column box generates no box of its own, but its
                // `width` still sizes the columns it covers (§17.5.1), so it is
                // kept as a child of the wrapper and only ever holds the column
                // boxes of a column group.
                self.append_column_box(child, format_parent, depth);
                continue;
            }
            let required = match parent {
                TableLevel::Rows => matches!(
                    level,
                    TableLevel::Rows | TableLevel::Row | TableLevel::Caption
                ),
                TableLevel::Row => level == TableLevel::Cell,
                // A column group holds nothing but column boxes, and a cell or
                // a caption holds ordinary flow content.
                TableLevel::Cell | TableLevel::Caption | TableLevel::Column | TableLevel::Other => {
                    false
                }
            };
            if required {
                self.append_dom_node(
                    child,
                    format_parent,
                    text_style_source,
                    false,
                    false,
                    &mut None,
                    depth,
                );
                continue;
            }
            let row_parent = match parent {
                TableLevel::Rows => match self.allocate(None, text_style_source, table_box()) {
                    Some(row) => {
                        self.append_child(format_parent, row);
                        row
                    }
                    None => return,
                },
                _ => format_parent,
            };
            let Some(cell) = self.allocate(None, text_style_source, table_box()) else {
                return;
            };
            self.append_child(row_parent, cell);
            self.append_dom_node(
                child,
                cell,
                text_style_source,
                false,
                false,
                &mut None,
                depth,
            );
        }
    }

    /// Keep a column box for its `width` (§17.5.1) without generating a box of
    /// its own, holding only the column boxes of a column group.
    fn append_column_box(&mut self, child: NodeId, format_parent: FormattingNodeId, depth: usize) {
        let Some(id) = self.allocate(Some(child), Some(child), table_box()) else {
            return;
        };
        self.append_child(format_parent, id);
        let enclosing = self.table_parent;
        self.table_parent = Some(TableLevel::Column);
        self.append_dom_children(child, id, Some(child), depth.saturating_add(1));
        self.table_parent = enclosing;
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
                let transform = self.text_transform_of(text_style_source);
                // `text-transform` is applied here and not in the painter
                // because it changes the characters the solver measures; see
                // `transformed_text`.
                let text = transformed_text(text, transform, &mut self.capitalize_word_start);
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
                    FormattingNodeKind::Text(text.into_owned()),
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

                let (mut kind, mut inline_level) = self.formatting_kind(dom_node, &display);
                // A table structure box constrains the level of its children;
                // `append_table_children` applies the CSS 2.1 §17.2.1 anonymous
                // table boxes while they are appended.
                let table_parent = table_level(&display);
                if inline_level && float(style) != Float::None {
                    kind = match kind {
                        FormattingNodeKind::Inline => FormattingNodeKind::BlockContainer {
                            context: FormattingContextKind::Block,
                        },
                        FormattingNodeKind::AtomicInline { context } => {
                            FormattingNodeKind::BlockContainer { context }
                        }
                        other => other,
                    };
                    inline_level = false;
                }
                if independent_children {
                    // Flex and grid items are blockified for layout, while
                    // retaining their inner formatting context and computed
                    // display value for the cascade.
                    kind = match kind {
                        FormattingNodeKind::Inline => FormattingNodeKind::BlockContainer {
                            context: FormattingContextKind::Block,
                        },
                        FormattingNodeKind::AtomicInline { context } => {
                            FormattingNodeKind::BlockContainer { context }
                        }
                        other => other,
                    };
                }
                // A block-level box opens a new line box and a `<br>` a new
                // line, so neither continues the previous `capitalize` word.
                // Without this, the second paragraph inside a container would
                // inherit the first one's open word and stay lowercase.
                let forced_line_start = matches!(
                    node.kind(),
                    NodeKind::Element(element) if element.local_name == "br"
                );
                if !inline_level || forced_line_start {
                    self.capitalize_word_start = true;
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
                let enclosing = self.table_parent;
                self.table_parent = Some(table_parent);
                self.append_dom_children(dom_node, id, Some(dom_node), depth.saturating_add(1));
                self.table_parent = enclosing;
                // An <input> has no DOM children, but it paints its current
                // `value` or, when empty, its `placeholder` as the visible
                // field content (HTML forms rendering).
                if matches!(
                    self.dom.node(dom_node).map(render_dom::Node::kind),
                    Some(NodeKind::Element(element))
                        if element.local_name == "input"
                            && !self
                                .dom
                                .attribute(dom_node, "type")
                                .ok()
                                .flatten()
                                .is_some_and(|value| value.eq_ignore_ascii_case("hidden"))
                ) && let Some(shown_text) = self
                    .dom
                    .attribute(dom_node, "value")
                    .ok()
                    .flatten()
                    .filter(|value| !value.is_empty())
                    .or_else(|| self.dom.attribute(dom_node, "placeholder").ok().flatten())
                {
                    let Some(text_id) = self.allocate(
                        Some(dom_node),
                        Some(dom_node),
                        FormattingNodeKind::Text(shown_text.to_owned()),
                    ) else {
                        return;
                    };
                    self.append_child(id, text_id);
                }
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
        if matches!(
            self.dom.node(node).map(render_dom::Node::kind),
            Some(NodeKind::Element(element))
                if matches!(element.local_name.as_str(), "img" | "video")
        ) {
            let inline = matches!(
                display,
                Display::Normal {
                    outside: DisplayOutside::Inline | DisplayOutside::RunIn,
                    ..
                }
            );
            return (
                FormattingNodeKind::AtomicInline {
                    context: FormattingContextKind::Block,
                },
                inline,
            );
        }
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
                if inline && *inside == DisplayInside::Flow {
                    (FormattingNodeKind::Inline, true)
                } else if inline {
                    (
                        FormattingNodeKind::AtomicInline {
                            context: context_for_inside(*inside),
                        },
                        true,
                    )
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
                // CSS 2.1 §17.2: row groups, rows, cells and captions are laid
                // out by the table solver, which places them itself. They keep
                // the ordinary block context so a table structure box outside a
                // table still lays out as a block.
                FormattingNodeKind::BlockContainer {
                    context: FormattingContextKind::Block,
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
            is_first_child: false,
            kind,
            children: Vec::new(),
        });
        Some(id)
    }

    fn append_child(&mut self, parent: FormattingNodeId, child: FormattingNodeId) {
        let is_first = self
            .get_mut(parent)
            .is_some_and(|parent| parent.children.is_empty());
        if let Some(parent) = self.get_mut(parent) {
            parent.children.push(child);
        }
        if is_first
            && let Some(child) = usize::try_from(child.as_u32())
                .ok()
                .and_then(|index| self.nodes.get_mut(index))
        {
            child.is_first_child = true;
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

    /// The casing transform that applies to text read for `style_source`.
    fn text_transform_of(&self, style_source: Option<NodeId>) -> TextTransform {
        style_source
            .and_then(|source| self.styles.get(&source))
            .and_then(|style| style.get("text-transform"))
            .map_or(TextTransform::None, |value| {
                TextTransform::parse(value.css_text())
            })
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

/// The casing transform a computed `text-transform` asks for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TextTransform {
    None,
    Uppercase,
    Lowercase,
    Capitalize,
}

impl TextTransform {
    /// The grammar allows at most one casing keyword alongside the two
    /// unmapped ones, so an exact match is enough and anything else - which
    /// includes the initial value - is no transform. Compared without
    /// lowercasing because this runs once per text node.
    fn parse(value: &str) -> Self {
        let value = value.trim();
        if value.eq_ignore_ascii_case("uppercase") {
            Self::Uppercase
        } else if value.eq_ignore_ascii_case("lowercase") {
            Self::Lowercase
        } else if value.eq_ignore_ascii_case("capitalize") {
            Self::Capitalize
        } else {
            Self::None
        }
    }
}

/// What counts as a letter for `text-transform: capitalize`.
///
/// CSS Text 3 §2.1.1 leaves the definition of a word to the user agent and
/// suggests UAX29 word segmentation. That data is not available to this
/// crate, so the approximation is Unicode's alphanumeric property, which is
/// what "the first letter of a word" means in the scripts this engine shapes:
/// a letter after a space, a hyphen or punctuation is titlecased, and
/// digits, punctuation and the interior of a word are left alone.
fn is_word_character(character: char) -> bool {
    character.is_alphanumeric()
}

/// Apply `text-transform` to the text a formatting node renders.
///
/// CSS Text 3 §2.1: the property "transforms text for styling purposes. It
/// has no effect on the underlying content, and must not affect the content
/// of a plain text copy & paste operation."
///
/// It is applied here, where the DOM text node is read into the formatting
/// tree, and not in the painter, because the transformed characters change
/// advance widths. `uppercase` on a CJK-free string changes nothing visible,
/// but on a Latin or Cyrillic one it changes where every following character
/// lands, and `capitalize` can change the width of a run outright. A painter
/// that uppercased the string it was handed would draw glyphs the solver
/// never measured, so the wrap points, the intrinsic widths and the painted
/// string would all disagree. Transforming once, at the single point where
/// layout first reads the text, makes them agree by construction - and leaves
/// the DOM text node untouched, so `textContent` still returns what the
/// author wrote.
///
/// `full-width` and `full-size-kana` are not implemented. Both are marked
/// at-risk in the CSS Text 3 CR, `full-size-kana` needs the Appendix G
/// mapping table, and `full-width` needs UAX11 decompositions; a declaration
/// using either renders untransformed.
fn transformed_text<'a>(
    text: &'a str,
    transform: TextTransform,
    capitalize_word_start: &mut bool,
) -> std::borrow::Cow<'a, str> {
    if text.is_empty() {
        return std::borrow::Cow::Borrowed(text);
    }
    match transform {
        TextTransform::None => std::borrow::Cow::Borrowed(text),
        TextTransform::Uppercase => std::borrow::Cow::Owned(text.to_uppercase()),
        TextTransform::Lowercase => std::borrow::Cow::Owned(text.to_lowercase()),
        // §2.1: "Puts the first typographic letter unit of each word, if
        // lowercase, in titlecase; other characters are unaffected." The word
        // state is threaded through the builder because §2.1.1 requires that
        // inline box boundaries introduce no word boundary.
        TextTransform::Capitalize => {
            let mut out = String::with_capacity(text.len());
            let mut at_word_start = *capitalize_word_start;
            for character in text.chars() {
                if at_word_start {
                    out.extend(character.to_uppercase());
                } else {
                    out.push(character);
                }
                at_word_start = !is_word_character(character);
            }
            *capitalize_word_start = at_word_start;
            std::borrow::Cow::Owned(out)
        }
    }
}

fn float(style: &ComputedStyle) -> Float {
    match style.typed("float") {
        Some(TypedPropertyValue::Float(value)) => *value,
        _ => Float::None,
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
    use render_css::cascade::{CascadeInput, CascadeOrigin};
    use render_css::computed::{ComputationLimits, PropertyRegistry, compute_document_styles};
    use render_css::selector::{MatchContext, parse_selector_list, select_all};
    use render_css::stylesheet::parse_stylesheet;
    use render_dom::NodeKind;
    use render_html::parse_document;

    use super::{
        FormattingContextKind, FormattingLimits, FormattingNodeKind, build_formatting_tree,
    };

    fn styles(
        dom: &render_dom::Dom,
        css: &str,
    ) -> std::collections::BTreeMap<render_dom::NodeId, render_css::computed::ComputedStyle> {
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

    fn find(dom: &render_dom::Dom, selector: &str) -> render_dom::NodeId {
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
    fn text_input_value_becomes_visible_formatting_text() {
        let output =
            parse_document("<!doctype html><body><input id=query type=search value='百度'>");
        let styles = styles(
            &output.dom,
            "body { display:block } input { display:inline-block; width:180px }",
        );
        let tree = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let input = find(&output.dom, "#query");

        assert!(tree.iter().any(|node| {
            node.source == Some(input)
                && matches!(&node.kind, FormattingNodeKind::Text(value) if value == "百度")
        }));
    }

    #[test]
    fn video_element_creates_an_atomic_inline_formatting_context_like_img() {
        // HTML-aware replaced elements default to `display: inline`, yet they
        // must still become atomic inline-level boxes (width/height replace
        // the content) rather than character-level inline runs.
        for tag in ["img", "video"] {
            let output = parse_document(&format!("<!doctype html><body><{tag} id=media>"));
            let styles = styles(&output.dom, "body { display:block }");
            let tree = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
            let media = find(&output.dom, "#media");
            assert!(
                tree.iter().any(|node| node.source == Some(media)
                    && matches!(
                        node.kind,
                        FormattingNodeKind::AtomicInline {
                            context: FormattingContextKind::Block
                        }
                    )),
                "{tag} must be an atomic inline"
            );
        }
    }

    #[test]
    fn inline_block_creates_an_atomic_inline_formatting_context() {
        let output = parse_document(
            "<!doctype html><body><span>before</span><a id=tile><b>inside</b></a><span>after</span>",
        );
        let styles = styles(
            &output.dom,
            "body { display:block } span, b { display:inline } #tile { display:inline-block }",
        );
        let tree = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let tile = find(&output.dom, "#tile");
        let tile_box = tree
            .iter()
            .find(|node| node.source == Some(tile))
            .expect("inline-block formatting node");

        assert!(matches!(
            tile_box.kind,
            FormattingNodeKind::AtomicInline {
                context: FormattingContextKind::Block
            }
        ));
        assert!(!tile_box.children.is_empty());
    }

    #[test]
    fn floating_inline_element_is_blockified_before_parent_flow_construction() {
        let output = parse_document(
            "<!doctype html><body><span id=float>navigation</span><span id=after>after</span>",
        );
        let styles = styles(
            &output.dom,
            "body { display:block } span { display:inline } #float { float:left }",
        );
        let tree = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let floated = tree
            .iter()
            .find(|node| node.source == Some(find(&output.dom, "#float")))
            .expect("floated formatting node");

        assert!(matches!(
            floated.kind,
            FormattingNodeKind::BlockContainer {
                context: FormattingContextKind::Block
            }
        ));
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

    #[test]
    fn only_a_table_wrapper_box_establishes_a_table_formatting_context() {
        // CSS 2.1 §17.2: the table solver places row groups, rows and cells
        // itself, so only `display: table` needs the table context.
        let output = parse_document(
            "<!doctype html><body><table id='t'><tbody id='g'><tr id='r'>\
             <td id='a'>a</td></tr></tbody></table></body>",
        );
        let styles = styles(
            &output.dom,
            "body { display:block } table { display:table } tbody { display:table-row-group } \
             tr { display:table-row } td { display:table-cell }",
        );
        let tree = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let context = |selector: &str| {
            let source = find(&output.dom, selector);
            let node = tree
                .iter()
                .find(|node| node.source == Some(source))
                .expect("formatting node");
            match node.kind {
                FormattingNodeKind::BlockContainer { context } => context,
                ref other => panic!("{selector} is {other:?}"),
            }
        };
        assert_eq!(context("#t"), FormattingContextKind::Table);
        assert_eq!(context("#g"), FormattingContextKind::Block);
        assert_eq!(context("#r"), FormattingContextKind::Block);
        assert_eq!(context("#a"), FormattingContextKind::Block);
    }

    #[test]
    fn table_structure_boxes_wrap_children_of_the_wrong_level() {
        // CSS 2.1 §17.2.1: a caption and a row are row-level children of the
        // wrapper, a column box is kept without generating a box of its own, and
        // a row child that is not a cell is wrapped in an anonymous table cell.
        let output = parse_document(
            "<!doctype html><body><table id='t'>\
             <caption id='cap'>c</caption><colgroup><col></colgroup>\
             <tr id='r'><td id='a' style='display:block'>x</td></tr></table></body>",
        );
        let styles = styles(
            &output.dom,
            "body { display:block } table { display:table } caption { display:table-caption } \
             colgroup { display:table-column-group } col { display:table-column } \
             tr { display:table-row } td { display:table-cell }",
        );
        let tree = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let children = |selector: &str| {
            let source = find(&output.dom, selector);
            tree.iter()
                .find(|node| node.source == Some(source))
                .expect("formatting node")
                .children
                .clone()
        };
        // The `colgroup` generates no box of its own and holds only the column
        // boxes of its group, so the wrapper has the caption, the group and the
        // row.
        assert_eq!(children("#t").len(), 3);
        let colgroup = children("#t")
            .into_iter()
            .map(|id| tree.get(id).expect("child node").clone())
            .find(|node| node.source == Some(find(&output.dom, "colgroup")))
            .expect("column group box");
        assert_eq!(colgroup.children.len(), 1, "the group's column box");
        let column = tree.get(colgroup.children[0]).expect("column box");
        assert_eq!(column.source, Some(find(&output.dom, "col")));
        assert!(
            column.children.is_empty(),
            "a column box must not generate descendants"
        );
        let row_children = children("#r");
        assert_eq!(row_children.len(), 1);
        let cell = tree.get(row_children[0]).expect("anonymous cell");
        assert_eq!(cell.source, None, "the wrapper cell is anonymous");
        let block = tree
            .iter()
            .find(|node| node.source == Some(find(&output.dom, "#a")))
            .expect("block formatting node");
        assert_eq!(cell.children, vec![block.id]);
    }
}
