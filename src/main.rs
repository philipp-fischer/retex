fn main() {
    std::process::exit(retex_rs2::cli::main(
        &std::env::args().skip(1).collect::<Vec<_>>(),
    ));
}
