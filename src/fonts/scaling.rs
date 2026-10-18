//! TFM and PDF fixed-point rounding (`fonts/scaling.py`).
use crate::core::errors::{Error, Result};
pub const TFM_FRACTION_BITS: u32 = 20;
pub const SCALED_POINT_FRACTION_BITS: u32 = 16;
pub const FIX_WORD_TO_SCALED_SHIFT: u32 = 4;
pub const TFM_SIZE_PRECISION_BOUNDARY: i64 = 1 << 23;

pub fn scale_fix_word(value: i64, mut size: i64) -> i64 {
    let mut shift = 0;
    while size >= TFM_SIZE_PRECISION_BOUNDARY {
        size /= 2;
        shift += 1;
    }
    let product = value as i128 * size as i128;
    if shift <= TFM_FRACTION_BITS {
        (product >> (TFM_FRACTION_BITS - shift)) as i64
    } else {
        (product << (shift - TFM_FRACTION_BITS)) as i64
    }
}
pub fn scaled_division(value: i64, size: i64, decimals: u32) -> Result<(i64, i64)> {
    if size <= 0 || size >= 2_147_483_647 / 10 {
        return Err(Error::value("Invalid PDF font size"));
    }
    let scale = 10_i128
        .checked_pow(decimals)
        .ok_or_else(|| Error::value("Arithmetic overflow"))?;
    let sign = if value < 0 { -1 } else { 1 };
    let magnitude = (value as i128).abs();
    let (mut quotient, mut remainder) = (
        magnitude * scale / size as i128,
        magnitude * scale % size as i128,
    );
    if 2 * remainder >= size as i128 {
        quotient += 1;
        remainder -= size as i128;
    }
    let adjustment = remainder / scale;
    Ok((
        (sign * quotient) as i64,
        (sign * (magnitude - adjustment)) as i64,
    ))
}
pub fn divide_scaled(value: i64, size: i64, decimals: u32) -> Result<i64> {
    Ok(scaled_division(value, size, decimals)?.0)
}
pub fn pdf_font_size(size: i64) -> i64 {
    let mut remainder = (size as i128 * 1_000_000).rem_euclid(6_578_176);
    if 2 * remainder >= 6_578_176 {
        remainder -= 6_578_176;
    }
    size - (remainder / 1_000_000) as i64
}
