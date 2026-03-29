"""HTTP/HTTPS client using Python stdlib urllib."""
import codecs
import ssl
import urllib.request
import urllib.parse
import urllib.error
import re
import threading
from collections import OrderedDict


_TEXT_CACHE_KIND = 'text'
_BYTES_CACHE_KIND = 'bytes'


class _HTTPCache:
    """Thread-safe LRU cache for HTTP responses."""
    def __init__(self, maxsize: int = 256):
        self._cache: OrderedDict = OrderedDict()
        self._lock = threading.Lock()
        self._maxsize = maxsize

    def get(self, kind: str, url: str):
        key = (kind, url)
        with self._lock:
            if key in self._cache:
                self._cache.move_to_end(key)
                return self._cache[key]
        return None

    def put(self, kind: str, url: str, value) -> None:
        key = (kind, url)
        with self._lock:
            if key in self._cache:
                self._cache.move_to_end(key)
            self._cache[key] = value
            if len(self._cache) > self._maxsize:
                self._cache.popitem(last=False)

    def clear(self) -> None:
        with self._lock:
            self._cache.clear()


_cache = _HTTPCache()


def clear_cache() -> None:
    """Clear the HTTP session cache."""
    _cache.clear()


def _is_cert_verification_failure(exc: Exception) -> bool:
    """Return True when urllib failed due to TLS certificate verification."""
    seen = set()
    current = exc
    while current is not None and id(current) not in seen:
        seen.add(id(current))
        if isinstance(current, ssl.SSLCertVerificationError):
            return True
        if isinstance(current, ssl.SSLError) and 'CERTIFICATE_VERIFY_FAILED' in str(current):
            return True
        current = getattr(current, 'reason', None) or getattr(current, '__cause__', None)
    return False


def _urlopen_with_cert_fallback(req, *, timeout: int):
    """Open a request, retrying once without certificate verification on TLS failures.

    Some CDN endpoints used by real pages fail certificate validation on local
    Python installs with stale trust stores. Browsers still load them, so retry
    once with an unverified context to keep page rendering working.
    """
    try:
        return urllib.request.urlopen(req, timeout=timeout)
    except urllib.error.URLError as exc:
        if not _is_cert_verification_failure(exc):
            raise
        insecure_context = ssl._create_unverified_context()
        return urllib.request.urlopen(req, timeout=timeout, context=insecure_context)


def fetch(url: str) -> tuple[str, str]:
    """Fetch URL and return (html_content, final_url).

    Handles:
    - HTTP and HTTPS
    - Redirects (urllib handles automatically)
    - Charset detection: Content-Type header > BOM > meta charset > UTF-8 fallback
    - Returns (html_text, final_url)
    """
    cached = _cache.get(_TEXT_CACHE_KIND, url)
    if cached is not None:
        return cached

    req = urllib.request.Request(url, headers={
        'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36',
        'Accept': 'text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8',
        'Accept-Language': 'zh-CN,zh;q=0.9,en-US;q=0.8,en;q=0.7',
        'Accept-Encoding': 'gzip, deflate',
    })

    with _urlopen_with_cert_fallback(req, timeout=15) as response:
        final_url = response.url
        content_type = response.headers.get('Content-Type', '')
        encoding_header = response.headers.get('Content-Encoding', '')
        raw = response.read()

    raw = _decode_content_encoding(raw, encoding_header)

    # Charset detection
    charset = _detect_charset(content_type, raw)
    html = raw.decode(charset, errors='replace')
    result = html, final_url
    _cache.put(_TEXT_CACHE_KIND, url, result)
    return result


def _detect_charset(content_type: str, raw: bytes) -> str:
    # 1. Content-Type header: text/html; charset=utf-8
    m = re.search(r'charset=([\w-]+)', content_type, re.I)
    if m:
        return _normalize_charset(m.group(1))

    # 2. BOM detection
    if raw.startswith(b'\xef\xbb\xbf'):
        return 'utf-8-sig'
    if raw.startswith(b'\xff\xfe'):
        return 'utf-16-le'
    if raw.startswith(b'\xfe\xff'):
        return 'utf-16-be'

    # 3. meta charset/http-equiv in first 16 KB
    # Wayback toolbar/script injection can push the original charset declaration
    # well past the first 1024 bytes on archived pages.
    meta_charset = _extract_meta_charset(raw[:16384])
    if meta_charset:
        return meta_charset

    # 4. Default
    return 'utf-8'


def _extract_meta_charset(raw_head: bytes) -> str | None:
    head = raw_head.decode('ascii', errors='replace')

    # HTML5 form: <meta charset="utf-8">
    m = re.search(r'<meta[^>]+charset\s*=\s*["\']?([^\s"\'>/;]+)', head, re.I)
    if m:
        return _normalize_charset(m.group(1))

    # Legacy form: <meta http-equiv="Content-Type" content="text/html; charset=gb2312">
    for meta_tag in re.findall(r'<meta\b[^>]*>', head, re.I):
        if not re.search(r'http-equiv\s*=\s*["\']?content-type["\']?', meta_tag, re.I):
            continue
        m = re.search(r'content\s*=\s*(?:["\']([^"\']*)["\']|([^\s>]+))', meta_tag, re.I)
        if not m:
            continue
        content_value = m.group(1) or m.group(2) or ''
        m2 = re.search(r'charset\s*=\s*([\w-]+)', content_value, re.I)
        if m2:
            return _normalize_charset(m2.group(1))

    return None


def _normalize_charset(charset: str) -> str:
    charset = charset.strip().strip(';').strip('"\'').lower()

    # Treat legacy Chinese encodings as gb18030 so archived GB2312/GBK pages
    # round-trip without mojibake. Python's codec aliases often handle this
    # already, but normalizing here avoids environment-specific differences.
    if charset in {'gb2312', 'gbk', 'gb_2312', 'x-gbk'}:
        return 'gb18030'

    try:
        return codecs.lookup(charset).name
    except LookupError:
        return charset


def resolve_url(base_url: str, url: str) -> str:
    """Resolve relative URL against base URL."""
    return urllib.parse.urljoin(base_url, url)


def fetch_bytes(url: str, base_url: str = '') -> bytes:
    """Fetch a resource and return raw bytes. Used for images etc."""
    if base_url:
        url = resolve_url(base_url, url)

    cached = _cache.get(_BYTES_CACHE_KIND, url)
    if cached is not None:
        return cached

    req = urllib.request.Request(url, headers={
        'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36',
        'Accept': 'image/webp,image/apng,image/*,*/*;q=0.8',
        'Accept-Encoding': 'gzip, deflate',
    })
    with _urlopen_with_cert_fallback(req, timeout=10) as resp:
        raw = resp.read()
        encoding = resp.headers.get('Content-Encoding', '')
    raw = _decode_content_encoding(raw, encoding)
    _cache.put(_BYTES_CACHE_KIND, url, raw)
    return raw


def _decode_content_encoding(raw: bytes, encoding_header: str) -> bytes:
    encoding = (encoding_header or '').lower()
    if 'gzip' in encoding:
        import gzip
        try:
            return gzip.decompress(raw)
        except Exception:
            return raw
    if 'deflate' in encoding:
        import zlib
        try:
            return zlib.decompress(raw)
        except Exception:
            try:
                return zlib.decompress(raw, -zlib.MAX_WBITS)
            except Exception:
                return raw
    return raw
