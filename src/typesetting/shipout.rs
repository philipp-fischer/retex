//! Reentrant interpreter/output services for ordered shipment (`shipout.py`).
use super::display::Placement;
use super::layout::{Layout, TypesettingContext};
use super::model::{IoEffect, Node, PdfEffect, SpecialEffect};
use crate::core::errors::Result;
pub trait ShipoutServices {
    fn begin_page(
        &mut self,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        height: i64,
        x: i64,
        y: i64,
    ) -> Result<usize>;
    fn finish_page(
        &mut self,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        page_use: usize,
    ) -> Result<()>;
    fn use_font(
        &mut self,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        number: usize,
    ) -> Result<usize>;
    fn perform_io(
        &mut self,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        effect: &IoEffect,
    ) -> Result<()>;
    fn ship_special(
        &mut self,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        effect: &SpecialEffect,
    ) -> Result<Option<(String, String)>>;
    fn ship_special_text(
        &mut self,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        text: &str,
    ) -> Result<Option<(String, String)>>;
    fn ship_extension(
        &mut self,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        effect: &PdfEffect,
        x: i64,
        y: i64,
        doing_leaders: bool,
        node: &Node,
    ) -> Result<Option<(String, String)>>;
    fn enter_box(
        &mut self,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        node: &Node,
        x: i64,
        y: i64,
        doing_leaders: bool,
    ) -> Result<()>;
    fn leave_box(
        &mut self,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
    ) -> Result<()>;
    fn capture_paint(
        &mut self,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        placement: Placement,
    ) -> Result<Vec<Placement>>;
}
