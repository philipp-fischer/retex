fn main() {
    let directory = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| retex_rs2::resources::locations::package_root().join("fonts/assets"));
    if let Err(error) = retex_rs2::fonts::generate_spacefont::generate(&directory) {
        eprintln!("{error}");
        std::process::exit(1)
    }
}
