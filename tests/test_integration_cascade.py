"""Integration tests: CSS Cascade, Specificity, Inheritance.

Verifies that the cascade algorithm correctly resolves which styles
apply to elements, matching real browser behavior.
"""
import pytest
from tests.render_helper import render, find_element, find_all, approx


class TestSpecificity:
    """Higher specificity rules win over lower ones."""

    def test_class_beats_tag(self):
        doc = render('''
        <html><head><style>
          div { width: 100px; }
          .wider { width: 300px; }
        </style></head>
        <body style="margin:0">
          <div id="d" class="wider" style="height:10px">x</div>
        </body></html>
        ''')
        d = find_element(doc, id_name='d')
        assert approx(d.box.content_width, 300.0), \
            f"Class should beat tag selector, got {d.box.content_width}"

    def test_id_beats_class(self):
        doc = render('''
        <html><head><style>
          .narrow { width: 100px; }
          #wide { width: 400px; }
        </style></head>
        <body style="margin:0">
          <div id="wide" class="narrow" style="height:10px">x</div>
        </body></html>
        ''')
        d = find_element(doc, id_name='wide')
        assert approx(d.box.content_width, 400.0), \
            f"ID should beat class selector, got {d.box.content_width}"

    def test_inline_style_beats_id(self):
        doc = render('''
        <html><head><style>
          #d { width: 100px; }
        </style></head>
        <body style="margin:0">
          <div id="d" style="width:500px; height:10px">x</div>
        </body></html>
        ''')
        d = find_element(doc, id_name='d')
        assert approx(d.box.content_width, 500.0), \
            f"Inline style should beat #id, got {d.box.content_width}"

    def test_important_beats_inline(self):
        doc = render('''
        <html><head><style>
          div { width: 250px !important; }
        </style></head>
        <body style="margin:0">
          <div id="d" style="width:500px; height:10px">x</div>
        </body></html>
        ''')
        d = find_element(doc, id_name='d')
        assert approx(d.box.content_width, 250.0), \
            f"!important should beat inline style, got {d.box.content_width}"


class TestCascadeOrder:
    """Later rules with same specificity win."""

    def test_later_rule_wins(self):
        doc = render('''
        <html><head><style>
          .box { width: 100px; }
          .box { width: 200px; }
        </style></head>
        <body style="margin:0">
          <div class="box" id="d" style="height:10px">x</div>
        </body></html>
        ''')
        d = find_element(doc, id_name='d')
        assert approx(d.box.content_width, 200.0), \
            f"Later rule should win, got {d.box.content_width}"

    def test_author_beats_ua(self):
        """Author stylesheet beats UA defaults."""
        doc = render('''
        <html><head><style>
          h1 { font-size: 50px; }
        </style></head>
        <body style="margin:0">
          <h1 id="h" style="margin:0">Title</h1>
        </body></html>
        ''')
        h = find_element(doc, id_name='h')
        assert h.style.get('font-size') == '50px', \
            f"Author should override UA font-size, got {h.style.get('font-size')}"


class TestInheritance:
    """Inherited properties propagate to children."""

    def test_color_inherits(self):
        doc = render('''
        <html><head><style>
          body { margin: 0; }
          .parent { color: red; }
        </style></head>
        <body>
          <div class="parent">
            <div id="child" style="height:10px">text</div>
          </div>
        </body></html>
        ''')
        child = find_element(doc, id_name='child')
        assert child.style.get('color') == 'red', \
            f"Color should inherit, got {child.style.get('color')}"

    def test_font_size_inherits(self):
        doc = render('''
        <html><head><style>
          .parent { font-size: 24px; }
        </style></head>
        <body style="margin:0">
          <div class="parent">
            <span id="child">text</span>
          </div>
        </body></html>
        ''')
        child = find_element(doc, id_name='child')
        assert child.style.get('font-size') == '24px', \
            f"Font-size should inherit, got {child.style.get('font-size')}"

    def test_font_family_inherits(self):
        doc = render('''
        <html><head><style>
          .parent { font-family: monospace; }
        </style></head>
        <body style="margin:0">
          <div class="parent">
            <span id="child">text</span>
          </div>
        </body></html>
        ''')
        child = find_element(doc, id_name='child')
        assert 'monospace' in child.style.get('font-family', ''), \
            f"Font-family should inherit, got {child.style.get('font-family')}"

    def test_width_does_not_inherit(self):
        """Width is non-inherited — child should NOT get parent's width."""
        doc = render('''
        <html><head><style>body { margin: 0; }</style></head>
        <body>
          <div style="width:200px">
            <div id="child" style="height:10px">x</div>
          </div>
        </body></html>
        ''')
        child = find_element(doc, id_name='child')
        # Child should fill parent (200px) via auto width, not inherit "200px"
        assert child.style.get('width') in ('auto', ''), \
            f"Width should not inherit, got {child.style.get('width')}"


class TestShorthandExpansion:
    """Shorthand properties expand to longhands correctly."""

    def test_margin_shorthand_1_value(self):
        """margin:20px expands to all four sides and layout uses them."""
        doc = render('<div id="d" style="margin:20px; width:100px; height:10px">x</div>')
        d = find_element(doc, id_name='d')
        # After compute, values may have decimal format like '20.0px'
        assert d.box.margin.top == 20.0
        assert d.box.margin.right == 20.0
        assert d.box.margin.bottom == 20.0
        assert d.box.margin.left == 20.0

    def test_margin_shorthand_2_values(self):
        doc = render('<div id="d" style="margin:10px 20px; width:100px; height:10px">x</div>')
        d = find_element(doc, id_name='d')
        assert d.box.margin.top == 10.0
        assert d.box.margin.right == 20.0
        assert d.box.margin.bottom == 10.0
        assert d.box.margin.left == 20.0

    def test_margin_shorthand_4_values(self):
        doc = render('<div id="d" style="margin:1px 2px 3px 4px; width:100px; height:10px">x</div>')
        d = find_element(doc, id_name='d')
        assert d.box.margin.top == 1.0
        assert d.box.margin.right == 2.0
        assert d.box.margin.bottom == 3.0
        assert d.box.margin.left == 4.0

    def test_padding_shorthand(self):
        doc = render('<div id="d" style="padding:15px 25px; width:100px; height:10px">x</div>')
        d = find_element(doc, id_name='d')
        assert d.box.padding.top == 15.0
        assert d.box.padding.right == 25.0

    def test_border_shorthand(self):
        doc = render('<div id="d" style="border:2px solid red; width:100px; height:10px">x</div>')
        d = find_element(doc, id_name='d')
        assert d.box.border.top == 2.0
        assert d.style.get('border-top-style') == 'solid'
        assert d.style.get('border-top-color') == 'red'


class TestUnitResolution:
    """CSS units are resolved to px correctly."""

    def test_em_relative_to_parent_font_size(self):
        doc = render('''
        <html><head><style>
          .parent { font-size: 20px; }
          .child { width: 10em; height: 10px; }
        </style></head>
        <body style="margin:0">
          <div class="parent">
            <div class="child" id="d">x</div>
          </div>
        </body></html>
        ''')
        d = find_element(doc, id_name='d')
        # 10em at 20px font-size = 200px
        assert approx(d.box.content_width, 200.0), \
            f"10em should be 200px, got {d.box.content_width}"

    def test_rem_relative_to_root_font_size(self):
        doc = render('''
        <html style="font-size:20px"><head><style>
          body { margin: 0; }
          .child { width: 5rem; height: 10px; }
        </style></head>
        <body>
          <div style="font-size:40px">
            <div class="child" id="d">x</div>
          </div>
        </body></html>
        ''')
        d = find_element(doc, id_name='d')
        # 5rem = 5 * 20px (root) = 100px
        assert approx(d.box.content_width, 100.0), \
            f"5rem should be 100px at 20px root, got {d.box.content_width}"

    def test_viewport_units(self):
        doc = render('''
        <html><head><style>
          body { margin: 0; }
          #d { width: 50vw; height: 25vh; }
        </style></head>
        <body><div id="d">x</div></body></html>
        ''', viewport_width=1000, viewport_height=800)
        d = find_element(doc, id_name='d')
        assert approx(d.box.content_width, 500.0), \
            f"50vw at 1000 should be 500, got {d.box.content_width}"
        assert approx(d.box.content_height, 200.0), \
            f"25vh at 800 should be 200, got {d.box.content_height}"


class TestCSSVariables:
    """CSS custom properties (var()) resolution."""

    def test_var_basic(self):
        doc = render('''
        <html><head><style>
          :root { --main-width: 300px; }
          #d { width: var(--main-width); height: 10px; }
        </style></head>
        <body style="margin:0"><div id="d">x</div></body></html>
        ''')
        d = find_element(doc, id_name='d')
        assert approx(d.box.content_width, 300.0), \
            f"var(--main-width) should resolve to 300, got {d.box.content_width}"

    def test_var_with_fallback(self):
        doc = render('''
        <html><head><style>
          #d { width: var(--undefined, 200px); height: 10px; }
        </style></head>
        <body style="margin:0"><div id="d">x</div></body></html>
        ''')
        d = find_element(doc, id_name='d')
        assert approx(d.box.content_width, 200.0), \
            f"var() fallback should be 200, got {d.box.content_width}"

    def test_var_inheritance(self):
        """Custom properties inherit to children."""
        doc = render('''
        <html><head><style>
          .parent { --size: 250px; }
          #child { width: var(--size); height: 10px; }
        </style></head>
        <body style="margin:0">
          <div class="parent">
            <div id="child">x</div>
          </div>
        </body></html>
        ''')
        child = find_element(doc, id_name='child')
        assert approx(child.box.content_width, 250.0), \
            f"var() should inherit from parent, got {child.box.content_width}"


class TestMultipleSelectors:
    """Comma-separated selectors."""

    def test_comma_selectors(self):
        doc = render('''
        <html><head><style>
          body { margin: 0; }
          h1, h2, .title { color: navy; }
        </style></head>
        <body>
          <h1 id="h1" style="margin:0">Title</h1>
          <h2 id="h2" style="margin:0">Subtitle</h2>
          <div class="title" id="t" style="height:10px">Other</div>
        </body></html>
        ''')
        for eid in ('h1', 'h2', 't'):
            el = find_element(doc, id_name=eid)
            assert el.style.get('color') == 'navy', \
                f"{eid} should have color=navy, got {el.style.get('color')}"


class TestDescendantSelectors:
    """Descendant and child combinators."""

    def test_descendant_selector(self):
        doc = render('''
        <html><head><style>
          body { margin: 0; }
          .container .item { color: green; }
        </style></head>
        <body>
          <div class="container">
            <div>
              <span class="item" id="s">deep</span>
            </div>
          </div>
          <span class="item" id="outside">outside</span>
        </body></html>
        ''')
        inside = find_element(doc, id_name='s')
        outside = find_element(doc, id_name='outside')
        assert inside.style.get('color') == 'green', \
            f"Descendant should match, got {inside.style.get('color')}"
        assert outside.style.get('color') != 'green', \
            f"Outside element should not match descendant selector"

    def test_child_selector(self):
        doc = render('''
        <html><head><style>
          body { margin: 0; }
          .parent > .child { color: blue; }
        </style></head>
        <body>
          <div class="parent">
            <div class="child" id="direct">direct</div>
            <div>
              <div class="child" id="nested">nested</div>
            </div>
          </div>
        </body></html>
        ''')
        direct = find_element(doc, id_name='direct')
        nested = find_element(doc, id_name='nested')
        assert direct.style.get('color') == 'blue', \
            f"Direct child should match, got {direct.style.get('color')}"
        assert nested.style.get('color') != 'blue', \
            f"Nested should not match child selector"


class TestPseudoClasses:
    """Structural pseudo-classes."""

    def test_first_child(self):
        doc = render('''
        <html><head><style>
          body { margin: 0; }
          li:first-child { color: red; }
        </style></head>
        <body>
          <ul style="margin:0; padding:0">
            <li id="first">First</li>
            <li id="second">Second</li>
          </ul>
        </body></html>
        ''')
        first = find_element(doc, id_name='first')
        second = find_element(doc, id_name='second')
        assert first.style.get('color') == 'red'
        assert second.style.get('color') != 'red'

    def test_last_child(self):
        doc = render('''
        <html><head><style>
          body { margin: 0; }
          li:last-child { color: blue; }
        </style></head>
        <body>
          <ul style="margin:0; padding:0">
            <li id="first">First</li>
            <li id="last">Last</li>
          </ul>
        </body></html>
        ''')
        first = find_element(doc, id_name='first')
        last = find_element(doc, id_name='last')
        assert first.style.get('color') != 'blue'
        assert last.style.get('color') == 'blue'


class TestMediaQueries:
    """@media queries filter rules based on viewport."""

    def test_min_width_match(self):
        doc = render('''
        <html><head><style>
          body { margin: 0; }
          #d { width: 100px; height: 10px; }
          @media (min-width: 600px) {
            #d { width: 300px; }
          }
        </style></head>
        <body><div id="d">x</div></body></html>
        ''', viewport_width=800)
        d = find_element(doc, id_name='d')
        assert approx(d.box.content_width, 300.0), \
            f"@media should apply at 800px viewport, got {d.box.content_width}"

    def test_min_width_no_match(self):
        doc = render('''
        <html><head><style>
          body { margin: 0; }
          #d { width: 100px; height: 10px; }
          @media (min-width: 1200px) {
            #d { width: 300px; }
          }
        </style></head>
        <body><div id="d">x</div></body></html>
        ''', viewport_width=800)
        d = find_element(doc, id_name='d')
        assert approx(d.box.content_width, 100.0), \
            f"@media should NOT apply at 800px, got {d.box.content_width}"

    def test_max_width(self):
        doc = render('''
        <html><head><style>
          body { margin: 0; }
          #d { width: 500px; height: 10px; }
          @media (max-width: 480px) {
            #d { width: 100%; }
          }
        </style></head>
        <body><div id="d">x</div></body></html>
        ''', viewport_width=400)
        d = find_element(doc, id_name='d')
        assert approx(d.box.content_width, 400.0), \
            f"@media max-width should apply, got {d.box.content_width}"
