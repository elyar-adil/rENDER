"""Tests for engine resource fetching helpers and CLI parsing."""
import os
import sys
import time
import unittest
from unittest.mock import patch


ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if ROOT not in sys.path:
    sys.path.insert(0, ROOT)

import engine
from html.parser import parse as parse_html
from rendering.display_list import DrawImage


class TestEnginePipelineHelpers(unittest.TestCase):
    def test_fetch_subresources_preserves_stylesheet_dom_order(self):
        doc = parse_html(
            """
            <html>
              <head>
                <link rel="stylesheet" href="first.css">
                <link rel="stylesheet" href="second.css">
              </head>
              <body></body>
            </html>
            """
        )

        def fake_fetch(url: str):
            if url == 'first.css':
                time.sleep(0.05)
                return ('body { color: red; }', url)
            return ('body { color: blue; }', url)

        with patch('network.http.fetch', side_effect=fake_fetch) as mocked_fetch:
            css_texts, img_data = engine._fetch_subresources(doc, base_url='')

        self.assertEqual(
            css_texts,
            ['body { color: red; }', 'body { color: blue; }'],
        )
        self.assertEqual(img_data, [])
        self.assertEqual(mocked_fetch.call_count, 2)

    def test_fetch_subresources_deduplicates_duplicate_stylesheet_requests(self):
        doc = parse_html(
            """
            <html>
              <head>
                <link rel="stylesheet" href="shared.css">
                <link rel="stylesheet" href="shared.css">
              </head>
              <body></body>
            </html>
            """
        )

        with patch('network.http.fetch', return_value=('body { color: green; }', 'shared.css')) as mocked_fetch:
            css_texts, _ = engine._fetch_subresources(doc, base_url='')

        self.assertEqual(
            css_texts,
            ['body { color: green; }', 'body { color: green; }'],
        )
        self.assertEqual(mocked_fetch.call_count, 1)

    def test_decode_data_uri_supports_non_base64_payloads(self):
        self.assertEqual(
            engine._decode_data_uri('data:text/plain,hello%20world'),
            b'hello world',
        )

    def test_parse_args_accepts_custom_viewport(self):
        args = engine._parse_args(['https://example.com', '--width', '1280', '--height', '720'])
        self.assertEqual(args.target, 'https://example.com')
        self.assertEqual(args.width, 1280)
        self.assertEqual(args.height, 720)

    def test_detects_browser_hydration_shell_markup(self):
        html = """
            <html>
              <body>
                <div id="root"></div>
                <div id="ssr" data-ssr-entry="/bundles/app.js" hidden></div>
              </body>
            </html>
        """
        self.assertTrue(
            engine._looks_like_browser_hydration_shell(
                html,
                'https://www.msn.cn/zh-cn',
            )
        )

    def test_pipeline_uses_browser_snapshot_for_empty_hydration_shell(self):
        class _FakeImage:
            def width(self):
                return 1280

            def height(self):
                return 777

        html = """
            <html>
              <head><title>MSN</title></head>
              <body>
                <div id="root"></div>
                <div id="ssr" data-ssr-entry="/bundles/app.js" hidden></div>
              </body>
            </html>
        """
        fake_image = _FakeImage()
        with patch('engine._capture_browser_snapshot', return_value=(fake_image, 777)):
            display_list, page_height, document = engine._pipeline(
                html,
                base_url='https://www.msn.cn/zh-cn',
                viewport_width=1280,
                viewport_height=900,
            )

        self.assertEqual(page_height, 777)
        self.assertEqual(len(display_list), 1)
        command = next(iter(display_list))
        self.assertIsInstance(command, DrawImage)
        self.assertIs(command.image_data, fake_image)
        self.assertEqual(document.children[0].tag, 'html')


if __name__ == '__main__':
    unittest.main()
