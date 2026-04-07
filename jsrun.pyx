from __future__ import annotations
#!/usr/bin/env python3
"""jsrun — run JavaScript with the rENDER JS engine (Node-like).

Usage:
    python jsrun.py script.js [arg1 arg2 ...]
    python jsrun.py -e "console.log('hello')"
    python jsrun.py --help
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))


def _usage() -> str:
    return (
        'Usage: jsrun.py <script.js> [args...]\n'
        '       jsrun.py -e <code>  [args...]\n'
        '       jsrun.py -h | --help\n'
    )


def main(argv: list[str] | None = None) -> int:
    if argv is None:
        argv = sys.argv[1:]

    if not argv or argv[0] in ('-h', '--help'):
        print(_usage(), end='')
        return 0

    from js.runtime import JSRuntime

    eval_mode = argv[0] in ('-e', '--eval')

    if eval_mode:
        if len(argv) < 2:
            print('jsrun.py: -e requires a code argument', file=sys.stderr)
            return 1
        code = argv[1]
        script_argv = argv[2:]
        runtime = JSRuntime(argv=['jsrun'] + script_argv)
        try:
            runtime.run_string(code, filename='<eval>')
        except SystemExit as exc:
            return exc.code if isinstance(exc.code, int) else 0
        except Exception as exc:
            print(f'Uncaught exception: {exc}', file=sys.stderr)
            return 1
    else:
        script = argv[0]
        script_argv = argv[1:]
        runtime = JSRuntime(argv=[script] + script_argv)
        try:
            runtime.run_file(script)
        except SystemExit as exc:
            return exc.code if isinstance(exc.code, int) else 0
        except FileNotFoundError:
            print(f'jsrun.py: {script}: No such file or directory', file=sys.stderr)
            return 1
        except Exception as exc:
            print(f'Uncaught exception: {exc}', file=sys.stderr)
            return 1

    return 0


if __name__ == '__main__':
    raise SystemExit(main())
