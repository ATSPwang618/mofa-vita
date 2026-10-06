"""Self-authored font fixtures. Requires fontTools; never needed at runtime."""
from pathlib import Path
import struct
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.pens.reverseContourPen import ReverseContourPen

directory = Path(__file__).parent
fb = FontBuilder(1000, isTTF=True)
fb.setupGlyphOrder(['.notdef', 'space', 'A', 'B', 'O', 'C'])
fb.setupCharacterMap({32: 'space', 65: 'A', 66: 'B', 79: 'O', 67: 'C', 63: 'A'})
glyphs = {}
for name, corners in {'.notdef': [], 'space': [], 'A': [(0, 0), (0, 700), (500, 700), (500, 0)],
                      'B': [(-100, 0), (-100, 500), (300, 500), (300, 0)]}.items():
    pen = TTGlyphPen(None)
    if corners:
        pen.moveTo(corners[0])
        for corner in corners[1:]: pen.lineTo(corner)
        pen.closePath()
    glyphs[name] = pen.glyph()
# Opposite contour windings preserve the counter (hole) in O.
pen = TTGlyphPen(None)
pen.moveTo((0, 350))
pen.qCurveTo((0, 700), (250, 700))
pen.qCurveTo((500, 700), (500, 350))
pen.qCurveTo((500, 0), (250, 0))
pen.qCurveTo((0, 0), (0, 350))
pen.closePath()
pen.moveTo((100, 350))
pen.qCurveTo((100, 100), (250, 100))
pen.qCurveTo((400, 100), (400, 350))
pen.qCurveTo((400, 600), (250, 600))
pen.qCurveTo((100, 600), (100, 350))
pen.closePath()
glyphs['O'] = pen.glyph()
# The same outlines with reversed winding exercise CFF-like decoration overlap.
pen = TTGlyphPen(None)
glyphs['O'].draw(ReverseContourPen(pen), None)
glyphs['C'] = pen.glyph()
fb.setupGlyf(glyphs)
fb.setupHorizontalMetrics({'.notdef': (600, 0), 'space': (300, 0), 'A': (600, 0), 'B': (400, -100), 'O': (600, 0), 'C': (600, 0)})
fb.setupHorizontalHeader(ascent=800, descent=-200)
fb.setupNameTable({'familyName': 'Krkr Fixture', 'styleName': 'Regular', 'uniqueFontIdentifier': 'KrkrFixture1',
                   'fullName': 'Krkr Fixture', 'psName': 'KrkrFixture'})
fb.setupOS2(sTypoAscender=800, sTypoDescender=-200, usWinAscent=800, usWinDescent=200)
fb.setupPost()
fb.setupMaxp()
fb.font['head'].created = fb.font['head'].modified = 0
fb.save(directory / 'fixture.ttf')

# One 2x2 65-level A, with independent advance 7, baseline origin (1, 2).
for version, compressed in [(0, bytes([64, 0x41, 1, 0, 0x41, 1])), (1, bytes([64, 0x41, 0, 0x41]))]:
    data = b'TVP pre-rendered font\x1a' + bytes([version, 2])
    data += struct.pack('<III', 1, 36, 38) + struct.pack('<H', 65)
    data += struct.pack('<IHHhhhhhH', 58, 2, 2, 1, 2, 7, 0, 7, 0) + compressed
    (directory / f'fixture-v{version}.tft').write_bytes(data)
