//! Scoped box constructors, discretionary alternatives and rule/accent syntax.
//! Translated from `typesetting/box_commands.py`.
use super::layout::{Layout, TypesettingContext};
use super::model::{BoxScanContext, Node, NodePayload};
use crate::core::constants::Constants as C;
use crate::core::enums::{BoxOperation, Command, GroupKind, ListMode, NodeKind, UnpackOperation};
use crate::core::errors::{Error, Result};
use crate::core::values::{GlueSpec, round_ratio};
use crate::fonts::microtypography::auto_kern;
use crate::language::character_tokens::Token;
use crate::language::records::{CommandOperand, Quantity, ValueKey};
use crate::language::services::ListServices;
use crate::typesetting::model::NodeFields;

pub trait BoxServices: ListServices {
    fn with_layout<T>(
        &mut self,
        operation: impl FnOnce(&mut Layout, &mut TypesettingContext<'_>) -> Result<T>,
    ) -> Result<T>;
    fn begin_group(&mut self, kind: &str, group: GroupKind) -> Result<()>;
    fn end_group(&mut self, kind: &str, flush: bool) -> Result<()>;
    fn start_paragraph(&mut self, indent: bool) -> Result<()>;
    fn finish_paragraph(&mut self) -> Result<()>;
    fn handle_par(&mut self) -> Result<()>;
    fn insert_par_token(&mut self, threshold: i64) -> Result<bool>;
    fn execute_token(&mut self, in_box: bool) -> Result<bool>;
    fn append_box(&mut self, pointer: i64, adjusted: bool) -> Result<()>;
    fn take_register_box(&mut self, register: i64, copy: bool) -> Result<i64>;
    fn assign_box(&mut self, register: i64, pointer: i64, global: bool) -> Result<()>;
    fn lastbox(&mut self) -> Result<i64>;
    fn split_box(&mut self) -> Result<i64>;
    fn take_discards(&mut self, split: bool) -> i64;
    fn setbox_allowed(&self) -> bool;
    fn set_setbox_allowed(&mut self, allowed: bool);
    fn pending_box_assignment(&mut self) -> Option<Token>;
    fn missing_character(&mut self, font: usize, code: u32);
    fn run_hook(&mut self, name: &str) -> Result<()> {
        let hook = match self.env().values.get(&ValueKey::parameter(name)) {
            Some(Quantity::Tokens(tokens)) => Some(tokens.clone()),
            _ => None,
        };
        if let Some(hook) = hook.filter(|hook| !hook.is_empty()) {
            let source = self.input().source();
            self.input_mut().begin_tokens(hook, source, None)?
        }
        Ok(())
    }
}
pub(crate) struct AccentMaterial {
    accent: Node,
    x_height: i64,
    accent_slant: i64,
}
pub struct BoxCommands;
impl BoxCommands {
    pub fn flush(services: &mut impl BoxServices) -> Result<()> {
        services.with_layout(|_, context| {
            context
                .builder
                .flush_lig_stack(context.fonts, context.env, true)
        })
    }
    pub fn read_box(
        services: &mut impl BoxServices,
        code: BoxOperation,
        capture: Option<&mut BoxScanContext>,
        initial_depth: Option<i64>,
        spec: bool,
        hook: bool,
        group_type: Option<GroupKind>,
    ) -> Result<i64> {
        use super::box_execution::{BoxAction, BoxExecution, BoxStep, BoxValue};
        let mut execution = BoxExecution::new(
            code,
            capture.as_deref().cloned(),
            initial_depth,
            spec,
            hook,
            group_type,
        );
        let mut returned = None;
        let result = loop {
            match execution.resume(services, returned.take()) {
                Ok(BoxStep::Complete(pointer)) => break Ok(pointer),
                Err(error) => break Err(error),
                Ok(BoxStep::Yield(action)) => {
                    returned = Some(match action {
                        BoxAction::ExpandedToken => {
                            services.expanded_token().map(|()| BoxValue::Unit)
                        }
                        BoxAction::Scan(request) => services.scan(request).map(BoxValue::Scan),
                        BoxAction::ExecuteToken => {
                            services.execute_token(true).map(BoxValue::Executed)
                        }
                    });
                }
            }
        };
        if let (Some(capture), Some(saved)) = (capture, execution.capture) {
            *capture = saved;
        }
        result
    }
    pub fn box_operand(
        services: &mut impl BoxServices,
        mut capture: Option<&mut BoxScanContext>,
        allow_rule: bool,
        adjusted: bool,
    ) -> Result<i64> {
        services.nonblank()?;
        while services.input().current.is_command(Command::Relax)
            && services.input().current.character() == Some(0)
        {
            services.nonblank()?
        }
        if allow_rule && services.input().current.is_command(Command::Rule) {
            return Self::scan_rule(
                services,
                services.input().current.character().unwrap_or(0) != 0,
            );
        }
        let CommandOperand::Box(code) = services.input().current.value else {
            return Err(Error::value("Expected a box expression")
                .with_issue(crate::core::recovery::Issue::BoxOperand));
        };
        if !services.input().current.is_command(Command::MakeBox) {
            return Err(Error::value("Expected a box expression")
                .with_issue(crate::core::recovery::Issue::BoxOperand));
        }
        if let Some(capture) = capture.as_deref_mut() {
            capture.fresh_hbox = code == BoxOperation::Hbox
        }
        match code {
            BoxOperation::Box | BoxOperation::Copy => {
                let register = services.scan_register(None)?;
                services.take_register_box(register, code == BoxOperation::Copy)
            }
            BoxOperation::LastBox => services.lastbox(),
            BoxOperation::Vsplit => services.split_box(),
            _ => Self::read_box(
                services,
                code,
                capture,
                None,
                true,
                true,
                if adjusted && code == BoxOperation::Hbox {
                    Some(GroupKind::AdjustedHbox)
                } else {
                    None
                },
            ),
        }
    }
    pub fn make_box(services: &mut impl BoxServices) -> Result<()> {
        services.back_input()?;
        let adjusted = services.builder().context.mode == ListMode::Vertical;
        let mut capture = BoxScanContext::default();
        let pointer = Self::box_operand(services, Some(&mut capture), false, adjusted)?;
        if pointer != C::NULL {
            let adjusted =
                capture.fresh_hbox && services.builder().context.mode == ListMode::Vertical;
            services.append_box(pointer, adjusted)?
        }
        Ok(())
    }
    pub fn use_box(services: &mut impl BoxServices, clear_register: bool) -> Result<()> {
        let register = services.scan_register(None)?;
        if services
            .env()
            .box_registers
            .get(&register)
            .copied()
            .unwrap_or(C::NULL)
            != C::NULL
        {
            Self::flush(services)?;
            let pointer = services.take_register_box(register, !clear_register)?;
            services.append_box(pointer, false)?
        }
        Ok(())
    }
    pub fn create_box(services: &mut impl BoxServices, code: BoxOperation) -> Result<()> {
        let pointer = Self::read_box(services, code, None, None, true, true, None)?;
        let adjusted =
            code == BoxOperation::Hbox && services.builder().context.mode == ListMode::Vertical;
        services.append_box(pointer, adjusted)
    }
    pub fn skip_spec(services: &mut impl BoxServices, code: i64) -> Result<GlueSpec> {
        Ok(match code {
            0 => services.scan_glue(false)?,
            1 => GlueSpec {
                stretch: C::UNITY,
                stretch_order: 1,
                ..GlueSpec::default()
            },
            2 => GlueSpec {
                stretch: C::UNITY,
                stretch_order: 2,
                ..GlueSpec::default()
            },
            3 => GlueSpec {
                stretch: C::UNITY,
                shrink: C::UNITY,
                stretch_order: 1,
                shrink_order: 1,
                ..GlueSpec::default()
            },
            _ => GlueSpec {
                stretch: -C::UNITY,
                stretch_order: 1,
                ..GlueSpec::default()
            },
        })
    }
    pub fn discretionary_part(services: &mut impl BoxServices) -> Result<i64> {
        let pointer = Self::read_box(
            services,
            BoxOperation::Hbox,
            None,
            None,
            true,
            false,
            Some(GroupKind::Disc),
        )?;
        Self::finish_discretionary_part(services, pointer)
    }
    pub(crate) fn finish_discretionary_part(
        services: &mut impl BoxServices,
        pointer: i64,
    ) -> Result<i64> {
        let head = services.builder().node(pointer)?.list_ptr;
        let nodes = services.builder().siblings(head)?;
        if nodes.into_iter().any(|pointer| {
            services.builder().node(pointer).is_ok_and(|node| {
                !matches!(
                    node.node_type,
                    NodeKind::Char
                        | NodeKind::Ligature
                        | NodeKind::Kern
                        | NodeKind::Hlist
                        | NodeKind::Vlist
                        | NodeKind::Rule
                )
            })
        }) {
            services.builder_mut().release(pointer, false)?;
            return Err(Error::value(
                "Improper discretionary list: only characters, boxes, rules and kerns are allowed",
            ));
        }
        services.builder_mut().node_mut(pointer)?.list_ptr = C::NULL;
        services.builder_mut().release(pointer, false)?;
        Ok(head)
    }
    pub(crate) fn begin_discretionary(
        services: &mut impl BoxServices,
        shorthand: bool,
    ) -> Result<i64> {
        let origins = vec![services.input().source()];
        let pointer = services.builder_mut().allocate(Node::from(NodeFields {
            node_type: NodeKind::Disc,
            origins: origins.clone(),
            ..NodeFields::default()
        }))?;
        services.builder_mut().tail_append(pointer)?;
        if shorthand {
            let number = services.fonts().current;
            let font = services.fonts().items[number].clone();
            let code = font.hyphenchar;
            if (0..=255).contains(&code) && font.has_character(code as u32) {
                let pre = services.with_layout(|_, context| {
                    context
                        .builder
                        .new_char_node(context.fonts, number, code as u32)
                })?;
                services.builder_mut().node_mut(pre)?.origins = origins.into();
                services.builder_mut().node_mut(pointer)?.pre_break = pre;
                for (left, right, before) in [(256, code as u32, true), (code as u32, 256, false)] {
                    let amount = auto_kern(
                        &font,
                        left,
                        right,
                        services.parameter("pdfprependkern", 0) > 0,
                        services.parameter("pdfappendkern", 0) > 0,
                    )?;
                    if amount != 0 {
                        let kern = services.builder_mut().new_kern(amount)?;
                        let node = services.builder_mut().node_mut(kern)?;
                        node.kern_kind = "auto".into();
                        node.payload = NodePayload::CharacterPolicy {
                            font: number,
                            code: left,
                        };
                        let head = services.builder().node(pointer)?.pre_break;
                        if before {
                            services.builder_mut().node_mut(kern)?.link = head;
                            services.builder_mut().node_mut(pointer)?.pre_break = kern
                        } else {
                            let tail = *services.builder().siblings(head)?.last().unwrap();
                            services.builder_mut().node_mut(tail)?.link = kern
                        }
                    }
                }
            }
        }
        Ok(pointer)
    }
    pub(crate) fn finish_discretionary(
        services: &mut impl BoxServices,
        pointer: i64,
    ) -> Result<()> {
        let replacement = services.builder().node(pointer)?.list_ptr;
        let (mut width, mut height, mut depth) = (0, 0, 0);
        for item in services.builder().siblings(replacement)? {
            let node = services.builder().node(item)?;
            width += node.width;
            height = height.max(node.height - node.shift);
            depth = depth.max(node.depth + node.shift)
        }
        let node = services.builder_mut().node_mut(pointer)?;
        node.width = width;
        node.height = height;
        node.depth = depth;
        Ok(())
    }
    pub fn discretionary(services: &mut impl BoxServices, shorthand: bool) -> Result<i64> {
        use super::box_operands::{DiscretionaryExecution, DiscretionaryStep};
        let mut task = DiscretionaryExecution::new(shorthand);
        let mut returned = None;
        loop {
            match task.resume(services, returned.take())? {
                DiscretionaryStep::ReadPart => returned = Some(Self::discretionary_part(services)),
                DiscretionaryStep::Complete(pointer) => return Ok(pointer),
            }
        }
    }
    pub fn hskip(services: &mut impl BoxServices) -> Result<()> {
        let code = services.input().current.character().unwrap_or(0);
        services.start_paragraph(false)?;
        Self::flush(services)?;
        let spec = Self::skip_spec(services, code)?;
        let pointer = glue(services, spec, "normal")?;
        services.builder_mut().tail_append(pointer)
    }
    pub fn vskip(services: &mut impl BoxServices) -> Result<()> {
        let code = services.input().current.character().unwrap_or(0);
        let spec = Self::skip_spec(services, code)?;
        services.finish_paragraph()?;
        if services.builder().context.mode != ListMode::Vertical {
            return Err(Error::value("Vertical skip in horizontal box")
                .with_issue(crate::core::recovery::Issue::Mode));
        }
        let pointer = glue(services, spec, "normal")?;
        services.builder_mut().tail_append(pointer)
    }
    pub fn setbox(services: &mut impl BoxServices, global: bool) -> Result<()> {
        let register = services.scan_register(None)?;
        services.optional_equals()?;
        let pointer = Self::box_operand(services, None, false, false)?;
        services.assign_box(register, pointer, global)?;
        if let Some(token) = services.pending_box_assignment() {
            let source = services.input().source();
            services
                .input_mut()
                .begin_tokens(vec![token].into(), source, None)?
        }
        Ok(())
    }
    pub fn shift_box(services: &mut impl BoxServices, code: u8) -> Result<()> {
        let horizontal = code < 2;
        let mode = services.builder().context.mode;
        if (horizontal && !matches!(mode, ListMode::Horizontal | ListMode::Math))
            || (!horizontal && mode != ListMode::Vertical)
        {
            return Err(Error::value("Box shift in the wrong mode")
                .with_issue(crate::core::recovery::Issue::Mode));
        }
        let amount = services.scan_dimen()?;
        let mut capture = BoxScanContext::default();
        let pointer = Self::box_operand(services, Some(&mut capture), false, false)?;
        if pointer != C::NULL {
            services.builder_mut().node_mut(pointer)?.shift = if code == 0 || code == 2 {
                -amount
            } else {
                amount
            };
            services.append_box(pointer, !horizontal && capture.fresh_hbox)?
        }
        Ok(())
    }
    pub(crate) fn prepare_accent(
        services: &mut impl BoxServices,
        code: u32,
    ) -> Result<Option<AccentMaterial>> {
        let accent_font = services.fonts().current;
        let font = services.fonts().items[accent_font].clone();
        if !font.has_character(code) {
            services.missing_character(accent_font, code);
            return Ok(None);
        }
        let x_height = font.get_parameter(5);
        let accent_slant = font.get_parameter(1);
        let accent = Node::from(NodeFields {
            node_type: NodeKind::Char,
            font: accent_font,
            char: code,
            width: font.get_char_width(code),
            height: font.get_char_height(code),
            depth: font.get_char_depth(code),
            origins: services.input().source().into_iter().map(Some).collect(),
            ..NodeFields::default()
        });
        Ok(Some(AccentMaterial {
            accent,
            x_height,
            accent_slant,
        }))
    }
    pub(crate) fn finish_accent(
        services: &mut impl BoxServices,
        material: AccentMaterial,
        base_code: Option<u32>,
    ) -> Result<()> {
        let AccentMaterial {
            accent,
            x_height,
            accent_slant,
        } = material;
        let accent_width = accent.width;
        let mut pointer = services.builder_mut().allocate(accent)?;
        let base_font = services.fonts().current;
        let font = services.fonts().items[base_font].clone();
        if let Some(base_code) = base_code.filter(|&code| font.has_character(code)) {
            let base = services.with_layout(|_, context| {
                context
                    .builder
                    .new_char_node(context.fonts, base_font, base_code)
            })?;
            services.builder_mut().node_mut(base)?.origins =
                services.input().source().into_iter().map(Some).collect();
            let height = font.get_char_height(base_code);
            let width = font.get_char_width(base_code);
            if height != x_height {
                pointer = services.with_layout(|layout, context| {
                    layout.hpack(context, pointer, None, false, false)
                })?;
                services.builder_mut().node_mut(pointer)?.shift = x_height - height
            }
            let delta = round_ratio(
                (width as i128 - accent_width as i128) * C::UNITY as i128
                    + 2 * (height as i128 * font.get_parameter(1) as i128
                        - x_height as i128 * accent_slant as i128),
                2 * C::UNITY as i128,
            )? as i64;
            for (width, item) in [(delta, pointer), (-accent_width - delta, base)] {
                let kern = services.builder_mut().new_kern(width)?;
                services.builder_mut().node_mut(kern)?.kern_kind = "accent".into();
                services.builder_mut().tail_append(kern)?;
                services.builder_mut().tail_append(item)?
            }
        } else {
            if let Some(code) = base_code {
                services.missing_character(base_font, code)
            }
            services.builder_mut().tail_append(pointer)?
        }
        services.builder_mut().context.space_factor = 1000;
        Ok(())
    }
    pub fn accent(services: &mut impl BoxServices) -> Result<()> {
        use super::box_operands::{AccentAction, AccentExecution, AccentStep, AccentValue};
        let mut task = AccentExecution::new();
        let mut returned = None;
        loop {
            match task.resume(services, returned.take())? {
                AccentStep::Complete => return Ok(()),
                AccentStep::Yield(AccentAction::Scan(request)) => {
                    returned = Some(services.scan(request).map(AccentValue::Scan))
                }
                AccentStep::Yield(AccentAction::ExecuteToken) => {
                    let in_box = !services.builder().context_stack.is_empty();
                    returned = Some(
                        services
                            .execute_token(in_box)
                            .map(|_| AccentValue::Executed),
                    );
                }
            }
        }
    }
    pub fn rule(services: &mut impl BoxServices, vertical: bool) -> Result<()> {
        if vertical {
            services.start_paragraph(false)?
        } else {
            if services.builder().context.restricted
                && services.builder().context.mode == ListMode::Horizontal
            {
                return Err(
                    Error::value("Horizontal rule in a restricted horizontal box")
                        .with_issue(crate::core::recovery::Issue::Mode),
                );
            }
            if services.builder().context.vertical_paragraph {
                services.finish_paragraph()?
            } else if services.builder().context.mode == ListMode::Horizontal {
                services.handle_par()?
            }
        }
        let pointer = Self::scan_rule(services, vertical)?;
        services.builder_mut().tail_append(pointer)?;
        if !vertical {
            services.builder_mut().context.prev_depth = None
        }
        Ok(())
    }
    pub fn scan_rule(services: &mut impl BoxServices, vertical: bool) -> Result<i64> {
        use super::box_execution::{RuleExecution, RuleStep};
        let mut task = RuleExecution::new(vertical);
        let mut returned = None;
        loop {
            match task.resume(services, returned.take())? {
                RuleStep::Scan(request) => returned = Some(services.scan(request)),
                RuleStep::Complete(pointer) => return Ok(pointer),
            }
        }
    }
    pub fn unpack(services: &mut impl BoxServices, operation: UnpackOperation) -> Result<()> {
        use super::box_operands::{UnpackAction, UnpackExecution, UnpackStep};
        use crate::language::scanning::ScanValue;
        let mut task = UnpackExecution::new(operation);
        let mut returned = None;
        loop {
            match task.resume(services, returned.take())? {
                UnpackStep::Complete => return Ok(()),
                UnpackStep::Yield(UnpackAction::Scan(request)) => {
                    returned = Some(services.scan(request))
                }
                UnpackStep::Yield(UnpackAction::StartParagraph) => {
                    returned = Some(services.start_paragraph(false).map(|()| ScanValue::Unit))
                }
                UnpackStep::Yield(UnpackAction::FinishParagraph) => {
                    returned = Some(services.finish_paragraph().map(|()| ScanValue::Unit))
                }
            }
        }
    }
}

fn glue(s: &mut impl BoxServices, spec: GlueSpec, kind: &str) -> Result<i64> {
    let font = s.fonts().current;
    s.builder_mut().new_glue(spec, kind, font)
}
