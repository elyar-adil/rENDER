# Contributing to rENDER

## Setup

```bash
python -m venv .venv
source .venv/bin/activate
pip install -r requirements-dev.txt
```

On Windows PowerShell:

```powershell
python -m venv .venv
.venv\Scripts\Activate.ps1
pip install -r requirements-dev.txt
```

## Required Checks

Before sending a change, run:

```bash
python -m pytest
python -m compileall engine.py screenshot.py css html js layout network rendering tests
python -m ruff check engine.py screenshot.py css html js layout network rendering tests
```

## Change Guidelines

- Prefer small, test-backed changes over broad rewrites.
- Preserve behavior unless the change explicitly fixes a bug or improves compatibility.
- Add or update regression tests for parser, cascade, layout, network, or engine changes.
- Do not revert unrelated work in a dirty tree.
- Keep active work in `engine.py`, package directories, and `tests/`.
- Treat `graphics.py`, `layout.py`, `run_hao123.py`, `res.py`, and `paser/` as historical code unless a change explicitly targets those paths.

## Review Expectations

Good contributions usually include:

- a failing test or a concrete reproduction
- the minimal code change needed to fix it
- an explanation of any tradeoff in semantics or compatibility
- verification output for the commands above

## Optional Visual Checks

The repository includes headless and browser-based helpers for visual comparisons:

```bash
python screenshot.py example/index.html out.png 1280 900
python tests/browser_visual_regression.py --module header
```

The browser-based regression helper requires a locally installed Chromium or Chrome-compatible binary.
