"""Regression tests for consistent Qt text measurement and painting."""

import pytest

from backend.qt.font import _get_qfont, _measure
from backend.qt.painter import _create_backing_pixmap, _make_qfont, paint
from rendering.display_list import DisplayList, DrawText


@pytest.mark.parametrize(
    ('css_weight', 'qt_weight'),
    [
        ('lighter', 300), ('300', 300), ('normal', 400), ('500', 500),
        ('600', 600), ('bold', 700), ('bolder', 700), ('900', 900),
    ],
)
def test_measurement_and_painting_use_the_same_requested_weight(css_weight, qt_weight):
    measured_font = _get_qfont('Arial, sans-serif', 14, css_weight, False)
    painted_font = _make_qfont(('Arial, sans-serif', 14, css_weight, ''))

    assert int(measured_font.weight()) == qt_weight
    assert int(painted_font.weight()) == qt_weight
    assert measured_font == painted_font


def test_measurement_uses_fractional_qfont_metrics():
    from PyQt6.QtGui import QFontMetricsF

    text = 'Mixed 中文 0123456789'
    font = _get_qfont('Arial, sans-serif', 13, 'normal', False)
    expected = QFontMetricsF(font)

    width, height = _measure(text, 'Arial, sans-serif', 13, 'normal', False)

    assert width == pytest.approx(expected.horizontalAdvance(text))
    assert height == pytest.approx(expected.height())


def test_painter_keeps_fractional_text_origin_and_enables_text_antialiasing():
    from PyQt6.QtCore import QPointF
    from PyQt6.QtGui import QFontMetricsF, QPainter

    class RecordingPainter:
        def __init__(self):
            self.hints = []
            self.text_points = []

        def setRenderHint(self, hint, enabled=True):
            self.hints.append((hint, enabled))

        def setFont(self, _font):
            pass

        def setPen(self, _pen):
            pass

        def drawText(self, point, text):
            self.text_points.append((point, text))

    display_list = DisplayList()
    display_list.add(DrawText(10.75, 20.25, '中文 Mixed', ('Arial', 13), 'black'))
    painter = RecordingPainter()

    paint(display_list, painter)

    assert (QPainter.RenderHint.TextAntialiasing, True) in painter.hints
    point, text = painter.text_points[0]
    assert isinstance(point, QPointF)
    assert point.x() == pytest.approx(10.75)
    expected_ascent = QFontMetricsF(_make_qfont(('Arial', 13))).ascent()
    assert point.y() == pytest.approx(20.25 + expected_ascent)
    assert text == '中文 Mixed'


def test_backing_pixmap_uses_physical_pixels_at_high_dpi():
    pixmap = _create_backing_pixmap(101, 51, 1.5)

    assert pixmap.width() == 152
    assert pixmap.height() == 77
    assert pixmap.devicePixelRatioF() == pytest.approx(1.5)
    assert pixmap.deviceIndependentSize().width() == pytest.approx(152 / 1.5)
    assert pixmap.deviceIndependentSize().height() == pytest.approx(77 / 1.5)
