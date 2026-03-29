from __future__ import annotations

from pathlib import Path
from unittest.mock import patch

import pytest

import engine
from html.dom import Element
from rendering.display_list import PushTransform
from tests.layout_test_utils import iter_elements, render_document, require_element


ROOT = Path(__file__).resolve().parents[1]
HAO123_FIXTURE_DIR = ROOT / "tests" / "fixtures" / "hao123_modules"
MICROSOFT_FIXTURE_DIR = ROOT / "tests" / "fixtures" / "microsoft"
HAO123_VIEWPORTS = {
    "header": (1280, 140),
    "search": (1280, 260),
    "sites": (1280, 360),
    "feed_columns": (1280, 420),
}


def _render_html_offline(html: str, *, viewport_w: int, viewport_h: int):
    with (
        patch("engine._fetch_subresources", return_value=([], [])),
        patch("engine._fetch_background_images", return_value=[]),
        patch("engine._execute_scripts", return_value=None),
    ):
        return render_document(html, viewport_w=viewport_w, viewport_h=viewport_h)


def _render_pipeline_offline(html: str, *, viewport_w: int, viewport_h: int):
    with (
        patch("engine._fetch_subresources", return_value=([], [])),
        patch("engine._fetch_background_images", return_value=[]),
        patch("engine._execute_scripts", return_value=None),
    ):
        return engine._pipeline(
            html,
            base_url="",
            viewport_width=viewport_w,
            viewport_height=viewport_h,
        )


def _render_pipeline_with_css_offline(html: str, css_texts: list[str], *, viewport_w: int, viewport_h: int):
    with (
        patch("engine._fetch_subresources", return_value=(css_texts, [])),
        patch("engine._fetch_background_images", return_value=[]),
        patch("engine._execute_scripts", return_value=None),
    ):
        return engine._pipeline(
            html,
            base_url="https://www.microsoft.com/zh-cn/",
            viewport_width=viewport_w,
            viewport_height=viewport_h,
        )


def _render_hao123_fixture(name: str):
    viewport_w, viewport_h = HAO123_VIEWPORTS[name]
    html = (HAO123_FIXTURE_DIR / f"{name}.html").read_text(encoding="utf-8")
    return _render_html_offline(html, viewport_w=viewport_w, viewport_h=viewport_h)


def _find_all_by_class(document, class_name: str) -> list[Element]:
    matches = []
    for node in iter_elements(document):
        classes = getattr(node, "attributes", {}).get("class", "").split()
        if class_name in classes:
            matches.append(node)
    return matches


def _require_first_by_class(document, class_name: str) -> Element:
    matches = _find_all_by_class(document, class_name)
    if not matches:
        raise AssertionError(f"Missing element with class {class_name!r}")
    return matches[0]


def test_hao123_header_keeps_major_modules_in_one_band():
    document = _render_hao123_fixture("header")

    top_column = require_element(document, "#topColumn")
    logo_area = _require_first_by_class(document, "head-item-logo-area")
    weather_area = _require_first_by_class(document, "head-item-weather-area")
    links_area = _require_first_by_class(document, "head-item-links-area")

    assert logo_area.box.x < weather_area.box.x < links_area.box.x
    assert top_column.box.y <= logo_area.box.y <= top_column.box.y + top_column.box.content_height
    assert top_column.box.y <= weather_area.box.y <= top_column.box.y + top_column.box.content_height
    assert top_column.box.y <= links_area.box.y <= top_column.box.y + top_column.box.content_height
    assert links_area.box.x + links_area.box.content_width <= top_column.box.content_width + 1


def test_hao123_search_keeps_input_and_submit_inline():
    document = _render_hao123_fixture("search")

    wrapper = _require_first_by_class(document, "searchWrapper")
    logo = _require_first_by_class(document, "main-logo")
    text_wrapper = _require_first_by_class(document, "textWrapper")
    submit_wrapper = _require_first_by_class(document, "submitWrapper")
    hotword = _require_first_by_class(document, "hotword")

    assert wrapper.box.content_width > 1100
    assert logo.box.x < text_wrapper.box.x < submit_wrapper.box.x
    assert submit_wrapper.box.x == pytest.approx(
        text_wrapper.box.x + text_wrapper.box.content_width,
        abs=2.0,
    )
    assert text_wrapper.box.y == pytest.approx(submit_wrapper.box.y, abs=6.0)
    assert hotword.box.x + hotword.box.content_width <= text_wrapper.box.x + text_wrapper.box.content_width + 1


def test_hao123_sites_wrap_site_items_onto_multiple_rows():
    document = _render_hao123_fixture("sites")

    items = _find_all_by_class(document, "site-item")
    assert len(items) >= 12

    first_item = items[0]
    second_item = items[1]
    first_row_y = first_item.box.y
    second_row_first = next(item for item in items if item.box.y > first_row_y + 5)

    assert second_item.box.y == pytest.approx(first_row_y, abs=1.0)
    assert second_item.box.x > first_item.box.x
    assert second_row_first.box.y > first_row_y
    assert second_row_first.box.x == pytest.approx(first_item.box.x, abs=2.0)


def test_hao123_feed_columns_preserve_two_column_shell():
    document = _render_hao123_fixture("feed_columns")

    left_column = require_element(document, "#leftWrapperBox")
    right_column = _require_first_by_class(document, "layout-left")
    feed_pagelet = require_element(document, "#feed_pagelet")

    assert left_column.box.x == pytest.approx(0.0, abs=1.0)
    assert right_column.box.x >= left_column.box.x + left_column.box.content_width - 1
    assert right_column.box.y == pytest.approx(left_column.box.y, abs=1.0)
    assert right_column.box.content_width > left_column.box.content_width * 2
    assert feed_pagelet.box.x == pytest.approx(right_column.box.x, abs=1.0)


def test_real_fixture_generates_before_pseudo_icon():
    document = _render_hao123_fixture("feed_columns")

    refresh_link = _require_first_by_class(document, "hotsearch-refresh")
    pseudo_children = [
        child for child in refresh_link.children
        if getattr(child, "tag", "") == "__before__"
    ]

    assert len(pseudo_children) == 1
    icon = pseudo_children[0]
    assert icon.box.content_width == pytest.approx(16.0, abs=1.0)
    assert icon.box.content_height == pytest.approx(16.0, abs=1.0)
    assert icon.box.x == pytest.approx(refresh_link.box.x, abs=1.0)
    assert icon.box.y == pytest.approx(refresh_link.box.y + 12.0, abs=2.0)


def test_top_level_fixed_element_is_laid_out_after_normal_flow_pass():
    html = """
        <html>
          <head>
            <style>
              body { margin: 0; min-height: 200px; }
              #banner {
                position: fixed;
                top: 12px;
                left: 24px;
                width: 180px;
                height: 40px;
              }
            </style>
          </head>
          <body>
            <div id="banner">Pinned</div>
          </body>
        </html>
    """
    display_list, _page_height, document = _render_pipeline_offline(
        html,
        viewport_w=400,
        viewport_h=200,
    )

    banner = require_element(document, "#banner")

    assert banner.box is not None
    assert banner.box.x == pytest.approx(24.0, abs=1.0)
    assert banner.box.y == pytest.approx(12.0, abs=1.0)
    assert any(getattr(cmd, "text", "") == "Pinned" for cmd in display_list)


def test_microsoft_fixture_keeps_fixed_header_and_footer_content_visible():
    html = (MICROSOFT_FIXTURE_DIR / "blocked.html").read_text(encoding="utf-8")
    css_texts = [
        (MICROSOFT_FIXTURE_DIR / "css_0.css").read_text(encoding="utf-8"),
        (MICROSOFT_FIXTURE_DIR / "css_1.css").read_text(encoding="utf-8"),
    ]

    display_list, _page_height, document = _render_pipeline_with_css_offline(
        html,
        css_texts,
        viewport_w=1280,
        viewport_h=900,
    )

    skip_link = require_element(document, "#uhfSkipToMain")
    header_area = require_element(document, "#headerArea")
    main = require_element(document, "#mainContent")
    header_descendants_with_boxes = [
        node for node in iter_elements(header_area)
        if getattr(node, "box", None) is not None
    ]

    assert skip_link.box is not None
    assert skip_link.box.y <= 20
    assert len(header_descendants_with_boxes) >= 5
    assert main.box.content_height > 100

    drawn_text = [getattr(cmd, "text", "") for cmd in display_list if hasattr(cmd, "text")]
    assert "Skip" in drawn_text
    assert "Microsoft" in drawn_text
    assert "Homepage" in drawn_text




@pytest.mark.xfail(strict=True, reason="grid-column span still lays out as a single track")
def test_common_card_grid_can_span_multiple_columns():
    html = """
        <html>
          <head>
            <style>
              body { margin: 0; }
              #grid {
                display: grid;
                grid-template-columns: 100px 100px 100px;
                grid-template-rows: 30px 30px;
                gap: 10px;
              }
              .card { height: 30px; }
              #hero { grid-column: span 2; }
            </style>
          </head>
          <body>
            <div id="grid">
              <div id="hero" class="card"></div>
              <div id="side" class="card"></div>
              <div id="next" class="card"></div>
            </div>
          </body>
        </html>
    """
    document = _render_html_offline(html, viewport_w=400, viewport_h=200)

    hero = require_element(document, "#hero")
    side = require_element(document, "#side")
    next_card = require_element(document, "#next")

    assert hero.box.content_width == pytest.approx(210.0, abs=1.0)
    assert side.box.x == pytest.approx(220.0, abs=1.0)
    assert next_card.box.y == pytest.approx(40.0, abs=1.0)


@pytest.mark.xfail(strict=True, reason="translate() percentages are still resolved as 0")
def test_common_centering_transform_uses_box_relative_percentages():
    html = """
        <html>
          <head>
            <style>
              body { margin: 0; }
              #badge {
                width: 40px;
                height: 20px;
                transform: translate(-50%, -50%);
              }
            </style>
          </head>
          <body>
            <div id="badge"></div>
          </body>
        </html>
    """
    display_list, _page_height, _document = _render_pipeline_offline(
        html,
        viewport_w=200,
        viewport_h=100,
    )

    transform = next(cmd for cmd in display_list if isinstance(cmd, PushTransform))
    assert transform.dx == pytest.approx(-20.0, abs=1.0)
    assert transform.dy == pytest.approx(-10.0, abs=1.0)
