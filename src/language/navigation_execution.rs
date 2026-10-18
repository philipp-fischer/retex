//! Shared owned source navigation commands and their typed operand children.
use super::{
    pdf_navigation::{NavigationCommandServices, VIEWS},
    pdf_scan_execution::{PdfScanExecution, PdfScanRequest, PdfScanValue},
    scanning::{ScanRequest, ScanValue},
};
use crate::{
    core::{
        enums::{CharacterCategory, ListMode},
        errors::{Error, Result},
    },
    pdf::navigation::{
        Action, AnnotationTemplate, DestinationEffect, Dimensions, Identifier, ThreadTemplate,
    },
    typesetting::model::PdfEffect,
};

pub(crate) enum NavigationAction {
    Scan(ScanRequest),
    Text,
    ExpandedToken,
    Child(PdfScanRequest),
}
pub(crate) enum NavigationValue {
    Unit,
    Scan(ScanValue),
    Text(String),
    Child(PdfScanValue),
}
pub(crate) enum NavigationStep {
    Yield(NavigationAction),
    Complete,
}
enum Phase {
    Start,
    OutlineAttribute,
    OutlineAttributeText,
    OutlineAction,
    OutlineCount,
    OutlineCountValue,
    OutlineTitle,
    DestinationStructure,
    DestinationStructureValue,
    DestinationIdentifier,
    DestinationView,
    DestinationZoom,
    DestinationZoomValue,
    DestinationSpace,
    DestinationDimensions,
    AnnotationReserve,
    AnnotationReserveSpace,
    AnnotationUse,
    AnnotationNumber,
    AnnotationDimensions,
    AnnotationText,
    LinkAttribute,
    LinkAttributeText,
    LinkAction,
    ThreadDimensions,
    ThreadAttribute,
    ThreadAttributeText,
    ThreadIdentifier,
    Done,
}
pub(crate) struct NavigationExecution {
    operation: String,
    phase: Phase,
    attributes: String,
    dimensions: Dimensions,
    identifier: Option<Identifier>,
    action: Option<Action>,
    number: usize,
    count: i64,
    structured: usize,
    view_index: usize,
    view: String,
    zoom: Option<i64>,
}
impl NavigationExecution {
    pub(crate) fn new(operation: String) -> Self {
        Self {
            operation,
            phase: Phase::Start,
            attributes: String::new(),
            dimensions: Dimensions::new(),
            identifier: None,
            action: None,
            number: 0,
            count: 0,
            structured: 0,
            view_index: 0,
            view: String::new(),
            zoom: None,
        }
    }
    pub(crate) fn drive(mut self, services: &mut impl NavigationCommandServices) -> Result<()> {
        let mut returned = None;
        loop {
            match self.resume(services, returned.take())? {
                NavigationStep::Complete => return Ok(()),
                NavigationStep::Yield(action) => {
                    returned = Some(match action {
                        NavigationAction::Scan(request) => {
                            services.scan(request).map(NavigationValue::Scan)
                        }
                        NavigationAction::Text => services.text().map(NavigationValue::Text),
                        NavigationAction::ExpandedToken => {
                            services.expanded_token().map(|_| NavigationValue::Unit)
                        }
                        NavigationAction::Child(request) => PdfScanExecution::new(request)
                            .drive(services)
                            .map(NavigationValue::Child),
                    })
                }
            }
        }
    }
    fn scan(&mut self, phase: Phase, request: ScanRequest) -> NavigationStep {
        self.phase = phase;
        NavigationStep::Yield(NavigationAction::Scan(request))
    }
    fn keyword(&mut self, phase: Phase, name: &str) -> NavigationStep {
        self.scan(phase, ScanRequest::Keyword(name.into()))
    }
    fn text(&mut self, phase: Phase) -> NavigationStep {
        self.phase = phase;
        NavigationStep::Yield(NavigationAction::Text)
    }
    fn child(&mut self, phase: Phase, request: PdfScanRequest) -> NavigationStep {
        self.phase = phase;
        NavigationStep::Yield(NavigationAction::Child(request))
    }
    fn space(&mut self, phase: Phase) -> NavigationStep {
        self.phase = phase;
        NavigationStep::Yield(NavigationAction::ExpandedToken)
    }
    fn done(&mut self) -> NavigationStep {
        self.phase = Phase::Done;
        NavigationStep::Complete
    }
    fn annotation(
        &mut self,
        services: &mut impl NavigationCommandServices,
    ) -> Result<NavigationStep> {
        if self.operation == "pdfstartlink" && services.builder().context.mode == ListMode::Vertical
        {
            return Err(Error::value(
                "PDF link start cannot be used in vertical mode",
            ));
        }
        if self.number == 0 {
            self.number = services.reserve_annotation();
        }
        Ok(self.child(Phase::AnnotationDimensions, PdfScanRequest::Dimensions))
    }
    fn publish_annotation(
        &mut self,
        services: &mut impl NavigationCommandServices,
    ) -> Result<NavigationStep> {
        let link = self.operation == "pdfstartlink";
        services.set_last_annotation(self.number, link);
        services.publish_annotation(AnnotationTemplate {
            number: self.number,
            spec: std::mem::take(&mut self.dimensions),
            attributes: std::mem::take(&mut self.attributes),
            action: self.action.take(),
        });
        services.append_pdf(
            if link {
                PdfEffect::StartLink(self.number)
            } else {
                PdfEffect::Annotation(self.number)
            },
            0,
            0,
            0,
        )?;
        Ok(self.done())
    }
    fn publish_destination(
        &mut self,
        services: &mut impl NavigationCommandServices,
    ) -> Result<NavigationStep> {
        if let Some(number) = services.publish_destination(DestinationEffect {
            identifier: self.identifier.take().unwrap(),
            structured: self.structured,
            view: std::mem::take(&mut self.view),
            zoom: self.zoom,
            spec: std::mem::take(&mut self.dimensions),
        })? {
            services.append_pdf(PdfEffect::Destination(number), 0, 0, 0)?;
        }
        Ok(self.done())
    }
    pub(crate) fn resume(
        &mut self,
        services: &mut impl NavigationCommandServices,
        returned: Option<Result<NavigationValue>>,
    ) -> Result<NavigationStep> {
        fn scan(value: Option<NavigationValue>) -> Result<ScanValue> {
            match value {
                Some(NavigationValue::Scan(value)) => Ok(value),
                _ => Err(Error::runtime("Invalid navigation scanner result")),
            }
        }
        fn text(value: Option<NavigationValue>) -> Result<String> {
            match value {
                Some(NavigationValue::Text(value)) => Ok(value),
                _ => Err(Error::runtime("Invalid navigation text result")),
            }
        }
        fn dimensions(value: Option<NavigationValue>) -> Result<Dimensions> {
            match value {
                Some(NavigationValue::Child(PdfScanValue::Dimensions(value))) => Ok(value),
                _ => Err(Error::runtime("Invalid navigation dimensions result")),
            }
        }
        fn identifier(value: Option<NavigationValue>) -> Result<Identifier> {
            match value {
                Some(NavigationValue::Child(PdfScanValue::Identifier(value))) => Ok(value),
                _ => Err(Error::runtime("Invalid navigation identifier result")),
            }
        }
        fn action(value: Option<NavigationValue>) -> Result<Action> {
            match value {
                Some(NavigationValue::Child(PdfScanValue::Action(value))) => Ok(value),
                _ => Err(Error::runtime("Invalid navigation action result")),
            }
        }
        let value = returned.transpose()?;
        Ok(match self.phase {
            Phase::Start => {
                if value.is_some() {
                    return Err(Error::runtime("Unrequested navigation result"));
                }
                match self.operation.as_str() {
                    "pdfendlink" | "pdfendthread" | "pdfrunninglinkon" | "pdfrunninglinkoff" => {
                        if self.operation == "pdfendlink"
                            && services.builder().context.mode == ListMode::Vertical
                        {
                            return Err(Error::value(
                                "PDF link end cannot be used in vertical mode",
                            ));
                        }
                        let effect = match self.operation.as_str() {
                            "pdfendlink" => PdfEffect::EndLink,
                            "pdfendthread" => PdfEffect::EndThread,
                            "pdfrunninglinkon" => PdfEffect::RunningLinkOn,
                            _ => PdfEffect::RunningLinkOff,
                        };
                        services.append_pdf(effect, 0, 0, 0)?;
                        self.done()
                    }
                    "pdfoutline" => self.keyword(Phase::OutlineAttribute, "attr"),
                    "pdfdest" => self.keyword(Phase::DestinationStructure, "struct"),
                    "pdfannot" => self.keyword(Phase::AnnotationReserve, "reserveobjnum"),
                    "pdfstartlink" => return self.annotation(services),
                    "pdfthread" | "pdfstartthread" => {
                        self.child(Phase::ThreadDimensions, PdfScanRequest::Dimensions)
                    }
                    _ => return Err(Error::value("Unknown PDF navigation operation")),
                }
            }
            Phase::OutlineAttribute => {
                if scan(value)?.flag()? {
                    self.text(Phase::OutlineAttributeText)
                } else {
                    self.child(Phase::OutlineAction, PdfScanRequest::Action)
                }
            }
            Phase::OutlineAttributeText => {
                self.attributes = text(value)?;
                self.child(Phase::OutlineAction, PdfScanRequest::Action)
            }
            Phase::OutlineAction => {
                self.action = Some(action(value)?);
                self.keyword(Phase::OutlineCount, "count")
            }
            Phase::OutlineCount => {
                if scan(value)?.flag()? {
                    self.scan(Phase::OutlineCountValue, ScanRequest::Int)
                } else {
                    self.text(Phase::OutlineTitle)
                }
            }
            Phase::OutlineCountValue => {
                self.count = scan(value)?.integer()?;
                self.text(Phase::OutlineTitle)
            }
            Phase::OutlineTitle => {
                services.outline(
                    self.action.take().unwrap(),
                    text(value)?,
                    self.count,
                    std::mem::take(&mut self.attributes),
                )?;
                self.done()
            }
            Phase::DestinationStructure => {
                if scan(value)?.flag()? {
                    self.scan(Phase::DestinationStructureValue, ScanRequest::Int)
                } else {
                    self.child(Phase::DestinationIdentifier, PdfScanRequest::Identifier)
                }
            }
            Phase::DestinationStructureValue => {
                let number = scan(value)?.integer()?;
                if number <= 0 {
                    return Err(Error::value("PDF structure identifier must be positive"));
                }
                self.structured = number as usize;
                self.child(Phase::DestinationIdentifier, PdfScanRequest::Identifier)
            }
            Phase::DestinationIdentifier => {
                self.identifier = Some(identifier(value)?);
                self.keyword(Phase::DestinationView, VIEWS[0].0)
            }
            Phase::DestinationView => {
                if scan(value)?.flag()? {
                    self.view = VIEWS[self.view_index].1.into();
                    if self.view == "XYZ" {
                        self.keyword(Phase::DestinationZoom, "zoom")
                    } else {
                        self.space(Phase::DestinationSpace)
                    }
                } else {
                    self.view_index += 1;
                    if self.view_index >= VIEWS.len() {
                        return Err(Error::value("PDF destination view missing"));
                    }
                    self.keyword(Phase::DestinationView, VIEWS[self.view_index].0)
                }
            }
            Phase::DestinationZoom => {
                if scan(value)?.flag()? {
                    self.scan(Phase::DestinationZoomValue, ScanRequest::Int)
                } else {
                    self.space(Phase::DestinationSpace)
                }
            }
            Phase::DestinationZoomValue => {
                self.zoom = Some(scan(value)?.integer()?);
                self.space(Phase::DestinationSpace)
            }
            Phase::DestinationSpace | Phase::AnnotationReserveSpace => {
                if !matches!(value, Some(NavigationValue::Unit)) {
                    return Err(Error::runtime("Invalid navigation optional space result"));
                }
                if services.input().current.category() != Some(CharacterCategory::Spacer) {
                    services.back_input()?;
                }
                if matches!(self.phase, Phase::AnnotationReserveSpace) {
                    self.done()
                } else if self.view == "FitR" {
                    self.child(Phase::DestinationDimensions, PdfScanRequest::Dimensions)
                } else {
                    return self.publish_destination(services);
                }
            }
            Phase::DestinationDimensions => {
                self.dimensions = dimensions(value)?;
                return self.publish_destination(services);
            }
            Phase::AnnotationReserve => {
                if scan(value)?.flag()? {
                    self.number = services.reserve_annotation();
                    services.set_last_annotation(self.number, false);
                    self.space(Phase::AnnotationReserveSpace)
                } else {
                    self.keyword(Phase::AnnotationUse, "useobjnum")
                }
            }
            Phase::AnnotationUse => {
                if scan(value)?.flag()? {
                    self.scan(Phase::AnnotationNumber, ScanRequest::Int)
                } else {
                    return self.annotation(services);
                }
            }
            Phase::AnnotationNumber => {
                let number = scan(value)?.integer()?;
                if number <= 0 {
                    return Err(Error::value("Invalid PDF annotation reservation"));
                }
                services.validate_annotation(number as usize)?;
                self.number = number as usize;
                return self.annotation(services);
            }
            Phase::AnnotationDimensions => {
                self.dimensions = dimensions(value)?;
                if self.operation == "pdfstartlink" {
                    self.keyword(Phase::LinkAttribute, "attr")
                } else {
                    self.text(Phase::AnnotationText)
                }
            }
            Phase::AnnotationText => {
                self.attributes = text(value)?;
                return self.publish_annotation(services);
            }
            Phase::LinkAttribute => {
                if scan(value)?.flag()? {
                    self.text(Phase::LinkAttributeText)
                } else {
                    self.child(Phase::LinkAction, PdfScanRequest::Action)
                }
            }
            Phase::LinkAttributeText => {
                self.attributes = text(value)?;
                self.child(Phase::LinkAction, PdfScanRequest::Action)
            }
            Phase::LinkAction => {
                self.action = Some(action(value)?);
                return self.publish_annotation(services);
            }
            Phase::ThreadDimensions => {
                self.dimensions = dimensions(value)?;
                self.keyword(Phase::ThreadAttribute, "attr")
            }
            Phase::ThreadAttribute => {
                if scan(value)?.flag()? {
                    self.text(Phase::ThreadAttributeText)
                } else {
                    self.child(Phase::ThreadIdentifier, PdfScanRequest::Identifier)
                }
            }
            Phase::ThreadAttributeText => {
                self.attributes = text(value)?;
                self.child(Phase::ThreadIdentifier, PdfScanRequest::Identifier)
            }
            Phase::ThreadIdentifier => {
                let number = services.publish_thread(ThreadTemplate {
                    identifier: identifier(value)?,
                    spec: std::mem::take(&mut self.dimensions),
                    attributes: std::mem::take(&mut self.attributes),
                });
                services.append_pdf(
                    if self.operation == "pdfthread" {
                        PdfEffect::Thread(number)
                    } else {
                        PdfEffect::StartThread(number)
                    },
                    0,
                    0,
                    0,
                )?;
                self.done()
            }
            Phase::Done => return Err(Error::runtime("Navigation command already completed")),
        })
    }
}
