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

def _pipeline(html_content: str, base_url: str = '') -> tuple:
    """Parse → CSS (+ external sheets) → Images → Layout.

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

    css_bind(doc, UA_CSS, viewport_width=VIEWPORT_W, viewport_height=VIEWPORT_H,
             extra_css_texts=css_texts)
    css_compute(doc, viewport_width=VIEWPORT_W, viewport_height=VIEWPORT_H)

    # Attach fetched image data to DOM nodes
    _attach_images(img_data)

    dl = layout_mod.layout(doc, viewport_width=VIEWPORT_W, viewport_height=VIEWPORT_H)
    height = _page_height(doc)
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
    from PyQt6.QtGui import QImage
    from PyQt6.QtCore import QByteArray

    for node, raw in img_data:
        if raw is None:
            continue
        try:
            ba = QByteArray(raw)
            qimg = QImage()
            qimg.loadFromData(ba)
            if not qimg.isNull():
                node.qimage = qimg
                node.natural_width = qimg.width()
                node.natural_height = qimg.height()
        except Exception:
            pass


def _page_height(document) -> int:
    ref = [VIEWPORT_H]
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
    done    = pyqtSignal(object, int, str, str, object)   # display_list, height, title, final_url, links
    error   = pyqtSignal(str)

    def __init__(self, target: str):
        super().__init__()
        self.target = target

    def run(self):
        try:
            target = self.target
            if target.startswith('http://') or target.startswith('https://'):
                from network.http import fetch
                html, final_url = fetch(target)
            else:
                if not os.path.isabs(target):
                    target = os.path.join(os.path.dirname(os.path.abspath(__file__)), target)
                with open(target, encoding='utf-8', errors='replace') as f:
                    html = f.read()
                final_url = 'file:///' + target.replace('\\', '/')

            dl, height, doc = _pipeline(html, base_url=final_url)
            title = _extract_title(doc)
            import layout as layout_mod
            links = layout_mod._extract_links(doc, final_url)
            self.done.emit(dl, height, title, final_url, links)
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
        self._thread = None
        self._loader = None

    def navigate(self, target: str) -> None:
        """Start async load of target (URL or file path). Non-blocking."""
        self._win.set_status('Loading…')
        self._win.address_bar.setText(target)

        # Clean up previous thread
        if self._thread and self._thread.isRunning():
            self._thread.quit()
            self._thread.wait(500)

        self._thread = QThread()
        self._loader = _Loader(target)
        self._loader.moveToThread(self._thread)

        self._thread.started.connect(self._loader.run)
        self._loader.done.connect(self._on_done)
        self._loader.error.connect(self._on_error)
        self._loader.done.connect(self._thread.quit)
        self._loader.error.connect(self._thread.quit)

        self._thread.start()

    def _on_done(self, display_list, height: int, title: str, final_url: str, links: list):
        self._win.set_display_list(display_list, page_height=height, title=title)
        self._win.canvas.set_links(links)
        self._win.address_bar.setText(final_url)
        self._win.set_status('')

    def _on_error(self, msg: str):
        self._win.set_status(f'Error: {msg}')
        print(f'[rENDER] Error: {msg}', file=sys.stderr)

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
