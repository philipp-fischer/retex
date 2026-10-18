# Development and testing

Build and test the public repository independently of the Python reference:

```sh
cargo fmt --all -- --check
cargo test --all-targets
python3 -m unittest discover -s tools -p 'test_*.py'
npm --prefix web run typecheck
npm --prefix web test
```

Native integration fixtures are in `tests/fixtures`. Runtime macros and font
resources are in `assets`, with upstream sources and notices retained.
Tests should exercise behavior and correctness boundaries, including invalidation,
effects, stale revisions, error recovery and exported output.

## Browser smoke test

After a [complete browser build](BUILDING.md), install Playwright in a development
virtual environment and run:

```sh
python3 -m venv .tools/browser-tests
.tools/browser-tests/bin/pip install playwright
.tools/browser-tests/bin/playwright install --with-deps chromium
.tools/browser-tests/bin/python tools/browser_smoke.py
```

The smoke test hosts the static artifact under a project subpath, loads Wasm,
typesets LaTeX, edits the source and downloads a PDF. It rejects external requests,
HTTP failures and browser errors. This checks the deployment boundary without
requiring a compilation server or private reference data.

Add `--live-packages` to exercise the full repository fallback, computed class
and package names, runtime missing-file requests, and individual cache removal.
This optional test needs internet access and allows downloads only from the
configured TeX Live mirror hosts; the default smoke test stays self-contained.

## Optional private Python reference

The independent Python implementation, differential runners, profiling experiments
and historical investigation reports are maintained in a separate development
repository. They are not required to build, use or test the public app.
Obtain access to that repository separately; it has no assumed public clone URL.

Its location is independent of this checkout. Set the public checkout path when
running its tools:

```sh
export RETEX_ROOT=/absolute/path/to/retex-rs2
cd /absolute/path/to/retex-python
python3 -m venv .venv
.venv/bin/pip install -r texk/web2c/pdftexdir/requirements.txt
.venv/bin/python tools/differential.py \
  --binary "$RETEX_ROOT/target/release/retex-rs2" \
  --output /tmp/retex-differential
```

The private repository retains the reference engine's fixture layout internally;
neither repository must be nested inside the other or inside a TeX Live checkout.
Additional browser/PDF oracle tools require Playwright, Poppler and their stated
dependencies. A standard upstream TeX Live clone does not supply the Python engine.

When comparing engines, align resources, time, auxiliary-pass inputs and options.
Compare diagnostics and file effects as well as geometry and PDF semantics. Keep
reference execution and diagnostic profiling outside latency measurements.
