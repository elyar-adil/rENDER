"""Integration tests: Display List Correctness.

Verifies that the rendering pipeline produces correct draw commands
for various CSS visual effects.
"""
import pytest
from tests.render_helper import render, find_element, get_display_list, approx
from rendering.display_list import (
    DrawRect, DrawText, DrawBorder, DrawImage,
    PushOpacity, PopOpacity, PushTransform, PopTransform,
    PushClip, PopClip, DrawBoxShadow, DrawLinearGradient,
)


class TestDrawCommands:
    """Basic draw command generation."""

    def test_background_generates_draw_rect(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="width:200px; height:100px; background-color:#ff0000">x</div>
        </body></html>
        ''')
        dl = get_display_list(doc)
        rects = [cmd for cmd in dl if isinstance(cmd, DrawRect) and cmd.color == '#ff0000']
        assert len(rects) == 1, f"Expected 1 red rect, got {len(rects)}"
        assert approx(rects[0].rect.width, 200.0)
        assert approx(rects[0].rect.height, 100.0)

    def test_border_generates_draw_border(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="width:200px; height:100px; border:3px solid green">x</div>
        </body></html>
        ''')
        dl = get_display_list(doc)
        borders = [cmd for cmd in dl if isinstance(cmd, DrawBorder)]
        assert len(borders) >= 1, "Should have border draw command"

    def test_text_generates_draw_text(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body><p style="margin:0; color:blue">Hello</p></body></html>
        ''')
        dl = get_display_list(doc)
        texts = [cmd for cmd in dl if isinstance(cmd, DrawText) and 'Hello' in cmd.text]
        assert len(texts) >= 1, "Should have DrawText for 'Hello'"
        assert texts[0].color == 'blue'


class TestOpacity:
    """opacity property generates push/pop commands."""

    def test_opacity_wraps_content(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="opacity:0.5; width:100px; height:100px; background-color:red">x</div>
        </body></html>
        ''')
        dl = get_display_list(doc)
        push_ops = [cmd for cmd in dl if isinstance(cmd, PushOpacity)]
        pop_ops = [cmd for cmd in dl if isinstance(cmd, PopOpacity)]
        assert len(push_ops) >= 1, "Should have PushOpacity"
        assert len(pop_ops) >= 1, "Should have PopOpacity"
        assert push_ops[0].opacity == 0.5


class TestOverflowClipping:
    """overflow:hidden generates clip commands."""

    def test_overflow_hidden_clips(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="overflow:hidden; width:200px; height:100px">
            <div style="height:500px">tall</div>
          </div>
        </body></html>
        ''')
        dl = get_display_list(doc)
        clips = [cmd for cmd in dl if isinstance(cmd, PushClip)]
        pops = [cmd for cmd in dl if isinstance(cmd, PopClip)]
        assert len(clips) >= 1, "overflow:hidden should push clip"
        assert len(pops) >= 1, "Should have matching PopClip"


class TestBoxShadow:
    """box-shadow generates DrawBoxShadow."""

    def test_box_shadow_renders(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="width:200px; height:100px; box-shadow: 5px 5px 10px rgba(0,0,0,0.3)">x</div>
        </body></html>
        ''')
        dl = get_display_list(doc)
        shadows = [cmd for cmd in dl if isinstance(cmd, DrawBoxShadow)]
        assert len(shadows) >= 1, "Should have DrawBoxShadow"


class TestLinearGradient:
    """linear-gradient background-image."""

    def test_gradient_renders(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="width:200px; height:100px;
                      background-image: linear-gradient(to right, red, blue)">x</div>
        </body></html>
        ''')
        dl = get_display_list(doc)
        grads = [cmd for cmd in dl if isinstance(cmd, DrawLinearGradient)]
        assert len(grads) >= 1, "Should have DrawLinearGradient"


class TestTransform:
    """CSS transform generates PushTransform/PopTransform."""

    def test_translate_transform(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="width:100px; height:100px; transform: translate(50px, 30px);
                      background-color: red">x</div>
        </body></html>
        ''')
        dl = get_display_list(doc)
        transforms = [cmd for cmd in dl if isinstance(cmd, PushTransform)]
        assert len(transforms) >= 1, "Should have PushTransform"
        t = transforms[0]
        assert approx(t.dx, 50.0)
        assert approx(t.dy, 30.0)


class TestDisplayNoneNoDraw:
    """display:none elements produce no draw commands."""

    def test_hidden_element_no_commands(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="display:none; width:200px; height:100px;
                      background-color:red">Hidden</div>
          <div style="width:200px; height:100px;
                      background-color:blue">Visible</div>
        </body></html>
        ''')
        dl = get_display_list(doc)
        red_rects = [cmd for cmd in dl if isinstance(cmd, DrawRect) and cmd.color == 'red']
        blue_rects = [cmd for cmd in dl if isinstance(cmd, DrawRect) and cmd.color == 'blue']
        assert len(red_rects) == 0, "Hidden element should not produce draw commands"
        assert len(blue_rects) >= 1, "Visible element should have draw commands"


class TestDrawOrder:
    """Draw order: background → border → content (painter's algorithm)."""

    def test_background_before_text(self):
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="width:200px; height:50px; background-color:yellow; color:black">
            Text content
          </div>
        </body></html>
        ''')
        dl = get_display_list(doc)
        bg_idx = None
        text_idx = None
        for i, cmd in enumerate(dl):
            if isinstance(cmd, DrawRect) and cmd.color == 'yellow' and bg_idx is None:
                bg_idx = i
            if isinstance(cmd, DrawText) and 'Text' in getattr(cmd, 'text', '') and text_idx is None:
                text_idx = i
        if bg_idx is not None and text_idx is not None:
            assert bg_idx < text_idx, \
                "Background should be drawn before text"
