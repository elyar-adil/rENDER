"""CSS length expression resolver.

Public API:
    resolve_length_expr(value, *, percentage_base=None, em_base=16,
                        rem_base=16, vw=0, vh=0) -> float | None

Supports:
  - Absolute units: px, pt, pc, in, cm, mm
  - Relative units: em, rem, vw, vh, vmin, vmax, %
  - calc() with +, -, *, /  (nested parens supported)
  - var(--name, fallback) inside calc — fallback resolved, var itself → 1
"""

import re

# ---------------------------------------------------------------------------
# Public
# ---------------------------------------------------------------------------

def resolve_length_expr(
    value: str,
    *,
    percentage_base: float | None = None,
    em_base: float = 16.0,
    rem_base: float = 16.0,
    vw: float = 0.0,
    vh: float = 0.0,
) -> float | None:
    """Resolve a CSS length value (possibly containing calc()) to px.

    Returns None if the value cannot be resolved (e.g. unknown keyword).
    """
    if not value:
        return None
    value = value.strip()
    ctx = _Context(
        percentage_base=percentage_base,
        em_base=em_base,
        rem_base=rem_base,
        vw=vw,
        vh=vh,
    )
    return _resolve(value, ctx)


# ---------------------------------------------------------------------------
# Internal
# ---------------------------------------------------------------------------

class _Context:
    percentage_base: float | None
    em_base: float
    rem_base: float
    vw: float
    vh: float

    def __init__(self, *, percentage_base: float | None, em_base: float,
                 rem_base: float, vw: float, vh: float) -> None:
        self.percentage_base = percentage_base
        self.em_base = float(em_base)
        self.rem_base = float(rem_base)
        self.vw = float(vw)
        self.vh = float(vh)


_UNIT_RE = re.compile(
    r'^([+-]?[\d]*\.?[\d]+(?:[eE][+-]?\d+)?)'
    r'\s*(px|em|rem|vw|vh|dvh|svh|lvh|dvw|svw|lvw|vmin|vmax|%|pt|pc|cm|mm|in|ex|ch|q)?$',
    re.IGNORECASE,
)

_VAR_RE = re.compile(r'\bvar\(\s*(--[\w-]+)\s*(?:,\s*([^)]*))?\s*\)', re.IGNORECASE)


def _strip_var(expr: str) -> str:
    """Replace var() with its fallback (or '1' if none)."""
    def _replacer(m: re.Match) -> str:
        fallback = (m.group(2) or '1').strip()
        return fallback if fallback else '1'
    result = expr
    for _ in range(20):  # guard against infinite loops
        new = _VAR_RE.sub(_replacer, result)
        if new == result:
            break
        result = new
    return result


def _resolve(value: str, ctx: _Context) -> float | None:
    value = value.strip()
    vl = value.lower()

    if vl.startswith('calc(') and vl.endswith(')'):
        inner = value[5:-1]
        return _eval_calc(inner, ctx)

    m = _UNIT_RE.match(value)
    if m:
        num = float(m.group(1))
        unit = (m.group(2) or 'px').lower()
        return _apply_unit(num, unit, ctx)

    return None


def _apply_unit(num: float, unit: str, ctx: _Context) -> float | None:
    if unit in ('px', ''):
        return num
    if unit == 'em':
        return num * ctx.em_base
    if unit == 'rem':
        return num * ctx.rem_base
    if unit == 'vw':
        return num * ctx.vw / 100.0
    if unit in ('vh', 'dvh', 'svh', 'lvh'):
        return num * ctx.vh / 100.0
    if unit in ('dvw', 'svw', 'lvw'):
        return num * ctx.vw / 100.0
    if unit == 'vmin':
        return num * min(ctx.vw, ctx.vh) / 100.0
    if unit == 'vmax':
        return num * max(ctx.vw, ctx.vh) / 100.0
    if unit == '%':
        if ctx.percentage_base is not None:
            return num * ctx.percentage_base / 100.0
        return None
    if unit == 'pt':
        return num * 96.0 / 72.0
    if unit == 'pc':
        return num * 96.0 / 6.0
    if unit == 'in':
        return num * 96.0
    if unit == 'cm':
        return num * 96.0 / 2.54
    if unit == 'mm':
        return num * 96.0 / 25.4
    if unit == 'q':
        return num * 96.0 / 101.6
    if unit in ('ex', 'ch'):
        return num * ctx.em_base * 0.5
    return None


# ---------------------------------------------------------------------------
# calc() evaluator — converts to a flat token stream then evaluates
# ---------------------------------------------------------------------------

def _eval_calc(expr: str, ctx: _Context) -> float | None:
    """Evaluate a CSS calc() inner expression."""
    expr = _strip_var(expr)
    try:
        tokens = _tokenize_calc(expr)
        result, _ = _parse_add_sub(tokens, 0, ctx)
        return result
    except Exception:
        return None


def _tokenize_calc(expr: str) -> list:
    """Split calc() expression into tokens: numbers-with-units, operators, parens."""
    tokens = []
    i = 0
    n = len(expr)
    while i < n:
        ch = expr[i]
        if ch in ' \t\n\r':
            i += 1
            continue
        if ch == '(':
            tokens.append('(')
            i += 1
            continue
        if ch == ')':
            tokens.append(')')
            i += 1
            continue
        if ch == '*':
            tokens.append('*')
            i += 1
            continue
        if ch == '/':
            tokens.append('/')
            i += 1
            continue
        # + and - are operators only when preceded by a value/paren token
        # (to distinguish from unary sign on numbers)
        if ch in '+-':
            prev = tokens[-1] if tokens else None
            is_binary = prev is not None and prev not in ('(', '+', '-', '*', '/')
            if is_binary:
                tokens.append(ch)
                i += 1
                continue
        # Number (possibly with leading sign)
        m = re.match(
            r'([+-]?[\d]*\.?[\d]+(?:[eE][+-]?\d+)?)'
            r'\s*(px|em|rem|vw|vh|dvh|svh|lvh|dvw|svw|lvw|vmin|vmax|%|pt|pc|cm|mm|in|ex|ch|q)?',
            expr[i:],
            re.IGNORECASE,
        )
        if m:
            num = float(m.group(1))
            unit = (m.group(2) or 'px').lower()
            tokens.append(('num', num, unit))
            i += m.end()
            continue
        # Skip unknown characters
        i += 1
    return tokens


def _parse_add_sub(tokens: list, pos: int, ctx: _Context) -> tuple:
    val, pos = _parse_mul_div(tokens, pos, ctx)
    while pos < len(tokens) and tokens[pos] in ('+', '-'):
        op = tokens[pos]
        pos += 1
        rhs, pos = _parse_mul_div(tokens, pos, ctx)
        if val is None or rhs is None:
            return None, pos
        val = val + rhs if op == '+' else val - rhs
    return val, pos


def _parse_mul_div(tokens: list, pos: int, ctx: _Context) -> tuple:
    val, pos = _parse_atom(tokens, pos, ctx)
    while pos < len(tokens) and tokens[pos] in ('*', '/'):
        op = tokens[pos]
        pos += 1
        rhs, pos = _parse_atom(tokens, pos, ctx)
        if val is None or rhs is None:
            return None, pos
        if op == '*':
            val = val * rhs
        else:
            val = val / rhs if rhs != 0 else None
    return val, pos


def _parse_atom(tokens: list, pos: int, ctx: _Context) -> tuple:
    if pos >= len(tokens):
        return None, pos
    tok = tokens[pos]
    if tok == '(':
        val, pos = _parse_add_sub(tokens, pos + 1, ctx)
        if pos < len(tokens) and tokens[pos] == ')':
            pos += 1
        return val, pos
    if isinstance(tok, tuple) and tok[0] == 'num':
        _, num, unit = tok
        return _apply_unit(num, unit, ctx), pos + 1
    return None, pos + 1
