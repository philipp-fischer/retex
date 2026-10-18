# Features and limitations

## Browser workspace

Use **Add / replace files**, or drop files and folders onto **Project files**,
to upload sources and assets. Dropping a single folder preserves its contents'
relative paths. **Open project folder…** replaces the current project with a folder.
The first TeX upload replaces the untouched starter document as the compilation
entry point. Later uploads keep the chosen entry point; **Set as main document**
changes it. Uploading a file at the current main path replaces its source too.
The file tree chooses the file being edited. Sources, images and fonts can be
added, replaced or removed without a server.
Uploaded bytes and line endings are preserved; encoding conversion is explicit.
Project edits live in browser memory, so download your source before closing.

The preview displays the current page. Clicking mapped text selects its source;
**Sync**, enabled by default, follows source selections in the preview. Macro
definition letters and argument letters retain their respective source locations.
Generated material can point to an invocation when its origin is known; some
content has no useful source target.

Diagnostics appear beside the editor and in the preview when a location is known.
The workspace supports resizable panes, zoom, light/dark themes and narrow screens.
Drag the preview with the hand cursor to pan; ordinary clicks still navigate to
source. Dark-mode source highlighting uses CodeMirror's One Dark palette, and
selected source text remains visible above the faint current-line highlight.
**Cancel** stops the current job. **Download PDF** uses completed output rather
than capturing the preview canvas.

## Interactive typesetting

The engine stays alive across edits and restores compatible checkpoints. Newly
typeset paragraphs may appear immediately while surrounding content is retained
from the previous revision. This feedback is provisional: subsequent line breaking,
floats or page assembly can move it. Whole-document completion takes longer than
the first visible paragraph update.

The editor retains successful auxiliary-file results and performs bounded
additional passes to settle references. It holds the job timestamp during typing;
opening a project or pressing **Compile** refreshes it. Large edits, preamble
changes and resource changes can require substantial recomputation.

Interactive error recovery helps show useful content while a command or group is
unfinished. Repairs are marked and do not qualify as fully correct output.
Recovered state is kept out of exact reuse and ordinary PDF/auxiliary promotion.
For strict compilation use `?recovery=strict`. Paragraph previews remain enabled
while complete pages and PDF output are computed.
Errors and warnings are reported without terminal prompts or requests to press
Return. Missing files must be downloaded or supplied as project resources.

## LaTeX, fonts and graphics

The browser uses a pinned LaTeX initialization with original TeX Live macros.
The packaged resource collection includes common classes, mathematics, graphics,
color, tables and bibliography formatting packages. Additional classes and
packages are resolved against the full current TeX Live catalog and downloaded
directly from a public CORS-enabled mirror. The browser verifies archive checksums,
unpacks runtime files locally, and loads dependencies. Required missing files
discovered during LaTeX execution use this same path, including computed package
names and dependencies not recorded in the catalog.

The status bar names the current download and shows a spinner. Storage settings
list cached packages and allow individual removal. Project files take precedence;
files outside TeX Live must be supplied with the project. The initialized kernel
and built-in resources remain pinned; an additional package requiring a newer
kernel or unsupported engine feature can still fail after a successful download.

Rendering supports physical font outlines, rules, transforms, clipping, colors,
PNG/JPEG images, PDF forms and supported graphics imported from PDF pages.
The Canvas preview and PDF writer are separate outputs; unsupported preview
painting can coexist with a valid PDF export.

An optional Unicode engine profile supports UTF-8, Unicode control sequences and
TrueType/OpenType shaping. It does not provide complete XeTeX/LuaTeX compatibility,
OpenType MATH, CFF Unicode fonts or full bidirectional paragraph layout.

## Current boundaries

- Output is PDF; there is no DVI output path.
- Browser jobs do not run shell commands, external converters, BibTeX or Biber.
  Supply generated `.bbl` files and directly supported graphics.
- Only packages present in TeX Live can be automatically resolved. Private classes,
  unpublished styles and project graphics must be included in the project.
- Repository access requires network access on first use. Cached packages can be
  reused offline; this does not make the entire website an offline-installable app.
- Some font, color and PDF features are not yet supported by the preview.
- Arbitrary non-UTF-8 physical filenames are not supported by the Wasm host.
- Editing latency and cancellation responsiveness depend on the operation and
  document; there is no universal sub-100-ms guarantee.

The engine and browser tests cover many workflows, but do not establish complete
compatibility with every TeX implementation or package.
