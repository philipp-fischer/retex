//! Selected-size font records and metric policy (`fonts/model.py`).
use super::expansion::ExpansionPolicy;
use super::formats::{
    afm::AfmMetrics,
    tfm::{CharacterTag, TfmFont},
    type1::STANDARD_ENCODING,
    vf::VirtualFont,
};
use super::ligatures::{LigatureInstruction, decode_ligature_program};
use super::materialization::FontMaterialization;
use super::microtypography::{FontCode, TypographyFont};
use super::scaling::{FIX_WORD_TO_SCALED_SHIFT, divide_scaled, pdf_font_size, scale_fix_word};
use super::unicode::{MetricDimension, UnicodeFont};
use crate::core::constants::Constants as C;
use crate::core::errors::Result;
use crate::core::persistence::SharedTable;
use serde::{Deserialize, Serialize};
use std::cell::{Ref, RefCell, RefMut};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

pub type MicroCodes = SharedTable<FontCode, BTreeMap<u32, i64>>;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FontInfo {
    pub materialization: FontMaterialization,
    pub name: String,
    pub size: i64,
    pub tfm_widths: Option<SharedTable<u32, i64>>,
    pub tfm_heights: Option<SharedTable<u32, i64>>,
    pub tfm_depths: Option<SharedTable<u32, i64>>,
    pub tfm_lig_kern: Option<Arc<[LigatureInstruction]>>,
    pub tfm_lig_start: Option<SharedTable<u32, usize>>,
    pub boundary_char: u32,
    pub boundary_label: Option<usize>,
    pub false_boundary_char: u32,
    pub tfm_kerns: Option<Vec<i64>>,
    pub tfm_design_size: Option<i64>,
    pub tfm_data: Option<Arc<TfmFont>>,
    pub afm: Option<Arc<AfmMetrics>>,
    pub char_width: i64,
    pub char_height: i64,
    pub char_depth: i64,
    pub space_width: i64,
    pub parameter_overrides: SharedTable<usize, i64>,
    pub tfm_file: Option<String>,
    pub parameter_count: usize,
    pub hyphenchar: i64,
    pub skewchar: i64,
    pub dynamic: bool,
    pub null_font: bool,
    pub virtual_font: Option<Arc<VirtualFont>>,
    pub unicode_font: Option<Arc<UnicodeFont>>,
    pub virtual_checked: bool,
    pub ligatures_disabled: bool,
    pub micro_codes: Rc<RefCell<MicroCodes>>,
    pub width_overrides: SharedTable<u32, i64>,
    pub letterspacing: Option<(usize, i64)>,
    pub italic_overrides: SharedTable<u32, i64>,
    pub expansion_policy: Option<ExpansionPolicy>,
    pub expansion_base: Option<usize>,
    pub expansion_ratio: i64,
    pub expansion_variants: SharedTable<i64, usize>,
}
impl Default for FontInfo {
    fn default() -> Self {
        let size = 11 * C::UNITY;
        Self {
            materialization: FontMaterialization::default(),
            name: "Times-Roman".into(),
            size,
            tfm_widths: None,
            tfm_heights: None,
            tfm_depths: None,
            tfm_lig_kern: None,
            tfm_lig_start: None,
            boundary_char: 256,
            boundary_label: None,
            false_boundary_char: 256,
            tfm_kerns: None,
            tfm_design_size: None,
            tfm_data: None,
            afm: None,
            char_width: size / 2,
            char_height: size * 7 / 10,
            char_depth: size / 5,
            space_width: size / 4,
            parameter_overrides: Default::default(),
            tfm_file: None,
            parameter_count: 7,
            hyphenchar: 45,
            skewchar: -1,
            dynamic: false,
            null_font: false,
            virtual_font: None,
            unicode_font: None,
            virtual_checked: false,
            ligatures_disabled: false,
            micro_codes: Rc::default(),
            width_overrides: Default::default(),
            letterspacing: None,
            italic_overrides: Default::default(),
            expansion_policy: None,
            expansion_base: None,
            expansion_ratio: 0,
            expansion_variants: Default::default(),
        }
    }
}
impl FontInfo {
    pub fn set_ligature_program(&mut self, program: &[[u8; 4]]) {
        self.tfm_lig_kern = Some(decode_ligature_program(program).into());
    }
    pub fn no_ligatures(&self) -> bool {
        if let Some(tfm) = &self.tfm_data {
            !tfm.char_tags
                .values()
                .any(|&(tag, _)| matches!(tag, CharacterTag::Ligature | CharacterTag::Extensible))
        } else {
            self.tfm_lig_start
                .as_ref()
                .is_none_or(|starts| starts.is_empty())
        }
    }
    pub fn metric_mode(&self) -> &'static str {
        if self.unicode_font.is_some() {
            "unicode_opentype"
        } else if self.null_font {
            "null_font"
        } else if self.tfm_data.is_some()
            || self
                .tfm_widths
                .as_ref()
                .is_some_and(|widths| !widths.is_empty())
        {
            "tfm_selected_size"
        } else if self.afm.is_some() {
            "standard14_afm"
        } else {
            "approximate"
        }
    }
    pub fn glyph_encoding(&self) -> Vec<String> {
        if let Some(encoding) = &self.materialization.encoding {
            let encoding = encoding.borrow();
            if !encoding.glyph_names.is_empty() {
                return encoding.glyph_names.clone();
            }
        }
        if let Some(encoding) = &self.materialization.native_encoding {
            if !encoding.is_empty() {
                return encoding.to_vec();
            }
        }
        if self.afm.is_some() || self.materialization.nonembedded {
            STANDARD_ENCODING
                .iter()
                .map(|name| name.to_string())
                .collect()
        } else {
            Vec::new()
        }
    }
    pub fn has_character(&self, code: u32) -> bool {
        if let Some(font) = &self.unicode_font {
            font.cmap.contains_key(&code)
        } else if self.null_font {
            false
        } else if let Some(tfm) = &self.tfm_data {
            tfm.width_fix.contains_key(&code)
        } else if let Some(afm) = &self.afm {
            afm.widths.contains_key(&code)
        } else {
            true
        }
    }
    pub fn metric(&self, code: u32, dimension: MetricDimension) -> Option<i64> {
        if let Some(font) = &self.unicode_font {
            return Some(font.metric(code, dimension));
        }
        if self.null_font {
            return Some(0);
        }
        if let Some(tfm) = &self.tfm_data {
            let table = match dimension {
                MetricDimension::Width => &tfm.width_fix,
                MetricDimension::Height => &tfm.height_fix,
                MetricDimension::Depth => &tfm.depth_fix,
            };
            return Some(scale_fix_word(
                table.get(&code).copied().unwrap_or(0),
                self.size,
            ));
        }
        if self
            .tfm_widths
            .as_ref()
            .is_some_and(|widths| !widths.is_empty())
        {
            let table = match dimension {
                MetricDimension::Width => &self.tfm_widths,
                MetricDimension::Height => &self.tfm_heights,
                MetricDimension::Depth => &self.tfm_depths,
            };
            let value = table
                .as_ref()
                .and_then(|table| table.get(&code))
                .copied()
                .unwrap_or(0);
            let design = self
                .tfm_design_size
                .filter(|size| *size != 0)
                .unwrap_or(self.size);
            return Some((value as i128 * self.size as i128).div_euclid(design as i128) as i64);
        }
        if let Some(afm) = &self.afm {
            let table = match dimension {
                MetricDimension::Width => &afm.widths,
                MetricDimension::Height => &afm.heights,
                MetricDimension::Depth => &afm.depths,
            };
            return Some(
                (table.get(&code).copied().unwrap_or(0) as i128 * self.size as i128)
                    .div_euclid(1000) as i64,
            );
        }
        None
    }
    pub fn get_char_width(&self, code: u32) -> i64 {
        if let Some(&width) = self.width_overrides.get(&code) {
            return width;
        }
        if let Some(width) = self.metric(code, MetricDimension::Width) {
            return width;
        }
        if [105, 108, 73, 46, 44, 33].contains(&code) {
            self.char_width.div_euclid(2)
        } else if [109, 119, 77, 87].contains(&code) {
            (self.char_width * 3).div_euclid(2)
        } else {
            self.char_width
        }
    }
    pub fn get_char_height(&self, code: u32) -> i64 {
        self.metric(code, MetricDimension::Height)
            .unwrap_or(self.char_height)
    }
    pub fn get_char_depth(&self, code: u32) -> i64 {
        self.metric(code, MetricDimension::Depth)
            .unwrap_or(self.char_depth)
    }
    pub fn get_char_italic(&self, code: u32) -> i64 {
        self.italic_overrides
            .get(&code)
            .copied()
            .unwrap_or_else(|| {
                self.tfm_data.as_ref().map_or(0, |tfm| {
                    scale_fix_word(tfm.italic_fix.get(&code).copied().unwrap_or(0), self.size)
                })
            })
    }
    pub fn pdf_char_width(&self, code: u32) -> Result<f64> {
        Ok(divide_scaled(self.get_char_width(code), pdf_font_size(self.size), 4)? as f64 / 10.0)
    }
    pub fn get_parameter(&self, index: usize) -> i64 {
        if let Some(&value) = self.parameter_overrides.get(&index) {
            return value;
        }
        if let Some(font) = &self.unicode_font {
            return font.parameter(index);
        }
        if self.null_font {
            return 0;
        }
        if let Some(tfm) = &self.tfm_data {
            let Some(&value) = index
                .checked_sub(1)
                .and_then(|index| tfm.param_fix.get(index))
            else {
                return 0;
            };
            return if index == 1 {
                value >> FIX_WORD_TO_SCALED_SHIFT
            } else {
                scale_fix_word(value, self.size)
            };
        }
        match index {
            2 => self.afm.as_ref().map_or(self.space_width, |afm| {
                (afm.widths.get(&32).copied().unwrap_or(0) * self.size).div_euclid(1000)
            }),
            5 => self.afm.as_ref().map_or(self.size.div_euclid(2), |afm| {
                (afm.x_height * self.size).div_euclid(1000)
            }),
            6 => self.size,
            _ => 0,
        }
    }
    pub fn has_lig_kern(&self, code: u32) -> bool {
        self.tfm_lig_start
            .as_ref()
            .is_some_and(|starts| starts.contains_key(&code))
    }
}
impl TypographyFont for FontInfo {
    fn name(&self) -> &str {
        &self.name
    }
    fn size(&self) -> i64 {
        self.size
    }
    fn has_character(&self, code: u32) -> bool {
        Self::has_character(self, code)
    }
    fn char_width(&self, code: u32) -> i64 {
        self.get_char_width(code)
    }
    fn parameter(&self, index: usize) -> i64 {
        self.get_parameter(index)
    }
    fn tfm(&self) -> Option<&TfmFont> {
        self.tfm_data.as_deref()
    }
    fn tfm_mut(&mut self) -> Option<&mut TfmFont> {
        self.tfm_data.as_mut().map(Arc::make_mut)
    }
    fn codes(&self) -> Ref<'_, MicroCodes> {
        self.micro_codes.borrow()
    }
    fn codes_mut(&mut self) -> RefMut<'_, MicroCodes> {
        self.micro_codes.borrow_mut()
    }
    fn remove_ligature_start(&mut self, code: u32) {
        if let Some(starts) = &mut self.tfm_lig_start {
            starts.remove(&code);
        }
    }
    fn install_virtual(
        &mut self,
        virtual_font: VirtualFont,
        width_overrides: BTreeMap<u32, i64>,
        letterspacing: (usize, i64),
    ) {
        self.virtual_font = Some(Arc::new(virtual_font));
        self.width_overrides = width_overrides.into();
        self.letterspacing = Some(letterspacing);
    }
}
impl crate::pdf::cursor::CursorFont for FontInfo {
    type BitmapOptions = super::formats::bitmap::BitmapOptions;
    fn char_width(&self, code: u32) -> i64 {
        self.get_char_width(code)
    }
    fn bitmap_advance(&self, code: u32, options: &Self::BitmapOptions) -> Result<i64> {
        match self.materialization.resource.as_deref() {
            Some(super::materialization::FontResource::Bitmap(resource)) => {
                resource.cursor_advance(self.size, self.get_char_width(code), options)
            }
            _ => Err(crate::core::errors::Error::value(
                "Bitmap font resource unavailable",
            )),
        }
    }
}
