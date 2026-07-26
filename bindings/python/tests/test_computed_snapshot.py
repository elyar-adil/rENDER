from render_runtime import computed_html_snapshot


def _style(result, index=0):
    return result["styles"][index]


def test_computed_snapshot_resolves_inheritance_variables_and_initial_values():
    result = computed_html_snapshot(
        "<!doctype html><div id='parent'><span id='child'></span></div>",
        "#parent { --base: red; --derived: var(--base); visibility: hidden; } "
        "#child { --base: blue; --fallback: var(--missing, var(--base)); "
        "visibility: unset; opacity: unset; position: var(--missing, relative); }",
        "#child",
    )
    properties = _style(result)["properties"]

    assert result["stylesheet_diagnostics"] == []
    assert properties["--base"] == "blue"
    assert properties["--derived"] == "red"
    assert properties["--fallback"] == "blue"
    assert properties["visibility"] == "hidden"
    assert properties["opacity"] == "1"
    assert properties["position"] == "relative"


def test_computed_snapshot_exposes_cycles_as_invalid_not_literal_values():
    result = computed_html_snapshot(
        "<!doctype html><div id='target'></div>",
        "#target { --a: var(--b, red); --b: var(--a, blue); "
        "--safe: var(--a, green); display: var(--a); }",
        "#target",
    )
    style = _style(result)

    assert style["properties"]["--safe"] == "green"
    assert style["properties"]["display"] == "inline"
    assert set(style["invalid_custom_properties"]) >= {"--a", "--b"}
    assert any("cycle" in item["message"] for item in style["diagnostics"])


def test_computed_snapshot_exposes_layout_typed_values_and_iacvt_fallbacks():
    result = computed_html_snapshot(
        "<!doctype html><div id='target'></div>",
        "#target { --number: 1; width: calc(100% - 2rem); "
        "padding-left: -3px; margin-left: -10%; opacity: 150%; "
        "display: inline flow-root; right: var(--number)px; }",
        "#target",
    )
    style = _style(result)
    properties = style["properties"]
    typed = style["typed_properties"]

    assert typed["width"] == {"kind": "size", "css": "calc(100% - 2rem)"}
    assert typed["display"] == {"kind": "display", "css": "inline-block"}
    assert typed["margin-left"] == {"kind": "margin", "css": "-10%"}
    assert typed["opacity"] == {"kind": "opacity", "css": "1"}
    assert properties["padding-left"] == "0px"
    assert properties["right"] == "auto"
    assert any(item["property"] == "padding-left" for item in style["diagnostics"])
    assert any(item["property"] == "right" for item in style["diagnostics"])
