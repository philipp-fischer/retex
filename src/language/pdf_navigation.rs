//! Native PDF structure scanning into retained typed records.
use crate::{
    core::{
        enums::CharacterCategory,
        errors::{Error, Result},
    },
    language::services::ListServices,
    pdf::navigation::{
        Action, AnnotationTemplate, DestinationEffect, Dimensions, Identifier, ThreadTemplate,
    },
    typesetting::model::PdfEffect,
};
pub const OPERATIONS: &[&str] = &[
    "pdfannot",
    "pdfdest",
    "pdfstartlink",
    "pdfendlink",
    "pdfoutline",
    "pdfthread",
    "pdfstartthread",
    "pdfendthread",
    "pdfrunninglinkon",
    "pdfrunninglinkoff",
];
pub const VIEWS: &[(&str, &str)] = &[
    ("xyz", "XYZ"),
    ("fitbh", "FitBH"),
    ("fitbv", "FitBV"),
    ("fitb", "FitB"),
    ("fith", "FitH"),
    ("fitv", "FitV"),
    ("fitr", "FitR"),
    ("fit", "Fit"),
];
pub trait NavigationCommandServices: ListServices {
    fn append_pdf(&mut self, effect: PdfEffect, width: i64, height: i64, depth: i64) -> Result<()>;
    fn reserve_annotation(&mut self) -> usize;
    fn validate_annotation(&mut self, number: usize) -> Result<()>;
    fn set_last_annotation(&mut self, number: usize, link: bool);
    fn publish_annotation(&mut self, t: AnnotationTemplate);
    fn publish_destination(&mut self, t: DestinationEffect) -> Result<Option<usize>>;
    fn publish_thread(&mut self, t: ThreadTemplate) -> usize;
    fn outline(&mut self, a: Action, title: String, count: i64, attributes: String) -> Result<()>;
}
pub fn optional_space(s: &mut impl ListServices) -> Result<()> {
    s.expanded_token()?;
    if s.input().current.category() != Some(CharacterCategory::Spacer) {
        s.back_input()?
    }
    Ok(())
}
pub fn identifier(s: &mut impl ListServices) -> Result<Identifier> {
    match super::pdf_scan_execution::PdfScanExecution::new(
        super::pdf_scan_execution::PdfScanRequest::Identifier,
    )
    .drive(s)?
    {
        super::pdf_scan_execution::PdfScanValue::Identifier(value) => Ok(value),
        _ => Err(Error::runtime("Invalid PDF identifier result")),
    }
}
pub fn dimensions(s: &mut impl ListServices) -> Result<Dimensions> {
    match super::pdf_scan_execution::PdfScanExecution::new(
        super::pdf_scan_execution::PdfScanRequest::Dimensions,
    )
    .drive(s)?
    {
        super::pdf_scan_execution::PdfScanValue::Dimensions(value) => Ok(value),
        _ => Err(Error::runtime("Invalid PDF dimensions result")),
    }
}
pub fn action(s: &mut impl ListServices) -> Result<Action> {
    match super::pdf_scan_execution::PdfScanExecution::new(
        super::pdf_scan_execution::PdfScanRequest::Action,
    )
    .drive(s)?
    {
        super::pdf_scan_execution::PdfScanValue::Action(value) => Ok(value),
        _ => Err(Error::runtime("Invalid PDF action result")),
    }
}
pub fn execute(s: &mut impl NavigationCommandServices, operation: &str) -> Result<()> {
    super::navigation_execution::NavigationExecution::new(operation.into()).drive(s)
}
