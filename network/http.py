"""HTTP/HTTPS client using Python stdlib urllib."""
import urllib.request
import urllib.parse
import urllib.error
import re
import threading
from collections import OrderedDict


class _HTTPCache:
    """Thread-safe LRU cache for HTTP responses."""
    def __init__(self, maxsize: int = 256):
        self._cache: OrderedDict = OrderedDict()
        self._lock = threading.Lock()
        self._maxsize = maxsize

    def get(self, url: str):
        with self._lock:
            if url in self._cache:
                self._cache.move_to_end(url)
                return self._cache[url]
        return None

    def put(self, url: str, value) -> None:
        with self._lock:
            if url in self._cache:
                self._cache.move_to_end(url)
            self._cache[url] = value
            if len(self._cache) > self._maxsize:
                self._cache.popitem(last=False)

    def clear(self) -> None:
        with self._lock:
            self._cache.clear()


_cache = _HTTPCache()


def clear_cache() -> None:
    """Clear the HTTP session cache."""
    _cache.clear()


def fetch(url: str) -> tuple[str, str]:
    """Fetch URL and return (html_content, final_url).

    Handles:
    - HTTP and HTTPS
    - Redirects (urllib handles automatically)
    - Charset detection: Content-Type header > BOM > meta charset > UTF-8 fallback
    - Returns (html_text, final_url)
    """
    cached = _cache.get(url)
    if cached is not None:
        return cached

    req = urllib.request.Request(url, headers={
        'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36',
        'Accept': 'text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8',
        'Accept-Language': 'zh-CN,zh;q=0.9,en-US;q=0.8,en;q=0.7',
        'Accept-Encoding': 'gzip, deflate',
    })

    with urllib.request.urlopen(req, timeout=15) as response:
        final_url = response.url
        content_type = response.headers.get('Content-Type', '')
        encoding_header = response.headers.get('Content-Encoding', '')
        raw = response.read()

    # Decompress if needed
    if 'gzip' in encoding_header:
        import gzip
        try:
            raw = gzip.decompress(raw)
        except Exception:
            pass
    elif 'deflate' in encoding_header:
        import zlib
        try:
            raw = zlib.decompress(raw)
        except Exception:
            try:
                raw = zlib.decompress(raw, -zlib.MAX_WBITS)
            except Exception:
                pass

    # Charset detection
    charset = _detect_charset(content_type, raw)
    html = raw.decode(charset, errors='replace')
    result = html, final_url
    _cache.put(url, result)
    return result


def _detect_charset(content_type: str, raw: bytes) -> str:
    # 1. Content-Type header: text/html; charset=utf-8
    m = re.search(r'charset=([\w-]+)', content_type, re.I)
    if m:
        return m.group(1)

    # 2. BOM detection
    if raw.startswith(b'\xef\xbb\xbf'):
        return 'utf-8-sig'
    if raw.startswith(b'\xff\xfe'):
        return 'utf-16-le'
    if raw.startswith(b'\xfe\xff'):
        return 'utf-16-be'

    # 3. meta charset in first 1024 bytes
    head = raw[:1024].decode('ascii', errors='replace')
    m = re.search(r'<meta[^>]+charset=["\']?([\w-]+)', head, re.I)
    if m:
        return m.group(1)

    # 4. Default
    return 'utf-8'


def resolve_url(base_url: str, url: str) -> str:
    """Resolve relative URL against base URL."""
    return urllib.parse.urljoin(base_url, url)


def fetch_bytes(url: str, base_url: str = '') -> bytes:
    """Fetch a resource and return raw bytes. Used for images etc."""
    if base_url:
        url = resolve_url(base_url, url)

    cached = _cache.get(url)
    if cached is not None:
        return cached

    req = urllib.request.Request(url, headers={
        'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36',
        'Accept': 'image/webp,image/apng,image/*,*/*;q=0.8',
        'Accept-Encoding': 'gzip, deflate',
    })
    with urllib.request.urlopen(req, timeout=10) as resp:
        raw = resp.read()
        encoding = resp.headers.get('Content-Encoding', '')
    if 'gzip' in encoding:
        import gzip
        try:
            raw = gzip.decompress(raw)
        except Exception:
            pass
    _cache.put(url, raw)
    return raw
