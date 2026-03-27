"""Helper to run the full rendering pipeline for integration tests.

Usage:
    from tests.render_helper import render, find_element, find_all

    doc = render('<div style="width:100px">hello</div>')
    div = find_element(doc, 'div')
    assert div.box.content_width == 100.0
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')

try:
    from PyQt6.QtWidgets import QApplication
    _app = QApplication.instance() or QApplication([])
except ImportError:
    _app = None

from html.parser import parse as parse_html
from html.dom import Element, Text, Document
from css.cascade import bind as css_bind
from css.computed import compute as css_compute
import layout as layout_mod

UA_CSS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                      'ua', 'user-agent.css')

DEFAULT_VP_W = 800
DEFAULT_VP_H = 600


def render(html_str: str, viewport_width: int = DEFAULT_VP_W,
           viewport_height: int = DEFAULT_VP_H) -> Document:
    """Run full pipeline: parse → cascade → compute → layout.

    Returns the Document with .box set on all visible elements.
    """
    doc = parse_html(html_str)
    css_bind(doc, UA_CSS,
             viewport_width=viewport_width,
             viewport_height=viewport_height)
    css_compute(doc, viewport_width=viewport_width,
                viewport_height=viewport_height)
    display_list = layout_mod.layout(doc, viewport_width, viewport_height)
    doc._display_list = display_list
    return doc


def find_element(node, tag: str = None, class_name: str = None,
                 id_name: str = None, nth: int = 0) -> Element:
    """Find the nth element matching tag/class/id. Raises if not found."""
    results = find_all(node, tag=tag, class_name=class_name, id_name=id_name)
    if nth >= len(results):
        raise ValueError(
            f"Element not found: tag={tag}, class={class_name}, id={id_name}, "
            f"nth={nth} (found {len(results)})")
    return results[nth]


def find_all(node, tag: str = None, class_name: str = None,
             id_name: str = None) -> list:
    """Find all elements matching tag/class/id."""
    results = []
    _walk(node, tag, class_name, id_name, results)
    return results


def _walk(node, tag, class_name, id_name, results):
    if isinstance(node, Element):
        match = True
        if tag and node.tag != tag:
            match = False
        if class_name and class_name not in node.attributes.get('class', '').split():
            match = False
        if id_name and node.attributes.get('id', '') != id_name:
            match = False
        if match and (tag or class_name or id_name):
            results.append(node)
    for child in getattr(node, 'children', []):
        _walk(child, tag, class_name, id_name, results)


def get_display_list(doc) -> list:
    """Return the display list commands as a list."""
    return list(doc._display_list) if hasattr(doc, '_display_list') else []


def box_rect(el):
    """Return (x, y, width, height) of element's content box."""
    b = el.box
    return (b.x, b.y, b.content_width, b.content_height)


def border_rect(el):
    """Return (x, y, width, height) of element's border box."""
    r = el.box.border_rect
    return (r.x, r.y, r.width, r.height)


def approx(val, expected, tolerance=2.0):
    """Check value is within tolerance of expected."""
    return abs(val - expected) <= tolerance
