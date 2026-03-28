"""JavaScript tree-walking interpreter for rENDER browser engine."""
import math
import json
import re
from js.parser import _UNDEF, ASTNode

_DESCRIPTOR_STORE_ATTR = '_render_descriptors'


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


class Environment:
    """Scope chain for variable lookup."""
    __slots__ = ('bindings', 'parent', 'is_function_scope')

    def __init__(self, parent=None, is_function_scope=False):
        self.bindings = {}
        self.parent = parent
        self.is_function_scope = is_function_scope

    def get(self, name):
        if name in self.bindings:
            return self.bindings[name]
        if self.parent:
            return self.parent.get(name)
        window = self.bindings.get('window')
        if isinstance(window, dict) and name in window:
            return window[name]
        return _UNDEF

    def set(self, name, value):
        """Set in existing scope (walk up chain), or create in current."""
        env = self
        while env is not None:
            if name in env.bindings:
                env.bindings[name] = value
                return
            env = env.parent
        self.bindings[name] = value
        window = self.bindings.get('window')
        if isinstance(window, dict):
            window[name] = value

    def define(self, name, value):
        """Define in current scope."""
        self.bindings[name] = value
        if self.parent is None and name not in ('window', 'self', 'globalThis'):
            window = self.bindings.get('window')
            if isinstance(window, dict):
                window[name] = value


class JSFunction:
    """A JavaScript function (closure)."""
    __slots__ = (
        'name', 'params', 'body', 'env',
        'prototype', 'is_class', 'super_func', 'home_object', 'class_props',
    )

    def __init__(
        self,
        name,
        params,
        body,
        env,
        *,
        prototype=None,
        is_class=False,
        super_func=None,
        home_object=None,
        class_props=None,
    ):
        self.name = name or '(anonymous)'
        self.params = params
        self.body = body
        self.env = env
        if prototype is None:
            prototype = JSObject()
        self.prototype = prototype
        self.is_class = is_class
        self.super_func = super_func
        self.home_object = home_object
        self.class_props = class_props if class_props is not None else {}
        if isinstance(self.prototype, dict):
            self.prototype['constructor'] = self

    def __repr__(self):
        return f'function {self.name}()'


class JSObject(dict):
    """A JavaScript object (plain dict with prototype stub)."""
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self._proto = getattr(JSObject, 'prototype', None)


class JSArray(list):
    """A JavaScript array."""
    def __init__(self, iterable=()):
        super().__init__(iterable)
        self._proto = getattr(JSArray, 'prototype', getattr(JSObject, 'prototype', None))


class JSMap:
    """Minimal JavaScript Map."""
    __slots__ = ('_entries', '_proto')

    def __init__(self, iterable=None):
        self._entries = []
        self._proto = getattr(JSMap, 'prototype', getattr(JSObject, 'prototype', None))
        if isinstance(iterable, (list, tuple, JSArray)):
            for item in iterable:
                if isinstance(item, (list, tuple, JSArray)) and len(item) >= 2:
                    self.set(item[0], item[1])

    @property
    def size(self):
        return len(self._entries)

    def clear(self):
        self._entries.clear()

    def delete(self, key):
        for idx, (entry_key, _entry_val) in enumerate(self._entries):
            if _strict_eq(entry_key, key):
                self._entries.pop(idx)
                return True
        return False

    def get(self, key):
        for entry_key, entry_val in self._entries:
            if _strict_eq(entry_key, key):
                return entry_val
        return _UNDEF

    def has(self, key):
        return any(_strict_eq(entry_key, key) for entry_key, _ in self._entries)

    def set(self, key, value):
        for idx, (entry_key, _entry_val) in enumerate(self._entries):
            if _strict_eq(entry_key, key):
                self._entries[idx] = (key, value)
                return self
        self._entries.append((key, value))
        return self

    def forEach(self, callback, this_arg=None):
        for key, value in list(self._entries):
            _invoke_callable(callback, [value, key, self], this_arg)

    def entries(self):
        return _make_iterator([JSArray([key, value]) for key, value in self._entries])

    def keys(self):
        return _make_iterator([key for key, _ in self._entries])

    def values(self):
        return _make_iterator([value for _, value in self._entries])


class JSSet:
    """Minimal JavaScript Set."""
    __slots__ = ('_values', '_proto')

    def __init__(self, iterable=None):
        self._values = []
        self._proto = getattr(JSSet, 'prototype', getattr(JSObject, 'prototype', None))
        if isinstance(iterable, (list, tuple, JSArray)):
            for item in iterable:
                self.add(item)

    @property
    def size(self):
        return len(self._values)

    def add(self, value):
        if not self.has(value):
            self._values.append(value)
        return self

    def clear(self):
        self._values.clear()

    def delete(self, value):
        for idx, entry in enumerate(self._values):
            if _strict_eq(entry, value):
                self._values.pop(idx)
                return True
        return False

    def has(self, value):
        return any(_strict_eq(entry, value) for entry in self._values)

    def forEach(self, callback, this_arg=None):
        for value in list(self._values):
            _invoke_callable(callback, [value, value, self], this_arg)

    def entries(self):
        return _make_iterator([JSArray([value, value]) for value in self._values])

    def keys(self):
        return _make_iterator(list(self._values))

    def values(self):
        return _make_iterator(list(self._values))


class JSSymbol:
    """Minimal JavaScript Symbol value."""
    __slots__ = ('description',)

    def __init__(self, description=''):
        self.description = description

    def __repr__(self):
        if self.description:
            return f'Symbol({self.description})'
        return 'Symbol()'


class JSSuper:
    """Runtime wrapper for super constructor/method dispatch."""
    __slots__ = ('target', 'this_val')

    def __init__(self, target, this_val):
        self.target = target
        self.this_val = this_val


class Interpreter:
    """JavaScript tree-walking interpreter."""

    MAX_ITERATIONS = 100000  # safety limit for loops

    def __init__(self):
        self.global_env = Environment(is_function_scope=True)
        self._symbol_registry = {}
        self._raf_queue = []
        self._raf_cancelled = set()
        self._next_raf_id = 1
        self._microtask_queue = []
        self._execute_depth = 0
        self._setup_globals()
        self._iteration_count = 0

    def _setup_globals(self):
        g = self.global_env
        self._storage = {
            'local': JSObject(),
            'session': JSObject(),
        }

        # console
        console = JSObject()
        console['log'] = lambda *args: self._console_log(*args)
        console['warn'] = lambda *args: self._console_log(*args)
        console['error'] = lambda *args: self._console_log(*args)
        console['info'] = lambda *args: self._console_log(*args)
        g.define('console', console)

        # JSON
        json_obj = JSObject()
        json_obj['parse'] = lambda s: json.loads(_to_str(s)) if isinstance(s, str) else None
        json_obj['stringify'] = lambda v, *a: json.dumps(_js_to_python(v))
        g.define('JSON', json_obj)

        # Math
        math_obj = JSObject()
        math_obj['abs'] = abs
        math_obj['round'] = round
        for fn_name in ('ceil', 'floor', 'sqrt', 'pow',
                         'sin', 'cos', 'tan', 'atan2', 'log', 'exp'):
            math_obj[fn_name] = getattr(math, fn_name)
        math_obj['max'] = lambda *a: max(a) if a else float('-inf')
        math_obj['min'] = lambda *a: min(a) if a else float('inf')
        math_obj['random'] = lambda: __import__('random').random()
        math_obj['PI'] = math.pi
        math_obj['E'] = math.e
        g.define('Math', math_obj)

        performance_obj = JSObject()
        performance_obj['now'] = lambda: self._now_ms()
        g.define('performance', performance_obj)
        symbol_ctor = lambda desc=_UNDEF: self._make_symbol(desc)
        setattr(symbol_ctor, 'for', lambda key: self._symbol_for(key))
        setattr(symbol_ctor, 'keyFor', lambda sym: self._symbol_key_for(sym))
        g.define('Symbol', symbol_ctor)

        # Global functions
        g.define('parseInt', lambda s, r=10: _parse_int(s, r))
        g.define('parseFloat', lambda s: _parse_float(s))
        g.define('isNaN', lambda v: v != v if isinstance(v, float) else False)
        g.define('isFinite', lambda v: isinstance(v, (int, float)) and math.isfinite(v))
        g.define('encodeURIComponent', lambda s: __import__('urllib.parse', fromlist=['quote']).quote(str(s), safe=''))
        g.define('decodeURIComponent', lambda s: __import__('urllib.parse', fromlist=['unquote']).unquote(str(s)))
        g.define('encodeURI', lambda s: __import__('urllib.parse', fromlist=['quote']).quote(str(s), safe=':/?#[]@!$&\'()*+,;=-._~'))
        g.define('decodeURI', lambda s: __import__('urllib.parse', fromlist=['unquote']).unquote(str(s)))
        def string_ctor(*args):
            if not args:
                return ''
            return _to_str(args[0])
        setattr(string_ctor, 'fromCharCode', lambda *codes: ''.join(chr(_to_int(code)) for code in codes))
        g.define('String', string_ctor)
        g.define('Number', lambda v=0: _to_num(v))
        g.define('Boolean', lambda v=False: _to_bool(v))
        object_prototype = JSObject()
        object_prototype['hasOwnProperty'] = JSFunction(
            'hasOwnProperty',
            ['key'],
            ASTNode(
                'Block',
                body=[
                    ASTNode(
                        'Return',
                        value=ASTNode(
                            'BinOp',
                            op='in',
                            left=ASTNode('Ident', name='key'),
                            right=ASTNode('This'),
                        ),
                    )
                ],
            ),
            g,
        )
        object_prototype['toString'] = lambda: '[object Object]'
        JSObject.prototype = object_prototype
        array_ctor = lambda *values: _array_constructor(*values)
        setattr(array_ctor, 'isArray', lambda value: isinstance(value, (list, tuple, JSArray)))
        setattr(array_ctor, 'from', lambda value, map_fn=_UNDEF, this_arg=_UNDEF: _array_from(value, map_fn, this_arg))
        setattr(array_ctor, 'prototype', JSObject())
        JSArray.prototype = getattr(array_ctor, 'prototype')
        object_ctor = lambda value=_UNDEF: _object_constructor(value)
        setattr(object_ctor, 'prototype', object_prototype)
        setattr(object_ctor, 'assign', lambda target, *sources: _object_assign(target, *sources))
        setattr(object_ctor, 'keys', lambda obj: JSArray(_enumerable_own_keys(obj)))
        setattr(object_ctor, 'values', lambda obj: JSArray([_get_property(obj, key) for key in _enumerable_own_keys(obj)]))
        setattr(object_ctor, 'entries', lambda obj: JSArray(JSArray([key, _get_property(obj, key)]) for key in _enumerable_own_keys(obj)))
        setattr(object_ctor, 'create', lambda proto=None, props=_UNDEF: _object_create(proto, props))
        setattr(object_ctor, 'defineProperty', lambda obj, prop, desc: _object_define_property(obj, prop, desc))
        setattr(object_ctor, 'defineProperties', lambda obj, props: _object_define_properties(obj, props))
        setattr(object_ctor, 'getOwnPropertyDescriptor', lambda obj, prop: _object_get_own_property_descriptor(obj, prop))
        setattr(object_ctor, 'getOwnPropertyNames', lambda obj: JSArray(_own_property_names(obj)))
        setattr(object_ctor, 'getOwnPropertySymbols', lambda obj: JSArray())
        setattr(object_ctor, 'getPrototypeOf', lambda obj: _object_get_prototype_of(obj))
        setattr(object_ctor, 'setPrototypeOf', lambda obj, proto: _object_set_prototype_of(obj, proto))
        setattr(object_ctor, 'isExtensible', lambda obj: True)
        setattr(object_ctor, 'preventExtensions', lambda obj: obj)
        map_ctor = lambda iterable=None: JSMap(iterable)
        set_ctor = lambda iterable=None: JSSet(iterable)
        setattr(map_ctor, 'prototype', JSObject())
        setattr(set_ctor, 'prototype', JSObject())
        JSMap.prototype = getattr(map_ctor, 'prototype')
        JSSet.prototype = getattr(set_ctor, 'prototype')
        map_proto = getattr(map_ctor, 'prototype')
        set_proto = getattr(set_ctor, 'prototype')
        map_proto['clear'] = _native_method(lambda self: self.clear())
        map_proto['delete'] = _native_method(lambda self, key: self.delete(key))
        map_proto['forEach'] = _native_method(lambda self, callback, this_arg=None: self.forEach(callback, this_arg))
        map_proto['get'] = _native_method(lambda self, key: self.get(key))
        map_proto['has'] = _native_method(lambda self, key: self.has(key))
        map_proto['set'] = _native_method(lambda self, key, value: self.set(key, value))
        map_proto['entries'] = _native_method(lambda self: self.entries())
        map_proto['keys'] = _native_method(lambda self: self.keys())
        map_proto['values'] = _native_method(lambda self: self.values())
        map_proto['Symbol(Symbol.iterator)'] = _native_method(lambda self: self.entries())
        set_proto['add'] = _native_method(lambda self, value: self.add(value))
        set_proto['clear'] = _native_method(lambda self: self.clear())
        set_proto['delete'] = _native_method(lambda self, value: self.delete(value))
        set_proto['forEach'] = _native_method(lambda self, callback, this_arg=None: self.forEach(callback, this_arg))
        set_proto['has'] = _native_method(lambda self, value: self.has(value))
        set_proto['entries'] = _native_method(lambda self: self.entries())
        set_proto['keys'] = _native_method(lambda self: self.keys())
        set_proto['values'] = _native_method(lambda self: self.values())
        set_proto['Symbol(Symbol.iterator)'] = _native_method(lambda self: self.values())
        function_ctor = lambda *args: JSFunction('(anonymous)', [], ASTNode('Block', body=[]), g)
        setattr(function_ctor, 'prototype', JSObject())
        g.define('Array', array_ctor)
        g.define('Object', object_ctor)
        g.define('Function', function_ctor)
        g.define('Map', map_ctor)
        g.define('Set', set_ctor)
        g.define('WeakMap', map_ctor)
        g.define('WeakSet', set_ctor)
        g.define('RegExp', lambda p, f='': re.compile(p))
        g.define('setTimeout', lambda fn, ms=0, *a: self._set_timeout(fn, ms, *a))
        g.define('setInterval', lambda fn, ms=0, *a: None)
        g.define('clearTimeout', lambda tid: None)
        g.define('clearInterval', lambda tid: None)
        g.define('queueMicrotask', lambda fn: self._queue_microtask_call(fn))
        g.define('requestAnimationFrame', lambda fn: self._request_animation_frame(fn))
        g.define('cancelAnimationFrame', lambda rid: self._cancel_animation_frame(rid))
        g.define('alert', lambda msg='': None)
        g.define('confirm', lambda msg='': False)
        g.define('prompt', lambda msg='', d='': d)
        g.define('undefined', _UNDEF)
        g.define('NaN', float('nan'))
        g.define('Infinity', float('inf'))
        g.define('Error', lambda msg='': JSObject({'message': str(msg)}))
        g.define('TypeError', lambda msg='': JSObject({'message': str(msg)}))
        g.define('Date', _JSDate)
        g.define('Image', lambda *a: self._make_image())
        promise_ctor = lambda executor=None: self._make_promise(executor)
        setattr(promise_ctor, 'resolve', lambda value=_UNDEF: self._resolve_promise(value))
        setattr(promise_ctor, 'reject', lambda reason=_UNDEF: self._reject_promise(reason))
        setattr(promise_ctor, 'prototype', JSObject())
        g.define('Promise', promise_ctor)
        setattr(symbol_ctor, 'iterator', self._symbol_for('Symbol.iterator'))
        setattr(symbol_ctor, 'species', self._symbol_for('Symbol.species'))
        setattr(symbol_ctor, 'toStringTag', self._symbol_for('Symbol.toStringTag'))
        setattr(symbol_ctor, 'unscopables', self._symbol_for('Symbol.unscopables'))
        object_prototype['constructor'] = object_ctor
        getattr(array_ctor, 'prototype')['constructor'] = array_ctor
        getattr(function_ctor, 'prototype')['constructor'] = function_ctor
        getattr(map_ctor, 'prototype')['constructor'] = map_ctor
        getattr(set_ctor, 'prototype')['constructor'] = set_ctor

        # window = global
        window = JSObject()
        window['document'] = _UNDEF  # will be set by DOMBinding
        window['location'] = JSObject({'href': '', 'hostname': '', 'pathname': '/'})
        window['history'] = JSObject({
            'length': 1,
            'pushState': lambda *a: None,
            'replaceState': lambda *a: None,
            'back': lambda *a: None,
            'forward': lambda *a: None,
        })
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
        window['localStorage'] = self._make_storage('local')
        window['sessionStorage'] = self._make_storage('session')
        for key in (
            'console', 'JSON', 'Math', 'performance', 'parseInt', 'parseFloat',
            'isNaN', 'isFinite', 'encodeURIComponent', 'decodeURIComponent',
            'encodeURI', 'decodeURI', 'String', 'Number', 'Boolean', 'Symbol', 'Array',
            'Function',
            'Object', 'Map', 'Set', 'WeakMap', 'WeakSet', 'RegExp', 'setTimeout', 'setInterval', 'clearTimeout',
            'clearInterval', 'queueMicrotask', 'requestAnimationFrame',
            'cancelAnimationFrame', 'alert', 'confirm', 'prompt', 'undefined',
            'NaN', 'Infinity', 'Error', 'TypeError', 'Date', 'Image', 'Promise',
        ):
            window[key] = g.get(key)
        g.define('window', window)
        g.define('self', window)
        g.define('globalThis', window)
        g.define('localStorage', window['localStorage'])
        g.define('sessionStorage', window['sessionStorage'])
        g.define('history', window['history'])

    def _console_log(self, *args):
        parts = [_to_str(a) for a in args]
        print('[JS]', ' '.join(parts))

    def _now_ms(self):
        try:
            import time
            return time.time() * 1000
        except Exception:
            return 0

    def _set_timeout(self, fn, ms, *args):
        """Execute scheduled callbacks immediately in the single-threaded test runtime."""
        if callable(fn):
            try:
                fn(*args)
            except Exception:
                pass
        elif isinstance(fn, JSFunction):
            try:
                self._call_function(fn, list(args))
            except Exception:
                pass

    def _request_animation_frame(self, fn):
        rid = self._next_raf_id
        self._next_raf_id += 1
        if callable(fn) or isinstance(fn, JSFunction):
            self._raf_queue.append((rid, fn))
        return rid

    def _cancel_animation_frame(self, rid):
        self._raf_cancelled.add(_to_int(rid, default=0))

    def _queue_microtask_call(self, fn):
        self._queue_microtask(lambda: self._call_value(fn, []))

    def _queue_microtask(self, callback):
        if callback is not None:
            self._microtask_queue.append(callback)

    def _make_symbol(self, desc=_UNDEF):
        if desc is _UNDEF:
            desc = ''
        return JSSymbol(_to_str(desc))

    def _symbol_for(self, key):
        key = _to_str(key)
        if key not in self._symbol_registry:
            self._symbol_registry[key] = JSSymbol(key)
        return self._symbol_registry[key]

    def _symbol_key_for(self, sym):
        if not isinstance(sym, JSSymbol):
            return _UNDEF
        for key, value in self._symbol_registry.items():
            if value is sym:
                return key
        return _UNDEF

    def _drain_microtasks(self):
        processed = 0
        while self._microtask_queue and processed < self.MAX_ITERATIONS:
            callback = self._microtask_queue.pop(0)
            try:
                callback()
            except Exception:
                pass
            processed += 1

    def _flush_animation_frame(self):
        if not self._raf_queue:
            return
        frame_callbacks = self._raf_queue
        self._raf_queue = []
        try:
            import time
            timestamp = time.time() * 1000
        except Exception:
            timestamp = 0
        for rid, fn in frame_callbacks:
            if rid in self._raf_cancelled:
                self._raf_cancelled.discard(rid)
                continue
            try:
                self._call_value(fn, [timestamp])
            except Exception:
                pass
            self._drain_microtasks()

    def _make_storage(self, kind: str):
        store = self._storage[kind]
        meta_keys = {'getItem', 'setItem', 'removeItem', 'clear', 'key', 'length'}

        def _storage_keys():
            return [k for k in store.keys() if k not in meta_keys]

        def _update_length():
            store['length'] = len(_storage_keys())

        def _set_item(key, value):
            store[_to_str(key)] = _to_str(value)
            _update_length()

        def _remove_item(key):
            store.pop(_to_str(key), None)
            _update_length()

        def _clear():
            for key in list(_storage_keys()):
                store.pop(key, None)
            _update_length()

        store['getItem'] = lambda key: store.get(_to_str(key), None)
        store['setItem'] = _set_item
        store['removeItem'] = _remove_item
        store['clear'] = _clear
        store['key'] = lambda index: _storage_keys()[_to_int(index)] if 0 <= _to_int(index) < len(_storage_keys()) else None
        _update_length()
        return store

    def _make_image(self):
        image = JSObject({'width': 0, 'height': 0, 'complete': False, 'onload': None, 'onerror': None, '__instanceof__': 'HTMLImageElement'})
        return image

    def _make_promise(self, executor=None):
        promise = JSObject()
        promise['_state'] = 'pending'
        promise['_value'] = _UNDEF
        promise['_handlers'] = []
        promise['then'] = lambda on_resolve=None, on_reject=None: self._promise_then(promise, on_resolve, on_reject)
        promise['catch'] = lambda on_reject=None: self._promise_then(promise, None, on_reject)
        if executor not in (None, _UNDEF):
            try:
                self._call_value(executor, [
                    lambda value=_UNDEF: self._settle_promise(promise, 'fulfilled', value),
                    lambda reason=_UNDEF: self._settle_promise(promise, 'rejected', reason),
                ])
            except Exception as exc:
                self._settle_promise(promise, 'rejected', str(exc))
        return promise

    def _resolve_promise(self, value=_UNDEF):
        if isinstance(value, dict) and value.get('then') not in (None, _UNDEF):
            return value
        promise = self._make_promise()
        return self._settle_promise(promise, 'fulfilled', value)

    def _reject_promise(self, reason=_UNDEF):
        promise = self._make_promise()
        return self._settle_promise(promise, 'rejected', reason)

    def _promise_then(self, promise, on_resolve=None, on_reject=None):
        chained = self._make_promise()
        handler = (on_resolve, on_reject, chained)
        if promise.get('_state') == 'pending':
            promise['_handlers'].append(handler)
        else:
            self._queue_microtask(lambda: self._run_promise_handler(promise, handler))
        return chained

    def _settle_promise(self, promise, state, value):
        if promise.get('_state') != 'pending':
            return promise
        promise['_state'] = state
        promise['_value'] = value
        handlers = list(promise.get('_handlers', []))
        promise['_handlers'] = []
        for handler in handlers:
            self._queue_microtask(lambda h=handler: self._run_promise_handler(promise, h))
        return promise

    def _run_promise_handler(self, promise, handler):
        on_resolve, on_reject, chained = handler
        state = promise.get('_state')
        value = promise.get('_value', _UNDEF)
        callback = on_resolve if state == 'fulfilled' else on_reject
        if callback in (None, _UNDEF):
            return self._settle_promise(chained, state, value)
        try:
            result = self._call_value(callback, [value])
            if isinstance(result, dict) and result.get('then') not in (None, _UNDEF):
                then = result.get('then')
                if callable(then) or isinstance(then, JSFunction):
                    self._call_value(then, [
                        lambda resolved=_UNDEF: self._settle_promise(chained, 'fulfilled', resolved),
                        lambda rejected=_UNDEF: self._settle_promise(chained, 'rejected', rejected),
                    ], result)
                    return chained
            return self._settle_promise(chained, 'fulfilled', result)
        except Exception as exc:
            return self._settle_promise(chained, 'rejected', str(exc))

    def execute(self, ast) -> None:
        """Execute a Program AST node."""
        if ast is None:
            return
        self._execute_depth += 1
        try:
            if ast.type == 'Program':
                for stmt in ast.data.get('body', []):
                    self._exec_stmt(stmt, self.global_env)
        finally:
            self._execute_depth -= 1
            if self._execute_depth == 0:
                self._drain_microtasks()
                self._flush_animation_frame()
                self._drain_microtasks()

    def evaluate(self, node):
        """Evaluate an expression node and return its value."""
        return self._eval(node, self.global_env)

    def _exec_stmt(self, node, env):
        if node is None:
            return
        t = node.type

        if t == 'ExprStmt':
            self._eval(node.data['expr'], env)

        elif t == 'VarDecl':
            decl_env = self._declaration_env(env, node.data.get('kind'))
            for name, init in node.data['decls']:
                val = self._eval(init, env) if init else _UNDEF
                self._bind_target(env, name, val, define_env=decl_env)

        elif t == 'FuncDecl':
            name = node.data.get('name')
            fn = JSFunction(name, node.data['params'], node.data['body'], env)
            if name:
                env.define(name, fn)

        elif t == 'ClassDecl':
            cls = self._build_class(node, env)
            if node.data.get('name'):
                env.define(node.data['name'], cls)

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
                update = node.data.get('update')
                if update:
                    self._eval(update, loop_env)

        elif t == 'ForIn':
            iterable = self._eval(node.data['iterable'], env)
            target = node.data.get('target')
            name = node.data.get('name')
            decl_env = self._declaration_env(loop_env := Environment(env), node.data.get('kind'))
            loop_type = node.data.get('loop_type', 'in')
            if isinstance(iterable, dict):
                items = list(iterable.keys()) if loop_type == 'in' else list(iterable.values())
            elif isinstance(iterable, (list, tuple)):
                items = list(range(len(iterable))) if loop_type == 'in' else list(iterable)
            elif isinstance(iterable, str):
                items = list(range(len(iterable))) if loop_type == 'in' else list(iterable)
            else:
                items = []
            for item in items:
                if target is not None:
                    if isinstance(target, ASTNode) and target.type == 'Member':
                        obj = self._eval(target.data['obj'], loop_env)
                        prop = self._resolve_prop(target, loop_env)
                        _set_property(obj, prop, item)
                    else:
                        self._bind_target(loop_env, target, item, define_env=decl_env)
                elif name:
                    self._bind_target(loop_env, name, item, define_env=decl_env)
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
                    if not matched:
                        test = self._eval(case.data['test'], env)
                        if disc == test:
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

    def _eval(self, node, env):
        if node is None:
            return _UNDEF
        t = node.type

        if t == 'Literal':
            return node.data['value']

        if t == 'Ident':
            return env.get(node.data['name'])

        if t == 'This':
            return env.get('this') or self.global_env.get('window')

        if t == 'BinOp':
            op = node.data['op']
            # Short-circuit operators
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
            return _binop(op, left, right)

        if t == 'UnaryOp':
            op = node.data['op']
            if op == 'typeof':
                operand = node.data['operand']
                if operand.type == 'Ident':
                    val = env.get(operand.data['name'])
                else:
                    val = self._eval(operand, env)
                return _typeof(val)
            if op == 'await':
                return self._eval(node.data['operand'], env)
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
                return ~_to_int(_to_num(val))
            return _UNDEF

        if t == 'UpdatePre':
            return self._update(node.data['operand'], node.data['op'], True, env)

        if t == 'UpdatePost':
            return self._update(node.data['operand'], node.data['op'], False, env)

        if t == 'Assign':
            return self._assign(node, env)

        if t == 'Ternary':
            cond = self._eval(node.data['cond'], env)
            if _to_bool(cond):
                return self._eval(node.data['then'], env)
            return self._eval(node.data['else_'], env)

        if t == 'Call':
            return self._exec_call(node, env)

        if t == 'New':
            return self._exec_new(node, env)

        if t == 'Member':
            obj = self._eval(node.data['obj'], env)
            if node.data.get('optional') and (obj is None or obj is _UNDEF):
                return _UNDEF
            return self._member_get(obj, node, env)

        if t == 'Array':
            result = JSArray()
            for el in node.data['elements']:
                if isinstance(el, ASTNode) and el.type == 'Spread':
                    spread_val = self._eval(el.data['arg'], env)
                    if isinstance(spread_val, (list, tuple)):
                        result.extend(spread_val)
                    elif spread_val not in (_UNDEF, None):
                        result.append(spread_val)
                else:
                    result.append(self._eval(el, env))
            return result

        if t == 'Object':
            result = JSObject()
            for key, val_node in node.data['props']:
                if isinstance(key, ASTNode) and key.type == 'SpreadProp':
                    spread_val = self._eval(key.data['arg'], env)
                    if isinstance(spread_val, dict):
                        result.update(spread_val)
                    continue
                if isinstance(key, ASTNode):  # computed key
                    key = _to_str(self._eval(key.data['expr'], env))
                if isinstance(val_node, ASTNode) and val_node.type == 'Accessor':
                    fn = JSFunction(val_node.data.get('name'), val_node.data['params'], val_node.data['body'], env)
                    descriptor = result.get(key)
                    if not isinstance(descriptor, dict) or '__accessor__' not in descriptor:
                        descriptor = JSObject({'__accessor__': True, 'get': _UNDEF, 'set': _UNDEF})
                    descriptor[val_node.data['kind']] = fn
                    result[key] = descriptor
                    continue
                value = self._eval(val_node, env)
                if isinstance(value, JSFunction) and value.name == '(anonymous)' and isinstance(key, str):
                    value.name = key
                result[key] = value
            return result

        if t == 'FuncDecl':
            fn = JSFunction(node.data.get('name'), node.data['params'],
                            node.data['body'], env)
            if node.data.get('name'):
                env.define(node.data['name'], fn)
            return fn

        if t == 'ClassDecl':
            cls = self._build_class(node, env)
            if node.data.get('name') and not node.data.get('as_expr'):
                env.define(node.data['name'], cls)
            return cls

        if t == 'TemplateLiteral':
            return ''.join(_to_str(self._eval(part, env)) for part in node.data['nodes'])

        if t == 'Comma':
            self._eval(node.data['left'], env)
            return self._eval(node.data['right'], env)

        if t == 'Spread':
            return self._eval(node.data['arg'], env)

        return _UNDEF

    def _resolve_prop(self, member_node, env):
        if member_node.data.get('computed'):
            return _to_str(self._eval(member_node.data['prop'], env))
        return member_node.data['prop']

    def _member_get(self, obj, member_node, env):
        prop = self._resolve_prop(member_node, env)
        return _get_property(obj, prop)

    def _assign(self, node, env):
        op = node.data['op']
        target = node.data['left']
        right_val = self._eval(node.data['right'], env)

        if op != '=':
            old_val = self._eval(target, env)
            right_val = _binop(op[:-1], old_val, right_val)

        if target.type == 'Ident':
            env.set(target.data['name'], right_val)
        elif target.type == 'Member':
            obj = self._eval(target.data['obj'], env)
            prop = self._resolve_prop(target, env)
            _set_property(obj, prop, right_val)
        return right_val

    def _update(self, target, op, prefix, env):
        old_val = _to_num(self._eval(target, env))
        new_val = old_val + 1 if op == '++' else old_val - 1
        if target.type == 'Ident':
            env.set(target.data['name'], new_val)
        elif target.type == 'Member':
            obj = self._eval(target.data['obj'], env)
            prop = self._resolve_prop(target, env)
            _set_property(obj, prop, new_val)
        return new_val if prefix else old_val

    def _exec_call(self, node, env):
        callee_node = node.data['callee']
        args = self._eval_call_args(node.data['args'], env)

        this_val = None
        if callee_node.type == 'Member':
            this_val = self._eval(callee_node.data['obj'], env)
            if callee_node.data.get('optional') and (this_val is None or this_val is _UNDEF):
                return _UNDEF
            if isinstance(this_val, JSSuper):
                fn = _get_property(this_val.target, self._resolve_prop(callee_node, env))
                call_this = this_val.this_val
            else:
                fn = self._member_get(this_val, callee_node, env)
                call_this = this_val
        else:
            fn = self._eval(callee_node, env)
            if node.data.get('optional') and (fn is None or fn is _UNDEF):
                return _UNDEF
            if isinstance(fn, JSSuper):
                return self._call_value(fn.target, args, fn.this_val)
            call_this = this_val

        return self._call_value(fn, args, call_this)

    def _eval_call_args(self, arg_nodes, env):
        args = []
        for arg in arg_nodes:
            if isinstance(arg, ASTNode) and arg.type == 'Spread':
                spread_val = self._eval(arg.data['arg'], env)
                if isinstance(spread_val, (list, tuple)):
                    args.extend(spread_val)
                elif spread_val not in (_UNDEF, None):
                    args.append(spread_val)
            else:
                args.append(self._eval(arg, env))
        return args

    def _call_value(self, fn, args, this_val=None):
        if fn is _UNDEF or fn is None:
            return _UNDEF
        if callable(fn) and not isinstance(fn, JSFunction):
            try:
                return _call_python_callable(fn, args, this_val)
            except TypeError:
                return _UNDEF
            except Exception:
                return _UNDEF
        if isinstance(fn, JSFunction):
            if fn.is_class and this_val is None:
                return _UNDEF
            return self._call_function(fn, args, this_val)
        return _UNDEF

    def _call_function(self, fn, args, this_val=None):
        call_env = Environment(fn.env, is_function_scope=True)
        for i, param in enumerate(fn.params):
            self._bind_param(call_env, param, args, i)
        call_env.define('arguments', JSArray(args))
        if this_val is not None:
            call_env.define('this', this_val)
        super_target = None
        if fn.super_func is not None:
            super_target = fn.super_func
        elif fn.home_object is not None:
            super_target = getattr(fn.home_object, '_proto', None)
        if super_target is not None and this_val is not None:
            call_env.define('super', JSSuper(super_target, this_val))
        try:
            self._exec_stmt(fn.body, call_env)
        except _Return as ret:
            return ret.value
        return _UNDEF

    def _exec_new(self, node, env):
        callee = self._eval(node.data['callee'], env)
        args = self._eval_call_args(node.data.get('args', []), env)

        if callee is _UNDEF or callee is None:
            return JSObject()
        if callable(callee) and not isinstance(callee, JSFunction):
            try:
                return callee(*args)
            except Exception:
                return JSObject()
        if isinstance(callee, JSFunction):
            obj = JSObject()
            if callee.prototype is not None:
                obj._proto = callee.prototype
            call_env = Environment(callee.env, is_function_scope=True)
            for i, param in enumerate(callee.params):
                self._bind_param(call_env, param, args, i)
            call_env.define('this', obj)
            call_env.define('arguments', JSArray(args))
            if callee.super_func is not None:
                call_env.define('super', JSSuper(callee.super_func, obj))
            try:
                if callee.body is None and callee.super_func is not None:
                    self._call_value(callee.super_func, args, obj)
                else:
                    self._exec_stmt(callee.body, call_env)
            except _Return as ret:
                if isinstance(ret.value, dict):
                    return ret.value
            return obj
        return JSObject()

    def _build_class(self, node, env):
        super_class = self._eval(node.data.get('super_class'), env) if node.data.get('super_class') else None
        prototype = JSObject()
        class_props = {}
        if isinstance(super_class, JSFunction) and super_class.prototype is not None:
            prototype._proto = super_class.prototype
        ctor_method = None
        for method in node.data.get('methods', []):
            if method.data.get('kind') == 'constructor':
                ctor_method = method
                continue
            target = class_props if method.data.get('static') else prototype
            key = method.data['key']
            if method.data.get('computed'):
                key = _to_str(self._eval(key, env))
            target[key] = JSFunction(
                key if isinstance(key, str) else None,
                method.data['params'],
                method.data['body'],
                env,
                super_func=super_class if method.data.get('static') else None,
                home_object=target,
            )
        ctor_name = node.data.get('name')
        if ctor_method is None:
            ctor = JSFunction(
                ctor_name,
                [],
                None,
                env,
                prototype=prototype,
                is_class=True,
                super_func=super_class,
                class_props=class_props,
            )
        else:
            ctor = JSFunction(
                ctor_name,
                ctor_method.data['params'],
                ctor_method.data['body'],
                env,
                prototype=prototype,
                is_class=True,
                super_func=super_class,
                home_object=prototype,
                class_props=class_props,
            )
        prototype['constructor'] = ctor
        return ctor

    def _bind_param(self, env, param, args, index):
        if isinstance(param, str):
            env.define(param, args[index] if index < len(args) else _UNDEF)
            return
        if not isinstance(param, ASTNode) or param.type != 'Param':
            self._bind_target(env, param, args[index] if index < len(args) else _UNDEF)
            return
        if param.data.get('rest'):
            value = JSArray(args[index:]) if index < len(args) else JSArray()
        else:
            value = args[index] if index < len(args) else _UNDEF
        self._bind_target(env, param.data['target'], value, default_node=param.data.get('default'))

    def _bind_target(self, env, target, value, default_node=None, define_env=None):
        define_env = define_env or env
        if default_node is not None and value is _UNDEF:
            value = self._eval(default_node, env)
        if isinstance(target, str):
            define_env.define(target, value)
            return
        if not isinstance(target, ASTNode):
            return
        t = target.type
        if t == 'Ident':
            define_env.define(target.data['name'], value)
            return
        if t == 'DefaultPattern':
            if value is _UNDEF:
                value = self._eval(target.data['default'], env)
            self._bind_target(env, target.data['target'], value, define_env=define_env)
            return
        if t == 'ArrayPattern':
            items = value if isinstance(value, (list, tuple, str)) else []
            idx = 0
            for element in target.data['elements']:
                if element is None:
                    idx += 1
                    continue
                if isinstance(element, ASTNode) and element.type == 'RestPattern':
                    rest = JSArray(items[idx:]) if not isinstance(items, str) else JSArray(list(items[idx:]))
                    self._bind_target(env, element.data['target'], rest, define_env=define_env)
                    return
                current = items[idx] if idx < len(items) else _UNDEF
                self._bind_target(env, element, current, define_env=define_env)
                idx += 1
            return
        if t == 'ObjectPattern':
            source = value if isinstance(value, dict) else {}
            used = set()
            for key, prop_target in target.data['props']:
                if isinstance(key, ASTNode) and key.type == 'RestPattern':
                    rest_target = key.data['target']
                    rest_obj = JSObject()
                    for rest_key, rest_val in source.items():
                        if rest_key not in used:
                            rest_obj[rest_key] = rest_val
                    self._bind_target(env, rest_target, rest_obj, define_env=define_env)
                    continue
                used.add(key)
                self._bind_target(env, prop_target, source.get(key, _UNDEF), define_env=define_env)

    def _declaration_env(self, env, kind):
        if kind != 'var':
            return env
        current = env
        while current is not None and not current.is_function_scope:
            current = current.parent
        return current or env


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _to_str(v) -> str:
    if isinstance(v, JSSymbol):
        return repr(v)
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
    if callable(v):
        return 'function () { [native code] }'
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
        v = v.strip()
        if not v:
            return 0
        try:
            if v.startswith('0x') or v.startswith('0X'):
                return int(v, 16)
            return float(v) if '.' in v or 'e' in v.lower() else int(v)
        except ValueError:
            return float('nan')
    return float('nan')


def _to_bool(v) -> bool:
    if v is _UNDEF or v is None:
        return False
    if isinstance(v, JSSymbol):
        return True
    if isinstance(v, bool):
        return v
    if isinstance(v, (int, float)):
        return v != 0 and v == v  # NaN is falsy
    if isinstance(v, str):
        return len(v) > 0
    return True  # objects/arrays/functions are truthy


def _typeof(v) -> str:
    if v is _UNDEF:
        return 'undefined'
    if v is None:
        return 'object'
    if isinstance(v, JSSymbol):
        return 'symbol'
    if isinstance(v, bool):
        return 'boolean'
    if isinstance(v, (int, float)):
        return 'number'
    if isinstance(v, str):
        return 'string'
    if isinstance(v, JSFunction) or callable(v):
        return 'function'
    return 'object'


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
    if op == '<':
        return _to_num(left) < _to_num(right) if not (isinstance(left, str) and isinstance(right, str)) else left < right
    if op == '>':
        return _to_num(left) > _to_num(right) if not (isinstance(left, str) and isinstance(right, str)) else left > right
    if op == '<=':
        return _to_num(left) <= _to_num(right) if not (isinstance(left, str) and isinstance(right, str)) else left <= right
    if op == '>=':
        return _to_num(left) >= _to_num(right) if not (isinstance(left, str) and isinstance(right, str)) else left >= right
    if op == '<<':
        return _to_int(_to_num(left)) << (_to_int(_to_num(right)) & 31)
    if op == '>>':
        return _to_int(_to_num(left)) >> (_to_int(_to_num(right)) & 31)
    if op == '>>>':
        return (_to_int(_to_num(left)) & 0xFFFFFFFF) >> (_to_int(_to_num(right)) & 31)
    if op == '|':
        return _to_int(_to_num(left)) | _to_int(_to_num(right))
    if op == '&':
        return _to_int(_to_num(left)) & _to_int(_to_num(right))
    if op == '^':
        return _to_int(_to_num(left)) ^ _to_int(_to_num(right))
    if op == 'instanceof':
        if isinstance(right, JSFunction) and right.prototype is not None and hasattr(left, '_proto'):
            proto = getattr(left, '_proto', None)
            while proto is not None:
                if proto is right.prototype:
                    return True
                proto = getattr(proto, '_proto', None)
        if callable(right) and hasattr(right, 'prototype') and hasattr(left, '_proto'):
            target_proto = getattr(right, 'prototype', None)
            proto = getattr(left, '_proto', None)
            while proto is not None:
                if proto is target_proto:
                    return True
                proto = getattr(proto, '_proto', None)
        if isinstance(right, dict):
            rhs = right.get('__instanceof__')
            if isinstance(left, dict):
                lhs = left.get('__instanceof__')
                if lhs == rhs:
                    return True
                if rhs == 'HTMLElement' and lhs in ('HTMLElement', 'HTMLInputElement', 'HTMLImageElement'):
                    return True
                if rhs == 'Element' and lhs in ('HTMLElement', 'HTMLInputElement', 'HTMLImageElement'):
                    return True
                if rhs == 'Node' and lhs in ('Node', 'HTMLElement', 'HTMLInputElement', 'HTMLImageElement'):
                    return True
        return False
    if op == 'in':
        if isinstance(right, dict):
            return _to_str(left) in right
        return False
    return _UNDEF


def _loose_eq(a, b):
    if type(a) is type(b):
        return _strict_eq(a, b)
    if a is None and b is _UNDEF:
        return True
    if a is _UNDEF and b is None:
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


def _strict_eq(a, b):
    if a is _UNDEF and b is _UNDEF:
        return True
    if a is None and b is None:
        return True
    if isinstance(a, float) and a != a:
        return False  # NaN !== NaN
    return a is b if isinstance(a, (dict, list)) else a == b


def _get_property(obj, prop):
    """Get a property from a JS value, including built-in methods."""
    if obj is _UNDEF or obj is None:
        return _UNDEF

    if isinstance(obj, JSSuper):
        return _get_property(obj.target, prop)

    if isinstance(obj, JSFunction):
        if prop == 'call':
            return lambda this_arg=_UNDEF, *args: _invoke_callable(obj, list(args), this_arg)
        if prop == 'apply':
            return lambda this_arg=_UNDEF, args=None: _invoke_callable(obj, _coerce_call_args(args), this_arg)
        if prop == 'bind':
            return lambda this_arg=_UNDEF, *bound: _bind_callable(obj, this_arg, list(bound))
        if prop == 'prototype':
            return obj.prototype
        if prop == 'toString':
            return lambda: f'function {obj.name}() {{ [native code] }}'
        if prop in obj.class_props:
            return obj.class_props[prop]
        return _UNDEF

    if callable(obj):
        if prop == 'call':
            return lambda this_arg=_UNDEF, *args: _invoke_callable(obj, list(args), this_arg)
        if prop == 'apply':
            return lambda this_arg=_UNDEF, args=None: _invoke_callable(obj, _coerce_call_args(args), this_arg)
        if prop == 'bind':
            return lambda this_arg=_UNDEF, *bound: _bind_callable(obj, this_arg, list(bound))
        if prop == 'toString':
            return lambda: 'function () { [native code] }'

    # String methods
    if isinstance(obj, str):
        if prop == 'length':
            return len(obj)
        if prop == 'charAt':
            return lambda i=0: obj[_to_int(i)] if 0 <= _to_int(i) < len(obj) else ''
        if prop == 'charCodeAt':
            return lambda i=0: ord(obj[_to_int(i)]) if 0 <= _to_int(i) < len(obj) else float('nan')
        if prop == 'indexOf':
            return lambda s, start=0: obj.find(str(s), _to_int(start))
        if prop == 'lastIndexOf':
            return lambda s, start=None: obj.rfind(str(s)) if start is None else obj.rfind(str(s), 0, _to_int(start) + 1)
        if prop == 'slice':
            return lambda s=0, e=None: obj[_to_int(s):] if e is None else obj[_to_int(s):_to_int(e)]
        if prop == 'substring':
            return lambda s=0, e=None: obj[_to_int(s):] if e is None else obj[_to_int(s):_to_int(e)]
        if prop == 'substr':
            return lambda s=0, l=None: obj[_to_int(s):] if l is None else obj[_to_int(s):_to_int(s)+_to_int(l)]
        if prop == 'split':
            return lambda sep=_UNDEF, limit=None: obj.split(str(sep)) if sep is not _UNDEF else [obj]
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
            return lambda pat, rep: _string_replace(obj, pat, rep, replace_all=False)
        if prop == 'replaceAll':
            return lambda pat, rep: _string_replace(obj, pat, rep, replace_all=True)
        if prop == 'includes':
            return lambda s: str(s) in obj
        if prop == 'startsWith':
            return lambda s: obj.startswith(str(s))
        if prop == 'endsWith':
            return lambda s: obj.endswith(str(s))
        if prop == 'match':
            return lambda pat: None  # stub
        if prop == 'repeat':
            return lambda n: obj * _to_int(n)
        if prop == 'padStart':
            return lambda l, c=' ': obj.rjust(_to_int(l), str(c)[:1])
        if prop == 'padEnd':
            return lambda l, c=' ': obj.ljust(_to_int(l), str(c)[:1])
        if prop == 'Symbol(Symbol.iterator)':
            return lambda: _make_iterator(list(obj))
        # Numeric index
        try:
            idx = _to_int(prop, default=-1)
            if 0 <= idx < len(obj):
                return obj[idx]
        except (ValueError, TypeError):
            pass
        return _UNDEF

    if isinstance(obj, JSMap):
        if prop == 'Symbol(Symbol.iterator)':
            return lambda: obj.entries()
        if prop == 'constructor':
            return getattr(JSMap, 'prototype', JSObject()).get('constructor', _UNDEF)

    if isinstance(obj, JSSet):
        if prop == 'Symbol(Symbol.iterator)':
            return lambda: obj.values()
        if prop == 'constructor':
            return getattr(JSSet, 'prototype', JSObject()).get('constructor', _UNDEF)

    # Array methods
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
            return lambda sep=',': str(sep).join(_to_str(x) for x in obj)
        if prop == 'indexOf':
            return lambda v, start=0: _list_index_of(obj, v, _to_int(start))
        if prop == 'lastIndexOf':
            return lambda v: _list_last_index_of(obj, v)
        if prop == 'includes':
            return lambda v: v in obj
        if prop == 'slice':
            return lambda s=0, e=None: JSArray(obj[_to_int(s):] if e is None else obj[_to_int(s):_to_int(e)])
        if prop == 'splice':
            return lambda *a: _array_splice(obj, *a)
        if prop == 'concat':
            return lambda *a: JSArray(obj + sum((list(x) if isinstance(x, list) else [x] for x in a), []))
        if prop == 'reverse':
            return lambda: (obj.reverse(), JSArray(obj))[-1]
        if prop == 'sort':
            return lambda key=None: (obj.sort(), JSArray(obj))[-1]
        if prop == 'map':
            return lambda fn: JSArray(_safe_call(fn, [x, i, obj]) for i, x in enumerate(obj))
        if prop == 'filter':
            return lambda fn: JSArray(x for i, x in enumerate(obj) if _to_bool(_safe_call(fn, [x, i, obj])))
        if prop == 'forEach':
            def _forEach(fn):
                for i, x in enumerate(obj):
                    _safe_call(fn, [x, i, obj])
            return _forEach
        if prop == 'find':
            return lambda fn: next((x for i, x in enumerate(obj) if _to_bool(_safe_call(fn, [x, i, obj]))), _UNDEF)
        if prop == 'findIndex':
            return lambda fn: next((i for i, x in enumerate(obj) if _to_bool(_safe_call(fn, [x, i, obj]))), -1)
        if prop == 'every':
            return lambda fn: all(_to_bool(_safe_call(fn, [x, i, obj])) for i, x in enumerate(obj))
        if prop == 'some':
            return lambda fn: any(_to_bool(_safe_call(fn, [x, i, obj])) for i, x in enumerate(obj))
        if prop == 'reduce':
            return lambda fn, *init: _array_reduce(obj, fn, *init)
        if prop == 'flat':
            return lambda depth=1: JSArray(_flatten(obj, _to_int(depth, default=1)))
        if prop == 'fill':
            return lambda val, s=0, e=None: _array_fill(obj, val, _to_int(s), e)
        if prop == 'keys':
            return lambda: _make_iterator(list(range(len(obj))))
        if prop == 'values':
            return lambda: _make_iterator(list(obj))
        if prop == 'entries':
            return lambda: _make_iterator([JSArray([i, x]) for i, x in enumerate(obj)])
        if prop == 'Symbol(Symbol.iterator)':
            return lambda: _make_iterator(list(obj))
        if prop == 'toString':
            return lambda: ','.join(_to_str(x) for x in obj)
        if prop == 'constructor':
            return getattr(JSArray, 'prototype', JSObject()).get('constructor', _UNDEF)
        # Numeric index
        try:
            idx = _to_int(prop, default=-1)
            if (isinstance(prop, (int, float)) or (isinstance(prop, str) and prop.lstrip('-').isdigit())) and 0 <= idx < len(obj):
                return obj[idx]
        except (ValueError, TypeError):
            pass
        extra = getattr(obj, '__dict__', None)
        if extra and prop in extra:
            return extra[prop]
        return _UNDEF

    # Dict/Object
    if isinstance(obj, dict):
        # Use __getitem__ which may be overridden (e.g. DOMElement)
        try:
            val = obj[prop]
            if isinstance(val, dict) and val.get('__accessor__'):
                getter = val.get('get', _UNDEF)
                if isinstance(getter, JSFunction):
                    interp = Interpreter.__new__(Interpreter)
                    interp.global_env = getter.env
                    interp._iteration_count = 0
                    return interp._call_function(getter, [], obj)
                return _UNDEF
            if val is not _UNDEF:
                return val
        except (KeyError, IndexError):
            pass
        # Object built-ins
        if prop == 'hasOwnProperty':
            return lambda k: k in obj
        if prop == 'propertyIsEnumerable':
            return lambda k: _property_is_enumerable(obj, k)
        if prop == 'keys':
            return lambda: JSArray(_enumerable_own_keys(obj))
        if prop == 'values':
            return lambda: JSArray([_get_property(obj, key) for key in _enumerable_own_keys(obj)])
        proto = getattr(obj, '_proto', None)
        if proto is not None:
            return _get_property(proto, prop)
        return _UNDEF

    # Number methods
    if isinstance(obj, (int, float)):
        if prop == 'toString':
            return lambda base=10: _num_to_string(obj, _to_int(base, default=10))
        if prop == 'toFixed':
            return lambda d=0: f'{float(obj):.{_to_int(d)}f}'
        return _UNDEF

    if hasattr(obj, prop):
        try:
            return getattr(obj, prop)
        except Exception:
            return _UNDEF

    return _UNDEF


def _set_property(obj, prop, value):
    if isinstance(obj, JSFunction):
        if prop == 'prototype' and isinstance(value, dict):
            obj.prototype = value
            return
        obj.class_props[prop] = value
        return
    if isinstance(obj, dict):
        current = obj.get(prop)
        if isinstance(current, dict) and current.get('__accessor__'):
            setter = current.get('set', _UNDEF)
            if isinstance(setter, JSFunction):
                interp = Interpreter.__new__(Interpreter)
                interp.global_env = setter.env
                interp._iteration_count = 0
                interp._call_function(setter, [value], obj)
                return
        obj[prop] = value
    elif isinstance(obj, list):
        if prop == 'length':
            new_len = _to_int(value)
            while len(obj) > new_len:
                obj.pop()
            while len(obj) < new_len:
                obj.append(_UNDEF)
            return
        try:
            idx = _to_int(prop, default=-1)
            if (isinstance(prop, (int, float)) or (isinstance(prop, str) and prop.lstrip('-').isdigit())) and idx >= 0:
                while len(obj) <= idx:
                    obj.append(_UNDEF)
                obj[idx] = value
                return
        except (ValueError, TypeError):
            pass
        extra = getattr(obj, '__dict__', None)
        if extra is not None:
            extra[prop] = value


def _safe_call(fn, args):
    return _invoke_callable(fn, args)


def _invoke_callable(fn, args, this_val=None):
    if callable(fn) and not isinstance(fn, JSFunction):
        try:
            return _call_python_callable(fn, args, this_val)
        except Exception:
            return _UNDEF
    if isinstance(fn, JSFunction):
        interp = Interpreter.__new__(Interpreter)
        interp.global_env = fn.env
        interp._iteration_count = 0
        try:
            return interp._call_function(fn, args, this_val)
        except Exception:
            return _UNDEF
    return _UNDEF


def _coerce_call_args(args):
    if args in (None, _UNDEF):
        return []
    if isinstance(args, (list, tuple, JSArray)):
        return list(args)
    return [args]


def _bind_callable(fn, this_arg, bound_args):
    return lambda *rest: _invoke_callable(fn, bound_args + list(rest), this_arg)


def _call_python_callable(fn, args, this_val=None):
    if this_val is not None and getattr(fn, '_expects_this', False):
        return fn(this_val, *args)
    return fn(*args)


def _native_method(fn):
    setattr(fn, '_expects_this', True)
    return fn


def _safe_call_legacy_stub(fn, args):
    if callable(fn) and not isinstance(fn, JSFunction):
        try:
            return fn(*args)
        except Exception:
            return _UNDEF
    if isinstance(fn, JSFunction):
        # Need interpreter — but we're in a helper. Use a simple approach.
        call_env = Environment(fn.env)
        for i, p in enumerate(fn.params):
            call_env.define(p, args[i] if i < len(args) else _UNDEF)
        call_env.define('arguments', JSArray(args))
        interp = Interpreter.__new__(Interpreter)
        interp.global_env = fn.env
        interp._iteration_count = 0
        try:
            interp._exec_stmt(fn.body, call_env)
        except _Return as ret:
            return ret.value
        except Exception:
            pass
        return _UNDEF
    return _UNDEF


def _array_constructor(*values):
    if len(values) == 1 and isinstance(values[0], (int, float)):
        length = max(0, _to_int(values[0]))
        return JSArray([_UNDEF] * length)
    return JSArray(values)


def _object_constructor(value=_UNDEF):
    if value in (_UNDEF, None):
        return JSObject()
    if isinstance(value, dict):
        return value
    if isinstance(value, (list, tuple, JSArray)):
        obj = JSObject()
        for idx, item in enumerate(value):
            obj[str(idx)] = item
        return obj
    return JSObject({'value': value})


def _object_assign(target, *sources):
    if not isinstance(target, dict):
        target = _object_constructor(target)
    for source in sources:
        if not isinstance(source, dict):
            continue
        for key in _enumerable_own_keys(source):
            value = _get_property(source, key)
            target[key] = value
    return target


def _descriptor_store(obj, create=False):
    meta = getattr(obj, '__dict__', None)
    if meta is None:
        return None
    store = meta.get(_DESCRIPTOR_STORE_ATTR)
    if store is None and create:
        store = {}
        meta[_DESCRIPTOR_STORE_ATTR] = store
    return store


def _property_key(prop):
    return _to_str(prop)


def _descriptor_for(obj, key):
    store = _descriptor_store(obj)
    if store is not None and key in store:
        return store[key]
    if isinstance(obj, list):
        if key == 'length':
            return {'enumerable': False, 'configurable': False, 'writable': True, 'value': len(obj)}
        idx = _to_int(key, default=-1)
        if idx >= 0 and idx < len(obj) and (isinstance(key, (int, float)) or (isinstance(key, str) and key.lstrip('-').isdigit())):
            return {'enumerable': True, 'configurable': True, 'writable': True, 'value': obj[idx]}
    if isinstance(obj, dict) and key in obj:
        value = obj[key]
        if isinstance(value, dict) and value.get('__accessor__'):
            return {
                'enumerable': value.get('enumerable', False),
                'configurable': value.get('configurable', False),
                'get': value.get('get', _UNDEF),
                'set': value.get('set', _UNDEF),
            }
        return {'enumerable': True, 'configurable': True, 'writable': True, 'value': value}
    extra = getattr(obj, '__dict__', None)
    if extra and key in extra and key != _DESCRIPTOR_STORE_ATTR:
        return {'enumerable': True, 'configurable': True, 'writable': True, 'value': extra[key]}
    return None


def _property_is_enumerable(obj, prop):
    desc = _descriptor_for(obj, _property_key(prop))
    return bool(desc and desc.get('enumerable', False))


def _own_property_names(obj):
    if isinstance(obj, dict):
        keys = list(obj.keys())
    elif isinstance(obj, list):
        keys = [str(i) for i in range(len(obj))] + ['length']
    else:
        keys = []
    extra = getattr(obj, '__dict__', None)
    if extra:
        for key in extra.keys():
            if key == _DESCRIPTOR_STORE_ATTR:
                continue
            if key not in keys:
                keys.append(key)
    return [_to_str(key) for key in keys]


def _enumerable_own_keys(obj):
    return [key for key in _own_property_names(obj) if _property_is_enumerable(obj, key)]


def _object_create(proto=None, props=_UNDEF):
    obj = JSObject()
    if proto is None or isinstance(proto, dict):
        obj._proto = proto
    if isinstance(props, dict):
        _object_define_properties(obj, props)
    return obj


def _object_define_property(obj, prop, desc):
    if not isinstance(obj, (dict, list)):
        raise TypeError(f'{_to_str(obj)} is not an object')
    key = _property_key(prop)
    if not isinstance(desc, dict):
        raise TypeError(f'{_to_str(desc)} is not an object')

    store = _descriptor_store(obj, create=True)
    descriptor = {
        'enumerable': _to_bool(desc.get('enumerable', False)),
        'configurable': _to_bool(desc.get('configurable', False)),
    }

    if 'get' in desc or 'set' in desc:
        getter = desc.get('get', _UNDEF)
        setter = desc.get('set', _UNDEF)
        accessor = JSObject({
            '__accessor__': True,
            'get': getter,
            'set': setter,
            'enumerable': descriptor['enumerable'],
            'configurable': descriptor['configurable'],
        })
        if isinstance(obj, dict):
            obj[key] = accessor
        else:
            extra = getattr(obj, '__dict__', None)
            if extra is not None:
                extra[key] = accessor
        descriptor['get'] = getter
        descriptor['set'] = setter
        store[key] = descriptor
        return obj

    value = desc.get('value', _UNDEF)
    descriptor['writable'] = _to_bool(desc.get('writable', False))
    descriptor['value'] = value
    _set_property(obj, key, value)
    store[key] = descriptor
    return obj


def _object_define_properties(obj, props):
    if not isinstance(props, dict):
        return obj
    for key in list(props.keys()):
        _object_define_property(obj, key, props[key])
    return obj


def _object_get_own_property_descriptor(obj, prop):
    if not isinstance(obj, (dict, list)):
        return _UNDEF
    desc = _descriptor_for(obj, _property_key(prop))
    if desc is None:
        return _UNDEF
    return JSObject(desc)


def _object_get_prototype_of(obj):
    if isinstance(obj, dict):
        return getattr(obj, '_proto', None)
    if isinstance(obj, list):
        return getattr(obj, '_proto', None)
    return _UNDEF


def _object_set_prototype_of(obj, proto):
    if isinstance(obj, (dict, list)):
        setattr(obj, '_proto', proto if isinstance(proto, dict) or proto is None else None)
    return obj


def _make_iterator(items):
    idx = {'value': 0}
    iterator = JSObject()

    def _next():
        cur = idx['value']
        if cur >= len(items):
            return JSObject({'done': True, 'value': _UNDEF})
        idx['value'] = cur + 1
        return JSObject({'done': False, 'value': items[cur]})

    iterator['next'] = _next
    iterator['Symbol(Symbol.iterator)'] = lambda: iterator
    return iterator


def _iterable_to_list(value):
    if value in (_UNDEF, None):
        return []
    if isinstance(value, (list, tuple, JSArray)):
        return list(value)
    if isinstance(value, str):
        return list(value)
    iterator_factory = _get_property(value, 'Symbol(Symbol.iterator)')
    if callable(iterator_factory) or isinstance(iterator_factory, JSFunction):
        iterator = _invoke_callable(iterator_factory, [], value)
        if isinstance(iterator, dict):
            items = []
            next_fn = _get_property(iterator, 'next')
            while callable(next_fn) or isinstance(next_fn, JSFunction):
                step = _invoke_callable(next_fn, [], iterator)
                if not isinstance(step, dict):
                    break
                if _to_bool(step.get('done')):
                    break
                items.append(step.get('value', _UNDEF))
            return items
    if isinstance(value, dict):
        return list(value.values())
    return [value]


def _array_from(value, map_fn=_UNDEF, this_arg=_UNDEF):
    result = JSArray()
    for idx, item in enumerate(_iterable_to_list(value)):
        mapped = item
        if map_fn not in (_UNDEF, None):
            mapped = _invoke_callable(map_fn, [item, idx], this_arg if this_arg is not _UNDEF else None)
        result.append(mapped)
    return result


def _parse_int(s, radix=10):
    try:
        s = str(s).strip()
        if not s:
            return float('nan')
        if s.startswith('0x') or s.startswith('0X'):
            return int(s, 16)
        return int(s, int(radix) if radix else 10)
    except (ValueError, TypeError):
        # Try to parse leading digits
        import re
        m = re.match(r'[+-]?\d+', str(s).strip())
        if m:
            return int(m.group(0))
        return float('nan')


def _parse_float(s):
    try:
        return float(str(s).strip())
    except (ValueError, TypeError):
        import re
        m = re.match(r'[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?', str(s).strip())
        if m:
            return float(m.group(0))
        return float('nan')


def _parse_regex_literal(value):
    if not isinstance(value, str) or len(value) < 2 or not value.startswith('/'):
        return None
    last = value.rfind('/')
    if last <= 0:
        return None
    return value[1:last], value[last + 1:]


def _string_replace(text, pat, rep, replace_all=False):
    regex = _parse_regex_literal(pat)
    if regex is None:
        old = str(pat)
        new = _to_str(rep)
        return text.replace(old, new) if replace_all else text.replace(old, new, 1)
    pattern, flags = regex
    re_flags = 0
    if 'i' in flags:
        re_flags |= re.I
    if 'm' in flags:
        re_flags |= re.M
    if 's' in flags:
        re_flags |= re.S
    count = 0 if replace_all or 'g' in flags else 1
    return re.sub(pattern, _to_str(rep), text, count=count, flags=re_flags)


def _to_int(v, default: int = 0) -> int:
    try:
        n = _to_num(v)
        if isinstance(n, float) and (n != n or n in (float('inf'), float('-inf'))):
            return default
        return int(n)
    except Exception:
        return default


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
    s = _to_int(start)
    if s < 0:
        s = max(0, len(arr) + s)
    dc = _to_int(delete_count) if delete_count is not None else len(arr) - s
    removed = JSArray(arr[s:s + dc])
    arr[s:s + dc] = list(items)
    return removed


def _array_reduce(arr, fn, *init):
    it = iter(enumerate(arr))
    if init:
        acc = init[0]
    else:
        try:
            i, acc = next(it)
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
    e = len(arr) if end is None else _to_int(end)
    for i in range(start, min(e, len(arr))):
        arr[i] = val
    return JSArray(arr)


def _num_to_string(n, base=10):
    if base == 10:
        return _to_str(n)
    if base == 16:
        return hex(int(n))[2:]
    if base == 2:
        return bin(int(n))[2:]
    if base == 8:
        return oct(int(n))[2:]
    return str(int(n))


def _js_to_python(v):
    """Convert JS value to Python-native for JSON.stringify."""
    if v is _UNDEF:
        return None
    if isinstance(v, JSFunction):
        return None
    if isinstance(v, JSArray):
        return [_js_to_python(x) for x in v]
    if isinstance(v, dict):
        return {k: _js_to_python(val) for k, val in v.items()}
    return v


class _JSDate:
    """Minimal Date constructor."""
    def __init__(self, *args):
        import time
        self._ts = time.time() * 1000
    def getTime(self):
        return self._ts
    def toString(self):
        return str(self._ts)
