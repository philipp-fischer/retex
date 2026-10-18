//! Paragraph lifecycle, baseline spacing and vertical transfers (`paragraph_operations.py`).
use super::hyphenation::Hyphenation;
use super::layout::{Layout, TypesettingContext};
use super::math_atoms::MathState;
use super::model::Node;
use super::nodes::{ListContext, glue_parameter, parameter};
use super::pages::{PageBuilder, PageServices};
use super::penalties::Penalties;
use super::vertical::Vertical;
use crate::core::characters::LanguageProfile;
use crate::core::constants::Constants as C;
use crate::core::enums::{ListMode, NodeKind};
use crate::core::errors::{Error, Result};
use crate::core::values::{GlueSpec, MAX_DIMEN};
use crate::language::input::InputProcessor;
use crate::language::records::{Quantity, ValueIndex, ValueKey};
use crate::typesetting::model::NodeFields;
pub struct ParagraphOperations;
impl ParagraphOperations {
    pub fn consume_outer(
        pages: &mut PageBuilder,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn PageServices,
    ) -> Result<()> {
        if context.builder.context.mode == ListMode::Vertical && !context.builder.context.restricted
        {
            let head = context.builder.take_list(context.fonts, context.env)?;
            if head != C::NULL {
                pages.contribute(vertical, layout, context, services, head)?;
            }
        }
        Ok(())
    }
    pub fn insert_par_token(
        context: &TypesettingContext<'_>,
        input: &mut InputProcessor,
        threshold: i64,
    ) -> Result<bool> {
        if context.builder.context.vertical_paragraph
            && parameter(context.env, "partokencontext", 0) > threshold
        {
            input.back_input()?;
            input.back_token(input.par_token, input.source())?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub fn internal_vertical(context: &TypesettingContext<'_>) -> bool {
        context.builder.context.mode == ListMode::Vertical && context.builder.context.restricted
    }
    pub fn start_box_paragraph(
        pages: &mut PageBuilder,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn PageServices,
        input: &mut InputProcessor,
        indent: bool,
        resume: bool,
    ) -> Result<()> {
        if context.builder.context.mode != ListMode::Vertical {
            return Ok(());
        }
        if !resume
            && (!context.builder.context.restricted
                || context.builder.context.cur_list_head != C::NULL)
        {
            let spec = glue_parameter(context.env, "parskip");
            let pointer = context
                .builder
                .new_glue(spec, "parskip", context.fonts.current)?;
            context.builder.tail_append(pointer)?;
            Self::consume_outer(pages, vertical, layout, context, services)?;
        }
        if !resume {
            context.builder.context.prev_graf = 0;
        }
        context
            .builder
            .push_context(context.fonts, context.env, ListMode::Horizontal, false)?;
        context.builder.context.vertical_paragraph = true;
        let state = Hyphenation::state(context.env, input.profile);
        context.builder.context.initial_hyphenation = state;
        context.builder.context.current_language = state.0;
        if indent {
            let width = parameter(context.env, "parindent", 0);
            let pointer = context.builder.new_null_box()?;
            context.builder.node_mut(pointer)?.width = width;
            context.builder.tail_append(pointer)?;
        }
        if !resume {
            if let Some(Quantity::Tokens(tokens)) =
                context.env.values.get(&ValueKey::parameter("everypar"))
            {
                if !tokens.is_empty() {
                    input.begin_tokens(tokens.clone(), input.source(), None)?;
                }
            }
        }
        Ok(())
    }
    pub fn vertical_context<'a>(context: &'a TypesettingContext<'_>) -> Result<&'a ListContext> {
        std::iter::once(&context.builder.context)
            .chain(context.builder.context_stack.iter().rev())
            .find(|context| context.mode == ListMode::Vertical)
            .ok_or_else(|| Error::value("Paragraph without enclosing vertical list"))
    }
    pub fn normal_paragraph(context: &mut TypesettingContext<'_>) {
        for (name, value) in [("looseness", 0), ("hangindent", 0), ("hangafter", 1)] {
            context
                .env
                .assign_value(ValueKey::parameter(name), Quantity::Integer(value), false);
        }
        context.env.assign_value(
            ValueKey {
                table: "parshape".into(),
                index: ValueIndex::Number(0),
            },
            Quantity::Shape(Vec::new()),
            false,
        );
        context.env.assign_value(
            ValueKey {
                table: "penalties".into(),
                index: ValueIndex::Number(0),
            },
            Quantity::Penalties(Vec::new()),
            false,
        );
    }
    pub fn handle_par(
        pages: &mut PageBuilder,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn PageServices,
        hyphenation: &mut Hyphenation,
        profile: LanguageProfile,
        math: &mut MathState,
        display: bool,
    ) -> Result<()> {
        if context.builder.context.vertical_paragraph {
            Self::finish_box_paragraph(
                pages,
                vertical,
                layout,
                context,
                services,
                hyphenation,
                profile,
                math,
                display,
            )
        } else {
            if context.builder.context.mode == ListMode::Vertical {
                Self::normal_paragraph(context);
            }
            Ok(())
        }
    }
    pub fn finish_box_paragraph(
        pages: &mut PageBuilder,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn PageServices,
        hyphenation: &mut Hyphenation,
        profile: LanguageProfile,
        math: &mut MathState,
        display: bool,
    ) -> Result<()> {
        if !context.builder.context.vertical_paragraph {
            return Ok(());
        }
        let head = context.builder.take_list(context.fonts, context.env)?;
        let previous_lines = Self::vertical_context(context)?.prev_graf;
        let boxes = if head == C::NULL {
            Vec::new()
        } else {
            layout.pack_paragraph(context, hyphenation, profile, head, previous_lines)?
        };
        let hold = context.builder.ownership.hold(boxes.iter().copied());
        if let Some(&last) = boxes.last() {
            math.set_last_line(Some(Self::line_extent(context, last)?));
        }
        context.builder.pop_context()?;
        context.builder.context.prev_graf += boxes.len() as i64;
        let penalties: Vec<_> = boxes
            .iter()
            .take(boxes.len().saturating_sub(1))
            .enumerate()
            .map(|(index, &pointer)| {
                Penalties::between(
                    context.env,
                    previous_lines + index as i64 + 1,
                    index as i64 + 1,
                    (boxes.len() - index - 1) as i64,
                    display,
                    context.builder.node(pointer)?.discretionary_break,
                )
            })
            .collect::<Result<_>>()?;
        if !display {
            Self::normal_paragraph(context);
        }
        if !boxes.is_empty() {
            layout.paragraph_count += 1;
        }
        for (index, &pointer) in boxes.iter().enumerate() {
            let (before, after) = vertical.extract_adjustments(context.builder, pointer)?;
            Self::splice(pages, vertical, layout, context, services, &before)?;
            hold.roots.borrow_mut().retain(|&root| root != pointer);
            Self::append_box(pages, vertical, layout, context, services, pointer)?;
            Self::splice(pages, vertical, layout, context, services, &after)?;
            if let Some(&penalty) = penalties.get(index).filter(|&&value| value != 0) {
                let pointer = context.builder.allocate(Node::from(NodeFields {
                    node_type: NodeKind::Penalty,
                    penalty,
                    ..NodeFields::default()
                }))?;
                context.builder.tail_append(pointer)?;
                Self::consume_outer(pages, vertical, layout, context, services)?;
            }
        }
        drop(hold);
        Ok(())
    }
    pub fn baseline_glue(
        context: &TypesettingContext<'_>,
        height: i64,
        previous: Option<i64>,
    ) -> Option<GlueSpec> {
        let previous = previous.filter(|&previous| previous > -1000 * C::UNITY)?;
        let baseline = glue_parameter(context.env, "baselineskip");
        let gap = baseline.width - previous - height;
        Some(if gap >= parameter(context.env, "lineskiplimit", 0) {
            GlueSpec {
                width: gap,
                protrusion_barrier: false,
                ..baseline
            }
        } else {
            glue_parameter(context.env, "lineskip")
        })
    }
    pub fn append_box(
        pages: &mut PageBuilder,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn PageServices,
        pointer: i64,
    ) -> Result<()> {
        if Self::prepare_append_box(context, pointer)? {
            Self::consume_outer(pages, vertical, layout, context, services)?;
        }
        Ok(())
    }
    pub(crate) fn prepare_append_box(
        context: &mut TypesettingContext<'_>,
        pointer: i64,
    ) -> Result<bool> {
        context
            .builder
            .flush_lig_stack(context.fonts, context.env, true)?;
        if context.builder.context.mode == ListMode::Vertical {
            let node = context.builder.nodes.shape(pointer)?;
            if let Some(spec) =
                Self::baseline_glue(context, node.height, context.builder.context.prev_depth)
            {
                let glue = context
                    .builder
                    .new_glue(spec, "normal", context.fonts.current)?;
                context.builder.tail_append(glue)?;
            }
            context.builder.tail_append(pointer)?;
            context.builder.context.prev_depth = Some(node.depth);
            Ok(true)
        } else {
            if context.builder.context.mode == ListMode::Horizontal {
                context.builder.context.space_factor = 1000;
            }
            context.builder.tail_append(pointer)?;
            Ok(false)
        }
    }
    pub fn splice(
        pages: &mut PageBuilder,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn PageServices,
        pointers: &[i64],
    ) -> Result<()> {
        for &pointer in pointers {
            vertical.take_migration(context.builder, pointer)?;
            Self::consume_outer(pages, vertical, layout, context, services)?;
        }
        Ok(())
    }
    pub fn append_adjusted(
        pages: &mut PageBuilder,
        vertical: &mut Vertical,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        services: &mut dyn PageServices,
        pointer: i64,
    ) -> Result<()> {
        let (before, after) = vertical.extract_adjustments(context.builder, pointer)?;
        let hold = context.builder.ownership.hold([pointer]);
        Self::splice(pages, vertical, layout, context, services, &before)?;
        hold.roots.borrow_mut().clear();
        Self::append_box(pages, vertical, layout, context, services, pointer)?;
        Self::splice(pages, vertical, layout, context, services, &after)
    }
    pub fn line_extent(context: &TypesettingContext<'_>, pointer: i64) -> Result<i64> {
        let box_node = context.builder.node(pointer)?;
        let mut extent =
            box_node.shift + 2 * context.fonts.items[context.fonts.current].get_parameter(6);
        let mut visible = -MAX_DIMEN;
        for pointer in context.builder.siblings(box_node.list_ptr)? {
            let node = context.builder.nodes.shape(pointer)?;
            if let Some(spec) = node.glue.filter(|_| box_node.glue_sign != 0) {
                let (order, amount) = if box_node.glue_sign > 0 {
                    (spec.stretch_order, spec.stretch)
                } else {
                    (spec.shrink_order, spec.shrink)
                };
                if amount != 0 && order == box_node.glue_order {
                    extent = MAX_DIMEN;
                }
            }
            if extent < MAX_DIMEN {
                extent += node.width;
            }
            if matches!(
                node.node_type,
                NodeKind::Char
                    | NodeKind::Ligature
                    | NodeKind::Hlist
                    | NodeKind::Vlist
                    | NodeKind::Rule
            ) {
                visible = extent;
            }
        }
        Ok(visible)
    }
}
