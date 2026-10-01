//! Dump a rENDER `Dom` in the html5lib tree format, and diff two such dumps.
//!
//! html5lib's expected trees are written as one node per line, indented by two
//! spaces per ancestor. The dumper reproduces that format, including the parts
//! the format defines that a DOM traversal would not produce on its own:
//! attributes sorted by name, the `content` pseudo-node for a template's
//! contents, and the namespace designator on element and attribute names.

use render_dom::{Dom, Namespace, NodeId, NodeKind};

const XLINK_NAMESPACE: &str = "http://www.w3.org/1999/xlink";
const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";

/// The tree dumped from a document, one line per node.
#[derive(Clone, Debug, Default)]
pub struct TreeDump {
    pub lines: Vec<String>,
}

/// Dump the whole document: the children of the document node, at depth zero.
#[must_use]
pub fn dump_document(dom: &Dom) -> TreeDump {
    let mut dump = TreeDump::default();
    for child in dom.children(dom.document()).unwrap_or_default() {
        dump_node(dom, *child, 0, &mut dump.lines);
    }
    dump
}

/// Dump a `DocumentFragment`: its children, at depth zero.
///
/// The format indents a node "two spaces per parent node that the node has
/// before the root document node", and the root of a fragment parse is the
/// `DocumentFragment` rather than a document, so a fragment case's expected tree
/// is that fragment's children at depth zero. `dump_node` already flattens a
/// `DocumentFragment` at its own depth, so this is the same walk rooted one node
/// lower.
#[must_use]
pub fn dump_fragment(dom: &Dom, fragment: NodeId) -> TreeDump {
    let mut dump = TreeDump::default();
    for child in dom.children(fragment).unwrap_or_default() {
        dump_node(dom, *child, 0, &mut dump.lines);
    }
    dump
}

/// The line prefix: `| ` followed by two spaces per ancestor, as the format
/// requires. The root is `| ` with nothing after it.
fn pad(depth: usize) -> String {
    " ".repeat(1 + depth * 2)
}

/// The element tag name string: the local name prefixed by a namespace
/// designator, which is empty for the HTML namespace.
fn tag_name(data: &render_dom::ElementData) -> String {
    let designator = match &data.namespace {
        Namespace::Html => "",
        Namespace::Svg => "svg ",
        Namespace::MathMl => "math ",
        Namespace::Other(other) => {
            return format!("{other} {}", data.local_name);
        }
    };
    format!("{designator}{}", data.local_name)
}

/// The attribute name string: the local name prefixed by a namespace
/// designator, which is empty for no namespace.
fn attribute_name(attribute: &render_dom::Attribute) -> String {
    let designator = match attribute.namespace.as_deref() {
        None => "",
        Some(XLINK_NAMESPACE) => "xlink ",
        Some(XML_NAMESPACE) => "xml ",
        Some(XMLNS_NAMESPACE) => "xmlns ",
        Some(other) => return format!("{other} {}", attribute.local_name),
    };
    format!("{designator}{}", attribute.local_name)
}

fn dump_node(dom: &Dom, node: NodeId, depth: usize, out: &mut Vec<String>) {
    let Some(kind) = dom.node(node).map(render_dom::Node::kind) else {
        return;
    };
    match kind {
        NodeKind::Element(data) => {
            out.push(format!("|{}<{}>", pad(depth), tag_name(data)));
            // The suite requires attributes sorted lexicographically by
            // attribute name string. The engine stores them in source order,
            // which is the order the HTML serialiser needs, so the sort happens
            // here rather than in the engine.
            let mut attributes = data
                .attributes
                .iter()
                .map(|attribute| {
                    let mut line = format!("|{}{}=", pad(depth + 1), attribute_name(attribute));
                    line.push('"');
                    line.push_str(&attribute.value);
                    line.push('"');
                    line
                })
                .collect::<Vec<_>>();
            attributes.sort();
            out.extend(attributes);
            if let Some(contents) = dom.template_contents(node) {
                // A template's contents are a separate fragment, not children
                // of the element, and the format represents them under a
                // `content` pseudo-node.
                out.push(format!("|{}content", pad(depth + 1)));
                for child in dom.children(contents).unwrap_or_default() {
                    dump_node(dom, *child, depth + 2, out);
                }
            }
            for child in dom.children(node).unwrap_or_default() {
                dump_node(dom, *child, depth + 1, out);
            }
        }
        NodeKind::Text(data) => out.push(format!("|{}\"{data}\"", pad(depth))),
        NodeKind::Comment(data) => out.push(format!("|{}<!-- {data} -->", pad(depth))),
        NodeKind::DocumentType(data) => {
            let mut line = format!("|{}<!DOCTYPE {}", pad(depth), data.name);
            if !data.public_id.is_empty() || !data.system_id.is_empty() {
                line.push_str(&format!(" \"{}\" \"{}\"", data.public_id, data.system_id));
            }
            line.push('>');
            out.push(line);
        }
        NodeKind::ProcessingInstruction { target, data } => {
            out.push(format!("|{}<?{target} {data}>", pad(depth)));
        }
        NodeKind::Document | NodeKind::DocumentFragment => {
            for child in dom.children(node).unwrap_or_default() {
                dump_node(dom, *child, depth, out);
            }
        }
    }
}

/// One step of the difference between an expected tree and an actual tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffEvent {
    /// The expected tree has a line the actual tree does not have here.
    Missing { at: usize, expected: String },
    /// The actual tree has a line the expected tree does not have here.
    Unexpected { at: usize, actual: String },
    /// The two trees have different lines at the same position.
    Substituted {
        at: usize,
        expected: String,
        actual: String,
    },
}

impl DiffEvent {
    /// The line index this event is about, which the report prints so a reader
    /// can look at the same line in both trees.
    #[must_use]
    pub fn position(&self) -> usize {
        match self {
            Self::Missing { at, .. }
            | Self::Unexpected { at, .. }
            | Self::Substituted { at, .. } => *at,
        }
    }
}

/// The full difference between two trees.
#[derive(Clone, Debug, Default)]
pub struct TreeDiff {
    pub events: Vec<DiffEvent>,
}

impl TreeDiff {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    #[must_use]
    pub fn first(&self) -> Option<&DiffEvent> {
        self.events.first()
    }
}

/// The number of lines to search ahead when resynchronising the two walks.
const RESYNC_WINDOW: usize = 4;

/// Compare an expected tree dump with an actual one.
///
/// The walk is index-wise with a bounded look-ahead resynchronisation, which is
/// enough to tell an inserted node from a renamed one without pulling in a
/// general diff algorithm. `max_events` bounds the work for a badly wrong tree.
#[must_use]
pub fn diff(expected: &[String], actual: &[String], max_events: usize) -> TreeDiff {
    let mut events = Vec::new();
    let mut left = 0usize;
    let mut right = 0usize;
    while left < expected.len() || right < actual.len() {
        if events.len() >= max_events {
            break;
        }
        match (expected.get(left), actual.get(right)) {
            (Some(want), Some(got)) if want == got => {
                left += 1;
                right += 1;
            }
            (Some(_), Some(_)) => {
                if let Some(shift) = resync(expected, actual, left, right) {
                    if shift.delete > 0 {
                        events.push(DiffEvent::Missing {
                            at: left,
                            expected: expected[left].clone(),
                        });
                        left += 1;
                    } else {
                        events.push(DiffEvent::Unexpected {
                            at: right,
                            actual: actual[right].clone(),
                        });
                        right += 1;
                    }
                    if shift.insert > 0 {
                        events.push(DiffEvent::Unexpected {
                            at: right,
                            actual: actual[right].clone(),
                        });
                        right += 1;
                    }
                } else {
                    events.push(DiffEvent::Substituted {
                        at: left,
                        expected: expected[left].clone(),
                        actual: actual[right].clone(),
                    });
                    left += 1;
                    right += 1;
                }
            }
            (Some(_), None) => {
                events.push(DiffEvent::Missing {
                    at: left,
                    expected: expected[left].clone(),
                });
                left += 1;
            }
            (None, Some(_)) => {
                events.push(DiffEvent::Unexpected {
                    at: right,
                    actual: actual[right].clone(),
                });
                right += 1;
            }
            (None, None) => break,
        }
    }
    TreeDiff { events }
}

struct Shift {
    delete: usize,
    insert: usize,
}

/// Find the smallest pair of skips within `RESYNC_WINDOW` that makes the two
/// walks line up again.
fn resync(expected: &[String], actual: &[String], left: usize, right: usize) -> Option<Shift> {
    for total in 1..=RESYNC_WINDOW {
        for delete in 0..=total {
            let insert = total - delete;
            let mut probe_left = left + delete;
            let mut probe_right = right + insert;
            let mut matched = 0usize;
            while probe_left < expected.len()
                && probe_right < actual.len()
                && expected[probe_left] == actual[probe_right]
            {
                probe_left += 1;
                probe_right += 1;
                matched += 1;
            }
            if matched > 0 {
                return Some(Shift { delete, insert });
            }
        }
    }
    None
}
