//! Ordered image operands shared by synchronous and resident execution.
use super::{
    pdf_extensions::ExtensionServices,
    pdf_scan_execution::{PdfScanExecution, PdfScanRequest, PdfScanValue},
    records::{Quantity, ValueKey},
    scanning::{ScanRequest, ScanValue},
};
use crate::{
    core::errors::{Error, Result},
    pdf::{
        image_records::ImageUse,
        images::{ImageOptions, PAGE_BOXES, page_box},
        navigation::Dimensions,
    },
    typesetting::model::PdfEffect,
};

pub(crate) enum ImageAction {
    Scan(ScanRequest),
    Text,
    Dimensions,
}
pub(crate) enum ImageValue {
    Scan(ScanValue),
    Text(String),
    Dimensions(Dimensions),
}
pub(crate) enum ImageStep {
    Yield(ImageAction),
    Complete,
}
enum Phase {
    Start,
    Reference,
    Dimensions,
    Attribute,
    AttributeText,
    Named,
    NamedText,
    Page,
    PageNumber,
    Colorspace,
    ColorspaceNumber,
    PageBox,
    Filename,
    Done,
}
pub(crate) struct ImageExecution {
    reference: bool,
    immediate: bool,
    phase: Phase,
    number: usize,
    dimensions: Dimensions,
    attributes: String,
    named: Option<String>,
    page: i64,
    colorspace: i64,
    pagebox: String,
    box_index: usize,
}
impl ImageExecution {
    pub(crate) fn new(reference: bool, immediate: bool) -> Self {
        Self {
            reference,
            immediate,
            phase: Phase::Start,
            number: 0,
            dimensions: Dimensions::new(),
            attributes: String::new(),
            named: None,
            page: 1,
            colorspace: 0,
            pagebox: String::new(),
            box_index: 0,
        }
    }
    pub(crate) fn drive(mut self, services: &mut impl ExtensionServices) -> Result<()> {
        let mut returned = None;
        loop {
            match self.resume(services, returned.take())? {
                ImageStep::Complete => return Ok(()),
                ImageStep::Yield(action) => {
                    returned = Some(match action {
                        ImageAction::Scan(request) => services.scan(request).map(ImageValue::Scan),
                        ImageAction::Text => services.text().map(ImageValue::Text),
                        ImageAction::Dimensions => {
                            PdfScanExecution::new(PdfScanRequest::Dimensions)
                                .drive(services)
                                .and_then(|value| match value {
                                    PdfScanValue::Dimensions(value) => {
                                        Ok(ImageValue::Dimensions(value))
                                    }
                                    _ => Err(Error::runtime("Invalid image dimensions result")),
                                })
                        }
                    })
                }
            }
        }
    }
    fn scan(&mut self, phase: Phase, request: ScanRequest) -> ImageStep {
        self.phase = phase;
        ImageStep::Yield(ImageAction::Scan(request))
    }
    fn keyword(&mut self, phase: Phase, name: &str) -> ImageStep {
        self.scan(phase, ScanRequest::Keyword(name.into()))
    }
    fn text(&mut self, phase: Phase) -> ImageStep {
        self.phase = phase;
        ImageStep::Yield(ImageAction::Text)
    }
    fn pagebox(&mut self, services: &mut impl ExtensionServices) -> Result<ImageStep> {
        if self.pagebox.is_empty() {
            self.pagebox = page_box(services.parameter("pdfpagebox", 0))
                .ok_or_else(|| Error::value("Invalid PDF page-box selector"))?
                .into();
        }
        for (obsolete, replacement) in [
            ("pdfoptionalwaysusepdfpagebox", "pdfforcepagebox"),
            ("pdfoptionpdfinclusionerrorlevel", "pdfinclusionerrorlevel"),
        ] {
            let value = services.parameter(obsolete, 0);
            if value != 0 {
                services.warning(&format!("{obsolete} is obsolete; use {replacement}"));
                services.env_mut().assign_value(
                    ValueKey::parameter(replacement),
                    Quantity::Integer(value),
                    false,
                );
                services.env_mut().assign_value(
                    ValueKey::parameter(obsolete),
                    Quantity::Integer(0),
                    false,
                );
            }
        }
        let forced = services.parameter("pdfforcepagebox", 0);
        if forced > 0 {
            services.warning("pdfforcepagebox is obsolete; use pdfpagebox");
            self.pagebox = page_box(forced)
                .ok_or_else(|| Error::value("Invalid forced PDF page-box selector"))?
                .into();
        }
        Ok(self.text(Phase::Filename))
    }
    pub(crate) fn resume(
        &mut self,
        services: &mut impl ExtensionServices,
        returned: Option<Result<ImageValue>>,
    ) -> Result<ImageStep> {
        fn scan(value: Option<ImageValue>) -> Result<ScanValue> {
            match value {
                Some(ImageValue::Scan(value)) => Ok(value),
                _ => Err(Error::runtime("Invalid image scanner result")),
            }
        }
        fn text(value: Option<ImageValue>) -> Result<String> {
            match value {
                Some(ImageValue::Text(value)) => Ok(value),
                _ => Err(Error::runtime("Invalid image text result")),
            }
        }
        let value = returned.transpose()?;
        Ok(match self.phase {
            Phase::Start => {
                if value.is_some() {
                    return Err(Error::runtime("Unrequested image operand result"));
                }
                if self.reference {
                    self.scan(Phase::Reference, ScanRequest::Int)
                } else {
                    services.check_pdf_version()?;
                    self.number = services.reserve_pdf_object();
                    let ordinal = services.document().images.len() + 1;
                    services.document_mut().images.insert(
                        self.number,
                        ImageUse::new(
                            self.number,
                            ordinal,
                            String::new(),
                            Vec::new(),
                            String::new(),
                            (0, 0),
                            (0, 0, 0),
                        ),
                    );
                    self.phase = Phase::Dimensions;
                    ImageStep::Yield(ImageAction::Dimensions)
                }
            }
            Phase::Reference => {
                let number = scan(value)?.integer()? as usize;
                let image = services.document().images.get(&number).ok_or_else(|| {
                    Error::value("Cannot find referenced PDF image")
                        .with_issue(crate::core::recovery::Issue::PdfOperation)
                })?;
                let (width, height, depth) = (image.width, image.height, image.depth);
                services.append_pdf(PdfEffect::RefImage(number), width, height, depth)?;
                self.phase = Phase::Done;
                ImageStep::Complete
            }
            Phase::Dimensions => {
                let Some(ImageValue::Dimensions(dimensions)) = value else {
                    return Err(Error::runtime("Invalid image dimensions result"));
                };
                self.dimensions = dimensions;
                self.keyword(Phase::Attribute, "attr")
            }
            Phase::Attribute => {
                if scan(value)?.flag()? {
                    self.text(Phase::AttributeText)
                } else {
                    self.keyword(Phase::Named, "named")
                }
            }
            Phase::AttributeText => {
                self.attributes = text(value)?;
                self.keyword(Phase::Named, "named")
            }
            Phase::Named => {
                if scan(value)?.flag()? {
                    self.text(Phase::NamedText)
                } else {
                    self.keyword(Phase::Page, "page")
                }
            }
            Phase::NamedText => {
                self.named = Some(text(value)?);
                self.keyword(Phase::Colorspace, "colorspace")
            }
            Phase::Page => {
                if scan(value)?.flag()? {
                    self.scan(Phase::PageNumber, ScanRequest::Int)
                } else {
                    self.keyword(Phase::Colorspace, "colorspace")
                }
            }
            Phase::PageNumber => {
                self.page = scan(value)?.integer()?;
                self.keyword(Phase::Colorspace, "colorspace")
            }
            Phase::Colorspace => {
                if scan(value)?.flag()? {
                    self.scan(Phase::ColorspaceNumber, ScanRequest::Int)
                } else {
                    self.keyword(Phase::PageBox, PAGE_BOXES[0].0)
                }
            }
            Phase::ColorspaceNumber => {
                self.colorspace = scan(value)?.integer()?;
                self.keyword(Phase::PageBox, PAGE_BOXES[0].0)
            }
            Phase::PageBox => {
                if scan(value)?.flag()? {
                    self.pagebox = PAGE_BOXES[self.box_index].0.into();
                    return self.pagebox(services);
                }
                self.box_index += 1;
                if self.box_index == PAGE_BOXES.len() {
                    return self.pagebox(services);
                }
                self.keyword(Phase::PageBox, PAGE_BOXES[self.box_index].0)
            }
            Phase::Filename => {
                let name = text(value)?;
                services.load_image(
                    self.number,
                    name,
                    std::mem::take(&mut self.dimensions),
                    ImageOptions {
                        attributes: std::mem::take(&mut self.attributes),
                        page: self.page,
                        named: self.named.take(),
                        pagebox: std::mem::take(&mut self.pagebox),
                        colorspace: self.colorspace,
                    },
                )?;
                if self.immediate {
                    services.emit_image(self.number)?;
                }
                self.phase = Phase::Done;
                ImageStep::Complete
            }
            Phase::Done => return Err(Error::runtime("Image operands already completed")),
        })
    }
}
