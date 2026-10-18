# License and third-party notices

ReTeX's Rust engine is a port of a Python implementation derived from pdfTeX.
The inherited engine license is the GNU General Public License, version 2 or
later; the license text is in [COPYING](COPYING). pdfTeX incorporates work by
the TeX, e-TeX and pdfTeX authors, including Donald E. Knuth, Peter Breitenlohner,
Hàn Thế Thành and other contributors.

Bundled components retain their own licenses and attribution. In particular:

- [TeX Live resources](assets/latex-texmf/README.md) include original runtime
  files, corresponding sources, distribution notices and archive/file hashes.
  Their `doc/` and `source/` directories include LaTeX Project Public License
  material and package-specific terms.
- `assets/tex/plain.tex` and `hyphen.tex` retain their original notices.
- [Font resources](assets/fonts/assets/README.md) retain original licenses,
  sources and provenance. Adobe font metrics have their own
  [PDFBox notice](assets/fonts/metrics/LICENSE.pdfbox).
- [GNU regular expressions and WASI C helpers](native-regex/README.md) retain
  upstream notices under `native-regex/vendor/`, including LGPL terms.
- JavaScript dependency notices are in [web/DEPENDENCY_NOTICES.txt](web/DEPENDENCY_NOTICES.txt).
  Static packaging appends the installed dependencies' license texts.
- Rust dependency versions and sources are recorded in `Cargo.lock`. The XZ and
  TAR decoder notices are also retained in `web/DEPENDENCY_NOTICES.txt`.
- The DejaVu test font retains its [license](tests/fixtures/resources/LICENSE-DejaVu.txt).

Keep these notices and any required corresponding sources available when
distributing native binaries or the browser artifact. The static package includes
this notice, the engine license, font notices and vendored C dependency licenses.
