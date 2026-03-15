# rENDER — Browser Engine

A browser engine built from scratch in Python, using PyQt6 for rendering.

## Architecture

```
html/parser.py     → Document (DOM tree)
css/cascade.py     → computed styles on each Element
css/computed.py    → resolve units (em→px, etc.)
layout/__init__.py → DisplayList of draw commands
rendering/qt_painter.py → QPainter execution
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
| `layout/text.py` | Text measurement via QFontMetrics |
| `layout/float_manager.py` | Float tracking within BFC |
| `rendering/display_list.py` | DrawRect/DrawText/DrawBorder/DrawImage commands |
| `rendering/qt_painter.py` | PyQt6 QPainter rendering backend |
| `js/` | JavaScript engine (Phase 5, stubs for now) |

## Design Principles

- No third-party libraries except PyQt6
- Each layer has a clear interface contract
- DOM → CSS → Layout → Rendering pipeline
- Tests must always pass: `python -m pytest tests/`
