"""Application lifecycle and Win32 message loop for the PyX GUI framework.

The :class:`Application` singleton initialises common controls, owns the
event loop, and provides convenience factory methods for the most common
top-level windows.
"""

import sys
from typing import Callable

from . import win32_api as w32
from .events import EventDispatcher
from .window import Window

# ---------------------------------------------------------------------------
# Common Controls initialisation
# ---------------------------------------------------------------------------

_COMCTL32_INITED: bool = False


def _init_common_controls() -> None:
    global _COMCTL32_INITED
    if _COMCTL32_INITED or not w32.IS_WINDOWS:
        return
    import ctypes
    try:
        comctl32 = ctypes.windll.comctl32
        # ICC_STANDARD_CLASSES | ICC_PROGRESS_CLASS | ICC_LISTVIEW_CLASSES
        ICC_FLAGS = 0x0001 | 0x0020 | 0x0004

        class INITCOMMONCONTROLSEX(ctypes.Structure):
            _fields_ = [("dwSize", ctypes.c_ulong), ("dwICC", ctypes.c_ulong)]

        icc = INITCOMMONCONTROLSEX()
        icc.dwSize = ctypes.sizeof(INITCOMMONCONTROLSEX)
        icc.dwICC = ICC_FLAGS
        comctl32.InitCommonControlsEx(ctypes.byref(icc))
    except Exception:
        pass
    _COMCTL32_INITED = True


# ---------------------------------------------------------------------------
# Application
# ---------------------------------------------------------------------------


class Application:
    """Manages the Win32 message loop and top-level window lifecycle.

    Usage::

        app = Application()
        win = app.create_window("My App", width=800, height=600)
        # … add widgets to win …
        win.show()
        sys.exit(app.run())

    A single :class:`Application` instance should be created per process.
    Creating multiple instances on the same thread is harmless (they share
    the same Win32 thread message queue) but unnecessary.
    """

    def __init__(self) -> None:
        _init_common_controls()
        self._windows: list[Window] = []
        self._running: bool = False
        self._exit_code: int = 0

    # ------------------------------------------------------------------
    # Factory helpers
    # ------------------------------------------------------------------

    def create_window(
        self,
        title: str = "PyX Application",
        width: int = 800,
        height: int = 600,
        resizable: bool = True,
        dispatcher: EventDispatcher | None = None,
    ) -> Window:
        """Create and register a top-level :class:`Window`."""
        win = Window(
            title=title,
            width=width,
            height=height,
            resizable=resizable,
            dispatcher=dispatcher,
        )
        self._windows.append(win)
        return win

    # ------------------------------------------------------------------
    # Event loop
    # ------------------------------------------------------------------

    def run(self) -> int:
        """Enter the Win32 message loop and return the process exit code.

        Blocks until all top-level windows have been destroyed or
        :meth:`quit` is called.
        """
        if not w32.IS_WINDOWS:
            # Non-Windows stub: just return immediately.
            return 0

        self._running = True
        msg = w32.MSG()
        while True:
            result = w32.get_message(msg)
            if result == 0:
                # WM_QUIT received
                self._exit_code = msg.wParam
                break
            if result == -1:
                # Error in GetMessage
                break
            w32.translate_message(msg)
            w32.dispatch_message(msg)

        self._running = False
        return self._exit_code

    def run_with_idle(self, idle: Callable[[], None]) -> int:
        """Like :meth:`run` but calls *idle* when the message queue is empty.

        Useful for animations or periodic updates.  *idle* should be fast
        (< 16 ms) to avoid blocking repaints.
        """
        if not w32.IS_WINDOWS:
            return 0

        self._running = True
        msg = w32.MSG()
        while self._running:
            if w32.peek_message(msg, remove=True):
                if msg.message == w32.WM_QUIT:
                    self._exit_code = msg.wParam
                    break
                w32.translate_message(msg)
                w32.dispatch_message(msg)
            else:
                idle()

        self._running = False
        return self._exit_code

    def quit(self, exit_code: int = 0) -> None:
        """Request the message loop to terminate."""
        self._running = False
        w32.post_quit_message(exit_code)

    # ------------------------------------------------------------------
    # Helpers
    # ------------------------------------------------------------------

    @staticmethod
    def message_box(
        text: str,
        title: str = "PyX",
        flags: int = w32.MB_OK | w32.MB_ICONINFORMATION,
        parent: Window | None = None,
    ) -> int:
        """Show a modal Win32 message box.  Returns IDOK / IDYES / IDNO etc."""
        hwnd = parent.hwnd if parent else 0
        return w32.message_box(hwnd, text, title, flags)
