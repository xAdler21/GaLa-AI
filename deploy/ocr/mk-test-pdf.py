#!/usr/bin/env python3
"""Erzeugt ein Test-PDF mit Textebene (LV-artige Zeilen) für deploy/ocr/gpu-test.sh.

Aufruf: mk-test-pdf.py <ziel.pdf>

Das PDF hat eine echte Textebene. `bench ocr` rendert die Seiten, erkennt sie per
OCR und vergleicht das Ergebnis mit der Textebene (Zeichenfehlerrate). Braucht nur
die Python-Standardbibliothek.
"""
import sys

PAGES = 8
LINES = [
    "Leistungsverzeichnis Außenanlagen – Los 2: Garten- und Landschaftsbau",
    "Pos.   Beschreibung                                 Menge  Einheit   EP       GP",
    "01.01  Oberboden abtragen, d = 30 cm, seitlich lagern   450,00  m²   3,85   1.732,50",
    "01.02  Baugrube ausheben, Bodenklasse 3–5, abfahren     120,50  m³  24,90   3.000,45",
    "02.01  Pflasterfläche aus Betonstein 20/10/8 cm, grau   310,00  m²  42,70  13.237,00",
    "02.02  Tiefbord 8/25/100 in Beton C20/25 setzen         185,00  m   28,40   5.254,00",
    "03.01  Hochstamm Acer platanoides, StU 18–20, liefern     12,00  St  395,00  4.740,00",
    "03.02  Hecke Carpinus betulus, 125–150 cm, pflanzen      240,00  St   14,60   3.504,00",
    "04.01  Rasenansaat RSM 2.3, inkl. Fertigstellungspflege  980,00  m²    2,95   2.891,00",
    "04.02  Bewässerung der Pflanzflächen, 6 Gänge            6,00  psch 185,00   1.110,00",
    "Für die Ausführung gelten die VOB/C sowie die ZTV-Wegebau in der gültigen Fassung.",
    "Übergrößen, Mehrstärken und Zuschläge für Böschungen über 1:2 sind einzurechnen.",
]


def esc(s: str) -> bytes:
    out = bytearray()
    for b in s.encode("cp1252"):
        if b in (0x28, 0x29, 0x5C) or b > 126:
            out += b"\\%03o" % b
        else:
            out.append(b)
    return bytes(out)


def page_stream(n: int) -> bytes:
    # Courier 9 pt: 5,4 pt je Zeichen, die längste Zeile (86 Zeichen) passt auf A4
    parts = [b"BT /F1 9 Tf 40 790 Td 15 TL"]
    parts.append(b"(" + esc(f"Seite {n} von {PAGES} – Projekt Nr. 2026-{1000 + n}") + b") Tj T*")
    for rep in range(4):
        for line in LINES:
            parts.append(b"(" + esc(line) + b") Tj T*")
    parts.append(b"ET")
    return b"\n".join(parts)


def main(path: str) -> None:
    objs = []  # (Nummer, Inhalt)
    kids = []
    objs.append((1, b"<< /Type /Catalog /Pages 2 0 R >>"))
    objs.append((3, b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /WinAnsiEncoding >>"))
    num = 4
    for n in range(1, PAGES + 1):
        stream = page_stream(n)
        objs.append((num, b"<< /Length %d >>\nstream\n" % len(stream) + stream + b"\nendstream"))
        objs.append((num + 1, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] "
                              b"/Resources << /Font << /F1 3 0 R >> >> /Contents %d 0 R >>" % num))
        kids.append(b"%d 0 R" % (num + 1))
        num += 2
    objs.append((2, b"<< /Type /Pages /Count %d /Kids [" % PAGES + b" ".join(kids) + b"] >>"))
    objs.sort()

    out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offsets = {}
    for no, body in objs:
        offsets[no] = len(out)
        out += b"%d 0 obj\n" % no + body + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n" % (len(objs) + 1)
    out += b"0000000000 65535 f \n"
    for no, _ in objs:
        out += b"%010d 00000 n \n" % offsets[no]
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, xref)
    with open(path, "wb") as f:
        f.write(out)


if __name__ == "__main__":
    main(sys.argv[1])
