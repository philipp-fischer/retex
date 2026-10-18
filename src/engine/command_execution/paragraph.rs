//! Owned paragraph and vertical transfers, preserving output callback ordering.
use super::*;
use crate::typesetting::model::NodeFields;
use crate::typesetting::{nodes::glue_parameter, paragraph_operations::ParagraphOperations as Par};

pub(in crate::engine) struct ParagraphMaterial {
    pub boxes: Vec<i64>,
    pub penalties: Vec<i64>,
    pub hold: RootHold,
}
#[derive(Clone)]
enum Kind {
    Start { indent: bool, resume: bool },
    Finish { display: bool },
    End,
}
#[derive(Clone)]
enum Stage {
    Start,
    Started,
    Prepared,
    Before,
    Appended,
    After,
    Penalty,
    Finished,
    Consumed,
    Done,
}
pub(in crate::engine) struct ParagraphTask {
    kind: Kind,
    stage: Stage,
    material: Option<ParagraphMaterial>,
    index: usize,
    local_seen: bool,
    after: Vec<i64>,
    #[cfg(feature = "latency-diagnostics")]
    audited: bool,
}
impl ParagraphTask {
    pub(super) fn local_literal_prefix(&self) -> bool {
        matches!(self.kind, Kind::Start { .. }) && self.material.is_none()
    }
    pub(super) fn checkpoint_holds(&self) -> usize {
        usize::from(self.material.is_some())
    }
    pub(super) fn checkpoint_clone(&self, roots: &crate::core::runtime::OwnershipRoots) -> Self {
        Self {
            #[cfg(feature = "latency-diagnostics")]
            audited: self.audited,
            local_seen: self.local_seen,
            kind: self.kind.clone(),
            stage: self.stage.clone(),
            index: self.index,
            after: self.after.clone(),
            material: self.material.as_ref().map(|material| ParagraphMaterial {
                boxes: material.boxes.clone(),
                penalties: material.penalties.clone(),
                hold: roots.hold(material.hold.roots.borrow().iter().copied()),
            }),
        }
    }
    fn new(kind: Kind) -> Self {
        Self {
            #[cfg(feature = "latency-diagnostics")]
            audited: false,
            local_seen: false,
            kind,
            stage: Stage::Start,
            material: None,
            index: 0,
            after: Vec::new(),
        }
    }
    pub fn start(indent: bool, resume: bool) -> Self {
        Self::new(Kind::Start { indent, resume })
    }
    pub fn reused(material: ParagraphMaterial) -> Self {
        let mut task = Self::finish(false);
        task.stage = Stage::Prepared;
        task.material = Some(material);
        task
    }
    pub fn finish(display: bool) -> Self {
        Self::new(Kind::Finish { display })
    }
    pub fn end() -> Self {
        Self::new(Kind::End)
    }
    pub(super) fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<FrameStep> {
        if let Some(value) = returned {
            if !matches!(value?, CommandValue::Executed(true)) {
                return Err(Error::runtime("Invalid paragraph child result"));
            }
        }
        let request = match self.stage {
            Stage::Start => match self.kind {
                Kind::Start { indent, resume } => {
                    if view.builder.context.mode != ListMode::Vertical {
                        return self.complete(true);
                    }
                    self.stage = Stage::Started;
                    if !resume
                        && (!view.builder.context.restricted
                            || view.builder.context.cur_list_head != C::NULL)
                    {
                        let spec = glue_parameter(view.env, "parskip");
                        let pointer = view.builder.new_glue(spec, "parskip", view.fonts.current)?;
                        view.builder.tail_append(pointer)?;
                        CommandRequest::Pages(PageTask::new(true, false))
                    } else {
                        view.finish_start_paragraph(indent, resume)?;
                        return self.complete(true);
                    }
                }
                Kind::End => {
                    self.stage = Stage::Finished;
                    CommandRequest::Paragraph(Self::finish(false))
                }
                Kind::Finish { display } => {
                    self.material = view.prepare_finish_paragraph(display)?;
                    self.stage = Stage::Prepared;
                    return self.resume(view, None);
                }
            },
            Stage::Started => {
                let Kind::Start { indent, resume } = self.kind else {
                    unreachable!()
                };
                view.finish_start_paragraph(indent, resume)?;
                return self.complete(true);
            }
            Stage::Prepared => {
                if !self.local_seen {
                    self.local_seen = true;
                    if let Some(m) = &self.material {
                        view.preview_paragraph(&m.boxes);
                        view.local_paragraph(&m.boxes, &m.penalties);
                    }
                }
                #[cfg(feature = "latency-diagnostics")]
                if !self.audited {
                    self.audited = true;
                    if let Some(material) = &self.material {
                        crate::local_update_audit::paragraph(
                            view.builder,
                            view.env,
                            view.pages,
                            &material.boxes,
                            &material.penalties,
                            view.layout.paragraph_count,
                            view.core
                                .input
                                .input_file
                                .first()
                                .and_then(Option::as_ref)
                                .and_then(|s| s.byte_position())
                                .unwrap_or(0),
                            view.core.reuse.edited_offset,
                        );
                    }
                }
                let Some(material) = &mut self.material else {
                    return self.complete(true);
                };
                if self.index == material.boxes.len() {
                    return self.complete(true);
                }
                let pointer = material.boxes[self.index];
                let (before, after) = view.vertical.extract_adjustments(view.builder, pointer)?;
                // Vertical owns the extracted migrations until SpliceTask
                // transfers each one; the paragraph owns only pending lines.
                self.after = after;
                self.stage = Stage::Before;
                CommandRequest::Splice(SpliceTask::new(view, before))
            }
            Stage::Before => {
                let material = self.material.as_mut().unwrap();
                let pointer = material.boxes[self.index];
                material.hold.roots.borrow_mut().retain(|&r| r != pointer);
                self.stage = Stage::Appended;
                CommandRequest::Append(AppendTask::new(view, pointer, false))
            }
            Stage::Appended => {
                self.stage = Stage::After;
                CommandRequest::Splice(SpliceTask::new(view, std::mem::take(&mut self.after)))
            }
            Stage::After => {
                self.stage = Stage::Penalty;
                if let Some(&penalty) = self
                    .material
                    .as_ref()
                    .unwrap()
                    .penalties
                    .get(self.index)
                    .filter(|&&n| n != 0)
                {
                    let pointer = view
                        .builder
                        .allocate(crate::typesetting::model::Node::from(NodeFields {
                            node_type: crate::core::enums::NodeKind::Penalty,
                            penalty,
                            ..Default::default()
                        }))?;
                    view.builder.tail_append(pointer)?;
                    CommandRequest::Pages(PageTask::new(true, false))
                } else {
                    return self.resume(view, None);
                }
            }
            Stage::Penalty => {
                self.index += 1;
                self.stage = Stage::Prepared;
                return self.resume(view, None);
            }
            Stage::Finished => {
                self.stage = Stage::Consumed;
                CommandRequest::Pages(PageTask::new(true, false))
            }
            Stage::Consumed => {
                self.stage = Stage::Done;
                CommandRequest::Pages(PageTask::new(false, true))
            }
            Stage::Done => return self.complete(false),
        };
        Ok(FrameStep::Yield(request))
    }
    fn complete(&mut self, success: bool) -> Result<FrameStep> {
        self.material = None;
        Ok(FrameStep::Complete(CommandValue::Executed(success)))
    }
}

pub(in crate::engine) struct AppendTask {
    pointer: i64,
    adjusted: bool,
    stage: u8,
    after: Vec<i64>,
    _hold: RootHold,
}
impl AppendTask {
    pub(super) fn checkpoint_clone(&self, roots: &crate::core::runtime::OwnershipRoots) -> Self {
        Self {
            pointer: self.pointer,
            adjusted: self.adjusted,
            stage: self.stage,
            after: self.after.clone(),
            _hold: roots.hold(self._hold.roots.borrow().iter().copied()),
        }
    }
    pub fn new(view: &EngineView<'_>, pointer: i64, adjusted: bool) -> Self {
        Self {
            pointer,
            adjusted,
            stage: 0,
            after: Vec::new(),
            _hold: view.builder.ownership.hold([pointer]),
        }
    }
    pub(super) fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<FrameStep> {
        if let Some(value) = returned {
            if !matches!(value?, CommandValue::Executed(true)) {
                return Err(Error::runtime("Invalid append child result"));
            }
        }
        match self.stage {
            0 => {
                self.stage = 1;
                if self.adjusted {
                    let (before, after) = view
                        .vertical
                        .extract_adjustments(view.builder, self.pointer)?;
                    // Extracted migrations remain rooted by Vertical until
                    // their individual transfer into the current list.
                    self.after = after;
                    return Ok(FrameStep::Yield(CommandRequest::Splice(SpliceTask::new(
                        view, before,
                    ))));
                }
                self.resume(view, None)
            }
            1 => {
                view.observe();
                self.stage = 2;
                let consume = view.with_layout(|_, c| Par::prepare_append_box(c, self.pointer))?;
                self._hold
                    .roots
                    .borrow_mut()
                    .retain(|&root| root != self.pointer);
                if consume {
                    Ok(FrameStep::Yield(CommandRequest::Pages(PageTask::new(
                        true, false,
                    ))))
                } else {
                    self.resume(view, None)
                }
            }
            2 => {
                self.stage = 3;
                if self.adjusted {
                    Ok(FrameStep::Yield(CommandRequest::Splice(SpliceTask::new(
                        view,
                        std::mem::take(&mut self.after),
                    ))))
                } else {
                    self.resume(view, None)
                }
            }
            _ => Ok(FrameStep::Complete(CommandValue::Executed(true))),
        }
    }
}
pub(in crate::engine) struct SpliceTask {
    pointers: Vec<i64>,
    index: usize,
    _hold: RootHold,
    started: bool,
}
impl SpliceTask {
    pub(super) fn checkpoint_clone(&self, roots: &crate::core::runtime::OwnershipRoots) -> Self {
        Self {
            pointers: self.pointers.clone(),
            index: self.index,
            started: self.started,
            _hold: roots.hold(self._hold.roots.borrow().iter().copied()),
        }
    }
    pub fn new(view: &EngineView<'_>, pointers: Vec<i64>) -> Self {
        Self {
            _hold: view.builder.ownership.hold(pointers.iter().copied()),
            pointers,
            index: 0,
            started: false,
        }
    }
    pub(super) fn resume(
        &mut self,
        view: &mut EngineView<'_>,
        returned: Option<Result<CommandValue>>,
    ) -> Result<FrameStep> {
        if let Some(value) = returned {
            if !matches!(value?, CommandValue::Executed(true)) {
                return Err(Error::runtime("Invalid splice child result"));
            }
        }
        if !self.started {
            view.observe();
            self.started = true;
        }
        if self.index == self.pointers.len() {
            return Ok(FrameStep::Complete(CommandValue::Executed(true)));
        }
        view.vertical
            .take_migration(view.builder, self.pointers[self.index])?;
        let pointer = self.pointers[self.index];
        self._hold
            .roots
            .borrow_mut()
            .retain(|&root| root != pointer);
        self.index += 1;
        Ok(FrameStep::Yield(CommandRequest::Pages(PageTask::new(
            true, false,
        ))))
    }
}
