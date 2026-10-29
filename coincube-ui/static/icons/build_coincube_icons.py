"""Build coincube-icons.ttf, the marks Bootstrap Icons does not have.

Metrics copy bootstrap-icons.ttf (300 units per em, ascent 300, descent 0) so
a glyph sits like its Bootstrap neighbours in the same text row.

    python3 -m venv venv && venv/bin/pip install fonttools==4.55.3
    venv/bin/python build_coincube_icons.py

Glyphs:
    U+E000  spark   Spark's asterisk, the path in ../logos/spark_badge.svg
"""

from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

UPM = 300

# spark_badge.svg's asterisk outline (all straight segments), in its own
# 67.29 x 64 box with y pointing down.
SPARK = [
    (39.7159, 25.248), (40.8727, 0.570312), (26.4219, 0.570312),
    (27.5787, 25.2483), (4.46555, 16.5221), (0, 30.2656),
    (23.8282, 36.7915), (8.38717, 56.0763), (20.0781, 64.5703),
    (33.6483, 43.9245), (47.2179, 64.5695), (58.9089, 56.0755),
    (43.4679, 36.7909), (67.2937, 30.2657), (62.8281, 16.5221),
]


def fitted(points, size):
    """Scale to `size` units wide, center in the em, flip y up."""
    xs = [x for x, _ in points]
    ys = [y for _, y in points]
    scale = size / (max(xs) - min(xs))
    height = (max(ys) - min(ys)) * scale
    left = (UPM - size) / 2
    bottom = (UPM - height) / 2
    out = [
        (round(left + (x - min(xs)) * scale), round(bottom + height - (y - min(ys)) * scale))
        for x, y in points
    ]
    # TrueType outer contours run clockwise (negative shoelace area, y up).
    area = sum(x0 * y1 - x1 * y0 for (x0, y0), (x1, y1) in zip(out, out[1:] + out[:1]))
    return out if area < 0 else out[::-1]


def polygon(points):
    pen = TTGlyphPen(None)
    pen.moveTo(points[0])
    for point in points[1:]:
        pen.lineTo(point)
    pen.closePath()
    return pen.glyph()


def main():
    builder = FontBuilder(UPM, isTTF=True)
    order = [".notdef", "spark"]
    builder.setupGlyphOrder(order)
    builder.setupCharacterMap({0xE000: "spark"})
    glyphs = {".notdef": TTGlyphPen(None).glyph(), "spark": polygon(fitted(SPARK, 270))}
    builder.setupGlyf(glyphs)
    # The left side bearing must equal xMin, or renderers shift the outline.
    builder.setupHorizontalMetrics(
        {name: (UPM, getattr(glyphs[name], "xMin", 0)) for name in order}
    )
    builder.setupHorizontalHeader(ascent=UPM, descent=0)
    # fsSelection bit 6: REGULAR.
    builder.setupOS2(
        sTypoAscender=UPM, sTypoDescender=0, usWinAscent=327, usWinDescent=29, fsSelection=0x40
    )
    # fontdb (iced's font database) skips a font without a PostScript name.
    builder.setupNameTable(
        {
            "familyName": "coincube-icons",
            "styleName": "Regular",
            "fullName": "coincube-icons",
            "psName": "coincube-icons",
        }
    )
    builder.setupPost()
    builder.save(str(Path(__file__).with_name("coincube-icons.ttf")))


if __name__ == "__main__":
    main()
