//! WEB leader spacing and live list operations (`classic_lists.py`).
use super::layout::TypesettingContext;
use super::math_atoms::{Atom, MathCommand, MathState};
use super::model::Node;
use super::pages::PageBuilder;
use crate::core::constants::Constants as C;
use crate::core::enums::{Command, ListMode, NodeKind};
use crate::core::errors::Error;
use crate::core::errors::Result;
use crate::core::values::GlueSpec;
use crate::core::values::trunc_div;
use crate::language::numeric_codes::last_node_type_code;
use crate::language::records::{Quantity, QuantityKind};
pub fn leader_positions(
    start: i64,
    origin: i64,
    extent: i64,
    size: i64,
    kind: &str,
) -> Result<Vec<i64>> {
    if size <= 0 || extent <= 0 {
        return Ok(Vec::new());
    }
    let extent = extent + 10;
    let edge = start + extent;
    let mut spacing = 0;
    let mut first = if kind == "leaders" {
        let first = origin + size * trunc_div((start - origin) as i128, size as i128)? as i64;
        if first < start { first + size } else { first }
    } else {
        let (count, remainder) = (extent.div_euclid(size), extent.rem_euclid(size));
        if kind == "cleaders" {
            start + remainder / 2
        } else {
            spacing = remainder / (count + 1);
            start + trunc_div((remainder - (count - 1) * spacing) as i128, 2)? as i64
        }
    };
    let mut result = Vec::new();
    while first + size <= edge {
        result.push(first);
        first += size + spacing;
    }
    Ok(result)
}
pub enum Tail {
    Node(Node),
    Atom(Atom),
}
enum TailView {
    Node(crate::checked_material::TailSummary),
    Atom(Atom),
}
pub struct ClassicLists;
impl ClassicLists {
    fn math_atoms(
        context: &TypesettingContext<'_>,
        math: &MathState,
    ) -> Option<std::rc::Rc<std::cell::RefCell<Vec<Atom>>>> {
        math.list_frames
            .iter()
            .rev()
            .find(|frame| frame.context_depth == context.builder.context_stack.len())
            .map(|frame| frame.atoms.clone())
    }
    pub fn tail(
        context: &TypesettingContext<'_>,
        pages: &PageBuilder,
        math: &MathState,
    ) -> Result<Option<Tail>> {
        let builder = &context.builder;
        if builder.context.cur_l.is_some() || !builder.context.lig_stack.is_empty() {
            return Ok(None);
        }
        if builder.context.cur_list_tail != C::NULL {
            let mut node = builder.node(builder.context.cur_list_tail)?.clone();
            if node.node_type == NodeKind::Disc && node.list_ptr != C::NULL {
                node = builder
                    .node(*builder.siblings(node.list_ptr)?.last().unwrap())?
                    .clone();
            }
            return Ok(Some(Tail::Node(node)));
        }
        if builder.context.mode == ListMode::Math {
            if let Some(atoms) = Self::math_atoms(context, math) {
                if let Some(atom) = atoms.borrow().last() {
                    return Ok(Some(if let MathCommand::Raw(pointer) = atom.command {
                        Tail::Node(builder.node(pointer)?.clone())
                    } else {
                        Tail::Atom(atom.clone())
                    }));
                }
            }
        }
        if builder.context.mode == ListMode::Vertical && !builder.context.restricted {
            if let Some(&pointer) = pages.queue.back() {
                return Ok(Some(Tail::Node(builder.node(pointer)?.clone())));
            }
        }
        Ok(None)
    }
    fn summary_tail(
        context: &TypesettingContext<'_>,
        pages: &PageBuilder,
        math: &MathState,
    ) -> Result<Option<TailView>> {
        let builder = &context.builder;
        if builder.context.cur_l.is_some() || !builder.context.lig_stack.is_empty() {
            return Ok(None);
        }
        if builder.context.cur_list_tail != C::NULL {
            let mut node = builder.nodes.tail_summary(builder.context.cur_list_tail)?;
            if node.node_type == NodeKind::Disc {
                let list = builder.node(builder.context.cur_list_tail)?.list_ptr;
                if list != C::NULL {
                    node = builder
                        .nodes
                        .tail_summary(*builder.siblings(list)?.last().unwrap())?;
                }
            }
            return Ok(Some(TailView::Node(node)));
        }
        if builder.context.mode == ListMode::Math {
            if let Some(atoms) = Self::math_atoms(context, math) {
                if let Some(atom) = atoms.borrow().last() {
                    return Ok(Some(if let MathCommand::Raw(pointer) = atom.command {
                        TailView::Node(builder.nodes.tail_summary(pointer)?)
                    } else {
                        TailView::Atom(atom.clone())
                    }));
                }
            }
        }
        if builder.context.mode == ListMode::Vertical && !builder.context.restricted {
            if let Some(&pointer) = pages.queue.back() {
                return Ok(Some(TailView::Node(builder.nodes.tail_summary(pointer)?)));
            }
        }
        Ok(None)
    }
    pub fn read(
        context: &TypesettingContext<'_>,
        pages: &PageBuilder,
        math: &MathState,
        code: i64,
        input_line: usize,
        last_badness: i64,
    ) -> Result<(QuantityKind, Quantity)> {
        if code == 5 {
            let value = if context.builder.context.mode == ListMode::Disabled {
                -1
            } else if context.builder.context.cur_l.is_some()
                || !context.builder.context.lig_stack.is_empty()
            {
                0
            } else {
                match Self::summary_tail(context, pages, math)? {
                    Some(TailView::Node(node)) => last_node_type_code(node.node_type),
                    Some(TailView::Atom(atom)) => match atom.command {
                        MathCommand::MuGlue(_) | MathCommand::Nonscript => 11,
                        MathCommand::MuKern(_) => 12,
                        _ => 15,
                    },
                    None => {
                        if context.builder.context.mode == ListMode::Vertical
                            && !context.builder.context.restricted
                        {
                            pages.last_node_kind.map_or(-1, last_node_type_code)
                        } else {
                            -1
                        }
                    }
                }
            };
            return Ok((QuantityKind::Count, Quantity::Integer(value)));
        }
        if code == 3 {
            return Ok((QuantityKind::Count, Quantity::Integer(input_line as i64)));
        }
        if code == 4 {
            return Ok((QuantityKind::Count, Quantity::Integer(last_badness)));
        }
        let mut kind = match code {
            0 => QuantityKind::Count,
            1 => QuantityKind::Dimen,
            2 => QuantityKind::Skip,
            _ => return Err(Error::value("Invalid last-item code")),
        };
        let mut value = if code == 2 {
            Quantity::Glue(GlueSpec::default())
        } else {
            Quantity::Integer(0)
        };
        if context.builder.context.mode == ListMode::Disabled {
            return Ok((kind, value));
        }
        match Self::summary_tail(context, pages, math)? {
            Some(TailView::Node(node)) => {
                if code == 0 && node.node_type == NodeKind::Penalty {
                    value = Quantity::Integer(node.penalty);
                } else if code == 1 && node.node_type == NodeKind::Kern {
                    value = Quantity::Integer(node.width);
                } else if code == 2 && node.node_type == NodeKind::Glue {
                    value = Quantity::Glue(node.glue.unwrap_or_default());
                    if node.glue_kind == "mu" {
                        kind = QuantityKind::Muskip;
                    }
                }
            }
            Some(TailView::Atom(atom)) => match atom.command {
                MathCommand::MuKern(amount) if code == 1 => value = Quantity::Integer(amount),
                MathCommand::MuGlue(spec) if code == 2 => {
                    kind = QuantityKind::Muskip;
                    value = Quantity::Glue(spec);
                }
                _ => {}
            },
            None if context.builder.context.mode == ListMode::Vertical
                && !context.builder.context.restricted =>
            {
                value = match code {
                    0 => Quantity::Integer(pages.last_penalty),
                    1 => Quantity::Integer(pages.last_kern),
                    _ => Quantity::Glue(pages.last_glue.unwrap_or_default()),
                }
            }
            _ => {}
        }
        Ok((kind, value))
    }
    pub fn detach(
        context: &mut TypesettingContext<'_>,
        pages: &mut PageBuilder,
        types: &[NodeKind],
    ) -> Result<i64> {
        {
            let _native = context
                .builder
                .nodes
                .contract(crate::checked_material::Consumer::Preparation);
            context
                .builder
                .flush_lig_stack(context.fonts, context.env, true)?;
        }
        let tail = context.builder.context.cur_list_tail;
        if tail != C::NULL {
            if !types.contains(&context.builder.nodes.tail_kind(tail)?) {
                return Ok(C::NULL);
            }
            let mut previous = C::NULL;
            let mut pointer = context.builder.context.cur_list_head;
            let mut seen = std::collections::BTreeSet::new();
            let mut pointers = Vec::new();
            while pointer != C::NULL {
                if !seen.insert(pointer) {
                    return Err(Error::value("Cycle in node list"));
                }
                pointers.push(pointer);
                pointer = context.builder.nodes.tail_link(pointer)?;
            }
            for pointer in pointers {
                if pointer == tail {
                    break;
                }
                previous = pointer;
            }
            if previous == C::NULL {
                context.builder.context.cur_list_head = C::NULL;
            } else {
                context.builder.nodes.set_tail_link(previous, C::NULL)?;
            }
            context.builder.context.cur_list_tail = previous;
            context.builder.nodes.set_tail_link(tail, C::NULL)?;
            return Ok(tail);
        }
        if context.builder.context.mode == ListMode::Vertical && !context.builder.context.restricted
        {
            if let Some(&tail) = pages.queue.back() {
                if types.contains(&context.builder.nodes.tail_kind(tail)?) {
                    return Ok(pages.queue.pop_back().unwrap());
                }
            }
        }
        Ok(C::NULL)
    }
    pub fn lastbox(context: &mut TypesettingContext<'_>, pages: &mut PageBuilder) -> Result<i64> {
        if context.builder.context.mode == ListMode::Math {
            return Err(Error::value("lastbox is not allowed in math mode"));
        }
        if context.builder.context.mode == ListMode::Vertical
            && !context.builder.context.restricted
            && pages.queue.is_empty()
            && context.builder.context.cur_list_tail == C::NULL
        {
            return Err(Error::value("Cannot take lastbox from the current page"));
        }
        let pointer = Self::detach(context, pages, &[NodeKind::Hlist, NodeKind::Vlist])?;
        if pointer != C::NULL {
            context.builder.node_mut(pointer)?.shift = 0;
        }
        Ok(pointer)
    }
    pub fn remove(
        context: &mut TypesettingContext<'_>,
        pages: &mut PageBuilder,
        math: &MathState,
        kind: NodeKind,
    ) -> Result<()> {
        if context.builder.context.mode == ListMode::Math
            && context.builder.context.cur_list_tail == C::NULL
        {
            if let Some(atoms) = Self::math_atoms(context, math) {
                let mut atoms = atoms.borrow_mut();
                if let Some(atom) = atoms.last() {
                    let remove = matches!(atom.command, MathCommand::MuGlue(_) if kind == NodeKind::Glue)
                        || matches!(atom.command, MathCommand::MuKern(_) if kind == NodeKind::Kern);
                    let raw = if let MathCommand::Raw(pointer) = atom.command {
                        (context.builder.node(pointer)?.node_type == kind).then_some(pointer)
                    } else {
                        None
                    };
                    if remove || raw.is_some() {
                        atoms.pop();
                        if let Some(pointer) = raw {
                            context.builder.release(pointer, false)?;
                        }
                    }
                    return Ok(());
                }
            }
        }
        if context.builder.context.mode == ListMode::Vertical
            && !context.builder.context.restricted
            && context.builder.context.cur_list_tail == C::NULL
            && pages.queue.is_empty()
        {
            if kind != NodeKind::Glue || pages.last_glue.is_some() {
                return Err(Error::value("Cannot remove material from the current page"));
            }
            return Ok(());
        }
        let pointer = Self::detach(context, pages, &[kind])?;
        if pointer != C::NULL {
            context.builder.release(pointer, false)?;
        }
        Ok(())
    }
    pub fn leaders(
        context: &mut TypesettingContext<'_>,
        kind: &str,
        pointer: i64,
        command: Command,
        spec: GlueSpec,
    ) -> Result<()> {
        if pointer == C::NULL {
            return Ok(());
        }
        let result = (|| {
            let expected = if context.builder.context.mode == ListMode::Vertical {
                Command::Vskip
            } else {
                Command::Hskip
            };
            if command != expected {
                return Err(Error::value("Leaders not followed by proper glue")
                    .with_issue(crate::core::recovery::Issue::BoxOperand));
            }
            let glue = context
                .builder
                .new_glue(spec, "normal", context.fonts.current)?;
            let node = context.builder.node_mut(glue)?;
            node.leader_ptr = pointer;
            node.leader_kind = kind.into();
            context.builder.tail_append(glue)
        })();
        if result.is_err() {
            context.builder.release(pointer, false)?;
        }
        result
    }
}
