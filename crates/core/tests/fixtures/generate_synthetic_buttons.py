"""Generate synthetic-buttons.hex; needs FontTools only when regenerating this fixture."""
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

chars = "HABXYZLSRC"
names = [".notdef"] + list(chars)
fb = FontBuilder(1000, isTTF=True)
fb.setupGlyphOrder(names)
fb.setupCharacterMap({ord(c): c for c in chars})
glyphs = {}
for i, name in enumerate(names):
    pen = TTGlyphPen(glyphs)
    if name == "C":
        pen.addComponent("A", (1, 0, 0, 1, 17, 4))
    elif i:
        pen.moveTo((30, 0))
        pen.lineTo((40, 700))
        if name == "A":
            pen.qCurveTo((100, 780), (250, 780), (300 + i * 7, 700))
        else:
            pen.lineTo((300 + i * 7, 700))
        pen.lineTo((350 + i * 7, 0))
        pen.closePath()
        pen.moveTo((100, 150))
        pen.lineTo((250, 150))
        pen.lineTo((240, 500))
        pen.lineTo((110, 500))
        pen.closePath()
    glyphs[name] = pen.glyph()
fb.setupGlyf(glyphs)
fb.setupHorizontalMetrics({name: (450 + i * 5, 0) for i, name in enumerate(names)})
fb.setupHorizontalHeader(ascent=800, descent=-200)
fb.setupNameTable({"familyName": "Synthetic Buttons", "styleName": "Regular"})
fb.setupOS2()
fb.setupPost()
fb.setupMaxp()
fb.save("/tmp/synthetic-buttons.ttf")
with open("/tmp/synthetic-buttons.ttf", "rb") as f:
    data = f.read().hex()
with open("crates/core/tests/fixtures/synthetic-buttons.hex", "w") as f:
    for i in range(0, len(data), 160):
        f.write(data[i:i + 160] + "\n")
# Map B to the composite for the Rust-only composite regression.
from fontTools.ttLib import TTFont
font = TTFont("/tmp/synthetic-buttons.ttf")
for table in font["cmap"].tables:
    if table.isUnicode():
        table.cmap[ord("B")] = "C"
font.save("/tmp/synthetic-buttons-composite.ttf")
with open("/tmp/synthetic-buttons-composite.ttf", "rb") as f:
    data = f.read().hex()
with open("crates/core/tests/fixtures/synthetic-buttons-composite.hex", "w") as f:
    for i in range(0, len(data), 160):
        f.write(data[i:i + 160] + "\n")
