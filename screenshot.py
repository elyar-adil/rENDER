"""Offscreen screenshot tool for rENDER.

Usage:
    python screenshot.py <url_or_file> <output.png> [width] [height]

Renders the page headlessly and saves to a PNG file.
"""
import sys
import os

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')

from PyQt6.QtWidgets import QApplication
from PyQt6.QtGui import QImage, QPainter
from PyQt6.QtCore import Qt


def screenshot(target: str, output: str, width: int = 1200, height: int = 900):
    app = QApplication.instance() or QApplication(sys.argv[:1])

    # Import pipeline
    from engine import _pipeline, VIEWPORT_W, VIEWPORT_H
    import engine as eng
    eng.VIEWPORT_W = width
    eng.VIEWPORT_H = height

    # Fetch content
    if target.startswith('http://') or target.startswith('https://'):
        from network.http import fetch
        html, final_url = fetch(target)
    else:
        if not os.path.isabs(target):
            target = os.path.join(os.path.dirname(os.path.abspath(__file__)), target)
        with open(target, encoding='utf-8', errors='replace') as f:
            html = f.read()
        final_url = 'file:///' + target.replace('\\', '/')

    print(f'[screenshot] Rendering {final_url} ...', flush=True)
    dl, page_height, doc = _pipeline(html, base_url=final_url)

    render_height = min(page_height, height * 3)  # cap at 3 viewports
    print(f'[screenshot] Page height: {page_height}px, rendering: {render_height}px', flush=True)

    img = QImage(width, render_height, QImage.Format.Format_RGB32)
    img.fill(0xFFFFFF)

    from rendering.qt_painter import paint
    painter = QPainter(img)
    painter.setRenderHint(QPainter.RenderHint.Antialiasing)
    paint(dl, painter)
    painter.end()

    img.save(output)
    print(f'[screenshot] Saved → {output}')


if __name__ == '__main__':
    if len(sys.argv) < 3:
        print('Usage: python screenshot.py <url_or_file> <output.png> [width] [height]')
        sys.exit(1)
    url = sys.argv[1]
    out = sys.argv[2]
    w = int(sys.argv[3]) if len(sys.argv) > 3 else 1200
    h = int(sys.argv[4]) if len(sys.argv) > 4 else 900
    screenshot(url, out, w, h)
