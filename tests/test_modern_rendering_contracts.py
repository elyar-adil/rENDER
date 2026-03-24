from __future__ import annotations

from pathlib import Path
from unittest.mock import patch
import os
import sys

import pytest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import engine
from html.dom import Element
from tests.layout_test_utils import iter_elements, require_element, render_document

ROOT = Path(__file__).resolve().parents[1]
FIXTURE_DIR = ROOT / "tests" / "fixtures" / "hao123_modules"
VIEWPORTS = {
    "header": (1280, 140),
    "search": (1280, 260),
    "sites": (1280, 360),
    "feed_columns": (1280, 420),
}


def _render_fixture(name: str):
    viewport_w, viewport_h = VIEWPORTS[name]
    html = (FIXTURE_DIR / f"{name}.html").read_text(encoding="utf-8")
    with (
        patch("engine._fetch_subresources", return_value=([], [])),
        patch("engine._fetch_background_images", return_value=[]),
        patch("engine._execute_scripts", return_value=None),
    ):
        return render_document(html, viewport_w=viewport_w, viewport_h=viewport_h)


def _elements_by_class(document, class_name: str) -> list[Element]:
    matches: list[Element] = []
    for node in iter_elements(document):
        classes = getattr(node, "attributes", {}).get("class", "").split()
        if class_name in classes:
            matches.append(node)
    return matches


def _assert_same_row(elements: list[Element], *, tolerance: float = 4.0) -> None:
    baseline = elements[0].box.y
    for element in elements[1:]:
        assert element.box.y == pytest.approx(baseline, abs=tolerance)


def _assert_left_to_right(elements: list[Element]) -> None:
    for current, nxt in zip(elements, elements[1:]):
        assert current.box.x < nxt.box.x


def _assert_inside(container: Element, target: Element, *, tolerance: float = 1.0) -> None:
    assert target.box.x >= container.box.x - tolerance
    assert target.box.y >= container.box.y - tolerance
    assert target.box.x + target.box.content_width <= (
        container.box.x + container.box.content_width + tolerance
    )
    assert target.box.y + target.box.content_height <= (
        container.box.y + container.box.content_height + tolerance
    )


def test_header_contract_matches_browser_like_shell_structure():
    document = _render_fixture("header")

    header = require_element(document, "#topColumn")
    logo = _elements_by_class(document, "head-item-logo-area")[0]
    weather = _elements_by_class(document, "head-item-weather-area")[0]
    links = _elements_by_class(document, "head-item-links-area")[0]

    _assert_left_to_right([logo, weather, links])
    _assert_same_row([logo, weather, links])
    _assert_inside(header, logo)
    _assert_inside(header, weather)
    _assert_inside(header, links)


def test_search_contract_preserves_logo_input_and_submit_track():
    document = _render_fixture("search")

    wrapper = _elements_by_class(document, "searchWrapper")[0]
    logo = _elements_by_class(document, "main-logo")[0]
    text_wrapper = _elements_by_class(document, "textWrapper")[0]
    submit_wrapper = _elements_by_class(document, "submitWrapper")[0]
    hotword = _elements_by_class(document, "hotword")[0]

    _assert_left_to_right([logo, text_wrapper, submit_wrapper])
    _assert_same_row([text_wrapper, submit_wrapper], tolerance=6.0)
    _assert_inside(wrapper, logo)
    _assert_inside(wrapper, text_wrapper)
    _assert_inside(wrapper, submit_wrapper)
    assert submit_wrapper.box.x == pytest.approx(
        text_wrapper.box.x + text_wrapper.box.content_width,
        abs=3.0,
    )
    assert hotword.box.x + hotword.box.content_width <= text_wrapper.box.x + text_wrapper.box.content_width + 2


def test_sites_contract_keeps_grid_wrapping_without_overlap():
    document = _render_fixture("sites")
    items = _elements_by_class(document, "site-item")

    assert len(items) >= 12

    first_row_y = items[0].box.y
    second_row = [item for item in items if item.box.y > first_row_y + 4]
    assert second_row, "site-item should wrap to a second row"

    first_row = [item for item in items if item.box.y <= first_row_y + 4]
    _assert_same_row(first_row[: min(5, len(first_row))])
    _assert_left_to_right(first_row[: min(5, len(first_row))])

    row2_first = second_row[0]
    assert row2_first.box.x == pytest.approx(items[0].box.x, abs=3.0)

    for item in items:
        assert item.box.content_width > 0
        assert item.box.content_height > 0


def test_feed_columns_contract_keeps_two_column_distribution():
    document = _render_fixture("feed_columns")

    left_wrapper = require_element(document, "#leftWrapperBox")
    right_column = _elements_by_class(document, "layout-left")[0]
    feed_root = require_element(document, "#feed_pagelet")

    assert left_wrapper.box.x == pytest.approx(0.0, abs=1.0)
    assert right_column.box.x >= left_wrapper.box.x + left_wrapper.box.content_width - 1
    assert right_column.box.y == pytest.approx(left_wrapper.box.y, abs=2.0)
    assert right_column.box.content_width > left_wrapper.box.content_width * 2
    assert feed_root.box.x == pytest.approx(right_column.box.x, abs=2.0)


def test_pipeline_contract_emits_before_icon_for_hot_refresh_link():
    html = (FIXTURE_DIR / "feed_columns.html").read_text(encoding="utf-8")

    with (
        patch("engine._fetch_subresources", return_value=([], [])),
        patch("engine._fetch_background_images", return_value=[]),
        patch("engine._execute_scripts", return_value=None),
    ):
        _display_list, _height, document = engine._pipeline(
            html,
            base_url="",
            viewport_width=VIEWPORTS["feed_columns"][0],
            viewport_height=VIEWPORTS["feed_columns"][1],
        )

    refresh_link = _elements_by_class(document, "hotsearch-refresh")[0]
    pseudo_children = [
        child for child in refresh_link.children
        if getattr(child, "tag", "") == "__before__"
    ]
    assert len(pseudo_children) == 1

    icon = pseudo_children[0]
    assert icon.box.content_width == pytest.approx(16.0, abs=1.0)
    assert icon.box.content_height == pytest.approx(16.0, abs=1.0)
