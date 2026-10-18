# Architecture

ReTeX uses one Rust typesetting engine for native programs and WebAssembly. The
browser adds a resident worker, a project model, a source editor and a Canvas
viewer. Execution does not invoke Python or an external TeX executable.

## Main components

| Location | Responsibility |
| --- | --- |
| `src/core`, `src/language`, `src/engine` | TeX state, tokenization, expansion and execution |
| `src/typesetting`, `src/fonts` | Line breaking, layout and font handling |
| `src/pdf` | PDF serialization and preview paint scenes |
| `src/resources` | Native and virtual filesystems, resource lookup and observations |
| `src/serialization` | Versioned initialized formats and resource provenance |
| `src/session.rs` | Resident editing, revision handling and checkpoint reuse |
| `web/engine-worker.js` | Browser job scheduling and Wasm communication |
| `web/app` | React workspace, CodeMirror editor, project state and navigation |
| `web/preview.js`, `web/preview-pixels.js` | Canvas presentation and image data |

## Execution and resources

The browser worker owns the engine and its virtual filesystem. Project files are
under `/project`; the immutable resource bundle is under `/bundle`. Project files
take precedence over bundled resources. Required missing files produce explicit
requests; optional absence remains observable and must not silently become a hit.

Prepared resource packs are fetched as static assets and checked against
published hashes. For other files, `web/texlive-repository.js` indexes the full
current TeX Live database, resolves the owning package and dependencies, and
fetches archives directly from a CORS-enabled mirror. Rust decodes XZ/TAR with
memory and size limits and verifies the runtime manifest without accessing the
filesystem. Archive checksums are verified against repository metadata before
unpacking. Catalogs and individual archives use the same managed package cache
as prepared packs, including its folder backend and manual removal controls.

LaTeX's required-file error is converted into a typed resource request before
its terminal-input code executes. Optional absence remains an optional probe.
The initialized kernel and already bundled dependencies are retained; repository
versions are recorded separately. Download availability and engine/kernel
compatibility are separate concerns. A Python resource resolver remains optional
for development against frozen TeX Live archives.

## Saved formats and editing checkpoints

A `.rsfmt` file stores initialized, typed engine state with version checks,
resource hashes and provenance. It avoids cold LaTeX initialization on each job.
It is distinct from a live editing checkpoint and incompatible with upstream
TeX `.fmt` or Python reference formats.

`EditingSession` tracks source revisions, focused editing, auxiliary passes and
bounded checkpoints. Fine checkpoints follow the edited region, including inside
included files; coarser checkpoints cover other document/page boundaries. Reuse
checks source prefixes and observed inputs, including resource contents, metadata,
lookup order, absence and clock values. Rollback restores state and output journals
together. Incompatible state falls back to ordinary execution.

Additional caches reuse eligible paragraph, macro and figure construction.
Cache entries must preserve their observed effects and ownership; reusing old
live node identifiers or ignoring changed inputs is unsafe. Eviction is always
allowed and falls back to recomputation.

## Preview and final output

The engine emits paint scenes using physical glyph outlines, image resources,
clipping and transforms. Full scenes and acknowledged deltas avoid resending
unchanged data. The viewer retains the previous output during typing and can
replace a paragraph before the remainder of the page is ready.

Provisional paragraph feedback is a presentation layer, not a proof that the
whole page or engine state is unchanged. Completed pages replace provisional
fragments. Revision identities and acknowledgements prevent stale results from
overwriting newer content. Source maps remain tied to the source revision that
produced their scene.

Interactive error recovery is similarly separate from strict output. Temporary
repairs carry diagnostics and do not become certified state or ordinary export.
Successful auxiliary-file writes are retained only at completed-pass boundaries.

## Scheduling

The worker advances the engine cooperatively and yields between bounded units
of work. Some layout, image, font and regular-expression operations remain
synchronous; a step budget is not a wall-clock deadline. Native applications can
use the same resident session API rather than spawning a CLI for every keystroke.
