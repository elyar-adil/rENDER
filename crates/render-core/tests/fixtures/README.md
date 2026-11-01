# Webfont fixture

`render-test-webfont.ttf` is a generated, redistributable test face. It exists
so a `@font-face` test can prove that layout measured the declared face rather
than the reference measurer's nominal advances.

`render-test-webfont.woff` and `render-test-webfont.woff2` are the same face in
the two compressed containers. They are used to test the WOFF 1.0 inflater and
the WOFF2 decoder.

`webfont-http-demo.html` is the manual shell fixture for the HTTP path; serve
this directory over HTTP and screenshot the page to see the webfont line wider
than the fallback line.

Properties:

- units per em: 1000
- ascent: 800, descent: 200
- every visible ASCII glyph advances 1000 units (1 em)
- the space advances 600 units (0.6 em)
- the outlines are simple H shapes; the metrics are the part under test

It was generated with fontTools 4.55.3. To regenerate it, run this from the
repository root:

```python
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789 "
names = [".notdef"] + ["uni%04X" % ord(c) for c in chars]
fb = FontBuilder(1000, isTTF=True)
fb.setupGlyphOrder(names)
fb.setupCharacterMap({ord(c): "uni%04X" % ord(c) for c in chars})

def glyph(advance, draw_h):
    pen = TTGlyphPen(None)
    if draw_h:
        # One H-shaped contour: two stems and a crossbar.
        left = 80
        right = advance - 80
        stem = 100
        bar_top = 400
        bar_bottom = 300
        pen.moveTo((left, 0))
        pen.lineTo((left + stem, 0))
        pen.lineTo((left + stem, bar_bottom))
        pen.lineTo((right - stem, bar_bottom))
        pen.lineTo((right - stem, 0))
        pen.lineTo((right, 0))
        pen.lineTo((right, 700))
        pen.lineTo((right - stem, 700))
        pen.lineTo((right - stem, bar_top))
        pen.lineTo((left + stem, bar_top))
        pen.lineTo((left + stem, 700))
        pen.lineTo((left, 700))
        pen.closePath()
    return pen.glyph()

glyphs = {".notdef": glyph(500, False)}
metrics = {".notdef": (500, 0)}
for c in chars:
    name = "uni%04X" % ord(c)
    advance = 1000 if c != " " else 600
    glyphs[name] = glyph(advance, c != " ")
    metrics[name] = (advance, 80 if c != " " else 0)

fb.setupGlyf(glyphs)
fb.setupHorizontalMetrics(metrics)
fb.setupHorizontalHeader(ascent=800, descent=-200)
fb.setupNameTable({
    "familyName": "Render Test Webfont",
    "styleName": "Regular",
    "uniqueFontIdentifier": "rENDER Render Test Webfont Regular 1.0",
    "fullName": "Render Test Webfont Regular",
    "psName": "RenderTestWebfont-Regular",
    "version": "Version 1.0",
})
fb.setupOS2(sTypoAscender=800, sTypoDescender=-200, sTypoLineGap=0,
            usWinAscent=800, usWinDescent=200)
fb.setupPost()
fb.save("crates/render-core/tests/fixtures/render-test-webfont.ttf")
```

Then write the two containers from that TTF (WOFF2 needs the Python `brotli`
package):

```python
from fontTools.ttLib import TTFont

for flavor, extension in [("woff", "woff"), ("woff2", "woff2")]:
    font = TTFont("crates/render-core/tests/fixtures/render-test-webfont.ttf")
    font.flavor = flavor
    font.save(f"crates/render-core/tests/fixtures/render-test-webfont.{extension}")
```
