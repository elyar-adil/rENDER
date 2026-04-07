"""Core JavaScript runtime types for rENDER browser engine."""
from typing import Any


class _Undefined:
    """Sentinel for JavaScript undefined."""
    _instance: Any = None

    def __new__(cls: type) -> "_Undefined":
        if cls._instance is None:
            cls._instance = super().__new__(cls)
        return cls._instance

    def __repr__(self) -> str:
        return 'undefined'

    def __bool__(self) -> bool:
        return False

    def __str__(self) -> str:
        return 'undefined'


_UNDEF = _Undefined()


class Environment:
    """Lexical scope chain for variable lookup."""
    bindings: dict
    parent: Any

    def __init__(self, parent: Any = None) -> None:
        self.bindings = {}
        self.parent = parent

    def get(self, name: str) -> Any:
        if name in self.bindings:
            return self.bindings[name]
        if self.parent:
            return self.parent.get(name)
        return _UNDEF

    def set(self, name: str, value: Any) -> None:
        """Assign to the nearest enclosing scope that owns *name*."""
        env: Any = self
        while env is not None:
            if name in env.bindings:
                env.bindings[name] = value
                return
            env = env.parent
        self.bindings[name] = value

    def define(self, name: str, value: Any) -> None:
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
    name: str
    params: list
    body: Any
    env: Any
    properties: Any

    def __init__(self, name: Any, params: list, body: Any, env: Any) -> None:
        self.name = name or '(anonymous)'
        self.params = params
        self.body = body
        self.env = env
        self.properties = JSObject()

    def __repr__(self) -> str:
        return f'function {self.name}()'
