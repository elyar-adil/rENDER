from render_runtime import cascade_html_snapshot


def test_cascade_snapshot_keeps_cascaded_and_computed_stages_separate():
    result = cascade_html_snapshot(
        "<!doctype html><div id='target'></div>",
        "#target { color: red; --Theme: calc(1px + var(--gap)); }",
        "#target",
    )

    assert result["stylesheet_diagnostics"] == []
    assert result["styles"][0]["properties"] == {
        "--Theme": "calc(1px + var(--gap))",
        "color": "red",
    }


def test_cascade_snapshot_reports_unsupported_grouping_rules():
    result = cascade_html_snapshot(
        "<!doctype html><div id='target'></div>",
        "@media all { #target { color: red } } #target { color: blue }",
        "#target",
    )

    assert result["styles"][0]["properties"]["color"] == "blue"
    assert any("@media" in item["message"] for item in result["stylesheet_diagnostics"])
