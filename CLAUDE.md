# rENDER — Minimal Browser

**Goal**: A lean, correct, usable browser built from scratch in Python with PyQt6.
Implement the minimum code necessary to render real web pages correctly.
Every line must earn its place — no gold-plating, no speculative features.

## Architecture

```
html/parser.py          → Document (DOM tree)
css/cascade.py          → computed styles on each Element
css/computed.py         → resolve units (em→px, etc.)
layout/__init__.py      → DisplayList of draw commands
rendering/display_list.py → platform-agnostic draw commands
backend/base.py         → FontMetrics + ImageLoader ABCs
backend/qt/painter.py   → QPainter execution (Qt implementation)
```

## Running

```bash
python engine.py                          # renders example/index.html
python engine.py example/index.html       # explicit file
python engine.py https://example.com      # fetch URL
```

## Tests

```bash
python -m pytest tests/
```

## Module Map

| Module | Purpose |
|--------|---------|
| `html/dom.py` | Node, Element, Text, Document classes |
| `html/parser.py` | HTML5 tokenizer + tree builder |
| `html/entities.py` | HTML entity decoding |
| `network/http.py` | HTTP/HTTPS fetching (urllib) |
| `css/tokenizer.py` | CSS lexer |
| `css/parser.py` | CSS rule/declaration parser |
| `css/selector.py` | Selector matching + specificity |
| `css/cascade.py` | Cascade algorithm (UA → author → inline) |
| `css/computed.py` | Unit resolution (em, rem, %, vw, vh → px) |
| `css/properties.py` | Property definitions (initial values, inheritance) |
| `layout/box.py` | BoxModel, Rect, EdgeSizes |
| `layout/block.py` | Block Formatting Context |
| `layout/inline.py` | Inline Formatting Context + LineBox |
| `layout/flex.py` | Flexbox layout |
| `layout/text.py` | Text measurement facade (delegates to backend) |
| `layout/float_manager.py` | Float tracking within BFC |
| `rendering/display_list.py` | DrawRect/DrawText/DrawBorder/DrawImage commands |
| `backend/base.py` | FontMetrics + ImageLoader abstract base classes |
| `backend/qt/font.py` | Qt font metrics implementation |
| `backend/qt/image.py` | Qt image loading/decoding implementation |
| `backend/qt/painter.py` | PyQt6 QPainter rendering backend |
| `backend/qt/app.py` | Qt application controller (Browser + loader thread) |
| `js/` | JavaScript engine (Phase 5, stubs for now) |

## Design Principles

- **Minimal**: solve the problem with the least code that works correctly
- **No third-party libraries** except PyQt6
- **Correctness over completeness**: implement features fully or not at all
- **Delete before adding**: prefer removing dead code to working around it
- Each layer has a clear interface: DOM → CSS → Layout → Rendering
- Tests must always pass: `python -m pytest tests/`
