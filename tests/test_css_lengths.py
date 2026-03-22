import pytest

from css.lengths import resolve_length_expr


def test_resolve_calc_with_px_arithmetic():
    assert resolve_length_expr('calc(456px*2)') == 912.0


def test_resolve_calc_with_percentage_reference():
    assert resolve_length_expr('calc(100% - 40px)', percentage_base=1440) == 1400.0


def test_resolve_calc_with_var_fallback():
    assert resolve_length_expr('calc(108px * var(--clientfont-scale, 1))') == 108.0


def test_resolve_nested_calc_with_viewport_units():
    assert resolve_length_expr('calc((100vw - 40px) / 2)', vw=1440) == 700.0


def test_resolve_calc_with_rem_and_em_bases():
    assert resolve_length_expr('calc(2rem + 0.5em)', rem_base=16, em_base=20) == 42.0


def test_resolve_physical_units_to_px():
    assert resolve_length_expr('1in') == 96.0
    assert resolve_length_expr('2.54cm') == pytest.approx(96.0, abs=0.01)
    assert resolve_length_expr('25.4mm') == pytest.approx(96.0, abs=0.01)


@pytest.mark.xfail(strict=True, reason='min() / max() / clamp() are not resolved yet')
def test_resolve_min_function_for_common_responsive_css():
    assert resolve_length_expr('min(20px, 5vw)', vw=1000) == 20.0


@pytest.mark.xfail(strict=True, reason='min() / max() / clamp() are not resolved yet')
def test_resolve_max_function_for_common_responsive_css():
    assert resolve_length_expr('max(10px, 2vw)', vw=1000) == 20.0


@pytest.mark.xfail(strict=True, reason='min() / max() / clamp() are not resolved yet')
def test_resolve_clamp_function_for_common_responsive_css():
    assert resolve_length_expr('clamp(10px, 5vw, 80px)', vw=1000) == 50.0
