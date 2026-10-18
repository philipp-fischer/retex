//! Live e-TeX quantities and the monotonic elapsed-time clock (`extended.py`).
use super::conditionals::ConditionalFrame;
use super::numeric_codes::group_type_code;
use super::records::{Quantity, ValueIndex, ValueKey};
use super::scanning::{InternalValue, ScanRequest, ScanValue};
use super::services::LanguageServices;
use crate::core::errors::{Error, Result};
pub const ITEMS: [&str; 23] = [
    "eTeXversion",
    "currentgrouplevel",
    "currentgrouptype",
    "currentiflevel",
    "currentiftype",
    "currentifbranch",
    "gluestretch",
    "glueshrink",
    "gluestretchorder",
    "glueshrinkorder",
    "mutoglue",
    "gluetomu",
    "fontcharwd",
    "fontcharht",
    "fontchardp",
    "fontcharic",
    "pdftexversion",
    "pdfshellescape",
    "pdfelapsedtime",
    "parshapelength",
    "parshapeindent",
    "parshapedimen",
    "pdfrandomseed",
];
pub trait ExtendedServices: LanguageServices {
    fn conditional_depth(&self) -> usize;
    fn conditional_frame(&self) -> Option<ConditionalFrame>;
    fn random_seed(&self) -> Result<i64>;
}
#[derive(Clone)]
pub struct ExtendedState {
    pub timer_start: u128,
}
impl Default for ExtendedState {
    fn default() -> Self {
        Self {
            timer_start: crate::resources::host::monotonic_nanos(),
        }
    }
}
impl ExtendedState {
    pub fn reset_timer(&mut self) {
        self.timer_start = crate::resources::host::monotonic_nanos()
    }
    pub fn read(&self, services: &mut impl ExtendedServices, code: usize) -> Result<InternalValue> {
        let mut execution = ExtendedExecution::new(self.clone(), code);
        let mut returned = None;
        loop {
            match execution.resume(services, returned.take())? {
                ExtendedStep::Complete(value) => return Ok(value),
                ExtendedStep::Scan(request) => returned = Some(services.scan(request)),
            }
        }
    }
    fn value(
        &self,
        services: &mut impl ExtendedServices,
        code: usize,
        operand: Option<ScanValue>,
        font: usize,
    ) -> Result<InternalValue> {
        use InternalValue as V;
        Ok(match code {
            0 => V::Count(2),
            1 => V::Count(services.env().groups.len() as i64),
            2 => V::Count(group_type_code(services.env().cur_group)),
            3 => V::Count(services.conditional_depth() as i64),
            4 | 5 => V::Count(services.conditional_frame().map_or(0, |frame| {
                if code == 4 {
                    (frame.code as i64 + 1) * if frame.unless { -1 } else { 1 }
                } else if frame.evaluating {
                    0
                } else if frame.alternative {
                    -1
                } else {
                    1
                }
            })),
            6..=9 => {
                let Some(ScanValue::Glue(spec)) = operand else {
                    return Err(Error::runtime("Missing extended glue"));
                };
                match code {
                    6 => V::Dimen(spec.stretch),
                    7 => V::Dimen(spec.shrink),
                    8 => V::Count(spec.stretch_order as i64),
                    _ => V::Count(spec.shrink_order as i64),
                }
            }
            10 | 11 => {
                let Some(ScanValue::Glue(spec)) = operand else {
                    return Err(Error::runtime("Missing extended glue"));
                };
                if code == 10 {
                    V::Skip(spec)
                } else {
                    V::Muskip(spec)
                }
            }
            12..=15 => {
                let character = operand
                    .ok_or_else(|| Error::runtime("Missing extended character"))?
                    .integer()? as u32;
                let font = &services.fonts().items[font];
                V::Dimen(match code {
                    12 => font.get_char_width(character),
                    13 => font.get_char_height(character),
                    14 => font.get_char_depth(character),
                    _ => font.get_char_italic(character),
                })
            }
            16 => V::Count(140),
            17 => V::Count(0),
            18 => V::Count(
                (crate::resources::host::monotonic_nanos().saturating_sub(self.timer_start) * 65536
                    / 1_000_000_000)
                    .min(2147483647) as i64,
            ),
            19..=21 => {
                let mut kind = code - 19;
                let mut index = operand
                    .ok_or_else(|| Error::runtime("Missing extended index"))?
                    .integer()?;
                let key = ValueKey {
                    table: "parshape".into(),
                    index: ValueIndex::Number(0),
                };
                let shape = match services.env().values.get(&key) {
                    Some(Quantity::Shape(shape)) => shape.as_slice(),
                    _ => &[],
                };
                if index <= 0 || shape.is_empty() {
                    V::Dimen(0)
                } else {
                    if kind == 2 {
                        kind = (index % 2) as usize;
                        index = (index + index % 2) / 2
                    }
                    let (indent, length) = shape[(index as usize).min(shape.len()) - 1];
                    V::Dimen(if kind == 1 { indent } else { length })
                }
            }
            22 => V::Count(services.random_seed()?),
            _ => return Err(Error::value("Unknown extended internal quantity")),
        })
    }
}

/// Operand syntax for the live quantities is shared by CLI and sliced evaluation.
pub enum ExtendedStep {
    Scan(ScanRequest),
    Complete(InternalValue),
}
enum Phase {
    Start,
    Operand,
    Font,
    Character,
    Done,
}
pub struct ExtendedExecution {
    state: ExtendedState,
    code: usize,
    phase: Phase,
    font: usize,
}
impl ExtendedExecution {
    pub fn new(state: ExtendedState, code: usize) -> Self {
        Self {
            state,
            code,
            phase: Phase::Start,
            font: 0,
        }
    }
    pub fn resume(
        &mut self,
        services: &mut impl ExtendedServices,
        returned: Option<Result<ScanValue>>,
    ) -> Result<ExtendedStep> {
        let value = returned.transpose()?;
        match self.phase {
            Phase::Start => {
                if value.is_some() {
                    return Err(Error::runtime("Unrequested extended result"));
                }
                let request = match self.code {
                    6..=9 | 11 => ScanRequest::Glue { mu: false },
                    10 => ScanRequest::Glue { mu: true },
                    12..=15 => {
                        self.phase = Phase::Font;
                        return Ok(ExtendedStep::Scan(ScanRequest::FontIdent));
                    }
                    19..=21 => ScanRequest::Int,
                    _ => {
                        self.phase = Phase::Done;
                        return self
                            .state
                            .value(services, self.code, None, 0)
                            .map(ExtendedStep::Complete);
                    }
                };
                self.phase = Phase::Operand;
                Ok(ExtendedStep::Scan(request))
            }
            Phase::Font => {
                let Some(ScanValue::Font(font)) = value else {
                    return Err(Error::runtime("Invalid extended font result"));
                };
                self.font = font;
                self.phase = Phase::Character;
                Ok(ExtendedStep::Scan(ScanRequest::CharNum))
            }
            Phase::Operand | Phase::Character => {
                self.phase = Phase::Done;
                self.state
                    .value(services, self.code, value, self.font)
                    .map(ExtendedStep::Complete)
            }
            Phase::Done => Err(Error::runtime("Extended result already consumed")),
        }
    }
}
