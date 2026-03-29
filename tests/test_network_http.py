"""Tests for HTTP helpers."""
import sys
import os
import ssl
import urllib.error
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import unittest
from unittest.mock import patch

from network.http import _detect_charset, clear_cache, fetch, fetch_bytes


class _FakeResponse:
    def __init__(self, url: str, body: bytes, *, content_type: str = 'text/html; charset=utf-8',
                 content_encoding: str = ''):
        self.url = url
        self._body = body
        self.headers = {
            'Content-Type': content_type,
            'Content-Encoding': content_encoding,
        }

    def read(self) -> bytes:
        return self._body

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, tb):
        return False


class TestCharsetDetection(unittest.TestCase):
    def tearDown(self):
        clear_cache()

    def test_detects_http_equiv_meta_charset(self):
        raw = (
            b"<html><head>"
            b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=gb2312\">"
            b"</head><body></body></html>"
        )
        self.assertEqual(_detect_charset('', raw), 'gb18030')

    def test_scans_past_wayback_preamble_for_meta_charset(self):
        raw = (
            b"<html><head>" + b" " * 3000 +
            b"<meta charset=\"gbk\">"
            b"</head><body></body></html>"
        )
        self.assertEqual(_detect_charset('', raw), 'gb18030')

    def test_header_charset_takes_precedence(self):
        raw = b'<meta charset="gbk">'
        self.assertEqual(
            _detect_charset('text/html; charset=UTF-8', raw),
            'utf-8',
        )

    def test_text_and_binary_cache_entries_do_not_collide(self):
        url = 'https://example.com/asset'
        responses = [
            _FakeResponse(url, b'<html><body>ok</body></html>'),
            _FakeResponse(url, b'\x89PNG\r\n\x1a\n', content_type='image/png'),
        ]

        with patch('network.http.urllib.request.urlopen', side_effect=responses) as mocked_urlopen:
            html, final_url = fetch(url)
            raw = fetch_bytes(url)
            cached_html, cached_final_url = fetch(url)
            cached_raw = fetch_bytes(url)

        self.assertEqual(final_url, url)
        self.assertEqual(cached_final_url, url)
        self.assertIn('ok', html)
        self.assertEqual(cached_html, html)
        self.assertEqual(raw, b'\x89PNG\r\n\x1a\n')
        self.assertEqual(cached_raw, raw)
        self.assertEqual(mocked_urlopen.call_count, 2)

    def test_retries_with_unverified_context_on_cert_verification_failure(self):
        url = 'https://example.com/style.css'
        cert_error = None
        try:
            raise ssl.SSLCertVerificationError("certificate verify failed")
        except ssl.SSLCertVerificationError as exc:
            cert_error = exc

        def fake_urlopen(*args, **kwargs):
            if 'context' not in kwargs:
                raise urllib.error.URLError(cert_error)
            return _FakeResponse(
                url,
                b'body { color: red; }',
                content_type='text/css; charset=utf-8',
            )

        with patch('network.http.urllib.request.urlopen', side_effect=fake_urlopen) as mocked_urlopen:
            css_text, final_url = fetch(url)

        self.assertEqual(final_url, url)
        self.assertIn('color: red', css_text)
        self.assertEqual(mocked_urlopen.call_count, 2)


if __name__ == '__main__':
    unittest.main()
