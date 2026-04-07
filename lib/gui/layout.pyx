"""Simple layout helpers for the PyX GUI framework.

Layouts arrange child widgets within a container rectangle.  They are
*not* traditional Win32 containers – they just compute positions and
resize children on demand.
"""
from __future__ import annotations

import dataclasses
from typing import Sequence

from . import win32_api as w32
from .widgets import Widget

# ---------------------------------------------------------------------------
# Geometry
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class Rect:
    x: int
    y: int
    width: int
    height: int

    @property
    def right(self) -> int:
        return self.x + self.width

    @property
    def bottom(self) -> int:
        return self.y + self.height


# ---------------------------------------------------------------------------
# Base layout
# ---------------------------------------------------------------------------


class Layout:
    """Abstract layout base class."""

    def __init__(self, padding: int = 8, spacing: int = 6) -> None:
        self.padding: int = padding
        self.spacing: int = spacing
        self._widgets: list[Widget] = []

    def add(self, widget: Widget) -> "Layout":
        self._widgets.append(widget)
        return self

    def apply(self, bounds: Rect) -> None:
        """Position all registered widgets within *bounds*."""
        raise NotImplementedError


# ---------------------------------------------------------------------------
# VBoxLayout – stack widgets vertically
# ---------------------------------------------------------------------------


class VBoxLayout(Layout):
    """Stack widgets top-to-bottom with equal height by default.

    Parameters
    ----------
    padding: Margin around the container edge (pixels).
    spacing: Gap between consecutive widgets (pixels).
    """

    def apply(self, bounds: Rect) -> None:
        if not self._widgets:
            return
        n = len(self._widgets)
        available_h = (
            bounds.height
            - 2 * self.padding
            - (n - 1) * self.spacing
        )
        item_h = max(1, available_h // n)
        x = bounds.x + self.padding
        y = bounds.y + self.padding
        w = bounds.width - 2 * self.padding
        for widget in self._widgets:
            widget.move(x, y, w, item_h)
            y += item_h + self.spacing


# ---------------------------------------------------------------------------
# HBoxLayout – stack widgets horizontally
# ---------------------------------------------------------------------------


class HBoxLayout(Layout):
    """Stack widgets left-to-right with equal width by default."""

    def apply(self, bounds: Rect) -> None:
        if not self._widgets:
            return
        n = len(self._widgets)
        available_w = (
            bounds.width
            - 2 * self.padding
            - (n - 1) * self.spacing
        )
        item_w = max(1, available_w // n)
        x = bounds.x + self.padding
        y = bounds.y + self.padding
        h = bounds.height - 2 * self.padding
        for widget in self._widgets:
            widget.move(x, y, item_w, h)
            x += item_w + self.spacing


# ---------------------------------------------------------------------------
# GridLayout – place widgets in a fixed grid
# ---------------------------------------------------------------------------


class GridLayout(Layout):
    """Arrange widgets in a *cols*-column grid, filling row by row.

    Parameters
    ----------
    cols:    Number of columns.
    row_height: Fixed row height in pixels (0 = auto-distribute).
    """

    def __init__(
        self,
        cols: int = 2,
        padding: int = 8,
        spacing: int = 6,
        row_height: int = 0,
    ) -> None:
        super().__init__(padding=padding, spacing=spacing)
        self.cols: int = max(1, cols)
        self.row_height: int = row_height

    def apply(self, bounds: Rect) -> None:
        if not self._widgets:
            return
        n = len(self._widgets)
        rows = (n + self.cols - 1) // self.cols
        cell_w = (
            bounds.width
            - 2 * self.padding
            - (self.cols - 1) * self.spacing
        ) // self.cols
        if self.row_height:
            cell_h = self.row_height
        else:
            cell_h = (
                bounds.height
                - 2 * self.padding
                - (rows - 1) * self.spacing
            ) // rows
        cell_w = max(1, cell_w)
        cell_h = max(1, cell_h)
        for idx, widget in enumerate(self._widgets):
            row = idx // self.cols
            col = idx % self.cols
            x = bounds.x + self.padding + col * (cell_w + self.spacing)
            y = bounds.y + self.padding + row * (cell_h + self.spacing)
            widget.move(x, y, cell_w, cell_h)
