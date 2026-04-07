"""Win32 GDI painter for rENDER display lists.

On Windows: creates a native window using lib/gui, then renders the display
list via Win32 GDI calls inside WM_PAINT.

On Linux/macOS: imports cleanly (no-op stubs) so tests pass on CI.

Architecture
------------
BrowserWindow  ← lib/gui Window + custom WM_PAINT handler
  └─ renders DisplayList commands via GDI (DrawRect, DrawText, DrawImage, …)
"""
from __future__ import annotations

import sys
import logging
from typing import Any

_logger: logging.Logger = logging.getLogger(__name__)
_IS_WINDOWS: bool = sys.platform == "win32"

# ---------------------------------------------------------------------------
# Lazy Win32 imports (only on Windows)
# ---------------------------------------------------------------------------

if _IS_WINDOWS:
    import ctypes
    import ctypes.wintypes as wt

    _gdi32: Any = ctypes.windll.gdi32
    _user32: Any = ctypes.windll.user32

    # GDI pen/brush styles
    _PS_SOLID: int = 0
    _BS_SOLID: int = 0
    _NULL_BRUSH: int = 5
    _NULL_PEN: int = 8

    # WM_PAINT message
    _WM_PAINT: int = 0x000F
    _WM_SIZE: int = 0x0005
    _WM_VSCROLL: int = 0x0115
    _WM_MOUSEWHEEL: int = 0x020A

    class _PAINTSTRUCT(ctypes.Structure):
        _fields_: list = [
            ("hdc", wt.HANDLE),
            ("fErase", wt.BOOL),
            ("rcPaint", wt.RECT),
            ("fRestore", wt.BOOL),
            ("fIncUpdate", wt.BOOL),
            ("rgbReserved", ctypes.c_byte * 32),
        ]


# ---------------------------------------------------------------------------
# Colour helper
# ---------------------------------------------------------------------------

def _parse_colour(css: str) -> int:
    """Convert a CSS colour string to a Win32 COLORREF (0x00BBGGRR)."""
    css = (css or "").strip()
    if css.startswith("#"):
        h = css[1:]
        if len(h) == 3:
            h = h[0]*2 + h[1]*2 + h[2]*2
        if len(h) == 6:
            r = int(h[0:2], 16)
            g = int(h[2:4], 16)
            b = int(h[4:6], 16)
            return (b << 16) | (g << 8) | r
    if css.startswith("rgb"):
        import re
        nums = re.findall(r"\d+", css)
        if len(nums) >= 3:
            r, g, b = int(nums[0]), int(nums[1]), int(nums[2])
            return (b << 16) | (g << 8) | r
    # Named colours (minimal set)
    _named: dict[str, int] = {
        "white": 0xFFFFFF, "black": 0x000000,
        "red": 0x0000FF, "green": 0x008000, "blue": 0xFF0000,
        "gray": 0x808080, "grey": 0x808080,
        "transparent": -1,
    }
    return _named.get(css.lower(), 0x000000)


# ---------------------------------------------------------------------------
# BrowserWindow
# ---------------------------------------------------------------------------

class BrowserWindow:
    """Native browser window rendering a DisplayList via Win32 GDI."""

    def __init__(self, display_list: list, page_height: int,
                 width: int = 980, height: int = 600,
                 title: str = "rENDER") -> None:
        self._display_list: list = display_list
        self._page_height: int = page_height
        self._scroll_y: int = 0
        self._width: int = width
        self._height: int = height
        self._title: str = title

    def run(self) -> None:
        """Open the window and enter the GUI event loop."""
        if not _IS_WINDOWS:
            _logger.warning("BrowserWindow.run(): Win32 GUI not available on %s", sys.platform)
            return

        # Import the lib/gui Application and Window
        try:
            import sys as _sys
            import os as _os
            _lib_dir = _os.path.join(_os.path.dirname(_os.path.abspath(__file__)),
                                     "..", "..", "lib")
            if _lib_dir not in _sys.path:
                _sys.path.insert(0, _lib_dir)
            from gui import Application, Window
        except ImportError as exc:
            _logger.error("lib/gui import failed: %s", exc)
            return

        app: Any = Application()
        win: Any = app.create_window(self._title,
                                     width=self._width,
                                     height=self._height)

        # Hook WM_PAINT to our renderer
        win.on_paint(self._on_paint)
        win.on_scroll(self._on_scroll)
        win.show()
        raise SystemExit(app.run())

    def _on_paint(self, hdc: int, rect: Any) -> None:
        """Called by the Win32 message loop for WM_PAINT."""
        self._render(hdc)

    def _on_scroll(self, delta: int) -> None:
        """Handle mouse-wheel or scrollbar scroll events."""
        self._scroll_y = max(0, self._scroll_y + delta)
        if _IS_WINDOWS:
            # Invalidate window to trigger repaint
            pass  # the lib/gui Window handles InvalidateRect

    def _render(self, hdc: int) -> None:
        """Execute the display list into a Win32 DC."""
        from rendering.display_list import (
            DrawRect, DrawText, DrawBorder, DrawImage,
        )
        offset_y: int = self._scroll_y

        for cmd in self._display_list:
            if isinstance(cmd, DrawRect):
                self._gdi_fill_rect(hdc, cmd.x, cmd.y - offset_y,
                                    cmd.width, cmd.height, cmd.color)
            elif isinstance(cmd, DrawText):
                self._gdi_draw_text(hdc, cmd.x, cmd.y - offset_y,
                                    cmd.text, cmd.color,
                                    cmd.font_family, cmd.font_size_px,
                                    cmd.font_weight, cmd.font_italic)
            elif isinstance(cmd, DrawBorder):
                self._gdi_draw_border(hdc, cmd.x, cmd.y - offset_y,
                                      cmd.width, cmd.height, cmd.color,
                                      cmd.widths)
            # DrawImage: out of scope for GDI-only renderer (skip)

    # ------------------------------------------------------------------
    # GDI drawing primitives
    # ------------------------------------------------------------------

    def _gdi_fill_rect(self, hdc: int, x: float, y: float,
                       w: float, h: float, color: str) -> None:
        if not _IS_WINDOWS:
            return
        colour: int = _parse_colour(color)
        if colour < 0:
            return  # transparent
        hbrush: int = _gdi32.CreateSolidBrush(colour)
        rc = wt.RECT(int(x), int(y), int(x + w), int(y + h))
        _user32.FillRect(hdc, ctypes.byref(rc), hbrush)
        _gdi32.DeleteObject(hbrush)

    def _gdi_draw_text(self, hdc: int, x: float, y: float, text: str,
                       color: str, family: str, size_px: float,
                       weight: str, italic: bool) -> None:
        if not _IS_WINDOWS or not text:
            return
        colour: int = _parse_colour(color)
        _gdi32.SetTextColor(hdc, colour)
        _gdi32.SetBkMode(hdc, 1)  # TRANSPARENT

        fw: int = 700 if weight in ("bold", "700", "800", "900") else 400
        hfont: int = _gdi32.CreateFontW(
            -int(size_px), 0, 0, 0,
            fw, int(italic), 0, 0,
            1, 0, 0, 0, 0, family,
        )
        old: int = _gdi32.SelectObject(hdc, hfont)
        rc = wt.RECT(int(x), int(y), int(x + 2000), int(y + int(size_px) + 4))
        _user32.DrawTextW(hdc, text, len(text), ctypes.byref(rc), 0)
        _gdi32.SelectObject(hdc, old)
        _gdi32.DeleteObject(hfont)

    def _gdi_draw_border(self, hdc: int, x: float, y: float,
                         w: float, h: float, color: str,
                         widths: Any) -> None:
        if not _IS_WINDOWS:
            return
        colour: int = _parse_colour(color)
        hpen: int = _gdi32.CreatePen(_PS_SOLID, 1, colour)
        hbrush: int = _gdi32.GetStockObject(_NULL_BRUSH)
        old_pen: int = _gdi32.SelectObject(hdc, hpen)
        old_brush: int = _gdi32.SelectObject(hdc, hbrush)
        _gdi32.Rectangle(hdc, int(x), int(y), int(x + w), int(y + h))
        _gdi32.SelectObject(hdc, old_pen)
        _gdi32.SelectObject(hdc, old_brush)
        _gdi32.DeleteObject(hpen)
