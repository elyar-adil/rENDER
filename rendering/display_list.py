"""Display list: draw commands emitted by the layout engine."""
from dataclasses import dataclass, field
from typing import Any


@dataclass
class DrawRect:
    """Draw a filled rectangle."""
    rect: Any          # layout.box.Rect
    color: str
    border_radius: float = 0.0


@dataclass
class DrawBorder:
    """Draw element borders."""
    rect: Any          # layout.box.Rect (border box)
    widths: Any        # layout.box.EdgeSizes
    colors: tuple      # (top, right, bottom, left) color strings
    styles: tuple      # (top, right, bottom, left) border style strings


@dataclass
class DrawText:
    """Draw text at (x, y)."""
    x: float
    y: float
    text: str
    font: tuple        # (family, size_px)  — or (family, size_px, 'bold') etc.
    color: str
    decoration: str = 'none'   # 'underline', 'line-through', 'none'
    weight: str = 'normal'
    italic: bool = False
    advance_width: float = 0.0
    text_shadow: str = ''      # CSS text-shadow value


@dataclass
class PushOpacity:
    """Push a painter opacity level (0.0–1.0)."""
    opacity: float


@dataclass
class PopOpacity:
    """Restore previous painter opacity."""
    pass


@dataclass
class DrawImage:
    """Draw an image."""
    x: float
    y: float
    image_data: Any    # QImage, QPixmap, or PIL Image
    width: float = 0.0
    height: float = 0.0


@dataclass
class DrawInput:
    """Draw a simple text input control."""
    x: float
    y: float
    width: float
    height: float
    text: str
    font: tuple
    color: str
    background_color: str = 'white'
    border_color: str = '#767676'
    border_width: float = 1.0


@dataclass
class PushClip:
    """Push a clipping rectangle."""
    rect: Any


@dataclass
class PopClip:
    """Pop the current clipping rectangle."""
    pass


@dataclass
class PushTransform:
    """Push a transform (translation, rotation, scale)."""
    dx: float = 0.0
    dy: float = 0.0
    rotate_deg: float = 0.0
    scale_x: float = 1.0
    scale_y: float = 1.0
    origin_x: float = 0.0
    origin_y: float = 0.0


@dataclass
class PopTransform:
    """Pop the current transform."""
    pass


@dataclass
class DrawBoxShadow:
    """Draw a box shadow with blur."""
    rect: Any           # layout.box.Rect (border box of element)
    offset_x: float
    offset_y: float
    blur_radius: float
    spread: float
    color: str
    border_radius: float = 0.0


@dataclass
class DrawLinearGradient:
    """Draw a linear gradient background fill."""
    rect: Any           # layout.box.Rect
    angle: float        # degrees — 0=top→bottom, 90=left→right, 180=bottom→top
    color_stops: list   # [(position: float 0..1, color_str), ...]


@dataclass
class DrawRadialGradient:
    """Draw a radial gradient background fill."""
    rect: Any           # layout.box.Rect
    cx: float           # center x (absolute)
    cy: float           # center y (absolute)
    radius_x: float     # horizontal radius
    radius_y: float     # vertical radius
    color_stops: list   # [(position: float 0..1, color_str), ...]


@dataclass
class DrawOutline:
    """Draw an outline (outside the border box)."""
    rect: Any           # layout.box.Rect (border box)
    width: float
    style: str          # solid, dashed, dotted, etc.
    color: str
    offset: float = 0.0


class DisplayList:
    """Ordered list of draw commands produced by the layout engine."""

    def __init__(self):
        self.commands: list = []

    def add(self, command) -> None:
        self.commands.append(command)

    def __iter__(self):
        return iter(self.commands)

    def __len__(self):
        return len(self.commands)

    def __repr__(self):
        return f'DisplayList({len(self.commands)} commands)'
