//! Language quantities and transitional addresses (`core/constants.py`).
use super::units::bp_to_sp;

pub struct Constants;
impl Constants {
    pub const ETEX_VERSION: i64 = 2;
    pub const ETEX_REVISION: &'static str = ".6";
    pub const PDFTEX_VERSION: i64 = 140;
    pub const PDFTEX_REVISION: &'static str = "25";
    pub const BANNER: &'static str = "This is ReTeX, Version 0.1.0";
    pub const PDFTEX_FULL_BANNER: &'static str =
        "This is pdfTeX, Version 3.141592653-2.6-1.40.25 (Web2C 2024) kpathsea version 6.3.6/dev";
    pub const HASH_SIZE: i64 = 100_000;
    pub const MIN_HALFWORD: i64 = -0xfffffff;
    pub const NULL: i64 = Self::MIN_HALFWORD;
    pub const BYTE_VALUES: i64 = 256;
    pub const MAX_BYTE: i64 = 255;
    pub const MAX_FILE_STREAM: i64 = 15;
    pub const TERMINAL_WRITE_STREAM: i64 = 16;
    pub const SHELL_WRITE_STREAM: i64 = 18;
    pub const OUTPUT_BOX_REGISTER: i64 = 255;
    pub const MAX_INSERTION_REGISTER: i64 = 254;
    pub const LEFT_BRACE_TOKEN: i64 = 256 + b'{' as i64;
    pub const RIGHT_BRACE_TOKEN: i64 = 512 + b'}' as i64;
    pub const OUT_PARAM_TOKEN: i64 = 0x2400;
    pub const LEVEL_ZERO: usize = 0;
    pub const LEVEL_ONE: usize = 1;
    pub const ACTIVE_BASE: i64 = 1;
    pub const SINGLE_BASE: i64 = 257;
    pub const NULL_CS: i64 = 513;
    pub const HASH_BASE: i64 = 514;
    pub const FONT_ID_BASE: i64 = Self::HASH_BASE + Self::HASH_SIZE + 1000;
    pub const FROZEN_PRIMITIVE: i64 = Self::FONT_ID_BASE - 1;
    pub const UNITY: i64 = 65_536;
    pub fn page_width() -> i64 {
        bp_to_sp(612.0)
    }
    pub fn page_height() -> i64 {
        bp_to_sp(792.0)
    }
    pub fn page_margin() -> i64 {
        bp_to_sp(72.0)
    }
}
