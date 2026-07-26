"""Named selector differences between the legacy Python and Rust engines.

These assertions deliberately do not require blind parity.  They preserve the
evidence that the Rust path follows Selectors/DOM semantics where the Python
reference is currently permissive or incomplete.
"""

from css import selector as python_selector
from html.dom import Element, Text
from render_runtime import query_html_snapshot


def _match_ids(html, selector):
    result = query_html_snapshot(html, selector)
    by_node_id = {}

    def visit(node):
        by_node_id[node["id"]] = node
        for child in node["children"]:
            visit(child)

    visit(result["document"])
    return [
        by_node_id[node_id].get("attributes", {}).get("id", "")
        for node_id in result["match_ids"]
    ]


def test_whitespace_text_prevents_empty_matching():
    element = Element("div")
    whitespace = Text(" ")
    whitespace.parent = element
    element.children = [whitespace]

    # Legacy Python incorrectly strips text before applying :empty.
    assert python_selector.matches(element, ":empty") is True
    assert _match_ids(
        "<!doctype html><div id='target'> </div>",
        "#target:empty",
    ) == []


def test_nth_child_of_selector_filters_sibling_list_before_counting():
    parent = Element("ul")
    python_children = [
        Element("li", {"class": "x"}),
        Element("li"),
        Element("li", {"class": "x"}),
    ]
    for child in python_children:
        child.parent = parent
    parent.children = python_children

    # Legacy Python treats the Level 4 `of S` clause as an invalid formula.
    assert not any(
        python_selector.matches(child, "li:nth-child(2 of .x)")
        for child in python_children
    )
    assert _match_ids(
        "<!doctype html><ul>"
        "<li id='a' class='x'></li><li id='b'></li><li id='c' class='x'></li>"
        "</ul>",
        "li:nth-child(2 of .x)",
    ) == ["c"]


def test_where_has_zero_specificity():
    # Legacy Python counts :where() as a pseudo-class. Selectors Level 4 says
    # both the pseudo-class and its argument contribute zero specificity.
    assert python_selector.specificity("div:where(#target)") == (0, 1, 1)
    result = query_html_snapshot(
        "<!doctype html><div id='target'></div>",
        "div:where(#target)",
    )
    assert result["specificities"] == [(0, 0, 1)]
