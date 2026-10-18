//! TeX character identities, control-sequence meanings, scopes and scanners.
pub mod assignments;
pub mod character_tokens;
pub mod code_validation;
pub mod commands;
pub mod conditionals;
pub mod conversions;
pub mod expansion;
pub mod expressions;
pub mod extended;
pub mod filename;
pub mod font_commands;
pub mod font_execution;
pub(crate) mod form_execution;
pub mod groups;
pub mod input;
pub mod input_adapters;
pub mod input_frames;
pub mod internal_values;
pub mod numeric_codes;
pub mod pdf_commands;
pub mod physical_input;
pub mod posix_regex;
mod primitive_data;
pub mod primitives;
pub mod random_numbers;
pub mod records;
pub mod retained_text;
pub mod scanning;
pub mod services;
pub mod specials;
pub mod state;
pub mod stream_execution;
pub mod streams;
pub mod token_input;
pub mod tokens;

pub(crate) mod image_execution;
pub(crate) mod navigation_execution;
pub mod pdf_extensions;
pub mod pdf_navigation;
pub(crate) mod pdf_operand_execution;
pub(crate) mod pdf_scan_execution;

pub(crate) mod meaning_memo;

pub(crate) mod observed_table;
