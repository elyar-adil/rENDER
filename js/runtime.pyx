"""Standalone JS runtime — Node-compatible execution context.

Provides a browser-free environment for running JavaScript:

    runtime = JSRuntime(argv=['script.js', '--flag'])
    runtime.run_file('script.js')

    runtime = JSRuntime()
    runtime.run_string('console.log("hello")')

Supports:
  - process  (argv, env, exit, stdout/stderr, cwd, platform, version)
  - require() for built-in modules: 'fs', 'path', 'os'
  - require() for relative/absolute JS files (CommonJS module.exports)
  - console.log/warn/error/info without the browser [JS] prefix
"""

import os
import sys
import stat
import tempfile

from js.interpreter import Interpreter, JSObject, JSArray, _UNDEF, _to_str, _to_num


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

class JSRuntime:
    """Execute JavaScript outside the browser with a Node-like environment."""

    def __init__(
        self,
        argv: list[str] | None = None,
        *,
        _module_cache: dict | None = None,
    ) -> None:
        self._interp = Interpreter()
        self._interp.console_prefix = ''   # clean output — no '[JS]' prefix
        self._cwd = os.getcwd()
        self._module_cache: dict[str, object] = {} if _module_cache is None else _module_cache
        self._setup_node_globals(list(argv) if argv else [])

    # ------------------------------------------------------------------
    # Execution
    # ------------------------------------------------------------------

    def run_string(self, code: str, *, filename: str = '<string>') -> None:
        """Execute a JS source string in this runtime."""
        from js.lexer import Lexer
        from js.parser import Parser
        tokens = Lexer(code).tokenize()
        ast = Parser(tokens).parse()
        self._interp.execute(ast)

    def run_file(self, path: str) -> None:
        """Execute a JS file in this runtime."""
        abs_path = os.path.abspath(path)
        self._cwd = os.path.dirname(abs_path)
        # Reflect the resolved path in process.argv
        process = self._interp.global_env.get('process')
        if isinstance(process, JSObject):
            argv = process.get('argv')
            if isinstance(argv, JSArray) and len(argv) > 1:
                argv[1] = abs_path
        with open(abs_path, encoding='utf-8') as fh:
            code = fh.read()
        self.run_string(code, filename=abs_path)

    # ------------------------------------------------------------------
    # Global setup
    # ------------------------------------------------------------------

    def _setup_node_globals(self, argv: list[str]) -> None:
        g = self._interp.global_env

        # Mask browser-only globals that have no meaning outside a browser.
        for name in ('window', 'self', 'document', 'alert', 'confirm', 'prompt'):
            g.define(name, _UNDEF)

        g.define('process', self._make_process(argv))
        g.define('require', lambda path: self._require(_to_str(path)))
        g.define('global', g.get('globalThis'))

        # CommonJS module / exports (top-level defaults)
        _mod = JSObject({'exports': JSObject()})
        g.define('module', _mod)
        g.define('exports', _mod['exports'])

    def _make_process(self, argv: list[str]) -> JSObject:
        process = JSObject()
        process['argv'] = JSArray([sys.executable] + argv)
        process['env'] = JSObject(dict(os.environ))
        process['platform'] = sys.platform
        process['version'] = 'v18.0.0'
        process['versions'] = JSObject({'node': '18.0.0', 'python': sys.version.split()[0]})
        process['exit'] = lambda code=0: sys.exit(int(_to_num(code)) if code is not _UNDEF else 0)
        process['cwd'] = lambda: self._cwd
        process['hrtime'] = lambda *_: JSArray([0, 0])
        _out = JSObject()
        _out['write'] = lambda s: sys.stdout.write(_to_str(s)) or _UNDEF
        process['stdout'] = _out
        _err = JSObject()
        _err['write'] = lambda s: sys.stderr.write(_to_str(s)) or _UNDEF
        process['stderr'] = _err
        return process

    # ------------------------------------------------------------------
    # require()
    # ------------------------------------------------------------------

    def _require(self, specifier: str) -> object:
        if specifier in self._module_cache:
            return self._module_cache[specifier]

        if specifier == 'fs':
            result = _make_fs_module()
        elif specifier == 'path':
            result = _make_path_module(self)
        elif specifier == 'os':
            result = _make_os_module()
        elif specifier.startswith('.') or os.path.isabs(specifier):
            result = self._load_file_module(specifier)
        else:
            raise RuntimeError(f"Cannot find module '{specifier}'")

        self._module_cache[specifier] = result
        return result

    def _load_file_module(self, specifier: str) -> object:
        """Load a relative/absolute JS file as a CommonJS module."""
        if not specifier.endswith('.js'):
            specifier += '.js'
        abs_path = os.path.normpath(os.path.join(self._cwd, specifier))

        # Return early stub to handle circular requires.
        if abs_path in self._module_cache:
            return self._module_cache[abs_path]
        stub_exports = JSObject()
        self._module_cache[abs_path] = stub_exports

        with open(abs_path, encoding='utf-8') as fh:
            code = fh.read()

        sub = JSRuntime(argv=[abs_path], _module_cache=self._module_cache)
        sub._cwd = os.path.dirname(abs_path)

        # Override module/exports with objects tied to the stub.
        mod_obj = JSObject({'exports': stub_exports})
        sub._interp.global_env.define('module', mod_obj)
        sub._interp.global_env.define('exports', stub_exports)

        sub.run_string(code, filename=abs_path)

        # module.exports may have been replaced (e.g. `module.exports = fn`)
        result = mod_obj['exports']
        self._module_cache[abs_path] = result
        return result


# ---------------------------------------------------------------------------
# Built-in module factories
# ---------------------------------------------------------------------------

def _make_fs_module() -> JSObject:
    fs = JSObject()

    def _read_file(p, enc='utf-8'):
        encoding = str(enc) if enc and enc is not _UNDEF else 'utf-8'
        if encoding.lower() in ('buffer', 'binary'):
            with open(str(p), 'rb') as f:
                return f.read()
        with open(str(p), encoding=encoding) as f:
            return f.read()

    fs['readFileSync'] = _read_file
    fs['writeFileSync'] = lambda p, d, *_: open(str(p), 'w', encoding='utf-8').write(_to_str(d))
    fs['appendFileSync'] = lambda p, d, *_: open(str(p), 'a', encoding='utf-8').write(_to_str(d))
    fs['existsSync'] = lambda p: os.path.exists(str(p))
    fs['readdirSync'] = lambda p: JSArray(os.listdir(str(p)))
    fs['mkdirSync'] = lambda p, *_: os.makedirs(str(p), exist_ok=True)
    fs['unlinkSync'] = lambda p: os.remove(str(p))
    fs['statSync'] = _stat_sync
    fs['lstatSync'] = _stat_sync
    return fs


def _stat_sync(p) -> JSObject:
    s = os.stat(str(p))
    info = JSObject()
    info['size'] = s.st_size
    info['mtime'] = JSObject({'getTime': lambda: int(s.st_mtime * 1000)})
    info['isFile'] = lambda: stat.S_ISREG(s.st_mode)
    info['isDirectory'] = lambda: stat.S_ISDIR(s.st_mode)
    info['isSymbolicLink'] = lambda: stat.S_ISLNK(s.st_mode)
    return info


def _make_path_module(runtime: JSRuntime) -> JSObject:
    m = JSObject()
    m['join'] = lambda *parts: os.path.join(*[str(p) for p in parts])
    m['dirname'] = lambda p: os.path.dirname(str(p))
    m['basename'] = lambda p, ext=_UNDEF: (
        os.path.basename(str(p)).removesuffix(str(ext))
        if ext is not _UNDEF else os.path.basename(str(p))
    )
    m['extname'] = lambda p: os.path.splitext(str(p))[1]
    m['resolve'] = lambda *parts: os.path.realpath(
        os.path.join(runtime._cwd, *[str(p) for p in parts])
    )
    m['isAbsolute'] = lambda p: os.path.isabs(str(p))
    m['normalize'] = lambda p: os.path.normpath(str(p))
    m['sep'] = os.sep
    m['delimiter'] = os.pathsep
    return m


def _make_os_module() -> JSObject:
    m = JSObject()
    m['platform'] = lambda: sys.platform
    m['homedir'] = lambda: os.path.expanduser('~')
    m['tmpdir'] = lambda: tempfile.gettempdir()
    m['EOL'] = os.linesep
    m['arch'] = 'x64'
    m['cpus'] = lambda: JSArray()
    m['hostname'] = lambda: __import__('socket').gethostname()
    m['networkInterfaces'] = lambda: JSObject()
    return m
