//! Font records, exact scaling and binary resource readers.
pub mod catalog;
pub(crate) mod construction;
pub mod encoding;
pub mod expansion;
pub mod formats;
pub mod glyph_search;
pub mod ligatures;
pub(crate) mod map_preparation;
pub mod maps;
pub mod materialization;
pub mod microtypography;
pub mod model;
pub mod outlines;
pub mod resolve;
pub mod resources;
pub mod scaling;
pub mod subfonts;
pub mod unicode;

pub mod generate_spacefont;
