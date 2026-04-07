"""Backend registry — holds the active FontMetrics and ImageLoader implementations.

Usage::

    import backend
    backend.get_font_metrics().measure(text, family, size_px)
    backend.get_image_loader().attach_images(img_data)

The Qt backend is registered automatically on first use.  To swap in a
different backend (e.g. for testing or a non-Qt GUI toolkit)::

    from backend import set_font_metrics, set_image_loader
    set_font_metrics(MyFontMetrics())
    set_image_loader(MyImageLoader())
"""

from typing import Any

_font_metrics: Any = None
_image_loader: Any = None


def set_font_metrics(impl: Any) -> None:
    """Register *impl* as the active FontMetrics backend."""
    global _font_metrics
    _font_metrics = impl


def set_image_loader(impl: Any) -> None:
    """Register *impl* as the active ImageLoader backend."""
    global _image_loader
    _image_loader = impl


def get_font_metrics() -> Any:
    """Return the active FontMetrics, auto-registering Qt if none is set."""
    if _font_metrics is None:
        _auto_register()
    return _font_metrics


def get_image_loader() -> Any:
    """Return the active ImageLoader, auto-registering Qt if none is set."""
    if _image_loader is None:
        _auto_register()
    return _image_loader


def _auto_register() -> None:
    from backend.qt import register as register_qt

    try:
        register_qt()
    except Exception:
        from backend.fallback import register as register_fallback
        register_fallback()
