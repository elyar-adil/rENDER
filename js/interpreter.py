"""JavaScript tree-walking interpreter for rENDER browser engine."""
import math
import json
import re
from js.parser import _UNDEF, ASTNode


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
    __slots__ = ('bindings', 'parent')

    def __init__(self, parent=None):
        self.bindings = {}
        self.parent = parent

    def get(self, name):
        if name in self.bindings:
            return self.bindings[name]
        if self.parent:
            return self.parent.get(name)
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

    def define(self, name, value):
        """Define in current scope."""
        self.bindings[name] = value


class JSFunction:
    """A JavaScript function (closure)."""
    __slots__ = ('name', 'params', 'body', 'env')

    def __init__(self, name, params, body, env):
        self.name = name or '(anonymous)'
        self.params = params
        self.body = body
        self.env = env

    def __repr__(self):
        return f'function {self.name}()'


class JSObject(dict):
    """A JavaScript object (plain dict with prototype stub)."""
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self._proto = None


class JSArray(list):
    """A JavaScript array."""
    pass


class Interpreter:
    """JavaScript tree-walking interpreter."""

    MAX_ITERATIONS = 100000  # safety limit for loops

    def __init__(self):
        self.global_env = Environment()
        self._setup_globals()
        self._iteration_count = 0

    def _setup_globals(self):
        g = self.global_env

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

        # Global functions
        g.define('parseInt', lambda s, r=10: _parse_int(s, r))
        g.define('parseFloat', lambda s: _parse_float(s))
        g.define('isNaN', lambda v: v != v if isinstance(v, float) else False)
        g.define('isFinite', lambda v: isinstance(v, (int, float)) and math.isfinite(v))
        g.define('encodeURIComponent', lambda s: __import__('urllib.parse', fromlist=['quote']).quote(str(s), safe=''))
        g.define('decodeURIComponent', lambda s: __import__('urllib.parse', fromlist=['unquote']).unquote(str(s)))
        g.define('encodeURI', lambda s: __import__('urllib.parse', fromlist=['quote']).quote(str(s), safe=':/?#[]@!$&\'()*+,;=-._~'))
        g.define('decodeURI', lambda s: __import__('urllib.parse', fromlist=['unquote']).unquote(str(s)))
        g.define('String', lambda v=_UNDEF: '' if v is _UNDEF else _to_str(v))
        g.define('Number', lambda v=0: _to_num(v))
        g.define('Boolean', lambda v=False: _to_bool(v))
        g.define('Array', JSArray)
        g.define('Object', JSObject)
        g.define('RegExp', lambda p, f='': re.compile(p))
        g.define('setTimeout', lambda fn, ms=0, *a: self._set_timeout(fn, ms, *a))
        g.define('setInterval', lambda fn, ms=0, *a: None)
        g.define('clearTimeout', lambda tid: None)
        g.define('clearInterval', lambda tid: None)
        g.define('alert', lambda msg='': None)
        g.define('confirm', lambda msg='': False)
        g.define('prompt', lambda msg='', d='': d)
        g.define('undefined', _UNDEF)
        g.define('NaN', float('nan'))
        g.define('Infinity', float('inf'))
        g.define('Error', lambda msg='': JSObject({'message': str(msg)}))
        g.define('TypeError', lambda msg='': JSObject({'message': str(msg)}))
        g.define('Date', _JSDate)

        # window = global
        window = JSObject()
        window['document'] = _UNDEF  # will be set by DOMBinding
        window['location'] = JSObject({'href': '', 'hostname': '', 'pathname': '/'})
        window['navigator'] = JSObject({'userAgent': 'rENDER/1.0'})
        window['innerWidth'] = 980
        window['innerHeight'] = 600
        window['setTimeout'] = g.get('setTimeout')
        window['setInterval'] = g.get('setInterval')
        window['clearTimeout'] = g.get('clearTimeout')
        window['clearInterval'] = g.get('clearInterval')
        window['addEventListener'] = lambda *a: None
        window['removeEventListener'] = lambda *a: None
        window['getComputedStyle'] = lambda el, *a: JSObject()
        g.define('window', window)
        g.define('self', window)
        g.define('globalThis', window)

    def _console_log(self, *args):
        parts = [_to_str(a) for a in args]
        print('[JS]', ' '.join(parts))

    def _set_timeout(self, fn, ms, *args):
        """Execute setTimeout(fn, 0) immediately; ignore delays > 0."""
        if isinstance(ms, (int, float)) and ms <= 0:
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

    def execute(self, ast) -> None:
        """Execute a Program AST node."""
        if ast is None:
            return
        if ast.type == 'Program':
            for stmt in ast.data.get('body', []):
                self._exec_stmt(stmt, self.global_env)

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
            for name, init in node.data['decls']:
                val = self._eval(init, env) if init else _UNDEF
                env.define(name, val)

        elif t == 'FuncDecl':
            name = node.data.get('name')
            fn = JSFunction(name, node.data['params'], node.data['body'], env)
            if name:
                env.define(name, fn)

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
            name = node.data['name']
            loop_type = node.data.get('loop_type', 'in')
            loop_env = Environment(env)
            if isinstance(iterable, dict):
                items = list(iterable.keys()) if loop_type == 'in' else list(iterable.values())
            elif isinstance(iterable, (list, tuple)):
                items = list(range(len(iterable))) if loop_type == 'in' else list(iterable)
            elif isinstance(iterable, str):
                items = list(range(len(iterable))) if loop_type == 'in' else list(iterable)
            else:
                items = []
            for item in items:
                loop_env.define(name, item)
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
            return self._member_get(obj, node, env)

        if t == 'Array':
            return JSArray(self._eval(el, env) for el in node.data['elements'])

        if t == 'Object':
            result = JSObject()
            for key, val_node in node.data['props']:
                if isinstance(key, ASTNode):  # computed key
                    key = _to_str(self._eval(key.data['expr'], env))
                result[key] = self._eval(val_node, env)
            return result

        if t == 'FuncDecl':
            fn = JSFunction(node.data.get('name'), node.data['params'],
                            node.data['body'], env)
            if node.data.get('name'):
                env.define(node.data['name'], fn)
            return fn

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
        args = [self._eval(a, env) for a in node.data['args']]

        this_val = None
        if callee_node.type == 'Member':
            this_val = self._eval(callee_node.data['obj'], env)
            fn = self._member_get(this_val, callee_node, env)
        else:
            fn = self._eval(callee_node, env)

        return self._call_value(fn, args, this_val)

    def _call_value(self, fn, args, this_val=None):
        if fn is _UNDEF or fn is None:
            return _UNDEF
        if callable(fn) and not isinstance(fn, JSFunction):
            try:
                return fn(*args)
            except TypeError:
                return _UNDEF
            except Exception:
                return _UNDEF
        if isinstance(fn, JSFunction):
            return self._call_function(fn, args, this_val)
        return _UNDEF

    def _call_function(self, fn, args, this_val=None):
        call_env = Environment(fn.env)
        for i, param in enumerate(fn.params):
            call_env.define(param, args[i] if i < len(args) else _UNDEF)
        call_env.define('arguments', JSArray(args))
        if this_val is not None:
            call_env.define('this', this_val)
        try:
            self._exec_stmt(fn.body, call_env)
        except _Return as ret:
            return ret.value
        return _UNDEF

    def _exec_new(self, node, env):
        callee = self._eval(node.data['callee'], env)
        args = [self._eval(a, env) for a in node.data.get('args', [])]

        if callee is _UNDEF or callee is None:
            return JSObject()
        if callable(callee) and not isinstance(callee, JSFunction):
            try:
                return callee(*args)
            except Exception:
                return JSObject()
        if isinstance(callee, JSFunction):
            obj = JSObject()
            call_env = Environment(callee.env)
            for i, param in enumerate(callee.params):
                call_env.define(param, args[i] if i < len(args) else _UNDEF)
            call_env.define('this', obj)
            call_env.define('arguments', JSArray(args))
            try:
                self._exec_stmt(callee.body, call_env)
            except _Return as ret:
                if isinstance(ret.value, dict):
                    return ret.value
            return obj
        return JSObject()


# ---------------------------------------------------------------------------
# Helpers
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

    # String methods
    if isinstance(obj, str):
        if prop == 'length':
            return len(obj)
        if prop == 'charAt':
            return lambda i=0: obj[int(i)] if 0 <= int(i) < len(obj) else ''
        if prop == 'charCodeAt':
            return lambda i=0: ord(obj[int(i)]) if 0 <= int(i) < len(obj) else float('nan')
        if prop == 'indexOf':
            return lambda s, start=0: obj.find(str(s), int(start))
        if prop == 'lastIndexOf':
            return lambda s, start=None: obj.rfind(str(s)) if start is None else obj.rfind(str(s), 0, int(start) + 1)
        if prop == 'slice':
            return lambda s=0, e=None: obj[int(s):] if e is None else obj[int(s):int(e)]
        if prop == 'substring':
            return lambda s=0, e=None: obj[int(s):] if e is None else obj[int(s):int(e)]
        if prop == 'substr':
            return lambda s=0, l=None: obj[int(s):] if l is None else obj[int(s):int(s)+int(l)]
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
            return lambda pat, rep: obj.replace(str(pat), str(rep), 1)
        if prop == 'replaceAll':
            return lambda pat, rep: obj.replace(str(pat), str(rep))
        if prop == 'includes':
            return lambda s: str(s) in obj
        if prop == 'startsWith':
            return lambda s: obj.startswith(str(s))
        if prop == 'endsWith':
            return lambda s: obj.endswith(str(s))
        if prop == 'match':
            return lambda pat: None  # stub
        if prop == 'repeat':
            return lambda n: obj * int(n)
        if prop == 'padStart':
            return lambda l, c=' ': obj.rjust(int(l), str(c)[:1])
        if prop == 'padEnd':
            return lambda l, c=' ': obj.ljust(int(l), str(c)[:1])
        # Numeric index
        try:
            idx = int(prop)
            if 0 <= idx < len(obj):
                return obj[idx]
        except (ValueError, TypeError):
            pass
        return _UNDEF

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
            return lambda v, start=0: _list_index_of(obj, v, int(start))
        if prop == 'lastIndexOf':
            return lambda v: _list_last_index_of(obj, v)
        if prop == 'includes':
            return lambda v: v in obj
        if prop == 'slice':
            return lambda s=0, e=None: JSArray(obj[int(s):] if e is None else obj[int(s):int(e)])
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
            return lambda depth=1: JSArray(_flatten(obj, int(depth)))
        if prop == 'fill':
            return lambda val, s=0, e=None: _array_fill(obj, val, int(s), e)
        # Numeric index
        try:
            idx = int(prop)
            if 0 <= idx < len(obj):
                return obj[idx]
        except (ValueError, TypeError):
            pass
        return _UNDEF

    # Dict/Object
    if isinstance(obj, dict):
        # Use __getitem__ which may be overridden (e.g. DOMElement)
        try:
            val = obj[prop]
            if val is not _UNDEF:
                return val
        except (KeyError, IndexError):
            pass
        # Object built-ins
        if prop == 'hasOwnProperty':
            return lambda k: k in obj
        if prop == 'keys':
            return lambda: JSArray(obj.keys())
        if prop == 'values':
            return lambda: JSArray(obj.values())
        return _UNDEF

    # Number methods
    if isinstance(obj, (int, float)):
        if prop == 'toString':
            return lambda base=10: _num_to_string(obj, int(base))
        if prop == 'toFixed':
            return lambda d=0: f'{float(obj):.{int(d)}f}'
        return _UNDEF

    return _UNDEF


def _set_property(obj, prop, value):
    if isinstance(obj, dict):
        obj[prop] = value
    elif isinstance(obj, list):
        try:
            idx = int(prop)
            while len(obj) <= idx:
                obj.append(_UNDEF)
            obj[idx] = value
        except (ValueError, TypeError):
            if prop == 'length':
                new_len = int(value)
                while len(obj) > new_len:
                    obj.pop()
                while len(obj) < new_len:
                    obj.append(_UNDEF)


def _safe_call(fn, args):
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
    e = len(arr) if end is None else int(end)
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
