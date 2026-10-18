# Native use

The native executable uses the same Rust engine as the browser. Linux/glibc is
the currently tested native platform. Install Rust, a C compiler and zlib
development files, then build from the repository root:

```sh
cargo build --release --bin retex-rs2
target/release/retex-rs2 examples/hello.tex hello.pdf
target/release/retex-rs2 --help
```

No Python implementation or external TeX engine is invoked. Basic macros and
fonts are bundled. The development executable locates those assets relative to
the crate's build-time location; copying only the binary to another machine is
not a complete installation. Use explicit resource paths when packaging it.

## LaTeX

Initialize the bundled kernel once, then reuse its format:

```sh
python3 tools/build_browser_format.py
target/release/retex-rs2 --format .tools/formats/latex.rsfmt paper.tex paper.pdf
```

The browser and native engine share `.rsfmt` support. These files are versioned
engine snapshots, not upstream TeX `.fmt` files. Rebuild them after incompatible
engine or resource changes. A simple format example is included:

```sh
target/release/retex-rs2 --ini --no-pdf --dump-format example.rsfmt examples/format-init.tex
target/release/retex-rs2 --format example.rsfmt examples/format-use.tex example.pdf
```

## Resource paths

Use `--tex-input-root`, `--font-dir`, `--configuration-path` and standard TeX
environment variables to select additional resources. `TEXINPUTS` supports
recursive `//` lookup. For example, to add macros from an independently installed
TeX Live distribution:

```sh
export TEXINPUTS=.:/absolute/path/to/texmf-dist/tex//
target/release/retex-rs2 --format .tools/formats/latex.rsfmt paper.tex paper.pdf
```

Font-dependent packages may also need `TFMFONTS`, `T1FONTS`, `ENCFONTS`,
`VFFONTS` and `TEXFONTMAPS`. Use matching resource versions; an arbitrary newer
package tree may not match the bundled kernel. A TeX Live source-code clone is
not a substitute for an installed runtime package tree.

## Embedding and inspection

`session::EditingSession` retains an engine, checkpoints and resources across
edits. Native interactive hosts should use it rather than starting a process per
keystroke. The `session-host` binary runs scripted session requests; `preview-host`
emits preview scenes. These are developer interfaces, not a separate desktop UI.

`--dump-layout`, `--trace` and `inspect-layout` expose execution and layout data.
`--exact-repro` selects reproducible behavior. Native compilation defaults to
strict error handling; `--recovery strict|classical|interactive` selects recovery
policy explicitly. Recovered preview output is not ordinary complete PDF output.
Compilation never waits for an error response on the terminal. Reads from missing
or terminal streams fail with a diagnostic; `-` still reads the source document
from standard input. TeX interaction-mode primitives only control console output.
