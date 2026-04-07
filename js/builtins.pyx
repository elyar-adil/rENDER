"""JavaScript built-in property access and method dispatch for rENDER browser engine."""

import logging
import time as _time
_logger = logging.getLogger(__name__)

from js.types import _UNDEF, JSObject, JSArray, JSFunction, Environment
from js.coerce import (
    _to_str, _to_num, _to_bool, _strict_eq,
    _expand_spread_values, _num_to_string,
)


# ---------------------------------------------------------------------------
# Object helpers
# ---------------------------------------------------------------------------

def _iter_enumerable_entries(obj):
    """Yield (key, value) pairs from any JS value."""
    if obj is _UNDEF or obj is None:
        return []
    if isinstance(obj, dict):
        return list(obj.items())
    if isinstance(obj, list):
        return [(str(i), v) for i, v in enumerate(obj)]
    if isinstance(obj, JSFunction):
        return list(obj.properties.items())
    if callable(obj) and hasattr(obj, '__dict__'):
        return [(k, v) for k, v in obj.__dict__.items() if not k.startswith('__')]
    return []


def _object_assign(target, *sources):
    if target is _UNDEF or target is None:
        target = JSObject()
    for src in sources:
        for k, v in _iter_enumerable_entries(src):
            _set_property(target, k, v)
    return target


def _object_create(proto):
    obj = JSObject()
    if isinstance(proto, dict):
        obj._proto = proto
    return obj


# ---------------------------------------------------------------------------
# Array helpers
# ---------------------------------------------------------------------------

def _list_index_of(arr, val, start=0):
    for i in range(max(0, start), len(arr)):
        if _strict_eq(arr[i], val):
            return i
    return -1


def _list_last_index_of(arr, val):
    for i in range(len(arr) - 1, -1, -1):
        if _strict_eq(arr[i], val):
            return i
    return -1


def _array_splice(arr, start=0, delete_count=None, *items):
    s = int(start)
    if s < 0:
        s = max(0, len(arr) + s)
    dc = int(delete_count) if delete_count is not None else len(arr) - s
    removed = JSArray(arr[s:s + dc])
    arr[s:s + dc] = list(items)
    return removed


def _array_reduce(arr, fn, *init):
    it = iter(enumerate(arr))
    if init:
        acc = init[0]
    else:
        try:
            _, acc = next(it)
        except StopIteration:
            return _UNDEF
    for i, val in it:
        acc = _safe_call(fn, [acc, val, i, arr])
    return acc


def _flatten(arr, depth):
    result = []
    for item in arr:
        if isinstance(item, list) and depth > 0:
            result.extend(_flatten(item, depth - 1))
        else:
            result.append(item)
    return result


def _array_fill(arr, val, start, end):
    e = len(arr) if end is None else int(end)
    for i in range(start, min(e, len(arr))):
        arr[i] = val
    return JSArray(arr)


# ---------------------------------------------------------------------------
# Function invocation helpers
# ---------------------------------------------------------------------------

def _invoke_callable(fn, args, this_val=None):
    """Call *fn* with *args*, binding *this_val* when fn is a JSFunction."""
    if fn is _UNDEF or fn is None:
        return _UNDEF
    if callable(fn) and not isinstance(fn, JSFunction):
        try:
            return fn(*args)
        except Exception as exc:
            _logger.debug('invoke_callable error: %s', exc)
            return _UNDEF
    if isinstance(fn, JSFunction):
        call_env = Environment(fn.env)
        for i, p in enumerate(fn.params):
            call_env.define(p, args[i] if i < len(args) else _UNDEF)
        call_env.define('arguments', JSArray(args))
        if this_val is not None:
            call_env.define('this', this_val)
        from js.interpreter import Interpreter as _Interp
        interp = _Interp.__new__(_Interp)
        interp.global_env = fn.env
        interp._iteration_count = 0
        from js.interpreter import _Return
        try:
            interp._exec_stmt(fn.body, call_env)
        except _Return as r:
            return r.value
        except Exception as exc:
            _logger.debug('_invoke_callable ignored: %s', exc)
        return _UNDEF
    return _UNDEF


def _safe_call(fn, args):
    """Call fn with args, swallowing all errors.  Used by array callbacks."""
    if callable(fn) and not isinstance(fn, JSFunction):
        try:
            return fn(*args)
        except Exception as exc:
            _logger.debug('safe_call error: %s', exc)
            return _UNDEF
    return _invoke_callable(fn, args)


# ---------------------------------------------------------------------------
# Function / callable property access
# ---------------------------------------------------------------------------

def _get_function_property(fn: JSFunction, prop):
    if prop == 'call':
        return lambda this=None, *a: _invoke_callable(fn, list(a), this)
    if prop == 'apply':
        return lambda this=None, a=None: _invoke_callable(
            fn, _expand_spread_values(a), this)
    if prop == 'bind':
        return lambda this=None, *bound: (
            lambda *rest: _invoke_callable(fn, list(bound) + list(rest), this))
    if prop == 'length':
        return len(fn.params)
    if prop == 'name':
        return fn.name
    if prop in fn.properties:
        return fn.properties[prop]
    return _UNDEF


def _get_callable_property(fn, prop):
    """Property access on a plain Python callable (not JSFunction)."""
    if prop == 'call':
        return lambda this=None, *a: _invoke_callable(fn, list(a), this)
    if prop == 'apply':
        return lambda this=None, a=None: _invoke_callable(
            fn, _expand_spread_values(a), this)
    if prop == 'bind':
        return lambda this=None, *bound: (
            lambda *rest: _invoke_callable(fn, list(bound) + list(rest), this))
    if hasattr(fn, '__dict__') and prop in fn.__dict__:
        return fn.__dict__[prop]
    return _UNDEF


# ---------------------------------------------------------------------------
# Property access dispatch
# ---------------------------------------------------------------------------

def _get_property(obj, prop):
    """Get a property from any JS value including built-in methods."""
    if obj is _UNDEF or obj is None:
        return _UNDEF

    if isinstance(obj, JSFunction):
        return _get_function_property(obj, prop)

    if callable(obj):
        v = _get_callable_property(obj, prop)
        if v is not _UNDEF:
            return v

    # ---- String ----
    if isinstance(obj, str):
        if prop == 'length':
            return len(obj)
        if prop == 'charAt':
            return lambda i=0: obj[int(i)] if 0 <= int(i) < len(obj) else ''
        if prop == 'charCodeAt':
            return lambda i=0: ord(obj[int(i)]) if 0 <= int(i) < len(obj) else float('nan')
        if prop == 'codePointAt':
            return lambda i=0: ord(obj[int(i)]) if 0 <= int(i) < len(obj) else _UNDEF
        if prop == 'indexOf':
            return lambda s, start=0: obj.find(_to_str(s), int(start))
        if prop == 'lastIndexOf':
            return lambda s, start=None: (
                obj.rfind(_to_str(s)) if start is None
                else obj.rfind(_to_str(s), 0, int(start) + 1))
        if prop == 'slice':
            def _str_slice(s=0, e=None):
                s = int(s); ln = len(obj)
                s = s if s >= 0 else max(0, ln + s)
                if e is None:
                    return obj[s:]
                e = int(e); e = e if e >= 0 else max(0, ln + e)
                return obj[s:e]
            return _str_slice
        if prop == 'substring':
            return lambda s=0, e=None: obj[int(s):] if e is None else obj[int(s):int(e)]
        if prop == 'substr':
            return lambda s=0, l=None: obj[int(s):] if l is None else obj[int(s):int(s)+int(l)]
        if prop == 'split':
            def _split(sep=_UNDEF, limit=None):
                if sep is _UNDEF:
                    return JSArray([obj])
                parts = obj.split(_to_str(sep))
                if limit is not None:
                    parts = parts[:int(limit)]
                return JSArray(parts)
            return _split
        if prop == 'trim':
            return lambda: obj.strip()
        if prop == 'trimStart':
            return lambda: obj.lstrip()
        if prop == 'trimEnd':
            return lambda: obj.rstrip()
        if prop == 'toLowerCase':
            return lambda: obj.lower()
        if prop == 'toUpperCase':
            return lambda: obj.upper()
        if prop == 'replace':
            import re as _re
            def _replace(pat, rep):
                ps = _to_str(pat)
                rs = _to_str(rep)
                if isinstance(pat, type(_re.compile(''))):
                    return pat.sub(rs, obj, count=1)
                return obj.replace(ps, rs, 1)
            return _replace
        if prop == 'replaceAll':
            return lambda pat, rep: obj.replace(_to_str(pat), _to_str(rep))
        if prop == 'includes':
            return lambda s, pos=0: _to_str(s) in obj[int(pos):]
        if prop == 'startsWith':
            return lambda s, pos=0: obj[int(pos):].startswith(_to_str(s))
        if prop == 'endsWith':
            return lambda s, end=None: (
                obj[:int(end)].endswith(_to_str(s)) if end is not None
                else obj.endswith(_to_str(s)))
        if prop == 'match':
            import re as _re
            def _match(pat):
                if isinstance(pat, type(_re.compile(''))):
                    m = pat.search(obj)
                    return JSArray([m.group(0)] + list(m.groups())) if m else None
                m = _re.search(_to_str(pat), obj)
                return JSArray([m.group(0)] + list(m.groups())) if m else None
            return _match
        if prop == 'search':
            import re as _re
            return lambda pat: (lambda m: m.start() if m else -1)(_re.search(_to_str(pat), obj))
        if prop == 'repeat':
            return lambda n: obj * max(0, int(n))
        if prop == 'padStart':
            return lambda l, c=' ': obj.rjust(int(l), (_to_str(c) or ' ')[0])
        if prop == 'padEnd':
            return lambda l, c=' ': obj.ljust(int(l), (_to_str(c) or ' ')[0])
        if prop == 'at':
            def _str_at(i):
                i = int(i)
                if i < 0:
                    i = len(obj) + i
                return obj[i] if 0 <= i < len(obj) else _UNDEF
            return _str_at
        if prop == 'toString' or prop == 'valueOf':
            return lambda: obj
        try:
            idx = int(prop)
            if 0 <= idx < len(obj):
                return obj[idx]
        except (ValueError, TypeError):
            pass
        return _UNDEF

    # ---- Array ----
    if isinstance(obj, list):
        if prop == 'length':
            return len(obj)
        if prop == 'push':
            return lambda *a: (obj.extend(a), len(obj))[-1]
        if prop == 'pop':
            return lambda: obj.pop() if obj else _UNDEF
        if prop == 'shift':
            return lambda: obj.pop(0) if obj else _UNDEF
        if prop == 'unshift':
            return lambda *a: (obj.__setitem__(slice(0, 0), list(a)), len(obj))[-1]
        if prop == 'join':
            return lambda sep=',': _to_str(sep).join(_to_str(x) for x in obj)
        if prop == 'indexOf':
            return lambda v, start=0: _list_index_of(obj, v, int(start))
        if prop == 'lastIndexOf':
            return lambda v: _list_last_index_of(obj, v)
        if prop == 'includes':
            return lambda v: any(_strict_eq(x, v) for x in obj)
        if prop == 'slice':
            def _arr_slice(s=0, e=None):
                s = int(s)
                return JSArray(obj[s:] if e is None else obj[s:int(e)])
            return _arr_slice
        if prop == 'splice':
            return lambda *a: _array_splice(obj, *a)
        if prop == 'concat':
            def _concat(*others):
                result = JSArray(obj)
                for o in others:
                    if isinstance(o, list):
                        result.extend(o)
                    else:
                        result.append(o)
                return result
            return _concat
        if prop == 'reverse':
            return lambda: (obj.reverse(), JSArray(obj))[-1]
        if prop == 'sort':
            def _sort(cmp=None):
                if cmp is not None and (isinstance(cmp, JSFunction) or callable(cmp)):
                    import functools
                    obj.sort(key=functools.cmp_to_key(
                        lambda a, b: int(_to_num(_safe_call(cmp, [a, b])))))
                else:
                    obj.sort(key=_to_str)
                return JSArray(obj)
            return _sort
        if prop == 'map':
            return lambda fn: JSArray(_safe_call(fn, [x, i, obj]) for i, x in enumerate(obj))
        if prop == 'filter':
            return lambda fn: JSArray(
                x for i, x in enumerate(obj) if _to_bool(_safe_call(fn, [x, i, obj])))
        if prop == 'forEach':
            def _forEach(fn):
                for i, x in enumerate(obj):
                    _safe_call(fn, [x, i, obj])
            return _forEach
        if prop == 'find':
            return lambda fn: next(
                (x for i, x in enumerate(obj) if _to_bool(_safe_call(fn, [x, i, obj]))), _UNDEF)
        if prop == 'findIndex':
            return lambda fn: next(
                (i for i, x in enumerate(obj) if _to_bool(_safe_call(fn, [x, i, obj]))), -1)
        if prop == 'every':
            return lambda fn: all(_to_bool(_safe_call(fn, [x, i, obj])) for i, x in enumerate(obj))
        if prop == 'some':
            return lambda fn: any(_to_bool(_safe_call(fn, [x, i, obj])) for i, x in enumerate(obj))
        if prop == 'reduce':
            return lambda fn, *init: _array_reduce(obj, fn, *init)
        if prop == 'reduceRight':
            def _reduce_right(fn, *init):
                rev = list(reversed(obj))
                return _array_reduce(rev, fn, *init)
            return _reduce_right
        if prop == 'flat':
            return lambda depth=1: JSArray(_flatten(obj, int(depth)))
        if prop == 'flatMap':
            def _flatMap(fn):
                result = JSArray()
                for i, x in enumerate(obj):
                    v = _safe_call(fn, [x, i, obj])
                    if isinstance(v, list):
                        result.extend(v)
                    else:
                        result.append(v)
                return result
            return _flatMap
        if prop == 'fill':
            return lambda val, s=0, e=None: _array_fill(obj, val, int(s), e)
        if prop == 'at':
            def _arr_at(i):
                i = int(i)
                if i < 0:
                    i = len(obj) + i
                return obj[i] if 0 <= i < len(obj) else _UNDEF
            return _arr_at
        if prop == 'keys':
            return lambda: JSArray(range(len(obj)))
        if prop == 'values':
            return lambda: JSArray(obj)
        if prop == 'entries':
            return lambda: JSArray([JSArray([i, v]) for i, v in enumerate(obj)])
        if prop == 'toString':
            return lambda: ','.join(_to_str(x) for x in obj)
        if prop == 'copyWithin':
            return lambda *_: JSArray(obj)   # stub
        try:
            idx = int(prop)
            if 0 <= idx < len(obj):
                return obj[idx]
        except (ValueError, TypeError):
            pass
        return _UNDEF

    # ---- Object / dict ----
    if isinstance(obj, dict):
        try:
            val = obj[prop]
            if val is not _UNDEF:
                return val
        except (KeyError, IndexError):
            pass
        # Walk prototype chain
        proto = getattr(obj, '_proto', None)
        if isinstance(proto, dict) and prop in proto:
            return proto[prop]
        if prop == 'hasOwnProperty':
            return lambda k: k in obj
        if prop == 'toString':
            return lambda: '[object Object]'
        if prop == 'valueOf':
            return lambda: obj
        return _UNDEF

    # ---- Number ----
    if isinstance(obj, (int, float)):
        if prop == 'toString':
            return lambda base=10: _num_to_string(obj, int(base))
        if prop == 'toFixed':
            return lambda d=0: f'{float(obj):.{int(d)}f}'
        if prop == 'toLocaleString':
            return lambda *_: _to_str(obj)
        if prop == 'valueOf':
            return lambda: obj
        return _UNDEF

    return _UNDEF


def _set_property(obj, prop, value):
    if isinstance(obj, dict):
        obj[prop] = value
    elif isinstance(obj, JSFunction):
        obj.properties[prop] = value
    elif callable(obj) and hasattr(obj, '__dict__'):
        setattr(obj, prop, value)
    elif isinstance(obj, list):
        try:
            idx = int(prop)
            while len(obj) <= idx:
                obj.append(_UNDEF)
            obj[idx] = value
            return
        except (ValueError, TypeError):
            pass
        if prop == 'length':
            new_len = int(value)
            while len(obj) > new_len:
                obj.pop()
            while len(obj) < new_len:
                obj.append(_UNDEF)


# ---------------------------------------------------------------------------
# Minimal Date built-in
# ---------------------------------------------------------------------------

class _JSDate:
    """Minimal Date constructor."""

    def __init__(self, *args):
        self._ts = _time.time() * 1000

    def getTime(self):
        return self._ts

    def toString(self):
        return str(self._ts)

    def toISOString(self):
        import datetime
        dt = datetime.datetime.utcfromtimestamp(self._ts / 1000)
        return dt.strftime('%Y-%m-%dT%H:%M:%S.') + f'{int(self._ts % 1000):03d}Z'

    def toLocaleDateString(self):
        return str(self._ts)
