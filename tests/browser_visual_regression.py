"""Browser-vs-rENDER regression runner for hao123 module fixtures.

This script prefers a real browser screenshot as the baseline so we can iterate
module-by-module instead of only eyeballing the full page.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / 'tests' / 'fixtures' / 'hao123_modules' / 'manifest.json'


def _load_manifest() -> list[dict]:
    return json.loads(MANIFEST.read_text(encoding='utf-8'))


def _detect_browser(explicit: str | None) -> list[str] | None:
    if explicit:
        return explicit.split()
    env_cmd = os.environ.get('RENDER_BROWSER_CMD')
    if env_cmd:
        return env_cmd.split()
    for candidate in ('google-chrome', 'google-chrome-stable', 'chromium', 'chromium-browser'):
        path = shutil.which(candidate)
        if path:
            return [path]
    return None


def _browser_screenshot(browser_cmd: list[str], html_path: Path, png_path: Path, width: int, height: int) -> None:
    url = html_path.resolve().as_uri()
    cmd = [
        *browser_cmd,
        '--headless',
        '--disable-gpu',
        f'--window-size={width},{height}',
        f'--screenshot={png_path}',
        url,
    ]
    subprocess.run(cmd, check=True)


def _render_project_screenshot(html_path: Path, png_path: Path, width: int, height: int) -> None:
    cmd = [sys.executable, str(ROOT / 'screenshot.py'), str(html_path), str(png_path), str(width), str(height)]
    subprocess.run(cmd, check=True)


def main() -> int:
    parser = argparse.ArgumentParser(description='Compare hao123 module fixtures against a real browser screenshot.')
    parser.add_argument('--module', dest='module_name', help='Only run the named module fixture')
    parser.add_argument('--browser-cmd', help='Explicit browser command, e.g. "google-chrome"')
    args = parser.parse_args()

    browser_cmd = _detect_browser(args.browser_cmd)
    if not browser_cmd:
        print('No supported browser command found. Set RENDER_BROWSER_CMD or pass --browser-cmd.', file=sys.stderr)
        return 2

    try:
        from PyQt6.QtGui import QImage  # noqa: F401
        from tests.visual_regression import compare_images
    except Exception as exc:
        print(f'PyQt6 visual comparison tooling is unavailable: {exc}', file=sys.stderr)
        return 2

    cases = _load_manifest()
    if args.module_name:
        cases = [case for case in cases if case['name'] == args.module_name]
        if not cases:
            print(f'Unknown module: {args.module_name}', file=sys.stderr)
            return 2

    failed = 0
    with tempfile.TemporaryDirectory(prefix='hao123-browser-diff-') as tmpdir:
        tmpdir = Path(tmpdir)
        for case in cases:
            html_path = ROOT / case['path']
            browser_png = tmpdir / f"{case['name']}_browser.png"
            render_png = tmpdir / f"{case['name']}_render.png"
            try:
                _browser_screenshot(browser_cmd, html_path, browser_png, case['viewport_w'], case['viewport_h'])
                _render_project_screenshot(html_path, render_png, case['viewport_w'], case['viewport_h'])
                from PyQt6.QtGui import QImage
                diff_pct, _ = compare_images(QImage(str(browser_png)), QImage(str(render_png)))
                print(f"{case['name']}: {diff_pct:.2f}% diff")
            except subprocess.CalledProcessError as exc:
                failed += 1
                print(f"{case['name']}: command failed: {exc}", file=sys.stderr)
            except Exception as exc:
                failed += 1
                print(f"{case['name']}: compare failed: {exc}", file=sys.stderr)

    return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
