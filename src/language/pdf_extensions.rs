//! Source object, image, form, color and coordinate policy commands.
use crate::{
    core::errors::{Error, Result},
    language::pdf_navigation::{self, NavigationCommandServices},
    pdf::{document::PDFDocument, images::ImageOptions},
    typesetting::model::PdfEffect,
};
pub const OPERATIONS: &[&str] = &[
    "pdfobj",
    "pdfrefobj",
    "pdfsavepos",
    "pdfinfo",
    "pdfcatalog",
    "pdfnames",
    "pdftrailer",
    "pdftrailerid",
    "pdfcolorstack",
    "pdfsetmatrix",
    "pdfsave",
    "pdfrestore",
    "pdfxform",
    "pdfrefxform",
    "pdfximage",
    "pdfrefximage",
    "pdfsnaprefpoint",
    "pdfsnapy",
    "pdfsnapycomp",
    "pdffontexpand",
    "pdfinterwordspaceon",
    "pdfinterwordspaceoff",
    "pdffakespace",
    "pdfspacefont",
];
pub const ITEMS: &[&str] = &[
    "pdflastobj",
    "pdfretval",
    "pdflastxpos",
    "pdflastypos",
    "pdflastxform",
    "pdflastximage",
    "pdflastximagepages",
    "pdflastximagecolordepth",
    "pdflastannot",
    "pdflastlink",
];
pub trait ExtensionServices: NavigationCommandServices {
    fn document(&self) -> &PDFDocument;
    fn document_mut(&mut self) -> &mut PDFDocument;
    fn reserve_pdf_object(&mut self) -> usize;
    fn reserve_source_object(&mut self, number: Option<usize>) -> Result<usize>;
    fn check_pdf_version(&mut self) -> Result<()>;
    fn emit_source_object(&mut self, number: usize) -> Result<()>;
    fn emit_form(&mut self, number: usize) -> Result<()>;
    fn load_image(
        &mut self,
        number: usize,
        name: String,
        spec: crate::pdf::navigation::Dimensions,
        options: ImageOptions,
    ) -> Result<()>;
    fn emit_image(&mut self, number: usize) -> Result<()>;
    fn take_box_register(&mut self, number: i64) -> Result<i64>;
    fn pdf_action_string(&mut self, action: crate::pdf::navigation::Action) -> Result<String>;
}
pub fn item(d: &PDFDocument, name: &str) -> Result<i64> {
    Ok(match name {
        "pdflastobj" => d.last_object as i64,
        "pdfretval" => d.return_value,
        "pdflastxpos" => d.last_x,
        "pdflastypos" => d.last_y,
        "pdflastxform" => d.last_form as i64,
        "pdflastximage" => d.last_image as i64,
        "pdflastximagepages" => d.last_image_pages as i64,
        "pdflastximagecolordepth" => d.last_image_depth as i64,
        "pdflastannot" => d.navigation.last_annotation as i64,
        "pdflastlink" => d.navigation.last_link as i64,
        _ => return Err(Error::value("Unknown PDF item")),
    })
}
pub fn execute(s: &mut impl ExtensionServices, operation: &str, immediate: bool) -> Result<()> {
    if super::pdf_operand_execution::PdfOperandExecution::supports(operation) {
        return super::pdf_operand_execution::PdfOperandExecution::new(operation.into(), immediate)
            .drive(s);
    }
    if pdf_navigation::OPERATIONS.contains(&operation) {
        return pdf_navigation::execute(s, operation);
    }
    match operation {
        "pdfinterwordspaceon" | "pdfinterwordspaceoff" | "pdffakespace" => {
            s.document_mut().extraction.active = true;
            s.append_pdf(
                match operation {
                    "pdfinterwordspaceon" => PdfEffect::ExtractionOn,
                    "pdfinterwordspaceoff" => PdfEffect::ExtractionOff,
                    _ => PdfEffect::FakeSpace,
                },
                0,
                0,
                0,
            )?;
        }
        "pdfsnaprefpoint" => s.append_pdf(PdfEffect::SnapReferencePoint, 0, 0, 0)?,
        "pdfximage" | "pdfrefximage" => {
            super::image_execution::ImageExecution::new(operation == "pdfrefximage", immediate)
                .drive(s)?;
        }
        "pdfxform" | "pdfrefxform" => {
            super::form_execution::FormExecution::new(operation == "pdfrefxform", immediate)
                .drive(s)?
        }
        "pdfsave" => s.append_pdf(PdfEffect::Save, 0, 0, 0)?,
        "pdfrestore" => s.append_pdf(PdfEffect::Restore, 0, 0, 0)?,
        "pdfsavepos" => s.append_pdf(PdfEffect::SavePosition, 0, 0, 0)?,
        _ => return Err(Error::value(format!("Unknown PDF extension: {operation}"))),
    }
    Ok(())
}
