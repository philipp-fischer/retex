//! Immutable TeX quantities and exact glue arithmetic (`core/values.py`).
use super::errors::{Error, Result};
use super::units::print_scaled;
use serde::{Deserialize, Serialize};

pub const MAX_INT: i64 = (1 << 31) - 1;
pub const MAX_DIMEN: i64 = (1 << 30) - 1;

pub fn checked(value: i128, dimension: bool) -> Result<i64> {
    let limit = if dimension { MAX_DIMEN } else { MAX_INT } as i128;
    if !(-limit..=limit).contains(&value) {
        return Err(Error::value(if dimension {
            "Dimension too large"
        } else {
            "Arithmetic overflow"
        }));
    }
    Ok(value as i64)
}

pub fn trunc_div(value: i128, divisor: i128) -> Result<i128> {
    if divisor == 0 {
        return Err(Error::value("Division by zero"));
    }
    value
        .checked_div(divisor)
        .ok_or_else(|| Error::value("Arithmetic overflow"))
}

/// Python's floor division, including opposite-signed operands.
pub fn floor_div(value: i128, divisor: i128) -> Result<i128> {
    let quotient = trunc_div(value, divisor)?;
    Ok(quotient - i128::from(value % divisor != 0 && (value < 0) != (divisor < 0)))
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct GlueSpec {
    pub width: i64,
    pub stretch: i64,
    pub shrink: i64,
    pub stretch_order: u8,
    pub shrink_order: u8,
    /// Literal zero glue blocks protrusion; this is excluded from equality.
    pub protrusion_barrier: bool,
}

impl PartialEq for GlueSpec {
    fn eq(&self, other: &Self) -> bool {
        (
            self.width,
            self.stretch,
            self.shrink,
            self.stretch_order,
            self.shrink_order,
        ) == (
            other.width,
            other.stretch,
            other.shrink,
            other.stretch_order,
            other.shrink_order,
        )
    }
}
impl Eq for GlueSpec {}

impl GlueSpec {
    pub fn scaled(self, numerator: i64, denominator: i64) -> Result<Self> {
        let scale = |v: i64| {
            checked(
                trunc_div(v as i128 * numerator as i128, denominator as i128)?,
                true,
            )
        };
        Ok(Self {
            width: scale(self.width)?,
            stretch: scale(self.stretch)?,
            shrink: scale(self.shrink)?,
            stretch_order: self.stretch_order,
            shrink_order: self.shrink_order,
            protrusion_barrier: true,
        })
    }

    pub fn added(self, other: Self) -> Result<Self> {
        fn component(a: i64, mut ao: u8, b: i64, mut bo: u8) -> Result<(i64, u8)> {
            if a == 0 {
                ao = 0;
            }
            if b == 0 {
                bo = 0;
            }
            if ao == bo {
                Ok((checked(a as i128 + b as i128, true)?, ao))
            } else if ao > bo {
                Ok((a, ao))
            } else {
                Ok((b, bo))
            }
        }
        let (stretch, stretch_order) = component(
            self.stretch,
            self.stretch_order,
            other.stretch,
            other.stretch_order,
        )?;
        let (shrink, shrink_order) = component(
            self.shrink,
            self.shrink_order,
            other.shrink,
            other.shrink_order,
        )?;
        Ok(Self {
            width: checked(self.width as i128 + other.width as i128, true)?,
            stretch,
            shrink,
            stretch_order,
            shrink_order,
            protrusion_barrier: true,
        })
    }

    pub fn text(self, mu: bool) -> String {
        let unit = if mu { "mu" } else { "pt" };
        let part = |value, order| {
            format!(
                "{}{}",
                print_scaled(value),
                match order {
                    0 => unit,
                    1 => "fil",
                    2 => "fill",
                    3 => "filll",
                    _ => unreachable!("invalid glue order"),
                }
            )
        };
        let mut result = part(self.width, 0);
        if self.stretch != 0 {
            result.push_str(&format!(" plus {}", part(self.stretch, self.stretch_order)));
        }
        if self.shrink != 0 {
            result.push_str(&format!(" minus {}", part(self.shrink, self.shrink_order)));
        }
        result
    }
}

pub fn badness(amount: i64, available: i64) -> i64 {
    if amount == 0 {
        return 0;
    }
    if available <= 0 {
        return 10_000;
    }
    let r = if amount <= 7_230_584 {
        amount as i128 * 297 / available as i128
    } else if available >= 1_663_497 {
        (amount / (available / 297)) as i128
    } else {
        amount as i128
    };
    if r > 1290 {
        10_000
    } else {
        ((r * r * r + 131_072) / 262_144) as i64
    }
}

pub fn round_ratio(numerator: i128, denominator: i128) -> Result<i128> {
    floor_div(2 * numerator + denominator, 2 * denominator)
}
