"""JavaScript tree-walking interpreter for rENDER browser engine."""
import logging
import math
import json
import re

_logger = logging.getLogger(__name__)

# --- sub-module imports -------------------------------------------------------
from js.types import _UNDEF, JSObject, JSArray, JSFunction, Environment
from js.ast import ASTNode
from js.coerce import (
    _to_str, _to_num, _to_bool, _typeof,
    _binop, _loose_eq, _strict_eq,
    _parse_int, _parse_float,
    _expand_spread_values, _js_to_python,
)
from js.builtins import (
    _get_property, _set_property,
    _get_function_property, _get_callable_property,
    _safe_call, _invoke_callable,
    _object_assign, _object_create, _iter_enumerable_entries,
    _JSDate,
)
from js.event_loop import get_event_loop
from js.promise import JSPromise, _PromiseCtor, drain_microtasks, _enqueue_microtask

# Re-export everything that external modules import from js.interpreter
__all__ = [
    'Interpreter', 'Environment',
    'JSObject', 'JSArray', 'JSFunction',
    '_UNDEF', '_to_str', '_to_num', '_to_bool',
    '_get_property', '_set_property',
    '_object_assign', '_iter_enumerable_entries',
    '_safe_call', '_invoke_callable',
]


# ---------------------------------------------------------------------------
# Control-flow signals
# ---------------------------------------------------------------------------

class _Break(Exception):
    pass

class _Continue(Exception):
    pass

class _Return(Exception):
    def __init__(self, value):
        self.value = value

class _Throw(Exception):
    def __init__(self, value):
        self.value = value

class _Yield(Exception):
    def __init__(self, value):
        self.value = value


class JSGenerator(JSObject):
    """JavaScript generator object returned by generator functions."""

    def __init__(self, values: list):
        super().__init__()
        self._values = iter(values)
        self._done = False
        self['next'] = self._next
        self['return'] = self._return
        self['throw'] = lambda v=_UNDEF: self._return(_UNDEF)
        # Make iterable
        self['Symbol.iterator'] = lambda: self

    def _next(self, value=_UNDEF):
        if self._done:
            return JSObject({'value': _UNDEF, 'done': True})
        try:
            v = next(self._values)
            return JSObject({'value': v, 'done': False})
        except StopIteration:
            self._done = True
            return JSObject({'value': _UNDEF, 'done': True})

    def _return(self, value=_UNDEF):
        self._done = True
        return JSObject({'value': value, 'done': True})


# ---------------------------------------------------------------------------
# Interpreter
# ---------------------------------------------------------------------------

class Interpreter:
    """JavaScript tree-walking interpreter."""

    MAX_ITERATIONS = 100_000

    def __init__(self):
        self.global_env = Environment()
        self.console_prefix = '[JS]'
        self._setup_globals()
        self._iteration_count = 0

    # ------------------------------------------------------------------
    # Global environment setup
    # ------------------------------------------------------------------

    def _setup_globals(self):
        g = self.global_env

        # console
        con = JSObject()
        for method in ('log', 'warn', 'error', 'info', 'debug'):
            con[method] = lambda *a: self._console_log(*a)
        g.define('console', con)

        # JSON
        json_obj = JSObject()
        json_obj['parse'] = lambda s: json.loads(_to_str(s)) if isinstance(s, str) else None
        json_obj['stringify'] = lambda v, *_: json.dumps(_js_to_python(v))
        g.define('JSON', json_obj)

        # Math
        math_obj = JSObject()
        for name in ('ceil', 'floor', 'round', 'abs', 'sqrt', 'pow',
                      'sin', 'cos', 'tan', 'atan', 'atan2', 'log', 'log2',
                      'log10', 'exp', 'sign', 'cbrt', 'hypot', 'trunc'):
            if hasattr(math, name):
                math_obj[name] = getattr(math, name)
        math_obj['max'] = lambda *a: max(a) if a else float('-inf')
        math_obj['min'] = lambda *a: min(a) if a else float('inf')
        math_obj['random'] = lambda: __import__('random').random()
        math_obj['PI'] = math.pi
        math_obj['E'] = math.e
        math_obj['LN2'] = math.log(2)
        math_obj['LN10'] = math.log(10)
        math_obj['SQRT2'] = math.sqrt(2)
        g.define('Math', math_obj)

        # Global functions
        g.define('parseInt', _parse_int)
        g.define('parseFloat', _parse_float)
        g.define('isNaN', lambda v: isinstance(v, float) and v != v)
        g.define('isFinite', lambda v: isinstance(v, (int, float)) and math.isfinite(v))
        _up = __import__('urllib.parse', fromlist=['quote', 'unquote'])
        g.define('encodeURIComponent', lambda s: _up.quote(_to_str(s), safe=''))
        g.define('decodeURIComponent', lambda s: _up.unquote(_to_str(s)))
        g.define('encodeURI', lambda s: _up.quote(_to_str(s), safe=":/?#[]@!$&'()*+,;=-._~"))
        g.define('decodeURI', lambda s: _up.unquote(_to_str(s)))

        # String constructor
        def _string_ctor(v=_UNDEF):
            return '' if v is _UNDEF else _to_str(v)
        _string_ctor.fromCharCode = lambda *codes: ''.join(chr(int(_to_num(c))) for c in codes)
        _string_ctor.fromCodePoint = lambda *codes: ''.join(chr(int(_to_num(c))) for c in codes)
        g.define('String', _string_ctor)

        # Number constructor
        def _number_ctor(v=0):
            return _to_num(v)
        _number_ctor.isNaN = lambda v: isinstance(v, float) and v != v
        _number_ctor.isFinite = lambda v: isinstance(v, (int, float)) and math.isfinite(v)
        _number_ctor.isInteger = lambda v: isinstance(v, (int, float)) and float(v) == int(v)
        _number_ctor.parseInt = _parse_int
        _number_ctor.parseFloat = _parse_float
        _number_ctor.MAX_SAFE_INTEGER = 2 ** 53 - 1
        _number_ctor.MIN_SAFE_INTEGER = -(2 ** 53 - 1)
        _number_ctor.POSITIVE_INFINITY = float('inf')
        _number_ctor.NEGATIVE_INFINITY = float('-inf')
        _number_ctor.NaN = float('nan')
        _number_ctor.EPSILON = 2.220446049250313e-16
        g.define('Number', _number_ctor)

        g.define('Boolean', lambda v=False: _to_bool(v))

        # Array constructor (needs 'from' as an attribute)
        class _ArrayCtor:
            def __call__(self_, *args):
                if len(args) == 1 and isinstance(args[0], (int, float)):
                    return JSArray([_UNDEF] * max(0, int(args[0])))
                return JSArray(args)
        _ac = _ArrayCtor()
        _ac.isArray = lambda v: isinstance(v, list)
        _ac.of = lambda *a: JSArray(a)
        _ac.__dict__['from'] = lambda v, fn=None: _array_from(v, fn)
        g.define('Array', _ac)

        # Object constructor
        def _object_ctor(v=_UNDEF):
            if v is _UNDEF or v is None:
                return JSObject()
            return v if isinstance(v, dict) else JSObject()
        _object_ctor.assign = _object_assign
        _object_ctor.keys = lambda obj: JSArray(k for k, _ in _iter_enumerable_entries(obj))
        _object_ctor.values = lambda obj: JSArray(v for _, v in _iter_enumerable_entries(obj))
        _object_ctor.entries = lambda obj: JSArray(JSArray([k, v]) for k, v in _iter_enumerable_entries(obj))
        _object_ctor.create = _object_create
        _object_ctor.freeze = lambda obj: obj
        _object_ctor.fromEntries = lambda pairs: JSObject(
            {_to_str(p[0]): p[1] for p in (pairs or []) if isinstance(p, (list, tuple)) and len(p) >= 2})
        _object_ctor.is_ = lambda a, b: _strict_eq(a, b)
        _object_ctor.hasOwn = lambda obj, k: isinstance(obj, dict) and _to_str(k) in obj
        _object_ctor.getPrototypeOf = lambda obj: getattr(obj, '_proto', None)
        _object_ctor.getOwnPropertyNames = lambda obj: JSArray(list(obj.keys()) if isinstance(obj, dict) else [])
        _object_ctor.defineProperty = lambda obj, k, d: obj.__setitem__(k, d.get('value', _UNDEF)) or obj if isinstance(obj, dict) else obj
        g.define('Object', _object_ctor)

        g.define('RegExp', lambda p, f='': re.compile(_to_str(p)))
        g.define('setTimeout', lambda fn, ms=0, *a: self._set_timeout(fn, ms, *a))
        g.define('setInterval', lambda fn, ms=0, *a: None)
        g.define('clearTimeout', lambda tid: self._clear_timeout(tid))
        g.define('clearInterval', lambda tid: self._clear_timeout(tid))
        g.define('requestAnimationFrame', lambda fn: self._request_animation_frame(fn))
        g.define('cancelAnimationFrame', lambda req_id: self._cancel_animation_frame(req_id))
        g.define('queueMicrotask', lambda fn: _enqueue_microtask(lambda: self._call_value(fn, [])))
        g.define('alert', lambda msg='': None)
        g.define('confirm', lambda msg='': False)
        g.define('prompt', lambda msg='', d='': d)
        g.define('undefined', _UNDEF)
        g.define('NaN', float('nan'))
        g.define('Infinity', float('inf'))

        _sym_registry = {}
        def _symbol(desc=_UNDEF):
            key = _to_str(desc) if desc is not _UNDEF else ''
            sym = f'Symbol({key})'
            return sym
        _sym_ctor = type('SymbolCtor', (), {
            '__call__': staticmethod(_symbol),
            'for': staticmethod(lambda key: _sym_registry.setdefault(_to_str(key), f'Symbol({_to_str(key)})')),
            'iterator': 'Symbol(Symbol.iterator)',
            'hasInstance': 'Symbol(Symbol.hasInstance)',
            'toPrimitive': 'Symbol(Symbol.toPrimitive)',
        })()
        g.define('Symbol', _sym_ctor)

        # Promise
        promise_ctor = _PromiseCtor(interp=self)
        promise_obj = promise_ctor.make_ctor_obj()
        # Allow 'new Promise(fn)' via _exec_new by making it a callable JSFunction-like
        promise_obj['__is_promise_ctor__'] = True
        g.define('Promise', promise_obj)

        for ename in ('Error', 'TypeError', 'RangeError', 'ReferenceError', 'SyntaxError'):
            g.define(ename, lambda msg='', _n=ename: JSObject({'message': _to_str(msg), 'name': _n}))

        g.define('Date', _JSDate)

        # window / globalThis
        window = JSObject()
        window['document'] = _UNDEF
        window['location'] = JSObject({'href': '', 'hostname': '', 'pathname': '/'})
        window['navigator'] = JSObject({'userAgent': 'rENDER/1.0'})
        window['innerWidth'] = 980
        window['innerHeight'] = 600
        window['setTimeout'] = g.get('setTimeout')
        window['setInterval'] = g.get('setInterval')
        window['clearTimeout'] = g.get('clearTimeout')
        window['clearInterval'] = g.get('clearInterval')
        window['requestAnimationFrame'] = g.get('requestAnimationFrame')
        window['cancelAnimationFrame'] = g.get('cancelAnimationFrame')
        window['addEventListener'] = lambda *a: None
        window['removeEventListener'] = lambda *a: None
        window['getComputedStyle'] = lambda el, *a: JSObject()
        g.define('window', window)
        g.define('self', window)
        g.define('globalThis', window)

    def _console_log(self, *args):
        msg = ' '.join(_to_str(a) for a in args)
        print(self.console_prefix, msg) if self.console_prefix else print(msg)

    def _set_timeout(self, fn, ms, *args):
        try:
            return get_event_loop().set_timeout(fn, int(_to_num(ms)), *args, _interp=self)
        except Exception as exc:
            _logger.debug('setTimeout ignored: %s', exc)
            return None

    def _clear_timeout(self, timer_id):
        try:
            get_event_loop().clear_timeout(int(_to_num(timer_id)))
        except Exception as exc:
            _logger.debug('clearTimeout ignored: %s', exc)

    def _request_animation_frame(self, fn):
        try:
            return get_event_loop().request_animation_frame(
                lambda ts=0.0: self._call_value(fn, [ts])
            )
        except Exception as exc:
            _logger.debug('requestAnimationFrame ignored: %s', exc)
            return None

    def _cancel_animation_frame(self, request_id):
        try:
            get_event_loop().cancel_animation_frame(int(_to_num(request_id)))
        except Exception as exc:
            _logger.debug('cancelAnimationFrame ignored: %s', exc)

    # ------------------------------------------------------------------
    # Entry points
    # ------------------------------------------------------------------

    def execute(self, ast) -> None:
        if ast is None:
            return
        if ast.type == 'Program':
            for stmt in ast.data.get('body', []):
                self._exec_stmt(stmt, self.global_env)

    def evaluate(self, node):
        return self._eval(node, self.global_env)

    # ------------------------------------------------------------------
    # Statement execution
    # ------------------------------------------------------------------

    def _exec_stmt(self, node, env):
        if node is None:
            return
        t = node.type

        if t == 'ExprStmt':
            self._eval(node.data['expr'], env)

        elif t == 'VarDecl':
            for name, init in node.data['decls']:
                val = self._eval(init, env) if init else _UNDEF
                if isinstance(name, str):
                    env.define(name, val)
                elif isinstance(name, ASTNode):
                    self._destructure(name, val, env)

        elif t == 'FuncDecl':
            fn = self._make_function(node, env)
            if fn.name and fn.name != '(anonymous)':
                env.define(fn.name, fn)

        elif t == 'ClassDecl':
            cls_fn = self._make_class(node, env)
            name = node.data.get('name')
            if name:
                env.define(name, cls_fn)

        elif t == 'Block':
            block_env = Environment(env)
            for stmt in node.data['body']:
                self._exec_stmt(stmt, block_env)

        elif t == 'Return':
            val = self._eval(node.data['value'], env) if node.data.get('value') else _UNDEF
            raise _Return(val)

        elif t == 'If':
            cond = self._eval(node.data['cond'], env)
            if _to_bool(cond):
                self._exec_stmt(node.data['then'], env)
            elif node.data.get('else_'):
                self._exec_stmt(node.data['else_'], env)

        elif t == 'While':
            self._iteration_count = 0
            while _to_bool(self._eval(node.data['cond'], env)):
                self._iteration_count += 1
                if self._iteration_count > self.MAX_ITERATIONS:
                    break
                try:
                    self._exec_stmt(node.data['body'], env)
                except _Break:
                    break
                except _Continue:
                    continue

        elif t == 'DoWhile':
            self._iteration_count = 0
            while True:
                self._iteration_count += 1
                if self._iteration_count > self.MAX_ITERATIONS:
                    break
                try:
                    self._exec_stmt(node.data['body'], env)
                except _Break:
                    break
                except _Continue:
                    pass
                if not _to_bool(self._eval(node.data['cond'], env)):
                    break

        elif t == 'For':
            loop_env = Environment(env)
            init = node.data.get('init')
            if init:
                if isinstance(init, ASTNode) and init.type == 'VarDecl':
                    self._exec_stmt(init, loop_env)
                else:
                    self._eval(init, loop_env)
            self._iteration_count = 0
            while True:
                self._iteration_count += 1
                if self._iteration_count > self.MAX_ITERATIONS:
                    break
                cond = node.data.get('cond')
                if cond and not _to_bool(self._eval(cond, loop_env)):
                    break
                try:
                    self._exec_stmt(node.data['body'], loop_env)
                except _Break:
                    break
                except _Continue:
                    pass
                upd = node.data.get('update')
                if upd:
                    self._eval(upd, loop_env)

        elif t == 'ForIn':
            iterable = self._eval(node.data['iterable'], env)
            loop_type = node.data.get('loop_type', 'in')
            loop_env = Environment(env)
            if isinstance(iterable, JSGenerator):
                # for-of generator: exhaust the iterator
                items = []
                if loop_type == 'of':
                    while True:
                        result = iterable._next()
                        if result.get('done'):
                            break
                        items.append(result.get('value', _UNDEF))
                else:
                    items = list(range(len(iterable._values)))
            elif isinstance(iterable, dict):
                items = list(iterable.keys()) if loop_type == 'in' else list(iterable.values())
            elif isinstance(iterable, (list, tuple)):
                items = list(range(len(iterable))) if loop_type == 'in' else list(iterable)
            elif isinstance(iterable, str):
                items = list(range(len(iterable))) if loop_type == 'in' else list(iterable)
            else:
                items = []
            name = node.data.get('name')
            pattern = node.data.get('pattern')
            for item in items:
                if name:
                    loop_env.define(name, item)
                elif isinstance(pattern, ASTNode):
                    self._destructure(pattern, item, loop_env)
                try:
                    self._exec_stmt(node.data['body'], loop_env)
                except _Break:
                    break
                except _Continue:
                    continue

        elif t == 'Try':
            try:
                self._exec_stmt(node.data['block'], env)
            except _Throw as e:
                if node.data.get('catch_body'):
                    catch_env = Environment(env)
                    if node.data.get('catch_param'):
                        catch_env.define(node.data['catch_param'], e.value)
                    self._exec_stmt(node.data['catch_body'], catch_env)
            except Exception as e:
                if node.data.get('catch_body'):
                    catch_env = Environment(env)
                    if node.data.get('catch_param'):
                        catch_env.define(node.data['catch_param'], str(e))
                    self._exec_stmt(node.data['catch_body'], catch_env)
            finally:
                if node.data.get('finally_body'):
                    self._exec_stmt(node.data['finally_body'], env)

        elif t == 'Throw':
            raise _Throw(self._eval(node.data['value'], env))

        elif t == 'Switch':
            disc = self._eval(node.data['disc'], env)
            matched = False
            for case in node.data['cases']:
                if case.type == 'Case':
                    if not matched and disc == self._eval(case.data['test'], env):
                        matched = True
                elif case.type == 'Default':
                    matched = True
                if matched:
                    try:
                        for stmt in case.data['body']:
                            self._exec_stmt(stmt, env)
                    except _Break:
                        return

        elif t == 'Break':
            raise _Break()
        elif t == 'Continue':
            raise _Continue()
        elif t == 'Labeled':
            try:
                self._exec_stmt(node.data['body'], env)
            except _Break:
                pass

    # ------------------------------------------------------------------
    # Expression evaluation
    # ------------------------------------------------------------------

    def _eval(self, node, env):
        if node is None:
            return _UNDEF
        t = node.type

        if t == 'Literal':
            return node.data['value']

        if t == 'Ident':
            return env.get(node.data['name'])

        if t == 'This':
            v = env.get('this')
            return v if v is not _UNDEF else self.global_env.get('window')

        if t == 'Super':
            return env.get('__super__')

        if t == 'TemplateLiteral':
            return ''.join(_to_str(self._eval(p, env)) for p in node.data['parts'])

        if t == 'BinOp':
            op = node.data['op']
            if op == '&&':
                left = self._eval(node.data['left'], env)
                return left if not _to_bool(left) else self._eval(node.data['right'], env)
            if op == '||':
                left = self._eval(node.data['left'], env)
                return left if _to_bool(left) else self._eval(node.data['right'], env)
            if op == '??':
                left = self._eval(node.data['left'], env)
                return left if left is not None and left is not _UNDEF else self._eval(node.data['right'], env)
            left = self._eval(node.data['left'], env)
            right = self._eval(node.data['right'], env)
            if op == 'instanceof':
                return self._instanceof(left, right)
            return _binop(op, left, right)

        if t == 'UnaryOp':
            return self._eval_unary(node, env)

        if t == 'UpdatePre':
            return self._update(node.data['operand'], node.data['op'], True, env)

        if t == 'UpdatePost':
            return self._update(node.data['operand'], node.data['op'], False, env)

        if t == 'Assign':
            return self._assign(node, env)

        if t == 'Ternary':
            cond = self._eval(node.data['cond'], env)
            return self._eval(node.data['then'], env) if _to_bool(cond) else self._eval(node.data['else_'], env)

        if t == 'Call':
            return self._exec_call(node, env)

        if t == 'New':
            return self._exec_new(node, env)

        if t == 'Member':
            optional = node.data.get('optional', False)
            obj = self._eval(node.data['obj'], env)
            if optional and (obj is _UNDEF or obj is None):
                return _UNDEF
            return self._member_get(obj, node, env)

        if t == 'Array':
            result = JSArray()
            for el in node.data['elements']:
                if isinstance(el, ASTNode) and el.type == 'Spread':
                    result.extend(_expand_spread_values(self._eval(el.data['arg'], env)))
                else:
                    result.append(self._eval(el, env))
            return result

        if t == 'Object':
            result = JSObject()
            for key, val_node in node.data['props']:
                if isinstance(key, ASTNode) and key.type in ('SpreadProp',):
                    _object_assign(result, self._eval(val_node, env))
                    continue
                if key is None:
                    _object_assign(result, self._eval(val_node, env))
                    continue
                if isinstance(key, ASTNode) and key.type == 'Computed':
                    key = _to_str(self._eval(key.data['expr'], env))
                elif isinstance(key, ASTNode):
                    key = _to_str(self._eval(key, env))
                result[key] = self._eval(val_node, env)
            return result

        if t == 'FuncDecl':
            fn = self._make_function(node, env)
            if fn.name and fn.name != '(anonymous)':
                env.define(fn.name, fn)
            return fn

        if t == 'ClassDecl':
            return self._make_class(node, env)

        if t == 'Await':
            val = self._eval(node.data['value'], env)
            return _unwrap_promise(val)

        if t == 'Yield':
            # Yield inside _exec_generator_stmt is handled there; here it's a no-op
            return self._eval(node.data.get('value'), env) if node.data.get('value') else _UNDEF

        if t == 'Spread':
            return self._eval(node.data['arg'], env)

        if t == 'Comma':
            self._eval(node.data['left'], env)
            return self._eval(node.data['right'], env)

        return _UNDEF

    def _eval_unary(self, node, env):
        op = node.data['op']
        if op == 'typeof':
            operand = node.data['operand']
            val = env.get(operand.data['name']) if operand.type == 'Ident' else self._eval(operand, env)
            return _typeof(val)
        if op == 'void':
            self._eval(node.data['operand'], env)
            return _UNDEF
        if op == 'delete':
            operand = node.data['operand']
            if operand.type == 'Member':
                obj = self._eval(operand.data['obj'], env)
                prop = self._resolve_prop(operand, env)
                if isinstance(obj, dict) and prop in obj:
                    del obj[prop]
            return True
        val = self._eval(node.data['operand'], env)
        if op == '!':
            return not _to_bool(val)
        if op == '-':
            return -_to_num(val)
        if op == '+':
            return _to_num(val)
        if op == '~':
            return ~int(_to_num(val))
        return _UNDEF

    # ------------------------------------------------------------------
    # Helpers
    # ------------------------------------------------------------------

    def _resolve_prop(self, member_node, env):
        if member_node.data.get('computed'):
            return _to_str(self._eval(member_node.data['prop'], env))
        return member_node.data['prop']

    def _member_get(self, obj, member_node, env):
        return _get_property(obj, self._resolve_prop(member_node, env))

    def _assign(self, node, env):
        op = node.data['op']
        target = node.data['left']
        rval = self._eval(node.data['right'], env)

        if op != '=':
            old = self._eval(target, env)
            if op == '&&=':
                rval = rval if _to_bool(old) else old; op = '='
            elif op == '||=':
                rval = old if _to_bool(old) else rval; op = '='
            elif op == '??=':
                rval = old if old is not None and old is not _UNDEF else rval; op = '='
            else:
                rval = _binop(op[:-1], old, rval)

        if target.type == 'Ident':
            env.set(target.data['name'], rval)
        elif target.type == 'Member':
            obj = self._eval(target.data['obj'], env)
            _set_property(obj, self._resolve_prop(target, env), rval)
        elif target.type in ('ObjectPattern', 'ArrayPattern'):
            self._destructure(target, rval, env)
        return rval

    def _update(self, target, op, prefix, env):
        old = _to_num(self._eval(target, env))
        new = old + 1 if op == '++' else old - 1
        if target.type == 'Ident':
            env.set(target.data['name'], new)
        elif target.type == 'Member':
            _set_property(self._eval(target.data['obj'], env),
                          self._resolve_prop(target, env), new)
        return new if prefix else old

    def _instanceof(self, obj, cls):
        proto = (cls.properties.get('prototype') if isinstance(cls, JSFunction)
                 else getattr(cls, 'prototype', None))
        if proto is None:
            return False
        p = getattr(obj, '_proto', None)
        while p is not None:
            if p is proto:
                return True
            p = getattr(p, '_proto', None)
        return False

    # ------------------------------------------------------------------
    # Function / class building
    # ------------------------------------------------------------------

    def _make_function(self, node, env) -> JSFunction:
        fn = JSFunction(node.data.get('name'), node.data.get('params', []),
                        node.data.get('body'), env)
        fn.param_defaults = node.data.get('param_defaults', {})
        fn.param_rest = node.data.get('param_rest')
        fn.param_patterns = node.data.get('param_patterns', {})
        fn.is_class = False
        fn.is_async = node.data.get('is_async', False)
        fn.is_generator = node.data.get('is_generator', False)
        proto = JSObject({'constructor': fn})
        fn.properties['prototype'] = proto
        return fn

    def _make_class(self, node, env) -> JSFunction:
        methods = node.data.get('methods', [])
        super_node = node.data.get('superclass')
        super_cls = self._eval(super_node, env) if super_node else None

        ctor_node = next((m for m in methods
                         if m.data.get('name') == 'constructor'
                         and not m.data.get('is_static')), None)

        if ctor_node:
            ctor_fn = JSFunction(node.data.get('name'), ctor_node.data.get('params', []),
                                 ctor_node.data.get('body'), env)
            ctor_fn.param_defaults = ctor_node.data.get('param_defaults', {})
            ctor_fn.param_rest = ctor_node.data.get('param_rest')
            ctor_fn.param_patterns = ctor_node.data.get('param_patterns', {})
        else:
            from js.ast import _node as _n
            ctor_fn = JSFunction(node.data.get('name'), [], _n('Block', body=[]), env)
            ctor_fn.param_defaults = {}
            ctor_fn.param_rest = None
            ctor_fn.param_patterns = {}

        ctor_fn.is_class = True
        ctor_fn.super_cls = super_cls

        proto = JSObject({'constructor': ctor_fn})
        if super_cls is not None:
            sp = (super_cls.properties.get('prototype') if isinstance(super_cls, JSFunction) else None)
            if isinstance(sp, dict):
                proto._proto = sp

        for m in methods:
            mname = m.data.get('name')
            if isinstance(mname, ASTNode):
                mname = _to_str(self._eval(mname, env))
            if mname == 'constructor' and not m.data.get('is_static'):
                continue
            fn = JSFunction(mname, m.data.get('params', []), m.data.get('body'), env)
            fn.param_defaults = m.data.get('param_defaults', {})
            fn.param_rest = m.data.get('param_rest')
            fn.param_patterns = m.data.get('param_patterns', {})
            fn.is_class = False
            fn.is_async = m.data.get('is_async', False)
            fn.is_generator = m.data.get('is_generator', False)
            target = ctor_fn.properties if m.data.get('is_static') else proto
            kind = m.data.get('kind', 'method')
            if kind == 'get':
                target[f'__get__{mname}'] = fn
            elif kind == 'set':
                target[f'__set__{mname}'] = fn
            else:
                target[mname] = fn

        ctor_fn.properties['prototype'] = proto
        return ctor_fn

    # ------------------------------------------------------------------
    # Function calls
    # ------------------------------------------------------------------

    def _exec_call(self, node, env):
        callee_node = node.data['callee']
        optional = node.data.get('optional', False)
        args = self._eval_args(node.data['args'], env)

        if callee_node.type == 'Member':
            this_val = self._eval(callee_node.data['obj'], env)
            if optional and (this_val is _UNDEF or this_val is None):
                return _UNDEF
            fn = self._member_get(this_val, callee_node, env)
            if optional and (fn is _UNDEF or fn is None):
                return _UNDEF
            return self._call_value(fn, args, this_val)

        fn = self._eval(callee_node, env)
        if optional and (fn is _UNDEF or fn is None):
            return _UNDEF
        return self._call_value(fn, args, None)

    def _eval_args(self, raw_args, env) -> list:
        args = []
        for arg in raw_args:
            if isinstance(arg, ASTNode) and arg.type == 'Spread':
                args.extend(_expand_spread_values(self._eval(arg.data['arg'], env)))
            else:
                args.append(self._eval(arg, env))
        return args

    def _call_value(self, fn, args, this_val=None):
        if fn is _UNDEF or fn is None:
            return _UNDEF
        if isinstance(fn, JSFunction):
            return self._call_function(fn, args, this_val)
        if callable(fn):
            try:
                return fn(*args)
            except Exception:
                return _UNDEF
        return _UNDEF

    def _call_function(self, fn: JSFunction, args: list, this_val=None):
        call_env = Environment(fn.env)
        params = fn.params
        defaults = getattr(fn, 'param_defaults', {})
        rest = getattr(fn, 'param_rest', None)
        patterns = getattr(fn, 'param_patterns', {})

        for i, param in enumerate(params):
            raw = args[i] if i < len(args) else _UNDEF
            if raw is _UNDEF and i in defaults:
                raw = self._eval(defaults[i], call_env)
            if param.startswith('__pattern__') and i in patterns:
                call_env.define(param, raw)
                self._destructure(patterns[i], raw, call_env)
            else:
                call_env.define(param, raw)

        if rest:
            call_env.define(rest, JSArray(args[len(params):]))

        call_env.define('arguments', JSArray(args))
        if this_val is not None:
            call_env.define('this', this_val)

        is_gen = getattr(fn, 'is_generator', False)
        is_async = getattr(fn, 'is_async', False)

        if is_gen:
            return self._run_generator(fn.body, call_env)

        if is_async:
            p = JSPromise(_interp=self)
            try:
                self._exec_stmt(fn.body, call_env)
                p._resolve(_UNDEF)
            except _Return as r:
                val = r.value
                # If the returned value is a Promise, chain it
                if isinstance(val, JSPromise):
                    val._then(p._resolve, p._reject)
                else:
                    p._resolve(val)
            except _Throw as e:
                p._reject(e.value)
            except Exception as e:
                p._reject(str(e))
            return p

        try:
            self._exec_stmt(fn.body, call_env)
        except _Return as r:
            return r.value
        return _UNDEF

    def _run_generator(self, body, env) -> 'JSGenerator':
        """Execute a generator body, collecting all yielded values eagerly.

        Yield does not truly suspend execution — all yields are collected
        synchronously and the caller gets an iterator over them.  This covers
        the common for-of / spread / Array.from usage without requiring
        Python coroutine machinery.
        """
        yielded: list = []
        env.define('__gen_yields__', yielded)
        try:
            self._exec_generator_stmt(body, env, yielded)
        except _Return:
            pass
        return JSGenerator(yielded)

    def _exec_generator_stmt(self, node, env, yields: list):
        """Like _exec_stmt but intercepts Yield nodes."""
        if node is None:
            return
        t = node.type
        if t == 'Yield':
            val = self._eval(node.data.get('value'), env) if node.data.get('value') else _UNDEF
            yields.append(val)
            return
        if t == 'ExprStmt':
            expr = node.data.get('expr')
            if expr is not None and expr.type == 'Yield':
                val = self._eval(expr.data.get('value'), env) if expr.data.get('value') else _UNDEF
                yields.append(val)
                return
            # Normal expression (no yield) — execute as-is
            self._eval(expr, env)
            return
        if t == 'Block':
            block_env = Environment(env)
            for stmt in node.data['body']:
                self._exec_generator_stmt(stmt, block_env, yields)
            return
        if t == 'For':
            loop_env = Environment(env)
            init = node.data.get('init')
            if init:
                if isinstance(init, ASTNode) and init.type == 'VarDecl':
                    self._exec_stmt(init, loop_env)
                else:
                    self._eval(init, loop_env)
            count = 0
            while True:
                count += 1
                if count > self.MAX_ITERATIONS:
                    break
                cond = node.data.get('cond')
                if cond and not _to_bool(self._eval(cond, loop_env)):
                    break
                try:
                    self._exec_generator_stmt(node.data['body'], loop_env, yields)
                except _Break:
                    break
                except _Continue:
                    pass
                upd = node.data.get('update')
                if upd:
                    self._eval(upd, loop_env)
            return
        if t == 'While':
            count = 0
            while _to_bool(self._eval(node.data['cond'], env)):
                count += 1
                if count > self.MAX_ITERATIONS:
                    break
                try:
                    self._exec_generator_stmt(node.data['body'], env, yields)
                except _Break:
                    break
                except _Continue:
                    continue
            return
        if t == 'If':
            if _to_bool(self._eval(node.data['cond'], env)):
                self._exec_generator_stmt(node.data['then'], env, yields)
            elif node.data.get('else_'):
                self._exec_generator_stmt(node.data['else_'], env, yields)
            return
        # Fall through to normal statement execution for everything else
        self._exec_stmt(node, env)

    def _exec_new(self, node, env):
        callee = self._eval(node.data['callee'], env)
        args = self._eval_args(node.data.get('args', []), env)

        if callee is _UNDEF or callee is None:
            return JSObject()

        # new Promise(executor)
        if isinstance(callee, dict) and callee.get('__is_promise_ctor__'):
            executor = args[0] if args else _UNDEF
            return JSPromise(executor if executor is not _UNDEF else None, _interp=self)

        if isinstance(callee, JSFunction):
            obj = JSObject()
            proto = callee.properties.get('prototype')
            if isinstance(proto, dict):
                obj._proto = proto

            super_cls = getattr(callee, 'super_cls', None)
            if super_cls is not None:
                def _super_call(*sargs, _obj=obj):
                    if isinstance(super_cls, JSFunction):
                        self._call_function(super_cls, list(sargs), _obj)
                    elif callable(super_cls):
                        super_cls(*sargs)
                # Inject super into environment via call_function
                _orig_body = callee.body
                # Patch env before calling
                callee_env_patch = Environment(callee.env)
                callee_env_patch.define('super', _super_call)
                sp = super_cls.properties.get('prototype') if isinstance(super_cls, JSFunction) else None
                callee_env_patch.define('__super__', sp or _UNDEF)
                patched_fn = JSFunction(callee.name, callee.params, callee.body, callee_env_patch)
                patched_fn.param_defaults = getattr(callee, 'param_defaults', {})
                patched_fn.param_rest = getattr(callee, 'param_rest', None)
                patched_fn.param_patterns = getattr(callee, 'param_patterns', {})
                patched_fn.is_class = False
                try:
                    self._call_function(patched_fn, args, obj)
                except _Return as r:
                    if isinstance(r.value, dict):
                        return r.value
                return obj

            try:
                self._call_function(callee, args, obj)
            except _Return as r:
                if isinstance(r.value, dict):
                    return r.value
            return obj

        if callable(callee):
            try:
                return callee(*args)
            except Exception:
                return JSObject()

        return JSObject()

    # ------------------------------------------------------------------
    # Destructuring
    # ------------------------------------------------------------------

    def _destructure(self, pattern: ASTNode, value, env):
        t = pattern.type

        if t == 'BindingDefault':
            name = pattern.data['name']
            default = pattern.data.get('default')
            val = value if value is not _UNDEF else (
                self._eval(default, env) if default is not None else _UNDEF)
            if isinstance(name, str):
                env.define(name, val)
            elif isinstance(name, ASTNode):
                self._destructure(name, val, env)

        elif t == 'ObjectPattern':
            obj = value if isinstance(value, dict) else JSObject()
            seen = set()
            for key, sub in pattern.data['props']:
                seen.add(key)
                raw = _get_property(obj, key)
                self._destructure(sub, raw, env)
            rest_name = pattern.data.get('rest')
            if rest_name:
                env.define(rest_name, JSObject(
                    {k: v for k, v in obj.items() if k not in seen}))

        elif t == 'ArrayPattern':
            lst = list(value) if isinstance(value, (list, tuple, str)) else []
            elements = pattern.data.get('elements', [])
            for i, elem in enumerate(elements):
                if elem is None:
                    continue
                self._destructure(elem, lst[i] if i < len(lst) else _UNDEF, env)
            rest_name = pattern.data.get('rest')
            if rest_name:
                env.define(rest_name, JSArray(lst[len(elements):]))


# ---------------------------------------------------------------------------
# Promise unwrap helper (used by await)
# ---------------------------------------------------------------------------

def _unwrap_promise(val):
    """Synchronously unwrap a settled Promise; return raw value otherwise."""
    if not isinstance(val, JSPromise):
        return val
    drain_microtasks()
    if val._state == JSPromise.FULFILLED:
        return val._value
    if val._state == JSPromise.REJECTED:
        raise _Throw(val._value)
    # Still pending — return undefined (best effort in synchronous model)
    return _UNDEF


# ---------------------------------------------------------------------------
# Array.from helper (used in _setup_globals)
# ---------------------------------------------------------------------------

def _array_from(iterable, map_fn=None):
    if iterable is _UNDEF or iterable is None:
        return JSArray()
    if isinstance(iterable, (list, tuple)):
        items = list(iterable)
    elif isinstance(iterable, str):
        items = list(iterable)
    elif isinstance(iterable, dict):
        items = list(iterable.values())
    else:
        items = []
    if map_fn is not None:
        items = [_safe_call(map_fn, [x, i]) for i, x in enumerate(items)]
    return JSArray(items)
