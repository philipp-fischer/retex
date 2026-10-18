//! Explicit per-job operational budgets (`core/capacities.py`).
use super::errors::{Error, Result};
use serde::{Deserialize, Serialize};

pub fn reached(limit: Option<usize>, value: usize) -> bool {
    limit.is_some_and(|n| value >= n)
}
pub fn exceeded(limit: Option<usize>, value: usize) -> bool {
    limit.is_some_and(|n| value > n)
}
pub fn read_size(limit: Option<usize>, occupied: usize) -> Result<Option<usize>> {
    match limit {
        None => Ok(None),
        Some(n) if n > occupied => Ok(Some(n - occupied)),
        Some(_) => {
            Err(Error::runtime("Buffer overflow")
                .with_issue(crate::core::recovery::Issue::Capacity))
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capacities {
    pub main_memory: Option<usize>,
    pub buf_size: Option<usize>,
    pub nest_size: Option<usize>,
    pub max_in_open: Option<usize>,
    pub stack_size: Option<usize>,
    pub font_max: Option<usize>,
    pub expand_depth: Option<usize>,
}
impl Capacities {
    pub fn configured(overrides: &[(String, i64)]) -> Result<Self> {
        let mut values = Self::default();
        for (name, value) in overrides {
            if [
                "pool_size",
                "max_strings",
                "hash_extra",
                "font_mem_size",
                "param_size",
            ]
            .contains(&name.as_str())
            {
                continue;
            }
            let slot = match name.as_str() {
                "main_memory" => &mut values.main_memory,
                "buf_size" => &mut values.buf_size,
                "nest_size" => &mut values.nest_size,
                "max_in_open" => &mut values.max_in_open,
                "stack_size" => &mut values.stack_size,
                "font_max" => &mut values.font_max,
                "expand_depth" => &mut values.expand_depth,
                _ => return Err(Error::value(format!("Unknown capacity: {name}"))),
            };
            if *value < 1 {
                return Err(Error::value("Resource capacities must be positive"));
            }
            *slot = Some(
                usize::try_from(*value)
                    .map_err(|_| Error::value("Resource capacity exceeds host address range"))?,
            );
        }
        Ok(values)
    }
}
