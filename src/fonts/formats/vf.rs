//! Validated virtual-font recipes and iterative painting (`formats/vf.py`).
use crate::core::errors::{Error, Result};
use crate::fonts::scaling::scale_fix_word;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;

pub struct ByteReader<'a> {
    pub data: &'a [u8],
    pub position: usize,
}
impl<'a> ByteReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }
    pub fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(count)
            .filter(|&end| end <= self.data.len())
            .ok_or_else(|| {
                Error::value("Invalid virtual font: unexpected EOF")
                    .with_issue(crate::core::recovery::Issue::Font)
            })?;
        let result = &self.data[self.position..end];
        self.position = end;
        Ok(result)
    }
    pub fn integer(&mut self, count: usize, signed: bool) -> Result<i64> {
        let data = self.take(count)?;
        if count > 8 {
            return Err(Error::value("Invalid virtual font integer width")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        let mut value = if signed && data.first().is_some_and(|byte| byte & 128 != 0) {
            -1_i64
        } else {
            0
        };
        for &byte in data {
            value = (value << 8) | i64::from(byte);
        }
        Ok(value)
    }
    pub fn remaining(&self) -> bool {
        self.position < self.data.len()
    }
}
pub const VF_DIMENSION_LIMIT: i64 = 1 << 24;
pub fn scaled(value: i64, size: i64) -> Result<i64> {
    if !(-VF_DIMENSION_LIMIT..VF_DIMENSION_LIMIT).contains(&value) {
        return Err(Error::value("Invalid virtual font: vf scaling")
            .with_issue(crate::core::recovery::Issue::Font));
    }
    Ok(scale_fix_word(value, size))
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VirtualOperation {
    Char {
        code: u32,
        advance: bool,
    },
    Rule {
        height: i64,
        width: i64,
        advance: bool,
    },
    Push,
    Pop,
    Right(i64),
    Down(i64),
    W(Option<i64>),
    X(Option<i64>),
    Y(Option<i64>),
    Z(Option<i64>),
    Font(usize),
    Special(String),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VirtualFont {
    pub path: String,
    pub fonts: BTreeMap<u32, usize>,
    pub font_order: Vec<u32>,
    pub packets: BTreeMap<u32, Arc<[VirtualOperation]>>,
}
#[derive(Clone, Debug)]
pub struct VirtualSelection {
    pub name: String,
    pub size: i64,
    pub design_size: i64,
    pub checksum: u32,
    pub virtual_font: Option<Arc<VirtualFont>>,
}
pub trait VirtualCatalog {
    fn load(&mut self, name: &str, directory: &Path, size: i64) -> Result<usize>;
    fn selection(&self, number: usize) -> VirtualSelection;
    fn char_width(&self, number: usize, code: u32) -> i64;
    fn bind(&mut self, number: usize, warn: &mut dyn FnMut(&str)) -> Result<()>;
}
impl VirtualFont {
    pub fn read(
        path: &Path,
        font_number: usize,
        catalog: &mut impl VirtualCatalog,
        warn: &mut dyn FnMut(&str),
    ) -> Result<Self> {
        let data = crate::resources::host::read(path)?;
        Self::read_bytes(path, &data, font_number, catalog, warn)
    }
    pub fn read_bytes(
        path: &Path,
        data: &[u8],
        font_number: usize,
        catalog: &mut impl VirtualCatalog,
        warn: &mut dyn FnMut(&str),
    ) -> Result<Self> {
        let font = catalog.selection(font_number);
        let mut reader = ByteReader::new(data);
        if reader.integer(1, false)? != 247 || reader.integer(1, false)? != 202 {
            return Err(Error::value("Invalid virtual font: PRE/id expected")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        let count = reader.integer(1, false)? as usize;
        reader.take(count)?;
        let checksum = reader.integer(4, false)? as u32;
        let design = reader.integer(4, true)?.div_euclid(16);
        if checksum != 0 && font.checksum != 0 && checksum != font.checksum {
            warn(&format!(
                "Checksum mismatch in font {}.vf ignored",
                font.name
            ));
        }
        if design != font.design_size {
            warn(&format!(
                "Design size mismatch in font {}.vf ignored",
                font.name
            ));
        }
        let mut fonts = BTreeMap::new();
        let mut font_order = Vec::new();
        let mut command = reader.integer(1, false)?;
        while (243..=246).contains(&command) {
            let external = reader.integer((command - 242) as usize, false)? as u32;
            let check = reader.integer(4, false)? as u32;
            let size = scaled(reader.integer(4, true)?, font.size)?;
            let design = reader.integer(4, true)?.div_euclid(16);
            let area_length = reader.integer(1, false)? as usize;
            let name_length = reader.integer(1, false)? as usize;
            reader.take(area_length)?;
            let name: String = reader
                .take(name_length)?
                .iter()
                .map(|&b| char::from(b))
                .collect();
            let number = catalog.load(&name, path.parent().unwrap_or(Path::new(".")), size)?;
            let local = catalog.selection(number);
            if check != 0 && local.checksum != 0 && check != local.checksum {
                warn(&format!(
                    "Checksum mismatch in local font {name} in virtual font {}.vf ignored",
                    font.name
                ));
            }
            if design != local.design_size {
                warn(&format!(
                    "Design size mismatch in local font {name} in virtual font {}.vf ignored",
                    font.name
                ));
            }
            if let std::collections::btree_map::Entry::Vacant(slot) = fonts.entry(external) {
                slot.insert(number);
                font_order.push(external);
            }
            command = reader.integer(1, false)?;
        }
        let mut packets = BTreeMap::new();
        while command <= 242 {
            let (length, code, width) = if command == 242 {
                (
                    reader.integer(4, false)? as usize,
                    reader.integer(4, false)? as u32,
                    scaled(reader.integer(4, true)?, font.size)?,
                )
            } else {
                (
                    command as usize,
                    reader.integer(1, false)? as u32,
                    scaled(reader.integer(3, false)?, font.size)?,
                )
            };
            if code > 255 {
                return Err(Error::value("Invalid virtual font: character code")
                    .with_issue(crate::core::recovery::Issue::Font));
            }
            if width != catalog.char_width(font_number, code) {
                warn(&format!(
                    "Character width mismatch in font {}.vf ignored",
                    font.name
                ));
            }
            packets.insert(code, Self::decode(reader.take(length)?, &fonts)?.into());
            command = reader.integer(1, false)?;
        }
        if command != 248 {
            return Err(Error::value("Invalid virtual font: POST expected")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        Ok(Self {
            path: path.to_string_lossy().into_owned(),
            fonts,
            font_order,
            packets,
        })
    }
    pub fn decode(data: &[u8], fonts: &BTreeMap<u32, usize>) -> Result<Vec<VirtualOperation>> {
        let mut reader = ByteReader::new(data);
        let mut operations = Vec::new();
        let mut level = 0_i64;
        use VirtualOperation::*;
        while reader.remaining() {
            let command = reader.integer(1, false)?;
            let operation = match command {
                0..=127 => Char {
                    code: command as u32,
                    advance: true,
                },
                128..=131 | 133..=136 => Char {
                    code: reader.integer(
                        (command - if command <= 131 { 127 } else { 132 }) as usize,
                        false,
                    )? as u32,
                    advance: command <= 131,
                },
                132 | 137 => Rule {
                    height: reader.integer(4, true)?,
                    width: reader.integer(4, true)?,
                    advance: command == 132,
                },
                138 => continue,
                141 | 142 => {
                    level += if command == 141 { 1 } else { -1 };
                    if level < 0 {
                        return Err(Error::value("Invalid virtual font: unbalanced PUSH/POP")
                            .with_issue(crate::core::recovery::Issue::Font));
                    }
                    if command == 141 { Push } else { Pop }
                }
                143..=146 => Right(reader.integer((command - 142) as usize, true)?),
                147..=151 => W(if command == 147 {
                    None
                } else {
                    Some(reader.integer((command - 147) as usize, true)?)
                }),
                152..=156 => X(if command == 152 {
                    None
                } else {
                    Some(reader.integer((command - 152) as usize, true)?)
                }),
                157..=160 => Down(reader.integer((command - 156) as usize, true)?),
                161..=165 => Y(if command == 161 {
                    None
                } else {
                    Some(reader.integer((command - 161) as usize, true)?)
                }),
                166..=170 => Z(if command == 166 {
                    None
                } else {
                    Some(reader.integer((command - 166) as usize, true)?)
                }),
                171..=238 => {
                    let external = if command <= 234 {
                        (command - 171) as u32
                    } else {
                        reader.integer((command - 234) as usize, false)? as u32
                    };
                    Font(*fonts.get(&external).ok_or_else(|| {
                        Error::value("Invalid virtual font: undefined local font")
                            .with_issue(crate::core::recovery::Issue::Font)
                    })?)
                }
                239..=242 => {
                    let count = reader.integer((command - 238) as usize, false)? as usize;
                    Special(reader.take(count)?.iter().map(|&b| char::from(b)).collect())
                }
                _ => {
                    return Err(Error::value("Invalid virtual font: improper DVI command")
                        .with_issue(crate::core::recovery::Issue::Font));
                }
            };
            operations.push(operation);
        }
        if level != 0 {
            return Err(Error::value("Invalid virtual font: more PUSHs than POPs")
                .with_issue(crate::core::recovery::Issue::Font));
        }
        Ok(operations)
    }
    pub fn paint(self: &Arc<Self>, code: u32, size: i64, x: i64, y: i64) -> Result<VirtualPaint> {
        let mut paint = VirtualPaint {
            frames: Vec::new(),
            active: Vec::new(),
            seen: HashSet::new(),
        };
        paint.begin(self.clone(), code, size, x, y)?;
        Ok(paint)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaintRecord {
    Glyph {
        x: i64,
        y: i64,
        font: usize,
        code: u32,
    },
    Rule {
        x: i64,
        y: i64,
        width: i64,
        height: i64,
    },
    Special {
        x: i64,
        y: i64,
        text: String,
    },
}
enum Advance {
    Amount(i64),
    Character(usize, u32),
}
struct PaintFrame {
    font: Arc<VirtualFont>,
    code: u32,
    size: i64,
    cursor: usize,
    current: Option<usize>,
    x: i64,
    y: i64,
    registers: [i64; 4],
    positions: Vec<(i64, i64, [i64; 4])>,
    advance: Option<Advance>,
}
pub struct VirtualPaint {
    frames: Vec<PaintFrame>,
    active: Vec<(String, u32)>,
    seen: HashSet<(String, u32)>,
}
impl VirtualPaint {
    fn begin(
        &mut self,
        font: Arc<VirtualFont>,
        code: u32,
        size: i64,
        x: i64,
        y: i64,
    ) -> Result<()> {
        let key = (font.path.clone(), code);
        if !self.seen.insert(key.clone()) {
            let chain = self
                .active
                .iter()
                .chain(std::iter::once(&key))
                .map(|(path, code)| {
                    format!(
                        "{}[{code}]",
                        Path::new(path)
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                    )
                })
                .collect::<Vec<_>>()
                .join(" -> ");
            return Err(Error::value(format!(
                "Cyclic virtual font glyph recipe: {chain}"
            )));
        }
        self.active.push(key);
        let current = font
            .font_order
            .first()
            .and_then(|key| font.fonts.get(key))
            .copied();
        self.frames.push(PaintFrame {
            font,
            code,
            size,
            cursor: 0,
            current,
            x,
            y,
            registers: [0; 4],
            positions: Vec::new(),
            advance: None,
        });
        Ok(())
    }
    /// The catalog is borrowed only for this step, allowing shipped specials to
    /// reenter the engine between records, as the Python generator permits.
    pub fn next(
        &mut self,
        catalog: &mut impl VirtualCatalog,
        warn: &mut dyn FnMut(&str),
    ) -> Result<Option<PaintRecord>> {
        loop {
            let Some(frame) = self.frames.last_mut() else {
                return Ok(None);
            };
            if let Some(advance) = frame.advance.take() {
                frame.x += match advance {
                    Advance::Amount(amount) => amount,
                    Advance::Character(font, code) => catalog.char_width(font, code),
                };
            }
            let operations = frame.font.packets.get(&frame.code);
            if operations.is_none_or(|operations| frame.cursor >= operations.len()) {
                let frame = self.frames.pop().expect("paint frame");
                self.seen.remove(&(frame.font.path.clone(), frame.code));
                self.active.pop();
                continue;
            }
            let operation = operations.expect("packet operations")[frame.cursor].clone();
            frame.cursor += 1;
            match operation {
                VirtualOperation::Font(number) => frame.current = Some(number),
                VirtualOperation::Push => frame.positions.push((frame.x, frame.y, frame.registers)),
                VirtualOperation::Pop => {
                    (frame.x, frame.y, frame.registers) =
                        frame.positions.pop().ok_or_else(|| {
                            Error::value("Invalid virtual font: unbalanced PUSH/POP")
                                .with_issue(crate::core::recovery::Issue::Font)
                        })?;
                }
                VirtualOperation::Char { code, advance } => {
                    if code > 255 {
                        warn(&format!("Invalid virtual character {code}"));
                        continue;
                    }
                    let number = frame.current.ok_or_else(|| {
                        Error::value("Invalid virtual font: no local font")
                            .with_issue(crate::core::recovery::Issue::Font)
                    })?;
                    catalog.bind(number, warn)?;
                    let font = catalog.selection(number);
                    if advance {
                        frame.advance = Some(Advance::Character(number, code));
                    }
                    let (x, y) = (frame.x, frame.y);
                    if let Some(font) = font.virtual_font {
                        self.begin(font, code, catalog.selection(number).size, x, y)?;
                    } else {
                        return Ok(Some(PaintRecord::Glyph {
                            x,
                            y,
                            font: number,
                            code,
                        }));
                    }
                }
                VirtualOperation::Rule {
                    height,
                    width,
                    advance,
                } => {
                    let (height, width) = (scaled(height, frame.size)?, scaled(width, frame.size)?);
                    if height > 0 && width > 0 {
                        if advance {
                            frame.advance = Some(Advance::Amount(width));
                        }
                        return Ok(Some(PaintRecord::Rule {
                            x: frame.x,
                            y: frame.y,
                            width,
                            height,
                        }));
                    }
                }
                VirtualOperation::Special(text) => {
                    return Ok(Some(PaintRecord::Special {
                        x: frame.x,
                        y: frame.y,
                        text,
                    }));
                }
                other => {
                    let (register, amount, vertical) = match other {
                        VirtualOperation::Right(v) => (None, Some(v), false),
                        VirtualOperation::Down(v) => (None, Some(v), true),
                        VirtualOperation::W(v) => (Some(0), v, false),
                        VirtualOperation::X(v) => (Some(1), v, false),
                        VirtualOperation::Y(v) => (Some(2), v, true),
                        VirtualOperation::Z(v) => (Some(3), v, true),
                        _ => unreachable!("movement recipe"),
                    };
                    let amount = if let Some(register) = register {
                        if let Some(amount) = amount {
                            frame.registers[register] = scaled(amount, frame.size)?;
                        }
                        frame.registers[register]
                    } else {
                        scaled(amount.expect("explicit movement"), frame.size)?
                    };
                    if vertical {
                        frame.y += amount;
                    } else {
                        frame.x += amount;
                    }
                }
            }
        }
    }
}
