//! Shared owned PDF identifiers, dimensions and navigation actions.
use super::{
    scanning::{ScanRequest, ScanValue},
    services::ListServices,
};
use crate::{
    core::{
        enums::CharacterCategory,
        errors::{Error, Result},
    },
    pdf::navigation::{Action, ActionTarget, Dimensions, Identifier},
};

pub(crate) enum PdfScanRequest {
    Identifier,
    Dimensions,
    Action,
}
pub(crate) enum PdfScanAction {
    Scan(ScanRequest),
    Text,
    ExpandedToken,
    Child(PdfScanRequest),
}
pub(crate) enum PdfScanValue {
    Unit,
    Scan(ScanValue),
    Text(String),
    Identifier(Identifier),
    Dimensions(Dimensions),
    Action(Action),
}
pub(crate) enum PdfScanStep {
    Yield(PdfScanAction),
    Complete(PdfScanValue),
}
enum Phase {
    Start,
    Name,
    NameText,
    Number,
    NumberValue,
    DimensionKeyword,
    DimensionValue,
    User,
    UserText,
    Goto,
    Thread,
    File,
    FileText,
    Structure,
    StructureText,
    StructureIdentifier,
    Page,
    PageNumber,
    PageText,
    DestinationIdentifier,
    NewWindow,
    NoNewWindow,
    WindowSpace,
    Done,
}
pub(crate) struct PdfScanExecution {
    request: PdfScanRequest,
    phase: Phase,
    index: usize,
    dimensions: Dimensions,
    action: Option<Action>,
    page: i64,
}
impl PdfScanExecution {
    pub(crate) fn new(request: PdfScanRequest) -> Self {
        Self {
            request,
            phase: Phase::Start,
            index: 0,
            dimensions: Dimensions::new(),
            action: Some(Action {
                kind: String::new(),
                destination: None,
                file: None,
                window: None,
                structure: None,
                text: String::new(),
            }),
            page: 0,
        }
    }
    pub(crate) fn drive(mut self, services: &mut impl ListServices) -> Result<PdfScanValue> {
        let mut returned = None;
        loop {
            match self.resume(services, returned.take())? {
                PdfScanStep::Complete(value) => return Ok(value),
                PdfScanStep::Yield(action) => {
                    returned = Some(match action {
                        PdfScanAction::Scan(request) => {
                            services.scan(request).map(PdfScanValue::Scan)
                        }
                        PdfScanAction::Text => services.text().map(PdfScanValue::Text),
                        PdfScanAction::ExpandedToken => {
                            services.expanded_token().map(|_| PdfScanValue::Unit)
                        }
                        PdfScanAction::Child(request) => Self::new(request).drive(services),
                    })
                }
            }
        }
    }
    fn scan(&mut self, phase: Phase, request: ScanRequest) -> PdfScanStep {
        self.phase = phase;
        PdfScanStep::Yield(PdfScanAction::Scan(request))
    }
    fn keyword(&mut self, phase: Phase, name: &str) -> PdfScanStep {
        self.scan(phase, ScanRequest::Keyword(name.into()))
    }
    fn text(&mut self, phase: Phase) -> PdfScanStep {
        self.phase = phase;
        PdfScanStep::Yield(PdfScanAction::Text)
    }
    fn child(&mut self, phase: Phase, request: PdfScanRequest) -> PdfScanStep {
        self.phase = phase;
        PdfScanStep::Yield(PdfScanAction::Child(request))
    }
    fn done(&mut self, value: PdfScanValue) -> PdfScanStep {
        self.phase = Phase::Done;
        PdfScanStep::Complete(value)
    }
    fn finish_action(&mut self) -> Result<PdfScanStep> {
        let action = self.action.take().unwrap();
        if action.window.is_some() && (action.file.is_none() || action.kind == "thread") {
            return Err(Error::value(
                "PDF window options require a remote GoTo action",
            ));
        }
        Ok(self.done(PdfScanValue::Action(action)))
    }
    pub(crate) fn resume(
        &mut self,
        services: &mut impl ListServices,
        returned: Option<Result<PdfScanValue>>,
    ) -> Result<PdfScanStep> {
        fn scan(value: Option<PdfScanValue>) -> Result<ScanValue> {
            match value {
                Some(PdfScanValue::Scan(value)) => Ok(value),
                _ => Err(Error::runtime("Invalid PDF structure scanner result")),
            }
        }
        fn text(value: Option<PdfScanValue>) -> Result<String> {
            match value {
                Some(PdfScanValue::Text(value)) => Ok(value),
                _ => Err(Error::runtime("Invalid PDF structure text result")),
            }
        }
        fn identifier(value: Option<PdfScanValue>) -> Result<Identifier> {
            match value {
                Some(PdfScanValue::Identifier(value)) => Ok(value),
                _ => Err(Error::runtime("Invalid PDF identifier result")),
            }
        }
        let value = returned.transpose()?;
        Ok(match self.phase {
            Phase::Start => {
                if value.is_some() {
                    return Err(Error::runtime("Unrequested PDF structure result"));
                }
                match self.request {
                    PdfScanRequest::Identifier => self.keyword(Phase::Name, "name"),
                    PdfScanRequest::Dimensions => self.keyword(Phase::DimensionKeyword, "width"),
                    PdfScanRequest::Action => self.keyword(Phase::User, "user"),
                }
            }
            Phase::Name => {
                if scan(value)?.flag()? {
                    self.text(Phase::NameText)
                } else {
                    self.keyword(Phase::Number, "num")
                }
            }
            Phase::NameText => self.done(PdfScanValue::Identifier(Identifier::Name(text(value)?))),
            Phase::Number => {
                if scan(value)?.flag()? {
                    self.scan(Phase::NumberValue, ScanRequest::Int)
                } else {
                    return Err(Error::value("PDF identifier requires name or num"));
                }
            }
            Phase::NumberValue => {
                let number = scan(value)?.integer()?;
                if number <= 0 {
                    return Err(Error::value("PDF numeric identifier must be positive")
                        .with_issue(crate::core::recovery::Issue::PdfOperation));
                }
                self.done(PdfScanValue::Identifier(Identifier::Number(number)))
            }
            Phase::DimensionKeyword => {
                if scan(value)?.flag()? {
                    self.scan(
                        Phase::DimensionValue,
                        ScanRequest::Dimen {
                            mu: false,
                            infinite: false,
                        },
                    )
                } else {
                    self.index += 1;
                    if self.index == 3 {
                        let dimensions = std::mem::take(&mut self.dimensions);
                        return Ok(self.done(PdfScanValue::Dimensions(dimensions)));
                    }
                    self.keyword(
                        Phase::DimensionKeyword,
                        ["width", "height", "depth"][self.index],
                    )
                }
            }
            Phase::DimensionValue => {
                self.dimensions.insert(
                    ["width", "height", "depth"][self.index].into(),
                    scan(value)?.integer()?,
                );
                self.index = 0;
                self.keyword(Phase::DimensionKeyword, "width")
            }
            Phase::User => {
                if scan(value)?.flag()? {
                    self.text(Phase::UserText)
                } else {
                    self.keyword(Phase::Goto, "goto")
                }
            }
            Phase::UserText => {
                let action = self.action.as_mut().unwrap();
                action.kind = "user".into();
                action.text = text(value)?;
                return self.finish_action();
            }
            Phase::Goto => {
                if scan(value)?.flag()? {
                    self.action.as_mut().unwrap().kind = "goto".into();
                    self.keyword(Phase::File, "file")
                } else {
                    self.keyword(Phase::Thread, "thread")
                }
            }
            Phase::Thread => {
                if !scan(value)?.flag()? {
                    return Err(Error::value("PDF action type missing")
                        .with_issue(crate::core::recovery::Issue::PdfOperation));
                }
                self.action.as_mut().unwrap().kind = "thread".into();
                self.keyword(Phase::File, "file")
            }
            Phase::File => {
                if scan(value)?.flag()? {
                    self.text(Phase::FileText)
                } else {
                    self.keyword(Phase::Structure, "struct")
                }
            }
            Phase::FileText => {
                self.action.as_mut().unwrap().file = Some(text(value)?);
                self.keyword(Phase::Structure, "struct")
            }
            Phase::Structure => {
                if scan(value)?.flag()? {
                    let action = self.action.as_ref().unwrap();
                    if action.kind != "goto" {
                        return Err(Error::value(
                            "Only GoTo actions accept structure destinations",
                        ));
                    }
                    if action.file.is_some() {
                        self.text(Phase::StructureText)
                    } else {
                        self.child(Phase::StructureIdentifier, PdfScanRequest::Identifier)
                    }
                } else {
                    self.keyword(Phase::Page, "page")
                }
            }
            Phase::StructureText => {
                self.action.as_mut().unwrap().structure = Some(Identifier::Name(text(value)?));
                self.keyword(Phase::Page, "page")
            }
            Phase::StructureIdentifier => {
                self.action.as_mut().unwrap().structure = Some(identifier(value)?);
                self.keyword(Phase::Page, "page")
            }
            Phase::Page => {
                if scan(value)?.flag()? {
                    if self.action.as_ref().unwrap().kind != "goto" {
                        return Err(Error::value("Only GoTo actions accept page destinations"));
                    }
                    self.scan(Phase::PageNumber, ScanRequest::Int)
                } else {
                    self.child(Phase::DestinationIdentifier, PdfScanRequest::Identifier)
                }
            }
            Phase::PageNumber => {
                self.page = scan(value)?.integer()?;
                if self.page <= 0 {
                    return Err(Error::value("PDF action page number must be positive"));
                }
                self.action.as_mut().unwrap().kind = "page".into();
                self.text(Phase::PageText)
            }
            Phase::PageText => {
                self.action.as_mut().unwrap().destination =
                    Some(ActionTarget::Page(self.page, text(value)?));
                self.keyword(Phase::NewWindow, "newwindow")
            }
            Phase::DestinationIdentifier => {
                let id = identifier(value)?;
                let action = self.action.as_mut().unwrap();
                if action.kind == "goto"
                    && action.file.is_some()
                    && matches!(id, Identifier::Number(_))
                {
                    return Err(Error::value(
                        "Remote GoTo actions require a named or page destination",
                    ));
                }
                action.destination = Some(ActionTarget::Identifier(id));
                self.keyword(Phase::NewWindow, "newwindow")
            }
            Phase::NewWindow | Phase::NoNewWindow => {
                if scan(value)?.flag()? {
                    let window = matches!(self.phase, Phase::NewWindow);
                    self.action.as_mut().unwrap().window = Some(window);
                    self.phase = Phase::WindowSpace;
                    PdfScanStep::Yield(PdfScanAction::ExpandedToken)
                } else if matches!(self.phase, Phase::NewWindow) {
                    self.keyword(Phase::NoNewWindow, "nonewwindow")
                } else {
                    return self.finish_action();
                }
            }
            Phase::WindowSpace => {
                if !matches!(value, Some(PdfScanValue::Unit)) {
                    return Err(Error::runtime("Invalid PDF window space result"));
                }
                if services.input().current.category() != Some(CharacterCategory::Spacer) {
                    services.back_input()?;
                }
                return self.finish_action();
            }
            Phase::Done => return Err(Error::runtime("PDF structure already completed")),
        })
    }
}
