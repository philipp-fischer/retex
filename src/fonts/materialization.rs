//! Independent per-job outline binding state (`fonts/materialization.py`).
use super::encoding::EncFileParser;
use super::formats::{bitmap::BitmapResource, sfnt::SfntResource, type1::Type1Resource};
use super::maps::FontMapEntry;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FontResource {
    Type1(Type1Resource),
    Sfnt(SfntResource),
    Bitmap(BitmapResource),
}
impl FontResource {
    pub fn data(&self) -> &[u8] {
        match self {
            Self::Type1(resource) => &resource.data,
            Self::Sfnt(resource) => &resource.data,
            Self::Bitmap(resource) => &resource.data,
        }
    }
    pub fn encoding(&self) -> &[String] {
        match self {
            Self::Type1(resource) => &resource.encoding,
            Self::Sfnt(resource) => &resource.encoding,
            Self::Bitmap(resource) => &resource.encoding,
        }
    }
    pub fn name(&self) -> &str {
        match self {
            Self::Type1(resource) => &resource.name,
            Self::Sfnt(resource) => &resource.name,
            Self::Bitmap(resource) => &resource.name,
        }
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Type1(_) => "type1",
            Self::Sfnt(resource) => {
                if resource.kind == super::formats::sfnt::SfntKind::Truetype {
                    "truetype"
                } else {
                    "opentype"
                }
            }
            Self::Bitmap(resource) => {
                if resource.kind == super::formats::bitmap::BitmapKind::Pk {
                    "pk"
                } else {
                    "pgc"
                }
            }
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FontMaterialization {
    pub pdf_name: String,
    pub pfb_file: Option<String>,
    pub pfb_data: Option<Arc<[u8]>>,
    pub resource: Option<Arc<FontResource>>,
    pub resource_error: Option<String>,
    pub enc_file: Option<String>,
    pub encoding: Option<Rc<RefCell<EncFileParser>>>,
    pub native_encoding: Option<Arc<[String]>>,
    pub map_bound: bool,
    pub bound_mapping: Option<FontMapEntry>,
    pub pdf_kind: String,
    pub embedding_available: bool,
    pub resource_ready: bool,
    pub nonembedded: bool,
    pub map_flags: Option<i64>,
    pub subset: bool,
    pub included_codes: BTreeSet<u32>,
    pub map_identity: usize,
    pub font_attributes: String,
    pub no_builtin_tounicode: bool,
    pub subfont: Option<Vec<i32>>,
    pub pid_eid: (u16, u16),
}
impl Default for FontMaterialization {
    fn default() -> Self {
        Self {
            pdf_name: "Times-Roman".into(),
            pfb_file: None,
            pfb_data: None,
            resource: None,
            resource_error: None,
            enc_file: None,
            encoding: None,
            native_encoding: None,
            map_bound: false,
            bound_mapping: None,
            pdf_kind: "scalable".into(),
            embedding_available: false,
            resource_ready: false,
            nonembedded: false,
            map_flags: None,
            subset: false,
            included_codes: BTreeSet::new(),
            map_identity: 0,
            font_attributes: String::new(),
            no_builtin_tounicode: false,
            subfont: None,
            pid_eid: (3, 1),
        }
    }
}
