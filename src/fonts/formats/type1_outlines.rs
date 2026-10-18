//! Type1 glyph paths over this port's original charstring dictionary/cipher.
//! Operator control flow was consulted from the older port's isolated outline
//! component, then adapted to typed errors, current Type1Program dictionaries
//! and unquantized coordinates. No old engine/module is a runtime dependency.
//! The independent oracle is FontTools drawing the same original font bytes.
use super::{
    type1::{STANDARD_ENCODING as STANDARD, Type1Resource},
    type1_subsets::Type1Program,
};
use crate::core::errors::{Error, Result as EngineResult};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    string::String,
    vec::Vec,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum OutlineCommand {
    MoveTo {
        x: f64,
        y: f64,
    },
    LineTo {
        x: f64,
        y: f64,
    },
    CurveTo {
        first: [f64; 2],
        second: [f64; 2],
        end: [f64; 2],
    },
    QuadTo {
        control: [f64; 2],
        end: [f64; 2],
    },
    Close,
}

/// Decrypted glyph programs and shared subroutines, independent of an encoding vector.
#[derive(Clone, Debug)]
pub struct Type1Outlines {
    glyphs: BTreeMap<String, Result<Rc<[u8]>, String>>,
    subrs: BTreeMap<usize, Result<Rc<[u8]>, String>>,
}
impl Type1Outlines {
    /// Decode the original reader's declarative binary dictionaries, including arbitrary RD/ND procedure names.
    pub fn parse(resource: &Type1Resource) -> EngineResult<Self> {
        let program = Type1Program::new(resource.clone())?;
        let glyphs = program
            .glyphs
            .iter()
            .map(|(name, entry)| {
                (
                    name.clone(),
                    program
                        .bytecode(entry)
                        .map(|bytes| Rc::from(bytes))
                        .map_err(|error| error.to_string()),
                )
            })
            .collect();
        let mut subrs = BTreeMap::new();
        for (&number, entry) in &program.subrs {
            let number = usize::try_from(number).map_err(|_| {
                Error::value("Invalid Type1 Subr index")
                    .with_issue(crate::core::recovery::Issue::Font)
            })?;
            subrs.insert(
                number,
                program
                    .bytecode(entry)
                    .map(Rc::from)
                    .map_err(|error| error.to_string()),
            );
        }
        Ok(Self { glyphs, subrs })
    }
    pub fn glyph_names(&self) -> impl Iterator<Item = &str> {
        self.glyphs.keys().map(String::as_str)
    }
    pub fn decoded_bytes(&self) -> usize {
        self.glyphs
            .values()
            .chain(self.subrs.values())
            .map(|code| match code {
                Ok(bytes) => bytes.len(),
                Err(error) => error.len(),
            })
            .sum()
    }
    /// Execute one actual named glyph, decomposing seac components into cubic preview paths.
    pub fn outline(&self, name: &str) -> Result<Vec<OutlineCommand>, String> {
        self.glyph(
            name,
            &mut BTreeSet::new(),
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            &mut 0,
        )
    }
    /// Apply the physical font matrix without coordinate quantization (em units).
    pub fn outline_with_matrix(
        &self,
        name: &str,
        matrix: [f64; 6],
    ) -> Result<Vec<OutlineCommand>, String> {
        self.glyph(name, &mut BTreeSet::new(), matrix, &mut 0)
    }
    fn glyph(
        &self,
        name: &str,
        active: &mut BTreeSet<String>,
        matrix: [f64; 6],
        operations: &mut usize,
    ) -> Result<Vec<OutlineCommand>, String> {
        let Some(code) = self.glyphs.get(name) else {
            return Err(format!("Missing Type1 glyph {name}"));
        };
        let code = code.clone()?;
        if !active.insert(name.into()) {
            return Err("Recursive Type1 seac glyph".into());
        }
        if active.len() > 64 {
            return Err("Type1 composite depth budget exceeded".into());
        }
        let mut machine = Machine {
            matrix,
            ..Machine::default()
        };
        let mut frames = std::vec![(None, code.clone(), 0usize)];
        while let Some((_, code, position)) = frames.last_mut() {
            *operations += 1;
            if *operations > 1_000_000
                || machine.stack.len() > 4096
                || machine.path.len() > 1_000_000
            {
                return Err("Type1 outline work/storage budget exceeded".into());
            }
            let Some(&byte) = code.get(*position) else {
                frames.pop();
                continue;
            };
            *position += 1;
            if byte >= 32 {
                let value = match byte {
                    32..=246 => f64::from(i32::from(byte) - 139),
                    247..=254 => {
                        let next = *code.get(*position).ok_or("Truncated Type1 number")?;
                        *position += 1;
                        if byte <= 250 {
                            f64::from((i32::from(byte) - 247) * 256 + i32::from(next) + 108)
                        } else {
                            f64::from(-(i32::from(byte) - 251) * 256 - i32::from(next) - 108)
                        }
                    }
                    255 => {
                        let bytes = code
                            .get(*position..*position + 4)
                            .ok_or("Truncated Type1 number")?;
                        *position += 4;
                        f64::from(i32::from_be_bytes(bytes.try_into().unwrap()))
                    }
                    _ => unreachable!(),
                };
                machine.stack.push(value);
                continue;
            }
            let operation = if byte == 12 {
                let escape = *code.get(*position).ok_or("Truncated Type1 operator")?;
                *position += 1;
                256 + u16::from(escape)
            } else {
                u16::from(byte)
            };
            match operation {
                10 => {
                    let index = machine.integer()?;
                    if frames.iter().any(|frame| frame.0 == Some(index)) {
                        return Err("Recursive Type1 Subr".into());
                    }
                    let subr = self
                        .subrs
                        .get(&index)
                        .ok_or_else(|| std::format!("Invalid Type1 Subr {index}"))?;
                    if frames.len() >= 256 {
                        return Err("Type1 Subr depth budget exceeded".into());
                    }
                    frames.push((Some(index), subr.clone()?, 0));
                }
                11 => {
                    frames.pop();
                }
                14 => {
                    machine.close();
                    frames.clear();
                }
                262 => {
                    let args = machine.args(5)?;
                    let base = integer(args[3])?;
                    let accent = integer(args[4])?;
                    let base = STANDARD.get(base).ok_or("Invalid Type1 seac code")?;
                    let accent = STANDARD.get(accent).ok_or("Invalid Type1 seac code")?;
                    machine.close();
                    machine
                        .path
                        .extend(self.glyph(base, active, matrix, operations)?);
                    let dx = args[1] + machine.sbx - args[0];
                    let x = finite_coordinate(matrix[0] * dx + matrix[2] * args[2])?;
                    let y = finite_coordinate(matrix[1] * dx + matrix[3] * args[2])?;
                    for mut command in self.glyph(accent, active, matrix, operations)? {
                        translate(&mut command, x, y)?;
                        machine.path.push(command);
                    }
                    frames.clear();
                }
                _ => machine.operation(operation)?,
            }
        }
        machine.close();
        active.remove(name);
        Ok(machine.path)
    }
}
#[derive(Default)]
struct Machine {
    matrix: [f64; 6],
    stack: Vec<f64>,
    path: Vec<OutlineCommand>,
    point: [f64; 2],
    sbx: f64,
    contour: bool,
    flex: bool,
    other_results: Vec<f64>,
}
fn integer(value: f64) -> Result<usize, String> {
    if value.is_finite()
        && value >= 0.0
        && value < usize::MAX as f64
        && value == value as usize as f64
    {
        Ok(value as usize)
    } else {
        Err("Invalid Type1 integer operand".into())
    }
}
fn finite_coordinate(value: f64) -> Result<f64, String> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err("Non-finite Type1 outline coordinate".into())
    }
}
fn translate(command: &mut OutlineCommand, x: f64, y: f64) -> Result<(), String> {
    let point = |a: &mut f64, b: &mut f64| -> Result<(), String> {
        *a = finite_coordinate(*a + x)?;
        *b = finite_coordinate(*b + y)?;
        Ok(())
    };
    match command {
        OutlineCommand::MoveTo { x, y } | OutlineCommand::LineTo { x, y } => point(x, y)?,
        OutlineCommand::CurveTo { first, second, end } => {
            for p in [first, second, end] {
                let [x, y] = p;
                point(x, y)?;
            }
        }
        OutlineCommand::QuadTo { control, end } => {
            for p in [control, end] {
                let [x, y] = p;
                point(x, y)?;
            }
        }
        OutlineCommand::Close => {}
    }
    Ok(())
}
impl Machine {
    fn coordinates(&self) -> Result<[f64; 2], String> {
        let [a, b, c, d, e, f] = self.matrix;
        let [x, y] = self.point;
        Ok([
            finite_coordinate(a * x + c * y + e)?,
            finite_coordinate(b * x + d * y + f)?,
        ])
    }
    fn integer(&mut self) -> Result<usize, String> {
        integer(self.stack.pop().ok_or("Type1 operand stack underflow")?)
    }
    fn args(&mut self, count: usize) -> Result<Vec<f64>, String> {
        if self.stack.len() != count {
            return Err(std::format!(
                "Invalid Type1 operand count: expected {count}, got {}",
                self.stack.len()
            ));
        }
        Ok(core::mem::take(&mut self.stack))
    }
    fn close(&mut self) {
        if self.contour {
            self.path.push(OutlineCommand::Close);
            self.contour = false;
        }
    }
    fn move_to(&mut self, delta: [f64; 2]) -> Result<(), String> {
        self.close();
        self.point[0] += delta[0];
        self.point[1] += delta[1];
        self.path.push(OutlineCommand::MoveTo {
            x: self.coordinates()?[0],
            y: self.coordinates()?[1],
        });
        self.contour = true;
        Ok(())
    }
    fn line(&mut self, delta: [f64; 2]) -> Result<(), String> {
        self.point[0] += delta[0];
        self.point[1] += delta[1];
        self.path.push(OutlineCommand::LineTo {
            x: self.coordinates()?[0],
            y: self.coordinates()?[1],
        });
        Ok(())
    }
    fn curve(&mut self, args: &[f64]) -> Result<(), String> {
        let mut points = [[0.0; 2]; 3];
        for (index, point) in points.iter_mut().enumerate() {
            self.point[0] += args[index * 2];
            self.point[1] += args[index * 2 + 1];
            *point = self.coordinates()?;
        }
        self.path.push(OutlineCommand::CurveTo {
            first: points[0],
            second: points[1],
            end: points[2],
        });
        Ok(())
    }
    fn operation(&mut self, operation: u16) -> Result<(), String> {
        match operation {
            // Hints do not change the geometric path. Arity is still validated.
            1 | 3 => {
                self.args(2)?;
            }
            257 | 258 => {
                self.args(6)?;
            }
            256 => {
                self.args(0)?;
            }
            13 => {
                let args = self.args(2)?;
                self.sbx = args[0];
                self.point[0] = args[0];
            }
            263 => {
                let args = self.args(4)?;
                self.sbx = args[0];
                self.point = [args[0], args[1]];
            }
            4 | 21 | 22 => {
                if self.flex {
                    if operation == 22 {
                        self.stack.push(0.0);
                    }
                    if operation == 4 {
                        let value = self.stack.pop().ok_or("Type1 operand stack underflow")?;
                        self.stack.extend([0.0, value]);
                    }
                } else {
                    let args = self.args(if operation == 21 { 2 } else { 1 })?;
                    self.move_to(match operation {
                        21 => [args[0], args[1]],
                        4 => [0.0, args[0]],
                        _ => [args[0], 0.0],
                    })?;
                }
            }
            5..=7 => {
                let args = self.args(if operation == 5 { 2 } else { 1 })?;
                self.line(match operation {
                    5 => [args[0], args[1]],
                    6 => [args[0], 0.0],
                    _ => [0.0, args[0]],
                })?;
            }
            8 => {
                let args = self.args(6)?;
                self.curve(&args)?;
            }
            30 | 31 => {
                let a = self.args(4)?;
                self.curve(&if operation == 30 {
                    [0.0, a[0], a[1], a[2], a[3], 0.0]
                } else {
                    [a[0], 0.0, a[1], a[2], 0.0, a[3]]
                })?;
            }
            9 => {
                self.args(0)?;
                self.close();
            }
            268 => {
                let divisor = self.stack.pop().ok_or("Type1 operand stack underflow")?;
                let numerator = self.stack.pop().ok_or("Type1 operand stack underflow")?;
                if divisor == 0.0 {
                    return Err("Type1 division by zero".into());
                }
                self.stack.push(numerator / divisor);
            }
            272 => {
                let subr = self.integer()?;
                let count = self.integer()?;
                match (subr, count) {
                    (1, 0) => {
                        self.flex = true;
                    }
                    (2, 0) if self.flex => {}
                    (0, 3) if self.flex => {
                        let args = self.args(17)?;
                        self.curve(&[
                            args[0] + args[2],
                            args[1] + args[3],
                            args[4],
                            args[5],
                            args[6],
                            args[7],
                        ])?;
                        self.curve(&args[8..14])?;
                        self.other_results.extend([args[16], args[15]]);
                        self.flex = false;
                    }
                    (3, 1) => {
                        let value = self.stack.pop().ok_or("Type1 operand stack underflow")?;
                        self.other_results.push(value);
                    }
                    _ => {
                        return Err(std::format!(
                            "Unsupported Type1 OtherSubr {subr} with {count} operands"
                        ));
                    }
                }
            }
            273 => self.stack.push(
                self.other_results
                    .pop()
                    .ok_or("Type1 OtherSubr result underflow")?,
            ),
            289 => {
                let args = self.args(2)?;
                self.point = [args[0], args[1]];
            }
            _ => {
                return Err(std::format!(
                    "Invalid Type1 charstring operator {operation}"
                ));
            }
        }
        Ok(())
    }
}
