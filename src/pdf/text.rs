//! Historical PDF cursor rounding, separate from logical TeX metrics (`text.py`).
use super::cursor::{self, COORDINATE_DECIMALS, FONT_SIZE_DECIMALS, SP_PER_HUNDRED_BP};
use super::syntax::character_string;
use crate::core::errors::Result;
use crate::core::units::sp_to_bp;
use crate::fonts::formats::bitmap::BitmapOptions;
use crate::fonts::model::FontInfo;
use crate::fonts::scaling::{divide_scaled, pdf_font_size, scaled_division};
use crate::typesetting::display::Placement;
#[derive(Clone, Debug, PartialEq)]
pub struct HistoricalOrigin {
    pub h: i64,
    pub v: i64,
    pub decimals: u32,
}
impl HistoricalOrigin {
    pub fn move_to(&mut self, h: i64, v: i64) -> Result<Option<(f64, f64)>> {
        let quantum = divide_scaled(SP_PER_HUNDRED_BP, 10_i64.pow(self.decimals + 2), 0)?;
        if (h - self.h).abs() < quantum && (v - self.v).abs() < quantum {
            return Ok(None);
        }
        let (x, represented_h) = cursor::coordinate(h - self.h, self.decimals)?;
        let (y, represented_v) = cursor::coordinate(self.v - v, self.decimals)?;
        self.h += represented_h;
        self.v -= represented_v;
        Ok(Some((
            x as f64 / 10_f64.powi(self.decimals as i32),
            y as f64 / 10_f64.powi(self.decimals as i32),
        )))
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct HistoricalText {
    pub page_height: i64,
    pub decimals: u32,
    pub origin_sp: Option<(i64, i64)>,
    pub font: Option<usize>,
    pub start_h: i64,
    pub baseline: i64,
    pub delta_h: i64,
    pub line_x: f64,
    pub line_y: f64,
    pub cursor_x: f64,
    pub expansion_ratio: i64,
    pub inserted_dummy: bool,
}
impl HistoricalText {
    pub fn new(page_height: i64, decimals: Option<u32>, origin_sp: Option<(i64, i64)>) -> Self {
        let (start_h, baseline) = origin_sp.unwrap_or((0, page_height));
        Self {
            page_height,
            decimals: decimals.unwrap_or(COORDINATE_DECIMALS),
            origin_sp,
            font: None,
            start_h,
            baseline,
            delta_h: 0,
            line_x: 0.0,
            line_y: 0.0,
            cursor_x: 0.0,
            expansion_ratio: 0,
            inserted_dummy: false,
        }
    }
    pub fn emit(
        &mut self,
        item: &Placement,
        font: &FontInfo,
        resource: &str,
        origin: (f64, f64),
        mut advance: f64,
        operators: &mut Vec<String>,
        expansion_ratio: i64,
        bitmap_options: Option<&BitmapOptions>,
    ) -> Result<(f64, f64, f64, f64)> {
        let (size_units, _) =
            scaled_division(font.size, SP_PER_HUNDRED_BP, FONT_SIZE_DECIMALS + 2)?;
        let size = size_units as f64 / 10_f64.powi(FONT_SIZE_DECIMALS as i32);
        if item.kind == "extraction_space"
            && item.literal_mode == "interword_space"
            && self.font.is_some()
            && self.font != Some(item.font)
        {
            operators.push(format!("/{resource} {size:.4} Tf"));
            operators.push("( ) Tj".into());
            self.inserted_dummy = true;
            advance *= size / sp_to_bp(font.size) * (1.0 + self.expansion_ratio as f64 / 1000.0);
            let x = self.cursor_x;
            self.cursor_x += advance;
            return Ok((x, self.line_y, size, advance));
        }
        let distance = item.x_sp - self.start_h - self.delta_h;
        let font_size = pdf_font_size(font.size);
        let (mut kern, represented_kern) = cursor::kern(distance, font_size, self.expansion_ratio)?;
        let (vertical, represented_vertical) =
            cursor::coordinate(self.baseline - item.baseline_y_sp, self.decimals)?;
        let changed_font = self.font != Some(item.font);
        if changed_font || self.inserted_dummy {
            operators.push(format!("/{resource} {size:.4} Tf"))
        }
        if cursor::resets_matrix(changed_font || self.inserted_dummy, vertical, kern) {
            if expansion_ratio != 0 || self.expansion_ratio != 0 {
                let (origin_h, origin_v) = self.origin_sp.unwrap_or((0, self.page_height));
                let (horizontal, represented_horizontal) =
                    cursor::coordinate(item.x_sp - origin_h, self.decimals)?;
                let (vertical, represented_vertical) =
                    cursor::coordinate(origin_v - item.baseline_y_sp, self.decimals)?;
                self.start_h = origin_h + represented_horizontal;
                self.line_x = origin.0 + horizontal as f64 / 10_f64.powi(self.decimals as i32);
                self.line_y = origin.1 + vertical as f64 / 10_f64.powi(self.decimals as i32);
                self.baseline = origin_v - represented_vertical;
                self.expansion_ratio = expansion_ratio;
                operators.push(format!(
                    "{:.3} 0 0 1 {:.9} {:.9} Tm",
                    1.0 + expansion_ratio as f64 / 1000.0,
                    self.line_x - origin.0,
                    self.line_y - origin.1
                ))
            } else {
                self.move_unexpanded(item, vertical, represented_vertical, origin, operators)?
            }
            self.cursor_x = self.line_x;
            self.delta_h = 0;
            kern = 0;
        }
        self.font = Some(item.font);
        self.inserted_dummy = false;
        if kern != 0 {
            self.delta_h += represented_kern;
            self.cursor_x += kern as f64 * size / 10_f64.powi(cursor::KERN_RATIO_DECIMALS as i32)
                * (1.0 + self.expansion_ratio as f64 / 1000.0)
        }
        operators.push(format!("[{}({})] TJ", -kern, character_string(item.code)));
        let x = self.cursor_x;
        self.delta_h += cursor::character_advance(
            font,
            item.code,
            font_size,
            self.expansion_ratio,
            bitmap_options,
        )?;
        advance *= size / sp_to_bp(font.size);
        self.cursor_x += advance;
        Ok((x, self.line_y, size, advance))
    }
    pub fn move_unexpanded(
        &mut self,
        item: &Placement,
        vertical: i64,
        represented_vertical: i64,
        origin: (f64, f64),
        operators: &mut Vec<String>,
    ) -> Result<()> {
        let (horizontal, represented_horizontal) =
            cursor::coordinate(item.x_sp - self.start_h, self.decimals)?;
        let dx = horizontal as f64 / 10_f64.powi(self.decimals as i32);
        let dy = vertical as f64 / 10_f64.powi(self.decimals as i32);
        if self.font.is_none() {
            self.line_x = origin.0 + dx;
            self.line_y = origin.1 + dy;
            operators.push(format!(
                "{:.9} {:.9} Td",
                self.line_x - origin.0,
                self.line_y - origin.1
            ))
        } else {
            operators.push(format!(
                "{dx:.precision$} {dy:.precision$} Td",
                precision = self.decimals as usize
            ));
            self.line_x += dx;
            self.line_y += dy
        }
        self.start_h += represented_horizontal;
        self.baseline -= represented_vertical;
        Ok(())
    }
}
