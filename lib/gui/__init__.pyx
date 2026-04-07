"""PyX GUI framework – ctypes-based native Windows GUI.

This package provides a lightweight GUI library built directly on the
Win32 API via :mod:`ctypes`.  It requires no external dependencies and
runs without Qt, wxWidgets, or any other third-party toolkit.

Quick-start example::

    from pyx.gui import Application, Window
    from pyx.gui.widgets import Button, Label

    app = Application()
    win = app.create_window("Hello PyX", width=400, height=200)

    lbl = Label(win, "Hello, world!", x=20, y=20, width=360, height=30)
    btn = Button(win, "Quit", x=150, y=80, width=100, height=30)
    btn.on_click(lambda _: app.quit())

    win.show()
    raise SystemExit(app.run())

Platform support
----------------
- **Windows 10/11 (x86-64)**: full support via Win32 API
- **Linux / macOS**: module imports without error but creates no windows
  (useful for unit tests on CI)
"""
from __future__ import annotations

from .app import Application
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
from .layout import GridLayout, HBoxLayout, Rect, VBoxLayout
from .widgets import (
    Button,
    CheckBox,
    ComboBox,
    GroupBox,
    Label,
    ListBox,
    MultilineEdit,
    ProgressBar,
    RadioButton,
    TextInput,
)
from .window import Window

__all__ = [
    # Application
    "Application",
    # Windows
    "Window",
    # Widgets
    "Button",
    "CheckBox",
    "ComboBox",
    "GroupBox",
    "Label",
    "ListBox",
    "MultilineEdit",
    "ProgressBar",
    "RadioButton",
    "TextInput",
    # Events
    "Event",
    "ClickEvent",
    "CloseEvent",
    "DestroyEvent",
    "EventDispatcher",
    "KeyEvent",
    "MouseEvent",
    "ResizeEvent",
    "TextChangedEvent",
    "TimerEvent",
    # Layouts
    "GridLayout",
    "HBoxLayout",
    "Rect",
    "VBoxLayout",
]
