"""Box model data structures."""
from dataclasses import dataclass, field


@dataclass
class EdgeSizes:
    top: float = 0.0
    right: float = 0.0
    bottom: float = 0.0
    left: float = 0.0

    def __iter__(self):
        return iter((self.top, self.right, self.bottom, self.left))


@dataclass
class Rect:
    x: float = 0.0
    y: float = 0.0
    width: float = 0.0
    height: float = 0.0

    def expanded_by(self, edges: EdgeSizes) -> 'Rect':
        return Rect(
            x=self.x - edges.left,
            y=self.y - edges.top,
            width=self.width + edges.left + edges.right,
            height=self.height + edges.top + edges.bottom,
        )


class BoxModel:
    """Full CSS box model for one layout box."""

    def __init__(self):
        self.x: float = 0.0
        self.y: float = 0.0
        self.content_width: float = 0.0
        self.content_height: float = 0.0
        self.padding: EdgeSizes = EdgeSizes()
        self.border: EdgeSizes = EdgeSizes()
        self.margin: EdgeSizes = EdgeSizes()

    @property
    def content_rect(self) -> Rect:
        return Rect(self.x, self.y, self.content_width, self.content_height)

    @property
    def padding_rect(self) -> Rect:
        return self.content_rect.expanded_by(self.padding)

    @property
    def border_rect(self) -> Rect:
        return self.padding_rect.expanded_by(self.border)

    @property
    def margin_rect(self) -> Rect:
        return self.border_rect.expanded_by(self.margin)

    def __repr__(self):
        return (f'BoxModel(x={self.x:.1f}, y={self.y:.1f}, '
                f'w={self.content_width:.1f}, h={self.content_height:.1f})')
