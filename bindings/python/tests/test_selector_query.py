from pathlib import Path

import pytest

from render_runtime import query_html_snapshot


ROOT = Path(__file__).resolve().parents[3]
FIXTURE = ROOT / "tests" / "fixtures" / "interop" / "selector_oracle.html"


def nodes_by_id(node):
    result = {node["id"]: node}
    for child in node["children"]:
        result.update(nodes_by_id(child))
    return result


def query(selector):
    result = query_html_snapshot(FIXTURE.read_text(encoding="utf-8"), selector)
    nodes = nodes_by_id(result["document"])
    return [nodes[node_id]["attributes"].get("id", "") for node_id in result["match_ids"]]


@pytest.mark.parametrize(
    ("selector", "expected"),
    [
        ("main > .card h2 + p.hot", ["lead"]),
        ("h2 ~ p", ["lead", "tail"]),
        (
            "[data-tags~=two][lang|=en][href^='https' i][href$='.html' i]",
            ["link"],
        ),
        ("li:nth-child(2)", ["item-b"]),
        ("li:nth-child(2 of .x)", ["item-c"]),
        ("article:is(.card, #missing):has(> h3)", ["article-a"]),
        ("div:has(span b)", ["sib-a"]),
        ("div:has(+ #sib-p)", ["sib-b"]),
        ("#empty:empty", ["empty"]),
        ("#whitespace:empty", []),
        (r"#\31 23.a\+b", ["123"]),
        (":scope > body", [""]),
        ("p::before", []),
    ],
)
def test_query_contract(selector, expected):
    assert query(selector) == expected


@pytest.mark.parametrize(
    "selector",
    ["#123", "div, :unsupported()", ":nth-of-type(2 of .x)"],
)
def test_invalid_selector_list_raises_value_error(selector):
    with pytest.raises(ValueError):
        query_html_snapshot("<!doctype html><div></div>", selector)


def test_query_returns_per_selector_specificity():
    result = query_html_snapshot("<!doctype html><div></div>", "div:where(#x), #id.a")
    assert result["specificities"] == [(0, 0, 1), (1, 1, 0)]
