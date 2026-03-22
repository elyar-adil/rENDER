from __future__ import annotations

import engine
import pytest
import layout as layout_mod
from rendering.display_list import DrawLinearGradient, PushTransform
from tests.layout_test_utils import render_document, require_element


def test_calc_width_with_viewport_units_flows_into_layout_geometry():
    html = """
        <html>
          <head>
            <style>
              body { margin: 0; }
              #panel { width: calc((100vw - 40px) / 2); height: 30px; }
            </style>
          </head>
          <body><div id="panel"></div></body>
        </html>
    """
    document = render_document(html, viewport_w=1440, viewport_h=200)
    panel = require_element(document, "#panel")

    assert panel.box.content_width == pytest.approx(700.0, abs=1.0)
    assert panel.box.content_height == pytest.approx(30.0, abs=1.0)


def test_grid_repeat_fr_with_gap_matches_three_equal_cards():
    html = """
        <html>
          <head>
            <style>
              body { margin: 0; }
              #grid {
                display: grid;
                grid-template-columns: repeat(3, 1fr);
                column-gap: 20px;
                width: 320px;
              }
              .item { height: 20px; }
            </style>
          </head>
          <body>
            <div id="grid">
              <div id="a" class="item"></div>
              <div id="b" class="item"></div>
              <div id="c" class="item"></div>
            </div>
          </body>
        </html>
    """
    document = render_document(html, viewport_w=400, viewport_h=200)
    first = require_element(document, "#a")
    second = require_element(document, "#b")
    third = require_element(document, "#c")

    assert first.box.content_width == pytest.approx(106.67, abs=1.0)
    assert second.box.x == pytest.approx(first.box.x + first.box.content_width + 20.0, abs=1.0)
    assert third.box.x == pytest.approx(second.box.x + second.box.content_width + 20.0, abs=1.0)
    assert first.box.y == pytest.approx(second.box.y, abs=1.0)
    assert second.box.y == pytest.approx(third.box.y, abs=1.0)


def test_before_pseudo_with_attr_content_is_inserted_before_text_content():
    html = """
        <html>
          <head>
            <style>
              body { margin: 0; }
              #tag::before {
                content: attr(data-icon);
                display: inline-block;
                width: 12px;
                height: 12px;
              }
            </style>
          </head>
          <body><div id="tag" data-icon="NEW">Body</div></body>
        </html>
    """
    document = render_document(html, viewport_w=400, viewport_h=200)
    tag = require_element(document, "#tag")
    pseudo = tag.children[0]

    assert getattr(pseudo, "tag", "") == "__before__"
    assert pseudo.children[0].data == "NEW"
    assert pseudo.box.content_width == pytest.approx(12.0, abs=1.0)
    assert pseudo.box.content_height >= 12.0


def test_fixed_position_uses_viewport_coordinates_without_joining_normal_flow():
    html = """
        <html>
          <head>
            <style>
              body { margin: 0; }
              #flow { height: 40px; }
              #fixed {
                position: fixed;
                top: 10px;
                left: 20px;
                width: 30px;
                height: 15px;
              }
            </style>
          </head>
              <body>
                <div id="flow"></div>
                <div id="fixed"></div>
                <div id="tail" style="height: 25px;"></div>
              </body>
            </html>
    """
    document = render_document(html, viewport_w=300, viewport_h=200)
    flow = require_element(document, "#flow")
    fixed = require_element(document, "#fixed")
    tail = require_element(document, "#tail")

    assert fixed.box.x == pytest.approx(20.0, abs=1.0)
    assert fixed.box.y == pytest.approx(10.0, abs=1.0)
    assert tail.box.y == pytest.approx(flow.box.y + flow.box.content_height, abs=1.0)


def test_transform_rotate_scale_emits_push_transform_with_origin():
    html = """
        <html>
          <head>
            <style>
              body { margin: 0; }
              #box {
                width: 40px;
                height: 20px;
                transform: rotate(15deg) scale(2);
                transform-origin: left top;
              }
            </style>
          </head>
          <body><div id="box"></div></body>
        </html>
    """
    display_list, _page_height, _document = engine._pipeline(
        html,
        base_url="",
        viewport_width=300,
        viewport_height=200,
    )
    transform = next(cmd for cmd in display_list if isinstance(cmd, PushTransform))

    assert transform.rotate_deg == pytest.approx(15.0, abs=0.1)
    assert transform.scale_x == pytest.approx(2.0, abs=0.1)
    assert transform.scale_y == pytest.approx(2.0, abs=0.1)
    assert transform.origin_x == pytest.approx(0.0, abs=0.1)
    assert transform.origin_y == pytest.approx(0.0, abs=0.1)


def test_linear_gradient_background_emits_gradient_draw_command():
    html = """
        <html>
          <head>
            <style>
              body { margin: 0; }
              .card {
                width: 100px;
                height: 40px;
                background: linear-gradient(to right, #000, #fff);
              }
            </style>
          </head>
          <body><div class="card"></div></body>
        </html>
    """
    document = render_document(html, viewport_w=200, viewport_h=120)
    display_list = layout_mod.layout(document, viewport_width=200, viewport_height=120)

    gradient = next(cmd for cmd in display_list if isinstance(cmd, DrawLinearGradient))
    assert gradient.angle == pytest.approx(90.0, abs=0.1)
    assert len(gradient.color_stops) == 2
