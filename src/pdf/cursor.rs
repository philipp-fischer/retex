//! Rounded PDF movements (`pdf/cursor.py`).
use crate::core::errors::Result;
use crate::core::units::SP_PER_PT;
use crate::fonts::scaling::{divide_scaled, scaled_division};
pub const COORDINATE_DECIMALS: u32 = 3;
pub const FONT_SIZE_DECIMALS: u32 = 4;
pub const WIDTH_RATIO_DECIMALS: u32 = 4;
pub const KERN_RATIO_DECIMALS: u32 = 3;
pub const SP_PER_HUNDRED_BP: i64 = SP_PER_PT * 803 / 8;
pub const TEXT_MATRIX_RESET_THRESHOLD: i64 = 1 << 15;
pub fn coordinate(displacement: i64, decimals: u32) -> Result<(i64, i64)> {
    scaled_division(displacement, SP_PER_HUNDRED_BP, decimals + 2)
}
pub fn expanded_amount(font_size: i64, ratio: i64, decimals: u32, expansion: i64) -> Result<i64> {
    let unexpanded = divide_scaled(font_size * ratio.abs(), 10_i64.pow(decimals), 0)?;
    let expanded = divide_scaled(unexpanded * (1000 + expansion), 1000, 0)?;
    Ok(if ratio < 0 { -expanded } else { expanded })
}
pub fn kern(mut displacement: i64, font_size: i64, expansion: i64) -> Result<(i64, i64)> {
    if expansion != 0 {
        displacement = divide_scaled(displacement * 1000, 1000 + expansion, 0)?;
    }
    let (ratio, mut represented) = scaled_division(displacement, font_size, KERN_RATIO_DECIMALS)?;
    if expansion != 0 && ratio.abs() < TEXT_MATRIX_RESET_THRESHOLD {
        represented = expanded_amount(font_size, ratio, KERN_RATIO_DECIMALS, expansion)?;
    }
    Ok((ratio, represented))
}
pub trait CursorFont {
    type BitmapOptions;
    fn char_width(&self, code: u32) -> i64;
    fn bitmap_advance(&self, code: u32, options: &Self::BitmapOptions) -> Result<i64>;
}
pub fn character_advance<F: CursorFont>(
    font: &F,
    code: u32,
    font_size: i64,
    expansion: i64,
    bitmap: Option<&F::BitmapOptions>,
) -> Result<i64> {
    if let Some(options) = bitmap {
        return font.bitmap_advance(code, options);
    }
    let mut width = font.char_width(code);
    if expansion != 0 {
        width = divide_scaled(width * 1000, 1000 + expansion, 0)?;
    }
    let (ratio, represented) = scaled_division(width, font_size, WIDTH_RATIO_DECIMALS)?;
    if expansion != 0 {
        expanded_amount(font_size, ratio, WIDTH_RATIO_DECIMALS, expansion)
    } else {
        Ok(represented)
    }
}
pub fn resets_matrix(changed_font: bool, vertical: i64, adjustment: i64) -> bool {
    changed_font || vertical != 0 || adjustment.unsigned_abs() >= TEXT_MATRIX_RESET_THRESHOLD as u64
}
