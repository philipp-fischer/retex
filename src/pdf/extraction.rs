//! Invisible interword extraction spaces and a rounded shadow cursor (`extraction.py`).
use super::cursor;
use crate::core::errors::Result;
use crate::fonts::catalog::FontCatalog;
use crate::fonts::scaling::pdf_font_size;
use crate::typesetting::display::Placement;
use serde::{Deserialize, Serialize};
pub trait ExtractionServices {
    fn fonts(&self) -> &FontCatalog;
    fn fonts_mut(&mut self) -> &mut FontCatalog;
    fn precision(&mut self) -> Result<u32>;
    fn dummy_space_font(&mut self) -> Result<usize>;
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExtractionSpaces {
    pub enabled: bool,
    pub active: bool,
    pub font: Option<usize>,
    pub start_h: i64,
    pub delta_h: i64,
    pub baseline: i64,
}
impl ExtractionSpaces {
    pub fn reset_cursor(&mut self) {
        self.font = None;
        self.start_h = 0;
        self.delta_h = 0;
        self.baseline = 0
    }
    pub fn advance(
        &mut self,
        services: &mut impl ExtractionServices,
        item: &Placement,
    ) -> Result<Option<i64>> {
        let font_size = pdf_font_size(services.fonts().items[item.font].size);
        let (kern, represented) =
            cursor::kern(item.x_sp - self.start_h - self.delta_h, font_size, 0)?;
        let (vertical, represented_vertical) =
            cursor::coordinate(self.baseline - item.baseline_y_sp, services.precision()?)?;
        let reset = cursor::resets_matrix(self.font != Some(item.font), vertical, kern);
        let gap = if reset { None } else { Some(represented) };
        if reset {
            let (_, horizontal) =
                cursor::coordinate(item.x_sp - self.start_h, services.precision()?)?;
            self.start_h += horizontal;
            self.baseline -= represented_vertical;
            self.delta_h = 0
        } else {
            self.delta_h += represented
        }
        self.delta_h += cursor::character_advance(
            &services.fonts().items[item.font],
            item.code,
            font_size,
            0,
            None,
        )?;
        self.font = Some(item.font);
        Ok(gap)
    }
    pub fn capture(
        &mut self,
        services: &mut impl ExtractionServices,
        mut item: Placement,
    ) -> Result<Vec<Placement>> {
        if !self.active {
            return Ok(vec![item]);
        }
        if item.kind == "literal" && item.literal_mode == "fake_space" {
            // Python's replaced record has a new identity; reserve a new paint ID.
            item.placement_id = usize::MAX;
            item.kind = "extraction_space".into();
            item.font = item
                .literal_text
                .parse()
                .map_err(|_| crate::core::errors::Error::value("Invalid extraction font"))?;
            item.code = 32;
            item.glyph_name = Some("space".into());
            item.literal_text.clear();
            self.advance(services, &item)?;
            return Ok(vec![item]);
        }
        if item.kind == "literal" && matches!(item.literal_mode.as_str(), "space_on" | "space_off")
        {
            return Ok(Vec::new());
        }
        if item.kind == "literal" || item.kind == "rule" {
            if item.kind != "literal" || item.literal_mode != "direct" {
                self.reset_cursor()
            }
            return Ok(vec![item]);
        }
        if item.kind != "glyph" {
            return Ok(vec![item]);
        }
        let old_end = self.start_h + self.delta_h;
        let old_baseline = self.baseline;
        let gap = self.advance(services, &item)?;
        let font = &services.fonts().items[item.font];
        if !self.enabled
            || gap.is_none_or(|gap| gap <= font.get_parameter(2) - font.get_parameter(4))
        {
            return Ok(vec![item]);
        }
        let real_space = font
            .materialization
            .encoding
            .as_ref()
            .is_some_and(|encoding| {
                encoding
                    .borrow()
                    .glyph_names
                    .get(32)
                    .is_some_and(|name| name == "space")
            })
            && font.materialization.pdf_kind == "scalable";
        let number = if real_space {
            item.font
        } else {
            services.dummy_space_font()?
        };
        services.fonts_mut().items[number]
            .materialization
            .included_codes
            .insert(32);
        let mut space = item.clone();
        space.placement_id = usize::MAX;
        space.kind = "extraction_space".into();
        space.literal_mode = "interword_space".into();
        space.font = number;
        space.code = 32;
        space.x_sp = old_end;
        space.baseline_y_sp = old_baseline;
        space.width_sp = 0;
        space.height_sp = 0;
        space.depth_sp = 0;
        space.glyph_name = Some("space".into());
        space.components.clear();
        space.origins.clear();
        Ok(vec![space, item])
    }
}
