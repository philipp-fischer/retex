# Exact GNU byte regular expressions

Linux/glibc builds use the system library in the reference's C locale. Wasm
builds compile unchanged GNU glibc 2.39 regex and dynarray sources, recorded by
URL, size and SHA-256 in `vendor/glibc-2.39/provenance.json`. GNU extensions,
leftmost-longest captures, backreferences and compile errors use that algorithm.
`portable/libc-config.h` selects standalone single-byte C-locale behavior and
prefixes symbols so native tests can link an independent libc oracle. Its
collating-symbol macro adapts the upstream standalone two-argument declaration
to an upstream call also passing libc-only tables; it does not replace matching.

For Wasm, install [WASI SDK 34](https://github.com/WebAssembly/wasi-sdk/releases/tag/wasi-sdk-34)
and set `RETEX_WASI_SDK` to its extracted directory. The Linux x86-64 archive
SHA-256 is `b761e3a0721dbae9c09a0059e5fdb2bf917d1b4a8a7b430fb3b5aafb0984b2c4`.
The default checkout location is `../.tools/wasi-sdk-34.0-x86_64-linux` relative
to this package. The build uses clang/llvm-ar and its static C helpers, without
WASI runtime imports. `src/wasm_alloc.rs` routes C allocations through Rust's
heap with checked sizes and aligned private headers, preventing overlapping
independent heaps. The browser matcher remains a synchronous operation;
cooperative cancellation inside long matching calls is still required.

GNU source license and notices are in `vendor/glibc-2.39/COPYING.LIB` and each
source file. Statically linked C-helper notices from the SDK's pinned wasi-libc
revision are under `vendor/wasi-libc-licenses`. Rebuild/relink the module from
these sources with the regular Cargo/wasm-bindgen commands in the [build guide](../docs/BUILDING.md).

Native validation (from the repository root):

```sh
cargo test --manifest-path native-regex/Cargo.toml --features portable-test
cargo build --release --target wasm32-unknown-unknown --example regex-wasm-probe
```

Browser regression investigations and independent Python/libc comparisons live in
the optional private reference repository. See [development](../docs/DEVELOPMENT.md).
