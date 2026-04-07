"""Qt backend — registers Qt implementations as the active backend."""


def register() -> None:
    """Register QtFontMetrics and QtImageLoader as the active backend."""
    import backend
    from backend.qt.font import QtFontMetrics
    from backend.qt.image import QtImageLoader
    if backend._font_metrics is None:
        backend.set_font_metrics(QtFontMetrics())
    if backend._image_loader is None:
        backend.set_image_loader(QtImageLoader())
