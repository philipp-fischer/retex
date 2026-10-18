//! Shared ordered PDF object, paint, metadata and font-expansion operands.
use super::{
    pdf_extensions::ExtensionServices,
    scanning::{ScanRequest, ScanValue},
};
use crate::{
    core::{
        enums::CharacterCategory,
        errors::{Error, Result},
    },
    pdf::document::SourceObject,
    typesetting::model::{ColorAction, PdfEffect},
};
use std::path::Path;

pub(crate) enum PdfOperandAction {
    Scan(ScanRequest),
    Text,
    ExpandedToken,
    NavigationAction,
}
pub(crate) enum PdfOperandValue {
    Scan(ScanValue),
    Text(String),
    Unit,
    Action(crate::pdf::navigation::Action),
}
pub(crate) enum PdfOperandStep {
    Yield(PdfOperandAction),
    Complete,
}
enum Phase {
    Start,
    Text,
    CatalogOpenAction,
    CatalogAction,
    Reference,
    SnapGlue,
    SnapRatio,
    Font,
    Equals,
    Stretch,
    Shrink,
    FontStep,
    Auto,
    AutoSpace,
    ColorNumber,
    ColorAction,
    ColorText,
    Reserve,
    ReserveSpace,
    UseNumber,
    Number,
    Stream,
    Attribute,
    AttributeText,
    File,
    ObjectText,
    Done,
}
pub(crate) struct PdfOperandExecution {
    operation: String,
    immediate: bool,
    phase: Phase,
    number: usize,
    stream: bool,
    file: bool,
    attributes: String,
    color_index: usize,
    color_action: Option<ColorAction>,
    font: usize,
    expansion: [i64; 3],
}
impl PdfOperandExecution {
    pub(crate) fn supports(operation: &str) -> bool {
        matches!(
            operation,
            "pdfobj"
                | "pdfrefobj"
                | "pdfsetmatrix"
                | "pdfcolorstack"
                | "pdfspacefont"
                | "pdffontexpand"
                | "pdfsnapy"
                | "pdfsnapycomp"
                | "pdfinfo"
                | "pdfcatalog"
                | "pdfnames"
                | "pdftrailer"
                | "pdftrailerid"
        )
    }
    pub(crate) fn new(operation: String, immediate: bool) -> Self {
        Self {
            operation,
            immediate,
            phase: Phase::Start,
            number: 0,
            stream: false,
            file: false,
            attributes: String::new(),
            color_index: 0,
            color_action: None,
            font: 0,
            expansion: [0; 3],
        }
    }
    pub(crate) fn drive(mut self, services: &mut impl ExtensionServices) -> Result<()> {
        let mut returned = None;
        loop {
            match self.resume(services, returned.take())? {
                PdfOperandStep::Complete => return Ok(()),
                PdfOperandStep::Yield(action) => {
                    returned = Some(match action {
                        PdfOperandAction::Scan(request) => {
                            services.scan(request).map(PdfOperandValue::Scan)
                        }
                        PdfOperandAction::Text => services.text().map(PdfOperandValue::Text),
                        PdfOperandAction::ExpandedToken => {
                            services.expanded_token().map(|_| PdfOperandValue::Unit)
                        }
                        PdfOperandAction::NavigationAction => {
                            super::pdf_scan_execution::PdfScanExecution::new(
                                super::pdf_scan_execution::PdfScanRequest::Action,
                            )
                            .drive(services)
                            .and_then(|value| match value {
                                super::pdf_scan_execution::PdfScanValue::Action(value) => {
                                    Ok(PdfOperandValue::Action(value))
                                }
                                _ => Err(Error::runtime("Invalid catalog action result")),
                            })
                        }
                    })
                }
            }
        }
    }
    fn scan(&mut self, phase: Phase, request: ScanRequest) -> PdfOperandStep {
        self.phase = phase;
        PdfOperandStep::Yield(PdfOperandAction::Scan(request))
    }
    fn text(&mut self, phase: Phase) -> PdfOperandStep {
        self.phase = phase;
        PdfOperandStep::Yield(PdfOperandAction::Text)
    }
    fn done(&mut self) -> PdfOperandStep {
        self.phase = Phase::Done;
        PdfOperandStep::Complete
    }
    fn color(
        &mut self,
        services: &mut impl ExtensionServices,
        text: String,
    ) -> Result<PdfOperandStep> {
        services.append_pdf(
            PdfEffect::ColorStack {
                number: self.number,
                action: self.color_action.take().unwrap(),
                text,
            },
            0,
            0,
            0,
        )?;
        Ok(self.done())
    }
    fn font(
        &mut self,
        services: &mut impl ExtensionServices,
        automatic: bool,
    ) -> Result<PdfOperandStep> {
        crate::fonts::expansion::configure(
            services.fonts_mut(),
            self.font,
            self.expansion[0],
            self.expansion[1],
            self.expansion[2],
            automatic,
        )?;
        Ok(self.done())
    }
    pub(crate) fn resume(
        &mut self,
        services: &mut impl ExtensionServices,
        returned: Option<Result<PdfOperandValue>>,
    ) -> Result<PdfOperandStep> {
        fn scan(value: Option<PdfOperandValue>) -> Result<ScanValue> {
            match value {
                Some(PdfOperandValue::Scan(value)) => Ok(value),
                _ => Err(Error::runtime("Invalid PDF operand scanner result")),
            }
        }
        fn text(value: Option<PdfOperandValue>) -> Result<String> {
            match value {
                Some(PdfOperandValue::Text(value)) => Ok(value),
                _ => Err(Error::runtime("Invalid PDF operand text result")),
            }
        }
        let returned = returned.transpose()?;
        Ok(match self.phase {
            Phase::Start => {
                if returned.is_some() {
                    return Err(Error::runtime("Unrequested PDF operand result"));
                }
                match self.operation.as_str() {
                    "pdfobj" => {
                        self.scan(Phase::Reserve, ScanRequest::Keyword("reserveobjnum".into()))
                    }
                    "pdfrefobj" => self.scan(Phase::Reference, ScanRequest::Int),
                    "pdfcolorstack" => self.scan(Phase::ColorNumber, ScanRequest::Int),
                    "pdffontexpand" => self.scan(Phase::Font, ScanRequest::FontIdent),
                    "pdfsnapy" => self.scan(Phase::SnapGlue, ScanRequest::Glue { mu: false }),
                    "pdfsnapycomp" => self.scan(Phase::SnapRatio, ScanRequest::Int),
                    "pdfsetmatrix" | "pdfspacefont" | "pdfinfo" | "pdfcatalog" | "pdfnames"
                    | "pdftrailer" | "pdftrailerid" => self.text(Phase::Text),
                    _ => return Err(Error::runtime("Unsupported owned PDF operand")),
                }
            }
            Phase::Text => {
                let text = text(returned)?;
                match self.operation.as_str() {
                    "pdfsetmatrix" => {
                        if services.input().recovery.enabled() {
                            if let Err(error) =
                                crate::pdf::graphics::Affine::around(&text, 0.0, 0.0)
                            {
                                services.input_mut().recover(crate::core::recovery::Issue::PdfOperation,error,"Ignored the invalid matrix; retained the previous graphics transform")?;
                            } else {
                                services.append_pdf(PdfEffect::SetMatrix(text), 0, 0, 0)?;
                            }
                        } else {
                            services.append_pdf(PdfEffect::SetMatrix(text), 0, 0, 0)?;
                        }
                    }
                    "pdfspacefont" => services.document_mut().space_font_name = text,
                    "pdfinfo" => services.document_mut().info.push_str(&text),
                    "pdfcatalog" => {
                        services.document_mut().catalog.push_str(&text);
                        return Ok(self.scan(
                            Phase::CatalogOpenAction,
                            ScanRequest::Keyword("openaction".into()),
                        ));
                    }
                    "pdfnames" => services.document_mut().names.push_str(&text),
                    "pdftrailer" => services.document_mut().trailer.push_str(&text),
                    "pdftrailerid" => services.document_mut().trailer_id.push_str(&text),
                    _ => unreachable!(),
                }
                self.done()
            }
            Phase::CatalogOpenAction => {
                if scan(returned)?.flag()? {
                    if !services.document().open_action.is_empty() {
                        return Err(Error::value("Duplicate PDF catalog openaction"));
                    }
                    let number = services.reserve_pdf_object();
                    services.document_mut().open_action_number = number;
                    self.phase = Phase::CatalogAction;
                    PdfOperandStep::Yield(PdfOperandAction::NavigationAction)
                } else {
                    self.done()
                }
            }
            Phase::CatalogAction => {
                let Some(PdfOperandValue::Action(action)) = returned else {
                    return Err(Error::runtime("Invalid catalog action result"));
                };
                services.check_pdf_version()?;
                let text = services.pdf_action_string(action)?;
                services.document_mut().open_action = text;
                self.done()
            }
            Phase::Reference => {
                let number = scan(returned)?.integer()? as usize;
                if !services.document().objects.contains_key(&number) {
                    return Err(Error::value("Cannot find referenced PDF object")
                        .with_issue(crate::core::recovery::Issue::PdfOperation));
                }
                services.append_pdf(PdfEffect::RefObject(number), 0, 0, 0)?;
                self.done()
            }
            Phase::SnapGlue => {
                let ScanValue::Glue(glue) = scan(returned)? else {
                    return Err(Error::runtime("Invalid PDF snap glue"));
                };
                if glue.width <= 0 {
                    return Err(Error::value("Snap grid spacing must be positive"));
                }
                services.append_pdf(PdfEffect::SnapY(glue), 0, 0, 0)?;
                self.done()
            }
            Phase::SnapRatio => {
                let ratio = scan(returned)?.integer()?.clamp(0, 1000);
                services.append_pdf(PdfEffect::SnapCompensation(ratio), 0, 0, 0)?;
                self.done()
            }
            Phase::Font => {
                let ScanValue::Font(font) = scan(returned)? else {
                    return Err(Error::value("Invalid font scanner result"));
                };
                self.font = font;
                self.scan(Phase::Equals, ScanRequest::OptionalEquals)
            }
            Phase::Equals => {
                scan(returned)?;
                self.scan(Phase::Stretch, ScanRequest::Int)
            }
            Phase::Stretch => {
                self.expansion[0] = scan(returned)?.integer()?;
                self.scan(Phase::Shrink, ScanRequest::Int)
            }
            Phase::Shrink => {
                self.expansion[1] = scan(returned)?.integer()?;
                self.scan(Phase::FontStep, ScanRequest::Int)
            }
            Phase::FontStep => {
                self.expansion[2] = scan(returned)?.integer()?;
                self.scan(Phase::Auto, ScanRequest::Keyword("autoexpand".into()))
            }
            Phase::Auto => {
                if scan(returned)?.flag()? {
                    self.phase = Phase::AutoSpace;
                    PdfOperandStep::Yield(PdfOperandAction::ExpandedToken)
                } else {
                    return self.font(services, false);
                }
            }
            Phase::AutoSpace | Phase::ReserveSpace => {
                if !matches!(returned, Some(PdfOperandValue::Unit)) {
                    return Err(Error::runtime("Invalid PDF optional space result"));
                }
                if services.input().current.category() != Some(CharacterCategory::Spacer) {
                    services.back_input()?;
                }
                if matches!(self.phase, Phase::AutoSpace) {
                    return self.font(services, true);
                }
                if self.immediate {
                    return Err(Error::value(
                        "pdfobj reserveobjnum cannot be used with immediate",
                    ));
                }
                self.done()
            }
            Phase::ColorNumber => {
                let number = scan(returned)?.integer()?;
                if number < 0 || number as usize >= services.document().colors.len() {
                    return Err(Error::value("PDF color stack has not been initialized"));
                }
                self.number = number as usize;
                self.scan(Phase::ColorAction, ScanRequest::Keyword("set".into()))
            }
            Phase::ColorAction => {
                if scan(returned)?.flag()? {
                    self.color_action = Some(match self.color_index {
                        0 => ColorAction::Set,
                        1 => ColorAction::Push,
                        2 => ColorAction::Pop,
                        _ => ColorAction::Current,
                    });
                    if self.color_index < 2 {
                        self.text(Phase::ColorText)
                    } else {
                        return self.color(services, String::new());
                    }
                } else {
                    self.color_index += 1;
                    let name = match self.color_index {
                        1 => "push",
                        2 => "pop",
                        3 => "current",
                        _ => return Err(Error::value("PDF color stack action missing")),
                    };
                    self.scan(Phase::ColorAction, ScanRequest::Keyword(name.into()))
                }
            }
            Phase::ColorText => return self.color(services, text(returned)?),
            Phase::Reserve => {
                if scan(returned)?.flag()? {
                    self.number = services.reserve_source_object(None)?;
                    services.document_mut().last_object = self.number;
                    self.phase = Phase::ReserveSpace;
                    PdfOperandStep::Yield(PdfOperandAction::ExpandedToken)
                } else {
                    self.scan(Phase::UseNumber, ScanRequest::Keyword("useobjnum".into()))
                }
            }
            Phase::UseNumber => {
                if scan(returned)?.flag()? {
                    self.scan(Phase::Number, ScanRequest::Int)
                } else {
                    self.number = services.reserve_source_object(None)?;
                    self.scan(Phase::Stream, ScanRequest::Keyword("stream".into()))
                }
            }
            Phase::Number => {
                let number = scan(returned)?.integer()? as usize;
                self.number = services.reserve_source_object(Some(number))?;
                self.scan(Phase::Stream, ScanRequest::Keyword("stream".into()))
            }
            Phase::Stream => {
                self.stream = scan(returned)?.flag()?;
                if self.stream {
                    self.scan(Phase::Attribute, ScanRequest::Keyword("attr".into()))
                } else {
                    self.scan(Phase::File, ScanRequest::Keyword("file".into()))
                }
            }
            Phase::Attribute => {
                if scan(returned)?.flag()? {
                    self.text(Phase::AttributeText)
                } else {
                    self.scan(Phase::File, ScanRequest::Keyword("file".into()))
                }
            }
            Phase::AttributeText => {
                self.attributes = text(returned)?;
                self.scan(Phase::File, ScanRequest::Keyword("file".into()))
            }
            Phase::File => {
                self.file = scan(returned)?.flag()?;
                self.text(Phase::ObjectText)
            }
            Phase::ObjectText => {
                let text = text(returned)?;
                let directory = services
                    .input()
                    .cursor
                    .name
                    .as_deref()
                    .and_then(|name| Path::new(name).parent())
                    .unwrap_or(Path::new("."))
                    .to_string_lossy()
                    .into_owned();
                services.document_mut().objects.insert(
                    self.number,
                    SourceObject {
                        text: Some(text),
                        stream: self.stream,
                        attributes: std::mem::take(&mut self.attributes),
                        directory,
                        file: self.file,
                        ..SourceObject::default()
                    },
                );
                services.document_mut().last_object = self.number;
                if self.immediate {
                    services.emit_source_object(self.number)?;
                }
                self.done()
            }
            Phase::Done => return Err(Error::runtime("PDF operand already completed")),
        })
    }
}
