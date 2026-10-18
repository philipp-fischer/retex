//! PDF syntax, resources, emission and document serialization.
pub mod cursor;
pub mod extraction;
pub mod font_embedding;
pub mod geometry;
pub mod graphics;
pub mod image_records;
pub mod jbig2;
pub mod raster;
pub mod resources;
pub mod syntax;
pub mod text;
pub mod unicode_fonts;
pub mod unicode_maps;
pub mod writer;

pub mod backend;
pub mod document;
pub mod emission;
pub mod forms;
pub mod images;
pub mod imports;
pub mod inclusion_fonts;
pub mod navigation;
pub mod parsing;
pub mod preview;

mod device_color;
pub mod preview_images;
