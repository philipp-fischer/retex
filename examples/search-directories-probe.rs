//! Native filesystem search observation for independent Python comparisons.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directories: Vec<_> = std::env::args()
        .skip(1)
        .map(|pattern| retex_rs2::resources::paths::SearchElement::new(pattern).directories())
        .collect();
    serde_json::to_writer(std::io::stdout(), &directories)?;
    Ok(())
}
