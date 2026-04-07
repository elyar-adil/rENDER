"""Native Win32 widgets for the PyX GUI framework.

Each widget class is a thin Python wrapper around a child Win32 control
created via CreateWindowExW.  Control IDs are auto-assigned and callbacks
are wired through the parent window's :class:`~pyx.gui.events.EventDispatcher`.

Available widgets
-----------------
- :class:`Button`       – push button (BS_PUSHBUTTON)
- :class:`Label`        – static text (SS_LEFT)
- :class:`TextInput`    – single-line edit (ES_AUTOHSCROLL)
- :class:`MultilineEdit`– multi-line edit (ES_MULTILINE | ES_AUTOVSCROLL)
- :class:`CheckBox`     – auto-checkbox (BS_AUTOCHECKBOX)
- :class:`RadioButton`  – auto radio button (BS_AUTORADIOBUTTON)
- :class:`GroupBox`     – group box frame (BS_GROUPBOX)
- :class:`ListBox`      – list box (LBS_STANDARD)
- :class:`ComboBox`     – drop-down combo (CBS_DROPDOWNLIST)
- :class:`ProgressBar`  – progress bar via comctl32
"""
from __future__ import annotations

import ctypes
from typing import Callable, Sequence

from . import win32_api as w32
from .events import (
    ClickEvent,
    Event,
    SelectionChangedEvent,
)
from .window import Window, _next_ctrl_id

# Common message IDs not in win32_api
LB_ADDSTRING: int = 0x0180
LB_DELETESTRING: int = 0x0182
LB_GETCOUNT: int = 0x018B
LB_GETCURSEL: int = 0x0188
LB_SETCURSEL: int = 0x0186
LB_GETTEXT: int = 0x0189
LB_GETTEXTLEN: int = 0x018A
LB_RESETCONTENT: int = 0x0184

CB_ADDSTRING: int = 0x0143
CB_DELETESTRING: int = 0x0144
CB_GETCOUNT: int = 0x0146
CB_GETCURSEL: int = 0x0147
CB_SETCURSEL: int = 0x014E
CB_GETLBTEXT: int = 0x0148
CB_GETLBTEXTLEN: int = 0x0149
CB_RESETCONTENT: int = 0x014B

PBM_SETRANGE: int = 0x0401
PBM_SETPOS: int = 0x0402
PBM_GETPOS: int = 0x0408
PBM_SETMARQUEE: int = 0x040A
PROGRESS_CLASS: str = "msctls_progress32"
PBS_SMOOTH: int = 0x01
PBS_MARQUEE: int = 0x08

BM_GETCHECK: int = 0x00F0
BM_SETCHECK: int = 0x00F1
BST_UNCHECKED: int = 0x0000
BST_CHECKED: int = 0x0001
BST_INDETERMINATE: int = 0x0002

EM_SETREADONLY: int = 0x00CF
EM_SETLIMITTEXT: int = 0x00C5
EM_GETLINECOUNT: int = 0x00BA


# ---------------------------------------------------------------------------
# Base widget
# ---------------------------------------------------------------------------


class Widget:
    """Abstract base for all PyX GUI widgets."""

    def __init__(
        self,
        parent: Window,
        class_name: str,
        text: str,
        style: int,
        x: int,
        y: int,
        width: int,
        height: int,
        ex_style: int = 0,
    ) -> None:
        self._parent = parent
        self._ctrl_id: int = _next_ctrl_id()
        self._hwnd: int = 0

        if w32.IS_WINDOWS:
            self._hwnd = w32.create_window(
                class_name=class_name,
                title=text,
                style=w32.WS_CHILD | w32.WS_VISIBLE | style,
                x=x,
                y=y,
                width=width,
                height=height,
                parent=parent.hwnd,
                menu=self._ctrl_id,
                hinstance=w32.get_module_handle(),
                ex_style=ex_style,
            )
            if not self._hwnd:
                raise RuntimeError(
                    f"CreateWindowExW failed for class '{class_name}'"
                )
        parent._add_child(self)

    @property
    def hwnd(self) -> int:
        return self._hwnd

    @property
    def ctrl_id(self) -> int:
        return self._ctrl_id

    def set_text(self, text: str) -> None:
        if self._hwnd:
            w32.set_window_text(self._hwnd, text)

    def get_text(self) -> str:
        if not self._hwnd:
            return ""
        return w32.get_window_text(self._hwnd)

    def move(self, x: int, y: int, width: int, height: int) -> None:
        if self._hwnd:
            w32.move_window(self._hwnd, x, y, width, height)

    def enable(self, enabled: bool = True) -> None:
        if self._hwnd:
            w32.enable_window(self._hwnd, enabled)

    def focus(self) -> None:
        if self._hwnd:
            w32.set_focus(self._hwnd)

    def destroy(self) -> None:
        if self._hwnd:
            w32.destroy_window(self._hwnd)
            self._hwnd = 0


# ---------------------------------------------------------------------------
# Button
# ---------------------------------------------------------------------------


class Button(Widget):
    """A push button widget.

    Parameters
    ----------
    parent:  Parent window.
    text:    Button label text.
    x/y:     Position relative to parent client area.
    width/height: Dimensions in pixels.
    default: When *True*, styled as the default button (BS_DEFPUSHBUTTON).
    """

    def __init__(
        self,
        parent: Window,
        text: str = "OK",
        x: int = 0,
        y: int = 0,
        width: int = 90,
        height: int = 30,
        default: bool = False,
    ) -> None:
        btn_style = w32.BS_DEFPUSHBUTTON if default else w32.BS_PUSHBUTTON
        super().__init__(
            parent=parent,
            class_name="BUTTON",
            text=text,
            style=btn_style | w32.WS_TABSTOP,
            x=x,
            y=y,
            width=width,
            height=height,
        )

    def on_click(self, handler: Callable[[ClickEvent], None]) -> None:
        """Register a click callback."""
        def _filter(event: Event) -> None:
            if isinstance(event, ClickEvent) and event.control_hwnd == self._hwnd:
                handler(event)
        self._parent.dispatcher.on("ClickEvent", _filter, self._parent.hwnd)


# ---------------------------------------------------------------------------
# Label
# ---------------------------------------------------------------------------


class Label(Widget):
    """A static text label (read-only, no border)."""

    def __init__(
        self,
        parent: Window,
        text: str = "",
        x: int = 0,
        y: int = 0,
        width: int = 200,
        height: int = 20,
        align: str = "left",
    ) -> None:
        align_styles = {"left": w32.SS_LEFT, "center": w32.SS_CENTER, "right": w32.SS_RIGHT}
        style = align_styles.get(align, w32.SS_LEFT) | w32.SS_NOPREFIX
        super().__init__(
            parent=parent,
            class_name="STATIC",
            text=text,
            style=style,
            x=x,
            y=y,
            width=width,
            height=height,
        )


# ---------------------------------------------------------------------------
# TextInput (single-line edit)
# ---------------------------------------------------------------------------


class TextInput(Widget):
    """A single-line text input field.

    Parameters
    ----------
    password:   Masks input with bullet characters.
    max_length: Maximum number of characters (0 = unlimited).
    readonly:   Make the field non-editable.
    """

    def __init__(
        self,
        parent: Window,
        text: str = "",
        x: int = 0,
        y: int = 0,
        width: int = 200,
        height: int = 24,
        password: bool = False,
        max_length: int = 0,
        readonly: bool = False,
    ) -> None:
        style = (
            w32.ES_LEFT
            | w32.ES_AUTOHSCROLL
            | w32.WS_TABSTOP
        )
        if password:
            style |= w32.ES_PASSWORD
        super().__init__(
            parent=parent,
            class_name="EDIT",
            text=text,
            style=style,
            x=x,
            y=y,
            width=width,
            height=height,
            ex_style=w32.WS_EX_CLIENTEDGE,
        )
        if max_length:
            w32.send_message(self._hwnd, EM_SETLIMITTEXT, max_length, 0)
        if readonly:
            w32.send_message(self._hwnd, EM_SETREADONLY, 1, 0)

    def on_change(self, handler: Callable[[str], None]) -> None:
        """Register a text-change callback receiving the new text."""
        from .events import TextChangedEvent

        def _filter(event: Event) -> None:
            if (
                isinstance(event, TextChangedEvent)
                and event.control_hwnd == self._hwnd
            ):
                handler(event.text)

        self._parent.dispatcher.on("TextChangedEvent", _filter, self._parent.hwnd)


# ---------------------------------------------------------------------------
# MultilineEdit
# ---------------------------------------------------------------------------


class MultilineEdit(Widget):
    """A multi-line text editor with vertical scrollbar."""

    def __init__(
        self,
        parent: Window,
        text: str = "",
        x: int = 0,
        y: int = 0,
        width: int = 400,
        height: int = 200,
        readonly: bool = False,
    ) -> None:
        style = (
            w32.ES_MULTILINE
            | w32.ES_AUTOVSCROLL
            | w32.ES_WANTRETURN
            | w32.WS_VSCROLL
            | w32.WS_TABSTOP
        )
        super().__init__(
            parent=parent,
            class_name="EDIT",
            text=text,
            style=style,
            x=x,
            y=y,
            width=width,
            height=height,
            ex_style=w32.WS_EX_CLIENTEDGE,
        )
        if readonly:
            w32.send_message(self._hwnd, EM_SETREADONLY, 1, 0)

    def line_count(self) -> int:
        return w32.send_message(self._hwnd, EM_GETLINECOUNT)


# ---------------------------------------------------------------------------
# CheckBox
# ---------------------------------------------------------------------------


class CheckBox(Widget):
    """An auto-checkbox control."""

    def __init__(
        self,
        parent: Window,
        text: str = "",
        x: int = 0,
        y: int = 0,
        width: int = 200,
        height: int = 24,
        checked: bool = False,
    ) -> None:
        super().__init__(
            parent=parent,
            class_name="BUTTON",
            text=text,
            style=w32.BS_AUTOCHECKBOX | w32.WS_TABSTOP,
            x=x,
            y=y,
            width=width,
            height=height,
        )
        if checked:
            self.set_checked(True)

    def is_checked(self) -> bool:
        return w32.send_message(self._hwnd, BM_GETCHECK) == BST_CHECKED

    def set_checked(self, checked: bool) -> None:
        w32.send_message(self._hwnd, BM_SETCHECK,
                         BST_CHECKED if checked else BST_UNCHECKED, 0)

    def on_click(self, handler: Callable[[bool], None]) -> None:
        """Register callback receiving the new checked state."""
        def _filter(event: Event) -> None:
            if isinstance(event, ClickEvent) and event.control_hwnd == self._hwnd:
                handler(self.is_checked())
        self._parent.dispatcher.on("ClickEvent", _filter, self._parent.hwnd)


# ---------------------------------------------------------------------------
# RadioButton
# ---------------------------------------------------------------------------


class RadioButton(Widget):
    """An auto radio button."""

    def __init__(
        self,
        parent: Window,
        text: str = "",
        x: int = 0,
        y: int = 0,
        width: int = 200,
        height: int = 24,
        checked: bool = False,
    ) -> None:
        super().__init__(
            parent=parent,
            class_name="BUTTON",
            text=text,
            style=w32.BS_AUTORADIOBUTTON | w32.WS_TABSTOP,
            x=x,
            y=y,
            width=width,
            height=height,
        )
        if checked:
            self.set_checked(True)

    def is_checked(self) -> bool:
        return w32.send_message(self._hwnd, BM_GETCHECK) == BST_CHECKED

    def set_checked(self, checked: bool) -> None:
        w32.send_message(self._hwnd, BM_SETCHECK,
                         BST_CHECKED if checked else BST_UNCHECKED, 0)


# ---------------------------------------------------------------------------
# GroupBox
# ---------------------------------------------------------------------------


class GroupBox(Widget):
    """A labelled group-box frame (BS_GROUPBOX)."""

    def __init__(
        self,
        parent: Window,
        text: str = "",
        x: int = 0,
        y: int = 0,
        width: int = 300,
        height: int = 150,
    ) -> None:
        super().__init__(
            parent=parent,
            class_name="BUTTON",
            text=text,
            style=w32.BS_GROUPBOX,
            x=x,
            y=y,
            width=width,
            height=height,
        )


# ---------------------------------------------------------------------------
# ListBox
# ---------------------------------------------------------------------------


class ListBox(Widget):
    """A list box control."""

    def __init__(
        self,
        parent: Window,
        items: Sequence[str] | None = None,
        x: int = 0,
        y: int = 0,
        width: int = 200,
        height: int = 150,
        multi_select: bool = False,
    ) -> None:
        style = w32.LBS_NOTIFY | w32.WS_VSCROLL | w32.WS_TABSTOP
        if multi_select:
            style |= w32.LBS_MULTIPLESEL
        super().__init__(
            parent=parent,
            class_name="LISTBOX",
            text="",
            style=style,
            x=x,
            y=y,
            width=width,
            height=height,
            ex_style=w32.WS_EX_CLIENTEDGE,
        )
        for item in (items or []):
            self.add_item(item)

    def add_item(self, text: str) -> int:
        return w32.send_message(self._hwnd, LB_ADDSTRING, 0,
                                ctypes.cast(ctypes.create_unicode_buffer(text),
                                            ctypes.c_void_p).value or 0)

    def remove_item(self, index: int) -> None:
        w32.send_message(self._hwnd, LB_DELETESTRING, index, 0)

    def clear(self) -> None:
        w32.send_message(self._hwnd, LB_RESETCONTENT, 0, 0)

    def count(self) -> int:
        return w32.send_message(self._hwnd, LB_GETCOUNT)

    def selected_index(self) -> int:
        return w32.send_message(self._hwnd, LB_GETCURSEL)

    def select(self, index: int) -> None:
        w32.send_message(self._hwnd, LB_SETCURSEL, index, 0)

    def item_text(self, index: int) -> str:
        length = w32.send_message(self._hwnd, LB_GETTEXTLEN, index, 0)
        if length <= 0:
            return ""
        buf = ctypes.create_unicode_buffer(length + 1)
        w32.send_message(self._hwnd, LB_GETTEXT, index,
                         ctypes.cast(buf, ctypes.c_void_p).value or 0)
        return buf.value

    def on_selection_changed(self, handler: Callable[[int], None]) -> None:
        """Register callback receiving the newly selected index."""
        def _filter(event: Event) -> None:
            if (
                isinstance(event, SelectionChangedEvent)
                and event.control_hwnd == self._hwnd
            ):
                handler(event.index)
        self._parent.dispatcher.on("SelectionChangedEvent", _filter,
                                   self._parent.hwnd)


# ---------------------------------------------------------------------------
# ComboBox
# ---------------------------------------------------------------------------


class ComboBox(Widget):
    """A drop-down combo box (CBS_DROPDOWNLIST)."""

    def __init__(
        self,
        parent: Window,
        items: Sequence[str] | None = None,
        x: int = 0,
        y: int = 0,
        width: int = 200,
        height: int = 200,  # includes the drop-down height
    ) -> None:
        super().__init__(
            parent=parent,
            class_name="COMBOBOX",
            text="",
            style=w32.CBS_DROPDOWNLIST | w32.CBS_AUTOHSCROLL | w32.WS_TABSTOP,
            x=x,
            y=y,
            width=width,
            height=height,
        )
        for item in (items or []):
            self.add_item(item)

    def add_item(self, text: str) -> int:
        return w32.send_message(self._hwnd, CB_ADDSTRING, 0,
                                ctypes.cast(ctypes.create_unicode_buffer(text),
                                            ctypes.c_void_p).value or 0)

    def remove_item(self, index: int) -> None:
        w32.send_message(self._hwnd, CB_DELETESTRING, index, 0)

    def clear(self) -> None:
        w32.send_message(self._hwnd, CB_RESETCONTENT, 0, 0)

    def count(self) -> int:
        return w32.send_message(self._hwnd, CB_GETCOUNT)

    def selected_index(self) -> int:
        return w32.send_message(self._hwnd, CB_GETCURSEL)

    def select(self, index: int) -> None:
        w32.send_message(self._hwnd, CB_SETCURSEL, index, 0)

    def item_text(self, index: int) -> str:
        length = w32.send_message(self._hwnd, CB_GETLBTEXTLEN, index, 0)
        if length <= 0:
            return ""
        buf = ctypes.create_unicode_buffer(length + 1)
        w32.send_message(self._hwnd, CB_GETLBTEXT, index,
                         ctypes.cast(buf, ctypes.c_void_p).value or 0)
        return buf.value

    def on_selection_changed(self, handler: Callable[[int], None]) -> None:
        def _filter(event: Event) -> None:
            if (
                isinstance(event, SelectionChangedEvent)
                and event.control_hwnd == self._hwnd
            ):
                handler(event.index)
        self._parent.dispatcher.on("SelectionChangedEvent", _filter,
                                   self._parent.hwnd)


# ---------------------------------------------------------------------------
# ProgressBar
# ---------------------------------------------------------------------------


class ProgressBar(Widget):
    """A progress bar using the msctls_progress32 control class."""

    def __init__(
        self,
        parent: Window,
        x: int = 0,
        y: int = 0,
        width: int = 300,
        height: int = 22,
        minimum: int = 0,
        maximum: int = 100,
        smooth: bool = True,
    ) -> None:
        style = PBS_SMOOTH if smooth else 0
        super().__init__(
            parent=parent,
            class_name=PROGRESS_CLASS,
            text="",
            style=style,
            x=x,
            y=y,
            width=width,
            height=height,
        )
        self.set_range(minimum, maximum)

    def set_range(self, minimum: int, maximum: int) -> None:
        w32.send_message(self._hwnd, PBM_SETRANGE, 0,
                         (maximum << 16) | (minimum & 0xFFFF))

    def set_value(self, value: int) -> None:
        w32.send_message(self._hwnd, PBM_SETPOS, value, 0)

    def get_value(self) -> int:
        return w32.send_message(self._hwnd, PBM_GETPOS)
