//! Stable resource anchors and explicit job overrides (`resources/locations.py`).
use crate::core::config::EngineConfig;
use crate::core::errors::{Error, Result};
use std::path::{Component, Path, PathBuf};
pub fn package_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets")
}
pub fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
#[derive(Clone, Debug)]
pub struct ResourceLocations {
    /// Physical bundle anchor for portable initialized-format resource locators.
    pub engine_root: PathBuf,
    pub font_assets: PathBuf,
    pub font_metrics: PathBuf,
    pub input_roots: Vec<(PathBuf, bool)>,
    pub configuration_paths: Option<Vec<PathBuf>>,
    pub reference_root: PathBuf,
}
impl Default for ResourceLocations {
    fn default() -> Self {
        let repo = repository_root();
        Self {
            engine_root: package_root(),
            font_assets: package_root().join("fonts/assets"),
            font_metrics: package_root().join("fonts/metrics"),
            input_roots: vec![
                (package_root().join("tex"), false),
                (package_root().join("latex-texmf/tex"), true),
            ],
            configuration_paths: None,
            reference_root: repo,
        }
    }
}
impl ResourceLocations {
    pub fn configured(config: &EngineConfig) -> Result<Self> {
        let mut locations = Self::default();
        if let Some(root) = &config.resource_root {
            let root = PathBuf::from(root);
            locations.engine_root = root.clone();
            locations.reference_root = root.clone();
            locations.font_assets = root.join("fonts/assets");
            locations.font_metrics = root.join("fonts/metrics");
            locations.input_roots = vec![(root.join("tex"), true)];
        }
        if let Some(directory) = &config.font_dir {
            locations.font_assets =
                super::paths::resolve_path(&super::paths::expand_user(directory));
        }
        if let Some(roots) = &config.tex_input_roots {
            locations.input_roots = roots.iter().map(|root| (root.into(), false)).collect();
        }
        locations.configuration_paths = config
            .configuration_paths
            .as_ref()
            .map(|paths| paths.iter().map(PathBuf::from).collect());
        Ok(locations)
    }
}
pub fn engine_resource(relative: impl AsRef<Path>) -> Result<PathBuf> {
    let relative = relative.as_ref();
    if crate::resources::host::is_absolute(&relative)
        || relative
            .components()
            .any(|part| part == Component::ParentDir)
    {
        return Err(Error::value("Invalid format resource locator"));
    }
    Ok(package_root().join(relative))
}
