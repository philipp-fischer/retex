fn main() {
    println!("cargo:rerun-if-changed=regex.c");
    let wasm = std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32");
    let portable_test = std::env::var_os("CARGO_FEATURE_PORTABLE_TEST").is_some();
    if wasm || portable_test {
        println!("cargo:rerun-if-changed=portable");
        println!("cargo:rerun-if-changed=vendor");
        let mut build = cc::Build::new();
        build
            .flag("-Wno-sign-compare")
            .flag("-Wno-unused-parameter")
            .include("portable")
            .include("vendor/glibc-2.39")
            .include("vendor/glibc-2.39/include")
            .include("vendor/glibc-2.39/malloc")
            .flag("-include")
            .flag("portable/libc-config.h")
            .file("portable/regex.c")
            .file("vendor/glibc-2.39/regex.c");
        for name in ["emplace_enlarge", "resize", "resize_clear", "finalize"] {
            build.file(format!("vendor/glibc-2.39/malloc/dynarray_{name}.c"));
        }
        if wasm {
            let sdk = std::env::var_os("RETEX_WASI_SDK")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| {
                    std::path::PathBuf::from("../.tools/wasi-sdk-34.0-x86_64-linux")
                });
            let sdk = sdk
                .canonicalize()
                .expect("Set RETEX_WASI_SDK to an installed WASI SDK");
            build
                .compiler(sdk.join("bin/clang"))
                .archiver(sdk.join("bin/llvm-ar"))
                .flag("--target=wasm32-wasip1");
            println!("cargo:rerun-if-env-changed=RETEX_WASI_SDK");
            println!(
                "cargo:rustc-link-search=native={}",
                sdk.join("share/wasi-sysroot/lib/wasm32-wasip1").display()
            );
            println!("cargo:rustc-link-lib=static=c");
        }
        build.compile("retex_regex");
        if portable_test {
            cc::Build::new()
                .file("regex.c")
                .define("retex_regex_match", "retex_libc_regex_match")
                .define("retex_regex_free", "retex_libc_regex_free")
                .compile("retex_libc_regex");
        }
    } else if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu")
    {
        cc::Build::new().file("regex.c").compile("retex_regex");
    }
}
