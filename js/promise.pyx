"""Promise and microtask queue helpers for the rENDER browser engine."""
from __future__ import annotations


from js.event_loop import get_event_loop
from js.types import _UNDEF, JSObject, JSArray, JSFunction


def _enqueue_microtask(fn) -> None:
    get_event_loop().enqueue_microtask(fn)


def drain_microtasks() -> None:
    """Run a microtask checkpoint (re-entrant safe)."""
    get_event_loop().perform_microtask_checkpoint()


# ---------------------------------------------------------------------------
# JSPromise
# ---------------------------------------------------------------------------

class JSPromise(JSObject):
    """JavaScript Promise.

    Settled promises run then/catch handlers inline via the microtask queue.
    Pending promises queue handlers until the promise settles.
    """

    PENDING = 'pending'
    FULFILLED = 'fulfilled'
    REJECTED = 'rejected'

    def __init__(self, executor=None, _interp=None):
        super().__init__()
        self._state = self.PENDING
        self._value = _UNDEF
        self._handlers: list = []  # list of (on_f, on_r, child)
        self._interp = _interp

        # Expose as JS methods
        self['then'] = lambda on_f=_UNDEF, on_r=_UNDEF: self._then(
            None if on_f is _UNDEF else on_f,
            None if on_r is _UNDEF else on_r,
        )
        self['catch'] = lambda on_r=_UNDEF: self._then(
            None, None if on_r is _UNDEF else on_r
        )
        self['finally'] = lambda fn=_UNDEF: self._finally(
            None if fn is _UNDEF else fn
        )

        if executor is not None:
            try:
                self._call(executor, [self._resolve, self._reject])
            except Exception as exc:
                self._reject(exc)

    # ------------------------------------------------------------------
    # Internal state machine
    # ------------------------------------------------------------------

    def _resolve(self, value=_UNDEF):
        if self._state != self.PENDING:
            return
        # Unwrap thenables
        if isinstance(value, JSPromise):
            value._then(self._resolve, self._reject)
            return
        if isinstance(value, dict):
            then_fn = value.get('then', _UNDEF)
            if then_fn not in (_UNDEF, None) and callable(then_fn):
                try:
                    then_fn(self._resolve, self._reject)
                    return
                except Exception as exc:
                    self._reject(exc)
                    return
        self._state = self.FULFILLED
        self._value = value
        self._settle()

    def _reject(self, reason=_UNDEF):
        if self._state != self.PENDING:
            return
        self._state = self.REJECTED
        self._value = reason
        self._settle()

    def _settle(self):
        handlers = self._handlers[:]
        self._handlers = []
        for entry in handlers:
            _enqueue_microtask(lambda e=entry: self._run_handler(*e))

    def _run_handler(self, on_f, on_r, child):
        handler = on_f if self._state == self.FULFILLED else on_r
        if handler is None:
            # pass-through
            if self._state == self.FULFILLED:
                child._resolve(self._value)
            else:
                child._reject(self._value)
            return
        try:
            result = self._call(handler, [self._value])
            child._resolve(result)
        except Exception as exc:
            child._reject(str(exc))

    def _then(self, on_fulfill=None, on_reject=None):
        child = JSPromise(_interp=self._interp)
        if self._state == self.PENDING:
            self._handlers.append((on_fulfill, on_reject, child))
        else:
            _enqueue_microtask(lambda: self._run_handler(on_fulfill, on_reject, child))
        return child

    def _finally(self, fn):
        def wrap_f(value):
            self._call(fn, [])
            return value
        def wrap_r(reason):
            self._call(fn, [])
            raise Exception(reason)
        return self._then(wrap_f if fn is not None else None,
                          wrap_r if fn is not None else None)

    # ------------------------------------------------------------------
    # Helper: call a JS or Python callable through the interpreter
    # ------------------------------------------------------------------

    def _call(self, fn, args):
        if fn is None or fn is _UNDEF:
            return _UNDEF
        if self._interp is not None:
            return self._interp._call_value(fn, list(args))
        if callable(fn):
            return fn(*args)
        return _UNDEF

    # ------------------------------------------------------------------
    # Static factory methods
    # ------------------------------------------------------------------

    @classmethod
    def resolve(cls, value=_UNDEF, _interp=None):
        if isinstance(value, JSPromise):
            return value
        p = cls(_interp=_interp)
        p._resolve(value)
        return p

    @classmethod
    def reject(cls, reason=_UNDEF, _interp=None):
        p = cls(_interp=_interp)
        p._reject(reason)
        return p

    @classmethod
    def all(cls, promises, _interp=None):
        items = _to_list(promises)
        if not items:
            return cls.resolve(JSArray(), _interp=_interp)
        result = cls(_interp=_interp)
        results = [_UNDEF] * len(items)
        remaining = [len(items)]
        for i, p in enumerate(items):
            p = _ensure_promise(p, _interp)
            def _make_ful(idx):
                def on_ful(v):
                    results[idx] = v
                    remaining[0] -= 1
                    if remaining[0] == 0:
                        result._resolve(JSArray(results))
                return on_ful
            p._then(_make_ful(i), result._reject)
        return result

    @classmethod
    def allSettled(cls, promises, _interp=None):
        items = _to_list(promises)
        if not items:
            return cls.resolve(JSArray(), _interp=_interp)
        result = cls(_interp=_interp)
        results = [_UNDEF] * len(items)
        remaining = [len(items)]
        def _make_handler(idx, fulfilled):
            def handler(value):
                if fulfilled:
                    results[idx] = JSObject({'status': 'fulfilled', 'value': value})
                else:
                    results[idx] = JSObject({'status': 'rejected', 'reason': value})
                remaining[0] -= 1
                if remaining[0] == 0:
                    result._resolve(JSArray(results))
            return handler
        for i, p in enumerate(items):
            p = _ensure_promise(p, _interp)
            p._then(_make_handler(i, True), _make_handler(i, False))
        return result

    @classmethod
    def race(cls, promises, _interp=None):
        result = cls(_interp=_interp)
        for p in _to_list(promises):
            _ensure_promise(p, _interp)._then(result._resolve, result._reject)
        return result

    @classmethod
    def any(cls, promises, _interp=None):
        items = _to_list(promises)
        if not items:
            return cls.reject(JSObject({'message': 'All promises were rejected'}), _interp=_interp)
        result = cls(_interp=_interp)
        errors = [_UNDEF] * len(items)
        remaining = [len(items)]
        def _make_rej(idx):
            def on_rej(r):
                errors[idx] = r
                remaining[0] -= 1
                if remaining[0] == 0:
                    result._reject(JSObject({'errors': JSArray(errors)}))
            return on_rej
        for i, p in enumerate(items):
            _ensure_promise(p, _interp)._then(result._resolve, _make_rej(i))
        return result


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _to_list(value):
    if isinstance(value, (list, tuple)):
        return list(value)
    if isinstance(value, JSArray):
        return list(value)
    return []


def _ensure_promise(value, _interp=None):
    if isinstance(value, JSPromise):
        return value
    return JSPromise.resolve(value, _interp=_interp)


# ---------------------------------------------------------------------------
# Constructor callable for use in JS globals
# ---------------------------------------------------------------------------

class _PromiseCtor:
    """Callable that acts as the JS Promise constructor and namespace."""

    def __init__(self, interp=None):
        self._interp = interp

    def __call__(self, executor=_UNDEF):
        return JSPromise(
            executor if executor is not _UNDEF else None,
            _interp=self._interp,
        )

    # Static methods exposed as attributes
    def _resolve(self, value=_UNDEF):
        return JSPromise.resolve(value, _interp=self._interp)

    def _reject(self, reason=_UNDEF):
        return JSPromise.reject(reason, _interp=self._interp)

    def _all(self, promises=_UNDEF):
        return JSPromise.all(_to_list(promises) if promises is not _UNDEF else [], _interp=self._interp)

    def _allSettled(self, promises=_UNDEF):
        return JSPromise.allSettled(_to_list(promises) if promises is not _UNDEF else [], _interp=self._interp)

    def _race(self, promises=_UNDEF):
        return JSPromise.race(_to_list(promises) if promises is not _UNDEF else [], _interp=self._interp)

    def _any(self, promises=_UNDEF):
        return JSPromise.any(_to_list(promises) if promises is not _UNDEF else [], _interp=self._interp)

    def make_ctor_obj(self):
        """Return a JSObject exposing Promise as used in JS."""
        obj = JSObject()
        obj['resolve'] = self._resolve
        obj['reject'] = self._reject
        obj['all'] = self._all
        obj['allSettled'] = self._allSettled
        obj['race'] = self._race
        obj['any'] = self._any
        # Allow new Promise(fn) via __call__
        obj['__call__'] = self.__call__
        return obj
