"""Font metrics implementation for the pyx_gui backend.

On platforms where lib/gui (Win32) is available, this backend can use
Win32 GDI text metrics.  On other platforms it falls back to a heuristic
estimate so layout still produces sensible output in headless / CI runs.
"""
from __future__ import annotations

import sys
from typing import Any
from backend.base import FontMetrics

# ---------------------------------------------------------------------------
# Character width heuristics (fallback for non-Windows)
# ---------------------------------------------------------------------------

# Approximate average character width as a fraction of font-size (em)
_AVG_CHAR_WIDTH_RATIO: float = 0.55
# Approximate line height as a fraction of font-size
_LINE_HEIGHT_RATIO: float = 1.2

# Generic CSS font families → reasonable system font names
_GENERIC_MAP: dict[str, str] = {
    "serif": "Times New Roman",
    "sans-serif": "Arial",
    "monospace": "Courier New",
    "cursive": "Comic Sans MS",
    "fantasy": "Impact",
    "system-ui": "Arial",
    "-apple-system": "Arial",
}

_IS_WINDOWS: bool = sys.platform == "win32"


class PyxFontMetrics(FontMetrics):
    """Font measurement using Win32 GDI on Windows, heuristics elsewhere."""

    def __init__(self) -> None:
        self._cache: dict[tuple, tuple[float, float]] = {}

    def measure(self, text: str, family: str, size_px: float,
                weight: str = "normal", italic: bool = False) -> tuple[float, float]:
        key: tuple = (text, family, size_px, weight, italic)
        cached = self._cache.get(key)
        if cached is not None:
            return cached

        if _IS_WINDOWS:
            result = self._measure_win32(text, family, size_px, weight, italic)
        else:
            result = self._measure_heuristic(text, size_px)

        self._cache[key] = result
        return result

    def _measure_heuristic(self, text: str, size_px: float) -> tuple[float, float]:
        """Approximate text dimensions without platform APIs."""
        width: float = len(text) * size_px * _AVG_CHAR_WIDTH_RATIO
        height: float = size_px * _LINE_HEIGHT_RATIO
        return width, height

    def _measure_win32(self, text: str, family: str, size_px: float,
                       weight: str, italic: bool) -> tuple[float, float]:
        """Measure text using Win32 GDI GetTextExtentPoint32W."""
        try:
            import ctypes
            import ctypes.wintypes as wt

            gdi32 = ctypes.windll.gdi32
            user32 = ctypes.windll.user32

            # Weight mapping
            fw_weight: int = 700 if weight in ("bold", "700", "800", "900") else 400

            # Create a logical font (negative height = character height in px)
            hfont = gdi32.CreateFontW(
                -int(size_px), 0, 0, 0,
                fw_weight, int(italic), 0, 0,
                1,  # DEFAULT_CHARSET
                0, 0, 0, 0,
                family,
            )
            if not hfont:
                return self._measure_heuristic(text, size_px)

            hdc = user32.GetDC(None)
            old_font = gdi32.SelectObject(hdc, hfont)

            class SIZE(ctypes.Structure):
                _fields_: list = [("cx", wt.LONG), ("cy", wt.LONG)]

            sz = SIZE()
            gdi32.GetTextExtentPoint32W(hdc, text, len(text), ctypes.byref(sz))

            gdi32.SelectObject(hdc, old_font)
            gdi32.DeleteObject(hfont)
            user32.ReleaseDC(None, hdc)

            return float(sz.cx), float(sz.cy)
        except Exception:
            return self._measure_heuristic(text, size_px)

    def resolve_family(self, family: str) -> str:
        """Resolve CSS font-family to a platform font name."""
        candidates: list[str] = [f.strip().strip("'\"") for f in family.split(",")]
        for candidate in candidates:
            mapped = _GENERIC_MAP.get(candidate.lower())
            if mapped:
                return mapped
            if candidate:
                return candidate
        return "Arial"

    def get_font(self, family: str, size_px: float,
                 weight: str = "normal", italic: bool = False) -> Any:
        """Return a (family, size_px, weight, italic) tuple as the font descriptor."""
        return (self.resolve_family(family), size_px, weight, italic)
