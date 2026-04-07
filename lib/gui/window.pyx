"""Top-level window implementation for the PyX GUI framework.

A :class:`Window` wraps a Win32 overlapped window, registers a
``WNDCLASSEXW``, and routes raw Win32 messages to the application-level
:class:`~pyx.gui.events.EventDispatcher`.
"""

import ctypes
import ctypes.wintypes as wt
import sys
from typing import Callable

from . import win32_api as w32
from .events import (
    ClickEvent,
    CloseEvent,
    DestroyEvent,
    Event,
    EventDispatcher,
    KeyEvent,
    MouseEvent,
    ResizeEvent,
    TextChangedEvent,
    TimerEvent,
)

# ---------------------------------------------------------------------------
# Module-level class registry
# ---------------------------------------------------------------------------

# Maps class_name → registered atom so we only call RegisterClassExW once.
_registered_classes: dict[str, int] = {}

# Maps hwnd → Window instance for WndProc dispatch.
_hwnd_map: dict[int, "Window"] = {}

# ---------------------------------------------------------------------------
# WndProc
# ---------------------------------------------------------------------------

if w32.IS_WINDOWS:
    @w32.WNDPROC  # type: ignore[misc]
    def _wnd_proc(hwnd: int, msg: int, wparam: int, lparam: int) -> int:
        win = _hwnd_map.get(hwnd)
        if win is not None:
            result = win._handle_message(msg, wparam, lparam)
            if result is not None:
                return result
        return w32.def_window_proc(hwnd, msg, wparam, lparam)
else:
    _wnd_proc = None  # type: ignore[assignment]


# ---------------------------------------------------------------------------
# Window class
# ---------------------------------------------------------------------------

_NEXT_CTRL_ID: int = 1001


def _next_ctrl_id() -> int:
    global _NEXT_CTRL_ID
    _id = _NEXT_CTRL_ID
    _NEXT_CTRL_ID += 1
    return _id


class Window:
    """A native Win32 overlapped (top-level) window.

    Parameters
    ----------
    title:
        Window title bar text.
    width / height:
        Initial client-area dimensions in device pixels.
    resizable:
        When *False*, the window has no resize frame or maximise button.
    dispatcher:
        Shared :class:`EventDispatcher`; a new one is created if omitted.
    """

    #: Shared window class name for all PyX windows.
    CLASS_NAME: str = "PyxWindow"

    def __init__(
        self,
        title: str = "PyX Application",
        width: int = 800,
        height: int = 600,
        resizable: bool = True,
        dispatcher: EventDispatcher | None = None,
    ) -> None:
        self.title: str = title
        self.width: int = width
        self.height: int = height
        self.dispatcher: EventDispatcher = dispatcher or EventDispatcher()
        self._hwnd: int = 0
        self._children: list[object] = []
        self._hfont: int = 0
        self._resizable: bool = resizable
        self._on_close: Callable[[], bool] | None = None  # return False to cancel

        if w32.IS_WINDOWS:
            self._create(resizable)

    # ------------------------------------------------------------------
    # Creation
    # ------------------------------------------------------------------

    def _register_class(self) -> None:
        if self.CLASS_NAME in _registered_classes:
            return
        hinstance = w32.get_module_handle()
        wc = w32.WNDCLASSEXW()
        wc.cbSize = ctypes.sizeof(w32.WNDCLASSEXW)
        wc.style = w32.CS_HREDRAW | w32.CS_VREDRAW | w32.CS_DBLCLKS
        wc.lpfnWndProc = ctypes.cast(_wnd_proc, ctypes.c_void_p).value
        wc.cbClsExtra = 0
        wc.cbWndExtra = 0
        wc.hInstance = hinstance
        wc.hIcon = w32.load_icon(w32.IDI_APPLICATION)
        wc.hCursor = w32.load_cursor(w32.IDC_ARROW)
        wc.hbrBackground = w32.get_sys_color_brush(w32.COLOR_BTNFACE)
        wc.lpszMenuName = None
        wc.lpszClassName = self.CLASS_NAME
        wc.hIconSm = wc.hIcon
        atom = w32.register_class(wc)
        _registered_classes[self.CLASS_NAME] = atom

    def _create(self, resizable: bool) -> None:
        self._register_class()
        hinstance = w32.get_module_handle()
        style = w32.WS_OVERLAPPEDWINDOW
        if not resizable:
            style &= ~(w32.WS_THICKFRAME | w32.WS_MAXIMIZEBOX)
        hwnd = w32.create_window(
            class_name=self.CLASS_NAME,
            title=self.title,
            style=style,
            x=w32.CW_USEDEFAULT,
            y=w32.CW_USEDEFAULT,
            width=self.width,
            height=self.height,
            parent=w32.HWND_DESKTOP,
            menu=0,
            hinstance=hinstance,
            ex_style=w32.WS_EX_APPWINDOW,
        )
        if not hwnd:
            raise RuntimeError("CreateWindowExW failed")
        self._hwnd = hwnd
        _hwnd_map[hwnd] = self
        # Default UI font
        self._hfont = w32.create_font(height=-13, face="Segoe UI")

    # ------------------------------------------------------------------
    # Public interface
    # ------------------------------------------------------------------

    @property
    def hwnd(self) -> int:
        return self._hwnd

    def show(self, cmd: int = w32.SW_SHOWNORMAL) -> None:
        if self._hwnd:
            w32.show_window(self._hwnd, cmd)
            w32.update_window(self._hwnd)

    def hide(self) -> None:
        if self._hwnd:
            w32.show_window(self._hwnd, w32.SW_HIDE)

    def set_title(self, title: str) -> None:
        self.title = title
        if self._hwnd:
            w32.set_window_text(self._hwnd, title)

    def resize(self, width: int, height: int) -> None:
        self.width = width
        self.height = height
        if self._hwnd:
            w32.move_window(self._hwnd, w32.CW_USEDEFAULT, w32.CW_USEDEFAULT,
                            width, height)

    def close(self) -> None:
        if self._hwnd:
            w32.destroy_window(self._hwnd)

    def on_close(self, handler: Callable[[], bool]) -> None:
        """Register a close-request handler.

        The handler should return *True* to allow closing, *False* to
        cancel.
        """
        self._on_close = handler

    def on(self, event_type: str, handler: Callable[[Event], None]) -> None:
        """Register an event handler for this window."""
        self.dispatcher.on(event_type, handler, self._hwnd)

    def set_font(self, hfont: int) -> None:
        """Apply *hfont* to the window and all child controls."""
        self._hfont = hfont
        if self._hwnd:
            w32.send_message(self._hwnd, w32.WM_SETFONT, hfont, 1)
        for child in self._children:
            if hasattr(child, "hwnd") and child.hwnd:  # type: ignore[union-attr]
                w32.send_message(child.hwnd, w32.WM_SETFONT, hfont, 1)  # type: ignore[union-attr]

    # ------------------------------------------------------------------
    # Message handler
    # ------------------------------------------------------------------

    def _handle_message(
        self, msg: int, wparam: int, lparam: int
    ) -> int | None:
        """Translate a raw Win32 message into an Event and dispatch it.

        Returns an integer result if the message was fully handled, or
        *None* to fall through to DefWindowProcW.
        """
        if msg == w32.WM_DESTROY:
            self.dispatcher.dispatch(DestroyEvent(hwnd=self._hwnd))
            _hwnd_map.pop(self._hwnd, None)
            if self._hfont:
                w32.delete_object(self._hfont)
                self._hfont = 0
            w32.post_quit_message(0)
            return 0

        if msg == w32.WM_CLOSE:
            allow = True
            if self._on_close is not None:
                allow = self._on_close()
            if allow:
                self.dispatcher.dispatch(CloseEvent(hwnd=self._hwnd))
                w32.destroy_window(self._hwnd)
            return 0

        if msg == w32.WM_SIZE:
            lo = lparam & 0xFFFF
            hi = (lparam >> 16) & 0xFFFF
            self.dispatcher.dispatch(ResizeEvent(hwnd=self._hwnd, width=lo, height=hi))
            return None  # let DefWindowProc handle sizing

        if msg == w32.WM_COMMAND:
            ctrl_id = wparam & 0xFFFF
            notif = (wparam >> 16) & 0xFFFF
            ctrl_hwnd = lparam
            self._handle_command(ctrl_id, notif, ctrl_hwnd)
            return 0

        if msg == w32.WM_KEYDOWN:
            self.dispatcher.dispatch(KeyEvent(hwnd=self._hwnd, vkey=wparam, is_down=True))
            return None

        if msg == w32.WM_KEYUP:
            self.dispatcher.dispatch(KeyEvent(hwnd=self._hwnd, vkey=wparam, is_down=False))
            return None

        if msg == w32.WM_LBUTTONDOWN:
            x = lparam & 0xFFFF
            y = (lparam >> 16) & 0xFFFF
            self.dispatcher.dispatch(MouseEvent(hwnd=self._hwnd, x=x, y=y,
                                                button="left", is_down=True))
            return None

        if msg == w32.WM_LBUTTONUP:
            x = lparam & 0xFFFF
            y = (lparam >> 16) & 0xFFFF
            self.dispatcher.dispatch(MouseEvent(hwnd=self._hwnd, x=x, y=y,
                                                button="left", is_down=False))
            return None

        if msg == w32.WM_RBUTTONDOWN:
            x = lparam & 0xFFFF
            y = (lparam >> 16) & 0xFFFF
            self.dispatcher.dispatch(MouseEvent(hwnd=self._hwnd, x=x, y=y,
                                                button="right", is_down=True))
            return None

        if msg == w32.WM_TIMER:
            self.dispatcher.dispatch(TimerEvent(hwnd=self._hwnd, timer_id=wparam))
            return 0

        return None

    def _handle_command(self, ctrl_id: int, notif: int, ctrl_hwnd: int) -> None:
        if notif == w32.BN_CLICKED:
            self.dispatcher.dispatch(ClickEvent(
                hwnd=self._hwnd,
                control_id=ctrl_id,
                control_hwnd=ctrl_hwnd,
            ))
        elif notif == w32.EN_CHANGE:
            text = w32.get_window_text(ctrl_hwnd)
            self.dispatcher.dispatch(TextChangedEvent(
                hwnd=self._hwnd,
                control_id=ctrl_id,
                control_hwnd=ctrl_hwnd,
                text=text,
            ))

    # ------------------------------------------------------------------
    # Child tracking
    # ------------------------------------------------------------------

    def _add_child(self, child: object) -> None:
        self._children.append(child)
        # Apply the window font to new children
        if self._hfont and hasattr(child, "hwnd") and child.hwnd:  # type: ignore[union-attr]
            w32.send_message(child.hwnd, w32.WM_SETFONT, self._hfont, 1)  # type: ignore[union-attr]
