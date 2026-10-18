fn main() {
    std::process::exit(retex_rs2::diagnostics::inspector::main(
        &std::env::args().skip(1).collect::<Vec<_>>(),
    ));
}
