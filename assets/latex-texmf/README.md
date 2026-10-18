# Pinned TeX Live resources

This tree contains unmodified resources from TeX Live 2024 final, including
LaTeX 2024-11-01 patch level 2 and the matching expl3/backend release of 2025-01-18.
`manifest.json` records archive URLs, checksums, package dependency closure and
individual file hashes.

The corresponding source works and distribution notices are retained in `source/`
and `doc/`. See `doc/latex/base/legal.txt`, `doc/latex/base/lppl.txt`, the expl3
and backend README files, the Unicode data notices and each package's documents.
Runtime and source files have not been patched for ReTeX.

The tree covers the initialization kernel and a selection of classes, mathematics,
graphics, tables and other packages. `tools/build_browser_format.py` initializes
the Rust engine with these resources. `tools/build_browser.py --common-papers`
adds packages from checksum-verified archives of the same frozen TeX Live release.

Package availability is distinct from engine compatibility. See the repository's
feature guide for current runtime and preview limitations.
