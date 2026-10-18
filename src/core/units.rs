//! Exact unit conversions and WEB decimal arithmetic (`core/units.py`).

pub const SP_PER_PT: i64 = 65_536;

pub fn sp_to_bp(value: i64) -> f64 {
    value as f64 * 800.0 / (803.0 * SP_PER_PT as f64)
}

pub fn bp_to_sp(value: f64) -> i64 {
    (value * 803.0 * SP_PER_PT as f64 / 800.0).round_ties_even() as i64
}

/// The shortest WEB spelling that scans back to this scaled integer.
pub fn print_scaled(value: i64) -> String {
    let sign = if value < 0 { "-" } else { "" };
    // Widen before negating so the host minimum does not overflow.
    let magnitude = (value as i128).abs();
    let unity = SP_PER_PT as i128;
    let whole = magnitude / unity;
    let mut scaled = 10 * (magnitude % unity) + 5;
    let mut delta = 10;
    let mut result = format!("{sign}{whole}.");
    loop {
        if delta > unity {
            scaled += unity / 2 - 50_000;
        }
        result.push(char::from(b'0' + (scaled / unity) as u8));
        scaled = 10 * (scaled % unity);
        delta *= 10;
        if scaled <= delta {
            return result;
        }
    }
}

pub fn round_decimals(digits: &[u8]) -> i64 {
    let mut accumulator = 0;
    for &digit in digits.iter().take(17).rev() {
        accumulator = (accumulator + i64::from(digit) * 2 * SP_PER_PT) / 10;
    }
    (accumulator + 1) / 2
}
