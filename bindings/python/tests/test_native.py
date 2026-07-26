import pytest

from render_runtime import resolve_length_expr, resolve_length_expr_strict


def test_native_length_resolution():
    assert resolve_length_expr(
        "calc(100% - 40px)", percentage_base=1440
    ) == pytest.approx(1400.0)


def test_compatibility_api_returns_none_for_invalid_css():
    assert resolve_length_expr("calc(1px + 2)") is None


def test_strict_api_preserves_diagnostics():
    with pytest.raises(ValueError, match="incompatible CSS numeric types"):
        resolve_length_expr_strict("calc(1px + 2)")
