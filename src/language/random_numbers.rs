//! Job-owned subtractive random streams (`language/random_numbers.py`).
use crate::core::errors::{Error, Result};
use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};
use std::time::UNIX_EPOCH;
pub const FRACTION_ONE: i64 = 1 << 28;
pub const FRACTION_HALF: i64 = 1 << 27;
pub const FRACTION_FOUR: i64 = 1 << 30;
pub const SCALED_UNIT: i64 = 1 << 16;
pub const LOG_SCALE: i64 = 1 << 24;
pub const NORMAL_ACCEPTANCE_SCALE: i64 = 1024 * LOG_SCALE;
pub const SPEC_LOG: [i64; 29] = [
    0, 93032640, 38612034, 17922280, 8662214, 4261238, 2113709, 1052693, 525315, 262400, 131136,
    65552, 32772, 16385, 8192, 4096, 2048, 1024, 512, 256, 128, 64, 32, 16, 8, 4, 2, 1, 1,
];

pub fn take_fraction(value: &BigInt, fraction: &BigInt) -> BigInt {
    let magnitude: BigInt = (value.abs() * fraction.abs() + FRACTION_HALF) / FRACTION_ONE;
    if value.is_negative() != fraction.is_negative() {
        -magnitude
    } else {
        magnitude
    }
}
pub fn make_fraction(value: &BigInt, denominator: &BigInt) -> Result<BigInt> {
    if denominator.is_zero() {
        return Err(Error::value("Division by zero"));
    }
    let magnitude: BigInt =
        (value.abs() * FRACTION_ONE * 2 + denominator.abs()) / (2 * denominator.abs());
    Ok(if value.is_negative() != denominator.is_negative() {
        -magnitude
    } else {
        magnitude
    })
}
pub enum Logarithm {
    Modern(f64),
    Exact(BigInt),
}
pub fn logarithm(value: &BigInt, exact_repro: bool) -> Result<Logarithm> {
    if value <= &BigInt::zero() {
        return Err(Error::value("Logarithm input must be a positive integer"));
    }
    if exact_repro {
        Ok(Logarithm::Exact(exact_logarithm(value)))
    } else {
        let log = match value.to_f64().filter(|v| v.is_finite()) {
            Some(value) => value.ln(),
            None => {
                let bits = value.bits();
                let shift = bits.saturating_sub(53);
                let top = value >> shift as usize;
                let low = value - (&top << shift as usize);
                let mut rounded = top.to_u64().expect("53-bit mantissa");
                if shift != 0 {
                    let halfway = BigInt::from(1_u8) << (shift as usize - 1);
                    if low > halfway || (low == halfway && rounded & 1 != 0) {
                        rounded += 1;
                    }
                }
                (rounded as f64).ln() + shift as f64 * std::f64::consts::LN_2
            }
        };
        Ok(Logarithm::Modern(
            LOG_SCALE as f64 * (log - (SCALED_UNIT as f64).ln()),
        ))
    }
}
pub fn exact_logarithm(value: &BigInt) -> BigInt {
    let mut value = value.clone();
    let mut y = BigInt::from(1_302_456_956_i64 + 4 - 100);
    let mut z = BigInt::from(27_595 + 6_553_600_i64);
    while value < BigInt::from(FRACTION_FOUR) {
        value *= 2;
        y -= 93_032_639;
        z -= 48_782;
    }
    y += &z / 65_536;
    let mut k = 2_usize;
    while value > BigInt::from(FRACTION_FOUR + 4) {
        z = ((&value - 1) >> k) + 1;
        while value < BigInt::from(FRACTION_FOUR) + &z {
            z = (&z + 1) / 2;
            k += 1;
        }
        y += SPEC_LOG[k];
        value -= &z;
    }
    y / 8
}
#[derive(Clone, Debug)]
pub struct RandomNumbers {
    pub exact_repro: bool,
    pub seed: BigInt,
    pub values: [i64; 55],
    pub index: usize,
}
impl RandomNumbers {
    pub fn new(exact_repro: bool) -> Self {
        let micros = crate::resources::host::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros();
        let seconds = micros / 1_000_000;
        let seed = micros % 1_000_000 * 1000 + seconds % 1_000_000;
        Self::seeded(BigInt::from(seed), exact_repro)
    }
    pub fn seeded(seed: BigInt, exact_repro: bool) -> Self {
        let mut stream = Self {
            exact_repro,
            seed: BigInt::zero(),
            values: [0; 55],
            index: 0,
        };
        stream.reset(seed);
        stream
    }
    pub fn reset(&mut self, seed: BigInt) {
        self.seed = seed.abs();
        let mut initial = self.seed.clone();
        while initial >= BigInt::from(FRACTION_ONE) {
            initial >>= 1;
        }
        let (mut j, mut k) = (initial.to_i64().expect("normalized random seed"), 1);
        self.values = [0; 55];
        for index in 0..55 {
            (j, k) = (k, (j - k).rem_euclid(FRACTION_ONE));
            self.values[(index * 21) % 55] = j;
        }
        for _ in 0..3 {
            self.new_batch();
        }
    }
    pub fn new_batch(&mut self) {
        for index in 0..24 {
            self.values[index] =
                (self.values[index] - self.values[index + 31]).rem_euclid(FRACTION_ONE);
        }
        for index in 24..55 {
            self.values[index] =
                (self.values[index] - self.values[index - 24]).rem_euclid(FRACTION_ONE);
        }
        self.index = 54;
    }
    pub fn next(&mut self) -> i64 {
        if self.index == 0 {
            self.new_batch();
        } else {
            self.index -= 1;
        }
        self.values[self.index]
    }
    pub fn uniform(&mut self, extent: &BigInt) -> BigInt {
        let result = take_fraction(&extent.abs(), &BigInt::from(self.next()));
        if result == extent.abs() {
            BigInt::zero()
        } else if extent > &BigInt::zero() {
            result
        } else {
            -result
        }
    }
    pub fn normal(&mut self) -> Result<i64> {
        loop {
            let (value, uniform) = loop {
                let value = take_fraction(
                    &BigInt::from(112_429),
                    &BigInt::from(self.next() - FRACTION_HALF),
                );
                let uniform = self.next();
                if value.abs() < BigInt::from(uniform) {
                    break (value, uniform);
                }
            };
            let value = make_fraction(&value, &BigInt::from(uniform))?;
            let accepted = if self.exact_repro {
                let threshold =
                    1024 * (BigInt::from(139_548_960) - exact_logarithm(&BigInt::from(uniform)));
                threshold >= &value * &value
            } else {
                let threshold =
                    -(NORMAL_ACCEPTANCE_SCALE as f64) * (uniform as f64 / FRACTION_ONE as f64).ln();
                threshold >= (&value * &value).to_f64().unwrap_or(f64::INFINITY)
            };
            if accepted {
                return value
                    .to_i64()
                    .ok_or_else(|| Error::value("Arithmetic overflow"));
            }
        }
    }
}
