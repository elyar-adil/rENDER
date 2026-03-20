"""rENDER Browser Engine — main entry point."""
import sys
import os
from concurrent.futures import ThreadPoolExecutor, as_completed

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from PyQt6.QtWidgets import QApplication
from PyQt6.QtCore import QThread, pyqtSignal, QObject

UA_CSS = os.path.join(os.path.dirname(__file__), 'ua', 'user-agent.css')
VIEWPORT_W = 980
VIEWPORT_H = 600


# ---------------------------------------------------------------------------
# Pipeline helpers
# ---------------------------------------------------------------------------

def _pipeline(html_content: str, base_url: str = '',
              viewport_width: int = VIEWPORT_W,
              viewport_height: int = VIEWPORT_H) -> tuple:
    """Parse → JS → CSS (+ external sheets) → Images → Layout.

    External stylesheets and images are fetched concurrently.
    Returns (display_list, page_height, document).
    """
    from html.parser import parse as parse_html
    from css.cascade import bind as css_bind
    from css.computed import compute as css_compute
    import layout as layout_mod

    doc = parse_html(html_content)

    # Collect external <link rel="stylesheet"> hrefs and <img src> nodes
    # concurrently so network round-trips overlap.
    css_texts, img_data = _fetch_subresources(doc, base_url)

    # Execute JavaScript (modifies DOM before layout)
    _execute_scripts(doc, base_url)

    # Re-fetch subresources that JS may have injected
    css_texts2, img_data2 = _fetch_subresources(doc, base_url)
    css_texts.extend(css_texts2)
    img_data.extend(img_data2)

    css_bind(doc, UA_CSS, viewport_width=viewport_width, viewport_height=viewport_height,
             extra_css_texts=css_texts, base_url=base_url)
    css_compute(doc, viewport_width=viewport_width, viewport_height=viewport_height)

    # Attach fetched image data to DOM nodes
    _attach_images(img_data)

    dl = layout_mod.layout(doc, viewport_width=viewport_width, viewport_height=viewport_height)
    height = _page_height(doc, viewport_height=viewport_height)
    return dl, height, doc


def _fetch_subresources(document, base_url: str) -> tuple[list, list]:
    """Concurrently fetch external CSS stylesheets and images.

    Returns:
        css_texts  — list of raw CSS strings from <link rel="stylesheet">
        img_data   — list of (node, raw_bytes_or_None) for <img> nodes
    """
    from html.dom import Element
    from network.http import fetch_bytes, resolve_url, fetch as fetch_text

    # Walk DOM once to collect all tasks
    css_jobs: list[tuple] = []   # (href_url,)
    img_jobs: list[tuple] = []   # (node, url_or_None, data_uri_or_None)

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
                    # Fallback for lazy-loaded images (JS would swap data-src → src)
                    src = node.attributes.get('data-src', '').strip()
                if src.startswith('data:'):
                    img_jobs.append((node, None, src))
                elif src:
                    url = resolve_url(base_url, src) if base_url else src
                    img_jobs.append((node, url, None))
        stack.extend(node.children)

    css_texts: list[str] = []
    img_data: list[tuple] = []   # (node, raw_bytes_or_None)

    if not css_jobs and not img_jobs:
        return css_texts, img_data

    # Each fetch is I/O-bound — use threads.
    # Limit workers to avoid hammering the server with too many simultaneous
    # connections (browsers typically allow 6 per origin).
    max_workers = min(16, len(css_jobs) + len(img_jobs))

    def _fetch_css(url):
        try:
            text, _ = fetch_text(url)
            return ('css', url, text)
        except Exception:
            return ('css', url, None)

    def _fetch_img(node, url, data_uri):
        if data_uri:
            try:
                import base64
                comma = data_uri.index(',')
                raw = base64.b64decode(data_uri[comma + 1:])
                return ('img', node, raw)
            except Exception:
                return ('img', node, None)
        try:
            raw = fetch_bytes(url)
            return ('img', node, raw)
        except Exception:
            return ('img', node, None)

    futures = []
    with ThreadPoolExecutor(max_workers=max_workers) as pool:
        for url in css_jobs:
            futures.append(pool.submit(_fetch_css, url))
        for node, url, data_uri in img_jobs:
            futures.append(pool.submit(_fetch_img, node, url, data_uri))

        for future in as_completed(futures):
            result = future.result()
            if result[0] == 'css':
                if result[2]:
                    css_texts.append(result[2])
            else:
                img_data.append((result[1], result[2]))

    return css_texts, img_data


def _attach_images(img_data: list) -> None:
    """Decode raw bytes and attach QImage to each node."""
    from PyQt6.QtGui import QImage, QPainter
    from PyQt6.QtCore import QByteArray

    for node, raw in img_data:
        if raw is None:
            continue
        try:
            # Detect SVG content
            if _is_svg(raw):
                qimg = _render_svg(raw)
                if qimg and not qimg.isNull():
                    node.qimage = qimg
                    node.natural_width = qimg.width()
                    node.natural_height = qimg.height()
                continue

            ba = QByteArray(raw)
            qimg = QImage()
            qimg.loadFromData(ba)
            if not qimg.isNull():
                node.qimage = qimg
                node.natural_width = qimg.width()
                node.natural_height = qimg.height()
        except Exception:
            pass


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

    # Register XMLHttpRequest constructor
    interp.global_env.define('XMLHttpRequest', XMLHttpRequest)

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
            print(f'[JS] Script error: {e}', file=sys.stderr)


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
        self._win.set_status('Loading…')
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

def main():
    if len(sys.argv) < 2:
        target = os.path.join(os.path.dirname(__file__), 'example', 'index.html')
    else:
        target = sys.argv[1]

    browser = Browser()
    browser.load(target)
    sys.exit(browser.exec())


if __name__ == '__main__':
    main()
