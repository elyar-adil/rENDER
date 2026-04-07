"""Dependency-free fallback backend used when Qt backend is unavailable."""

from typing import Any

from backend.base import FontMetrics, ImageLoader


class SimpleFontMetrics(FontMetrics):
    """Approximate text metrics for headless/layout-only environments."""

    def measure(
        self,
        text: str,
        family: str,
        size_px: float,
        weight: str = 'normal',
        italic: bool = False,
    ) -> tuple[float, float]:
        # Reasonable heuristic: CJK characters are wider; latin chars are narrower.
        size = max(1.0, float(size_px or 16.0))
        width = 0.0
        for ch in text:
            codepoint = ord(ch)
            if ch.isspace():
                width += size * 0.33
            elif codepoint >= 0x2E80:
                width += size * 1.0
            else:
                width += size * 0.56
        if weight in {'bold', '700', '800', '900'}:
            width *= 1.05
        return width, size * 1.2

    def resolve_family(self, family: str) -> str:
        if not family:
            return 'sans-serif'
        return family.split(',')[0].strip().strip('"\'') or 'sans-serif'

    def get_font(
        self,
        family: str,
        size_px: float,
        weight: str = 'normal',
        italic: bool = False,
    ) -> Any:
        return None


class SimpleImageLoader(ImageLoader):
    """No-op image loader for environments without Qt image decoders."""

    def attach_images(self, img_data: list) -> None:
        for node, _raw in img_data:
            setattr(node, 'image', None)
            setattr(node, 'natural_width', 0)
            setattr(node, 'natural_height', 0)


def register() -> None:
    import backend

    if backend._font_metrics is None:
        backend.set_font_metrics(SimpleFontMetrics())
    if backend._image_loader is None:
        backend.set_image_loader(SimpleImageLoader())
