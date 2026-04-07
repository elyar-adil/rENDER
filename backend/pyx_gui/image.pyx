"""Image loader implementation for the pyx_gui backend.

On Windows, uses ctypes to decode images via WIC (Windows Imaging Component).
On other platforms, attempts stdlib-only decoding (struct + zlib for PNG),
falling back gracefully to no-op so layout continues without images.
"""

import sys
import struct
import zlib
import logging
from backend.base import ImageLoader

_logger: logging.Logger = logging.getLogger(__name__)
_IS_WINDOWS: bool = sys.platform == "win32"


class PyxImageLoader(ImageLoader):
    """Decode image bytes and attach dimensions to DOM nodes."""

    def attach_images(self, img_data: list) -> None:
        """Process (node, raw_bytes) pairs, attaching image metadata to nodes."""
        for node, raw in img_data:
            try:
                w, h = _get_dimensions(raw)
                node.natural_width = w
                node.natural_height = h
                node.image = raw  # store raw bytes; painter can decode on demand
            except Exception as exc:
                _logger.debug("Image decode failed: %s", exc)


# ---------------------------------------------------------------------------
# Dimension extraction helpers (platform-independent)
# ---------------------------------------------------------------------------

def _get_dimensions(raw: bytes) -> tuple[int, int]:
    """Return (width, height) by sniffing image format from magic bytes."""
    if raw[:8] == b"\x89PNG\r\n\x1a\n":
        return _png_dimensions(raw)
    if raw[:2] in (b"\xff\xd8", b"\xff\xe0", b"\xff\xe1"):
        return _jpeg_dimensions(raw)
    if raw[:6] in (b"GIF87a", b"GIF89a"):
        w, h = struct.unpack_from("<HH", raw, 6)
        return w, h
    if raw[:2] in (b"BM",):
        w, h = struct.unpack_from("<ii", raw, 18)
        return w, abs(h)
    if raw[:4] == b"RIFF" and raw[8:12] == b"WEBP":
        return _webp_dimensions(raw)
    return 0, 0


def _png_dimensions(raw: bytes) -> tuple[int, int]:
    w, h = struct.unpack_from(">II", raw, 16)
    return w, h


def _jpeg_dimensions(raw: bytes) -> tuple[int, int]:
    i: int = 2
    n: int = len(raw)
    while i < n:
        if raw[i] != 0xFF:
            break
        marker: int = raw[i + 1]
        if marker in (0xC0, 0xC1, 0xC2):
            h, w = struct.unpack_from(">HH", raw, i + 5)
            return w, h
        seg_len: int = struct.unpack_from(">H", raw, i + 2)[0]
        i += 2 + seg_len
    return 0, 0


def _webp_dimensions(raw: bytes) -> tuple[int, int]:
    # VP8 lossy
    if raw[12:16] == b"VP8 ":
        w = (struct.unpack_from("<H", raw, 26)[0]) & 0x3FFF
        h = (struct.unpack_from("<H", raw, 28)[0]) & 0x3FFF
        return w, h
    # VP8L lossless
    if raw[12:16] == b"VP8L":
        b0, b1, b2, b3 = raw[21], raw[22], raw[23], raw[24]
        bits: int = b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
        w = (bits & 0x3FFF) + 1
        h = ((bits >> 14) & 0x3FFF) + 1
        return w, h
    return 0, 0
