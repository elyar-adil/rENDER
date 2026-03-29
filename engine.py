"""rENDER Browser Engine — main entry point."""
import argparse
import sys
import os
import re
import json
import shutil
import subprocess
import tempfile
from concurrent.futures import ThreadPoolExecutor, as_completed
from urllib.parse import unquote_to_bytes

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from PyQt6.QtWidgets import QApplication
from PyQt6.QtCore import QThread, pyqtSignal, QObject

UA_CSS = os.path.join(os.path.dirname(__file__), 'ua', 'user-agent.css')
VIEWPORT_W = 980
VIEWPORT_H = 600
_BROWSER_SNAPSHOT_SCRIPT = os.path.join(os.path.dirname(__file__), 'scripts', 'browser_snapshot.mjs')


# ---------------------------------------------------------------------------
# Pipeline helpers
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
    from rendering.display_list import DisplayList, DrawImage

    doc = parse_html(html_content)

    if _looks_like_browser_hydration_shell(html_content, base_url):
        browser_snapshot = _capture_browser_snapshot(base_url, viewport_width, viewport_height)
        if browser_snapshot is not None:
            image, page_height = browser_snapshot
            display_list = DisplayList()
            display_list.add(DrawImage(0.0, 0.0, image, image.width(), image.height()))
            return display_list, max(page_height, image.height()), doc

    _hydrate_bigpipe_pagelets(doc, html_content)

    # Collect external <link rel="stylesheet"> hrefs and <img src> nodes
    # concurrently so network round-trips overlap.
    css_texts, img_data = _fetch_subresources(doc, base_url)

    # Execute JavaScript (modifies DOM before layout)
    _execute_scripts(doc, base_url)
    _prune_loading_placeholders(doc)

    # Re-fetch subresources that JS may have injected
    css_texts2, img_data2 = _fetch_subresources(doc, base_url)
    css_texts.extend(css_texts2)
    img_data.extend(img_data2)

    css_bind(doc, UA_CSS, viewport_width=viewport_width, viewport_height=viewport_height,
             extra_css_texts=css_texts, base_url=base_url)
    css_compute(doc, viewport_width=viewport_width, viewport_height=viewport_height)

    # Attach fetched image data to DOM nodes
    _attach_images(img_data)
    _attach_inline_svgs(doc)
    _attach_images(_fetch_background_images(doc, base_url), attr_name='background_qimage')

    dl = layout_mod.layout(doc, viewport_width=viewport_width, viewport_height=viewport_height)
    height = _page_height(doc, viewport_height=viewport_height)
    return dl, height, doc


def _looks_like_browser_hydration_shell(html_content: str, base_url: str = '') -> bool:
    """Detect pages that ship an empty shell and rely on browser-side hydration.

    MSN currently returns an empty ``#root`` plus an ``#ssr`` marker that points
    at a browser-rendered entry bundle. Our own JS/runtime stack cannot execute
    that application, so we optionally fall back to a browser snapshot instead
    of returning a blank page.
    """
    if not base_url.startswith('http://') and not base_url.startswith('https://'):
        return False
    if 'data-ssr-entry=' not in html_content:
        return False
    if re.search(r'<div\s+id=["\']root["\'][^>]*>\s*</div>', html_content, re.IGNORECASE):
        return True
    return False


def _find_edge_executable() -> str | None:
    candidates = [
        os.environ.get('RENDER_EDGE_PATH', ''),
        r'C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe',
        r'C:\Program Files\Microsoft\Edge\Application\msedge.exe',
        os.path.expandvars(r'%LOCALAPPDATA%\Microsoft\Edge\Application\msedge.exe'),
    ]
    for candidate in candidates:
        if candidate and os.path.exists(candidate):
            return candidate
    return None


def _capture_browser_snapshot(page_url: str, viewport_width: int, viewport_height: int):
    """Render a JS-heavy page in Edge and return a raster snapshot for fallback.

    This is a compatibility escape hatch for pages that only ship an empty HTML
    shell plus client-side hydration entrypoints. We still paint the result via
    our own Qt canvas, but the DOM/CSS execution is delegated to the local Edge
    runtime when our engine would otherwise render a fully blank page.
    """
    if not os.path.exists(_BROWSER_SNAPSHOT_SCRIPT):
        return None

    edge_path = _find_edge_executable()
    node_path = shutil.which('node')
    if not edge_path or not node_path:
        return None

    fd, png_path = tempfile.mkstemp(prefix='render-browser-fallback-', suffix='.png')
    os.close(fd)
    try:
        cmd = [
            node_path,
            _BROWSER_SNAPSHOT_SCRIPT,
            edge_path,
            page_url,
            png_path,
            str(viewport_width),
            str(viewport_height),
        ]
        completed = subprocess.run(
            cmd,
            capture_output=True,
            timeout=45,
            check=False,
        )
        if completed.returncode != 0:
            return None

        metadata = None
        stdout = (completed.stdout or b'').decode('utf-8', errors='replace')
        for line in reversed(stdout.splitlines()):
            line = line.strip()
            if not line:
                continue
            try:
                metadata = json.loads(line)
                break
            except json.JSONDecodeError:
                continue
        if not metadata:
            return None

        from PyQt6.QtGui import QImage
        image = QImage()
        if not image.load(png_path):
            return None
        page_height = int(metadata.get('page_height') or image.height())
        return image, page_height
    except Exception:
        return None
    finally:
        try:
            os.remove(png_path)
        except OSError:
            pass


def _hydrate_bigpipe_pagelets(document, html_content: str) -> None:
    """Inject BigPipe <code><!-- html --></code> payloads into their targets.

    hao123 ships several server-rendered modules in hidden <code> nodes and
    relies on BigPipe.onPageletArrive(...) to move them into placeholder
    containers. Our JS runtime does not implement that loader, so we hydrate
    the static payloads directly before CSS/layout.
    """
    try:
        import re
        from html.parser import parse as parse_html
        from html.dom import Element

        container_map = {}
        for match in re.finditer(
            r'<code\s+id="([^"]+)"[^>]*>\s*<!--\s*(.*?)\s*-->\s*</code>',
            html_content,
            re.IGNORECASE | re.DOTALL,
        ):
            container_id, snippet = match.groups()
            snippet = snippet.strip()
            if snippet:
                container_map[container_id] = snippet

        if not container_map:
            return

        pagelet_map = {}
        for match in re.finditer(
            r'BigPipe\.onPageletArrive\(\{"id":"([^"]+)"[^\)]*?"html":\{"container":"([^"]+)"\}',
            html_content,
            re.IGNORECASE | re.DOTALL,
        ):
            pagelet_id, container_id = match.groups()
            if pagelet_id and container_id in container_map:
                pagelet_map[pagelet_id] = container_map[container_id]

        if not pagelet_map:
            return

        targets = {}
        stack = [document]
        while stack:
            node = stack.pop()
            if isinstance(node, Element):
                node_id = node.attributes.get('id', '')
                if node_id in pagelet_map:
                    targets[node_id] = node
            stack.extend(getattr(node, 'children', []))

        for pagelet_id, snippet in pagelet_map.items():
            target = targets.get(pagelet_id)
            if target is None:
                continue

            fragment_doc = parse_html(f'<html><body><div id="__bigpipe_root__">{snippet}</div></body></html>')
            root = None
            stack = [fragment_doc]
            while stack and root is None:
                node = stack.pop()
                if isinstance(node, Element) and node.attributes.get('id') == '__bigpipe_root__':
                    root = node
                    break
                stack.extend(getattr(node, 'children', []))
            if root is None:
                continue

            target.children = root.children
            for child in target.children:
                child.parent = target
    except Exception:
        return


def _fetch_subresources(document, base_url: str) -> tuple[list, list]:
    """Concurrently fetch external CSS stylesheets and images.

    Returns:
        css_texts  — list of raw CSS strings from <link rel="stylesheet">
        img_data   — list of (node, raw_bytes_or_None) for <img> nodes
    """
    from html.dom import Element
    from network.http import fetch_bytes, resolve_url, fetch as fetch_text

    # Walk DOM once to collect all tasks
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
                    # Fallback for lazy-loaded images (JS would swap data-src -> src)
                    src = node.attributes.get('data-src', '').strip()
                if src.startswith('data:'):
                    img_jobs.append((node, None, src))
                elif src:
                    url = resolve_url(base_url, src) if base_url else src
                    img_jobs.append((node, url, None))
        stack.extend(reversed(node.children))

    css_texts: list[str] = []
    img_data: list[tuple] = []

    if not css_jobs and not img_jobs:
        return css_texts, img_data

    # Each fetch is I/O-bound — use threads.
    # Limit workers to avoid hammering the server with too many simultaneous
    # connections (browsers typically allow 6 per origin).
    max_workers = min(16, len(css_jobs) + len(img_jobs))

    def _fetch_css(url):
        try:
            text, _ = fetch_text(url)
            return text
        except Exception:
            return None

    css_results: list[str | None] = [None] * len(css_jobs)
    with ThreadPoolExecutor(max_workers=max_workers) as pool:
        css_futures_by_url = {}
        css_future_map = {}
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

    css_texts = [text for text in css_results if text]
    img_data = _fetch_binary_jobs(
        img_jobs,
        fetcher=fetch_bytes,
        max_workers=max_workers,
    )

    return css_texts, img_data


def _extract_background_image_url(style: dict) -> str:
    bg_image = (style or {}).get('background-image', '')
    if not bg_image or bg_image in ('none', ''):
        return ''
    m = re.search(r'url\((["\']?)([^)"\']+)\1\)', bg_image)
    return m.group(2).strip() if m else ''


def _fetch_background_images(document, base_url: str) -> list:
    from html.dom import Element
    from network.http import fetch_bytes, resolve_url

    jobs = []
    stack = [document]
    while stack:
        node = stack.pop()
        if isinstance(node, Element):
            url = _extract_background_image_url(getattr(node, 'style', {}) or {})
            if url:
                if url.startswith('data:'):
                    jobs.append((node, None, url))
                else:
                    jobs.append((node, resolve_url(base_url, url) if base_url else url, None))
        stack.extend(reversed(getattr(node, 'children', [])))

    return _fetch_binary_jobs(
        jobs,
        fetcher=fetch_bytes,
        max_workers=min(16, len(jobs) or 1),
    )


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
        futures_by_url = {}
        future_map = {}
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


def _attach_images(img_data: list, attr_name: str = 'qimage') -> None:
    """Decode raw bytes and attach QImage to each node."""
    for node, raw in img_data:
        if raw is None:
            continue
        try:
            qimg = _decode_image(raw)
            if not qimg.isNull():
                setattr(node, attr_name, qimg)
                if attr_name == 'qimage':
                    node.natural_width = qimg.width()
                    node.natural_height = qimg.height()
        except Exception:
            pass


def _attach_inline_svgs(document) -> None:
    """Rasterize inline <svg> elements so layout can treat them like images."""
    from html.dom import Element, Text
    import html as html_mod

    def _serialize(node) -> str:
        if isinstance(node, Text):
            return html_mod.escape(node.data, quote=False)
        if not isinstance(node, Element):
            return ''
        attrs = ''.join(
            f' {name}="{html_mod.escape(str(value), quote=True)}"'
            for name, value in node.attributes.items()
        )
        inner = ''.join(_serialize(child) for child in node.children)
        return f'<{node.tag}{attrs}>{inner}</{node.tag}>'

    stack = [document]
    while stack:
        node = stack.pop()
        if isinstance(node, Element) and node.tag == 'svg':
            try:
                qimg = _render_svg(_serialize(node).encode('utf-8'))
                if qimg is not None and not qimg.isNull():
                    node.qimage = qimg
                    node.natural_width = qimg.width()
                    node.natural_height = qimg.height()
            except Exception:
                pass
        stack.extend(getattr(node, 'children', []))


def _prune_loading_placeholders(document) -> None:
    """Remove unresolved skeleton/loading placeholders with no real content."""
    from html.dom import Element, Text

    def _has_meaningful_content(node) -> bool:
        if isinstance(node, Text):
            return bool(node.data.strip())
        for child in getattr(node, 'children', []):
            if _has_meaningful_content(child):
                return True
        return False

    stack = [document]
    while stack:
        node = stack.pop()
        if not hasattr(node, 'children'):
            continue
        kept = []
        for child in getattr(node, 'children', []):
            if isinstance(child, Element):
                cls = child.attributes.get('class', '').lower()
                if 'skeleton' in cls and not _has_meaningful_content(child):
                    continue
            kept.append(child)
        node.children = kept
        for child in node.children:
            child.parent = node
            stack.append(child)


def _decode_image(raw: bytes):
    from PyQt6.QtGui import QImage
    from PyQt6.QtCore import QByteArray

    if _is_svg(raw):
        qimg = _render_svg(raw)
        return qimg or QImage()

    ba = QByteArray(raw)
    qimg = QImage()
    qimg.loadFromData(ba)
    return qimg


def _is_svg(raw: bytes) -> bool:
    """Check if raw bytes look like SVG content."""
    head = raw[:200].lstrip()
    return (head.startswith(b'<?xml') and b'<svg' in raw[:500]) or \
           head.startswith(b'<svg') or \
           (head.startswith(b'<!') and b'<svg' in raw[:500])


def _render_svg(raw: bytes):
    """Render SVG bytes to a QImage. Returns QImage or None."""
    try:
        from PyQt6.QtSvg import QSvgRenderer
        from PyQt6.QtGui import QImage, QPainter
        from PyQt6.QtCore import QByteArray
        renderer = QSvgRenderer(QByteArray(raw))
        if not renderer.isValid():
            return None
        size = renderer.defaultSize()
        if size.width() <= 0 or size.height() <= 0:
            size = renderer.viewBox().size()
        if size.width() <= 0 or size.height() <= 0:
            return None
        img = QImage(size, QImage.Format.Format_ARGB32)
        img.fill(0)
        painter = QPainter(img)
        renderer.render(painter)
        painter.end()
        return img
    except ImportError:
        return None
    except Exception:
        return None


def _execute_scripts(document, base_url: str) -> None:
    """Find and execute <script> elements in DOM order."""
    from html.dom import Element, Text
    from js.lexer import Lexer
    from js.parser import Parser
    from js.interpreter import Interpreter
    from js.dom_api import DOMBinding
    from js.xhr import XMLHttpRequest

    # Collect all <script> elements in document order
    scripts = []
    _collect_scripts(document, scripts)

    if not scripts:
        return

    # Create a single interpreter instance shared across all scripts
    interp = Interpreter()
    binding = DOMBinding(document, interp)
    binding.setup()

    if base_url:
        from urllib.parse import urlparse
        parsed = urlparse(base_url)
        window = interp.global_env.get('window')
        if isinstance(window, dict):
            window['location'] = {
                'href': base_url,
                'hostname': parsed.hostname or '',
                'pathname': parsed.path or '/',
                'protocol': f'{parsed.scheme}:' if parsed.scheme else '',
                'origin': f'{parsed.scheme}://{parsed.netloc}' if parsed.scheme and parsed.netloc else '',
            }
            window['history'] = interp.global_env.get('history')
        doc_obj = interp.global_env.get('document')
        if isinstance(doc_obj, dict):
            doc_obj['location'] = window['location']
            doc_obj['URL'] = base_url

    # Register XMLHttpRequest constructor
    interp.global_env.define('XMLHttpRequest', lambda: XMLHttpRequest(base_url=base_url))

    for script_node in scripts:
        src = script_node.attributes.get('src', '').strip() if hasattr(script_node, 'attributes') else ''
        script_type = script_node.attributes.get('type', '').strip().lower() if hasattr(script_node, 'attributes') else ''

        # Skip non-JS scripts (e.g. type="application/json", type="text/template")
        if script_type and script_type not in ('text/javascript', 'application/javascript',
                                                 'module', ''):
            continue

        js_code = ''
        if src:
            # External script
            try:
                from network.http import fetch as fetch_text, resolve_url
                url = resolve_url(base_url, src) if base_url else src
                js_code, _ = fetch_text(url)
            except Exception:
                continue
        else:
            # Inline script
            js_code = ''.join(
                c.data for c in script_node.children
                if isinstance(c, Text)
            )

        if not js_code or not js_code.strip():
            continue

        try:
            lexer = Lexer(js_code)
            tokens = lexer.tokenize()
            parser = Parser(tokens)
            ast = parser.parse()
            interp.execute(ast)
        except Exception as e:
            label = src or '<inline>'
            print(f'[JS] Script error in {label}: {_summarize_js_error(e)}', file=sys.stderr)


def _summarize_js_error(err: Exception) -> str:
    value = getattr(err, 'value', None)
    if isinstance(value, dict):
        name = value.get('name')
        message = value.get('message')
        if name and message:
            msg = f'{name}: {message}'
        elif message:
            msg = str(message)
        else:
            msg = str(value).strip() or err.__class__.__name__
    else:
        msg = str(err).strip() or err.__class__.__name__
    if len(msg) > 220:
        msg = msg[:217] + '...'
    return msg


def _collect_scripts(node, scripts):
    """Collect <script> elements in document order."""
    from html.dom import Element
    for child in getattr(node, 'children', []):
        if isinstance(child, Element):
            if child.tag == 'script':
                scripts.append(child)
            else:
                _collect_scripts(child, scripts)


def _page_height(document, viewport_height: int = VIEWPORT_H) -> int:
    ref = [viewport_height]
    def walk(n):
        if hasattr(n, 'box') and n.box is not None:
            ref[0] = max(ref[0], n.box.y + n.box.content_height)
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
# Background loader thread
# ---------------------------------------------------------------------------

class _Loader(QObject):
    """Fetches + pipelines a page in a worker thread."""
    done    = pyqtSignal(object, int, str, str, object, str)   # display_list, height, title, final_url, links, html
    error   = pyqtSignal(str)

    def __init__(self, target: str | None = None, *,
                 html_content: str | None = None, base_url: str = '',
                 viewport_width: int = VIEWPORT_W, viewport_height: int = VIEWPORT_H):
        super().__init__()
        self.target = target
        self.html_content = html_content
        self.base_url = base_url
        self.viewport_width = viewport_width
        self.viewport_height = viewport_height

    def run(self):
        try:
            if self.html_content is not None:
                html = self.html_content
                final_url = self.base_url
            else:
                target = self.target or ''
                if target.startswith('http://') or target.startswith('https://'):
                    from network.http import fetch
                    html, final_url = fetch(target)
                else:
                    if not os.path.isabs(target):
                        target = os.path.join(os.path.dirname(os.path.abspath(__file__)), target)
                    with open(target, encoding='utf-8', errors='replace') as f:
                        html = f.read()
                    final_url = 'file:///' + target.replace('\\', '/')

            dl, height, doc = _pipeline(
                html,
                base_url=final_url,
                viewport_width=self.viewport_width,
                viewport_height=self.viewport_height,
            )
            title = _extract_title(doc)
            import layout as layout_mod
            links = layout_mod._extract_links(doc, final_url)
            self.done.emit(dl, height, title, final_url, links, html)
        except Exception as e:
            self.error.emit(str(e))


# ---------------------------------------------------------------------------
# Browser controller
# ---------------------------------------------------------------------------

class Browser:
    def __init__(self):
        self._app = QApplication.instance() or QApplication(sys.argv)
        from rendering.qt_painter import BrowserWidget
        self._win = BrowserWidget('rENDER')
        self._win.navigate_callback = self.navigate
        self._win.viewport_changed.connect(self._on_viewport_changed)
        self._thread = None
        self._loader = None
        self._current_html = None
        self._current_url = ''
        self._current_target = ''

    def navigate(self, target: str) -> None:
        """Start async load of target (URL or file path). Non-blocking."""
        self._win.set_status('Loading...')
        self._win.address_bar.setText(target)
        self._current_target = target

        # Clean up previous thread
        if self._thread and self._thread.isRunning():
            self._thread.quit()
            self._thread.wait(500)

        self._thread = QThread()
        vw, vh = self._win.viewport_size()
        self._loader = _Loader(target, viewport_width=vw, viewport_height=vh)
        self._loader.moveToThread(self._thread)

        self._thread.started.connect(self._loader.run)
        self._loader.done.connect(self._on_done)
        self._loader.error.connect(self._on_error)
        self._loader.done.connect(self._thread.quit)
        self._loader.error.connect(self._thread.quit)

        self._thread.start()

    def _on_done(self, display_list, height: int, title: str, final_url: str, links: list, html: str):
        self._win.set_display_list(display_list, page_height=height, title=title)
        self._win.canvas.set_links(links)
        self._win.address_bar.setText(final_url)
        self._win.set_status('')
        self._current_html = html
        self._current_url = final_url

    def _on_error(self, msg: str):
        self._win.set_status(f'Error: {msg}')
        print(f'[rENDER] Error: {msg}', file=sys.stderr)

    def _on_viewport_changed(self, viewport_width: int, viewport_height: int) -> None:
        if not self._current_html or not self._current_url:
            return
        if self._thread and self._thread.isRunning():
            return

        self._thread = QThread()
        self._loader = _Loader(
            html_content=self._current_html,
            base_url=self._current_url,
            viewport_width=viewport_width,
            viewport_height=viewport_height,
        )
        self._loader.moveToThread(self._thread)

        self._thread.started.connect(self._loader.run)
        self._loader.done.connect(self._on_done)
        self._loader.error.connect(self._on_error)
        self._loader.done.connect(self._thread.quit)
        self._loader.error.connect(self._thread.quit)

        self._thread.start()

    def load(self, target: str) -> None:
        self._win.show()
        self.navigate(target)

    def exec(self) -> int:
        return self._app.exec()


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def _default_target() -> str:
    return os.path.join(os.path.dirname(__file__), 'example', 'index.html')


def _build_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description='Render a local HTML file or URL with the rENDER browser engine.'
    )
    parser.add_argument(
        'target',
        nargs='?',
        default=_default_target(),
        help='HTML file path or URL to render',
    )
    parser.add_argument('--width', type=int, default=VIEWPORT_W, help='Initial viewport width in pixels')
    parser.add_argument('--height', type=int, default=VIEWPORT_H, help='Initial viewport height in pixels')
    return parser


def _parse_args(argv: list[str]):
    return _build_arg_parser().parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(sys.argv[1:] if argv is None else argv)
    browser = Browser()
    browser._win.resize(args.width, args.height)
    browser.load(args.target)
    return browser.exec()


if __name__ == '__main__':
    raise SystemExit(main())
