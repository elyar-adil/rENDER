"""PyX GUI backend for rENDER.

Uses the lib/gui framework (ctypes Win32 on Windows, no-op stubs elsewhere)
together with a font-metrics fallback for layout measurement.

Registration::

    from backend.pyx_gui import register
    register()
"""
from __future__ import annotations

from backend.pyx_gui.font import PyxFontMetrics
from backend.pyx_gui.image import PyxImageLoader
import backend as _backend


def register() -> None:
    """Install the pyx_gui font/image backends."""
    _backend.set_font_metrics(PyxFontMetrics())
    _backend.set_image_loader(PyxImageLoader())
