"""Low-level Win32 API bindings via ctypes.

This module exposes just enough of the Windows API to implement a minimal
GUI framework.  All declarations are plain ctypes – no C headers required.

Supported targets: Windows x86-64 (tested on Windows 10/11).
"""

import ctypes
import ctypes.wintypes as wt
import sys

# ---------------------------------------------------------------------------
# Availability guard
# ---------------------------------------------------------------------------

IS_WINDOWS: bool = sys.platform == "win32"

if IS_WINDOWS:
    _user32: ctypes.WinDLL = ctypes.windll.user32
    _kernel32: ctypes.WinDLL = ctypes.windll.kernel32
    _gdi32: ctypes.WinDLL = ctypes.windll.gdi32
    _comctl32: ctypes.WinDLL = ctypes.windll.comctl32
else:
    # Stub objects so the module can be imported on non-Windows for testing.
    _user32 = None  # type: ignore[assignment]
    _kernel32 = None  # type: ignore[assignment]
    _gdi32 = None  # type: ignore[assignment]
    _comctl32 = None  # type: ignore[assignment]

# ---------------------------------------------------------------------------
# Window styles
# ---------------------------------------------------------------------------

WS_OVERLAPPED: int = 0x00000000
WS_CAPTION: int = 0x00C00000
WS_SYSMENU: int = 0x00080000
WS_THICKFRAME: int = 0x00040000
WS_MINIMIZEBOX: int = 0x00020000
WS_MAXIMIZEBOX: int = 0x00010000
WS_OVERLAPPEDWINDOW: int = (
    WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU |
    WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX
)
WS_VISIBLE: int = 0x10000000
WS_CHILD: int = 0x40000000
WS_CLIPCHILDREN: int = 0x02000000
WS_CLIPSIBLINGS: int = 0x04000000
WS_BORDER: int = 0x00800000
WS_VSCROLL: int = 0x00200000
WS_HSCROLL: int = 0x00100000
WS_TABSTOP: int = 0x00010000
WS_GROUP: int = 0x00020000

# Extended window styles
WS_EX_CLIENTEDGE: int = 0x00000200
WS_EX_WINDOWEDGE: int = 0x00000100
WS_EX_APPWINDOW: int = 0x00040000
WS_EX_TOPMOST: int = 0x00000008
WS_EX_CONTROLPARENT: int = 0x00010000

# Button styles
BS_PUSHBUTTON: int = 0x00000000
BS_DEFPUSHBUTTON: int = 0x00000001
BS_CHECKBOX: int = 0x00000002
BS_AUTOCHECKBOX: int = 0x00000003
BS_RADIOBUTTON: int = 0x00000004
BS_AUTORADIOBUTTON: int = 0x00000009
BS_GROUPBOX: int = 0x00000007
BS_FLAT: int = 0x00008000

# Edit styles
ES_LEFT: int = 0x0000
ES_CENTER: int = 0x0001
ES_RIGHT: int = 0x0002
ES_MULTILINE: int = 0x0004
ES_AUTOHSCROLL: int = 0x0080
ES_AUTOVSCROLL: int = 0x0040
ES_PASSWORD: int = 0x0020
ES_READONLY: int = 0x0800
ES_WANTRETURN: int = 0x1000

# Static (label) styles
SS_LEFT: int = 0x00000000
SS_CENTER: int = 0x00000001
SS_RIGHT: int = 0x00000002
SS_SIMPLE: int = 0x0000000B
SS_NOPREFIX: int = 0x00000080

# Listbox styles
LBS_STANDARD: int = 0x00A00001
LBS_NOTIFY: int = 0x0001
LBS_SORT: int = 0x0002
LBS_NOREDRAW: int = 0x0004
LBS_MULTIPLESEL: int = 0x0008

# Combobox styles
CBS_SIMPLE: int = 0x0001
CBS_DROPDOWN: int = 0x0002
CBS_DROPDOWNLIST: int = 0x0003
CBS_AUTOHSCROLL: int = 0x0040
CBS_SORT: int = 0x0100

# ---------------------------------------------------------------------------
# Class styles
# ---------------------------------------------------------------------------

CS_VREDRAW: int = 0x0001
CS_HREDRAW: int = 0x0002
CS_DBLCLKS: int = 0x0008
CS_OWNDC: int = 0x0020

# ---------------------------------------------------------------------------
# Window messages
# ---------------------------------------------------------------------------

WM_NULL: int = 0x0000
WM_CREATE: int = 0x0001
WM_DESTROY: int = 0x0002
WM_MOVE: int = 0x0003
WM_SIZE: int = 0x0005
WM_ACTIVATE: int = 0x0006
WM_PAINT: int = 0x000F
WM_CLOSE: int = 0x0010
WM_QUIT: int = 0x0012
WM_ERASEBKGND: int = 0x0014
WM_SHOWWINDOW: int = 0x0018
WM_SETFOCUS: int = 0x0007
WM_KILLFOCUS: int = 0x0008
WM_KEYDOWN: int = 0x0100
WM_KEYUP: int = 0x0101
WM_CHAR: int = 0x0102
WM_SYSKEYDOWN: int = 0x0104
WM_SYSKEYUP: int = 0x0105
WM_COMMAND: int = 0x0111
WM_TIMER: int = 0x0113
WM_HSCROLL: int = 0x0114
WM_VSCROLL: int = 0x0115
WM_LBUTTONDOWN: int = 0x0201
WM_LBUTTONUP: int = 0x0202
WM_LBUTTONDBLCLK: int = 0x0203
WM_RBUTTONDOWN: int = 0x0204
WM_RBUTTONUP: int = 0x0205
WM_RBUTTONDBLCLK: int = 0x0206
WM_MBUTTONDOWN: int = 0x0207
WM_MBUTTONUP: int = 0x0208
WM_MOUSEMOVE: int = 0x0200
WM_MOUSEWHEEL: int = 0x020A
WM_CONTEXTMENU: int = 0x007B
WM_NOTIFY: int = 0x004E
WM_SETTEXT: int = 0x000C
WM_GETTEXT: int = 0x000D
WM_GETTEXTLENGTH: int = 0x000E
WM_SETFONT: int = 0x0030
WM_GETFONT: int = 0x0031
WM_INITDIALOG: int = 0x0110
WM_CTLCOLORSTATIC: int = 0x0138
WM_CTLCOLOREDIT: int = 0x0133
WM_CTLCOLORBTN: int = 0x0135
WM_CTLCOLORLISTBOX: int = 0x0132
WM_ERASEBKGND: int = 0x0014

# ---------------------------------------------------------------------------
# WM_COMMAND notification codes (hi-word of wParam)
# ---------------------------------------------------------------------------

BN_CLICKED: int = 0
EN_CHANGE: int = 0x0300
EN_SETFOCUS: int = 0x0100
EN_KILLFOCUS: int = 0x0200
CBN_SELCHANGE: int = 1
LBN_SELCHANGE: int = 1

# ---------------------------------------------------------------------------
# Virtual key codes (subset)
# ---------------------------------------------------------------------------

VK_BACK: int = 0x08
VK_TAB: int = 0x09
VK_RETURN: int = 0x0D
VK_ESCAPE: int = 0x1B
VK_SPACE: int = 0x20
VK_END: int = 0x23
VK_HOME: int = 0x24
VK_LEFT: int = 0x25
VK_UP: int = 0x26
VK_RIGHT: int = 0x27
VK_DOWN: int = 0x28
VK_DELETE: int = 0x2E
VK_F1: int = 0x70
VK_F4: int = 0x73
VK_F5: int = 0x74
VK_F12: int = 0x7B

# ---------------------------------------------------------------------------
# Show-window commands
# ---------------------------------------------------------------------------

SW_HIDE: int = 0
SW_SHOWNORMAL: int = 1
SW_SHOWMINIMIZED: int = 2
SW_SHOWMAXIMIZED: int = 3
SW_SHOWNOACTIVATE: int = 4
SW_SHOW: int = 5
SW_MINIMIZE: int = 6
SW_RESTORE: int = 9
SW_SHOWDEFAULT: int = 10

# ---------------------------------------------------------------------------
# Misc constants
# ---------------------------------------------------------------------------

IDC_ARROW: int = 32512
IDI_APPLICATION: int = 32512
COLOR_WINDOW: int = 5
COLOR_BTNFACE: int = 15
COLOR_WINDOWTEXT: int = 8
HWND_DESKTOP: int = 0
CW_USEDEFAULT: int = 0x80000000
NULL: int = 0

# GDI stock objects
DEFAULT_GUI_FONT: int = 17
SYSTEM_FONT: int = 13
SYSTEM_FIXED_FONT: int = 16
WHITE_BRUSH: int = 0
LTGRAY_BRUSH: int = 1
GRAY_BRUSH: int = 2
DKGRAY_BRUSH: int = 3
BLACK_BRUSH: int = 4
NULL_BRUSH: int = 5

# SetWindowLong / GetWindowLong indices
GWL_STYLE: int = -16
GWL_EXSTYLE: int = -20
GWL_WNDPROC: int = -4
GWLP_USERDATA: int = -21
GWLP_ID: int = -12

# Message box flags
MB_OK: int = 0x00000000
MB_OKCANCEL: int = 0x00000001
MB_ABORTRETRYIGNORE: int = 0x00000002
MB_YESNOCANCEL: int = 0x00000003
MB_YESNO: int = 0x00000004
MB_RETRYCANCEL: int = 0x00000005
MB_ICONERROR: int = 0x00000010
MB_ICONQUESTION: int = 0x00000020
MB_ICONWARNING: int = 0x00000030
MB_ICONINFORMATION: int = 0x00000040
IDOK: int = 1
IDCANCEL: int = 2
IDABORT: int = 3
IDRETRY: int = 4
IDIGNORE: int = 5
IDYES: int = 6
IDNO: int = 7

# ---------------------------------------------------------------------------
# Portable handle type aliases
# On non-Windows platforms ctypes.wintypes omits GDI/USER handle typedefs,
# so we alias them to c_void_p for import-time compatibility.
# ---------------------------------------------------------------------------

if IS_WINDOWS:
    _HANDLE   = wt.HANDLE    if hasattr(wt, "HANDLE")   else ctypes.c_void_p
    _HINSTANCE = wt.HINSTANCE if hasattr(wt, "HINSTANCE") else ctypes.c_void_p
    _HICON    = wt.HICON     if hasattr(wt, "HICON")    else ctypes.c_void_p
    _HCURSOR  = wt.HCURSOR   if hasattr(wt, "HCURSOR")  else ctypes.c_void_p
    _HBRUSH   = wt.HBRUSH    if hasattr(wt, "HBRUSH")   else ctypes.c_void_p
    _HDC      = wt.HDC       if hasattr(wt, "HDC")      else ctypes.c_void_p
    _HWND     = wt.HWND      if hasattr(wt, "HWND")     else ctypes.c_void_p
    _UINT     = wt.UINT      if hasattr(wt, "UINT")     else ctypes.c_uint
    _WPARAM   = wt.WPARAM    if hasattr(wt, "WPARAM")   else ctypes.c_ulonglong
    _LPARAM   = wt.LPARAM    if hasattr(wt, "LPARAM")   else ctypes.c_longlong
    _DWORD    = wt.DWORD     if hasattr(wt, "DWORD")    else ctypes.c_ulong
    _LONG     = wt.LONG      if hasattr(wt, "LONG")     else ctypes.c_long
    _BOOL     = wt.BOOL      if hasattr(wt, "BOOL")     else ctypes.c_int
    _LPCWSTR  = wt.LPCWSTR   if hasattr(wt, "LPCWSTR")  else ctypes.c_wchar_p
    _RECT     = wt.RECT      if hasattr(wt, "RECT")     else ctypes.c_byte * 16
    _POINT    = wt.POINT     if hasattr(wt, "POINT")    else ctypes.c_byte * 8
else:
    _HANDLE = _HINSTANCE = _HICON = _HCURSOR = _HBRUSH = _HDC = _HWND = ctypes.c_void_p
    _UINT = ctypes.c_uint
    _WPARAM = ctypes.c_ulonglong
    _LPARAM = ctypes.c_longlong
    _DWORD = ctypes.c_ulong
    _LONG = ctypes.c_long
    _BOOL = ctypes.c_int
    _LPCWSTR = ctypes.c_wchar_p
    _RECT = ctypes.c_byte * 16
    _POINT = ctypes.c_byte * 8

# ---------------------------------------------------------------------------
# Structures
# ---------------------------------------------------------------------------


class WNDCLASSEXW(ctypes.Structure):
    _fields_ = [
        ("cbSize",        _UINT),
        ("style",         _UINT),
        ("lpfnWndProc",   ctypes.c_void_p),
        ("cbClsExtra",    ctypes.c_int),
        ("cbWndExtra",    ctypes.c_int),
        ("hInstance",     _HINSTANCE),
        ("hIcon",         _HICON),
        ("hCursor",       _HCURSOR),
        ("hbrBackground", _HBRUSH),
        ("lpszMenuName",  _LPCWSTR),
        ("lpszClassName", _LPCWSTR),
        ("hIconSm",       _HICON),
    ]


class MSG(ctypes.Structure):
    _fields_ = [
        ("hwnd",    _HWND),
        ("message", _UINT),
        ("wParam",  _WPARAM),
        ("lParam",  _LPARAM),
        ("time",    _DWORD),
        ("pt",      _POINT),
    ]


class PAINTSTRUCT(ctypes.Structure):
    _fields_ = [
        ("hdc",         _HDC),
        ("fErase",      _BOOL),
        ("rcPaint",     _RECT),
        ("fRestore",    _BOOL),
        ("fIncUpdate",  _BOOL),
        ("rgbReserved", ctypes.c_byte * 32),
    ]


class LOGFONTW(ctypes.Structure):
    _fields_ = [
        ("lfHeight",          _LONG),
        ("lfWidth",           _LONG),
        ("lfEscapement",      _LONG),
        ("lfOrientation",     _LONG),
        ("lfWeight",          _LONG),
        ("lfItalic",          ctypes.c_byte),
        ("lfUnderline",       ctypes.c_byte),
        ("lfStrikeOut",       ctypes.c_byte),
        ("lfCharSet",         ctypes.c_byte),
        ("lfOutPrecision",    ctypes.c_byte),
        ("lfClipPrecision",   ctypes.c_byte),
        ("lfQuality",         ctypes.c_byte),
        ("lfPitchAndFamily",  ctypes.c_byte),
        ("lfFaceName",        ctypes.c_wchar * 32),
    ]


# ---------------------------------------------------------------------------
# Callback type for window procedures
# ---------------------------------------------------------------------------

WNDPROC = ctypes.WINFUNCTYPE(
    ctypes.c_long,          # return: LRESULT
    wt.HWND,                # hwnd
    wt.UINT,                # uMsg
    wt.WPARAM,              # wParam
    wt.LPARAM,              # lParam
) if IS_WINDOWS else None  # type: ignore[assignment]

# ---------------------------------------------------------------------------
# Typed function wrappers
# ---------------------------------------------------------------------------


def get_module_handle(module_name: str | None = None) -> int:
    """Return the HINSTANCE of the current process (or *module_name*)."""
    if not IS_WINDOWS:
        return 0
    return _kernel32.GetModuleHandleW(module_name)


def register_class(wc: WNDCLASSEXW) -> int:
    """Register a window class; returns atom or 0 on failure."""
    if not IS_WINDOWS:
        return 0
    return _user32.RegisterClassExW(ctypes.byref(wc))


def create_window(
    class_name: str,
    title: str,
    style: int,
    x: int,
    y: int,
    width: int,
    height: int,
    parent: int,
    menu: int,
    hinstance: int,
    ex_style: int = 0,
) -> int:
    """Thin wrapper around CreateWindowExW; returns HWND or 0."""
    if not IS_WINDOWS:
        return 0
    return _user32.CreateWindowExW(
        ex_style, class_name, title, style,
        x, y, width, height,
        parent, menu, hinstance, None,
    )


def show_window(hwnd: int, cmd: int = SW_SHOWNORMAL) -> bool:
    if not IS_WINDOWS:
        return False
    return bool(_user32.ShowWindow(hwnd, cmd))


def update_window(hwnd: int) -> bool:
    if not IS_WINDOWS:
        return False
    return bool(_user32.UpdateWindow(hwnd))


def destroy_window(hwnd: int) -> bool:
    if not IS_WINDOWS:
        return False
    return bool(_user32.DestroyWindow(hwnd))


def post_quit_message(exit_code: int = 0) -> None:
    if IS_WINDOWS:
        _user32.PostQuitMessage(exit_code)


def def_window_proc(hwnd: int, msg: int, wparam: int, lparam: int) -> int:
    if not IS_WINDOWS:
        return 0
    return _user32.DefWindowProcW(hwnd, msg, wparam, lparam)


def get_message(msg: MSG, hwnd: int = 0) -> int:
    """Return >0 for normal message, 0 for WM_QUIT, -1 on error."""
    if not IS_WINDOWS:
        return 0
    return _user32.GetMessageW(ctypes.byref(msg), hwnd, 0, 0)


def translate_message(msg: MSG) -> bool:
    if not IS_WINDOWS:
        return False
    return bool(_user32.TranslateMessage(ctypes.byref(msg)))


def dispatch_message(msg: MSG) -> int:
    if not IS_WINDOWS:
        return 0
    return _user32.DispatchMessageW(ctypes.byref(msg))


def peek_message(msg: MSG, hwnd: int = 0, remove: bool = True) -> bool:
    """Non-blocking message check.  Returns True if a message was available."""
    if not IS_WINDOWS:
        return False
    PM_REMOVE = 0x0001
    PM_NOREMOVE = 0x0000
    flag = PM_REMOVE if remove else PM_NOREMOVE
    return bool(_user32.PeekMessageW(ctypes.byref(msg), hwnd, 0, 0, flag))


def set_window_text(hwnd: int, text: str) -> bool:
    if not IS_WINDOWS:
        return False
    return bool(_user32.SetWindowTextW(hwnd, text))


def get_window_text(hwnd: int) -> str:
    if not IS_WINDOWS:
        return ""
    length: int = _user32.GetWindowTextLengthW(hwnd)
    buf = ctypes.create_unicode_buffer(length + 1)
    _user32.GetWindowTextW(hwnd, buf, length + 1)
    return buf.value


def send_message(hwnd: int, msg: int, wparam: int = 0, lparam: int = 0) -> int:
    if not IS_WINDOWS:
        return 0
    return _user32.SendMessageW(hwnd, msg, wparam, lparam)


def post_message(hwnd: int, msg: int, wparam: int = 0, lparam: int = 0) -> bool:
    if not IS_WINDOWS:
        return False
    return bool(_user32.PostMessageW(hwnd, msg, wparam, lparam))


def get_stock_object(obj_id: int) -> int:
    """Return a handle to the requested GDI stock object."""
    if not IS_WINDOWS:
        return 0
    return _gdi32.GetStockObject(obj_id)


def load_cursor(cursor_id: int) -> int:
    if not IS_WINDOWS:
        return 0
    return _user32.LoadCursorW(None, cursor_id)


def load_icon(icon_id: int) -> int:
    if not IS_WINDOWS:
        return 0
    return _user32.LoadIconW(None, icon_id)


def message_box(
    hwnd: int,
    text: str,
    caption: str = "PyX",
    flags: int = MB_OK,
) -> int:
    """Show a modal message box.  Returns IDOK / IDYES / IDNO etc."""
    if not IS_WINDOWS:
        return IDOK
    return _user32.MessageBoxW(hwnd, text, caption, flags)


def get_client_rect(hwnd: int) -> tuple[int, int, int, int]:
    """Return (left, top, right, bottom) of the client area."""
    if not IS_WINDOWS:
        return (0, 0, 0, 0)
    rc = wt.RECT()
    _user32.GetClientRect(hwnd, ctypes.byref(rc))
    return (rc.left, rc.top, rc.right, rc.bottom)


def move_window(
    hwnd: int,
    x: int,
    y: int,
    width: int,
    height: int,
    repaint: bool = True,
) -> bool:
    if not IS_WINDOWS:
        return False
    return bool(_user32.MoveWindow(hwnd, x, y, width, height, int(repaint)))


def set_window_long(hwnd: int, index: int, value: int) -> int:
    """SetWindowLongPtrW."""
    if not IS_WINDOWS:
        return 0
    return _user32.SetWindowLongPtrW(hwnd, index, value)


def get_window_long(hwnd: int, index: int) -> int:
    """GetWindowLongPtrW."""
    if not IS_WINDOWS:
        return 0
    return _user32.GetWindowLongPtrW(hwnd, index)


def enable_window(hwnd: int, enable: bool = True) -> bool:
    if not IS_WINDOWS:
        return False
    return bool(_user32.EnableWindow(hwnd, int(enable)))


def set_focus(hwnd: int) -> int:
    if not IS_WINDOWS:
        return 0
    return _user32.SetFocus(hwnd)


def get_parent(hwnd: int) -> int:
    if not IS_WINDOWS:
        return 0
    return _user32.GetParent(hwnd)


def create_font(
    height: int = -13,
    face: str = "Segoe UI",
    weight: int = 400,
    italic: bool = False,
) -> int:
    """CreateFontW – returns an HFONT handle."""
    if not IS_WINDOWS:
        return 0
    return _gdi32.CreateFontW(
        height, 0, 0, 0, weight,
        int(italic), 0, 0,
        1,   # ANSI_CHARSET
        0, 0, 0, 0,
        face,
    )


def delete_object(handle: int) -> bool:
    if not IS_WINDOWS:
        return False
    return bool(_gdi32.DeleteObject(handle))


def invalidate_rect(hwnd: int, erase: bool = True) -> bool:
    if not IS_WINDOWS:
        return False
    return bool(_user32.InvalidateRect(hwnd, None, int(erase)))


def get_sys_color_brush(color_index: int) -> int:
    if not IS_WINDOWS:
        return 0
    return _user32.GetSysColorBrush(color_index)
