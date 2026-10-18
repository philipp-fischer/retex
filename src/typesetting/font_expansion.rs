//! Glyph and normal-kern elasticity (`typesetting/font_expansion.py`).
use super::layout::TypesettingContext;
use super::model::Node;
use super::nodes::NodeBuilder;
use super::nodes::try_lig_kern;
use crate::core::enums::NodeKind;
use crate::core::errors::Result;
use crate::fonts::catalog::FontCatalog;
use crate::fonts::expansion::{capacity, expanded, substitute};
use crate::fonts::microtypography::{FontCode, code_value};
use crate::fonts::scaling::divide_scaled;
use crate::language::expressions::quotient;

fn pair_kern(fonts: &FontCatalog, font: usize, left: u32, right: u32) -> Result<i64> {
    Ok(try_lig_kern(&fonts.items[font], left, right)?
        .and_then(|matched| matched.kern)
        .unwrap_or(0))
}
pub fn node_capacity(
    context: &mut TypesettingContext<'_>,
    node: &Node,
    previous: Option<&Node>,
    following: Option<&Node>,
) -> Result<(i64, i64)> {
    node_capacity_parts(context.fonts, context.builder, node, previous, following)
}
/// Capacity changes font variants, never node material. Splitting these
/// borrows lets measurement read nodes without cloning their payloads.
pub(crate) fn node_capacity_parts(
    fonts: &mut FontCatalog,
    builder: &NodeBuilder,
    node: &Node,
    previous: Option<&Node>,
    following: Option<&Node>,
) -> Result<(i64, i64)> {
    if node.glyph() {
        return capacity(fonts, node.font, node.char);
    }
    if node.node_type == NodeKind::Disc {
        return list_capacity_parts(fonts, builder, builder.siblings(node.list_ptr)?);
    }
    let (Some(previous), Some(following)) = (previous, following) else {
        return Ok((0, 0));
    };
    if node.node_type != NodeKind::Kern
        || node.kern_kind != "normal"
        || !previous.glyph()
        || !following.glyph()
        || previous.font != following.font
    {
        return Ok((0, 0));
    }
    let font = &fonts.items[previous.font];
    let Some(policy) = font
        .expansion_policy
        .filter(|_| font.expansion_base.is_none())
    else {
        return Ok((0, 0));
    };
    let factor = code_value(font, FontCode::Expansion, previous.char);
    let wide = expanded(fonts, previous.font, policy.stretch)?;
    let narrow = expanded(fonts, previous.font, -policy.shrink)?;
    Ok((
        quotient(
            (pair_kern(fonts, wide, previous.char, following.char)? - node.width) as i128
                * factor as i128,
            1000,
        )? as i64,
        quotient(
            (node.width - pair_kern(fonts, narrow, previous.char, following.char)?) as i128
                * factor as i128,
            1000,
        )? as i64,
    ))
}
pub fn list_capacity(
    context: &mut TypesettingContext<'_>,
    pointers: Vec<i64>,
) -> Result<(i64, i64)> {
    list_capacity_parts(context.fonts, context.builder, pointers)
}
fn list_capacity_parts(
    fonts: &mut FontCatalog,
    builder: &NodeBuilder,
    pointers: Vec<i64>,
) -> Result<(i64, i64)> {
    let mut pending = vec![pointers];
    let mut total = (0, 0);
    while let Some(sequence) = pending.pop() {
        for (index, &pointer) in sequence.iter().enumerate() {
            let node = builder.node(pointer)?;
            if node.node_type == NodeKind::Disc {
                pending.push(builder.siblings(node.list_ptr)?);
                continue;
            }
            let previous = index
                .checked_sub(1)
                .map(|index| builder.node(sequence[index]))
                .transpose()?;
            let following = sequence
                .get(index + 1)
                .map(|&pointer| builder.node(pointer))
                .transpose()?;
            let values = node_capacity_parts(fonts, builder, node, previous, following)?;
            total.0 += values.0;
            total.1 += values.1;
        }
    }
    Ok(total)
}
pub fn pack(context: &mut TypesettingContext<'_>, head: i64, target: i64) -> Result<()> {
    let pointers = context.builder.siblings(head)?;
    let mut totals = (0, 0);
    let mut infinite = false;
    let mut natural = 0;
    for (index, &pointer) in pointers.iter().enumerate() {
        let node = context.builder.node(pointer)?.clone();
        natural += node.width;
        infinite |= node
            .glue
            .is_some_and(|spec| spec.stretch != 0 && spec.stretch_order != 0);
        let previous = index
            .checked_sub(1)
            .map(|index| context.builder.node(pointers[index]).cloned())
            .transpose()?;
        let following = pointers
            .get(index + 1)
            .map(|&pointer| context.builder.node(pointer).cloned())
            .transpose()?;
        let values = node_capacity(context, &node, previous.as_ref(), following.as_ref())?;
        totals.0 += values.0;
        totals.1 += values.1;
    }
    let difference = target - natural;
    if difference == 0 || difference > 0 && infinite {
        return Ok(());
    }
    let available = if difference > 0 { totals.0 } else { totals.1 };
    if available <= 0 {
        return Ok(());
    }
    let normalized = divide_scaled(difference, available, 3)?.clamp(-1000, 1000);
    for pointer in pointers {
        let node = context.builder.node(pointer)?.clone();
        let children = if node.glyph() {
            vec![pointer]
        } else if node.node_type == NodeKind::Disc {
            let mut result = context.builder.siblings(node.pre_break)?;
            result.extend(context.builder.siblings(node.post_break)?);
            result
        } else {
            Vec::new()
        };
        for child in children {
            let character = context.builder.node(child)?;
            if !character.glyph() {
                continue;
            }
            let (font, code) = (character.font, character.char);
            let selected = substitute(context.fonts, font, code, normalized)?;
            let width = context.fonts.items[selected].get_char_width(code);
            let character = context.builder.node_mut(child)?;
            character.font = selected;
            character.width = width;
        }
    }
    Ok(())
}
