//! Owned discretionary and accent scanning over the original box semantics.
use super::box_commands::{AccentMaterial, BoxCommands, BoxServices};
use crate::{
    core::{
        enums::{CharacterCategory as Cat, Command, ListMode, NodeKind, UnpackOperation},
        errors::{Error, Result},
    },
    language::scanning::{ScanRequest, ScanValue},
};

pub(crate) enum DiscretionaryStep {
    ReadPart,
    Complete(i64),
}
pub(crate) struct DiscretionaryExecution {
    shorthand: bool,
    pointer: Option<i64>,
    part: u8,
}
impl DiscretionaryExecution {
    pub fn new(shorthand: bool) -> Self {
        Self {
            shorthand,
            pointer: None,
            part: 0,
        }
    }
    pub fn resume(
        &mut self,
        services: &mut impl BoxServices,
        returned: Option<Result<i64>>,
    ) -> Result<DiscretionaryStep> {
        if self.pointer.is_none() {
            services.start_paragraph(false)?;
            self.pointer = Some(BoxCommands::begin_discretionary(services, self.shorthand)?);
            if self.shorthand {
                return Ok(DiscretionaryStep::Complete(self.pointer.unwrap()));
            }
        } else {
            let head = returned.ok_or_else(|| Error::runtime("Missing discretionary part"))??;
            let node = services.builder_mut().node_mut(self.pointer.unwrap())?;
            match self.part {
                0 => node.pre_break = head,
                1 => node.post_break = head,
                2 => node.list_ptr = head,
                _ => return Err(Error::runtime("Invalid discretionary stage")),
            }
            self.part += 1;
            if self.part == 3 {
                BoxCommands::finish_discretionary(services, self.pointer.unwrap())?;
                return Ok(DiscretionaryStep::Complete(self.pointer.unwrap()));
            }
        }
        Ok(DiscretionaryStep::ReadPart)
    }
}

pub(crate) enum AccentAction {
    Scan(ScanRequest),
    ExecuteToken,
}
pub(crate) enum AccentValue {
    Scan(ScanValue),
    Executed,
}
pub(crate) enum AccentStep {
    Yield(AccentAction),
    Complete,
}
pub(crate) struct AccentExecution {
    stage: u8,
    material: Option<AccentMaterial>,
    allowed: Option<bool>,
}
impl AccentExecution {
    pub fn new() -> Self {
        Self {
            stage: 0,
            material: None,
            allowed: None,
        }
    }
    pub fn resume(
        &mut self,
        services: &mut impl BoxServices,
        returned: Option<Result<AccentValue>>,
    ) -> Result<AccentStep> {
        let result = self.resume_inner(services, returned);
        if result.is_err() {
            self.restore_allowed(services);
        }
        result
    }
    fn restore_allowed(&mut self, services: &mut impl BoxServices) {
        if let Some(allowed) = self.allowed.take() {
            services.set_setbox_allowed(allowed);
        }
    }
    fn resume_inner(
        &mut self,
        services: &mut impl BoxServices,
        returned: Option<Result<AccentValue>>,
    ) -> Result<AccentStep> {
        let returned = returned.transpose()?;
        match self.stage {
            0 => {
                services.start_paragraph(false)?;
                self.stage = 1;
                return Ok(AccentStep::Yield(AccentAction::Scan(ScanRequest::CharNum)));
            }
            1 => {
                self.material =
                    BoxCommands::prepare_accent(services, scan(returned)?.integer()? as u32)?;
                if self.material.is_none() {
                    return Ok(AccentStep::Complete);
                }
                self.stage = 2;
                return Ok(AccentStep::Yield(AccentAction::Scan(ScanRequest::Nonblank)));
            }
            2 => {
                scan(returned)?;
                self.allowed = Some(services.setbox_allowed());
                services.set_setbox_allowed(false);
            }
            3 => {
                scan(returned)?;
            }
            4 => {
                if !matches!(returned, Some(AccentValue::Executed)) {
                    return Err(Error::runtime("Invalid accent command result"));
                }
                self.stage = 3;
                return Ok(AccentStep::Yield(AccentAction::Scan(ScanRequest::Nonblank)));
            }
            5 => {
                let code = scan(returned)?.integer()? as u32;
                BoxCommands::finish_accent(services, self.material.take().unwrap(), Some(code))?;
                return Ok(AccentStep::Complete);
            }
            _ => return Err(Error::runtime("Invalid accent stage")),
        }
        let current = &services.input().current;
        if current.is_command(Command::Relax) && current.character() == Some(0) {
            self.stage = 3;
            return Ok(AccentStep::Yield(AccentAction::Scan(ScanRequest::Nonblank)));
        }
        if current.operation().is_some_and(|command| {
            matches!(
                command,
                Command::Def
                    | Command::Let
                    | Command::SetBox
                    | Command::Register
                    | Command::RegAlias
                    | Command::Parameter
                    | Command::Shorthand
                    | Command::CodeAssign
                    | Command::FontDimen
                    | Command::SpaceFactor
                    | Command::PrevDepth
                    | Command::Arithmetic
                    | Command::BoxDimen
                    | Command::DefFont
                    | Command::SetFont
                    | Command::FontInt
                    | Command::FontCode
                    | Command::Global
                    | Command::LongPrefix
                    | Command::OuterPrefix
                    | Command::HyphData
            )
        }) {
            self.stage = 4;
            return Ok(AccentStep::Yield(AccentAction::ExecuteToken));
        }
        self.restore_allowed(services);
        let current = &services.input().current;
        let base = if current.is_category(Cat::Letter)
            || current.is_category(Cat::OtherChar)
            || current.is_command(Command::CharGiven)
        {
            Some(current.character().unwrap() as u32)
        } else if current.is_command(Command::CharNum) {
            self.stage = 5;
            return Ok(AccentStep::Yield(AccentAction::Scan(ScanRequest::CharNum)));
        } else {
            services.back_input()?;
            None
        };
        BoxCommands::finish_accent(services, self.material.take().unwrap(), base)?;
        Ok(AccentStep::Complete)
    }
}
fn scan(value: Option<AccentValue>) -> Result<ScanValue> {
    if let Some(AccentValue::Scan(value)) = value {
        Ok(value)
    } else {
        Err(Error::runtime("Invalid accent scanner result"))
    }
}

pub(crate) enum UnpackAction {
    Scan(ScanRequest),
    StartParagraph,
    FinishParagraph,
}
pub(crate) enum UnpackStep {
    Yield(UnpackAction),
    Complete,
}
pub(crate) struct UnpackExecution {
    operation: UnpackOperation,
    stage: u8,
    register: i64,
}
impl UnpackExecution {
    pub fn new(operation: UnpackOperation) -> Self {
        Self {
            operation,
            stage: 0,
            register: 0,
        }
    }
    pub fn resume(
        &mut self,
        services: &mut impl BoxServices,
        returned: Option<Result<ScanValue>>,
    ) -> Result<UnpackStep> {
        #[cfg(feature = "latency-diagnostics")]
        let _timing = crate::output_diagnostics::operation("box_unpack_resume");
        use crate::core::constants::Constants as C;
        let returned = returned.transpose()?;
        match self.stage {
            0 => {
                if self.operation.discard_source().is_some() {
                    self.stage = 1;
                    return Ok(UnpackStep::Yield(UnpackAction::FinishParagraph));
                }
                BoxCommands::flush(services)?;
                self.stage = 2;
                return Ok(UnpackStep::Yield(UnpackAction::Scan(
                    ScanRequest::Register(None),
                )));
            }
            1 => {
                unit(returned)?;
                if services.builder().context.mode == ListMode::Horizontal
                    && !services.builder().context.restricted
                {
                    services.handle_par()?;
                }
                if services.builder().context.mode != ListMode::Vertical {
                    return Err(Error::value("Vertical unpacking in horizontal box")
                        .with_issue(crate::core::recovery::Issue::BoxOperand));
                }
                let head = services.take_discards(self.operation.discard_source() == Some("split"));
                services.builder_mut().append_list(head)?;
                return Ok(UnpackStep::Complete);
            }
            2 => {
                self.register = returned
                    .ok_or_else(|| Error::runtime("Missing unpack register"))?
                    .integer()?;
                let pointer = services
                    .env()
                    .box_registers
                    .get(&self.register)
                    .copied()
                    .unwrap_or(C::NULL);
                if pointer == C::NULL {
                    return Ok(UnpackStep::Complete);
                }
                let expected = if self.operation.is_horizontal() {
                    NodeKind::Hlist
                } else {
                    NodeKind::Vlist
                };
                if services.builder().node(pointer)?.node_type != expected {
                    return Err(Error::value(format!(
                        "Wrong box type for {}",
                        self.operation.as_str()
                    )));
                }
                self.stage = 3;
                return Ok(UnpackStep::Yield(if self.operation.is_horizontal() {
                    UnpackAction::StartParagraph
                } else {
                    UnpackAction::FinishParagraph
                }));
            }
            3 => {
                unit(returned)?;
                if !self.operation.is_horizontal()
                    && services.builder().context.mode != ListMode::Vertical
                {
                    return Err(Error::value("Vertical unpacking in horizontal box")
                        .with_issue(crate::core::recovery::Issue::BoxOperand));
                }
                // Output callbacks during paragraph finishing can change this
                // register; take its contents at the original semantic point.
                #[cfg(feature = "latency-diagnostics")]
                if crate::output_diagnostics::detailed() {
                    crate::output_diagnostics::event(
                        "unpack",
                        format!("register={}|operation={:?}", self.register, self.operation),
                    );
                }
                let consumed =
                    services.take_register_box(self.register, self.operation.copies_contents())?;
                #[cfg(feature = "latency-diagnostics")]
                crate::local_update_audit::consumer(if self.operation.is_horizontal() {
                    "unpack_horizontal"
                } else {
                    "unpack_vertical"
                });
                let head = services.builder().node(consumed)?.list_ptr;
                services.builder_mut().node_mut(consumed)?.list_ptr = C::NULL;
                services.builder_mut().release(consumed, false)?;
                services.builder_mut().append_list(head)?;
                return Ok(UnpackStep::Complete);
            }
            _ => return Err(Error::runtime("Invalid unpack stage")),
        }
    }
}
fn unit(value: Option<ScanValue>) -> Result<()> {
    if matches!(value, Some(ScanValue::Unit)) {
        Ok(())
    } else {
        Err(Error::runtime("Invalid unpack paragraph result"))
    }
}
