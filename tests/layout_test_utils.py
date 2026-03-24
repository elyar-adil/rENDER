from __future__ import annotations

import os
from pathlib import Path

from html.dom import Element
from html.parser import parse as parse_html
from css.cascade import bind as css_bind
from css.computed import compute as css_compute
import layout as layout_mod

ROOT = Path(__file__).resolve().parents[1]
UA_CSS = os.path.join(ROOT, "ua", "user-agent.css")


def render_document(html: str, *, viewport_w: int = 1280, viewport_h: int = 720):
    """Render HTML into a DOM tree with style and layout boxes attached."""
    document = parse_html(html)
    css_bind(
        document,
        UA_CSS,
        viewport_width=viewport_w,
        viewport_height=viewport_h,
        extra_css_texts=[],
        base_url="",
    )
    css_compute(document, viewport_width=viewport_w, viewport_height=viewport_h)
    layout_mod.layout(document, viewport_width=viewport_w, viewport_height=viewport_h)
    return document


def iter_elements(node):
    """Yield DOM elements in document order."""
    if isinstance(node, Element):
        yield node
    for child in getattr(node, "children", []):
        yield from iter_elements(child)


def _matches_selector(element: Element, selector: str) -> bool:
    if selector.startswith("#"):
        return element.attributes.get("id") == selector[1:]
    if selector.startswith("."):
        classes = element.attributes.get("class", "").split()
        return selector[1:] in classes
    return element.tag == selector


def require_element(document, selector: str) -> Element:
    """Find one element by simple selector (#id/.class/tag) or raise."""
    for element in iter_elements(document):
        if _matches_selector(element, selector):
            return element
    raise AssertionError(f"Missing element for selector: {selector}")
