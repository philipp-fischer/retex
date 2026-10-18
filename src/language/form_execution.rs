//! Shared ordered form definition/reference scanning and optional emission.
use super::{
    pdf_extensions::ExtensionServices,
    scanning::{ScanRequest, ScanValue},
};
use crate::{
    core::{
        constants::Constants as C,
        errors::{Error, Result},
    },
    pdf::forms::Form,
    typesetting::model::PdfEffect,
};

pub(crate) enum FormAction {
    Scan(ScanRequest),
    Text,
    Emit(usize),
}
pub(crate) enum FormValue {
    Scan(ScanValue),
    Text(String),
    Unit,
}
pub(crate) enum FormStep {
    Yield(FormAction),
    Complete,
}
enum Phase {
    Start,
    Attribute,
    AttributeText,
    Resources,
    ResourcesText,
    Register,
    Reference,
    Emitted,
    Done,
}
pub(crate) struct FormExecution {
    reference: bool,
    immediate: bool,
    phase: Phase,
    number: usize,
    attributes: String,
    resources: String,
}
impl FormExecution {
    pub(crate) fn new(reference: bool, immediate: bool) -> Self {
        Self {
            reference,
            immediate,
            phase: Phase::Start,
            number: 0,
            attributes: String::new(),
            resources: String::new(),
        }
    }
    pub(crate) fn drive(mut self, s: &mut impl ExtensionServices) -> Result<()> {
        let mut returned = None;
        loop {
            match self.resume(s, returned.take())? {
                FormStep::Complete => return Ok(()),
                FormStep::Yield(action) => {
                    returned = Some(match action {
                        FormAction::Scan(request) => s.scan(request).map(FormValue::Scan),
                        FormAction::Text => s.text().map(FormValue::Text),
                        FormAction::Emit(number) => s.emit_form(number).map(|_| FormValue::Unit),
                    })
                }
            }
        }
    }
    pub(crate) fn resume(
        &mut self,
        s: &mut impl ExtensionServices,
        returned: Option<Result<FormValue>>,
    ) -> Result<FormStep> {
        fn scan(value: Option<Result<FormValue>>) -> Result<ScanValue> {
            match value.ok_or_else(|| Error::runtime("Missing form scanner result"))?? {
                FormValue::Scan(value) => Ok(value),
                _ => Err(Error::runtime("Invalid form scanner result")),
            }
        }
        fn text(value: Option<Result<FormValue>>) -> Result<String> {
            match value.ok_or_else(|| Error::runtime("Missing form text result"))?? {
                FormValue::Text(value) => Ok(value),
                _ => Err(Error::runtime("Invalid form text result")),
            }
        }
        let request = match self.phase {
            Phase::Start => {
                if returned.is_some() {
                    return Err(Error::runtime("Unrequested form definition result"));
                }
                if self.reference {
                    self.phase = Phase::Reference;
                    FormAction::Scan(ScanRequest::Int)
                } else {
                    self.number = s.reserve_pdf_object();
                    let ordinal = s.document().forms.len() + 1;
                    s.document_mut().forms.insert(
                        self.number,
                        Form::new(self.number, ordinal, C::NULL, 0, 0, 0),
                    );
                    self.phase = Phase::Attribute;
                    FormAction::Scan(ScanRequest::Keyword("attr".into()))
                }
            }
            Phase::Attribute => {
                let ScanValue::Flag(found) = scan(returned)? else {
                    return Err(Error::runtime("Invalid form attribute keyword result"));
                };
                if found {
                    self.phase = Phase::AttributeText;
                    FormAction::Text
                } else {
                    self.phase = Phase::Resources;
                    FormAction::Scan(ScanRequest::Keyword("resources".into()))
                }
            }
            Phase::AttributeText => {
                self.attributes = text(returned)?;
                self.phase = Phase::Resources;
                FormAction::Scan(ScanRequest::Keyword("resources".into()))
            }
            Phase::Resources => {
                let ScanValue::Flag(found) = scan(returned)? else {
                    return Err(Error::runtime("Invalid form resource keyword result"));
                };
                if found {
                    self.phase = Phase::ResourcesText;
                    FormAction::Text
                } else {
                    self.phase = Phase::Register;
                    FormAction::Scan(ScanRequest::Register(None))
                }
            }
            Phase::ResourcesText => {
                self.resources = text(returned)?;
                self.phase = Phase::Register;
                FormAction::Scan(ScanRequest::Register(None))
            }
            Phase::Register => {
                let register = scan(returned)?.integer()?;
                let pointer = s.take_box_register(register)?;
                if pointer == C::NULL {
                    return Err(Error::value("PDF form requires a nonvoid box")
                        .with_issue(crate::core::recovery::Issue::PdfOperation));
                }
                let node = s.builder().node(pointer)?;
                let (width, height, depth) = (node.width, node.height, node.depth);
                let doc = s.document_mut();
                let form = doc.forms.get_mut(&self.number).unwrap();
                form.box_ptr = pointer;
                form.width = width;
                form.height = height;
                form.depth = depth;
                form.attributes = std::mem::take(&mut self.attributes);
                form.extra_resources = std::mem::take(&mut self.resources);
                doc.last_form = self.number;
                if self.immediate {
                    self.phase = Phase::Emitted;
                    FormAction::Emit(self.number)
                } else {
                    self.phase = Phase::Done;
                    return Ok(FormStep::Complete);
                }
            }
            Phase::Reference => {
                let number = scan(returned)?.integer()? as usize;
                let form = s.document().forms.get(&number).ok_or_else(|| {
                    Error::value("Cannot find referenced PDF form")
                        .with_issue(crate::core::recovery::Issue::PdfOperation)
                })?;
                let (width, height, depth) = (form.width, form.height, form.depth);
                s.append_pdf(PdfEffect::RefForm(number), width, height, depth)?;
                self.phase = Phase::Done;
                return Ok(FormStep::Complete);
            }
            Phase::Emitted => {
                if !matches!(
                    returned.ok_or_else(|| Error::runtime("Missing immediate form result"))??,
                    FormValue::Unit
                ) {
                    return Err(Error::runtime("Invalid immediate form result"));
                }
                self.phase = Phase::Done;
                return Ok(FormStep::Complete);
            }
            Phase::Done => return Err(Error::runtime("Form definition already completed")),
        };
        Ok(FormStep::Yield(request))
    }
}
