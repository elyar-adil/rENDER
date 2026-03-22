# rENDER

`rENDER` is a small browser engine written in Python with `PyQt6`. It parses HTML, CSS, and a growing subset of JavaScript, computes styles, builds layout boxes, and paints real pages without delegating rendering to an existing browser engine.

The project is intentionally hands-on: core subsystems such as parsing, cascade, layout, and painting live in this repository and are readable end to end.

## Highlights

- HTML, CSS, DOM, layout, and painting are implemented in Python.
- Real-page compatibility work is covered by focused regression tests.
- Layout coverage includes block, inline, float, flex, grid, table, and positioned flows.
- External stylesheets and images are fetched concurrently during pipeline execution.
- Headless screenshot tooling and browser-vs-engine visual diff helpers are included.
- WebKit-inspired layout fixtures are imported and adapted into deterministic geometry tests.

## Screenshots

### `example/index.html`
![rENDER rendering index.html](docs/screenshot_index.png)

### `example/hao123.html`
![rENDER rendering hao123.html](docs/screenshot_hao123.png)

## Quick Start

### 1. Create an environment

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

### 2. Run the browser

```bash
python engine.py
python engine.py example/index.html
python engine.py https://example.com
python engine.py --width 1280 --height 800 example/hao123.html
```

### 3. Render a screenshot

```bash
python screenshot.py example/index.html out.png 1280 900
```

## Development

Run the test suite:

```bash
python -m pytest
```

Run syntax and lint checks:

```bash
python -m compileall engine.py screenshot.py css html js layout network rendering tests
python -m ruff check engine.py screenshot.py css html js layout network rendering tests
```

## Repository Layout

```text
engine.py                Browser entry point and pipeline orchestration
html/                    HTML parser and DOM nodes
css/                     Tokenizer, parser, cascade, computed styles
js/                      Lexer, parser, interpreter, DOM bindings, XHR
layout/                  Block, inline, float, flex, grid, table layout engines
rendering/               Display list and PyQt6 painter backend
network/                 HTTP fetching and response decoding
tests/                   Unit, layout, compatibility, and visual regression helpers
docs/                    Notes, screenshots, and compatibility analysis
example/                 Local HTML fixtures for manual testing
```

## Architecture

The main render path looks like this:

1. Parse HTML into a DOM tree.
2. Fetch external CSS and image resources.
3. Execute supported JavaScript against the DOM.
4. Bind CSS, compute styles, and resolve layout boxes.
5. Paint the display list with the PyQt6 backend.

The central orchestration lives in [`engine.py`](engine.py). Layout engines are split by formatting context under [`layout/`](layout/).

## Compatibility Strategy

The project is not trying to mimic every browser subsystem at once. Instead, it improves compatibility through narrow, test-backed slices:

- page-specific regressions captured as unit tests
- adapted WebKit layout fixtures
- visual regression helpers for local fixtures
- compatibility notes under `docs/`

This keeps behavior improvements concrete and reviewable.

## Historical Code

Some top-level modules such as `graphics.py`, `layout.py`, `run_hao123.py`, `res.py`, and `paser/` are older prototypes kept for reference. Active engine work should target `engine.py`, the package directories, and `tests/`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup, review expectations, and change guidelines.

## License

Distributed under the terms in [COPYING](COPYING).
