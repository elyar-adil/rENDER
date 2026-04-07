"""Qt image loading — ImageLoader implementation backed by PyQt6."""
from __future__ import annotations

import logging
from backend.base import ImageLoader

_logger = logging.getLogger(__name__)


def _is_svg(raw: bytes) -> bool:
    head = raw[:200].lstrip()
    return (
        (head.startswith(b'<?xml') and b'<svg' in raw[:500])
        or head.startswith(b'<svg')
        or (head.startswith(b'<!') and b'<svg' in raw[:500])
    )


def _render_svg(raw: bytes):
    """Render SVG bytes to a QImage.  Returns QImage or None."""
    try:
        from PyQt6.QtSvg import QSvgRenderer
        from PyQt6.QtGui import QImage, QPainter
        from PyQt6.QtCore import QByteArray
        renderer = QSvgRenderer(QByteArray(raw))
        if not renderer.isValid():
            return None
        size = renderer.defaultSize()
        if size.width() <= 0 or size.height() <= 0:
            size = renderer.viewBox().size()
        if size.width() <= 0 or size.height() <= 0:
            return None
        img = QImage(size, QImage.Format.Format_ARGB32)
        img.fill(0)
        painter = QPainter(img)
        renderer.render(painter)
        painter.end()
        return img
    except Exception:
        return None


class QtImageLoader(ImageLoader):
    """ImageLoader implementation backed by PyQt6 QImage."""

    def attach_images(self, img_data: list) -> None:
        """Decode raw bytes and attach QImage to each node as ``node.image``."""
        try:
            from PyQt6.QtGui import QImage
            from PyQt6.QtCore import QByteArray
        except Exception:
            for node, _raw in img_data:
                node.image = None
                node.natural_width = 0
                node.natural_height = 0
            return

        for node, raw in img_data:
            if raw is None:
                continue
            try:
                if _is_svg(raw):
                    qimg = _render_svg(raw)
                    if qimg and not qimg.isNull():
                        node.image = qimg
                        node.natural_width = qimg.width()
                        node.natural_height = qimg.height()
                    continue

                ba = QByteArray(raw)
                qimg = QImage()
                qimg.loadFromData(ba)
                if not qimg.isNull():
                    node.image = qimg
                    node.natural_width = qimg.width()
                    node.natural_height = qimg.height()
            except Exception as exc:
                _logger.debug("Ignored image decode error: %s", exc)
