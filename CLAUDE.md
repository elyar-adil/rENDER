# rENDER — Minimal Browser

**Goal**: A lean, correct, usable browser built from scratch in PyX (a typed
Python subset that compiles to native code) with PyQt6 (primary) or the
bundled pyx Win32 GUI library (fallback).
Implement the minimum code necessary to render real web pages correctly.
Every line must earn its place — no gold-plating, no speculative features.

## Architecture

```
html/parser.pyx          → Document (DOM tree)
css/cascade.pyx          → computed styles on each Element
css/computed.pyx         → resolve units (em→px, etc.)
layout/__init__.pyx      → DisplayList of draw commands
rendering/display_list.pyx → platform-agnostic draw commands
backend/base.pyx         → FontMetrics + ImageLoader ABCs
backend/qt/painter.pyx   → QPainter execution (Qt implementation)
backend/pyx_gui/         → Win32 GDI backend (uses lib/gui)
lib/gui/                 → ctypes Win32 GUI framework (from pyx repo)
third_party/pyx/         → pyx compiler/checker (git submodule)
```

## Running

```bash
python engine.pyx                          # renders example/index.html
python engine.pyx example/index.html       # explicit file
python engine.pyx https://example.com      # fetch URL
```

## Tests

```bash
python -m pytest tests/
```

## Source file convention

All source files use the `.pyx` extension (PyX — typed Python subset).
CPython runs them directly via the import hook installed in `engine.pyx`
and `tests/conftest.py`. The hook adds `.pyx` to
`importlib.machinery.SOURCE_SUFFIXES` so the normal import machinery
resolves `.pyx` files as if they were `.py`.

## Module Map

| Module | Purpose |
|--------|---------|
| `html/dom.pyx` | Node, Element, Text, Document classes |
| `html/parser.pyx` | HTML5 tokenizer + tree builder |
| `html/entities.pyx` | HTML entity decoding |
| `network/http.pyx` | HTTP/HTTPS fetching (urllib) |
| `css/tokenizer.pyx` | CSS lexer |
| `css/parser.pyx` | CSS rule/declaration parser |
| `css/selector.pyx` | Selector matching + specificity |
| `css/cascade.pyx` | Cascade algorithm (UA → author → inline) |
| `css/computed.pyx` | Unit resolution (em, rem, %, vw, vh → px) |
| `css/properties.pyx` | Property definitions (initial values, inheritance) |
| `layout/box.pyx` | BoxModel, Rect, EdgeSizes |
| `layout/block.pyx` | Block Formatting Context |
| `layout/inline.pyx` | Inline Formatting Context + LineBox |
| `layout/flex.pyx` | Flexbox layout |
| `layout/text.pyx` | Text measurement facade (delegates to backend) |
| `layout/float_manager.pyx` | Float tracking within BFC |
| `rendering/display_list.pyx` | DrawRect/DrawText/DrawBorder/DrawImage commands |
| `backend/base.pyx` | FontMetrics + ImageLoader abstract base classes |
| `backend/qt/font.pyx` | Qt font metrics implementation |
| `backend/qt/image.pyx` | Qt image loading/decoding implementation |
| `backend/qt/painter.pyx` | PyQt6 QPainter rendering backend |
| `backend/qt/app.pyx` | Qt application controller (Browser + loader thread) |
| `backend/pyx_gui/font.pyx` | Win32 GDI font metrics (heuristic fallback) |
| `backend/pyx_gui/image.pyx` | Pure-Python image dimension decoder |
| `backend/pyx_gui/painter.pyx` | Win32 GDI painter (uses lib/gui Window) |
| `lib/gui/` | ctypes Win32 GUI framework (from pyx GUI branch) |
| `third_party/pyx/` | pyx compiler & static checker (git submodule, main) |
| `js/` | JavaScript engine |

## Design Principles

- **Minimal**: solve the problem with the least code that works correctly
- **No third-party libraries** except PyQt6 (or lib/gui on Windows)
- **Correctness over completeness**: implement features fully or not at all
- **Delete before adding**: prefer removing dead code to working around it
- Each layer has a clear interface: DOM → CSS → Layout → Rendering
- Tests must always pass: `python -m pytest tests/`
- PyX rules: explicit type annotations on all functions, class field declarations,
  no monkey-patching
