# TeX and LaTeX font resources

These unmodified TFM metrics and Type1 outlines supply all 47 fonts named by
the bundled `assets/tex/plain.tex`, the additional Computer Modern
sizes used by LaTeX, AMS symbol fonts and smaller extension fonts, and the
classic LaTeX symbol and picture fonts. They are resources, not Python
replacements for font definitions. Source/cwd resources and an explicit
`--font-dir` retain precedence; this directory is the default bundled fallback.

Computer Modern metrics come from CTAN's `fonts/cm/tfm.zip`; their distribution
license is the Knuth license. Computer Modern outlines come from the AMSFonts
distribution and retain its copyright and SIL Open Font License in `OFL.txt`.
No outlines, metrics, names or encodings have been modified.

The `manfnt.tfm` metric comes from TeX Live's `knuth-lib` package. Its Type1
outline is Taco Hoekwater's 1998 conversion, from `manfnt-font`, distributed under
the Knuth license; its copyright notice remains in the PFB header. The canonical
source package is <https://ctan.org/tex-archive/fonts/manual>.

The LaTeX metrics and their unmodified METAFONT source files in `source/` come
from TeX Live's `latex-fonts` package. Their Type1 outlines and `latxfont.map`
come from AMSFonts. The bundled map resolves genuine outline names such as
`lcircle10` to `lcircle1.pfb`; no font files have been renamed or modified.

`manifest.json` records the archive URLs/hashes and every font/source file's SHA-256.
The engine uses these local files without a separate TeX installation.
It loads the bundled default `pdftex.map` at first PDF font use unless an
unprefixed map command cancels it. It includes original `latxfont.map` entries
and selected original CM-Super T1/TS1 entries, plus explicit classic Type1
entries generated from each bundled outline's retained FontName. Native-profile
jobs select outlines through maps; an unmapped font follows PK/PGC lookup even
when a matching PFB is present. Both engines receive matching explicit maps in
native comparison fixtures. Runtime `pdfmapfile`/`pdfmapline`
editing is supported.

The additional EC/TC metrics and CM-Super programs come from pinned TeX Live
2024 final `ec` and `cm-super` packages. Roman T1/TS1 sizes from 5pt through
24.88pt, including 10.95pt, share the matching `sfrm` outlines. Selected bold
and italic T1 sizes cover the integration corpus; this is not the complete
EC/CM-Super distribution. Unmodified `cm-super-t1.enc` and `cm-super-ts1.enc`
vectors map precomposed accented glyphs and text symbols.

The complete original EC/TC METAFONT sources are in `source/ec/`; original EC
license/readme/manifest documents are in `licenses/ec/`. CM-Super's original
GPL license, distribution README and PDF embedding exception are in
`licenses/cm-super/`. Added archive and file hashes are recorded in
`manifest.json`. The default map is assembled from unmodified upstream lines;
it is a bundled resource profile, not a claim of full native font coverage.

`pdftexspace.tfm` and `pdftexspace.pfb` are implementation-owned extraction
resources, independently created by [generate-spacefont](../../../src/bin/generate-spacefont.rs).
They contain empty `.notdef` and `space` glyphs with zero width and no ink.
The generator uses fontTools and canonical Type1 framing; native differential
fixtures use these same two resources. No historical font outlines are modified.
