"""Static acceptance gate over the real-site fixtures.

This is the *fast* half of the harness described in
``docs/real_site_acceptance.md``. It reads the fixture bytes and the fixture's
local stylesheet with the standard library only, and asserts the parts of the
contract that live in the markup:

* a decoded document title;
* header, navigation, main, article, section, and aside semantics;
* usable links;
* a ``role=search`` form with a named input and a submit control;
* stylesheet, image, and script resource classification, and the deferred
  ``data-src`` / ``srcset`` shapes the portal fixture is required to keep;
* a scroll region made of enough declared block items that it cannot fit in a
  600px first screen, whose text blocks are in ascending document order.

It asserts none of that against a browser engine. The engine-level half - the
same page shapes parsed, styled, laid out, painted, and compared against a
pixel baseline - is ``tests/real_site_tasks``, run with cargo. Keeping the two
apart is deliberate: this gate needs no build, so it is the one that can tell
you a fixture was edited into an unusable state before you wait on a compile.

Run it either way::

    pytest -q tests/test_real_site_capabilities.py
    python tests/test_real_site_capabilities.py

No check here needs Internet access. Site names are test labels: nothing below
decides behaviour based on which site a URL belongs to.
"""

from __future__ import annotations

import re
import unittest
from dataclasses import dataclass, field
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import urljoin, urlparse

FIXTURE_ROOT = Path(__file__).resolve().parent / "fixtures" / "real_sites"

#: The first-screen bound the acceptance contract names.
FIRST_SCREEN_HEIGHT = 600

#: Landmarks every fixture must carry.
REQUIRED_LANDMARKS = ("header", "nav", "main", "article", "section", "aside")

#: Elements that are never rendered as content, so they may not own text.
NON_RENDERED = frozenset(
    {"head", "title", "style", "script", "link", "meta", "base", "template"}
)

#: Attributes that carry a URL a loader would fetch.
URL_ATTRIBUTES = (
    ("a", "href"),
    ("form", "action"),
    ("link", "href"),
    ("img", "src"),
    ("script", "src"),
    ("source", "src"),
    ("video", "poster"),
)


@dataclass(frozen=True)
class Fixture:
    """One reduced real-site page shape and the contract it pins down."""

    label: str
    html_file: str
    css_file: str
    base_url: str
    expected_title: str
    expected_stylesheets: int
    expected_images: int
    expected_deferred_images: int
    expected_srcset_images: int
    expected_video_posters: int
    expected_scripts: int
    min_links: int
    min_channel_sections: int
    min_scroll_blocks: int
    #: Class token of the feed items that make up the scroll region. Used to
    #: read the item geometry the fixture declares for itself.
    scroll_item_class: str
    #: Optional: origins the fixture's resources must come from, checked as
    #: inert data in the markup.
    required_image_hosts: tuple[str, ...] = ()
    required_static_hosts: tuple[str, ...] = ()


FIXTURES: tuple[Fixture, ...] = (
    Fixture(
        label="baidu_home",
        html_file="baidu_home.html",
        css_file="baidu_home.css",
        base_url="https://www.baidu.com/",
        expected_title="百度一下，你就知道",
        expected_stylesheets=1,
        expected_images=2,
        expected_deferred_images=0,
        expected_srcset_images=0,
        expected_video_posters=0,
        expected_scripts=1,
        min_links=30,
        min_channel_sections=2,
        min_scroll_blocks=20,
        scroll_item_class="feed-item",
        required_image_hosts=("ss1.bdstatic.com",),
        required_static_hosts=("ss1.bdstatic.com",),
    ),
    Fixture(
        label="baidu_results",
        html_file="baidu_results.html",
        css_file="baidu_results.css",
        base_url="https://www.baidu.com/s?wd=%E6%B5%8F%E8%A7%88%E5%99%A8",
        expected_title="前端布局引擎_百度搜索",
        expected_stylesheets=1,
        expected_images=2,
        expected_deferred_images=0,
        expected_srcset_images=0,
        expected_video_posters=0,
        expected_scripts=1,
        min_links=25,
        min_channel_sections=2,
        min_scroll_blocks=12,
        scroll_item_class="result",
        required_image_hosts=("ss1.bdstatic.com",),
        required_static_hosts=("ss1.bdstatic.com",),
    ),
    Fixture(
        label="zhihu_home",
        html_file="zhihu_home.html",
        css_file="zhihu_home.css",
        base_url="https://www.zhihu.com/",
        expected_title="知乎 - 有问题，就会有答案",
        expected_stylesheets=1,
        expected_images=2,
        expected_deferred_images=0,
        expected_srcset_images=0,
        expected_video_posters=0,
        expected_scripts=1,
        min_links=40,
        min_channel_sections=2,
        min_scroll_blocks=20,
        scroll_item_class="hot-item",
        required_image_hosts=("static.zhihu.com", "picx.zhihu.com"),
        required_static_hosts=("static.zhihu.com",),
    ),
    Fixture(
        label="zhihu_article",
        html_file="zhihu_article.html",
        css_file="zhihu_article.css",
        base_url="https://zhuanlan.zhihu.com/p/600000001",
        expected_title="用确定性测量给渲染结果建立像素基线 - 知乎专栏",
        expected_stylesheets=1,
        expected_images=4,
        expected_deferred_images=0,
        expected_srcset_images=1,
        expected_video_posters=0,
        expected_scripts=1,
        min_links=14,
        min_channel_sections=2,
        min_scroll_blocks=4,
        scroll_item_class="article-section",
        required_image_hosts=("static.zhihu.com", "pic1.zhihu.com"),
        required_static_hosts=("static.zhihu.com",),
    ),
    Fixture(
        label="netease_163_home",
        html_file="netease_163_home.html",
        css_file="netease_163_home.css",
        base_url="https://www.163.com/",
        expected_title="网易新闻 - 网易",
        expected_stylesheets=2,
        expected_images=6,
        expected_deferred_images=3,
        expected_srcset_images=2,
        expected_video_posters=1,
        expected_scripts=1,
        min_links=60,
        min_channel_sections=6,
        min_scroll_blocks=24,
        scroll_item_class="rank-item",
        required_image_hosts=("nimg.ws.126.net",),
        required_static_hosts=("static.ws.126.net",),
    ),
    # -- the four page shapes added once table layout, sticky positioning, the
    # derived form owner, @supports evaluation and the stylesheet diagnostics
    # had landed and none of them had ever met a real page. All five original
    # fixtures are single-column, so all four new ones are shapes the first
    # group structurally cannot express.
    Fixture(
        label="reference_two_column",
        html_file="reference_two_column.html",
        css_file="reference_two_column.css",
        base_url="https://developer.mozilla.org/zh-CN/docs/Web/CSS/position",
        expected_title="position 属性 - CSS 参考 | 开发者文档",
        expected_stylesheets=2,
        expected_images=2,
        expected_deferred_images=0,
        expected_srcset_images=0,
        expected_video_posters=0,
        expected_scripts=1,
        min_links=34,
        min_channel_sections=2,
        min_scroll_blocks=10,
        scroll_item_class="ref-block",
        required_image_hosts=("static.devdocs.example",),
        required_static_hosts=("static.devdocs.example",),
    ),
    Fixture(
        label="spec_data_table",
        html_file="spec_data_table.html",
        css_file="spec_data_table.css",
        base_url="https://www.w3.org/TR/css-position-3/",
        expected_title="CSS 定位布局 第 3 级规范 - 绝对定位盒模型取值汇总",
        expected_stylesheets=2,
        expected_images=2,
        expected_deferred_images=0,
        expected_srcset_images=0,
        expected_video_posters=0,
        expected_scripts=1,
        min_links=30,
        min_channel_sections=2,
        min_scroll_blocks=12,
        scroll_item_class="spec-clause",
        required_image_hosts=("www.w3.org",),
        required_static_hosts=("www.w3.org",),
    ),
    Fixture(
        label="sticky_toolbar",
        html_file="sticky_toolbar.html",
        css_file="sticky_toolbar.css",
        base_url="https://kubernetes.io/zh-cn/docs/tasks/configure-pod-container/assign-memory-resource/",
        expected_title="为容器和 Pod 分配内存资源 - Kubernetes 文档",
        expected_stylesheets=2,
        expected_images=1,
        expected_deferred_images=0,
        expected_srcset_images=0,
        expected_video_posters=0,
        expected_scripts=1,
        min_links=26,
        min_channel_sections=0,
        min_scroll_blocks=11,
        scroll_item_class="doc-section",
        required_image_hosts=("kubernetes.io",),
        required_static_hosts=("kubernetes.io",),
    ),
    Fixture(
        label="form_heavy_signin",
        html_file="form_heavy_signin.html",
        css_file="form_heavy_signin.css",
        base_url="https://passport.csdn.net/login?code=public",
        expected_title="登录 - 会员中心",
        expected_stylesheets=2,
        expected_images=1,
        expected_deferred_images=0,
        expected_srcset_images=0,
        expected_video_posters=0,
        expected_scripts=1,
        min_links=26,
        min_channel_sections=2,
        min_scroll_blocks=14,
        scroll_item_class="faq-item",
        required_image_hosts=("static.member.example",),
        required_static_hosts=("static.member.example",),
    ),
)

#: The fixtures ``docs/real_site_acceptance.md`` named when the harness was
#: written. Kept so a change to the first five and an addition to the set are
#: different things in a diff.
ORIGINAL_FIXTURES: tuple[str, ...] = (
    "baidu_home",
    "baidu_results",
    "zhihu_home",
    "zhihu_article",
    "netease_163_home",
)


# ---------------------------------------------------------------------------
# A minimal DOM, built from the standard library's own HTML parser.
# ---------------------------------------------------------------------------


@dataclass
class Element:
    tag: str
    attrs: dict[str, str] = field(default_factory=dict)
    children: list["Element"] = field(default_factory=list)
    text_parts: list[str] = field(default_factory=list)
    parent: "Element | None" = None
    #: Index of this element in document order, assigned during the build.
    order: int = -1

    def classes(self) -> list[str]:
        return self.attrs.get("class", "").split()

    def descendants(self) -> list["Element"]:
        found: list[Element] = []
        pending = list(self.children)
        while pending:
            current = pending.pop(0)
            found.append(current)
            pending = list(current.children) + pending
        return found

    def iter_all(self) -> list["Element"]:
        return [self] + self.descendants()

    def text(self) -> str:
        parts: list[str] = []
        pending: list[Element] = [self]
        while pending:
            current = pending.pop(0)
            parts.extend(current.text_parts)
            pending.extend(current.children)
        return "".join(parts)

    def normalized_text(self) -> str:
        return " ".join(self.text().split())


class _DomBuilder(HTMLParser):
    """Builds the element tree, tolerating the implicit closes real pages rely on.

    This is a *static* reader for fixtures this repository controls, not an HTML
    parser for the web: it closes an element when a new one arrives that the
    fixture's own nesting already closed. Every fixture is well nested, so the
    stack never needs a special-case rule, and an unbalanced fixture shows up as
    a mismatched-tag assertion rather than as silently different structure.
    """

    VOID = frozenset(
        {
            "area", "base", "br", "col", "embed", "hr", "img", "input",
            "link", "meta", "param", "source", "track", "wbr",
        }
    )

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.root = Element(tag="#document")
        self.stack: list[Element] = [self.root]
        self.unbalanced: list[str] = []
        self._order = 0

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        element = Element(
            tag=tag,
            attrs={name: (value or "") for name, value in attrs},
            parent=self.stack[-1],
            order=self._order,
        )
        self._order += 1
        self.stack[-1].children.append(element)
        if tag not in self.VOID:
            self.stack.append(element)

    def handle_startendtag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        self.handle_starttag(tag, attrs)
        if tag not in self.VOID:
            self.handle_endtag(tag)

    def handle_endtag(self, tag: str) -> None:
        if len(self.stack) == 1:
            self.unbalanced.append(f"stray </{tag}>")
            return
        if self.stack[-1].tag != tag:
            self.unbalanced.append(
                f"</{tag}> closes <{self.stack[-1].tag}> at element "
                f"{self.stack[-1].order}"
            )
            return
        self.stack.pop()

    def handle_data(self, data: str) -> None:
        self.stack[-1].text_parts.append(data)


def parse_html(source: str) -> tuple[Element, list[str]]:
    builder = _DomBuilder()
    builder.feed(source)
    builder.close()
    if len(builder.stack) != 1:
        unclosed = ", ".join(f"<{element.tag}>" for element in builder.stack[1:])
        builder.unbalanced.append(f"unclosed: {unclosed}")
    return builder.root, builder.unbalanced


# ---------------------------------------------------------------------------
# A single-compound-selector reader for the fixture's own stylesheet.
# ---------------------------------------------------------------------------

_RULE = re.compile(r"([^{}]+)\{([^{}]*)\}", re.DOTALL)
_COMPOUND = re.compile(r"(?P<tag>^[a-z][a-z0-9]*)?(?P<rest>(?:[.#][\w-]+)*)$")


@dataclass(frozen=True)
class Compound:
    """A type, class, or id selector with no combinator and no pseudo-class."""

    tag: str | None
    classes: frozenset[str]
    element_id: str | None


def read_declarations(css: str) -> dict[Compound, dict[str, str]]:
    """Map single-compound selectors to their declared longhand properties.

    Only single compounds are understood, on purpose. The fixtures declare their
    block geometry with plain ``.class`` rules; anything more elaborate would be
    a fixture that this gate cannot reason about, and it says so rather than
    guessing.
    """
    declarations: dict[Compound, dict[str, str]] = {}
    for selector_text, body in _RULE.findall(css):
        declarations_text = _parse_declarations(body)
        for selector in selector_text.split(","):
            compound = _parse_compound(selector)
            if compound is None or not declarations_text:
                continue
            declarations.setdefault(compound, {}).update(declarations_text)
    return declarations


def _parse_declarations(body: str) -> dict[str, str]:
    found: dict[str, str] = {}
    for declaration in body.split(";"):
        if ":" not in declaration:
            continue
        name, _, value = declaration.partition(":")
        name = name.strip().lower()
        value = value.strip()
        if name and value:
            found[name] = value
    return found


def _parse_compound(selector: str) -> Compound | None:
    text = selector.strip()
    if not text or any(character in text for character in " :>+~[]()"):
        return None
    match = _COMPOUND.match(text)
    if match is None:
        return None
    rest = match.group("rest") or ""
    classes = frozenset(part[1:] for part in re.findall(r"\.[\w-]+", rest))
    ids = re.findall(r"#[\w-]+", rest)
    return Compound(
        tag=match.group("tag"),
        classes=classes,
        element_id=ids[0][1:] if ids else None,
    )


def declarations_for(
    element: Element, declarations: dict[Compound, dict[str, str]]
) -> dict[str, str]:
    """Cascade the fixture's own rules that match `element` exactly."""
    merged: dict[str, str] = {}
    for compound, values in declarations.items():
        if compound.tag is not None and compound.tag != element.tag:
            continue
        if compound.element_id is not None and compound.element_id != element.attrs.get("id"):
            continue
        if compound.classes and not compound.classes.issubset(set(element.classes())):
            continue
        if compound.tag is None and not compound.classes and compound.element_id is None:
            continue
        merged.update(values)
    return merged


def pixels(value: str) -> float | None:
    """A plain pixel length, or `None` for anything this gate does not evaluate."""
    match = re.fullmatch(r"(-?\d+(?:\.\d+)?)px", value.strip())
    return float(match.group(1)) if match else None


# ---------------------------------------------------------------------------
# The loaded fixture.
# ---------------------------------------------------------------------------


@dataclass
class Loaded:
    fixture: Fixture
    raw: bytes
    source: str
    charset: str | None
    root: Element
    unbalanced: list[str]
    declarations: dict[Compound, dict[str, str]]


def load(fixture: Fixture) -> Loaded:
    raw = (FIXTURE_ROOT / fixture.html_file).read_bytes()
    charset = declared_charset(raw)
    source = raw.decode(charset)
    root, unbalanced = parse_html(source)
    css = (FIXTURE_ROOT / fixture.css_file).read_text(encoding="utf-8")
    return Loaded(
        fixture=fixture,
        raw=raw,
        source=source,
        charset=charset,
        root=root,
        unbalanced=unbalanced,
        declarations=read_declarations(css),
    )


def declared_charset(raw: bytes) -> str | None:
    """The charset a browser sniffs from a BOM or a meta declaration.

    Returns ``None`` when the declaration is unreachable. The HTML encoding
    prescan reads only the first 1024 bytes, so a fixture whose leading comment
    pushes ``<meta charset>`` past that boundary is decoded with the fallback
    label instead. That is correct engine behaviour and a fixture bug worth
    failing on, not a capability worth asserting.
    """
    if raw.startswith(b"\xef\xbb\xbf"):
        return "utf-8-sig"
    if raw.startswith((b"\xff\xfe", b"\xfe\xff")):
        return "utf-16"
    head = raw[:1024].decode("ascii", errors="replace")
    match = re.search(
        r"""<meta[^>]+charset\s*=\s*["']?\s*([A-Za-z0-9_.:-]+)""", head, re.IGNORECASE
    )
    if not match:
        return None
    label = match.group(1).strip().lower()
    return {"utf8": "utf-8", "gb2312": "gb18030", "gbk": "gb18030"}.get(label, label)


def document_title(loaded: Loaded) -> str:
    for element in loaded.root.iter_all():
        if element.tag == "title":
            return " ".join(element.text().split())
    return ""


def tag_all(loaded: Loaded, tag: str) -> list[Element]:
    return [element for element in loaded.root.iter_all() if element.tag == tag]


def has_rel(element: Element, token: str) -> bool:
    return token in element.attrs.get("rel", "").split()


def resource_elements(loaded: Loaded, tag: str, attribute: str) -> list[Element]:
    return [element for element in tag_all(loaded, tag) if element.attrs.get(attribute, "").strip()]


def scroll_region(loaded: Loaded, item_class: str) -> Element | None:
    """The element with the most direct children carrying `item_class`."""
    mains = [element for element in loaded.root.iter_all() if element.tag == "main"]
    if not mains:
        mains = [loaded.root]
    inside: list[Element] = []
    for main in mains:
        inside.extend(main.descendants())
    best: tuple[int, Element] | None = None
    for candidate in inside:
        count = sum(1 for child in candidate.children if item_class in child.classes())
        if count and (best is None or count > best[0]):
            best = (count, candidate)
    return best[1] if best else None


def scroll_items(loaded: Loaded, item_class: str) -> list[Element]:
    region = scroll_region(loaded, item_class)
    if region is None:
        return []
    return [child for child in region.children if item_class in child.classes()]


# ---------------------------------------------------------------------------
# The checks.
# ---------------------------------------------------------------------------


class RealSiteFixtureTest(unittest.TestCase):
    """The static half of the offline real-site acceptance contract."""

    def _each(self):
        loaded = [(fixture, load(fixture)) for fixture in FIXTURES]
        for fixture, data in loaded:
            with self.subTest(fixture=fixture.label):
                yield fixture, data

    # -- 1. a decoded document title -------------------------------------

    def test_every_fixture_decodes_and_declares_its_title(self) -> None:
        for fixture, loaded in self._each():
            self.assertEqual(
                loaded.charset,
                "utf-8",
                f"{fixture.label} does not declare <meta charset=utf-8> inside the first "
                f"1024 bytes, so the engine legitimately decodes it as windows-1252",
            )
            self.assertNotIn("\ufffd", loaded.source, f"{fixture.label} has undecodable bytes")
            self.assertEqual(
                document_title(loaded), fixture.expected_title, f"{fixture.label} title"
            )
            self.assertTrue(
                any(character > "\x7f" for character in fixture.expected_title),
                f"{fixture.label} must assert a non-ASCII title, so decoding is observable",
            )

    def test_no_fixture_is_unbalanced_or_misses_a_doctype(self) -> None:
        for fixture, loaded in self._each():
            self.assertEqual(loaded.unbalanced, [], f"{fixture.label} nesting")
            self.assertTrue(
                loaded.source.lstrip().lower().startswith("<!doctype html>"),
                f"{fixture.label} must declare <!doctype html>; quirks mode is not implemented",
            )

    # -- 2. landmark semantics -------------------------------------------

    def test_every_fixture_carries_the_required_landmarks(self) -> None:
        for fixture, loaded in self._each():
            for landmark in REQUIRED_LANDMARKS:
                matches = tag_all(loaded, landmark)
                self.assertTrue(
                    matches, f"{fixture.label} has no <{landmark}> landmark"
                )
                for element in matches:
                    self.assertNotIn(
                        element.tag,
                        NON_RENDERED,
                        f"{fixture.label} <{landmark}> must be a content landmark",
                    )
            mains = tag_all(loaded, "main")
            self.assertEqual(
                len(mains), 1, f"{fixture.label} must have exactly one <main>"
            )

    # -- 3. usable links -------------------------------------------------

    def test_every_link_is_navigable_and_named(self) -> None:
        for fixture, loaded in self._each():
            links = [element for element in tag_all(loaded, "a") if "href" in element.attrs]
            self.assertGreaterEqual(
                len(links), fixture.min_links, f"{fixture.label} link count"
            )
            for link in links:
                href = link.attrs["href"].strip()
                self.assertTrue(href, f"{fixture.label} has an <a> with an empty href")
                resolved = urljoin(fixture.base_url, href)
                self.assertIn(
                    resolved.split(":", 1)[0],
                    ("http", "https"),
                    f"{fixture.label} link {href!r} is not navigable",
                )
                self.assertTrue(
                    link.normalized_text()
                    or any(
                        image.attrs.get("alt", "").strip()
                        for image in link.iter_all()
                        if image.tag == "img"
                    ),
                    f"{fixture.label} link {href!r} has no accessible name",
                )

    def test_every_fetched_url_resolves(self) -> None:
        for fixture, loaded in self._each():
            for tag, attribute in URL_ATTRIBUTES:
                for element in resource_elements(loaded, tag, attribute):
                    value = element.attrs[attribute].strip()
                    resolved = urlparse(urljoin(fixture.base_url, value))
                    self.assertIn(
                        resolved.scheme,
                        ("http", "https"),
                        f"{fixture.label} <{tag} {attribute}={value!r}> is not fetchable",
                    )
                    self.assertTrue(
                        resolved.netloc, f"{fixture.label} <{tag} {attribute}> has no host"
                    )

    # -- 4. the search landmark -------------------------------------------

    def test_every_fixture_has_one_role_search_form_with_a_named_input_and_a_submit(self) -> None:
        for fixture, loaded in self._each():
            forms = [
                element
                for element in tag_all(loaded, "form")
                if element.attrs.get("role", "").strip() == "search"
            ]
            self.assertEqual(
                len(forms), 1, f"{fixture.label} must have exactly one role=search form"
            )
            form = forms[0]
            named = [
                input_element
                for input_element in form.iter_all()
                if input_element.tag == "input"
                and input_element.attrs.get("name", "").strip()
                and input_element.attrs.get("type", "text").strip().lower() != "hidden"
            ]
            self.assertTrue(
                named, f"{fixture.label} search form has no named, visible input"
            )
            submits = [
                element
                for element in form.iter_all()
                if element.tag == "button"
                or (
                    element.tag == "input"
                    and element.attrs.get("type", "").strip().lower() == "submit"
                )
            ]
            self.assertTrue(
                submits, f"{fixture.label} search form has no submit control"
            )

    def test_a_form_that_is_not_a_search_landmark_is_not_mistaken_for_one(self) -> None:
        # The article fixture keeps a plain comment composer next to the search
        # form, so a gate that keys on "there is a form" instead of the landmark
        # role fails here.
        loaded = load(next(f for f in FIXTURES if f.label == "zhihu_article"))
        plain = [
            element
            for element in tag_all(loaded, "form")
            if element.attrs.get("role", "").strip() != "search"
        ]
        self.assertTrue(plain, "the article fixture should keep a non-search form")

    # -- 5. resource classification ---------------------------------------

    def test_every_fixture_classifies_its_stylesheet_image_and_script_resources(self) -> None:
        for fixture, loaded in self._each():
            links = tag_all(loaded, "link")
            stylesheets = [element for element in links if has_rel(element, "stylesheet")]
            self.assertEqual(
                len(stylesheets),
                fixture.expected_stylesheets,
                f"{fixture.label} stylesheet classification",
            )
            for element in stylesheets:
                self.assertTrue(
                    element.attrs.get("href", "").strip(),
                    f"{fixture.label} has a rel=stylesheet with no href",
                )
                if "type" in element.attrs:
                    self.assertEqual(
                        element.attrs["type"].strip().lower(),
                        "text/css",
                        f"{fixture.label} declares a non-CSS rel=stylesheet",
                    )
            self.assertFalse(
                [
                    element
                    for element in links
                    if has_rel(element, "icon")
                    and element.attrs.get("href", "").strip().endswith(".css")
                ],
                f"{fixture.label} must not classify an icon as a stylesheet",
            )

            images = [
                element
                for element in tag_all(loaded, "img")
                if element.attrs.get("src", "").strip()
                or element.attrs.get("srcset", "").strip()
            ]
            deferred = [
                element
                for element in tag_all(loaded, "img")
                if (
                    element.attrs.get("data-src", "").strip()
                    or element.attrs.get("data-original", "").strip()
                )
                and not element.attrs.get("src", "").strip()
                and not element.attrs.get("srcset", "").strip()
            ]
            srcset_only = [
                element
                for element in tag_all(loaded, "img")
                if element.attrs.get("srcset", "").strip()
                and not element.attrs.get("src", "").strip()
            ]
            posters = resource_elements(loaded, "video", "poster")
            self.assertEqual(
                len(images) + len(posters),
                fixture.expected_images,
                f"{fixture.label} image classification",
            )
            self.assertEqual(
                len(deferred),
                fixture.expected_deferred_images,
                f"{fixture.label} deferred data-src image count",
            )
            self.assertEqual(
                len(srcset_only),
                fixture.expected_srcset_images,
                f"{fixture.label} srcset-only image count",
            )
            self.assertEqual(
                len(posters), fixture.expected_video_posters, f"{fixture.label} poster count"
            )

            scripts = [
                element
                for element in tag_all(loaded, "script")
                if element.attrs.get("src", "").strip()
            ]
            self.assertEqual(
                len(scripts), fixture.expected_scripts, f"{fixture.label} script count"
            )
            for script in scripts:
                self.assertIn(
                    "defer",
                    script.attrs,
                    f"{fixture.label} external script must be deferred",
                )

    def test_every_image_carries_alt_text(self) -> None:
        for fixture, loaded in self._each():
            for image in tag_all(loaded, "img"):
                self.assertTrue(
                    image.attrs.get("alt", "").strip(),
                    f"{fixture.label} <img> has no alt text",
                )

    def test_resources_come_from_the_origins_the_fixture_declares(self) -> None:
        # The origin is inert data here: this asserts the fixture kept the URL
        # shapes the real page uses, and never asks the engine to behave
        # differently because of them.
        for fixture, loaded in self._each():
            hosts = {
                urlparse(
                    urljoin(fixture.base_url, element.attrs[attribute].strip())
                ).netloc
                for tag, attribute in URL_ATTRIBUTES
                for element in resource_elements(loaded, tag, attribute)
            }
            for host in fixture.required_image_hosts:
                self.assertIn(host, hosts, f"{fixture.label} lost its {host} assets")
            for host in fixture.required_static_hosts:
                self.assertIn(host, hosts, f"{fixture.label} lost its {host} assets")

    def test_the_portal_fixture_keeps_its_lazy_and_srcset_shapes(self) -> None:
        fixture = next(f for f in FIXTURES if f.label == "netease_163_home")
        loaded = load(fixture)

        lazy = [
            image
            for image in tag_all(loaded, "img")
            if "lazy" in image.classes()
        ]
        self.assertEqual(len(lazy), fixture.expected_deferred_images)
        for image in lazy:
            for attribute in ("data-src", "data-original"):
                value = image.attrs.get(attribute, "").strip()
                self.assertTrue(
                    value, f"a lazy image has no {attribute}"
                )
                self.assertTrue(
                    urlparse(urljoin(fixture.base_url, value)).netloc,
                    f"{attribute}={value!r} does not resolve",
                )
            self.assertFalse(
                image.attrs.get("src", "").strip(),
                "a deferred image must not carry an eager src",
            )

        candidates = [
            image
            for image in tag_all(loaded, "img")
            if image.attrs.get("srcset", "").strip()
        ]
        self.assertEqual(len(candidates), fixture.expected_srcset_images)
        descriptors = {"w": 0, "x": 0}
        for image in candidates:
            for candidate in image.attrs["srcset"].split(","):
                parts = candidate.split()
                self.assertGreaterEqual(len(parts), 2, "a srcset candidate needs a descriptor")
                self.assertIn(parts[-1][-1], descriptors, "unknown srcset descriptor")
                descriptors[parts[-1][-1]] += 1
            self.assertTrue(
                image.attrs.get("sizes", "").strip(),
                "a srcset image must declare sizes, or selection is undefined",
            )
        self.assertGreater(descriptors["w"], 0, "no width-descriptor candidate")
        self.assertGreater(descriptors["x"], 0, "no density-descriptor candidate")

    def test_the_portal_fixture_keeps_a_long_channel_feed_and_channel_links(self) -> None:
        fixture = next(f for f in FIXTURES if f.label == "netease_163_home")
        loaded = load(fixture)
        channels = [
            section
            for section in tag_all(loaded, "section")
            if sum(
                1
                for link in section.iter_all()
                if link.tag == "a" and link.attrs.get("href", "").strip()
            )
            >= 3
        ]
        self.assertGreaterEqual(
            len(channels), fixture.min_channel_sections, "channel section count"
        )
        channel_links = [
            link
            for link in tag_all(loaded, "a")
            if link.attrs.get("href", "").strip() and "163.com/" in link.attrs["href"]
        ]
        self.assertGreaterEqual(len(channel_links), 6)
        items = scroll_items(loaded, fixture.scroll_item_class)
        self.assertGreaterEqual(len(items), 20, "the channel feed is not long")

    # -- 6 and 7. the scroll region ---------------------------------------

    def test_every_fixture_declares_a_scroll_region_that_cannot_fit_a_600px_screen(self) -> None:
        for fixture, loaded in self._each():
            items = scroll_items(loaded, fixture.scroll_item_class)
            self.assertGreaterEqual(
                len(items),
                fixture.min_scroll_blocks,
                f"{fixture.label} scroll region holds too few blocks",
            )
            heights = []
            for item in items:
                declared = declarations_for(item, loaded.declarations)
                height = pixels(declared.get("height", ""))
                self.assertIsNotNone(
                    height,
                    f"{fixture.label} scroll item {item.tag}.{item.classes()} declares no "
                    f"pixel height, so its vertical size is undefined to this gate",
                )
                heights.append(height)
            self.assertTrue(
                min(heights) * len(heights) > FIRST_SCREEN_HEIGHT,
                f"{fixture.label} declares {len(heights)} items of at least "
                f"{min(heights)}px, which cannot fill more than {FIRST_SCREEN_HEIGHT}px",
            )

    def test_every_scroll_region_has_ordered_distinct_text_blocks(self) -> None:
        for fixture, loaded in self._each():
            items = scroll_items(loaded, fixture.scroll_item_class)
            texts = [item.normalized_text() for item in items]
            self.assertTrue(all(texts), f"{fixture.label} has an empty scroll text block")
            self.assertEqual(
                len(set(texts)),
                len(texts),
                f"{fixture.label} scroll blocks are not distinct",
            )
            # A ranked feed carries a rising ordinal in each block's own text, so
            # document order is checkable from the markup alone. Where a fixture
            # does that, the ordinals must ascend; where it does not, the
            # ordering assertion - that the engine lays the blocks out top to
            # bottom in document order - belongs to tests/real_site_tasks, which
            # has the fragment tree to check it against.
            ordinals = [order(text) for text in texts]
            if all(ordinal is not None for ordinal in ordinals):
                self.assertEqual(
                    ordinals,
                    sorted(ordinal for ordinal in ordinals if ordinal is not None),
                    f"{fixture.label} feed order is not ascending",
                )

    # -- the local responses exist and are usable ------------------------

    def test_every_fixture_ships_its_local_stylesheet_response(self) -> None:
        for fixture in FIXTURES:
            path = FIXTURE_ROOT / fixture.css_file
            self.assertTrue(path.is_file(), f"{fixture.css_file} is missing")
            declarations = read_declarations(path.read_text(encoding="utf-8"))
            self.assertTrue(
                declarations, f"{fixture.css_file} declares no rules this gate can read"
            )


def order(text: str) -> int | None:
    """The trailing ordinal a feed block carries in its own text."""
    match = re.search(r"(\d{1,3})$", text)
    return int(match.group(1)) if match else None


if __name__ == "__main__":
    unittest.main(verbosity=2)
