"""Build standalone hao123 module fixtures from example/hao123.html.

These fixtures let us regression-test complex layout regions independently
instead of only comparing the full page.
"""
from __future__ import annotations

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'example' / 'hao123.html'
OUT_DIR = ROOT / 'tests' / 'fixtures' / 'hao123_modules'

MODULES = [
    {
        'name': 'header',
        'start': '<div id="topColumn" class="layout-header sam-A">',
        'end': '<div class="searchWrapper">',
        'viewport_w': 1280,
        'viewport_h': 140,
    },
    {
        'name': 'search',
        'start': '<div class="searchWrapper">',
        'end': '<div class="layout-main">',
        'viewport_w': 1280,
        'viewport_h': 260,
    },
    {
        'name': 'sites',
        'start': '<div id="sites2_wrapper" class="sites2-wrapper g-ib" monkey="site" >',
        'end': '<div id=\'popup-site-holder\'',
        'viewport_w': 1280,
        'viewport_h': 360,
    },
    {
        'name': 'feed_columns',
        'start': '<div class="g-ib layout-right" id="leftWrapperBox">',
        'end': '<div id=\'shortcut-box\'',
        'viewport_w': 1280,
        'viewport_h': 420,
    },
]


def _clean_head(html: str) -> str:
    head_match = re.search(r'<head>(.*?)</head>', html, re.S | re.I)
    if not head_match:
        raise ValueError('Could not find <head> in hao123 fixture source')
    head = head_match.group(1)
    head = re.sub(r'<script\b.*?</script>', '', head, flags=re.S | re.I)
    head = re.sub(r'<noscript\b.*?</noscript>', '', head, flags=re.S | re.I)
    return head.strip()


def _slice_module(html: str, start: str, end: str) -> str:
    start_idx = html.find(start)
    if start_idx == -1:
        raise ValueError(f'Module start marker not found: {start}')
    end_idx = html.find(end, start_idx)
    if end_idx == -1:
        raise ValueError(f'Module end marker not found: {end}')
    return html[start_idx:end_idx].strip()


TEMPLATE = """<!doctype html>
<html>
<head>
<meta charset="utf-8">
{head}
</head>
<body>
{body}
</body>
</html>
"""


def build() -> list[dict]:
    html = SOURCE.read_text(encoding='utf-8')
    head = _clean_head(html)
    OUT_DIR.mkdir(parents=True, exist_ok=True)

    manifest = []
    for module in MODULES:
        body = _slice_module(html, module['start'], module['end'])
        out_path = OUT_DIR / f"{module['name']}.html"
        out_path.write_text(TEMPLATE.format(head=head, body=body), encoding='utf-8')
        manifest.append({
            'name': module['name'],
            'path': str(out_path.relative_to(ROOT)).replace('\\', '/'),
            'viewport_w': module['viewport_w'],
            'viewport_h': module['viewport_h'],
        })

    manifest_path = OUT_DIR / 'manifest.json'
    manifest_path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    return manifest


if __name__ == '__main__':
    for item in build():
        print(f"wrote {item['path']}")
