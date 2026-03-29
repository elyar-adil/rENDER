"""rENDER browser engine pipeline and entry point."""
import logging
_logger = logging.getLogger(__name__)
import argparse
import sys
import os
import re
from concurrent.futures import ThreadPoolExecutor, as_completed
from urllib.parse import unquote_to_bytes

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

UA_CSS = os.path.join(os.path.dirname(__file__), 'ua', 'user-agent.css')
VIEWPORT_W = 980
VIEWPORT_H = 600


# ---------------------------------------------------------------------------
# Pipeline
# ---------------------------------------------------------------------------

def _pipeline(html_content: str, base_url: str = '',
              viewport_width: int = VIEWPORT_W,
              viewport_height: int = VIEWPORT_H) -> tuple:
    """Parse -> JS -> CSS (+ external sheets) -> images -> layout.

    External stylesheets and images are fetched concurrently.
    Returns (display_list, page_height, document).
    """
    from html.parser import parse as parse_html
    from css.cascade import bind as css_bind
    from css.computed import compute as css_compute
    import layout as layout_mod

    doc = parse_html(html_content)

    css_texts, img_data = _fetch_subresources(doc, base_url)

    _execute_scripts(doc, base_url)

    css_texts2, img_data2 = _fetch_subresources(doc, base_url)
    css_texts.extend(css_texts2)
    img_data.extend(img_data2)

    css_bind(doc, UA_CSS, viewport_width=viewport_width, viewport_height=viewport_height,
             extra_css_texts=css_texts, base_url=base_url)
    css_compute(doc, viewport_width=viewport_width, viewport_height=viewport_height)

    img_data.extend(_fetch_background_images(doc, base_url))

    import backend
    backend.get_image_loader().attach_images(img_data)

    dl = layout_mod.layout(doc, viewport_width=viewport_width, viewport_height=viewport_height)
    height = _page_height(doc, viewport_height=viewport_height)
    return dl, height, doc


def _looks_like_browser_hydration_shell(html_content: str, base_url: str = '') -> bool:
    """Detect pages whose visible content only appears after client hydration."""
    if not base_url.startswith('http://') and not base_url.startswith('https://'):
        return False
    if 'data-ssr-entry=' not in html_content:
        return False
    return bool(
        re.search(
            r'<div\s+id=["\']root["\'][^>]*>\s*</div>',
            html_content,
            re.IGNORECASE,
        )
    )




# ---------------------------------------------------------------------------
# Subresource fetching
# ---------------------------------------------------------------------------

def _fetch_subresources(document, base_url: str) -> tuple[list, list]:
    """Concurrently fetch external CSS stylesheets and images.

    CSS results are returned in DOM order even though requests run concurrently.
    """
    from html.dom import Element
    from network.http import fetch_bytes, resolve_url, fetch as fetch_text

    css_jobs: list[str] = []
    img_jobs: list[tuple] = []

    stack = [document]
    while stack:
        node = stack.pop()
        if isinstance(node, Element):
            tag = node.tag
            if tag == 'link':
                rel = node.attributes.get('rel', '').lower()
                href = node.attributes.get('href', '').strip()
                if 'stylesheet' in rel and href and not href.startswith('data:'):
                    url = resolve_url(base_url, href) if base_url else href
                    css_jobs.append(url)
            elif tag == 'img':
                src = node.attributes.get('src', '').strip()
                if not src:
                    src = node.attributes.get('data-src', '').strip()
                if src.startswith('data:'):
                    img_jobs.append((node, None, src))
                elif src:
                    url = resolve_url(base_url, src) if base_url else src
                    img_jobs.append((node, url, None))
        stack.extend(reversed(node.children))

    if not css_jobs and not img_jobs:
        return [], []

    max_workers = min(16, len(css_jobs) + len(img_jobs))

    def _fetch_css(url):
        try:
            text, _ = fetch_text(url)
            return text
        except Exception:
            return None

    css_results: list[str | None] = [None] * len(css_jobs)
    with ThreadPoolExecutor(max_workers=max_workers) as pool:
        css_futures_by_url: dict = {}
        css_future_map: dict = {}
        for idx, url in enumerate(css_jobs):
            future = css_futures_by_url.get(url)
            if future is None:
                future = pool.submit(_fetch_css, url)
                css_futures_by_url[url] = future
            css_future_map.setdefault(future, []).append(idx)

        for future in as_completed(css_future_map):
            text = future.result()
            for idx in css_future_map[future]:
                css_results[idx] = text

    css_texts = [t for t in css_results if t]
    img_data = _fetch_binary_jobs(img_jobs, fetcher=fetch_bytes, max_workers=max_workers)
    return css_texts, img_data


def _fetch_binary_jobs(jobs: list[tuple], *, fetcher, max_workers: int) -> list[tuple]:
    """Fetch binary resources concurrently while preserving DOM order."""
    if not jobs:
        return []

    results: list[tuple | None] = [None] * len(jobs)

    def _fetch_single(url, data_uri):
        if data_uri:
            return _decode_data_uri(data_uri)
        try:
            return fetcher(url)
        except Exception:
            return None

    with ThreadPoolExecutor(max_workers=max_workers) as pool:
        futures_by_url: dict = {}
        future_map: dict = {}
        for idx, (_, url, data_uri) in enumerate(jobs):
            if data_uri:
                future = pool.submit(_fetch_single, None, data_uri)
            else:
                future = futures_by_url.get(url)
                if future is None:
                    future = pool.submit(_fetch_single, url, None)
                    futures_by_url[url] = future
            future_map.setdefault(future, []).append(idx)

        for future in as_completed(future_map):
            raw = future.result()
            for idx in future_map[future]:
                results[idx] = (jobs[idx][0], raw)

    return [item for item in results if item is not None]


class _BackgroundImageTarget:
    """Proxy that lets the generic image loader attach CSS background images."""

    __slots__ = ('node', 'natural_width', 'natural_height')

    def __init__(self, node):
        self.node = node
        self.natural_width = 0
        self.natural_height = 0

    @property
    def image(self):
        return getattr(self.node, 'background_image', None)

    @image.setter
    def image(self, value):
        self.node.background_image = value


def _fetch_background_images(document, base_url: str) -> list[tuple]:
    """Fetch CSS background-image URLs after styles have been computed."""
    from html.dom import Element
    from network.http import fetch_bytes, resolve_url

    jobs: list[tuple] = []
    stack = [document]
    while stack:
        node = stack.pop()
        if isinstance(node, Element):
            bg_image = (getattr(node, 'style', {}) or {}).get('background-image', '').strip()
            bg_url = _extract_background_image_url(bg_image)
            if bg_url:
                target = _BackgroundImageTarget(node)
                if bg_url.startswith('data:'):
                    jobs.append((target, None, bg_url))
                else:
                    url = resolve_url(base_url, bg_url) if base_url else bg_url
                    jobs.append((target, url, None))
        stack.extend(reversed(getattr(node, 'children', [])))

    if not jobs:
        return []

    return _fetch_binary_jobs(jobs, fetcher=fetch_bytes, max_workers=min(16, len(jobs)))


def _extract_background_image_url(value: str) -> str | None:
    import re

    if not value or value in ('none', ''):
        return None

    match = re.search(r'url\(\s*([\'"]?)(.+?)\1\s*\)', value, re.IGNORECASE)
    if not match:
        return None
    return match.group(2).strip()


def _decode_data_uri(data_uri: str) -> bytes | None:
    try:
        header, payload = data_uri.split(',', 1)
    except ValueError:
        return None
    if ';base64' in header.lower():
        try:
            import base64
            return base64.b64decode(payload)
        except Exception:
            return None
    try:
        return unquote_to_bytes(payload)
    except Exception:
        return None


# ---------------------------------------------------------------------------
# JavaScript execution
# ---------------------------------------------------------------------------

def _execute_scripts(document, base_url: str) -> None:
    """Find and execute <script> elements in DOM order."""
    from html.dom import Text
    from js.lexer import Lexer
    from js.parser import Parser
    from js.interpreter import Interpreter
    from js.dom_api import DOMBinding
    from js.xhr import XMLHttpRequest

    scripts: list = []
    _collect_scripts(document, scripts)
    if not scripts:
        return

    interp = Interpreter()
    binding = DOMBinding(document, interp)
    binding.setup()
    interp.global_env.define('XMLHttpRequest', XMLHttpRequest)

    for script_node in scripts:
        src = script_node.attributes.get('src', '').strip() if hasattr(script_node, 'attributes') else ''
        script_type = script_node.attributes.get('type', '').strip().lower() if hasattr(script_node, 'attributes') else ''

        if script_type and script_type not in ('text/javascript', 'application/javascript', 'module', ''):
            continue

        js_code = ''
        if src:
            try:
                from network.http import fetch as fetch_text, resolve_url
                url = resolve_url(base_url, src) if base_url else src
                js_code, _ = fetch_text(url)
            except Exception:
                continue
        else:
            js_code = ''.join(c.data for c in script_node.children if isinstance(c, Text))

        if not js_code or not js_code.strip():
            continue
        try:
            tokens = Lexer(js_code).tokenize()
            ast = Parser(tokens).parse()
            interp.execute(ast)
        except Exception as e:
            print(f'[JS] Script error: {e}', file=sys.stderr)


def _collect_scripts(node, scripts: list) -> None:
    from html.dom import Element
    for child in getattr(node, 'children', []):
        if isinstance(child, Element):
            if child.tag == 'script':
                scripts.append(child)
            else:
                _collect_scripts(child, scripts)


# ---------------------------------------------------------------------------
# Utilities
# ---------------------------------------------------------------------------

def _page_height(document, viewport_height: int = VIEWPORT_H) -> int:
    ref = [viewport_height]

    def walk(n):
        if hasattr(n, 'box') and n.box is not None:
            box = n.box
            bottom = box.y + box.content_height + box.padding.bottom + box.border.bottom
            ref[0] = max(ref[0], bottom)
        for c in n.children:
            walk(c)

    walk(document)
    return int(ref[0]) + 50


def _extract_title(document) -> str:
    def walk(n):
        if getattr(n, 'tag', '') == 'title':
            for c in n.children:
                if c.node_type == 'text' and c.data.strip():
                    return c.data.strip()
        for c in n.children:
            r = walk(c)
            if r:
                return r
        return ''

    return walk(document) or 'rENDER'


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def _default_target() -> str:
    return os.path.join(os.path.dirname(__file__), 'example', 'index.html')


def _build_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description='Render a local HTML file or URL with the rENDER browser engine.'
    )
    parser.add_argument('target', nargs='?', default=_default_target(),
                        help='HTML file path or URL to render')
    parser.add_argument('--width',  type=int, default=VIEWPORT_W, help='Initial viewport width in pixels')
    parser.add_argument('--height', type=int, default=VIEWPORT_H, help='Initial viewport height in pixels')
    return parser


def _parse_args(argv: list[str]):
    return _build_arg_parser().parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(sys.argv[1:] if argv is None else argv)
    from backend.qt.app import Browser
    browser = Browser()
    browser._win.resize(args.width, args.height)
    browser.load(args.target)
    return browser.exec()


if __name__ == '__main__':
    raise SystemExit(main())
