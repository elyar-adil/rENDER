"""Text measurement — delegates to the active platform backend.

The pure-Python helper ``_parse_px`` lives here because it is used heavily
throughout the layout engine and has no platform dependency.

All font/metrics operations are forwarded to ``backend.get_font_metrics()``,
which defaults to the Qt implementation but can be swapped for testing or
alternative GUI toolkits.
"""

from functools import lru_cache


# ---------------------------------------------------------------------------
# Pure Python — no platform dependency
# ---------------------------------------------------------------------------

@lru_cache(maxsize=1024)
def _parse_px(value: str) -> float:
    """Extract a numeric pixel value from a CSS length string."""
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


# ---------------------------------------------------------------------------
# Platform-delegating API
# ---------------------------------------------------------------------------

def measure_text(text: str, family: str = 'Times', size_px: float = 16,
                 weight: str = 'normal', italic: bool = False) -> tuple[float, float]:
    """Return *(width, height)* in pixels for *text* rendered in the given font."""
    import backend
    return backend.get_font_metrics().measure(text, family, size_px, weight, italic)


def measure_word(word: str, style: dict) -> tuple[float, float]:
    """Measure a single word given a CSS computed-style dict."""
    family = style.get('font-family', 'Times')
    size_px = _parse_px(style.get('font-size', '16px'))
    weight = style.get('font-weight', 'normal')
    italic = style.get('font-style', 'normal') in ('italic', 'oblique')
    word_spacing = _parse_px(style.get('word-spacing', '0px'))
    w, h = measure_text(word, family, size_px, weight, italic)
    return w + word_spacing, h


def get_font(family: str, size_px: float, weight: str = 'normal', italic: bool = False):
    """Return a platform font object for the given parameters."""
    import backend
    return backend.get_font_metrics().get_font(family, size_px, weight, italic)


def resolve_font_family(family: str) -> str:
    """Resolve a CSS font-family value to an installed font name."""
    import backend
    return backend.get_font_metrics().resolve_family(family)
