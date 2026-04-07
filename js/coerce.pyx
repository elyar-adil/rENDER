"""JavaScript type coercion and operator helpers for rENDER browser engine."""
from __future__ import annotations

import re as _re
from js.types import _UNDEF, JSFunction, JSArray


# ---------------------------------------------------------------------------
# Primitive coercion
# ---------------------------------------------------------------------------

def _to_str(v) -> str:
    if v is _UNDEF:
        return 'undefined'
    if v is None:
        return 'null'
    if v is True:
        return 'true'
    if v is False:
        return 'false'
    if isinstance(v, float):
        if v != v:
            return 'NaN'
        if v == float('inf'):
            return 'Infinity'
        if v == float('-inf'):
            return '-Infinity'
        if v == int(v):
            return str(int(v))
    if isinstance(v, (list, JSArray)):
        return ','.join(_to_str(x) for x in v)
    if isinstance(v, dict):
        return '[object Object]'
    if isinstance(v, JSFunction):
        return f'function {v.name}() {{ [native code] }}'
    return str(v)


def _to_num(v):
    if v is _UNDEF:
        return float('nan')
    if v is None:
        return 0
    if v is True:
        return 1
    if v is False:
        return 0
    if isinstance(v, (int, float)):
        return v
    if isinstance(v, str):
        s = v.strip()
        if not s:
            return 0
        try:
            if s.startswith('0x') or s.startswith('0X'):
                return int(s, 16)
            return float(s) if '.' in s or 'e' in s.lower() else int(s)
        except ValueError:
            return float('nan')
    return float('nan')


def _to_bool(v) -> bool:
    if v is _UNDEF or v is None:
        return False
    if isinstance(v, bool):
        return v
    if isinstance(v, (int, float)):
        return v != 0 and v == v   # NaN is falsy
    if isinstance(v, str):
        return len(v) > 0
    return True   # objects/arrays/functions are truthy


def _typeof(v) -> str:
    if v is _UNDEF:
        return 'undefined'
    if v is None:
        return 'object'
    if isinstance(v, bool):
        return 'boolean'
    if isinstance(v, (int, float)):
        return 'number'
    if isinstance(v, str):
        return 'string'
    if isinstance(v, JSFunction) or callable(v):
        return 'function'
    return 'object'


# ---------------------------------------------------------------------------
# Equality
# ---------------------------------------------------------------------------

def _loose_eq(a, b) -> bool:
    if type(a) is type(b):
        return _strict_eq(a, b)
    if (a is None and b is _UNDEF) or (a is _UNDEF and b is None):
        return True
    if isinstance(a, (int, float)) and isinstance(b, str):
        return a == _to_num(b)
    if isinstance(a, str) and isinstance(b, (int, float)):
        return _to_num(a) == b
    if isinstance(a, bool):
        return _loose_eq(_to_num(a), b)
    if isinstance(b, bool):
        return _loose_eq(a, _to_num(b))
    return a is b


def _strict_eq(a, b) -> bool:
    if a is _UNDEF and b is _UNDEF:
        return True
    if a is None and b is None:
        return True
    if isinstance(a, float) and a != a:
        return False   # NaN !== NaN
    return a is b if isinstance(a, (dict, list)) else a == b


# ---------------------------------------------------------------------------
# Binary operator
# ---------------------------------------------------------------------------

def _binop(op, left, right):
    if op == '+':
        if isinstance(left, str) or isinstance(right, str):
            return _to_str(left) + _to_str(right)
        return _to_num(left) + _to_num(right)
    if op == '-':
        return _to_num(left) - _to_num(right)
    if op == '*':
        return _to_num(left) * _to_num(right)
    if op == '/':
        r = _to_num(right)
        return float('nan') if r == 0 else _to_num(left) / r
    if op == '%':
        r = _to_num(right)
        return float('nan') if r == 0 else _to_num(left) % r
    if op == '**':
        return _to_num(left) ** _to_num(right)
    if op == '==':
        return _loose_eq(left, right)
    if op == '!=':
        return not _loose_eq(left, right)
    if op == '===':
        return _strict_eq(left, right)
    if op == '!==':
        return not _strict_eq(left, right)
    _both_str = isinstance(left, str) and isinstance(right, str)
    if op == '<':
        return left < right if _both_str else _to_num(left) < _to_num(right)
    if op == '>':
        return left > right if _both_str else _to_num(left) > _to_num(right)
    if op == '<=':
        return left <= right if _both_str else _to_num(left) <= _to_num(right)
    if op == '>=':
        return left >= right if _both_str else _to_num(left) >= _to_num(right)
    if op == '<<':
        return int(_to_num(left)) << (int(_to_num(right)) & 31)
    if op == '>>':
        return int(_to_num(left)) >> (int(_to_num(right)) & 31)
    if op == '>>>':
        return (int(_to_num(left)) & 0xFFFFFFFF) >> (int(_to_num(right)) & 31)
    if op == '|':
        return int(_to_num(left)) | int(_to_num(right))
    if op == '&':
        return int(_to_num(left)) & int(_to_num(right))
    if op == '^':
        return int(_to_num(left)) ^ int(_to_num(right))
    if op == 'instanceof':
        # Walk ._proto chain; updated by builtins when callee.prototype is known
        return False
    if op == 'in':
        return _to_str(left) in right if isinstance(right, dict) else False
    return _UNDEF


# ---------------------------------------------------------------------------
# Parsing helpers
# ---------------------------------------------------------------------------

def _parse_int(s, radix=10):
    try:
        s = str(s).strip()
        if not s:
            return float('nan')
        if s.startswith('0x') or s.startswith('0X'):
            return int(s, 16)
        return int(s, int(radix) if radix else 10)
    except (ValueError, TypeError):
        m = _re.match(r'[+-]?\d+', str(s).strip())
        return int(m.group(0)) if m else float('nan')


def _parse_float(s):
    try:
        return float(str(s).strip())
    except (ValueError, TypeError):
        m = _re.match(r'[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?', str(s).strip())
        return float(m.group(0)) if m else float('nan')


# ---------------------------------------------------------------------------
# Miscellaneous
# ---------------------------------------------------------------------------

def _num_to_string(n, base=10) -> str:
    if base == 10:
        return _to_str(n)
    if base == 16:
        return hex(int(n))[2:]
    if base == 2:
        return bin(int(n))[2:]
    if base == 8:
        return oct(int(n))[2:]
    return str(int(n))


def _expand_spread_values(value) -> list:
    if isinstance(value, (list, tuple)):
        return list(value)
    if isinstance(value, str):
        return list(value)
    return []


def _js_to_python(v):
    """Recursively convert JS value to Python-native for JSON.stringify."""
    if v is _UNDEF:
        return None
    if isinstance(v, JSFunction):
        return None
    if isinstance(v, list):
        return [_js_to_python(x) for x in v]
    if isinstance(v, dict):
        return {k: _js_to_python(val) for k, val in v.items()}
    return v
