"""Abstract base classes for platform backends."""
from __future__ import annotations

from abc import ABC, abstractmethod
from typing import Any


class FontMetrics(ABC):
    """Platform-agnostic font and text measurement interface.

    Implementations must be thread-safe; layout runs in a worker thread.
    """

    @abstractmethod
    def measure(self, text: str, family: str, size_px: float,
                weight: str = 'normal', italic: bool = False) -> tuple[float, float]:
        """Return *(width, height)* in pixels for *text* rendered in the given font."""

    @abstractmethod
    def resolve_family(self, family: str) -> str:
        """Resolve a CSS font-family value (possibly comma-separated) to an
        installed font name, falling back through generic keyword candidates."""

    @abstractmethod
    def get_font(self, family: str, size_px: float,
                 weight: str = 'normal', italic: bool = False) -> Any:
        """Return a platform-specific font object for the given parameters.

        The return type is opaque to callers outside the backend package.
        """


class ImageLoader(ABC):
    """Platform-agnostic image decoding interface."""

    @abstractmethod
    def attach_images(self, img_data: list) -> None:
        """Decode raw bytes and attach image objects to DOM nodes.

        *img_data* is a list of ``(node, raw_bytes)`` pairs.  On success,
        each node receives ``node.image``, ``node.natural_width``, and
        ``node.natural_height``.  Failures are silently skipped.
        """
