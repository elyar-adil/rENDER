"""Qt font metrics — FontMetrics implementation backed by PyQt6."""
from __future__ import annotations

import sys
from functools import lru_cache
from backend.base import FontMetrics

# Primary and fallback candidates for each CSS generic family keyword.
_GENERIC_CANDIDATES: dict[str, list[str]] = {
    'serif':         ['Times New Roman', 'DejaVu Serif', 'Liberation Serif', 'FreeSerif',
                      'Bitstream Charter', 'Georgia', 'Serif'],
    'sans-serif':    ['Arial', 'Helvetica', 'DejaVu Sans', 'Liberation Sans', 'FreeSans',
                      'Nimbus Sans', 'Sans Serif'],
    'monospace':     ['Courier New', 'Courier', 'DejaVu Sans Mono', 'Liberation Mono',
                      'FreeMono', 'Courier 10 Pitch', 'Monospace'],
    'cursive':       ['Comic Sans MS', 'URW Chancery L', 'Loma'],
    'fantasy':       ['Impact', 'Copperplate', 'DejaVu Sans'],
    'system-ui':     ['Arial', 'DejaVu Sans', 'Liberation Sans', 'Sans Serif'],
    'ui-sans-serif': ['Arial', 'DejaVu Sans', 'Liberation Sans', 'Sans Serif'],
    'ui-serif':      ['Times New Roman', 'DejaVu Serif', 'Serif'],
    'ui-monospace':  ['Courier New', 'DejaVu Sans Mono', 'Monospace'],
}

_app = None
_text_cache: dict = {}
_installed_fonts: set | None = None


def _fallback_measure(text: str, size_px: float, weight: str = 'normal') -> tuple[float, float]:
    size = max(1.0, float(size_px or 16.0))
    width = 0.0
    for ch in text:
        codepoint = ord(ch)
        if ch.isspace():
            width += size * 0.33
        elif codepoint >= 0x2E80:
            width += size * 1.0
        else:
            width += size * 0.56
    if str(weight).lower() in {'bold', '700', '800', '900'}:
        width *= 1.05
    return width, size * 1.2


def _ensure_app() -> None:
    global _app
    from PyQt6.QtWidgets import QApplication
    if _app is None and not QApplication.instance():
        _app = QApplication(sys.argv)


@lru_cache(maxsize=512)
def _get_qfont(family: str, size_px: float, weight: str = 'normal', italic: bool = False):
    """Create a QFont from CSS-style parameters (module-level LRU cache)."""
    from PyQt6.QtGui import QFont
    _ensure_app()
    font = QFont()
    families = [f.strip().strip('"\'') for f in family.split(',')]
    font.setFamily(_resolve_family(families[0]))
    font.setPixelSize(max(1, int(size_px)))
    weight_map = {
        'bold':   QFont.Weight.Bold,
        '700':    QFont.Weight.Bold,
        '600':    QFont.Weight.DemiBold,
        'normal': QFont.Weight.Normal,
        '400':    QFont.Weight.Normal,
        '300':    QFont.Weight.Light,
        '200':    QFont.Weight.ExtraLight,
        '100':    QFont.Weight.Thin,
        '800':    QFont.Weight.ExtraBold,
        '900':    QFont.Weight.Black,
    }
    resolved_weight = weight_map.get(str(weight).lower(), QFont.Weight.Normal)
    font.setWeight(resolved_weight)
    font.setBold(resolved_weight >= QFont.Weight.Bold)
    font.setItalic(italic)
    return font


def _measure(text: str, family: str, size_px: float,
             weight: str = 'normal', italic: bool = False) -> tuple[float, float]:
    """Measure text dimensions in pixels (module-level dict cache)."""
    cache_key = (family, size_px, weight, italic, text)
    if cache_key in _text_cache:
        return _text_cache[cache_key]
    try:
        from PyQt6.QtGui import QFontMetrics
        font = _get_qfont(family, size_px, weight, italic)
        fm = QFontMetrics(font)
        result = (float(fm.horizontalAdvance(text)), float(fm.height()))
    except Exception:
        result = _fallback_measure(text, size_px, weight)
    _text_cache[cache_key] = result
    return result


def _get_installed_fonts() -> set:
    global _installed_fonts
    if _installed_fonts is None:
        _ensure_app()
        from PyQt6.QtGui import QFontDatabase
        _installed_fonts = set(QFontDatabase.families())
    return _installed_fonts


@lru_cache(maxsize=256)
def _resolve_family(family: str) -> str:
    """Resolve a CSS font-family value to an installed font name."""
    families = [f.strip().strip('"\'') for f in family.split(',')]
    installed = _get_installed_fonts()
    for f in families:
        fl = f.lower()
        candidates = _GENERIC_CANDIDATES.get(fl)
        if candidates is not None:
            for c in candidates:
                if c in installed:
                    return c
            continue
        if f and f in installed:
            return f
    # Last resort: return preferred generic candidate even if not installed
    for f in families:
        candidates = _GENERIC_CANDIDATES.get(f.lower())
        if candidates:
            return candidates[0]
    return families[0] if families else 'Arial'


class QtFontMetrics(FontMetrics):
    """FontMetrics implementation backed by PyQt6 QFontMetrics."""

    def measure(self, text: str, family: str, size_px: float,
                weight: str = 'normal', italic: bool = False) -> tuple[float, float]:
        return _measure(text, family, size_px, weight, italic)

    def resolve_family(self, family: str) -> str:
        return _resolve_family(family)

    def get_font(self, family: str, size_px: float,
                 weight: str = 'normal', italic: bool = False):
        return _get_qfont(family, size_px, weight, italic)
