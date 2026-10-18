//! Extended penalty arrays and post-line-break accumulation (`penalties.py`).
use crate::core::capacities::exceeded;
use crate::core::errors::{Error, Result};
use crate::language::records::{Quantity, ValueIndex, ValueKey};
use crate::language::state::Environment;
pub const NAMES: [&str; 4] = [
    "interlinepenalties",
    "clubpenalties",
    "widowpenalties",
    "displaywidowpenalties",
];
pub trait PenaltyInput {
    fn scan_optional_equals(&mut self) -> Result<()>;
    fn scan_int(&mut self) -> Result<i64>;
}
pub struct Penalties;
impl Penalties {
    fn key(code: i64) -> ValueKey {
        ValueKey {
            table: "penalties".into(),
            index: ValueIndex::Number(code),
        }
    }
    pub fn values(environment: &Environment, code: i64) -> &[i64] {
        match environment.values.get(&Self::key(code)) {
            Some(Quantity::Penalties(values)) => values,
            _ => &[],
        }
    }
    pub fn assign(
        environment: &mut Environment,
        input: &mut impl PenaltyInput,
        node_limit: Option<usize>,
        code: i64,
        global: bool,
    ) -> Result<()> {
        input.scan_optional_equals()?;
        let count = input.scan_int()?;
        if count >= 0 && exceeded(node_limit, count as usize) {
            return Err(Error::value("Penalty array exceeds resource limit"));
        }
        let values = (0..count.max(0))
            .map(|_| input.scan_int())
            .collect::<Result<Vec<_>>>()?;
        environment.assign_value(Self::key(code), Quantity::Penalties(values), global);
        Ok(())
    }
    pub fn read(
        environment: &Environment,
        input: &mut impl PenaltyInput,
        code: i64,
    ) -> Result<i64> {
        let index = input.scan_int()?;
        Ok(Self::at(environment, code, index))
    }
    pub fn at(environment: &Environment, code: i64, index: i64) -> i64 {
        let values = Self::values(environment, code);
        if values.is_empty() || index < 0 {
            0
        } else if index == 0 {
            values.len() as i64
        } else {
            values[(index as usize).min(values.len()) - 1]
        }
    }
    pub fn between(
        environment: &Environment,
        line: i64,
        paragraph_line: i64,
        remaining: i64,
        display: bool,
        broken: bool,
    ) -> Result<i64> {
        let scalar = |name| match environment.values.get(&ValueKey::parameter(name)) {
            Some(Quantity::Integer(number)) => Ok(*number),
            _ => Err(Error::value(format!("Missing penalty parameter {name}"))),
        };
        let contribution = |code, index: i64, name, applies| -> Result<i64> {
            let values = Self::values(environment, code);
            if values.is_empty() {
                return if applies { scalar(name) } else { Ok(0) };
            }
            let at = index.min(values.len() as i64) - 1;
            let at = if at < 0 { values.len() as i64 + at } else { at };
            values
                .get(at as usize)
                .copied()
                .ok_or_else(|| Error::value("Invalid penalty array index"))
        };
        let mut penalty = contribution(0, line, "interlinepenalty", true)?;
        penalty += contribution(1, paragraph_line, "clubpenalty", paragraph_line == 1)?;
        penalty += contribution(
            if display { 3 } else { 2 },
            remaining,
            if display {
                "displaywidowpenalty"
            } else {
                "widowpenalty"
            },
            remaining == 1,
        )?;
        if broken {
            penalty += scalar("brokenpenalty")?;
        }
        Ok(penalty)
    }
}
