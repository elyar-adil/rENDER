"""Offscreen screenshot tool for rENDER.

Usage:
    python screenshot.py <url_or_file> <output.png> [width] [height]

Renders the page headlessly and saves to a PNG file.
"""
from __future__ import annotations

import argparse
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
    from engine import _pipeline

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
    dl, page_height, doc = _pipeline(html, base_url=final_url,
                                     viewport_width=width, viewport_height=height)

    render_height = min(page_height, height * 3)  # cap at 3 viewports
    print(f'[screenshot] Page height: {page_height}px, rendering: {render_height}px', flush=True)

    img = QImage(width, render_height, QImage.Format.Format_RGB32)
    img.fill(0xFFFFFF)

    from backend.qt.painter import paint
    painter = QPainter(img)
    painter.setRenderHint(QPainter.RenderHint.Antialiasing)
    paint(dl, painter)
    painter.end()

    img.save(output)
    print(f'[screenshot] Saved -> {output}')


def _build_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description='Render a page to a PNG file with the rENDER engine.')
    parser.add_argument('target', help='URL or local HTML file to render')
    parser.add_argument('output', help='Output PNG path')
    parser.add_argument('width', nargs='?', type=int, default=1200, help='Viewport width in pixels')
    parser.add_argument('height', nargs='?', type=int, default=900, help='Viewport height in pixels')
    return parser


if __name__ == '__main__':
    args = _build_arg_parser().parse_args()
    screenshot(args.target, args.output, args.width, args.height)
