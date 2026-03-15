"""PyQt6 rendering backend for rENDER browser engine."""
import sys
import os
import math
from PyQt6.QtWidgets import (
    QApplication, QMainWindow, QWidget, QScrollArea,
    QVBoxLayout, QHBoxLayout, QLineEdit, QPushButton,
    QSizePolicy, QLabel
)
from PyQt6.QtGui import (
    QPainter, QColor, QFont, QFontMetrics, QPen, QBrush,
    QPixmap, QImage, QLinearGradient,
)
from PyQt6.QtCore import Qt, QRect, QSize, QPoint, pyqtSignal

from rendering.display_list import (
    DisplayList, DrawRect, DrawText, DrawBorder, DrawImage,
    PushClip, PopClip, PushOpacity, PopOpacity,
    PushTransform, PopTransform, DrawLinearGradient,
)


def _hsl_to_rgb(h: float, s: float, l: float) -> tuple:
    """Convert HSL (h in [0,360], s and l in [0,1]) to (r, g, b) each in [0,255]."""
    h = h % 360
    if s == 0:
        v = int(l * 255)
        return v, v, v
    if l < 0.5:
        q = l * (1 + s)
    else:
        q = l + s - l * s
    p = 2 * l - q

    def _hue(t):
        t = t % 1.0
        if t < 1 / 6:
            return p + (q - p) * 6 * t
        if t < 1 / 2:
            return q
        if t < 2 / 3:
            return p + (q - p) * (2 / 3 - t) * 6
        return p

    r = int(_hue((h / 360) + 1 / 3) * 255)
    g = int(_hue(h / 360) * 255)
    b = int(_hue((h / 360) - 1 / 3) * 255)
    return (max(0, min(255, r)), max(0, min(255, g)), max(0, min(255, b)))


def _parse_color(color_str: str) -> QColor:
    """Parse a CSS color string into QColor."""
    if not color_str or color_str in ('transparent', 'none'):
        return QColor(0, 0, 0, 0)

    color_str = color_str.strip()

    # Named colors (CSS3 full list)
    named = {
        'black': '#000000', 'white': '#ffffff', 'red': '#ff0000',
        'green': '#008000', 'blue': '#0000ff', 'yellow': '#ffff00',
        'orange': '#ffa500', 'purple': '#800080', 'pink': '#ffc0cb',
        'gray': '#808080', 'grey': '#808080', 'silver': '#c0c0c0',
        'aqua': '#00ffff', 'cyan': '#00ffff', 'magenta': '#ff00ff',
        'fuchsia': '#ff00ff', 'lime': '#00ff00', 'maroon': '#800000',
        'navy': '#000080', 'olive': '#808000', 'teal': '#008080',
        'brown': '#a52a2a', 'gold': '#ffd700', 'coral': '#ff7f50',
        'salmon': '#fa8072', 'khaki': '#f0e68c', 'indigo': '#4b0082',
        'violet': '#ee82ee', 'beige': '#f5f5dc', 'ivory': '#fffff0',
        'lavender': '#e6e6fa', 'turquoise': '#40e0d0', 'tan': '#d2b48c',
        'sienna': '#a0522d', 'crimson': '#dc143c', 'darkblue': '#00008b',
        'darkgreen': '#006400', 'darkred': '#8b0000', 'darkorange': '#ff8c00',
        'lightblue': '#add8e6', 'lightgreen': '#90ee90', 'lightgray': '#d3d3d3',
        'lightgrey': '#d3d3d3', 'currentcolor': '#000000', 'inherit': '#000000',
        # Extended CSS3 named colors
        'aliceblue': '#f0f8ff', 'antiquewhite': '#faebd7', 'aquamarine': '#7fffd4',
        'azure': '#f0ffff', 'bisque': '#ffe4c4', 'blanchedalmond': '#ffebcd',
        'blueviolet': '#8a2be2', 'burlywood': '#deb887', 'cadetblue': '#5f9ea0',
        'chartreuse': '#7fff00', 'chocolate': '#d2691e', 'cornflowerblue': '#6495ed',
        'cornsilk': '#fff8dc', 'darkcyan': '#008b8b', 'darkgoldenrod': '#b8860b',
        'darkgray': '#a9a9a9', 'darkgrey': '#a9a9a9', 'darkkhaki': '#bdb76b',
        'darkmagenta': '#8b008b', 'darkolivegreen': '#556b2f', 'darkorchid': '#9932cc',
        'darksalmon': '#e9967a', 'darkseagreen': '#8fbc8f', 'darkslateblue': '#483d8b',
        'darkslategray': '#2f4f4f', 'darkslategrey': '#2f4f4f',
        'darkturquoise': '#00ced1', 'darkviolet': '#9400d3', 'deeppink': '#ff1493',
        'deepskyblue': '#00bfff', 'dimgray': '#696969', 'dimgrey': '#696969',
        'dodgerblue': '#1e90ff', 'firebrick': '#b22222', 'floralwhite': '#fffaf0',
        'forestgreen': '#228b22', 'gainsboro': '#dcdcdc', 'ghostwhite': '#f8f8ff',
        'goldenrod': '#daa520', 'greenyellow': '#adff2f', 'honeydew': '#f0fff0',
        'hotpink': '#ff69b4', 'indianred': '#cd5c5c', 'lawngreen': '#7cfc00',
        'lemonchiffon': '#fffacd', 'lightcoral': '#f08080', 'lightcyan': '#e0ffff',
        'lightgoldenrodyellow': '#fafad2', 'lightpink': '#ffb6c1',
        'lightsalmon': '#ffa07a', 'lightseagreen': '#20b2aa', 'lightskyblue': '#87cefa',
        'lightslategray': '#778899', 'lightslategrey': '#778899',
        'lightsteelblue': '#b0c4de', 'lightyellow': '#ffffe0', 'limegreen': '#32cd32',
        'linen': '#faf0e6', 'mediumaquamarine': '#66cdaa', 'mediumblue': '#0000cd',
        'mediumorchid': '#ba55d3', 'mediumpurple': '#9370db',
        'mediumseagreen': '#3cb371', 'mediumslateblue': '#7b68ee',
        'mediumspringgreen': '#00fa9a', 'mediumturquoise': '#48d1cc',
        'mediumvioletred': '#c71585', 'midnightblue': '#191970',
        'mintcream': '#f5fffa', 'mistyrose': '#ffe4e1', 'moccasin': '#ffe4b5',
        'navajowhite': '#ffdead', 'oldlace': '#fdf5e6', 'olivedrab': '#6b8e23',
        'orangered': '#ff4500', 'orchid': '#da70d6', 'palegoldenrod': '#eee8aa',
        'palegreen': '#98fb98', 'paleturquoise': '#afeeee', 'palevioletred': '#db7093',
        'papayawhip': '#ffefd5', 'peachpuff': '#ffdab9', 'peru': '#cd853f',
        'plum': '#dda0dd', 'powderblue': '#b0e0e6', 'rosybrown': '#bc8f8f',
        'royalblue': '#4169e1', 'saddlebrown': '#8b4513', 'sandybrown': '#f4a460',
        'seagreen': '#2e8b57', 'seashell': '#fff5ee', 'skyblue': '#87ceeb',
        'slateblue': '#6a5acd', 'slategray': '#708090', 'slategrey': '#708090',
        'snow': '#fffafa', 'springgreen': '#00ff7f', 'steelblue': '#4682b4',
        'thistle': '#d8bfd8', 'tomato': '#ff6347', 'wheat': '#f5deb3',
        'whitesmoke': '#f5f5f5', 'yellowgreen': '#9acd32',
    }
    lower = color_str.lower()
    if lower in named:
        color_str = named[lower]

    # #rgb / #rrggbb / #rgba / #rrggbbaa
    if color_str.startswith('#'):
        hex_str = color_str[1:]
        if len(hex_str) == 3:
            hex_str = ''.join(c * 2 for c in hex_str)
        if len(hex_str) == 4:
            hex_str = ''.join(c * 2 for c in hex_str)
        if len(hex_str) == 6:
            r = int(hex_str[0:2], 16)
            g = int(hex_str[2:4], 16)
            b = int(hex_str[4:6], 16)
            return QColor(r, g, b)
        if len(hex_str) == 8:
            r = int(hex_str[0:2], 16)
            g = int(hex_str[2:4], 16)
            b = int(hex_str[4:6], 16)
            a = int(hex_str[6:8], 16)
            return QColor(r, g, b, a)

    # rgb(r, g, b) / rgba(r, g, b, a)
    import re
    m = re.match(r'rgba?\s*\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)(?:\s*,\s*([\d.]+))?\s*\)', color_str, re.I)
    if m:
        r, g, b = int(m.group(1)), int(m.group(2)), int(m.group(3))
        a = int(float(m.group(4)) * 255) if m.group(4) else 255
        return QColor(r, g, b, a)

    # hsl(h, s%, l%) / hsla(h, s%, l%, a)  — comma-separated
    m = re.match(
        r'hsla?\s*\(\s*([\d.]+)\s*,\s*([\d.]+)%\s*,\s*([\d.]+)%'
        r'(?:\s*,\s*([\d.]+))?\s*\)',
        color_str, re.I)
    if m:
        h = float(m.group(1))
        s = float(m.group(2)) / 100
        l = float(m.group(3)) / 100
        a = int(float(m.group(4)) * 255) if m.group(4) else 255
        r, g, b = _hsl_to_rgb(h, s, l)
        return QColor(r, g, b, a)

    # hsl(h s% l%) / hsl(h s% l% / a)  — modern space-separated
    m = re.match(
        r'hsla?\s*\(\s*([\d.]+)\s+([\d.]+)%\s+([\d.]+)%'
        r'(?:\s*/\s*([\d.]+))?\s*\)',
        color_str, re.I)
    if m:
        h = float(m.group(1))
        s = float(m.group(2)) / 100
        l = float(m.group(3)) / 100
        a = int(float(m.group(4)) * 255) if m.group(4) else 255
        r, g, b = _hsl_to_rgb(h, s, l)
        return QColor(r, g, b, a)

    # Try Qt's own parser as fallback
    c = QColor(color_str)
    return c if c.isValid() else QColor(0, 0, 0)


def _make_qfont(font_tuple: tuple) -> QFont:
    """Create QFont from (family, size_px[, weight_str[, style_str]]) tuple."""
    family = 'Arial'
    size_px = 16
    weight_str = 'normal'
    style_str = ''

    if isinstance(font_tuple, (list, tuple)):
        if len(font_tuple) >= 1:
            family = str(font_tuple[0]).split(',')[0].strip().strip('"\'') or 'Arial'
        if len(font_tuple) >= 2:
            try:
                size_px = max(1, int(font_tuple[1]))
            except (ValueError, TypeError):
                size_px = 16
        if len(font_tuple) >= 3:
            weight_str = str(font_tuple[2]).lower()
        if len(font_tuple) >= 4:
            style_str = str(font_tuple[3]).lower()

    font = QFont()
    font.setFamily(family)
    font.setPixelSize(size_px)
    if weight_str in ('bold', '700', '800', '900') or 'bold' in weight_str:
        font.setWeight(QFont.Weight.Bold)
    elif weight_str in ('600',):
        font.setWeight(QFont.Weight.DemiBold)
    if 'italic' in weight_str or 'italic' in style_str or 'oblique' in style_str:
        font.setItalic(True)
    return font


class RenderCanvas(QWidget):
    """Canvas that paints the display list using QPainter."""

    link_clicked = pyqtSignal(str)

    def __init__(self, parent=None):
        super().__init__(parent)
        self._display_list: DisplayList = DisplayList()
        self._links: list = []  # [(rect, href), ...]
        self.setMinimumSize(980, 600)
        self.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Expanding)
        self.setMouseTracking(True)
        self.setCursor(Qt.CursorShape.ArrowCursor)

    def set_display_list(self, dl: DisplayList) -> None:
        self._display_list = dl
        self._links = []
        self.update()

    def set_links(self, links: list) -> None:
        """Set [(rect, href)] list for click handling."""
        self._links = links

    def paintEvent(self, event):
        painter = QPainter(self)
        painter.setRenderHint(QPainter.RenderHint.Antialiasing)
        painter.fillRect(self.rect(), QColor(255, 255, 255))
        paint(self._display_list, painter)
        painter.end()

    def mouseMoveEvent(self, event):
        pos = event.position()
        x, y = pos.x(), pos.y()
        for rect, href in self._links:
            if rect.x <= x <= rect.x + rect.width and rect.y <= y <= rect.y + rect.height:
                self.setCursor(Qt.CursorShape.PointingHandCursor)
                return
        self.setCursor(Qt.CursorShape.ArrowCursor)

    def mousePressEvent(self, event):
        if event.button() == Qt.MouseButton.LeftButton:
            pos = event.position()
            x, y = pos.x(), pos.y()
            for rect, href in self._links:
                if rect.x <= x <= rect.x + rect.width and rect.y <= y <= rect.y + rect.height:
                    self.link_clicked.emit(href)
                    return

    def sizeHint(self) -> QSize:
        return QSize(980, 600)


class BrowserWidget(QMainWindow):
    """Main browser window with address bar and render area."""

    def __init__(self, title: str = 'rENDER Browser'):
        super().__init__()
        self.setWindowTitle(title)
        self.setMinimumSize(1024, 768)

        # Central widget
        central = QWidget()
        self.setCentralWidget(central)
        main_layout = QVBoxLayout(central)
        main_layout.setContentsMargins(0, 0, 0, 0)
        main_layout.setSpacing(0)

        # Toolbar
        toolbar = QWidget()
        toolbar.setFixedHeight(40)
        toolbar.setStyleSheet('background: #f0f0f0; border-bottom: 1px solid #ccc;')
        toolbar_layout = QHBoxLayout(toolbar)
        toolbar_layout.setContentsMargins(6, 4, 6, 4)
        toolbar_layout.setSpacing(4)

        self.back_btn = QPushButton('←')
        self.back_btn.setFixedSize(30, 28)
        self.forward_btn = QPushButton('→')
        self.forward_btn.setFixedSize(30, 28)
        self.reload_btn = QPushButton('↻')
        self.reload_btn.setFixedSize(30, 28)

        self.address_bar = QLineEdit()
        self.address_bar.setPlaceholderText('Enter URL or file path...')
        self.address_bar.returnPressed.connect(self._on_navigate)

        self.go_btn = QPushButton('Go')
        self.go_btn.setFixedWidth(40)
        self.go_btn.clicked.connect(self._on_navigate)

        toolbar_layout.addWidget(self.back_btn)
        toolbar_layout.addWidget(self.forward_btn)
        toolbar_layout.addWidget(self.reload_btn)
        toolbar_layout.addWidget(self.address_bar)
        toolbar_layout.addWidget(self.go_btn)

        # Scroll area + canvas
        self.scroll_area = QScrollArea()
        self.scroll_area.setWidgetResizable(True)
        self.scroll_area.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAsNeeded)
        self.scroll_area.setVerticalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAsNeeded)

        self.canvas = RenderCanvas()
        self.canvas.link_clicked.connect(self._on_link_click)
        self.scroll_area.setWidget(self.canvas)

        main_layout.addWidget(toolbar)
        main_layout.addWidget(self.scroll_area)

        # Navigation callback (set by engine)
        self.navigate_callback = None

    def set_display_list(self, dl: DisplayList, page_height: int = 600, title: str = '') -> None:
        self.canvas.set_display_list(dl)
        self.canvas.setFixedHeight(max(600, page_height))
        if title:
            self.setWindowTitle(f'{title} — rENDER')

    def set_url(self, url: str) -> None:
        self.address_bar.setText(url)

    def set_status(self, msg: str) -> None:
        """Show loading status in the title bar."""
        if msg:
            self.setWindowTitle(f'{msg} — rENDER')
        # Don't clear the title here; set_display_list will update it

    def _on_navigate(self) -> None:
        url = self.address_bar.text().strip()
        if self.navigate_callback and url:
            self.navigate_callback(url)

    def _on_link_click(self, href: str) -> None:
        if self.navigate_callback:
            self.navigate_callback(href)


def paint(display_list: DisplayList, painter: QPainter) -> None:
    """Execute display list draw commands using QPainter."""
    clip_stack = []
    opacity_stack = []

    for cmd in display_list:
        if isinstance(cmd, PushOpacity):
            opacity_stack.append(painter.opacity())
            painter.setOpacity(painter.opacity() * max(0.0, min(1.0, cmd.opacity)))
            continue

        if isinstance(cmd, PopOpacity):
            if opacity_stack:
                painter.setOpacity(opacity_stack.pop())
            continue

        if isinstance(cmd, DrawRect):
            color = _parse_color(cmd.color)
            if color.alpha() == 0:
                continue
            painter.setBrush(QBrush(color))
            painter.setPen(Qt.PenStyle.NoPen)
            r = cmd.rect
            if cmd.border_radius > 0:
                painter.drawRoundedRect(
                    int(r.x), int(r.y), int(r.width), int(r.height),
                    cmd.border_radius, cmd.border_radius
                )
            else:
                painter.drawRect(int(r.x), int(r.y), int(r.width), int(r.height))

        elif isinstance(cmd, DrawText):
            color = _parse_color(cmd.color)
            font = _make_qfont(cmd.font)
            painter.setFont(font)
            painter.setPen(color)
            fm = QFontMetrics(font)
            text_y = int(cmd.y + fm.ascent())
            painter.drawText(int(cmd.x), text_y, cmd.text)
            # Text decoration
            dec = getattr(cmd, 'decoration', 'none')
            if dec and dec not in ('none', ''):
                tw = fm.horizontalAdvance(cmd.text)
                if 'underline' in dec:
                    ul_y = text_y + 2
                    painter.drawLine(int(cmd.x), ul_y, int(cmd.x) + tw, ul_y)
                if 'line-through' in dec:
                    lt_y = int(cmd.y + fm.ascent() - fm.ascent() // 3)
                    painter.drawLine(int(cmd.x), lt_y, int(cmd.x) + tw, lt_y)

        elif isinstance(cmd, DrawBorder):
            r = cmd.rect
            sides = [
                ('top',    r.x,             r.y,              r.x + r.width,   r.y),
                ('right',  r.x + r.width,   r.y,              r.x + r.width,   r.y + r.height),
                ('bottom', r.x,             r.y + r.height,   r.x + r.width,   r.y + r.height),
                ('left',   r.x,             r.y,              r.x,             r.y + r.height),
            ]
            widths = cmd.widths
            colors = cmd.colors
            styles_list = cmd.styles
            for i, (side, x1, y1, x2, y2) in enumerate(sides):
                w_val = getattr(widths, side, 1) if hasattr(widths, side) else 1
                c_val = colors[i] if i < len(colors) else 'black'
                s_val = styles_list[i] if i < len(styles_list) else 'solid'
                if s_val == 'none' or w_val == 0:
                    continue
                pen = QPen(_parse_color(c_val))
                pen.setWidth(max(1, int(w_val)))
                if s_val == 'dashed':
                    pen.setStyle(Qt.PenStyle.DashLine)
                elif s_val == 'dotted':
                    pen.setStyle(Qt.PenStyle.DotLine)
                else:
                    pen.setStyle(Qt.PenStyle.SolidLine)
                painter.setPen(pen)
                painter.drawLine(int(x1), int(y1), int(x2), int(y2))

        elif isinstance(cmd, DrawImage):
            img_data = cmd.image_data
            if img_data is not None:
                try:
                    from PyQt6.QtGui import QImage
                    if isinstance(img_data, QImage):
                        pixmap = QPixmap.fromImage(img_data)
                    elif isinstance(img_data, QPixmap):
                        pixmap = img_data
                    elif hasattr(img_data, 'tobytes'):
                        # PIL Image fallback
                        img_data = img_data.convert('RGBA')
                        iw, ih = img_data.size
                        data = img_data.tobytes('raw', 'RGBA')
                        qimg = QImage(data, iw, ih, QImage.Format.Format_RGBA8888)
                        pixmap = QPixmap.fromImage(qimg)
                    else:
                        pixmap = None
                    if pixmap and not pixmap.isNull():
                        dw = int(cmd.width) if cmd.width > 0 else pixmap.width()
                        dh = int(cmd.height) if cmd.height > 0 else pixmap.height()
                        if dw != pixmap.width() or dh != pixmap.height():
                            pixmap = pixmap.scaled(
                                dw, dh,
                                Qt.AspectRatioMode.IgnoreAspectRatio,
                                Qt.TransformationMode.SmoothTransformation,
                            )
                        painter.drawPixmap(int(cmd.x), int(cmd.y), pixmap)
                except Exception:
                    pass

        elif isinstance(cmd, PushClip):
            r = cmd.rect
            painter.save()
            painter.setClipRect(int(r.x), int(r.y), int(r.width), int(r.height))
            clip_stack.append(True)

        elif isinstance(cmd, PopClip):
            if clip_stack:
                clip_stack.pop()
                painter.restore()

        elif isinstance(cmd, PushTransform):
            painter.save()
            painter.translate(cmd.dx, cmd.dy)

        elif isinstance(cmd, PopTransform):
            painter.restore()

        elif isinstance(cmd, DrawLinearGradient):
            r = cmd.rect
            angle_rad = math.radians(cmd.angle)
            cx = r.x + r.width / 2
            cy = r.y + r.height / 2
            dx = math.sin(angle_rad)
            dy = -math.cos(angle_rad)
            # Scale direction vector to reach the edge of the rect
            sx = (r.width / 2 / abs(dx)) if abs(dx) > 1e-9 else float('inf')
            sy = (r.height / 2 / abs(dy)) if abs(dy) > 1e-9 else float('inf')
            scale = min(sx, sy)
            x1, y1 = cx - dx * scale, cy - dy * scale
            x2, y2 = cx + dx * scale, cy + dy * scale
            gradient = QLinearGradient(x1, y1, x2, y2)
            for pos, color_str in cmd.color_stops:
                gradient.setColorAt(max(0.0, min(1.0, pos)), _parse_color(color_str))
            painter.setBrush(QBrush(gradient))
            painter.setPen(Qt.PenStyle.NoPen)
            painter.drawRect(int(r.x), int(r.y), int(r.width), int(r.height))


def run_browser(display_list: DisplayList, page_height: int = 600,
                title: str = 'rENDER', navigate_callback=None) -> None:
    """Create and show the browser window. Blocks until window is closed."""
    app = QApplication.instance() or QApplication(sys.argv)
    window = BrowserWidget(title)
    window.set_display_list(display_list, page_height=page_height, title=title)
    if navigate_callback:
        window.navigate_callback = navigate_callback
    window.show()
    sys.exit(app.exec())
