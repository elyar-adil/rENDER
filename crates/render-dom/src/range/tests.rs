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

fn body_with_text(dom: &mut Dom, data: &str) -> (NodeId, NodeId, NodeId) {
    let document = dom.document();
    let body = dom.create_element("body");
    dom.append_child(document, body).expect("append body");
    let text = dom.create_text(data);
    dom.append_child(body, text).expect("append text");
    (document, body, text)
}

#[test]
fn a_collapsed_selection_is_a_caret_at_the_text_offset() {
    let mut dom = Dom::new();
    let (document, _, text) = body_with_text(&mut dom, "hello world");
    dom.selection_collapse(document, Some(text), 6)
        .expect("collapse");

    assert_eq!(dom.selection_range_count(document).unwrap(), 1);
    assert_eq!(dom.selection_type(document).unwrap(), "Caret");
    assert!(dom.selection_is_collapsed(document).unwrap());
    assert_eq!(
        dom.selection_anchor(document).unwrap(),
        Some(point(text, 6))
    );
    assert_eq!(dom.selection_focus(document).unwrap(), Some(point(text, 6)));
    assert_eq!(
        dom.selection_to_string(document).unwrap(),
        "",
        "a collapsed caret selects no text"
    );
    assert_eq!(dom.selection_direction_name(document).unwrap(), "forward");
}

#[test]
fn a_selection_across_two_text_nodes_has_exact_boundaries_and_text() {
    let mut dom = Dom::new();
    let (parent, hello, span, world, _) = hello_world(&mut dom);
    let document = dom.document();
    dom.append_child(document, parent).expect("attach");
    dom.selection_set_base_and_extent(document, point(hello, 2), point(world, 3))
        .expect("set base and extent");

    assert_eq!(dom.selection_type(document).unwrap(), "Range");
    assert!(!dom.selection_is_collapsed(document).unwrap());
    assert_eq!(
        dom.selection_anchor(document).unwrap(),
        Some(point(hello, 2))
    );
    assert_eq!(
        dom.selection_focus(document).unwrap(),
        Some(point(world, 3))
    );
    let range = dom.selection_range(document).unwrap().expect("one range");
    assert_eq!(dom.range_start(range).unwrap(), point(hello, 2));
    assert_eq!(dom.range_end(range).unwrap(), point(world, 3));
    assert_eq!(dom.selection_to_string(document).unwrap(), "llowor");
    let _ = (parent, span);
}

#[test]
fn a_backwards_selection_keeps_the_range_normalized_and_swaps_anchor_and_focus() {
    let mut dom = Dom::new();
    let (parent, hello, _, world, _) = hello_world(&mut dom);
    let document = dom.document();
    dom.append_child(document, parent).expect("attach");
    dom.selection_set_base_and_extent(document, point(world, 4), point(hello, 1))
        .expect("set base and extent");

    assert_eq!(dom.selection_direction_name(document).unwrap(), "backward");
    assert_eq!(
        dom.selection_anchor(document).unwrap(),
        Some(point(world, 4))
    );
    assert_eq!(
        dom.selection_focus(document).unwrap(),
        Some(point(hello, 1))
    );
    let range = dom.selection_range(document).unwrap().expect("one range");
    assert_eq!(dom.range_start(range).unwrap(), point(hello, 1));
    assert_eq!(dom.range_end(range).unwrap(), point(world, 4));
    assert_eq!(dom.selection_to_string(document).unwrap(), "elloworl");
}

#[test]
fn add_range_adopts_only_the_first_range_and_remove_all_empties_it() {
    let mut dom = Dom::new();
    let document = dom.document();
    let body = dom.create_element("body");
    dom.append_child(document, body).expect("append");
    let first_child = dom.create_element("a");
    let second_child = dom.create_element("b");
    dom.append_child(body, first_child).expect("append");
    dom.append_child(body, second_child).expect("append");

    let first = range_between(&mut dom, point(body, 0), point(body, 1));
    let second = range_between(&mut dom, point(body, 1), point(body, 2));
    dom.selection_add_range(document, first).expect("add first");
    dom.selection_add_range(document, second)
        .expect("add second");

    assert_eq!(dom.selection_range_count(document).unwrap(), 1);
    assert_eq!(
        dom.selection_range(document).unwrap(),
        Some(first),
        "a selection holds the first range only"
    );
    assert_eq!(dom.range_start(first).unwrap(), point(body, 0));
    assert_eq!(dom.range_end(first).unwrap(), point(body, 1));

    dom.selection_remove_all_ranges(document)
        .expect("remove all");
    assert_eq!(dom.selection_range_count(document).unwrap(), 0);
    assert_eq!(dom.selection_type(document).unwrap(), "None");
    assert_eq!(dom.selection_anchor(document).unwrap(), None);
    assert_eq!(dom.selection_to_string(document).unwrap(), "");
}

#[test]
fn collapse_to_start_and_end_replace_the_selection_range() {
    let mut dom = Dom::new();
    let (parent, hello, _, world, _) = hello_world(&mut dom);
    let document = dom.document();
    dom.append_child(document, parent).expect("attach");
    dom.selection_set_base_and_extent(document, point(hello, 2), point(world, 3))
        .expect("set base and extent");
    let original = dom.selection_range(document).unwrap().expect("range");

    dom.selection_collapse_to_start(document).expect("start");
    assert_eq!(
        dom.selection_anchor(document).unwrap(),
        Some(point(hello, 2))
    );
    assert!(dom.selection_is_collapsed(document).unwrap());
    assert_ne!(
        dom.selection_range(document).unwrap(),
        Some(original),
        "collapseToStart replaces the range"
    );

    dom.selection_set_base_and_extent(document, point(hello, 2), point(world, 3))
        .expect("set base and extent");
    dom.selection_collapse_to_end(document).expect("end");
    assert_eq!(
        dom.selection_focus(document).unwrap(),
        Some(point(world, 3))
    );
    assert!(dom.selection_is_collapsed(document).unwrap());
}

#[test]
fn extend_moves_the_focus_and_records_the_direction() {
    let mut dom = Dom::new();
    let (parent, hello, _, world, _) = hello_world(&mut dom);
    let document = dom.document();
    dom.append_child(document, parent).expect("attach");
    dom.selection_collapse(document, Some(hello), 4)
        .expect("collapse");
    dom.selection_extend(document, world, 2).expect("extend");
    assert_eq!(dom.selection_direction_name(document).unwrap(), "forward");
    assert_eq!(
        dom.selection_anchor(document).unwrap(),
        Some(point(hello, 4))
    );
    assert_eq!(
        dom.selection_focus(document).unwrap(),
        Some(point(world, 2))
    );

    dom.selection_extend(document, hello, 1)
        .expect("extend back");
    assert_eq!(dom.selection_direction_name(document).unwrap(), "backward");
    assert_eq!(
        dom.selection_anchor(document).unwrap(),
        Some(point(hello, 4))
    );
    assert_eq!(
        dom.selection_focus(document).unwrap(),
        Some(point(hello, 1))
    );
    let range = dom.selection_range(document).unwrap().expect("range");
    assert_eq!(dom.range_start(range).unwrap(), point(hello, 1));
    assert_eq!(dom.range_end(range).unwrap(), point(hello, 4));
}

#[test]
fn select_all_children_and_delete_from_document_change_the_tree() {
    let mut dom = Dom::new();
    let (parent, hello, span, world, _) = hello_world(&mut dom);
    let document = dom.document();
    dom.append_child(document, parent).expect("attach");
    dom.selection_select_all_children(document, hello)
        .expect("select text");
    assert_eq!(dom.selection_to_string(document).unwrap(), "hello");
    assert_eq!(
        dom.selection_anchor(document).unwrap(),
        Some(point(hello, 0))
    );
    assert_eq!(
        dom.selection_focus(document).unwrap(),
        Some(point(hello, 5))
    );

    dom.selection_set_base_and_extent(document, point(hello, 2), point(world, 3))
        .expect("set base and extent");
    dom.selection_delete_from_document(document)
        .expect("delete");
    assert_eq!(text_of(&dom, hello), "he");
    assert_eq!(text_of(&dom, world), "ld");
    assert_eq!(dom.children(span).unwrap(), &[world]);
    assert_eq!(dom.children(parent).unwrap().len(), 2);
    assert!(dom.selection_is_collapsed(document).unwrap());
    assert_eq!(dom.selection_to_string(document).unwrap(), "");
}

#[test]
fn removing_a_selected_node_clamps_the_selection_to_its_parent() {
    let mut dom = Dom::new();
    let document = dom.document();
    let body = dom.create_element("body");
    dom.append_child(document, body).expect("append");
    let paragraph = dom.create_element("p");
    dom.append_child(body, paragraph).expect("append");
    let text = dom.create_text("selected");
    dom.append_child(paragraph, text).expect("append");
    dom.selection_collapse(document, Some(text), 4)
        .expect("collapse");

    dom.remove_child(body, paragraph).expect("remove");
    assert_eq!(
        dom.selection_anchor(document).unwrap(),
        Some(point(body, 0)),
        "a removed boundary collapses into the old parent"
    );
    assert!(dom.selection_is_collapsed(document).unwrap());
    assert_eq!(dom.selection_type(document).unwrap(), "Caret");
    let range = dom.selection_range(document).unwrap().expect("range");
    assert_eq!(dom.range_start(range).unwrap().node, body);
    assert_eq!(dom.range_start(range).unwrap().offset, 0);
}

#[test]
fn split_text_keeps_the_selection_on_the_text_it_named() {
    let mut dom = Dom::new();
    let (_, _, text) = body_with_text(&mut dom, "abcdef");
    let document = dom.document();
    dom.selection_set_base_and_extent(document, point(text, 2), point(text, 5))
        .expect("set base and extent");
    let rest = dom.split_text(text, 3).expect("split");
    assert_eq!(
        dom.selection_anchor(document).unwrap(),
        Some(point(text, 2))
    );
    assert_eq!(dom.selection_focus(document).unwrap(), Some(point(rest, 2)));
    assert_eq!(dom.selection_to_string(document).unwrap(), "cde");
}

#[test]
fn normalize_moves_the_selection_into_the_merged_text_node() {
    let mut dom = Dom::new();
    let document = dom.document();
    let body = dom.create_element("body");
    dom.append_child(document, body).expect("body");
    let paragraph = dom.create_element("p");
    dom.append_child(body, paragraph).expect("paragraph");
    let first = dom.create_text("ab");
    let second = dom.create_text("cd");
    dom.append_child(paragraph, first).expect("first");
    dom.append_child(paragraph, second).expect("second");
    dom.selection_set_base_and_extent(document, point(second, 0), point(second, 2))
        .expect("selection");
    dom.normalize(paragraph).expect("normalize");

    assert_eq!(
        dom.selection_anchor(document).unwrap(),
        Some(point(first, 2))
    );
    assert_eq!(
        dom.selection_focus(document).unwrap(),
        Some(point(first, 4))
    );
    assert_eq!(dom.selection_to_string(document).unwrap(), "cd");
}

#[test]
fn contains_node_checks_full_and_partial_containment() {
    let mut dom = Dom::new();
    let (parent, hello, span, world, _) = hello_world(&mut dom);
    let document = dom.document();
    dom.append_child(document, parent).expect("attach");
    dom.selection_set_base_and_extent(document, point(parent, 0), point(parent, 1))
        .expect("select hello");
    assert!(dom.selection_contains_node(document, hello, false).unwrap());
    assert!(dom.selection_contains_node(document, hello, true).unwrap());
    assert!(!dom.selection_contains_node(document, span, false).unwrap());

    dom.selection_set_base_and_extent(document, point(hello, 2), point(world, 3))
        .expect("partial");
    assert!(!dom.selection_contains_node(document, hello, false).unwrap());
    assert!(dom.selection_contains_node(document, hello, true).unwrap());
    assert!(dom.selection_contains_node(document, span, true).unwrap());
}

#[test]
fn every_document_has_its_own_selection() {
    let mut dom = Dom::new();
    let first = body_with_text(&mut dom, "one");
    dom.selection_set_base_and_extent(first.0, point(first.2, 1), point(first.2, 3))
        .expect("first");
    let second_document = dom.create_document("text/html");
    let second_body = dom.create_element("body");
    dom.append_child(second_document, second_body)
        .expect("body");
    let second_text = dom.create_text("two");
    dom.append_child(second_body, second_text).expect("text");
    dom.selection_set_base_and_extent(
        second_document,
        point(second_text, 2),
        point(second_text, 3),
    )
    .expect("second");

    assert_eq!(dom.selection_to_string(first.0).unwrap(), "ne");
    assert_eq!(dom.selection_to_string(second_document).unwrap(), "o");
    dom.selection_remove_all_ranges(first.0)
        .expect("empty first");
    assert_eq!(dom.selection_range_count(first.0).unwrap(), 0);
    assert_eq!(dom.selection_range_count(second_document).unwrap(), 1);
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
