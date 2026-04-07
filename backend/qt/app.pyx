"""Qt application controller — Browser window and background page loader."""
from __future__ import annotations

import os
import sys

from PyQt6.QtWidgets import QApplication
from PyQt6.QtCore import QThread, pyqtSignal, QObject

VIEWPORT_W = 980
VIEWPORT_H = 600


class _Loader(QObject):
    """Fetches and pipelines a page in a worker thread."""
    done  = pyqtSignal(object, int, str, str, object, str)  # dl, height, title, url, links, html
    error = pyqtSignal(str)

    def __init__(self, target: str | None = None, *,
                 html_content: str | None = None, base_url: str = '',
                 viewport_width: int = VIEWPORT_W, viewport_height: int = VIEWPORT_H):
        super().__init__()
        self.target = target
        self.html_content = html_content
        self.base_url = base_url
        self.viewport_width = viewport_width
        self.viewport_height = viewport_height

    def run(self) -> None:
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
                        target = os.path.join(
                            os.path.dirname(os.path.abspath(__file__)), '..', '..', target
                        )
                        target = os.path.normpath(target)
                    with open(target, encoding='utf-8', errors='replace') as f:
                        html = f.read()
                    final_url = 'file:///' + target.replace('\\', '/')

            from engine import _pipeline, _extract_title
            import layout as layout_mod
            dl, height, doc = _pipeline(
                html,
                base_url=final_url,
                viewport_width=self.viewport_width,
                viewport_height=self.viewport_height,
            )
            title = _extract_title(doc)
            links = layout_mod._extract_links(doc, final_url)
            self.done.emit(dl, height, title, final_url, links, html)
        except Exception as e:
            self.error.emit(str(e))


class Browser:
    """Top-level browser controller: owns the QApplication and BrowserWidget."""

    def __init__(self):
        self._app = QApplication.instance() or QApplication(sys.argv)
        from backend.qt.painter import BrowserWidget
        self._win = BrowserWidget('rENDER')
        self._win.navigate_callback = self.navigate
        self._win.back_callback = self.go_back
        self._win.forward_callback = self.go_forward
        self._win.viewport_changed.connect(self._on_viewport_changed)
        self._thread: QThread | None = None
        self._loader: _Loader | None = None
        self._current_html: str | None = None
        self._current_url: str = ''
        self._history: list[str] = []
        self._history_pos: int = -1
        self._is_history_nav: bool = False

    def navigate(self, target: str) -> None:
        """Start async load of *target* (URL or file path).  Non-blocking."""
        self._is_history_nav = False
        self._start_load(target)

    def go_back(self) -> None:
        if self._history_pos > 0:
            self._history_pos -= 1
            self._is_history_nav = True
            self._start_load(self._history[self._history_pos])

    def go_forward(self) -> None:
        if self._history_pos < len(self._history) - 1:
            self._history_pos += 1
            self._is_history_nav = True
            self._start_load(self._history[self._history_pos])

    def _start_load(self, target: str) -> None:
        self._win.set_status('Loading...')
        self._win.address_bar.setText(target)

        if self._thread and self._thread.isRunning():
            self._thread.quit()
            self._thread.wait(500)

        vw, vh = self._win.viewport_size()
        self._loader = _Loader(target, viewport_width=vw, viewport_height=vh)
        self._thread = QThread()
        self._loader.moveToThread(self._thread)
        self._thread.started.connect(self._loader.run)
        self._loader.done.connect(self._on_done)
        self._loader.error.connect(self._on_error)
        self._loader.done.connect(self._thread.quit)
        self._loader.error.connect(self._thread.quit)
        self._thread.start()

    def _on_done(self, display_list, height: int, title: str,
                 final_url: str, links: list, html: str) -> None:
        self._win.set_display_list(display_list, page_height=height, title=title)
        self._win.canvas.set_links(links)
        self._win.address_bar.setText(final_url)
        self._win.set_status('')
        self._current_html = html
        self._current_url = final_url

        if not self._is_history_nav:
            # Truncate forward history, then push new entry
            self._history = self._history[:self._history_pos + 1]
            if not self._history or self._history[-1] != final_url:
                self._history.append(final_url)
            self._history_pos = len(self._history) - 1

        self._win.back_btn.setEnabled(self._history_pos > 0)
        self._win.forward_btn.setEnabled(self._history_pos < len(self._history) - 1)

    def _on_error(self, msg: str) -> None:
        self._win.set_status(f'Error: {msg}')
        print(f'[rENDER] Error: {msg}', file=sys.stderr)

    def _on_viewport_changed(self, viewport_width: int, viewport_height: int) -> None:
        if not self._current_html or not self._current_url:
            return
        if self._thread and self._thread.isRunning():
            return

        self._loader = _Loader(
            html_content=self._current_html,
            base_url=self._current_url,
            viewport_width=viewport_width,
            viewport_height=viewport_height,
        )
        self._thread = QThread()
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
