"""Tests for HTTP charset detection helpers."""
import sys
import os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import unittest

from network.http import _detect_charset


class TestCharsetDetection(unittest.TestCase):
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


if __name__ == '__main__':
    unittest.main()
