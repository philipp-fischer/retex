//! Bounded content-keyed outline storage shared by native and Wasm previews.
use super::formats::{
    type1::Type1Resource,
    type1_outlines::{OutlineCommand, Type1Outlines},
};
use crate::core::errors::{Error, Result};
use rustybuzz::ttf_parser::{Face, GlyphId, OutlineBuilder};
use std::{
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    sync::Arc,
};

pub struct OutlineCache {
    programs: BTreeMap<String, Rc<Type1Outlines>>,
    program_order: VecDeque<String>,
    program_bytes: usize,
    paths: BTreeMap<(String, String), Arc<[OutlineCommand]>>,
    path_order: VecDeque<(String, String)>,
    path_bytes: usize,
}
impl Default for OutlineCache {
    fn default() -> Self {
        Self {
            programs: BTreeMap::new(),
            program_order: VecDeque::new(),
            program_bytes: 0,
            paths: BTreeMap::new(),
            path_order: VecDeque::new(),
            path_bytes: 0,
        }
    }
}
impl OutlineCache {
    pub fn bytes(&self) -> usize {
        self.program_bytes + self.path_bytes
    }
    pub fn entries(&self) -> usize {
        self.paths.len()
    }
    fn remember(&mut self, key: (String, String), path: Arc<[OutlineCommand]>) {
        let bytes = std::mem::size_of_val(path.as_ref());
        if bytes > 16 * 1024 * 1024 {
            return;
        }
        while self.paths.len() >= 2048 || self.path_bytes + bytes > 16 * 1024 * 1024 {
            let Some(old) = self.path_order.pop_front() else {
                break;
            };
            if let Some(path) = self.paths.remove(&old) {
                self.path_bytes -= std::mem::size_of_val(path.as_ref());
            }
        }
        self.path_bytes += bytes;
        self.path_order.push_back(key.clone());
        self.paths.insert(key, path);
    }
    pub fn type1(
        &mut self,
        key: &str,
        resource: &Type1Resource,
        name: &str,
    ) -> Result<Arc<[OutlineCommand]>> {
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::phase(
            "outline_cache_lookup",
            &format!("{key}:{name}"),
            None,
        );
        let lookup = (key.into(), name.into());
        if let Some(path) = self.paths.get(&lookup) {
            #[cfg(feature = "latency-diagnostics")]
            crate::latency_diagnostics::count("hits", 1);
            return Ok(path.clone());
        }
        #[cfg(feature = "latency-diagnostics")]
        crate::latency_diagnostics::count("misses", 1);
        if !self.programs.contains_key(key) {
            #[cfg(feature = "latency-diagnostics")]
            crate::latency_diagnostics::count("program_parses", 1);
            let program = Rc::new(Type1Outlines::parse(resource)?);
            let bytes = program.decoded_bytes();
            if bytes > 8 * 1024 * 1024 {
                return Err(Error::value(
                    "Type1 preview decoded program budget exceeded",
                ));
            }
            while self.programs.len() >= 32 || self.program_bytes + bytes > 8 * 1024 * 1024 {
                let Some(old) = self.program_order.pop_front() else {
                    break;
                };
                if let Some(program) = self.programs.remove(&old) {
                    self.program_bytes -= program.decoded_bytes();
                }
            }
            self.program_bytes += bytes;
            self.program_order.push_back(key.into());
            self.programs.insert(key.into(), program);
        }
        let program = &self.programs[key];
        let path: Arc<[OutlineCommand]> = program.outline(name).map_err(Error::value)?.into();
        self.remember(lookup, path.clone());
        Ok(path)
    }
    pub fn sfnt(&mut self, key: &str, data: &[u8], glyph: u16) -> Result<Arc<[OutlineCommand]>> {
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::phase(
            "outline_cache_lookup",
            &format!("{key}:gid:{glyph}"),
            None,
        );
        let lookup = (key.into(), format!("gid:{glyph}"));
        if let Some(path) = self.paths.get(&lookup) {
            #[cfg(feature = "latency-diagnostics")]
            crate::latency_diagnostics::count("hits", 1);
            return Ok(path.clone());
        }
        let face = Face::parse(data, 0)
            .map_err(|e| Error::value(format!("Invalid preview SFNT: {e:?}")))?;
        if glyph >= face.number_of_glyphs() {
            return Err(Error::value("Preview glyph index outside original font"));
        }
        let mut builder = PathBuilder::default();
        let bbox = face.outline_glyph(GlyphId(glyph), &mut builder);
        if bbox.is_none() && !builder.path.is_empty() {
            return Err(Error::value("Malformed preview SFNT outline"));
        }
        if builder.overflow {
            return Err(Error::value("SFNT preview path budget exceeded"));
        }
        let path: Arc<[OutlineCommand]> = builder.path.into();
        self.remember(lookup, path.clone());
        Ok(path)
    }
}
#[derive(Default)]
struct PathBuilder {
    path: Vec<OutlineCommand>,
    overflow: bool,
}
impl PathBuilder {
    fn push(&mut self, command: OutlineCommand) {
        if self.path.len() >= 1_000_000 {
            self.overflow = true;
        } else {
            self.path.push(command);
        }
    }
}
impl OutlineBuilder for PathBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.push(OutlineCommand::MoveTo {
            x: x as f64,
            y: y as f64,
        });
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.push(OutlineCommand::LineTo {
            x: x as f64,
            y: y as f64,
        });
    }
    fn quad_to(&mut self, x: f32, y: f32, end_x: f32, end_y: f32) {
        self.push(OutlineCommand::QuadTo {
            control: [x as f64, y as f64],
            end: [end_x as f64, end_y as f64],
        });
    }
    fn curve_to(&mut self, x: f32, y: f32, x2: f32, y2: f32, end_x: f32, end_y: f32) {
        self.push(OutlineCommand::CurveTo {
            first: [x as f64, y as f64],
            second: [x2 as f64, y2 as f64],
            end: [end_x as f64, end_y as f64],
        });
    }
    fn close(&mut self) {
        self.push(OutlineCommand::Close);
    }
}
