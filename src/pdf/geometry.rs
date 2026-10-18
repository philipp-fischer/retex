//! Page magnification and historical arithmetic (`pdf/geometry.py`).
use crate::core::errors::Result;
use crate::core::values::round_ratio;
pub const MAGNIFICATION_BASE: i64 = 1000;
pub const HISTORICAL_PRODUCT_RADIX: i128 = 1 << 15;
pub fn magnified_dimension(value: i64, magnification: i64, exact_repro: bool) -> Result<i128> {
    if magnification == MAGNIFICATION_BASE {
        return Ok(value as i128);
    }
    if !exact_repro {
        return round_ratio(
            value as i128 * magnification as i128,
            MAGNIFICATION_BASE as i128,
        );
    }
    let radix = HISTORICAL_PRODUCT_RADIX;
    let (high, low) = ((value as i128).abs() / radix, (value as i128).abs() % radix);
    let product = low * magnification as i128;
    let (carry, low) = (product.div_euclid(radix), product.rem_euclid(radix));
    let intermediate = high * magnification as i128 + carry;
    let (quotient, remainder) = (intermediate.div_euclid(1000), intermediate.rem_euclid(1000));
    let remainder = remainder * radix + low;
    let mut result = if quotient >= radix {
        intermediate
    } else {
        radix * quotient + remainder.div_euclid(1000)
    };
    if 2 * remainder.rem_euclid(1000) >= 1000 {
        result += 1;
    }
    Ok(if value < 0 { -result } else { result })
}
