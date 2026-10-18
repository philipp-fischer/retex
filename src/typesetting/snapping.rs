//! Ordered vertical grid effects independent of metrics (`snapping.py`).
use super::layout::{Layout, TypesettingContext};
use super::model::{Node, NodePayload, PdfEffect};
use crate::core::enums::NodeKind;
use crate::core::errors::{Error, Result};
use crate::core::values::{GlueSpec, MAX_DIMEN, trunc_div};
use crate::language::expressions::quotient;
pub fn gap(glue: GlueSpec, position: i64, reference: i64) -> Result<i64> {
    if glue.width <= 0 {
        return Err(Error::value("Snap grid spacing must be positive"));
    }
    let lower = reference
        + (glue.width as i128
            * trunc_div(position as i128 - reference as i128, glue.width as i128)?)
            as i64;
    let backward = position - lower;
    let forward = lower + glue.width - position;
    let shrink = if glue.shrink_order != 0 {
        MAX_DIMEN
    } else {
        glue.shrink
    };
    let stretch = if glue.stretch_order != 0 {
        MAX_DIMEN
    } else {
        glue.stretch
    };
    let mut choices = Vec::new();
    if backward < shrink {
        choices.push((-backward, backward));
    }
    if forward < stretch {
        choices.push((forward, forward));
    }
    Ok(choices
        .into_iter()
        .min_by_key(|&(movement, distance)| (distance, -movement))
        .map_or(0, |(movement, _)| movement))
}
pub fn apply(
    layout: &mut Layout,
    context: &mut TypesettingContext<'_>,
    node: &Node,
    pointer: i64,
    y: i64,
    vertical: bool,
    parent: Option<&Node>,
) -> Result<i64> {
    let NodePayload::Pdf(effect) = &node.payload else {
        return Err(Error::value("Snap requires a PDF effect"));
    };
    if matches!(effect, PdfEffect::SnapReferencePoint) {
        layout.snap_reference_y = y;
        return Ok(0);
    }
    if !vertical {
        return Err(Error::value("Vertical snapping requires a vertical list"));
    }
    if let PdfEffect::SnapY(glue) = effect {
        return match node.snap_adjustment {
            Some(adjustment) => Ok(adjustment),
            None => gap(*glue, y, layout.snap_reference_y),
        };
    }
    let PdfEffect::SnapCompensation(value) = effect else {
        return Err(Error::value("Invalid snap effect"));
    };
    let Some(parent) = parent else {
        return Ok(0);
    };
    let mut parent = parent.clone();
    parent.list_ptr = pointer;
    let mut position = y;
    for (current, effective) in layout.effective_widths(context.builder, &parent, true)? {
        let candidate = context.builder.node(current)?;
        if let NodePayload::Pdf(PdfEffect::SnapY(glue)) = candidate.payload {
            let displacement = gap(glue, position, layout.snap_reference_y)?;
            let adjustment = quotient(displacement as i128 * *value as i128, 1000)? as i64;
            let remainder = displacement - adjustment;
            context.builder.node_mut(current)?.snap_adjustment =
                Some(if layout.exact_repro && remainder == 0 {
                    1
                } else {
                    remainder
                });
            return Ok(adjustment);
        }
        position += if matches!(
            candidate.node_type,
            NodeKind::Hlist | NodeKind::Vlist | NodeKind::Rule
        ) || candidate.sized_whatsit()
        {
            candidate.height + candidate.depth
        } else {
            effective
        };
    }
    Ok(0)
}
