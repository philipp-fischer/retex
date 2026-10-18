//! Owned Unicode font metrics and HarfBuzz-compatible runs (`fonts/unicode.py`).
//! Uses the native Rust shaping port: https://docs.rs/rustybuzz/0.20.1/rustybuzz/.
use super::formats::sfnt::{self, CompositeGlyphFlag};
use crate::core::errors::{Error, Result};
use crate::core::values::round_ratio;
use rustybuzz::ttf_parser::{Face, GlyphId, PlatformId};
use rustybuzz::{BufferFlags, Direction, Feature, UnicodeBuffer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShapedGlyph {
    pub glyph_id: u32,
    pub advance_sp: i64,
    pub x_offset_sp: i64,
    pub y_offset_sp: i64,
    pub cluster_start: usize,
    pub cluster_end: usize,
    pub text: String,
    pub ink_sp: [i64; 4],
    pub unsafe_to_break: bool,
}
pub const SHAPED_RUN_CACHE_SIZE: usize = 2048;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetricDimension {
    Width,
    Height,
    Depth,
}
#[derive(Clone, Debug, Default)]
struct ShapeCache {
    clock: u64,
    entries: BTreeMap<(String, bool), (u64, Arc<[ShapedGlyph]>)>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UnicodeFont {
    pub path: String,
    pub size: i64,
    pub data: Arc<[u8]>,
    pub sha256: String,
    pub upem: i64,
    pub cmap: BTreeMap<u32, u16>,
    pub glyph_names: Vec<String>,
    pub glyph_ids: BTreeMap<String, u16>,
    pub name: String,
    pub bbox: [i64; 4],
    pub ascent: i64,
    pub descent: i64,
    pub italic_angle: f64,
    pub advances: Vec<i64>,
    #[serde(skip)]
    cache: RefCell<ShapeCache>,
}
impl UnicodeFont {
    pub fn read(path: &Path, size: i64) -> Result<Self> {
        let path = crate::resources::paths::resolve_path(path);
        let data = crate::resources::host::read(&path)?;
        Self::parse(path.to_string_lossy().into_owned(), data, size)
    }
    pub fn parse(path: String, data: Vec<u8>, size: i64) -> Result<Self> {
        let tables = sfnt::table_directory(&data)?;
        if !tables.contains_key(b"glyf") {
            return Err(Error::value(
                "The Unicode font cohort currently requires TrueType outlines",
            ));
        }
        let face = Face::parse(&data, 0)
            .map_err(|error| Error::value(format!("Invalid Unicode font: {error:?}")))?;
        let mut cmap = BTreeMap::new();
        if let Some(table) = face.tables().cmap {
            let priorities = [
                (PlatformId::Windows, 10),
                (PlatformId::Unicode, 6),
                (PlatformId::Unicode, 4),
                (PlatformId::Windows, 1),
                (PlatformId::Unicode, 3),
                (PlatformId::Unicode, 2),
                (PlatformId::Unicode, 1),
                (PlatformId::Unicode, 0),
            ];
            for (platform, encoding) in priorities {
                if let Some(selected) = table.subtables.into_iter().find(|subtable| {
                    subtable.platform_id == platform
                        && subtable.encoding_id == encoding
                        && subtable.is_unicode()
                }) {
                    selected.codepoints(|code| {
                        if let Some(glyph) = selected.glyph_index(code) {
                            cmap.insert(code, glyph.0);
                        }
                    });
                    break;
                }
            }
        }
        let glyph_names: Vec<_> = (0..face.number_of_glyphs())
            .map(|id| {
                face.glyph_name(GlyphId(id))
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        if id == 0 {
                            ".notdef".into()
                        } else {
                            format!("glyph{id:05}")
                        }
                    })
            })
            .collect();
        let glyph_ids = glyph_names
            .iter()
            .enumerate()
            .map(|(id, name)| (name.clone(), id as u16))
            .collect();
        let name = face
            .names()
            .into_iter()
            .filter(|record| record.name_id == 6)
            .find_map(|record| record.to_string())
            .unwrap_or_else(|| {
                Path::new(&path)
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
        let head = tables.get(b"head").unwrap();
        let hhea = tables
            .get(b"hhea")
            .ok_or_else(|| Error::value("Missing Unicode font hhea"))?;
        let post = tables
            .get(b"post")
            .ok_or_else(|| Error::value("Missing Unicode font post"))?;
        let bbox = [
            sfnt::i16_at(head, 36)? as i64,
            sfnt::i16_at(head, 38)? as i64,
            sfnt::i16_at(head, 40)? as i64,
            sfnt::i16_at(head, 42)? as i64,
        ];
        let ascent = sfnt::i16_at(hhea, 4)? as i64;
        let descent = sfnt::i16_at(hhea, 6)? as i64;
        let italic_angle =
            i32::from_be_bytes(sfnt::bytes(post, 4, 4)?.try_into().unwrap()) as f64 / 65536.0;
        let advances = (0..face.number_of_glyphs())
            .map(|id| face.glyph_hor_advance(GlyphId(id)).unwrap_or(0) as i64)
            .collect();
        let upem = face.units_per_em() as i64;
        let sha256 = format!("{:x}", Sha256::digest(&data));
        Ok(Self {
            path,
            size,
            data: data.into(),
            sha256,
            upem,
            cmap,
            glyph_names,
            glyph_ids,
            name,
            bbox,
            ascent,
            descent,
            italic_angle,
            advances,
            cache: RefCell::default(),
        })
    }
    pub fn scaled(&self, units: i64) -> i64 {
        round_ratio(units as i128 * self.size as i128, self.upem as i128)
            .expect("positive units per em") as i64
    }
    pub fn glyph_id(&self, code: u32) -> u16 {
        self.cmap.get(&code).copied().unwrap_or(0)
    }
    fn face(&self) -> Face<'_> {
        Face::parse(&self.data, 0).expect("owned font validated at load")
    }
    pub fn metric(&self, code: u32, dimension: MetricDimension) -> i64 {
        let glyph = self.glyph_id(code);
        if dimension == MetricDimension::Width {
            return self.scaled(self.advances[glyph as usize]);
        }
        self.face()
            .glyph_bounding_box(GlyphId(glyph))
            .map_or(0, |extents| {
                self.scaled(if dimension == MetricDimension::Height {
                    extents.y_max as i64
                } else {
                    -(extents.y_min as i64)
                })
                .max(0)
            })
    }
    pub fn parameter(&self, index: usize) -> i64 {
        let space = self.metric(32, MetricDimension::Width);
        match index {
            2 => space,
            3 => space.div_euclid(2),
            4 | 7 => space.div_euclid(3),
            5 => self.metric(120, MetricDimension::Height),
            6 => self.size,
            _ => 0,
        }
    }
    pub fn hyphenated_width(&self, text: &str, hyphen: u32, ligatures: bool) -> Result<i64> {
        let hyphen = char::from_u32(hyphen)
            .ok_or_else(|| Error::value("Invalid Unicode hyphen"))?
            .to_string();
        Ok(self
            .shape(text, ligatures)?
            .iter()
            .chain(self.shape(&hyphen, ligatures)?.iter())
            .map(|glyph| glyph.advance_sp)
            .sum())
    }
    pub fn shape(&self, text: &str, ligatures: bool) -> Result<Arc<[ShapedGlyph]>> {
        let key = (text.to_string(), ligatures);
        {
            let mut cache = self.cache.borrow_mut();
            cache.clock += 1;
            let clock = cache.clock;
            if let Some((used, result)) = cache.entries.get_mut(&key) {
                *used = clock;
                return Ok(result.clone());
            }
        }
        let result = self.shape_uncached(text, ligatures)?;
        let mut cache = self.cache.borrow_mut();
        if cache.entries.len() >= SHAPED_RUN_CACHE_SIZE {
            let oldest = cache
                .entries
                .iter()
                .min_by_key(|(_, (used, _))| used)
                .map(|(key, _)| key.clone())
                .unwrap();
            cache.entries.remove(&oldest);
        }
        let used = cache.clock;
        cache.entries.insert(key, (used, result.clone()));
        Ok(result)
    }
    fn shape_uncached(&self, text: &str, ligatures: bool) -> Result<Arc<[ShapedGlyph]>> {
        if text.is_empty() {
            return Ok(Arc::from([]));
        }
        let mut buffer = UnicodeBuffer::new();
        let characters: Vec<char> = text.chars().collect();
        for (cluster, &character) in characters.iter().enumerate() {
            buffer.add(character, cluster as u32);
        }
        buffer.guess_segment_properties();
        if buffer.direction() != Direction::LeftToRight {
            return Err(Error::value(
                "Right-to-left paragraph layout is outside this Unicode profile",
            ));
        }
        buffer.set_flags(buffer.flags() | BufferFlags::PRODUCE_UNSAFE_TO_CONCAT);
        let face =
            rustybuzz::Face::from_slice(&self.data, 0).expect("owned font validated at load");
        let features = [
            Feature::new(
                rustybuzz::ttf_parser::Tag::from_bytes(b"liga"),
                u32::from(ligatures),
                ..,
            ),
            Feature::new(
                rustybuzz::ttf_parser::Tag::from_bytes(b"clig"),
                u32::from(ligatures),
                ..,
            ),
        ];
        let shaped = rustybuzz::shape(&face, &features, buffer);
        let infos = shaped.glyph_infos();
        let positions = shaped.glyph_positions();
        let mut boundaries: BTreeSet<_> = infos.iter().map(|info| info.cluster as usize).collect();
        boundaries.insert(characters.len());
        let boundaries: Vec<_> = boundaries.into_iter().collect();
        let ends: BTreeMap<_, _> = boundaries
            .windows(2)
            .map(|pair| (pair[0], pair[1]))
            .collect();
        let mut seen = BTreeSet::new();
        let mut result = Vec::new();
        for (info, position) in infos.iter().zip(positions) {
            let start = info.cluster as usize;
            let end = ends[&start];
            let original = if seen.insert(start) {
                characters[start..end].iter().collect()
            } else {
                String::new()
            };
            let ink = face
                .glyph_bounding_box(GlyphId(info.glyph_id as u16))
                .map_or([0; 4], |bounds| {
                    [
                        bounds.x_min as i64,
                        bounds.y_min as i64,
                        bounds.x_max as i64,
                        bounds.y_max as i64,
                    ]
                });
            result.push(ShapedGlyph {
                glyph_id: info.glyph_id,
                advance_sp: self.scaled(position.x_advance as i64),
                x_offset_sp: self.scaled(position.x_offset as i64),
                y_offset_sp: self.scaled(position.y_offset as i64),
                cluster_start: start,
                cluster_end: end,
                text: original,
                ink_sp: ink.map(|units| self.scaled(units)),
                unsafe_to_break: info.unsafe_to_break(),
            });
        }
        Ok(result.into())
    }
    /// Preserve glyph IDs for the PDF CID map and close composite dependencies.
    /// FontTools metadata rewriting is replaced by native SFNT serialization.
    pub fn subset(&self, glyph_ids: &BTreeSet<u16>) -> Result<Vec<u8>> {
        let mut tables = sfnt::table_directory(&self.data)?;
        let mut selected = glyph_ids.clone();
        selected.insert(0);
        let count = self.advances.len();
        if selected.iter().any(|&glyph| glyph as usize >= count) {
            return Err(Error::value("Invalid Unicode subset glyph"));
        }
        let head = tables.get(b"head").unwrap();
        let long = sfnt::i16_at(head, 50)? != 0;
        let loca = tables
            .get(b"loca")
            .ok_or_else(|| Error::value("Missing Unicode loca"))?;
        let offsets = (0..=count)
            .map(|index| {
                if long {
                    sfnt::u32_at(loca, index * 4).map(|n| n as usize)
                } else {
                    sfnt::u16_at(loca, index * 2).map(|n| n as usize * 2)
                }
            })
            .collect::<Result<Vec<_>>>()?;
        let glyf = tables.get(b"glyf").unwrap();
        if offsets[count] > glyf.len() || offsets.windows(2).any(|pair| pair[0] > pair[1]) {
            return Err(Error::value("Invalid Unicode loca"));
        }
        let mut pending: Vec<u16> = selected.iter().copied().collect();
        while let Some(glyph) = pending.pop() {
            let program = &glyf[offsets[glyph as usize]..offsets[glyph as usize + 1]];
            if program.is_empty() || sfnt::i16_at(program, 0)? >= 0 {
                continue;
            }
            let mut cursor = 10;
            loop {
                let flags = CompositeGlyphFlag(sfnt::u16_at(program, cursor)?);
                let component = sfnt::u16_at(program, cursor + 2)?;
                if component as usize >= count {
                    return Err(Error::value("Invalid Unicode composite glyph"));
                }
                if selected.insert(component) {
                    pending.push(component);
                }
                cursor += 4 + flags.argument_bytes() + flags.transform_bytes();
                if cursor > program.len() {
                    return Err(Error::value("Truncated Unicode composite glyph"));
                }
                if flags.0 & CompositeGlyphFlag::MORE_COMPONENTS == 0 {
                    break;
                }
            }
        }
        let new_count = selected.last().copied().unwrap() as usize + 1;
        let mut new_glyf = Vec::new();
        let mut new_loca = Vec::new();
        for glyph in 0..new_count {
            new_loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());
            if selected.contains(&(glyph as u16)) {
                new_glyf.extend_from_slice(&glyf[offsets[glyph]..offsets[glyph + 1]]);
            }
        }
        new_loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());
        tables.insert(*b"glyf", new_glyf);
        tables.insert(*b"loca", new_loca);
        tables.get_mut(b"head").unwrap()[50..52].copy_from_slice(&1_u16.to_be_bytes());
        tables.get_mut(b"maxp").unwrap()[4..6].copy_from_slice(&(new_count as u16).to_be_bytes());
        let face = self.face();
        let mut hmtx = Vec::new();
        for glyph in 0..new_count {
            let id = GlyphId(glyph as u16);
            let (advance, bearing) = if selected.contains(&id.0) {
                (
                    face.glyph_hor_advance(id).unwrap_or(0),
                    face.glyph_hor_side_bearing(id).unwrap_or(0),
                )
            } else {
                (0, 0)
            };
            hmtx.extend_from_slice(&advance.to_be_bytes());
            hmtx.extend_from_slice(&bearing.to_be_bytes());
        }
        tables.insert(*b"hmtx", hmtx);
        tables.get_mut(b"hhea").unwrap()[34..36].copy_from_slice(&(new_count as u16).to_be_bytes());
        // Layout operates on the full font before subsetting; PDF uses explicit CIDs.
        for tag in [
            b"FFTM", b"GSUB", b"GPOS", b"GDEF", b"kern", b"cmap", b"hdmx", b"LTSH", b"VDMX",
            b"vhea", b"vmtx",
        ] {
            tables.remove(tag);
        }
        if let Some(post) = tables.get_mut(b"post") {
            post.truncate(32);
            post[..4].copy_from_slice(&0x30000_u32.to_be_bytes());
        }
        sfnt::assemble(&tables)
    }
}
