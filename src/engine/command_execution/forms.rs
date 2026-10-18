//! Owned form capture and ordered resource flush using shared form/placement IR.
use super::*;
use crate::{
    language::{
        form_execution::{FormAction, FormExecution, FormStep as DefinitionStep, FormValue},
        pdf_extensions::ExtensionServices,
        streams::display_tokens,
    },
    pdf::{
        forms::{Form, FormEmission, FormStep},
        resources::PageUse,
    },
    typesetting::placement::{PlaceLineExecution, PlaceStep},
};

pub(in crate::engine) struct FormDefinitionTask {
    execution: FormExecution,
}
impl FormDefinitionTask {
    pub(in crate::engine) fn new(reference: bool, immediate: bool) -> Self {
        Self {
            execution: FormExecution::new(reference, immediate),
        }
    }
    pub(super) fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<FrameStep> {
        let returned = returned.map(|value| {
            value.and_then(|value| match value {
                CommandValue::Evaluation(Value::Scan(value)) => Ok(FormValue::Scan(value)),
                CommandValue::Evaluation(Value::Expansion(ExpansionValue::Tokens(value))) => {
                    display_tokens(view, &value).map(FormValue::Text)
                }
                CommandValue::Executed(true) => Ok(FormValue::Unit),
                _ => Err(Error::runtime("Invalid form definition child result")),
            })
        });
        match self.execution.resume(view, returned)? {
            DefinitionStep::Complete => Ok(FrameStep::Complete(CommandValue::Executed(true))),
            DefinitionStep::Yield(action) => Ok(FrameStep::Yield(match action {
                FormAction::Scan(request) => eval_scan(request),
                FormAction::Text => {
                    CommandRequest::Evaluation(Request::Expand(ExpansionRequest::GeneralText {
                        expanded: true,
                    }))
                }
                FormAction::Emit(number) => CommandRequest::Form(FormTask::new(number)),
            })),
        }
    }
}

pub(in crate::engine) struct FlushTask {
    objects: Vec<usize>,
    xobjects: Vec<usize>,
    object_index: usize,
    xobject_index: usize,
    awaiting: bool,
}
impl FlushTask {
    pub(super) fn new(page: &PageUse) -> Self {
        Self {
            objects: page.objects.iter().copied().collect(),
            xobjects: page.xobjects.values().copied().collect(),
            object_index: 0,
            xobject_index: 0,
            awaiting: false,
        }
    }
    pub(super) fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<Option<FrameStep>> {
        if self.awaiting {
            self.awaiting = false;
            if !matches!(
                returned.ok_or_else(|| Error::runtime("Missing form resource result"))??,
                CommandValue::Executed(true)
            ) {
                return Err(Error::runtime("Invalid form resource result"));
            }
        } else if returned.is_some() {
            return Err(Error::runtime("Unrequested resource flush result"));
        }
        if self.object_index < self.objects.len() {
            let number = self.objects[self.object_index];
            self.object_index += 1;
            view.emit_source_object(number)?;
            return Ok(None);
        }
        if self.xobject_index < self.xobjects.len() {
            let number = self.xobjects[self.xobject_index];
            self.xobject_index += 1;
            if view.core.backend.document.forms.contains_key(&number) {
                self.awaiting = true;
                return Ok(Some(FrameStep::Yield(CommandRequest::Form(FormTask::new(
                    number,
                )))));
            }
            view.emit_image(number)?;
            return Ok(None);
        }
        Ok(Some(FrameStep::Complete(CommandValue::Executed(true))))
    }
}
pub(in crate::engine) struct FinishPageTask {
    started: bool,
}
impl FinishPageTask {
    pub(super) fn new() -> Self {
        Self { started: false }
    }
    pub(super) fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<FrameStep> {
        if !self.started {
            if returned.is_some() {
                return Err(Error::runtime("Unrequested PDF page result"));
            }
            let page = view
                .core
                .backend
                .document
                .current_page
                .clone()
                .ok_or_else(|| Error::runtime("Missing shipment page"))?;
            self.started = true;
            return Ok(FrameStep::Yield(CommandRequest::Flush(FlushTask::new(
                &page,
            ))));
        }
        if !matches!(
            returned.ok_or_else(|| Error::runtime("Missing PDF resource flush result"))??,
            CommandValue::Executed(true)
        ) {
            return Err(Error::runtime("Invalid PDF resource flush result"));
        }
        view.eval().with_document(|d, r, s| d.finish_page(s, r))?;
        Ok(FrameStep::Complete(CommandValue::Executed(true)))
    }
}

enum Phase {
    Start,
    Place,
    Flushed,
    Done,
}
pub(in crate::engine) struct FormTask {
    number: usize,
    form: Option<Form>,
    emission: FormEmission,
    phase: Phase,
    scope: Option<crate::engine::shipment::FormCaptureScope>,
    placement: Option<PlaceLineExecution>,
    page: Option<PageUse>,
    awaiting: bool,
}
impl FormTask {
    pub(super) fn new(number: usize) -> Self {
        Self {
            number,
            form: None,
            emission: FormEmission::new(),
            phase: Phase::Start,
            scope: None,
            placement: None,
            page: None,
            awaiting: false,
        }
    }
    fn finish(
        &mut self,
        view: &mut EngineView<'_>,
        result: Result<()>,
    ) -> Result<Option<FrameStep>> {
        if let Some(scope) = self.scope.take() {
            scope.restore(&mut view.eval());
        }
        let form = self.form.as_mut().unwrap();
        let result = view
            .eval()
            .form_emission_step(&mut self.emission, form, Some(result));
        view.core
            .backend
            .document
            .forms
            .insert(self.number, self.form.take().unwrap());
        self.phase = Phase::Done;
        match result? {
            FormStep::Complete => Ok(Some(FrameStep::Complete(CommandValue::Executed(true)))),
            FormStep::Capture => Err(Error::runtime("Form captured twice")),
        }
    }
    pub(super) fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<Option<FrameStep>> {
        match self.phase {
            Phase::Start => {
                if returned.is_some() {
                    return Err(Error::runtime("Unrequested form result"));
                }
                let form = view
                    .core
                    .backend
                    .document
                    .forms
                    .get(&self.number)
                    .cloned()
                    .ok_or_else(|| {
                        Error::value("Cannot find referenced PDF form")
                            .with_issue(crate::core::recovery::Issue::PdfOperation)
                    })?;
                if form.emitting {
                    return Err(Error::value("Cycle in PDF form references")
                        .with_issue(crate::core::recovery::Issue::PdfOperation));
                }
                if form.data.is_some() {
                    self.phase = Phase::Done;
                    return Ok(Some(FrameStep::Complete(CommandValue::Executed(true))));
                }
                view.core
                    .backend
                    .document
                    .forms
                    .get_mut(&self.number)
                    .unwrap()
                    .emitting = true;
                self.form = Some(form);
                let form = self.form.as_mut().unwrap();
                match view
                    .eval()
                    .form_emission_step(&mut self.emission, form, None)
                {
                    Err(error) => {
                        view.core
                            .backend
                            .document
                            .forms
                            .insert(self.number, self.form.take().unwrap());
                        self.phase = Phase::Done;
                        return Err(error);
                    }
                    Ok(FormStep::Complete) => {
                        return Err(Error::runtime("Unrequested form emission completion"));
                    }
                    Ok(FormStep::Capture) => {}
                }
                self.scope = Some(view.eval().begin_form_capture(form));
                self.placement = Some(PlaceLineExecution::new(
                    view.layout,
                    form.box_ptr,
                    Some(0),
                    Some(form.height),
                    false,
                    None,
                    None,
                    None,
                ));
                self.phase = Phase::Place;
                Ok(None)
            }
            Phase::Place => {
                let returned = if self.awaiting {
                    self.awaiting = false;
                    Some(
                        returned
                            .ok_or_else(|| Error::runtime("Missing form shipment effect"))?
                            .and_then(|value| match value {
                                CommandValue::ShipmentResult(value) => Ok(value),
                                _ => Err(Error::runtime("Invalid form shipment effect")),
                            }),
                    )
                } else {
                    if returned.is_some() {
                        return Err(Error::runtime("Unexpected form shipment effect"));
                    }
                    None
                };
                // The original form capture suppresses typesetting warnings.
                let mut ignored = Vec::new();
                let result =
                    view.with_pages_buffered(&mut ignored, |_, _, layout, context, services| {
                        let mut adapter =
                            Some(services as &mut dyn crate::typesetting::shipout::ShipoutServices);
                        self.placement.as_mut().unwrap().resume(
                            layout,
                            context,
                            &mut adapter,
                            returned,
                        )
                    });
                match result {
                    Err(error) => self.finish(view, Err(error)),
                    Ok(PlaceStep::Continue) => Ok(None),
                    Ok(PlaceStep::Yield(action)) => {
                        self.awaiting = true;
                        Ok(Some(FrameStep::Yield(CommandRequest::Effect(
                            EffectTask::new(action),
                        ))))
                    }
                    Ok(PlaceStep::Complete(line)) => {
                        self.placement = None;
                        match view
                            .eval()
                            .render_form_capture(self.form.as_mut().unwrap(), line)
                        {
                            Err(error) => self.finish(view, Err(error)),
                            Ok(page) => {
                                let task = FlushTask::new(&page);
                                self.page = Some(page);
                                self.phase = Phase::Flushed;
                                Ok(Some(FrameStep::Yield(CommandRequest::Flush(task))))
                            }
                        }
                    }
                }
            }
            Phase::Flushed => {
                let result = returned
                    .ok_or_else(|| Error::runtime("Missing form resource flush result"))?
                    .and_then(|value| {
                        if matches!(value, CommandValue::Executed(true)) {
                            Ok(())
                        } else {
                            Err(Error::runtime("Invalid form resource flush result"))
                        }
                    });
                if result.is_ok() {
                    view.eval().finish_form_resources(
                        self.form.as_mut().unwrap(),
                        self.page.take().unwrap(),
                    );
                }
                self.finish(view, result)
            }
            Phase::Done => Err(Error::runtime("Form task already completed")),
        }
    }
}
