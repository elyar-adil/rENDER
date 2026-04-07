"""Core JavaScript runtime types for rENDER browser engine."""
from __future__ import annotations



class _Undefined:
    """Sentinel for JavaScript undefined."""
    _instance = None

    def __new__(cls):
        if cls._instance is None:
            cls._instance = super().__new__(cls)
        return cls._instance

    def __repr__(self):
        return 'undefined'

    def __bool__(self):
        return False

    def __str__(self):
        return 'undefined'


_UNDEF = _Undefined()


class Environment:
    """Lexical scope chain for variable lookup."""
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
        """Assign to the nearest enclosing scope that owns *name*."""
        env = self
        while env is not None:
            if name in env.bindings:
                env.bindings[name] = value
                return
            env = env.parent
        self.bindings[name] = value

    def define(self, name, value):
        """Declare in the current scope."""
        self.bindings[name] = value


class JSObject(dict):
    """A JavaScript plain object (dict subclass with prototype link)."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self._proto = None


class JSArray(list):
    """A JavaScript array (list subclass)."""


class JSFunction:
    """A JavaScript function value (closure)."""

    def __init__(self, name, params, body, env):
        self.name = name or '(anonymous)'
        self.params = params          # list[str]
        self.body = body              # ASTNode (Block)
        self.env = env                # Environment (closure)
        self.properties = JSObject()  # own properties (.prototype, etc.)

    def __repr__(self):
        return f'function {self.name}()'
