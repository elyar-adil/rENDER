"""Tests for CSS properties and features that are missing or not implemented.

These tests document gaps between what real browsers support and what rENDER
currently handles. Many tests are expected to fail (xfail) for unimplemented
features; others reveal outright bugs.
"""
import sys
import os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import pytest
from html.dom import Document, Element, Text
from css.properties import PROPERTIES, expand_shorthand
import css.selector as selector_mod
import css.parser as css_parser


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def make_element(tag='div', attrs=None, style=None, parent=None):
    el = Element(tag)
    el.attributes = attrs or {}
    el.style = style or {}
    if parent is not None:
        el.parent = parent
        parent.children.append(el)
    return el


def bind_css(css_text, body_builder=None, vw=980, vh=600):
    """Build a Document, bind CSS, return (doc, body_el)."""
    from css.cascade import bind
    import os as _os
    ua = _os.path.join(_os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))),
                       'ua', 'user-agent.css')
    doc = Document()
    html_el = make_element('html')
    html_el.parent = doc
    doc.children.append(html_el)
    head = make_element('head', parent=html_el)
    style_el = make_element('style', parent=head)
    style_el.style = {}
    txt = Text(css_text)
    txt.parent = style_el
    style_el.children.append(txt)
    body = make_element('body', parent=html_el)
    body.style = {}
    if body_builder:
        body_builder(body)
    bind(doc, ua, viewport_width=vw, viewport_height=vh)
    return doc, body


# ===========================================================================
# 1. CSS PROPERTY REGISTRY GAPS
#    Properties that real browsers support but are absent from PROPERTIES dict
# ===========================================================================

class TestMissingPropertyDefinitions:
    """Properties missing from PROPERTIES (no initial value or inheritance info)."""

    def test_object_fit_in_properties(self):
        assert 'object-fit' in PROPERTIES

    def test_object_position_in_properties(self):
        assert 'object-position' in PROPERTIES

    def test_aspect_ratio_in_properties(self):
        assert 'aspect-ratio' in PROPERTIES

    def test_filter_in_properties(self):
        assert 'filter' in PROPERTIES

    def test_backdrop_filter_in_properties(self):
        assert 'backdrop-filter' in PROPERTIES

    def test_clip_path_in_properties(self):
        assert 'clip-path' in PROPERTIES

    def test_mask_in_properties(self):
        assert 'mask' in PROPERTIES

    def test_writing_mode_in_properties(self):
        assert 'writing-mode' in PROPERTIES

    def test_direction_in_properties(self):
        assert 'direction' in PROPERTIES

    def test_unicode_bidi_in_properties(self):
        assert 'unicode-bidi' in PROPERTIES

    def test_contain_in_properties(self):
        assert 'contain' in PROPERTIES

    def test_isolation_in_properties(self):
        assert 'isolation' in PROPERTIES

    def test_counter_reset_in_properties(self):
        assert 'counter-reset' in PROPERTIES

    def test_counter_increment_in_properties(self):
        assert 'counter-increment' in PROPERTIES

    def test_quotes_in_properties(self):
        assert 'quotes' in PROPERTIES

    def test_list_style_position_in_properties(self):
        assert 'list-style-position' in PROPERTIES

    def test_list_style_image_in_properties(self):
        assert 'list-style-image' in PROPERTIES

    def test_column_count_in_properties(self):
        assert 'column-count' in PROPERTIES

    def test_column_width_in_properties(self):
        assert 'column-width' in PROPERTIES

    def test_font_variant_in_properties(self):
        assert 'font-variant' in PROPERTIES

    def test_font_feature_settings_in_properties(self):
        assert 'font-feature-settings' in PROPERTIES

    def test_shape_outside_in_properties(self):
        assert 'shape-outside' in PROPERTIES

    def test_user_select_in_properties(self):
        assert 'user-select' in PROPERTIES

    def test_appearance_in_properties(self):
        assert 'appearance' in PROPERTIES

    def test_resize_in_properties(self):
        assert 'resize' in PROPERTIES

    def test_scroll_behavior_in_properties(self):
        assert 'scroll-behavior' in PROPERTIES

    def test_overscroll_behavior_in_properties(self):
        assert 'overscroll-behavior' in PROPERTIES

    def test_scroll_snap_type_in_properties(self):
        assert 'scroll-snap-type' in PROPERTIES

    def test_touch_action_in_properties(self):
        assert 'touch-action' in PROPERTIES

    def test_image_rendering_in_properties(self):
        assert 'image-rendering' in PROPERTIES


# ===========================================================================
# 2. CSS SHORTHAND EXPANSION GAPS
# ===========================================================================

class TestShorthandGaps:

    @pytest.mark.xfail(reason="grid shorthand not expanded")
    def test_grid_shorthand_expanded(self):
        result = expand_shorthand('grid', '100px / 1fr 1fr')
        assert 'grid-template-rows' in result
        assert 'grid-template-columns' in result

    def test_list_style_shorthand_expanded_to_position(self):
        result = expand_shorthand('list-style', 'disc inside')
        assert 'list-style-type' in result
        assert 'list-style-position' in result

    @pytest.mark.xfail(reason="transition shorthand not parsed")
    def test_transition_shorthand_expanded(self):
        result = expand_shorthand('transition', 'opacity 0.3s ease')
        # Should produce transition-property, transition-duration, etc.
        assert len(result) > 1

    @pytest.mark.xfail(reason="animation shorthand not parsed")
    def test_animation_shorthand_expanded(self):
        result = expand_shorthand('animation', 'spin 1s linear infinite')
        assert len(result) > 1

    def test_flex_none_expanded(self):
        """flex: none → flex-grow:0, flex-shrink:0, flex-basis:auto."""
        result = expand_shorthand('flex', 'none')
        assert result.get('flex-grow') == '0'
        assert result.get('flex-shrink') == '0'
        assert result.get('flex-basis') == 'auto'

    def test_flex_auto_expanded(self):
        """flex: auto → flex-grow:1, flex-shrink:1, flex-basis:auto."""
        result = expand_shorthand('flex', 'auto')
        assert result.get('flex-grow') == '1'
        assert result.get('flex-shrink') == '1'
        assert result.get('flex-basis') == 'auto'


# ===========================================================================
# 3. CSS CASCADE: UNKNOWN PROPERTIES PASS-THROUGH
# ===========================================================================

class TestUnknownPropertyCascade:
    """Unknown CSS properties should still be stored on element.style
    so that future features can read them without a parser change."""

    def test_object_fit_stored_in_style(self):
        """object-fit declared in CSS should end up in element.style."""
        _, body = bind_css(
            'img { object-fit: cover; }',
            lambda b: make_element('img', parent=b),
        )
        img = body.children[0]
        # This will fail until object-fit is stored/cascaded
        assert img.style.get('object-fit') == 'cover', (
            f"object-fit not found in style; got keys: {list(img.style.keys())}"
        )

    def test_aspect_ratio_stored_in_style(self):
        """aspect-ratio declared in CSS should end up in element.style."""
        _, body = bind_css(
            'div { aspect-ratio: 16 / 9; }',
            lambda b: make_element('div', parent=b),
        )
        div = body.children[0]
        assert div.style.get('aspect-ratio') == '16 / 9', (
            f"aspect-ratio not found; style keys: {list(div.style.keys())}"
        )

    def test_filter_stored_in_style(self):
        """filter: blur() should be stored in element.style."""
        _, body = bind_css(
            '.blurred { filter: blur(4px); }',
            lambda b: make_element('div', attrs={'class': 'blurred'}, parent=b),
        )
        div = body.children[0]
        assert div.style.get('filter') == 'blur(4px)', (
            f"filter not stored; style keys: {list(div.style.keys())}"
        )


# ===========================================================================
# 4. CSS SELECTOR GAPS
# ===========================================================================

class TestSelectorGaps:

    def test_basic_class_selector(self):
        el = make_element('div', attrs={'class': 'foo'})
        assert selector_mod.matches(el, '.foo')

    def test_basic_id_selector(self):
        el = make_element('div', attrs={'id': 'main'})
        assert selector_mod.matches(el, '#main')

    def test_attribute_exists(self):
        el = make_element('input', attrs={'disabled': ''})
        assert selector_mod.matches(el, '[disabled]')

    def test_attribute_exact(self):
        el = make_element('input', attrs={'type': 'submit'})
        assert selector_mod.matches(el, '[type="submit"]')

    def test_attribute_contains(self):
        el = make_element('div', attrs={'class': 'foo bar baz'})
        assert selector_mod.matches(el, '[class*="bar"]')

    def test_attribute_starts_with(self):
        el = make_element('a', attrs={'href': 'https://example.com'})
        assert selector_mod.matches(el, '[href^="https"]')

    def test_attribute_ends_with(self):
        el = make_element('a', attrs={'href': '/page.html'})
        assert selector_mod.matches(el, '[href$=".html"]')

    def test_not_selector_simple(self):
        el = make_element('p', attrs={'class': 'note'})
        assert selector_mod.matches(el, ':not(.highlight)')

    def test_nth_child_odd(self):
        parent = make_element('ul')
        li1 = make_element('li', parent=parent)
        li2 = make_element('li', parent=parent)
        li3 = make_element('li', parent=parent)
        assert selector_mod.matches(li1, ':nth-child(odd)')
        assert not selector_mod.matches(li2, ':nth-child(odd)')
        assert selector_mod.matches(li3, ':nth-child(odd)')

    def test_nth_child_even(self):
        parent = make_element('ul')
        li1 = make_element('li', parent=parent)
        li2 = make_element('li', parent=parent)
        assert not selector_mod.matches(li1, ':nth-child(even)')
        assert selector_mod.matches(li2, ':nth-child(even)')

    def test_nth_child_formula(self):
        parent = make_element('ul')
        children = [make_element('li', parent=parent) for _ in range(6)]
        # 3n matches 3rd, 6th
        assert not selector_mod.matches(children[0], ':nth-child(3n)')
        assert not selector_mod.matches(children[1], ':nth-child(3n)')
        assert selector_mod.matches(children[2], ':nth-child(3n)')
        assert selector_mod.matches(children[5], ':nth-child(3n)')

    def test_first_of_type(self):
        parent = make_element('div')
        p1 = make_element('p', parent=parent)
        span = make_element('span', parent=parent)
        p2 = make_element('p', parent=parent)
        assert selector_mod.matches(p1, 'p:first-of-type')
        assert not selector_mod.matches(p2, 'p:first-of-type')

    def test_last_of_type(self):
        parent = make_element('div')
        p1 = make_element('p', parent=parent)
        p2 = make_element('p', parent=parent)
        assert not selector_mod.matches(p1, 'p:last-of-type')
        assert selector_mod.matches(p2, 'p:last-of-type')

    def test_only_child(self):
        parent = make_element('div')
        child = make_element('p', parent=parent)
        assert selector_mod.matches(child, ':only-child')
        make_element('span', parent=parent)
        assert not selector_mod.matches(child, ':only-child')

    def test_empty_pseudo(self):
        el = make_element('div')
        assert selector_mod.matches(el, ':empty')
        txt = Text('hello')
        txt.parent = el
        el.children.append(txt)
        assert not selector_mod.matches(el, ':empty')

    def test_adjacent_sibling(self):
        parent = make_element('div')
        h1 = make_element('h1', parent=parent)
        p = make_element('p', parent=parent)
        assert selector_mod.matches(p, 'h1 + p')
        assert not selector_mod.matches(h1, 'h1 + p')

    def test_general_sibling(self):
        parent = make_element('div')
        h1 = make_element('h1', parent=parent)
        span = make_element('span', parent=parent)
        p = make_element('p', parent=parent)
        assert selector_mod.matches(p, 'h1 ~ p')
        assert not selector_mod.matches(h1, 'h1 ~ p')

    def test_is_pseudo(self):
        el = make_element('h2', attrs={'class': 'title'})
        assert selector_mod.matches(el, ':is(h1, h2, h3)')

    def test_where_pseudo(self):
        el = make_element('button', attrs={'class': 'btn'})
        assert selector_mod.matches(el, ':where(button, a)')

    def test_has_pseudo(self):
        parent = make_element('div')
        child = make_element('img', parent=parent)
        assert selector_mod.matches(parent, 'div:has(img)')

    def test_has_pseudo_descendant(self):
        parent = make_element('article')
        make_element('h2', parent=parent)
        assert selector_mod.matches(parent, 'article:has(h2)')

    def test_not_multiple_args_second_arg_checked(self):
        """An element with .bar (but not .foo) should NOT match :not(.foo, .bar)
        because it has one of the excluded classes.  Currently only the first
        argument (.foo) is checked, so .bar elements incorrectly pass."""
        el = make_element('span', attrs={'class': 'bar'})
        # Should be False: element has .bar which is excluded
        assert not selector_mod.matches(el, ':not(.foo, .bar)')

    def test_not_multiple_args_no_class_matches(self):
        """Element with no matching classes should match :not(.foo, .bar)."""
        el = make_element('span')
        assert selector_mod.matches(el, ':not(.foo, .bar)')

    @pytest.mark.xfail(reason=":nth-child(An+B of selector) not implemented")
    def test_nth_child_of_selector(self):
        parent = make_element('ul')
        make_element('li', attrs={'class': 'a'}, parent=parent)
        make_element('li', attrs={'class': 'b'}, parent=parent)
        make_element('li', attrs={'class': 'a'}, parent=parent)
        children = parent.children
        # :nth-child(2 of .a) should match second .a li
        assert selector_mod.matches(children[2], ':nth-child(2 of .a)')

    def test_placeholder_pseudo_element(self):
        """::placeholder is recognized as a pseudo-element by the selector parser."""
        el = make_element('input', attrs={'type': 'text', 'placeholder': 'Search...'})
        assert selector_mod.get_pseudo_element('input::placeholder') == 'placeholder'

    @pytest.mark.xfail(reason=":placeholder-shown pseudo-class not implemented")
    def test_placeholder_shown_pseudo_class(self):
        el = make_element('input', attrs={'type': 'text', 'placeholder': 'hint'})
        assert selector_mod.matches(el, ':placeholder-shown')

    @pytest.mark.xfail(reason=":focus-within pseudo-class not implemented (always False)")
    def test_focus_within_pseudo_class(self):
        parent = make_element('form')
        inp = make_element('input', parent=parent)
        # :focus-within should match parent when child has focus
        assert selector_mod.matches(parent, ':focus-within')

    def test_hover_always_false(self):
        """hover always returns False in static rendering – this is expected."""
        el = make_element('a', attrs={'href': '#'})
        assert not selector_mod.matches(el, ':hover')

    def test_link_pseudo_class(self):
        el = make_element('a', attrs={'href': 'http://example.com'})
        assert selector_mod.matches(el, ':link')

    def test_checked_on_checked_input(self):
        el = make_element('input', attrs={'type': 'checkbox', 'checked': ''})
        assert selector_mod.matches(el, ':checked')

    def test_disabled_on_disabled_input(self):
        el = make_element('input', attrs={'type': 'text', 'disabled': ''})
        assert selector_mod.matches(el, ':disabled')

    def test_enabled_on_enabled_input(self):
        el = make_element('input', attrs={'type': 'text'})
        assert selector_mod.matches(el, ':enabled')


# ===========================================================================
# 5. CSS COMPUTED VALUE GAPS
#    Units and functions that are not resolved
# ===========================================================================

class TestComputedValueGaps:

    def test_px_unit_resolved(self):
        """Basic px unit must be resolved in computed.py."""
        from css.computed import _resolve_length
        result = _resolve_length('16px', 16, 16, 980, 600)
        assert result == 16.0

    def test_em_unit_resolved(self):
        from css.computed import _resolve_length
        result = _resolve_length('1.5em', 16, 16, 980, 600)
        assert result == 24.0

    def test_rem_unit_resolved(self):
        from css.computed import _resolve_length
        result = _resolve_length('2rem', 16, 16, 980, 600)
        assert result == 32.0

    def test_vw_unit_resolved(self):
        from css.computed import _resolve_length
        result = _resolve_length('50vw', 16, 16, 980, 600)
        assert result == 490.0

    def test_vh_unit_resolved(self):
        from css.computed import _resolve_length
        result = _resolve_length('100vh', 16, 16, 980, 600)
        assert result == 600.0

    def test_pt_unit_resolved(self):
        from css.computed import _resolve_length
        result = _resolve_length('12pt', 16, 16, 980, 600)
        # 12pt = 12 * 96/72 = 16px
        assert abs(result - 16.0) < 0.1

    def test_cm_unit_resolved(self):
        from css.computed import _resolve_length
        result = _resolve_length('1cm', 16, 16, 980, 600)
        # 1cm = 96/2.54 ≈ 37.8px
        assert abs(result - 37.8) < 0.1

    def test_auto_returns_none(self):
        from css.computed import _resolve_length
        assert _resolve_length('auto', 16, 16, 980, 600) is None

    def test_percent_without_context_returns_none(self):
        """% lengths (non-font-size) can't be resolved without container reference."""
        from css.computed import _resolve_length
        assert _resolve_length('50%', 16, 16, 980, 600) is None

    def test_min_function(self):
        from css.computed import _resolve_length
        result = _resolve_length('min(50px, 100px)', 16, 16, 980, 600)
        assert result == 50.0

    def test_max_function(self):
        from css.computed import _resolve_length
        result = _resolve_length('max(20px, 10px)', 16, 16, 980, 600)
        assert result == 20.0

    def test_clamp_function(self):
        from css.computed import _resolve_length
        result = _resolve_length('clamp(10px, 5vw, 100px)', 16, 16, 1000, 600)
        assert result == 50.0  # 5vw = 50px, between 10 and 100

    @pytest.mark.xfail(reason="env() CSS function not supported")
    def test_env_function(self):
        from css.computed import _resolve_length
        result = _resolve_length('env(safe-area-inset-top, 20px)', 16, 16, 980, 600)
        assert result == 20.0  # should use fallback

    def test_dvh_unit(self):
        from css.computed import _resolve_length
        result = _resolve_length('100dvh', 16, 16, 980, 600)
        assert result == 600.0


# ===========================================================================
# 6. CSS @RULE GAPS
# ===========================================================================

class TestAtRuleGaps:

    def test_at_media_min_width_applied(self):
        """@media (min-width) should apply styles at matching viewport."""
        _, body = bind_css(
            '@media (min-width: 600px) { .box { color: red; } }',
            lambda b: make_element('div', attrs={'class': 'box'}, parent=b),
            vw=980,
        )
        div = body.children[0]
        assert div.style.get('color') == 'red'

    def test_at_media_min_width_not_applied_below(self):
        """@media rule should NOT apply when viewport is smaller."""
        _, body = bind_css(
            '@media (min-width: 1200px) { .box { color: red; } }',
            lambda b: make_element('div', attrs={'class': 'box'}, parent=b),
            vw=980,
        )
        div = body.children[0]
        assert div.style.get('color', '') != 'red'

    def test_at_supports_property(self):
        """@supports (display: grid) should be honoured."""
        _, body = bind_css(
            '@supports (display: grid) { .box { color: green; } }',
            lambda b: make_element('div', attrs={'class': 'box'}, parent=b),
        )
        div = body.children[0]
        assert div.style.get('color') == 'green'

    def test_at_layer_base(self):
        """@layer should establish a cascade layer."""
        _, body = bind_css(
            '@layer base { .box { color: blue; } }',
            lambda b: make_element('div', attrs={'class': 'box'}, parent=b),
        )
        div = body.children[0]
        assert div.style.get('color') == 'blue'

    @pytest.mark.xfail(reason="@container rule not implemented; container queries ignored")
    def test_at_container_query(self):
        """@container queries are not applied."""
        def build(b):
            wrapper = make_element('div', attrs={'class': 'wrapper'}, parent=b)
            make_element('div', attrs={'class': 'box'}, parent=wrapper)
        _, body = bind_css(
            '.wrapper { container-type: inline-size; }'
            '@container (min-width: 400px) { .box { color: purple; } }',
            build,
        )
        wrapper = body.children[0]
        box_el = wrapper.children[0]
        # container queries should apply color:purple at this viewport width
        assert box_el.style.get('color') == 'purple'

    def test_at_keyframes_parsed(self):
        """@keyframes blocks should be parsed without error and accessible."""
        sheet = css_parser.parse_stylesheet(
            '@keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }'
        )
        keyframes = [r for r in sheet.rules if hasattr(r, 'name') and 'spin' in str(getattr(r, 'name', ''))]
        assert len(keyframes) == 1


# ===========================================================================
# 7. CSS COLOR FUNCTION GAPS
# ===========================================================================

class TestColorFunctionGaps:

    def test_hex_color_applied(self):
        """Basic hex color should be stored."""
        _, body = bind_css(
            'p { color: #ff0000; }',
            lambda b: make_element('p', parent=b),
        )
        assert body.children[0].style.get('color') == '#ff0000'

    def test_rgb_comma_syntax(self):
        """rgb(r, g, b) should be stored."""
        _, body = bind_css(
            'p { color: rgb(255, 0, 0); }',
            lambda b: make_element('p', parent=b),
        )
        assert 'rgb' in body.children[0].style.get('color', '')

    def test_rgba_stored(self):
        _, body = bind_css(
            'p { color: rgba(0, 0, 255, 0.5); }',
            lambda b: make_element('p', parent=b),
        )
        assert 'rgba' in body.children[0].style.get('color', '')

    def test_hsl_stored(self):
        _, body = bind_css(
            'p { color: hsl(120, 50%, 50%); }',
            lambda b: make_element('p', parent=b),
        )
        assert 'hsl' in body.children[0].style.get('color', '')

    def test_hsl_space_separated_modern_syntax_stored(self):
        """CSS Color Level 4 hsl() without commas is stored in style (not filtered).
        Rendering may not interpret it correctly."""
        _, body = bind_css(
            'p { color: hsl(120 50% 50%); }',
            lambda b: make_element('p', parent=b),
        )
        assert 'hsl' in body.children[0].style.get('color', '')

    def test_rgb_space_separated_modern_syntax_stored(self):
        """CSS Color Level 4 rgb() without commas is stored in style.
        Rendering may not interpret it correctly."""
        _, body = bind_css(
            'p { color: rgb(255 0 0 / 0.5); }',
            lambda b: make_element('p', parent=b),
        )
        assert 'rgb' in body.children[0].style.get('color', '')

    def test_oklch_color_stored(self):
        """oklch() color values are stored as-is; rendering falls back to black."""
        _, body = bind_css(
            'p { color: oklch(60% 0.15 200); }',
            lambda b: make_element('p', parent=b),
        )
        assert 'oklch' in body.children[0].style.get('color', '')

    def test_color_mix_stored(self):
        """color-mix() values are stored; actual color mixing not implemented."""
        _, body = bind_css(
            'p { color: color-mix(in srgb, red 50%, blue 50%); }',
            lambda b: make_element('p', parent=b),
        )
        assert body.children[0].style.get('color') is not None


# ===========================================================================
# 8. CSS CUSTOM PROPERTIES (var()) GAPS
# ===========================================================================

class TestCSSCustomProperties:

    def test_var_resolved_from_same_element(self):
        """A CSS variable defined and used on the same element."""
        _, body = bind_css(
            ':root { --brand: blue; } p { color: var(--brand); }',
            lambda b: make_element('p', parent=b),
        )
        p = body.children[0]
        # var() should be resolved to 'blue'
        assert p.style.get('color') == 'blue'

    def test_var_with_fallback_when_undefined(self):
        """var(--undefined, fallback) should use the fallback."""
        _, body = bind_css(
            'p { color: var(--no-such-var, green); }',
            lambda b: make_element('p', parent=b),
        )
        p = body.children[0]
        assert p.style.get('color') == 'green'

    def test_var_inherited_from_parent(self):
        """CSS custom properties are inherited."""
        def build(b):
            parent = make_element('div', parent=b)
            make_element('span', parent=parent)
        _, body = bind_css(
            'div { --text-color: red; } span { color: var(--text-color); }',
            build,
        )
        span = body.children[0].children[0]
        assert span.style.get('color') == 'red'

    def test_nested_var(self):
        """var(--a) where --a itself contains var(--b) – resolved transitively."""
        _, body = bind_css(
            ':root { --base: 16px; --size: var(--base); } p { font-size: var(--size); }',
            lambda b: make_element('p', parent=b),
        )
        p = body.children[0]
        assert p.style.get('font-size') == '16px'

    @pytest.mark.xfail(reason="var() inside shorthand properties not resolved")
    def test_var_in_border_shorthand(self):
        """var() inside a border shorthand value."""
        _, body = bind_css(
            ':root { --bw: 2px; } div { border: var(--bw) solid black; }',
            lambda b: make_element('div', parent=b),
        )
        div = body.children[0]
        assert div.style.get('border-top-width') == '2px'


# ===========================================================================
# 9. CSS BACKGROUND GAPS
# ===========================================================================

class TestBackgroundGaps:

    def test_background_color_applied(self):
        _, body = bind_css(
            'div { background-color: yellow; }',
            lambda b: make_element('div', parent=b),
        )
        assert body.children[0].style.get('background-color') == 'yellow'

    def test_background_shorthand_color(self):
        _, body = bind_css(
            'div { background: red; }',
            lambda b: make_element('div', parent=b),
        )
        div = body.children[0]
        assert div.style.get('background-color') == 'red'

    def test_background_shorthand_with_image(self):
        _, body = bind_css(
            'div { background: url(img.png) no-repeat center; }',
            lambda b: make_element('div', parent=b),
        )
        div = body.children[0]
        assert 'url' in div.style.get('background-image', '')

    def test_background_size_cover_stored(self):
        """background-size: cover is stored in the style dict.
        Note: actual image scaling is not implemented in the renderer."""
        _, body = bind_css(
            'div { background-size: cover; }',
            lambda b: make_element('div', parent=b),
        )
        div = body.children[0]
        assert div.style.get('background-size') == 'cover'

    @pytest.mark.xfail(reason="multiple backgrounds not supported")
    def test_multiple_backgrounds(self):
        """Multiple background layers separated by commas."""
        _, body = bind_css(
            'div { background: url(a.png), url(b.png) no-repeat; }',
            lambda b: make_element('div', parent=b),
        )
        div = body.children[0]
        # Should store both background layers
        bg = div.style.get('background-image', '')
        assert 'a.png' in bg and 'b.png' in bg


# ===========================================================================
# 10. FLEX ORDER PROPERTY
# ===========================================================================

class TestFlexOrder:

    def test_flex_order_reorders_children(self):
        """Children with order: -1 should appear before order: 0."""
        from html.dom import Element
        from layout.box import BoxModel
        from layout.flex import FlexLayout
        from layout.context import LayoutContext

        container = Element('div')
        container.style = {
            'display': 'flex', 'flex-direction': 'row',
            'width': '300px', 'height': 'auto',
        }
        child1 = make_element('div', style={
            'display': 'block', 'width': '100px', 'height': '50px',
            'order': '2',
        })
        child2 = make_element('div', style={
            'display': 'block', 'width': '100px', 'height': '50px',
            'order': '-1',
        })
        child1.parent = container
        child2.parent = container
        container.children = [child1, child2]

        c = BoxModel()
        c.x = 0.0
        c.y = 0.0
        c.content_width = 300.0
        c.content_height = 0.0
        FlexLayout().layout(container, c, LayoutContext())

        # child2 (order:-1) should be to the left of child1 (order:2)
        assert child2.box.x < child1.box.x

    @pytest.mark.xfail(reason="align-self not overriding align-items in flex")
    def test_flex_align_self_override(self):
        """align-self: flex-start on one child should override align-items: center."""
        from html.dom import Element
        from layout.box import BoxModel
        from layout.flex import FlexLayout
        from layout.context import LayoutContext

        container = Element('div')
        container.style = {
            'display': 'flex', 'flex-direction': 'row',
            'align-items': 'center',
            'width': '300px', 'height': '200px',
        }
        child_start = make_element('div', style={
            'display': 'block', 'width': '50px', 'height': '50px',
            'align-self': 'flex-start',
        })
        child_center = make_element('div', style={
            'display': 'block', 'width': '50px', 'height': '50px',
        })
        child_start.parent = container
        child_center.parent = container
        container.children = [child_start, child_center]

        c = BoxModel()
        c.x = 0.0; c.y = 0.0
        c.content_width = 300.0; c.content_height = 200.0
        FlexLayout().layout(container, c, LayoutContext())

        # child with align-self: flex-start → y == container.y
        assert child_start.box.y == 0.0
        # child without align-self inherits center → y > 0
        assert child_center.box.y > 0.0
