# Building ReTeX

The browser build is self-contained. Its pinned LaTeX kernel, basic macros,
fonts and associated sources/licenses are in `assets/`. Additional packages
used for the prepared resource collection are downloaded from frozen TeX Live
2024 archives and checksum-verified. At runtime, the browser can resolve other
packages directly from the full current TeX Live repository; these archives are
verified and unpacked in the worker. The bundled initialized kernel is not
silently upgraded when an additional package is downloaded.

The optional Python reference repository is not a build dependency.

## Prerequisites

Linux with glibc is the currently tested native build environment. Install a C
compiler and zlib development files (on Debian/Ubuntu: `build-essential zlib1g-dev`).
Use a current stable Rust toolchain; the crate declares Rust 1.85 as its minimum,
but that minimum has not been separately validated for all locked dependencies.
Also install Python 3.10+ and Node.js 22.12+.

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
```

Download [WASI SDK 34](https://github.com/WebAssembly/wasi-sdk/releases/tag/wasi-sdk-34)
for your build host and set `RETEX_WASI_SDK` to its extracted directory:

```sh
export RETEX_WASI_SDK=/absolute/path/to/wasi-sdk-34.0-x86_64-linux
```

The SDK compiles a small C regular-expression dependency. The resulting browser
module does not require a WASI runtime. The archive checksum and dependency
licenses are documented in [native-regex](../native-regex/README.md).

## Complete browser build

Run these commands at the repository root:

```sh
cargo build --release --bin retex-rs2
python3 tools/build_browser_format.py
cargo build --release --target wasm32-unknown-unknown --lib
python3 tools/build_browser.py --latex --common-papers --format .tools/formats/latex.rsfmt
npm --prefix web ci
npm --prefix web run typecheck
npm --prefix web test
npm --prefix web run package
```

The native binary initializes LaTeX once and writes `.tools/formats/latex.rsfmt`.
Packaging embeds this format, fonts, resources and the compiled Wasm module in
`web/generated/`. Vite builds the UI and the packaging script assembles everything
into `dist/workspace/`. Serve that directory over HTTP, not with a `file:` URL.

```sh
python3 -m http.server --bind 127.0.0.1 --directory dist/workspace 8000
```

`tools/build_browser.py --offline` reuses cached package archives in
`.tools/texlive`; it fails if a required archive is absent. Cargo and npm also
need their dependencies available before a fully offline build. Generated files,
download caches, installed tools and build outputs are ignored by Git.

## Development cycle

For UI changes, run `npm --prefix web run dev`. The dev server uses an existing
`web/generated/` bundle; it does not compile Rust. After changing the engine:

```sh
cargo build --release --target wasm32-unknown-unknown --lib
python3 tools/build_browser.py --wasm-only
npm --prefix web run package
```

Rebuild the initialized format and full resource bundle when format compatibility,
initialization behavior or resources change. A saved format is tied to compatible
engine state and resource bytes, not just its filename.

`./dev` is an optional Cargo wrapper. It uses Cargo on `PATH`, or a toolchain
installed at `.tools/toolchain`, and reuses a checkout-local Cargo cache if present.
Ordinary Cargo builds do not require this wrapper or another checkout.
