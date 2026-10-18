//! Inline/display math placement (`math_layout.py`); parser entry is driven by the engine.
use super::box_commands::BoxServices;
use super::layout::{Layout, TypesettingContext};
use super::math_atoms::{Atom, MathCommand};
use super::math_atoms::{MathState, half};
use super::math_construction::MathContext;
use super::math_families::MathFamilies;
use super::model::Node;
use super::nodes::{glue_parameter, parameter};
use super::pages::{PageBuilder, PageServices};
use super::paragraph_operations::ParagraphOperations;
use super::vertical::Vertical;
use crate::core::constants::Constants as C;
use crate::core::enums::{CharacterCategory as Cat, GroupKind, ListMode};
use crate::core::enums::{MathStyle, NodeKind};
use crate::core::errors::ErrorKind;
use crate::core::errors::{Error, Result};
use crate::language::records::{Quantity, ValueIndex, ValueKey};
use crate::typesetting::model::NodeFields;
use std::collections::BTreeSet;
pub trait MathLifecycleServices: BoxServices {
    fn math_state(&self) -> &MathState;
    fn math_state_mut(&mut self) -> &mut MathState;
    fn paragraph_before_display(&mut self) -> Result<()>;
    fn vertical_prev_graf(&self) -> i64;
    fn increment_prev_graf(&mut self, amount: i64);
    fn parse_math_shift(&mut self) -> Result<Vec<Atom>>;
    fn render_math(
        &mut self,
        atoms: Vec<Atom>,
        style: MathStyle,
        penalties: bool,
    ) -> Result<Vec<i64>>;
    fn place_display_alignment(&mut self, head: i64) -> Result<()>;
    fn place_display(&mut self, pointers: &[i64]) -> Result<()>;
    fn resume_paragraph(&mut self) -> Result<()>;
}
pub struct MathLayout;
impl MathLayout {
    pub fn enter(services: &mut impl MathLifecycleServices) -> Result<()> {
        let mut execution = MathExecution::default();
        let mut returned = None;
        loop {
            match execution.resume(services, returned.take())? {
                MathStep::Complete => return Ok(()),
                MathStep::Yield(action) => {
                    returned = Some(match action {
                        MathAction::Parse => services.parse_math_shift().map(MathValue::Atoms),
                        MathAction::Render {
                            atoms,
                            style,
                            penalties,
                        } => services
                            .render_math(atoms, style, penalties)
                            .map(MathValue::Rendered),
                        MathAction::ExpandedToken => {
                            services.expanded_token().map(|_| MathValue::Unit)
                        }
                        MathAction::ParagraphBeforeDisplay => services
                            .paragraph_before_display()
                            .map(|()| MathValue::Unit),
                        MathAction::PlaceDisplay(pointers) => {
                            services.place_display(&pointers).map(|()| MathValue::Unit)
                        }
                        MathAction::PlaceAlignment(head) => services
                            .place_display_alignment(head)
                            .map(|()| MathValue::Unit),
                        MathAction::OptionalSpace => services
                            .scan(crate::language::scanning::ScanRequest::OptionalSpace)
                            .map(|_| MathValue::Unit),
                    });
                }
            }
        }
    }
    pub fn roots(context: &TypesettingContext<'_>, state: &MathState) -> Vec<i64> {
        state
            .held
            .iter()
            .copied()
            .filter(|pointer| context.builder.nodes.contains_key(pointer))
            .collect()
    }
    pub fn validate_fonts(context: &TypesettingContext<'_>) -> Result<()> {
        for (family, required) in [(2, 22), (3, 13)] {
            for level in 0..3 {
                let font = MathFamilies::family(context.env, level, family);
                if font == 0 || context.fonts.items[font].parameter_count < required {
                    return Err(Error::value("Insufficient math symbol/extension fonts")
                        .with_issue(crate::core::recovery::Issue::MathStructure));
                }
            }
        }
        Ok(())
    }
    pub fn penalty(context: &mut TypesettingContext<'_>, value: i64) -> Result<()> {
        let pointer = context.builder.allocate(Node::from(NodeFields {
            node_type: NodeKind::Penalty,
            penalty: value,
            ..NodeFields::default()
        }))?;
        context.builder.tail_append(pointer)
    }
    pub fn inline(context: &mut TypesettingContext<'_>, pointers: &[i64]) -> Result<()> {
        let surround = parameter(context.env, "mathsurround", 0);
        for before in [true, false] {
            let pointer = context.builder.allocate(Node::from(NodeFields {
                node_type: NodeKind::Math,
                width: surround,
                automatic: before,
                ..NodeFields::default()
            }))?;
            context.builder.tail_append(pointer)?;
            if before {
                let head = Vertical::link(context.builder, pointers)?;
                context.builder.append_list(head)?;
            }
        }
        context.builder.context.space_factor = 1000;
        Ok(())
    }
    pub fn display_alignment(
        pages: &mut PageBuilder,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn PageServices,
        head: i64,
        rows: &BTreeSet<i64>,
        previous_depth: Option<i64>,
    ) -> Result<()> {
        Self::prepare_display_alignment(layout, context, head, rows, previous_depth)?;
        ParagraphOperations::consume_outer(pages, vertical, layout, context, services)
    }
    pub(crate) fn prepare_display_alignment(
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        head: i64,
        rows: &BTreeSet<i64>,
        previous_depth: Option<i64>,
    ) -> Result<()> {
        Self::penalty(context, parameter(context.env, "predisplaypenalty", 0))?;
        let pointer = context.builder.new_glue(
            glue_parameter(context.env, "abovedisplayskip"),
            "normal",
            context.fonts.current,
        )?;
        context.builder.tail_append(pointer)?;
        let indent = parameter(context.env, "displayindent", 0);
        for mut pointer in context.builder.siblings(head)? {
            let node = context.builder.node(pointer)?.clone();
            context.builder.node_mut(pointer)?.link = C::NULL;
            if rows.contains(&pointer) {
                context.builder.node_mut(pointer)?.shift = indent;
            } else if node.node_type == NodeKind::Rule && indent != 0 {
                pointer = layout.hpack(context, pointer, None, false, false)?;
                context.builder.node_mut(pointer)?.shift = indent;
            }
            context.builder.tail_append(pointer)?;
        }
        context.builder.context.prev_depth = previous_depth;
        Self::penalty(context, parameter(context.env, "postdisplaypenalty", 0))?;
        let pointer = context.builder.new_glue(
            glue_parameter(context.env, "belowdisplayskip"),
            "normal",
            context.fonts.current,
        )?;
        context.builder.tail_append(pointer)?;
        Ok(())
    }
    pub fn display(
        pages: &mut PageBuilder,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn PageServices,
        state: &mut MathState,
        pointers: &[i64],
    ) -> Result<()> {
        let mut execution = DisplayExecution::new(pointers.to_vec());
        let mut returned = None;
        loop {
            match execution.resume(vertical, layout, context, state, returned.take())? {
                DisplayStep::Complete => return Ok(()),
                DisplayStep::Yield(action) => {
                    returned = Some(match action {
                        DisplayAction::Append(pointer) => ParagraphOperations::append_box(
                            pages, vertical, layout, context, services, pointer,
                        ),
                        DisplayAction::Splice(pointers) => ParagraphOperations::splice(
                            pages, vertical, layout, context, services, &pointers,
                        ),
                        DisplayAction::Consume => ParagraphOperations::consume_outer(
                            pages, vertical, layout, context, services,
                        ),
                    })
                }
            }
        }
    }
    fn display_plan(
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        state: &mut MathState,
        pointers: &[i64],
    ) -> Result<DisplayPlan> {
        let box_pointer = MathContext {
            typesetting: context,
            layout,
        }
        .hbox(pointers)?;
        let (before, after) = vertical.extract_adjustments(context.builder, box_pointer)?;
        state.held.push(box_pointer);
        let hold = context.builder.ownership.hold([box_pointer]);
        let width = parameter(context.env, "displaywidth", 0);
        let indent = parameter(context.env, "displayindent", 0);
        let (left, equation) = state
            .equation_number
            .map_or((false, None), |(left, equation)| (left, Some(equation)));
        let mut number_width = equation
            .map(|pointer| context.builder.node(pointer).map(|node| node.width))
            .transpose()?
            .unwrap_or(0);
        let required = if number_width != 0 {
            number_width + MathFamilies::param(context.env, context.fonts, 6, MathStyle::Text, 2)?
        } else {
            0
        };
        let mut formula = context.builder.node(box_pointer)?.clone();
        if formula.width + required > width {
            let mut shrink = [0; 4];
            for pointer in context.builder.siblings(formula.list_ptr)? {
                if let Some(spec) = context.builder.node(pointer)?.glue {
                    shrink[spec.shrink_order as usize] += spec.shrink;
                }
            }
            let natural = formula.width;
            if number_width != 0
                && (formula.width - shrink[0] + required <= width
                    || shrink[1..].iter().any(|&amount| amount != 0))
            {
                layout.set_glue(
                    context,
                    &mut formula,
                    natural,
                    Some(width - required),
                    false,
                    false,
                )?;
            } else {
                number_width = 0;
                if formula.width > width {
                    layout.set_glue(context, &mut formula, natural, Some(width), false, false)?;
                }
            }
            *context.builder.node_mut(box_pointer)? = formula.clone();
        }
        let mut displacement = half(width - formula.width);
        if number_width != 0 && displacement < 2 * number_width {
            displacement = half(width - formula.width - number_width);
            if formula.list_ptr != C::NULL
                && context.builder.node(formula.list_ptr)?.node_type == NodeKind::Glue
            {
                displacement = 0;
            }
        }
        let pre = parameter(context.env, "predisplaysize", 0);
        let short = displacement + indent > pre && !left;
        let above = if short {
            "abovedisplayshortskip"
        } else {
            "abovedisplayskip"
        };
        let below = Some(if short {
            "belowdisplayshortskip"
        } else {
            "belowdisplayskip"
        });
        Self::penalty(context, parameter(context.env, "predisplaypenalty", 0))?;
        Ok(DisplayPlan {
            box_pointer,
            before,
            after,
            hold,
            width,
            indent,
            left,
            equation,
            number_width,
            formula_width: formula.width,
            displacement,
            above,
            below,
        })
    }
}

/// Owned math-shift lifecycle. Parsing and language work are driven by the engine.
pub(crate) enum MathAction {
    Parse,
    Render {
        atoms: Vec<Atom>,
        style: MathStyle,
        penalties: bool,
    },
    ExpandedToken,
    OptionalSpace,
    ParagraphBeforeDisplay,
    PlaceAlignment(i64),
    PlaceDisplay(Vec<i64>),
}
pub(crate) enum MathValue {
    Atoms(Vec<Atom>),
    Rendered(Vec<i64>),
    Unit,
}
pub(crate) enum MathStep {
    Yield(MathAction),
    Complete,
}
#[derive(Default)]
pub(crate) struct MathExecution {
    stage: u8,
    display: bool,
    held_start: Option<usize>,
    previous_equation: Option<(bool, i64)>,
    atoms: Option<Vec<Atom>>,
    display_start: Option<DisplayStart>,
}
struct DisplayStart {
    shape: Vec<(i64, i64)>,
    hangindent: i64,
    hangafter: i64,
    preceding: bool,
}
impl MathExecution {
    pub fn resume(
        &mut self,
        services: &mut impl MathLifecycleServices,
        returned: Option<Result<MathValue>>,
    ) -> Result<MathStep> {
        let result = self.step(services, returned);
        let opened = self.held_start.is_some();
        if self.stage > 0 && (result.is_err() || matches!(result, Ok(MathStep::Complete))) {
            if let Some(start) = self.held_start.take() {
                services.math_state_mut().held.truncate(start);
                services.math_state_mut().equation_number = self.previous_equation.take();
            }
        }
        result.map_err(|error| {
            if opened && error.kind == ErrorKind::Eof {
                Error::value("Unexpected EOF in math shift")
                    .with_issue(crate::core::recovery::Issue::RunawayArgument)
            } else {
                error
            }
        })
    }
    fn step(
        &mut self,
        services: &mut impl MathLifecycleServices,
        returned: Option<Result<MathValue>>,
    ) -> Result<MathStep> {
        let returned = returned.transpose()?;
        if self.stage == 0 {
            services.start_paragraph(false)?;
            services.raw_token()?;
            let display = services.input().current.is_category(Cat::MathShift)
                && !services.builder().context.restricted;
            let shape = match services.env().values.get(&ValueKey {
                table: "parshape".into(),
                index: ValueIndex::Number(0),
            }) {
                Some(Quantity::Shape(shape)) => shape.clone(),
                _ => Vec::new(),
            };
            let hangindent = services.parameter("hangindent", 0);
            let hangafter = services.parameter("hangafter", 1);
            let preceding = services.builder().context.vertical_paragraph
                && services.builder().context.cur_list_head != C::NULL;
            if !display {
                services.back_input()?;
                services.start_paragraph(false)?
            } else {
                services.math_state_mut().set_last_line(None);
                self.display_start = Some(DisplayStart {
                    shape,
                    hangindent,
                    hangafter,
                    preceding,
                });
                self.stage = 6;
                return Ok(MathStep::Yield(MathAction::ParagraphBeforeDisplay));
            }
            return self.open(
                services,
                display,
                DisplayStart {
                    shape,
                    hangindent,
                    hangafter,
                    preceding,
                },
            );
        }
        if self.stage == 6 {
            if !matches!(returned, Some(MathValue::Unit)) {
                return Err(Error::runtime("Invalid pre-display paragraph result"));
            }
            let start = self.display_start.take().unwrap();
            return self.open(services, true, start);
        }
        if self.stage == 1 {
            let Some(MathValue::Atoms(atoms)) = returned else {
                return Err(Error::runtime("Invalid math parse result"));
            };
            self.atoms = Some(atoms);
            services.builder_mut().pop_context()?;
            if self.display && services.math_state().equation_number.is_none() {
                self.stage = 2;
                return Ok(MathStep::Yield(MathAction::ExpandedToken));
            }
        } else if self.stage == 2 {
            if !matches!(returned, Some(MathValue::Unit)) {
                return Err(Error::runtime("Invalid math token result"));
            }
            if !services.input().current.is_category(Cat::MathShift) {
                return Err(Error::value("Display math should end with $$")
                    .with_issue(crate::core::recovery::Issue::MathStructure));
            }
        } else if self.stage == 3 {
            if !matches!(returned, Some(MathValue::Unit)) {
                return Err(Error::runtime("Invalid math optional space result"));
            }
            return Ok(MathStep::Complete);
        } else if self.stage == 5 || self.stage == 7 {
            if !matches!(returned, Some(MathValue::Unit)) {
                return Err(Error::runtime("Invalid display alignment placement result"));
            }
        } else if self.stage == 4 {
            let Some(MathValue::Rendered(pointers)) = returned else {
                return Err(Error::runtime("Invalid math render result"));
            };
            if self.display {
                self.stage = 7;
                return Ok(MathStep::Yield(MathAction::PlaceDisplay(pointers)));
            } else {
                services.with_layout(|_, context| MathLayout::inline(context, &pointers))?;
            }
        } else {
            return Err(Error::runtime("Invalid math lifecycle stage"));
        }
        if !matches!(self.stage, 4 | 5 | 7) {
            let atoms = self.atoms.take().unwrap();
            let aligned = atoms.len() == 1 && matches!(atoms[0].command, MathCommand::Alignment(_));
            if atoms
                .iter()
                .any(|atom| matches!(atom.command, MathCommand::Alignment(_)))
                && !aligned
            {
                return Err(Error::value("Material after display alignment")
                    .with_issue(crate::core::recovery::Issue::MathStructure));
            }
            if aligned {
                let MathCommand::Alignment(head) = atoms[0].command else {
                    unreachable!()
                };
                self.stage = 5;
                return Ok(MathStep::Yield(MathAction::PlaceAlignment(head)));
            } else {
                services.with_layout(|_, context| MathLayout::validate_fonts(context))?;
                let penalties = !self.display && !services.builder().context.restricted;
                self.stage = 4;
                return Ok(MathStep::Yield(MathAction::Render {
                    atoms,
                    style: if self.display {
                        MathStyle::Display
                    } else {
                        MathStyle::Text
                    },
                    penalties,
                }));
            }
        }
        let display = self.display;
        services.end_group("math_shift", true)?;
        services
            .math_state_mut()
            .held
            .truncate(self.held_start.unwrap());
        if display {
            services.increment_prev_graf(3);
            services.resume_paragraph()?;
        }

        if display {
            self.stage = 3;
            Ok(MathStep::Yield(MathAction::OptionalSpace))
        } else {
            Ok(MathStep::Complete)
        }
    }
    fn open(
        &mut self,
        services: &mut impl MathLifecycleServices,
        display: bool,
        start: DisplayStart,
    ) -> Result<MathStep> {
        let DisplayStart {
            shape,
            hangindent,
            hangafter,
            preceding,
        } = start;
        self.held_start = Some(services.math_state().held.len());
        self.previous_equation = services.math_state_mut().equation_number.take();
        services.with_layout(|_, context| {
            context
                .builder
                .push_context(context.fonts, context.env, ListMode::Math, !display)
        })?;
        services.begin_group("math_shift", GroupKind::MathShift)?;
        services
            .env_mut()
            .assign_value(ValueKey::parameter("fam"), Quantity::Integer(-1), false);
        if display {
            let graf = services.vertical_prev_graf();
            let mut width = services.parameter("hsize", 0);
            let mut indent = 0;
            if !shape.is_empty() {
                (indent, width) = shape[((graf + 1).max(0) as usize).min(shape.len() - 1)]
            } else if hangindent != 0
                && ((hangafter >= 0 && graf + 2 > hangafter) || graf + 1 < -hangafter)
            {
                width -= hangindent.abs();
                indent = hangindent.max(0)
            }
            services.env_mut().assign_value(
                ValueKey::parameter("displaywidth"),
                Quantity::Integer(width),
                false,
            );
            services.env_mut().assign_value(
                ValueKey::parameter("displayindent"),
                Quantity::Integer(indent),
                false,
            );
            let size = if preceding {
                services
                    .math_state()
                    .last_line()
                    .unwrap_or(-crate::core::values::MAX_DIMEN)
            } else {
                -crate::core::values::MAX_DIMEN
            };
            services.env_mut().assign_value(
                ValueKey::parameter("predisplaysize"),
                Quantity::Integer(size),
                false,
            )
        }
        services.run_hook(if display { "everydisplay" } else { "everymath" })?;
        self.display = display;

        self.stage = 1;
        return Ok(MathStep::Yield(MathAction::Parse));
    }
}

struct DisplayPlan {
    box_pointer: i64,
    before: Vec<i64>,
    after: Vec<i64>,
    hold: crate::core::runtime::RootHold,
    width: i64,
    indent: i64,
    left: bool,
    equation: Option<i64>,
    number_width: i64,
    formula_width: i64,
    displacement: i64,
    above: &'static str,
    below: Option<&'static str>,
}
pub(crate) enum DisplayAction {
    Append(i64),
    Splice(Vec<i64>),
    Consume,
}
pub(crate) enum DisplayStep {
    Yield(DisplayAction),
    Complete,
}
pub(crate) struct DisplayExecution {
    stage: u8,
    pointers: Vec<i64>,
    plan: Option<DisplayPlan>,
}
impl DisplayExecution {
    pub fn new(pointers: Vec<i64>) -> Self {
        Self {
            stage: 0,
            pointers,
            plan: None,
        }
    }
    pub fn resume(
        &mut self,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        state: &mut MathState,
        returned: Option<Result<()>>,
    ) -> Result<DisplayStep> {
        if let Some(returned) = returned {
            returned?;
        }
        if self.stage == 0 {
            self.plan = Some(MathLayout::display_plan(
                vertical,
                layout,
                context,
                state,
                &self.pointers,
            )?);
            let plan = self.plan.as_ref().unwrap();
            if plan.left && plan.number_width == 0 {
                let equation = plan
                    .equation
                    .ok_or_else(|| Error::value("Left equation number without a box"))?;
                context.builder.node_mut(equation)?.shift = plan.indent;
                self.stage = 1;
                return Ok(DisplayStep::Yield(DisplayAction::Append(equation)));
            }
            let pointer = context.builder.new_glue(
                glue_parameter(context.env, plan.above),
                "normal",
                context.fonts.current,
            )?;
            context.builder.tail_append(pointer)?;
            self.stage = 2;
        }
        let plan = self.plan.as_mut().unwrap();
        if self.stage == 1 {
            MathLayout::penalty(context, 10000)?;
            self.stage = 2;
        }
        if self.stage == 2 {
            if plan.number_width != 0 {
                let gap = plan.width - plan.formula_width - plan.number_width - plan.displacement;
                let kern = context.builder.new_kern(gap)?;
                let equation = plan.equation.unwrap();
                plan.box_pointer = MathContext {
                    typesetting: context,
                    layout,
                }
                .hbox(&if plan.left {
                    [equation, kern, plan.box_pointer]
                } else {
                    [plan.box_pointer, kern, equation]
                })?;
                plan.hold.roots.borrow_mut().push(plan.box_pointer);
                if plan.left {
                    plan.displacement = 0;
                }
            }
            context.builder.node_mut(plan.box_pointer)?.shift = plan.indent + plan.displacement;
            self.stage = 3;
            // Append's receiver owns the complete formula before page/output
            // callbacks. Retaining its children here would outlive consumption.
            plan.hold.roots.borrow_mut().clear();
            return Ok(DisplayStep::Yield(DisplayAction::Append(plan.box_pointer)));
        }
        if self.stage == 3 {
            if let Some(equation) = plan
                .equation
                .filter(|_| plan.number_width == 0 && !plan.left)
            {
                MathLayout::penalty(context, 10000)?;
                let number_width = context.builder.node(equation)?.width;
                context.builder.node_mut(equation)?.shift = plan.indent + plan.width - number_width;
                self.stage = 4;
                return Ok(DisplayStep::Yield(DisplayAction::Append(equation)));
            }
            self.stage = 5;
        }
        if self.stage == 4 {
            plan.below = None;
            self.stage = 5;
        }
        if self.stage == 5 {
            self.stage = 6;
            return Ok(DisplayStep::Yield(DisplayAction::Splice(std::mem::take(
                &mut plan.after,
            ))));
        }
        if self.stage == 6 {
            self.stage = 7;
            return Ok(DisplayStep::Yield(DisplayAction::Splice(std::mem::take(
                &mut plan.before,
            ))));
        }
        if self.stage == 7 {
            MathLayout::penalty(context, parameter(context.env, "postdisplaypenalty", 0))?;
            if let Some(below) = plan.below {
                let pointer = context.builder.new_glue(
                    glue_parameter(context.env, below),
                    "normal",
                    context.fonts.current,
                )?;
                context.builder.tail_append(pointer)?;
            }
            self.stage = 8;
            return Ok(DisplayStep::Yield(DisplayAction::Consume));
        }
        Ok(DisplayStep::Complete)
    }
}
