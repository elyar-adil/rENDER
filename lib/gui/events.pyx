"""Event types and dispatcher for the PyX GUI framework.

Events are simple dataclasses passed to registered handler callables.
The dispatcher maps (hwnd, message) pairs to Python-level handlers so
that the raw WndProc stays decoupled from application logic.
"""
from __future__ import annotations

import dataclasses
from collections import defaultdict
from typing import Callable

# ---------------------------------------------------------------------------
# Event dataclasses
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class Event:
    """Base class for all GUI events."""
    hwnd: int


@dataclasses.dataclass
class CloseEvent(Event):
    """Fired when the user clicks the window close button (WM_CLOSE)."""


@dataclasses.dataclass
class DestroyEvent(Event):
    """Fired when a window is destroyed (WM_DESTROY)."""


@dataclasses.dataclass
class ClickEvent(Event):
    """Fired when a button is clicked (WM_COMMAND / BN_CLICKED)."""
    control_id: int
    control_hwnd: int


@dataclasses.dataclass
class TextChangedEvent(Event):
    """Fired when an edit control's text changes (WM_COMMAND / EN_CHANGE)."""
    control_id: int
    control_hwnd: int
    text: str


@dataclasses.dataclass
class SelectionChangedEvent(Event):
    """Fired when a listbox or combobox selection changes."""
    control_id: int
    control_hwnd: int
    index: int


@dataclasses.dataclass
class KeyEvent(Event):
    """Fired on WM_KEYDOWN / WM_KEYUP."""
    vkey: int
    is_down: bool


@dataclasses.dataclass
class MouseEvent(Event):
    """Fired on mouse button messages."""
    x: int
    y: int
    button: str   # "left" | "right" | "middle"
    is_down: bool


@dataclasses.dataclass
class ResizeEvent(Event):
    """Fired on WM_SIZE."""
    width: int
    height: int


@dataclasses.dataclass
class PaintEvent(Event):
    """Fired on WM_PAINT (raw HDC passed as context)."""
    hdc: int


@dataclasses.dataclass
class TimerEvent(Event):
    """Fired on WM_TIMER."""
    timer_id: int


# ---------------------------------------------------------------------------
# Event dispatcher
# ---------------------------------------------------------------------------

Handler = Callable[[Event], None]


class EventDispatcher:
    """Registry mapping (hwnd, message_type) → list[Handler].

    The WndProc calls :meth:`dispatch` for every message.  Handlers
    registered with :meth:`on` are invoked in registration order.
    """

    def __init__(self) -> None:
        # hwnd -> msg_type -> [handler, ...]
        self._handlers: dict[int, dict[str, list[Handler]]] = defaultdict(
            lambda: defaultdict(list)
        )
        # Global handlers (hwnd == 0 means "any window")
        self._global: dict[str, list[Handler]] = defaultdict(list)

    # ------------------------------------------------------------------
    # Registration helpers
    # ------------------------------------------------------------------

    def on(
        self,
        event_type: str,
        handler: Handler,
        hwnd: int = 0,
    ) -> None:
        """Register *handler* for *event_type* on *hwnd* (0 = any window)."""
        if hwnd:
            self._handlers[hwnd][event_type].append(handler)
        else:
            self._global[event_type].append(handler)

    def off(
        self,
        event_type: str,
        handler: Handler,
        hwnd: int = 0,
    ) -> None:
        """Remove a previously registered handler."""
        if hwnd:
            lst = self._handlers[hwnd].get(event_type, [])
        else:
            lst = self._global.get(event_type, [])
        if handler in lst:
            lst.remove(handler)

    # ------------------------------------------------------------------
    # Dispatch
    # ------------------------------------------------------------------

    def dispatch(self, event: Event) -> None:
        """Call all handlers registered for the event's type and hwnd."""
        etype = type(event).__name__
        # Per-window handlers
        for h in list(self._handlers.get(event.hwnd, {}).get(etype, [])):
            h(event)
        # Global handlers
        for h in list(self._global.get(etype, [])):
            h(event)

    def clear(self, hwnd: int) -> None:
        """Remove all handlers registered for *hwnd*."""
        self._handlers.pop(hwnd, None)
