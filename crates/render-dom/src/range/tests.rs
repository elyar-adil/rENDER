use crate::{BoundaryPoint, Dom, DomErrorKind, NodeId, NodeKind, RangeId};

fn point(node: NodeId, offset: usize) -> BoundaryPoint {
    BoundaryPoint { node, offset }
}

/// An element with one element child per name, in order.
fn parent_with_children(dom: &mut Dom, names: &[&str]) -> (NodeId, Vec<NodeId>) {
    let parent = dom.create_element("div");
    let mut children = Vec::new();
    for name in names {
        let child = dom.create_element(*name);
        dom.append_child(parent, child).expect("append");
        children.push(child);
    }
    (parent, children)
}

fn text_of(dom: &Dom, node: NodeId) -> String {
    match dom.node(node).map(crate::Node::kind) {
        Some(NodeKind::Text(data)) => data.clone(),
        other => panic!("expected a Text node, found {other:?}"),
    }
}

fn range_between(dom: &mut Dom, start: BoundaryPoint, end: BoundaryPoint) -> RangeId {
    let range = dom.create_live_range(start.node).expect("range");
    dom.range_set_start(range, start.node, start.offset)
        .expect("start");
    dom.range_set_end(range, end.node, end.offset).expect("end");
    range
}

#[test]
fn insert_moves_points_past_the_insertion_point() {
    let mut dom = Dom::new();
    let (parent, kids) = parent_with_children(&mut dom, &["a", "b", "c"]);
    let range = range_between(&mut dom, point(parent, 1), point(parent, 3));
    let added = dom.create_element("x");
    dom.insert_before(parent, added, Some(kids[0]))
        .expect("insert");
    assert_eq!(dom.range_start(range).unwrap(), point(parent, 2));
    assert_eq!(dom.range_end(range).unwrap(), point(parent, 4));
}

#[test]
fn remove_moves_points_inside_the_removed_node_to_its_place() {
    let mut dom = Dom::new();
    let (parent, kids) = parent_with_children(&mut dom, &["a", "b", "c"]);
    let text = dom.create_text("hi");
    dom.append_child(kids[1], text).expect("append");
    let range = range_between(&mut dom, point(text, 1), point(parent, 3));
    dom.remove_child(parent, kids[1]).expect("remove");
    assert_eq!(dom.range_start(range).unwrap(), point(parent, 1));
    assert_eq!(dom.range_end(range).unwrap(), point(parent, 2));
}

#[test]
fn replace_data_clamps_points_into_the_replaced_span() {
    let mut dom = Dom::new();
    let text = dom.create_text("abcdef");
    let range = range_between(&mut dom, point(text, 2), point(text, 5));
    dom.replace_data(text, 1, 3, "XY").expect("replace");
    assert_eq!(text_of(&dom, text), "aXYef");
    assert_eq!(dom.range_start(range).unwrap(), point(text, 1));
    assert_eq!(dom.range_end(range).unwrap(), point(text, 4));
}

#[test]
fn split_text_moves_points_after_the_split_into_the_new_node() {
    let mut dom = Dom::new();
    let (parent, _) = parent_with_children(&mut dom, &[]);
    let text = dom.create_text("abcd");
    dom.append_child(parent, text).expect("append");
    let range = range_between(&mut dom, point(text, 3), point(parent, 1));
    let rest = dom.split_text(text, 2).expect("split");
    assert_eq!(text_of(&dom, text), "ab");
    assert_eq!(text_of(&dom, rest), "cd");
    assert_eq!(dom.range_start(range).unwrap(), point(rest, 1));
    assert_eq!(dom.range_end(range).unwrap(), point(parent, 2));
}

#[test]
fn normalize_merges_points_into_the_first_text_node() {
    let mut dom = Dom::new();
    let (parent, _) = parent_with_children(&mut dom, &[]);
    let first = dom.create_text("ab");
    let second = dom.create_text("cd");
    dom.append_child(parent, first).expect("append");
    dom.append_child(parent, second).expect("append");
    let range = range_between(&mut dom, point(second, 1), point(parent, 2));
    dom.normalize(parent).expect("normalize");
    assert_eq!(text_of(&dom, first), "abcd");
    assert_eq!(dom.parent(second), None);
    assert_eq!(dom.range_start(range).unwrap(), point(first, 3));
    assert_eq!(dom.range_end(range).unwrap(), point(parent, 1));
}

#[test]
fn normalize_drops_empty_text_nodes() {
    let mut dom = Dom::new();
    let (parent, kids) = parent_with_children(&mut dom, &["a", "b"]);
    let empty = dom.create_text("");
    dom.insert_before(parent, empty, Some(kids[1]))
        .expect("insert");
    dom.normalize(parent).expect("normalize");
    assert_eq!(dom.children(parent).unwrap(), kids.as_slice());
}

#[test]
fn extract_contents_moves_the_middle_element_and_collapses() {
    let mut dom = Dom::new();
    let (parent, kids) = parent_with_children(&mut dom, &["a", "b", "c"]);
    let range = range_between(&mut dom, point(parent, 1), point(parent, 2));
    let fragment = dom.range_extract_contents(range).expect("extract");
    assert_eq!(dom.children(fragment).unwrap(), &[kids[1]]);
    assert_eq!(dom.children(parent).unwrap(), &[kids[0], kids[2]]);
    assert_eq!(dom.range_start(range).unwrap(), point(parent, 1));
    assert_eq!(dom.range_end(range).unwrap(), point(parent, 1));
}

/// `<div>hello<span>world</span></div>`, with the range from `hello[2]` to
/// `world[3]`.
fn hello_world(dom: &mut Dom) -> (NodeId, NodeId, NodeId, NodeId, RangeId) {
    let (parent, _) = parent_with_children(dom, &[]);
    let hello = dom.create_text("hello");
    let span = dom.create_element("span");
    let world = dom.create_text("world");
    dom.append_child(parent, hello).expect("append");
    dom.append_child(parent, span).expect("append");
    dom.append_child(span, world).expect("append");
    let range = range_between(dom, point(hello, 2), point(world, 3));
    (parent, hello, span, world, range)
}

#[test]
fn delete_contents_trims_text_at_both_ends() {
    let mut dom = Dom::new();
    let (parent, hello, span, world, range) = hello_world(&mut dom);
    dom.range_delete_contents(range).expect("delete");
    assert_eq!(text_of(&dom, hello), "he");
    assert_eq!(text_of(&dom, world), "ld");
    assert_eq!(dom.children(span).unwrap(), &[world]);
    assert_eq!(dom.range_start(range).unwrap(), point(parent, 1));
    assert_eq!(dom.range_end(range).unwrap(), point(parent, 1));
}

#[test]
fn clone_contents_copies_the_selected_text_and_leaves_the_tree_alone() {
    let mut dom = Dom::new();
    let (parent, hello, span, world, range) = hello_world(&mut dom);
    let fragment = dom.range_clone_contents(range).expect("clone");
    let parts = dom.children(fragment).unwrap().to_vec();
    assert_eq!(parts.len(), 2);
    assert_eq!(text_of(&dom, parts[0]), "llo");
    assert!(matches!(
        dom.node(parts[1]).map(crate::Node::kind),
        Some(NodeKind::Element(data)) if data.local_name == "span"
    ));
    let copied_text = dom.children(parts[1]).unwrap()[0];
    assert_eq!(text_of(&dom, copied_text), "wor");
    assert_eq!(dom.children(parent).unwrap(), &[hello, span]);
    assert_eq!(text_of(&dom, hello), "hello");
    assert_eq!(text_of(&dom, world), "world");
}

#[test]
fn to_string_joins_the_selected_text() {
    let mut dom = Dom::new();
    let (_, _, _, _, range) = hello_world(&mut dom);
    assert_eq!(dom.range_to_string(range).unwrap(), "llowor");
}

#[test]
fn surround_contents_wraps_the_selected_element() {
    let mut dom = Dom::new();
    let (parent, kids) = parent_with_children(&mut dom, &["a", "b", "c"]);
    let range = range_between(&mut dom, point(parent, 1), point(parent, 2));
    let wrapper = dom.create_element("w");
    dom.range_surround_contents(range, wrapper)
        .expect("surround");
    assert_eq!(dom.children(parent).unwrap(), &[kids[0], wrapper, kids[2]]);
    assert_eq!(dom.children(wrapper).unwrap(), &[kids[1]]);
    assert_eq!(dom.range_start(range).unwrap(), point(parent, 1));
    assert_eq!(dom.range_end(range).unwrap(), point(parent, 2));
}

#[test]
fn surround_contents_refuses_a_partially_selected_element() {
    let mut dom = Dom::new();
    let (parent, kids) = parent_with_children(&mut dom, &["b", "c"]);
    let text = dom.create_text("x");
    dom.append_child(kids[0], text).expect("append");
    // The element `b` contains the start and not the end, so it is partially
    // contained, and an element may not be partially contained.
    let range = range_between(&mut dom, point(text, 0), point(parent, 2));
    let wrapper = dom.create_element("w");
    let error = dom
        .range_surround_contents(range, wrapper)
        .expect_err("a partially selected element");
    assert_eq!(error.kind(), DomErrorKind::InvalidState);
}

#[test]
fn compare_boundary_points_orders_two_ranges() {
    let mut dom = Dom::new();
    let (parent, _) = parent_with_children(&mut dom, &["a", "b", "c"]);
    let first = range_between(&mut dom, point(parent, 0), point(parent, 1));
    let second = range_between(&mut dom, point(parent, 2), point(parent, 3));
    assert_eq!(
        dom.range_compare_boundary_points(first, 0, second).unwrap(),
        -1
    );
    assert_eq!(
        dom.range_compare_boundary_points(second, 0, first).unwrap(),
        1
    );
    assert_eq!(
        dom.range_compare_boundary_points(first, 2, first).unwrap(),
        0
    );
    let error = dom
        .range_compare_boundary_points(first, 4, second)
        .expect_err("unknown how");
    assert_eq!(error.kind(), DomErrorKind::NotSupported);
}

#[test]
fn comparison_across_trees_is_wrong_document() {
    let mut dom = Dom::new();
    let (left, _) = parent_with_children(&mut dom, &[]);
    let (right, _) = parent_with_children(&mut dom, &[]);
    let first = range_between(&mut dom, point(left, 0), point(left, 0));
    let second = range_between(&mut dom, point(right, 0), point(right, 0));
    let error = dom
        .range_compare_boundary_points(first, 0, second)
        .expect_err("different roots");
    assert_eq!(error.kind(), DomErrorKind::WrongDocument);
}

#[test]
fn insert_node_into_a_collapsed_range_expands_it_around_the_node() {
    let mut dom = Dom::new();
    let (parent, kids) = parent_with_children(&mut dom, &["a", "b"]);
    let range = range_between(&mut dom, point(parent, 1), point(parent, 1));
    let added = dom.create_element("x");
    dom.range_insert_node(range, added).expect("insert");
    assert_eq!(dom.children(parent).unwrap(), &[kids[0], added, kids[1]]);
    assert_eq!(dom.range_start(range).unwrap(), point(parent, 1));
    assert_eq!(dom.range_end(range).unwrap(), point(parent, 2));
}

#[test]
fn static_ranges_are_not_updated_by_mutations() {
    let mut dom = Dom::new();
    let (parent, kids) = parent_with_children(&mut dom, &["a", "b"]);
    let fixed = dom
        .create_static_range(point(parent, 1), point(parent, 2))
        .expect("static range");
    dom.remove_child(parent, kids[0]).expect("remove");
    assert_eq!(dom.range_start(fixed).unwrap(), point(parent, 1));
    assert_eq!(dom.range_end(fixed).unwrap(), point(parent, 2));
    assert_eq!(dom.range_kind(fixed), Some(crate::RangeKind::Static));
}
