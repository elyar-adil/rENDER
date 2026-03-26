"""Text measurement using QFontMetrics (PyQt6)."""
import sys

# Need a QApplication to use Qt font metrics
_app = None

def _ensure_app():
    global _app
    from PyQt6.QtWidgets import QApplication
    if _app is None and not QApplication.instance():
        _app = QApplication.instance() or QApplication(sys.argv)


# Cache: (family, size_px, weight, italic) -> QFontMetrics
_metrics_cache = {}
# Cache: (family, size_px, weight, italic, text) -> (width, height)
_text_cache = {}


def get_font(family: str, size_px: float, weight: str = 'normal', italic: bool = False):
    """Create a QFont from CSS-style parameters."""
    from PyQt6.QtGui import QFont
    _ensure_app()
    font = QFont()

    # Handle font family (may be comma-separated)
    families = [f.strip().strip('"\'') for f in family.split(',')]
    font.setFamily(resolve_font_family(families[0]))

    # Size
    font.setPixelSize(max(1, int(size_px)))

    # Weight
    from PyQt6.QtGui import QFont
    weight_map = {
        'bold': QFont.Weight.Bold,
        '700': QFont.Weight.Bold,
        '600': QFont.Weight.DemiBold,
        'normal': QFont.Weight.Normal,
        '400': QFont.Weight.Normal,
        '300': QFont.Weight.Light,
        '200': QFont.Weight.ExtraLight,
        '100': QFont.Weight.Thin,
        '800': QFont.Weight.ExtraBold,
        '900': QFont.Weight.Black,
    }
    resolved_weight = weight_map.get(str(weight).lower(), QFont.Weight.Normal)
    font.setWeight(resolved_weight)
    font.setBold(resolved_weight >= QFont.Weight.Bold)
    font.setItalic(italic)

    return font


def measure_text(text: str, family: str = 'Times', size_px: float = 16,
                 weight: str = 'normal', italic: bool = False) -> tuple[float, float]:
    """Measure text dimensions. Returns (width, height) in pixels."""
    from PyQt6.QtGui import QFontMetrics
    cache_key = (family, size_px, weight, italic, text)
    if cache_key in _text_cache:
        return _text_cache[cache_key]

    font = get_font(family, size_px, weight, italic)
    fm = QFontMetrics(font)
    width = fm.horizontalAdvance(text)
    height = fm.height()

    result = (float(width), float(height))
    _text_cache[cache_key] = result
    return result


def measure_word(word: str, style: dict) -> tuple[float, float]:
    """Measure a single word given a CSS style dict."""
    family = style.get('font-family', 'Times')
    size_str = style.get('font-size', '16px')
    size_px = _parse_px(size_str)
    weight = style.get('font-weight', 'normal')
    italic = style.get('font-style', 'normal') in ('italic', 'oblique')
    word_spacing_str = style.get('word-spacing', '0px')
    word_spacing = _parse_px(word_spacing_str)

    w, h = measure_text(word, family, size_px, weight, italic)
    return w + word_spacing, h


# Primary and fallback candidates for each CSS generic family keyword.
# Ordered from most-preferred to least-preferred.
_GENERIC_CANDIDATES: dict[str, list[str]] = {
    'serif':       ['Times New Roman', 'DejaVu Serif', 'Liberation Serif', 'FreeSerif',
                    'Bitstream Charter', 'Georgia', 'Serif'],
    'sans-serif':  ['Arial', 'Helvetica', 'DejaVu Sans', 'Liberation Sans', 'FreeSans',
                    'Nimbus Sans', 'Sans Serif'],
    'monospace':   ['Courier New', 'Courier', 'DejaVu Sans Mono', 'Liberation Mono',
                    'FreeMono', 'Courier 10 Pitch', 'Monospace'],
    'cursive':     ['Comic Sans MS', 'URW Chancery L', 'Loma'],
    'fantasy':     ['Impact', 'Copperplate', 'DejaVu Sans'],
    'system-ui':   ['Arial', 'DejaVu Sans', 'Liberation Sans', 'Sans Serif'],
    'ui-sans-serif': ['Arial', 'DejaVu Sans', 'Liberation Sans', 'Sans Serif'],
    'ui-serif':    ['Times New Roman', 'DejaVu Serif', 'Serif'],
    'ui-monospace': ['Courier New', 'DejaVu Sans Mono', 'Monospace'],
}

_installed_fonts: set | None = None


def _get_installed_fonts() -> set:
    global _installed_fonts
    if _installed_fonts is None:
        _ensure_app()
        from PyQt6.QtGui import QFontDatabase
        _installed_fonts = set(QFontDatabase.families())
    return _installed_fonts


def resolve_font_family(family: str) -> str:
    """Resolve a CSS font-family list to a single installed family name.

    Checks each candidate against the system's installed fonts and returns
    the first match. Falls back through generic keyword alternatives.
    """
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
    # Last resort: find any installed font from the generic candidate lists.
    for f in families:
        fl = f.lower()
        candidates = _GENERIC_CANDIDATES.get(fl)
        if candidates:
            return candidates[0]  # return preferred even if not installed
    return families[0] if families else 'Arial'


def _parse_px(value: str) -> float:
    """Extract numeric px value from a CSS length string."""
    if not value:
        return 0.0
    value = value.strip()
    if value.endswith('px'):
        try:
            return float(value[:-2])
        except ValueError:
            return 0.0
    try:
        return float(value)
    except ValueError:
        return 0.0
