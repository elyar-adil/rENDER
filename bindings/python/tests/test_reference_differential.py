"""Migration checks against the Python reference implementation.

Equality here prevents accidental regressions for behavior that was already
correct. The explicit inequality cases document places where standards-correct
Rust behavior intentionally supersedes permissive Python behavior.
"""

from css.lengths import resolve_length_expr as resolve_python
from html.parser import parse as parse_python
from render_runtime import parse_html_snapshot, resolve_length_expr as resolve_rust


def test_valid_reference_cases_stay_aligned():
    cases = [
        ("calc(456px*2)", {}),
        ("calc(100% - 40px)", {"percentage_base": 1440}),
        ("calc((100vw - 40px) / 2)", {"vw": 1440}),
        ("calc(2rem + 0.5em)", {"rem_base": 16, "em_base": 20}),
        ("clamp(10px, 5vw, 80px)", {"vw": 1000}),
    ]
    for expression, context in cases:
        assert resolve_rust(expression, **context) == resolve_python(expression, **context)


def test_rust_rejects_dimensionally_invalid_values_python_accepted():
    assert resolve_python("calc(1px + 2)") == 3.0
    assert resolve_rust("calc(1px + 2)") is None

    assert resolve_python("calc(1px * 2px)") == 2.0
    assert resolve_rust("calc(1px * 2px)") is None


def test_rust_does_not_invent_units_or_font_metrics():
    assert resolve_python("12") == 12.0
    assert resolve_rust("12") is None

    assert resolve_python("1ch", em_base=16) == 8.0
    assert resolve_rust("1ch", em_base=16) is None


def test_rust_html_uses_standard_first_duplicate_attribute_rule():
    python_document = parse_python("<div id=first id=second></div>")
    python_div = python_document.children[0].children[0].children[0]
    assert python_div.attributes["id"] == "second"

    rust_snapshot = parse_html_snapshot("<div id=first id=second></div>")

    def find(node):
        if node.get("local_name") == "div":
            return node
        for child in node["children"]:
            if found := find(child):
                return found
        return None

    rust_div = find(rust_snapshot["document"])
    assert rust_div["attributes"]["id"] == "first"
    assert "duplicate-attribute" in {
        error["code"] for error in rust_snapshot["errors"]
    }
