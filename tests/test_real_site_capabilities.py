"""Offline acceptance checks for common page shapes from Baidu and Zhihu.

The fixture names identify the source scenario only. Assertions are expressed
in terms of HTML semantics, resource kinds, and layout behavior so the engine
does not need a host-specific rendering path.
"""

from __future__ import annotations

import base64
import json
from pathlib import Path
from unittest.mock import patch
from urllib.parse import urljoin, urlsplit

import pytest

import engine
from html.dom import Element, Text
from html.parser import parse as parse_html
from tests.layout_test_utils import iter_elements, render_document


ROOT = Path(__file__).resolve().parents[1]
FIXTURE_DIR = ROOT / "tests" / "fixtures" / "real_sites"
MANIFEST = json.loads((FIXTURE_DIR / "manifest.json").read_text(encoding="utf-8"))
CASES = MANIFEST["fixtures"]


def _fixture_html(case: dict) -> str:
    return (FIXTURE_DIR / f"{case['name']}.html").read_text(encoding="utf-8")


def _text_content(node) -> str:
    parts: list[str] = []
    pending = [node]
    while pending:
        current = pending.pop()
        if isinstance(current, Text):
            parts.append(current.data)
        pending.extend(reversed(getattr(current, "children", [])))
    return "".join(parts)


def _find_by_id(document, element_id: str) -> Element:
    for element in iter_elements(document):
        if element.attributes.get("id") == element_id:
            return element
    raise AssertionError(f"missing element id={element_id!r}")


def _title(document) -> str:
    for element in iter_elements(document):
        if element.tag == "title":
            return _text_content(element).strip()
    return ""


def _resource_references(document, base_url: str) -> list[tuple[str, str]]:
    """Collect external resources without making a network request."""
    references: list[tuple[str, str]] = []
    seen: set[tuple[str, str]] = set()
    for element in iter_elements(document):
        reference: tuple[str, str] | None = None
        if element.tag == "link":
            rel = element.attributes.get("rel", "").lower().split()
            href = element.attributes.get("href", "").strip()
            if "stylesheet" in rel and href:
                reference = ("stylesheet", href)
        elif element.tag == "img":
            source = element.attributes.get("src", "").strip()
            if not source:
                source = element.attributes.get("data-src", "").strip()
            if source:
                reference = ("image", source)
        elif element.tag == "script":
            source = element.attributes.get("src", "").strip()
            if source:
                reference = ("script", source)
        if reference is None:
            continue
        kind, raw_url = reference
        resolved = urljoin(base_url, raw_url)
        item = (kind, resolved)
        if item not in seen:
            references.append(item)
            seen.add(item)
    return references


def _image_source_values(element: Element) -> list[str]:
    """Return source candidates used by common 163 lazy-image markup."""
    values: list[str] = []
    for attribute in ("src", "data-src", "data-original", "data-lazy-src"):
        value = element.attributes.get(attribute, "").strip()
        if value:
            values.append(value)
    for attribute in ("srcset", "data-srcset"):
        raw_value = element.attributes.get(attribute, "").strip()
        for candidate in raw_value.split(","):
            value = candidate.strip().split(maxsplit=1)
            if value:
                values.append(value[0])
    return values


def _find_search_forms(document) -> list[Element]:
    return [
        element
        for element in iter_elements(document)
        if element.tag == "form"
        and element.attributes.get("role", "").lower() == "search"
    ]


def _descendants(node) -> list[Element]:
    result: list[Element] = []
    pending = list(reversed(getattr(node, "children", [])))
    while pending:
        current = pending.pop()
        if isinstance(current, Element):
            result.append(current)
        pending.extend(reversed(getattr(current, "children", [])))
    return result


def _bottom(element: Element) -> float:
    box = element.box
    assert box is not None
    return (
        box.y
        + box.content_height
        + box.padding.top
        + box.padding.bottom
        + box.border.top
        + box.border.bottom
    )


@pytest.mark.parametrize("case", CASES, ids=lambda case: case["name"])
def test_offline_fixture_has_title_semantic_sections_and_links(case: dict):
    document = parse_html(_fixture_html(case))
    assert _title(document) == case["title"]

    tags = {element.tag for element in iter_elements(document)}
    assert set(case["required_sections"]) <= tags

    links = [
        element
        for element in iter_elements(document)
        if element.tag == "a" and element.attributes.get("href", "").strip()
    ]
    assert len(links) >= case["minimum_links"]

    for link in links:
        assert link.attributes["href"] not in {"javascript:void(0)", "#"}


@pytest.mark.parametrize("case", CASES, ids=lambda case: case["name"])
def test_offline_fixture_exposes_a_submittable_search_form(case: dict):
    document = parse_html(_fixture_html(case))
    forms = _find_search_forms(document)
    assert forms, "a common content site should expose a discoverable search form"

    form = forms[0]
    assert urljoin(case["source_url"], form.attributes.get("action", "")) .endswith(
        case["search_action"]
    )
    controls = _descendants(form)
    assert any(
        element.tag == "input"
        and element.attributes.get("name") == case["search_name"]
        and element.attributes.get("type", "text").lower() in {"text", "search"}
        for element in controls
    )
    assert any(
        (element.tag == "button" and element.attributes.get("type", "submit") == "submit")
        or (element.tag == "input" and element.attributes.get("type", "").lower() == "submit")
        for element in controls
    )


@pytest.mark.parametrize("case", CASES, ids=lambda case: case["name"])
def test_offline_fixture_classifies_external_css_images_and_scripts(case: dict):
    document = parse_html(_fixture_html(case))
    references = _resource_references(document, case["source_url"])
    by_kind = {kind: [url for resource_kind, url in references if resource_kind == kind]
               for kind in {kind for kind, _ in references}}

    assert len(by_kind.get("stylesheet", [])) >= 2
    assert len(by_kind.get("image", [])) >= 1
    assert len(by_kind.get("script", [])) == 1
    assert all(url.startswith(("http://", "https://")) for _, url in references)


def test_163_fixture_preserves_live_resource_shapes_and_lazy_sources():
    case = next(case for case in CASES if case["name"] == "163_home")
    document = parse_html(_fixture_html(case))
    images = [element for element in iter_elements(document) if element.tag == "img"]
    assert len(images) >= case["minimum_images"]

    lazy_images = [
        element
        for element in images
        if "lazy" in element.attributes.get("class", "").split()
        or element.attributes.get("loading", "").lower() == "lazy"
    ]
    assert len(lazy_images) >= case["lazy_image_minimum"]
    assert sum(bool(element.attributes.get("data-src", "").strip()) for element in lazy_images) >= case[
        "lazy_data_src_minimum"
    ]
    assert any(element.attributes.get("data-original", "").strip() for element in lazy_images)
    assert sum(bool(element.attributes.get("srcset", "").strip()) for element in images) >= case[
        "srcset_image_minimum"
    ]
    assert any(
        not element.attributes.get("src", "").strip()
        and element.attributes.get("data-src", "").strip()
        for element in lazy_images
    ), "163 lazy images must be allowed to defer their src attribute"

    for image in lazy_images:
        candidates = _image_source_values(image)
        assert candidates, f"lazy image has no usable source attributes: {image.attributes}"
        assert all(
            urljoin(case["source_url"], candidate).startswith(("http://", "https://"))
            for candidate in candidates
        )
        assert image.attributes.get("alt", "").strip()

    references = _resource_references(document, case["source_url"])
    hosts = {urlsplit(url).netloc for _kind, url in references}
    assert set(case["resource_hosts"]) <= hosts


def test_163_lazy_resources_replay_offline_without_network():
    case = next(case for case in CASES if case["name"] == "163_home")
    document = parse_html(_fixture_html(case))
    expected_images = [
        url for kind, url in _resource_references(document, case["source_url"]) if kind == "image"
    ]
    requested_images: list[str] = []

    def fake_fetch(url: str):
        return f"/* offline stylesheet for {url} */", url

    def fake_fetch_bytes(url: str):
        requested_images.append(url)
        return base64.b64decode(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk"
            "+A8AAQUBAScY42YAAAAASUVORK5CYII="
        )

    with (
        patch("network.http.fetch", side_effect=fake_fetch),
        patch("network.http.fetch_bytes", side_effect=fake_fetch_bytes),
    ):
        _css_texts, image_data = engine._fetch_subresources(document, case["source_url"])

    assert len(image_data) == len(expected_images)
    assert set(requested_images) == set(expected_images)


@pytest.mark.parametrize("case", CASES, ids=lambda case: case["name"])
def test_offline_fixture_renders_beyond_the_initial_viewport(case: dict):
    document = render_document(_fixture_html(case), viewport_w=1024, viewport_h=600)
    scroll_content = _find_by_id(document, "scroll-content")
    assert scroll_content.box is not None
    assert _bottom(scroll_content) > 600

    articles = [element for element in iter_elements(document) if element.tag == "article"]
    assert articles
    assert all(article.box is not None for article in articles)
    content_blocks = [
        element
        for element in iter_elements(scroll_content)
        if element.tag in {"article", "p"} and element.box is not None
    ]
    assert len(content_blocks) >= 4
    assert all(content_blocks[index].box.y < content_blocks[index + 1].box.y
               for index in range(len(content_blocks) - 1))
    assert len(_text_content(scroll_content).strip()) >= case["minimum_scroll_text"]


def test_external_resources_can_be_replayed_without_network():
    """The generic resource discovery path is testable with deterministic responses."""
    requested_text: list[str] = []
    requested_bytes: list[str] = []

    def fake_fetch(url: str):
        requested_text.append(url)
        return f"/* offline stylesheet for {url} */", url

    def fake_fetch_bytes(url: str):
        requested_bytes.append(url)
        return base64.b64decode(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk"
            "+A8AAQUBAScY42YAAAAASUVORK5CYII="
        )

    expected_css: list[str] = []
    expected_images: list[str] = []
    expected_scripts: list[str] = []
    for case in CASES:
        document = parse_html(_fixture_html(case))
        css = _resource_references(document, case["source_url"])
        expected_css.extend(url for kind, url in css if kind == "stylesheet")
        expected_images.extend(url for kind, url in css if kind == "image")
        expected_scripts.extend(url for kind, url in css if kind == "script")

        with (
            patch("network.http.fetch", side_effect=fake_fetch),
            patch("network.http.fetch_bytes", side_effect=fake_fetch_bytes),
        ):
            css_texts, image_data = engine._fetch_subresources(document, case["source_url"])

        assert css_texts == [f"/* offline stylesheet for {url} */" for url in expected_css[-2:]]
        assert len(image_data) == len([url for kind, url in css if kind == "image"])

    assert set(requested_text) == set(expected_css)
    assert set(requested_bytes) == set(expected_images)
    assert not set(expected_scripts) & (set(requested_text) | set(requested_bytes))
