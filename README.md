# ReTeX

**Edit LaTeX and see it typeset in your browser.** ReTeX is a Rust implementation
of TeX, compiled to WebAssembly, with an editor built for interactive feedback.
Compilation runs locally in a browser worker; documents do not need to be uploaded
to a compilation server.

- Open a `.tex` file or a project folder with sources, fonts and graphics.
- See paragraph updates while the rest of the document finishes in the background.
- Navigate between source and preview, inspect diagnostics, and download a PDF.
- Use bundled LaTeX resources and download additional TeX Live packages on demand.
- Host the complete browser app as static files, including on GitHub Pages.

ReTeX is experimental. Package compatibility and preview support are still
evolving, and editing latency depends on the document. See
[features and limitations](docs/FEATURES.md) before relying on a particular workflow.

## Run the browser app

Build prerequisites: Rust, Python 3, Node.js 22.12+, `wasm-bindgen-cli` 0.2.129,
and WASI SDK 34. Follow [the setup instructions](docs/BUILDING.md) to install them.
From this directory:

```sh
cargo build --release --bin retex-rs2
python3 tools/build_browser_format.py
cargo build --release --target wasm32-unknown-unknown --lib
python3 tools/build_browser.py --latex --common-papers --format .tools/formats/latex.rsfmt
npm --prefix web ci
npm --prefix web run package
python3 -m http.server --bind 127.0.0.1 --directory dist/workspace 8000
```

Open [localhost:8000](http://localhost:8000). The first build downloads additional
TeX Live packages; subsequent builds can use the local cache. No separate TeX Live
installation or Python reference implementation is required.

The output directory, `dist/workspace`, is the complete static site.
[Deployment instructions](docs/DEPLOYMENT.md) cover GitHub Pages, package loading
and local hosting.

## Learn more

| Topic | Guide |
| --- | --- |
| Editor, packages, graphics and current limitations | [Features](docs/FEATURES.md) |
| Build setup and frontend development | [Building](docs/BUILDING.md) |
| Static hosting and GitHub Pages | [Deployment](docs/DEPLOYMENT.md) |
| Engine, worker, checkpoints and rendering | [Architecture](docs/ARCHITECTURE.md) |
| Tests and optional reference comparisons | [Development](docs/DEVELOPMENT.md) |

## Native use

The same engine also runs locally: `cargo build --release --bin retex-rs2`, then
`target/release/retex-rs2 examples/hello.tex hello.pdf`.
See [native usage](docs/NATIVE.md) for LaTeX formats and resource configuration.

## License

The engine is distributed under [GPL-2.0-or-later](COPYING).
Bundled fonts, TeX Live resources and other dependencies retain their own
licenses and notices; see [third-party notices](NOTICE.md).
