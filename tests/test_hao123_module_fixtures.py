"""Tests for modular hao123 fixture generation."""
import json
import sys
import os
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from tests.extract_hao123_modules import build, OUT_DIR


def test_builds_expected_module_manifest():
    manifest = build()
    names = [item['name'] for item in manifest]
    assert names == ['header', 'search', 'sites', 'feed_columns']

    manifest_path = OUT_DIR / 'manifest.json'
    assert manifest_path.exists()
    saved = json.loads(manifest_path.read_text(encoding='utf-8'))
    assert saved == manifest


def test_generated_files_are_standalone_html_documents():
    build()
    for path in sorted(OUT_DIR.glob('*.html')):
        html = path.read_text(encoding='utf-8')
        assert '<!doctype html>' in html.lower()
        assert '<meta charset="utf-8">' in html.lower()
        assert '<style' in html.lower()
        assert '<body>' in html.lower()
