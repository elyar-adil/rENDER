"""Render invalidation tracking for DOM/style/layout/paint changes."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass(frozen=True)
class InvalidationRecord:
    """One dirtying event captured before the next render opportunity."""

    phase: str
    node: Any = None
    reason: str = ''


@dataclass(frozen=True)
class InvalidationSnapshot:
    """Immutable snapshot consumed at a render opportunity."""

    style_dirty: bool = False
    layout_dirty: bool = False
    paint_dirty: bool = False
    records: tuple[InvalidationRecord, ...] = field(default_factory=tuple)

    @property
    def dirty_phases(self) -> tuple[str, ...]:
        phases: list[str] = []
        if self.style_dirty:
            phases.append('style')
        if self.layout_dirty:
            phases.append('layout')
        if self.paint_dirty:
            phases.append('paint')
        return tuple(phases)

    def has_pending(self) -> bool:
        return self.style_dirty or self.layout_dirty or self.paint_dirty


class InvalidationGraph:
    """Tracks pending style/layout/paint work until the next render opportunity."""

    def __init__(self) -> None:
        self.clear()

    def clear(self) -> None:
        self._style_dirty = False
        self._layout_dirty = False
        self._paint_dirty = False
        self._records: list[InvalidationRecord] = []

    def mark_style(self, node: Any = None, reason: str = '') -> None:
        self._style_dirty = True
        self._layout_dirty = True
        self._paint_dirty = True
        self._records.append(InvalidationRecord('style', node=node, reason=reason))

    def mark_layout(self, node: Any = None, reason: str = '') -> None:
        self._layout_dirty = True
        self._paint_dirty = True
        self._records.append(InvalidationRecord('layout', node=node, reason=reason))

    def mark_paint(self, node: Any = None, reason: str = '') -> None:
        self._paint_dirty = True
        self._records.append(InvalidationRecord('paint', node=node, reason=reason))

    def snapshot(self) -> InvalidationSnapshot:
        return InvalidationSnapshot(
            style_dirty=self._style_dirty,
            layout_dirty=self._layout_dirty,
            paint_dirty=self._paint_dirty,
            records=tuple(self._records),
        )

    def consume(self) -> InvalidationSnapshot:
        snapshot = self.snapshot()
        self.clear()
        return snapshot
