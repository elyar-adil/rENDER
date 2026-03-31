"""Float manager: tracks floated elements within a Block Formatting Context."""
from dataclasses import dataclass, field


@dataclass
class FloatBox:
    x: float
    y: float
    width: float
    height: float
    side: str  # 'left' or 'right'


class FloatManager:
    """Manages float boxes within a BFC."""

    def __init__(self):
        self._floats: list[FloatBox] = []

    def add_float(self, x: float, y: float, width: float, height: float, side: str) -> None:
        self._floats.append(FloatBox(x, y, width, height, side))

    @staticmethod
    def _overlaps(f: FloatBox, y: float, height: float) -> bool:
        """Return True if the float box overlaps the vertical span [y, y+height)."""
        return f.y < y + height and f.y + f.height > y

    def available_rect(self, y: float, height: float, container_x: float, container_width: float) -> tuple[float, float]:
        """Return (left_x, available_width) for a line at position y with given height."""
        left = container_x
        right = container_x + container_width

        for f in self._floats:
            if self._overlaps(f, y, height):
                if f.side == 'left':
                    left = max(left, f.x + f.width)
                elif f.side == 'right':
                    right = min(right, f.x)

        return left, max(0.0, right - left)

    def clear_y(self, side: str = 'both') -> float:
        """Return the Y coordinate after all floats on the given side(s) have ended."""
        y = 0.0
        for f in self._floats:
            if side == 'both' or f.side == side:
                y = max(y, f.y + f.height)
        return y

    def active_floats_at(self, y: float) -> list[FloatBox]:
        return [f for f in self._floats if f.y <= y < f.y + f.height]
