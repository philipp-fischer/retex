//! Math rendering on an explicit native continuation stack (`math_renderer.py`).
use super::ligature_cycles::PumpingGuard;
use super::math_atoms::{Atom, Field, MathCommand, MathField, half};
use super::math_codes::DelimiterCode;
use super::math_construction::MathContext;
use super::math_families::MathFamilies;
use super::math_spacing::{AtomKind, SPACING};
use super::model::Node;
use super::nodes::{glue_parameter, parameter, try_lig_kern};
use crate::core::constants::Constants as C;
use crate::core::enums::{MathStyle, NodeKind};
use crate::core::errors::{Error, Result};
use crate::core::values::{GlueSpec, trunc_div};
use crate::fonts::formats::tfm::CharacterTag;
use crate::fonts::ligatures::LigatureAction;
use crate::typesetting::model::NodeFields;
use std::collections::BTreeSet;
use std::sync::Arc;

pub struct RenderedMath {
    pub nodes: Vec<i64>,
    pub middles: Vec<(i64, DelimiterCode)>,
}
enum MathValue {
    Box(i64),
    Field(Field),
    Nodes(RenderedMath),
}
impl MathValue {
    fn box_pointer(self) -> Result<i64> {
        if let Self::Box(pointer) = self {
            Ok(pointer)
        } else {
            Err(Error::value("Unexpected math continuation result"))
        }
    }
    fn field(self) -> Result<Field> {
        if let Self::Field(field) = self {
            Ok(field)
        } else {
            Err(Error::value("Unexpected math field result"))
        }
    }
    fn nodes(self) -> Result<RenderedMath> {
        if let Self::Nodes(nodes) = self {
            Ok(nodes)
        } else {
            Err(Error::value("Unexpected math render result"))
        }
    }
}
enum Request {
    Render {
        atoms: Vec<Atom>,
        style: MathStyle,
        penalties: bool,
        allow_middles: bool,
    },
    Clean(Field, MathStyle),
    Atom(Atom, MathStyle),
    Fraction(Atom, MathStyle),
    Scripts(Atom, Vec<i64>, i64, MathStyle),
    Accent(Atom, MathStyle),
    Operator(Atom, Field, MathStyle),
}
type Resume = Box<
    dyn for<'a, 'b> FnOnce(&mut MathRenderer, &mut MathContext<'a, 'b>, MathValue) -> Result<()>,
>;
enum Operation {
    Request(Request),
    Resume(Resume),
}
#[derive(Default)]
pub struct MathRenderer {
    operations: Vec<Operation>,
    value: Option<MathValue>,
}
struct RenderFrame {
    atoms: Vec<Atom>,
    initial: MathStyle,
    style: MathStyle,
    index: usize,
    previous: Option<AtomKind>,
    suppress: bool,
    penalties: bool,
    allow_middles: bool,
    result: RenderedMath,
}
fn char_field(field: &Field) -> Option<(i64, u32)> {
    match field.as_deref() {
        Some(MathField::Character { family, code, .. }) => Some((*family, *code)),
        _ => None,
    }
}

fn nodes_value(nodes: Vec<i64>) -> MathValue {
    MathValue::Nodes(RenderedMath {
        nodes,
        middles: Vec::new(),
    })
}
fn mu_glue(spec: GlueSpec, mu: i64) -> Result<GlueSpec> {
    let scale = |amount: i64| {
        trunc_div(amount as i128 * mu as i128, C::UNITY as i128).map(|value| value as i64)
    };
    Ok(GlueSpec {
        width: scale(spec.width)?,
        stretch: if spec.stretch_order == 0 {
            scale(spec.stretch)?
        } else {
            spec.stretch
        },
        shrink: if spec.shrink_order == 0 {
            scale(spec.shrink)?
        } else {
            spec.shrink
        },
        ..spec
    })
}
impl MathRenderer {
    fn defer(
        &mut self,
        request: Request,
        resume: impl for<'a, 'b> FnOnce(&mut Self, &mut MathContext<'a, 'b>, MathValue) -> Result<()>
        + 'static,
    ) {
        self.operations.push(Operation::Resume(Box::new(resume)));
        self.operations.push(Operation::Request(request));
    }
    fn complete(&mut self, value: MathValue) {
        self.value = Some(value);
    }
    fn start(&mut self, request: Request) -> Result<()> {
        if !self.operations.is_empty() {
            return Err(Error::value(
                "Math renderer reentered with pending continuations",
            ));
        }
        self.value = None;
        self.operations.push(Operation::Request(request));
        Ok(())
    }
    fn advance_value(
        &mut self,
        context: &mut MathContext<'_, '_>,
        steps: usize,
    ) -> Result<Option<MathValue>> {
        let result = (|| {
            for _ in 0..steps {
                let Some(operation) = self.operations.pop() else {
                    break;
                };
                match operation {
                    Operation::Request(request) => self.request(context, request)?,
                    Operation::Resume(resume) => {
                        let value = self
                            .value
                            .take()
                            .ok_or_else(|| Error::value("Missing math child result"))?;
                        resume(self, context, value)?;
                    }
                }
            }
            if self.operations.is_empty() {
                Ok(Some(
                    self.value
                        .take()
                        .ok_or_else(|| Error::value("Missing math result"))?,
                ))
            } else {
                Ok(None)
            }
        })();
        if result.is_err() {
            self.operations.clear();
            self.value = None;
        }
        result
    }
    pub(crate) fn start_render(
        &mut self,
        atoms: Vec<Atom>,
        style: MathStyle,
        penalties: bool,
        allow_middles: bool,
    ) -> Result<()> {
        self.start(Request::Render {
            atoms,
            style,
            penalties,
            allow_middles,
        })
    }
    pub(crate) fn advance_render(
        &mut self,
        context: &mut MathContext<'_, '_>,
        steps: usize,
    ) -> Result<Option<RenderedMath>> {
        self.advance_value(context, steps)?
            .map(MathValue::nodes)
            .transpose()
    }
    fn run(&mut self, context: &mut MathContext<'_, '_>, request: Request) -> Result<MathValue> {
        self.start(request)?;
        self.advance_value(context, usize::MAX)?
            .ok_or_else(|| Error::runtime("Synchronous math render did not finish"))
    }
    pub fn render(
        &mut self,
        context: &mut MathContext<'_, '_>,
        atoms: Vec<Atom>,
        style: MathStyle,
        penalties: bool,
        allow_middles: bool,
    ) -> Result<RenderedMath> {
        self.run(
            context,
            Request::Render {
                atoms,
                style,
                penalties,
                allow_middles,
            },
        )?
        .nodes()
    }
    pub fn clean(
        &mut self,
        context: &mut MathContext<'_, '_>,
        field: Field,
        style: MathStyle,
    ) -> Result<i64> {
        self.run(context, Request::Clean(field, style))?
            .box_pointer()
    }
    pub fn atom(
        &mut self,
        context: &mut MathContext<'_, '_>,
        atom: Atom,
        style: MathStyle,
    ) -> Result<Vec<i64>> {
        Ok(self
            .run(context, Request::Atom(atom, style))?
            .nodes()?
            .nodes)
    }
    fn request(&mut self, context: &mut MathContext<'_, '_>, request: Request) -> Result<()> {
        match request {
            Request::Render {
                atoms,
                style,
                penalties,
                allow_middles,
            } => self.render_begin(context, atoms, style, penalties, allow_middles),
            Request::Clean(field, style) => self.clean_begin(context, field, style),
            Request::Atom(atom, style) => self.atom_begin(context, atom, style),
            Request::Fraction(atom, style) => self.fraction_begin(context, atom, style),
            Request::Scripts(atom, pointers, delta, style) => {
                self.scripts_begin(context, atom, pointers, delta, style)
            }
            Request::Accent(atom, style) => self.accent_begin(context, atom, style),
            Request::Operator(atom, nucleus, style) => {
                self.operator_begin(context, atom, nucleus, style)
            }
        }
    }
    fn render_begin(
        &mut self,
        context: &mut MathContext<'_, '_>,
        original: Vec<Atom>,
        initial: MathStyle,
        penalties: bool,
        allow_middles: bool,
    ) -> Result<()> {
        let mut style = initial;
        let mut atoms = Vec::new();
        for atom in original {
            if let MathCommand::Choice(choices) = &atom.command {
                if let Some(field) = &choices[style.choice_index()] {
                    if let MathField::List(chosen) = field.as_ref() {
                        atoms.extend(chosen.clone());
                    } else {
                        atoms.push(Atom {
                            nucleus: Some(field.clone()),
                            ..Atom::default()
                        });
                    }
                } else {
                    atoms.push(Atom::default());
                }
            } else {
                atoms.push(atom.clone());
            }
            match atom.command {
                MathCommand::Style(selected) => style = selected,
                MathCommand::Middle(_) => style = initial,
                _ => {}
            }
        }
        let mut previous: Option<(Option<usize>, AtomKind)> = None;
        for index in 0..atoms.len() {
            if atoms[index].command.is_transparent() {
                continue;
            }
            if atoms[index].kind == AtomKind::Binary
                && previous.is_none_or(|(_, kind)| {
                    matches!(
                        kind,
                        AtomKind::Operator
                            | AtomKind::Binary
                            | AtomKind::Relation
                            | AtomKind::Opening
                            | AtomKind::Punctuation
                    )
                })
            {
                atoms[index].kind = AtomKind::Ordinary;
            } else if matches!(
                atoms[index].kind,
                AtomKind::Relation | AtomKind::Closing | AtomKind::Punctuation
            ) {
                if let Some((Some(at), AtomKind::Binary)) = previous {
                    atoms[at].kind = AtomKind::Ordinary;
                }
            }
            previous = Some(if matches!(atoms[index].command, MathCommand::Middle(_)) {
                (None, AtomKind::Opening)
            } else {
                (Some(index), atoms[index].kind)
            });
        }
        if let Some((Some(index), AtomKind::Binary)) = previous {
            atoms[index].kind = AtomKind::Ordinary;
        }
        let mut current_style = initial;
        let mut index = 0;
        while index < atoms.len() {
            match atoms[index].command {
                MathCommand::Style(style) => current_style = style,
                MathCommand::Middle(_) => current_style = initial,
                _ if atoms[index].kind == AtomKind::Ordinary => {
                    Self::make_ord(context, &mut atoms, index, current_style)?
                }
                _ => {}
            }
            index += 1;
        }
        self.render_next(
            context,
            RenderFrame {
                atoms,
                initial,
                style: initial,
                index: 0,
                previous: None,
                suppress: false,
                penalties,
                allow_middles,
                result: RenderedMath {
                    nodes: Vec::new(),
                    middles: Vec::new(),
                },
            },
        )
    }
    fn render_next(
        &mut self,
        context: &mut MathContext<'_, '_>,
        mut frame: RenderFrame,
    ) -> Result<()> {
        while frame.index < frame.atoms.len() {
            let index = frame.index;
            let atom = frame.atoms[index].clone();
            frame.index += 1;
            if frame.suppress {
                frame.suppress = false;
                let suppressed = matches!(
                    atom.command,
                    MathCommand::MuGlue(_) | MathCommand::MuKern(_)
                ) || matches!(atom.command, MathCommand::Raw(pointer) if matches!(context.typesetting.builder.node(pointer)?.node_type, NodeKind::Glue | NodeKind::Kern));
                if suppressed {
                    continue;
                }
            }
            match atom.command {
                MathCommand::Nonscript => {
                    frame.suppress = frame.style.is_script();
                    frame
                        .result
                        .nodes
                        .push(context.typesetting.builder.new_glue(
                            GlueSpec::default(),
                            "normal",
                            context.typesetting.fonts.current,
                        )?);
                    continue;
                }
                MathCommand::Style(style) => {
                    frame.style = style;
                    continue;
                }
                MathCommand::Raw(pointer) => {
                    frame.result.nodes.push(pointer);
                    continue;
                }
                MathCommand::MuKern(amount) => {
                    let mu = trunc_div(context.param(6, frame.style, 2)? as i128, 18)?;
                    let pointer =
                        context
                            .typesetting
                            .builder
                            .new_kern(trunc_div(amount as i128 * mu, C::UNITY as i128)? as i64)?;
                    context.typesetting.builder.node_mut(pointer)?.kern_kind = "explicit".into();
                    frame.result.nodes.push(pointer);
                    continue;
                }
                MathCommand::MuGlue(spec) => {
                    let mu = trunc_div(context.param(6, frame.style, 2)? as i128, 18)? as i64;
                    frame
                        .result
                        .nodes
                        .push(context.typesetting.builder.new_glue(
                            mu_glue(spec, mu)?,
                            "normal",
                            context.typesetting.fonts.current,
                        )?);
                    continue;
                }
                MathCommand::Middle(_) => frame.style = frame.initial,
                _ => {}
            }
            if let Some(previous) = frame.previous {
                if let Some(name) = SPACING[previous as usize][atom.kind as usize]
                    .parameter(frame.style.font_size() != 0)
                {
                    let spec = glue_parameter(context.typesetting.env, name);
                    let mu = trunc_div(context.param(6, frame.style, 2)? as i128, 18)? as i64;
                    frame
                        .result
                        .nodes
                        .push(context.typesetting.builder.new_glue(
                            mu_glue(spec, mu)?,
                            "normal",
                            context.typesetting.fonts.current,
                        )?);
                }
            }
            if let MathCommand::Middle(code) = atom.command {
                if !frame.allow_middles {
                    return Err(Error::value("Unmatched middle delimiter")
                        .with_issue(crate::core::recovery::Issue::MathDelimiter));
                }
                let placeholder = context.typesetting.builder.new_null_box()?;
                frame.result.middles.push((placeholder, code));
                frame.result.nodes.push(placeholder);
                frame.previous = Some(AtomKind::Opening);
                continue;
            }
            let mut penalty = None;
            if frame.penalties && matches!(atom.kind, AtomKind::Binary | AtomKind::Relation) {
                if let Some(following) = frame.atoms.get(index + 1) {
                    let explicit = matches!(following.command, MathCommand::Raw(pointer) if context.typesetting.builder.node(pointer)?.node_type == NodeKind::Penalty);
                    if following.kind != AtomKind::Relation && !explicit {
                        let value = parameter(
                            context.typesetting.env,
                            if atom.kind == AtomKind::Binary {
                                "binoppenalty"
                            } else {
                                "relpenalty"
                            },
                            0,
                        );
                        if value < 10000 {
                            penalty = Some(value);
                        }
                    }
                }
            }
            frame.previous = Some(atom.kind);
            let style = frame.style;
            self.defer(
                Request::Atom(atom, style),
                move |renderer, context, value| {
                    frame.result.nodes.extend(value.nodes()?.nodes);
                    if let Some(penalty) = penalty {
                        frame
                            .result
                            .nodes
                            .push(context.typesetting.builder.allocate(Node::from(
                                NodeFields {
                                    node_type: NodeKind::Penalty,
                                    penalty,
                                    ..NodeFields::default()
                                },
                            ))?);
                    }
                    renderer.render_next(context, frame)
                },
            );
            return Ok(());
        }
        self.complete(MathValue::Nodes(frame.result));
        Ok(())
    }
    pub fn make_ord(
        context: &mut MathContext<'_, '_>,
        atoms: &mut Vec<Atom>,
        index: usize,
        style: MathStyle,
    ) -> Result<()> {
        let mut guard = PumpingGuard::new("math ligature construction");
        loop {
            let atom = &atoms[index];
            if !matches!(atom.command, MathCommand::Nucleus)
                || atom.math_text
                || atom.sup.is_some()
                || atom.sub.is_some()
                || index + 1 >= atoms.len()
            {
                return Ok(());
            }
            let Some((family, left)) = char_field(&atom.nucleus) else {
                return Ok(());
            };
            let following = &atoms[index + 1];
            let Some((right_family, right)) = char_field(&following.nucleus) else {
                return Ok(());
            };
            if !matches!(following.command, MathCommand::Nucleus)
                || following.kind == AtomKind::Inner
                || family != right_family
            {
                return Ok(());
            }
            let identity = |field: &Field| {
                field
                    .as_ref()
                    .map_or(0, |field| Arc::as_ptr(field) as usize)
            };
            guard.visit(
                (
                    family,
                    left,
                    right,
                    following.kind,
                    identity(&following.sup),
                    identity(&following.sub),
                ),
                atoms.len() - index - 1,
            )?;
            atoms[index].math_text = true;
            let font = MathFamilies::family(context.typesetting.env, style.font_size(), family);
            let Some(matched) = try_lig_kern(&context.typesetting.fonts.items[font], left, right)?
            else {
                return Ok(());
            };
            if let Some(kern) = matched.kern {
                let pointer = context.typesetting.builder.new_kern(kern)?;
                atoms.insert(
                    index + 1,
                    Atom {
                        command: MathCommand::Raw(pointer),
                        ..Atom::default()
                    },
                );
                return Ok(());
            }
            let Some(code) = matched.character else {
                return Ok(());
            };
            let behavior = matched
                .operation
                .ok_or_else(|| Error::value("Missing math ligature operation"))?
                .behavior();
            let mut origins = atoms[index].nucleus.as_ref().unwrap().origins().to_vec();
            for origin in atoms[index + 1].nucleus.as_ref().unwrap().origins() {
                if !origins.contains(origin) {
                    origins.push(origin.clone());
                }
            }
            let replacement = || {
                Some(Arc::new(MathField::Character {
                    family,
                    code,
                    origins: origins.clone(),
                }))
            };
            match behavior.action {
                LigatureAction::ReplaceLeft => atoms[index].nucleus = replacement(),
                LigatureAction::ReplaceRight => atoms[index + 1].nucleus = replacement(),
                LigatureAction::InsertBetween => atoms.insert(
                    index + 1,
                    Atom {
                        nucleus: replacement(),
                        math_text: behavior.inserted_math_text,
                        ..Atom::default()
                    },
                ),
                LigatureAction::ReplaceBoth => {
                    let following = atoms.remove(index + 1);
                    atoms[index].nucleus = replacement();
                    atoms[index].sup = following.sup;
                    atoms[index].sub = following.sub;
                }
            }
            if behavior.finish_math_pair {
                return Ok(());
            }
            atoms[index].math_text = false;
        }
    }
    fn clean_begin(
        &mut self,
        context: &mut MathContext<'_, '_>,
        field: Field,
        style: MathStyle,
    ) -> Result<()> {
        let Some(field) = field else {
            self.complete(MathValue::Box(context.typesetting.builder.new_null_box()?));
            return Ok(());
        };
        let atoms = match field.as_ref() {
            MathField::Box(pointer) => {
                let pointer = if *pointer == C::NULL {
                    context.typesetting.builder.new_null_box()?
                } else if context.typesetting.builder.node(*pointer)?.shift != 0 {
                    context.hbox(&[*pointer])?
                } else {
                    *pointer
                };
                self.complete(MathValue::Box(pointer));
                return Ok(());
            }
            MathField::Character { .. } => vec![Atom {
                nucleus: Some(field.clone()),
                ..Atom::default()
            }],
            MathField::List(atoms) => atoms.clone(),
        };
        self.defer(
            Request::Render {
                atoms,
                style,
                penalties: false,
                allow_middles: false,
            },
            |renderer, context, value| {
                let pointers = value.nodes()?.nodes;
                let result = if pointers.len() == 1
                    && matches!(
                        context.typesetting.builder.node(pointers[0])?.node_type,
                        NodeKind::Hlist | NodeKind::Vlist
                    )
                    && context.typesetting.builder.node(pointers[0])?.shift == 0
                {
                    pointers[0]
                } else {
                    context.hbox(&pointers)?
                };
                let children = context
                    .typesetting
                    .builder
                    .siblings(context.typesetting.builder.node(result)?.list_ptr)?;
                if children.len() == 2
                    && context.typesetting.builder.node(children[0])?.node_type == NodeKind::Char
                    && context.typesetting.builder.node(children[1])?.node_type == NodeKind::Kern
                {
                    context.typesetting.builder.node_mut(children[0])?.link = C::NULL;
                    context.typesetting.builder.release(children[1], false)?;
                }
                renderer.complete(MathValue::Box(result));
                Ok(())
            },
        );
        Ok(())
    }
    fn fraction_begin(
        &mut self,
        context: &mut MathContext<'_, '_>,
        atom: Atom,
        style: MathStyle,
    ) -> Result<()> {
        let MathCommand::Fraction {
            thickness,
            left,
            right,
            denominator,
        } = atom.command
        else {
            return Err(Error::value("Expected fraction noad"));
        };
        let thickness = thickness.unwrap_or(context.param(8, style, 3)?);
        self.defer(
            Request::Clean(atom.nucleus, style.numerator()),
            move |renderer, _, value| {
                let num = value.box_pointer()?;
                renderer.defer(
                    Request::Clean(denominator, style.denominator()),
                    move |renderer, context, value| {
                        let den = value.box_pointer()?;
                        let width = context
                            .typesetting
                            .builder
                            .node(num)?
                            .width
                            .max(context.typesetting.builder.node(den)?.width);
                        let num = context.rebox(num, width)?;
                        let den = context.rebox(den, width)?;
                        let n = context.typesetting.builder.node(num)?.clone();
                        let d = context.typesetting.builder.node(den)?.clone();
                        let mut up = context.param(
                            if style.is_display() {
                                8
                            } else if thickness != 0 {
                                9
                            } else {
                                10
                            },
                            style,
                            2,
                        )?;
                        let mut down =
                            context.param(if style.is_display() { 11 } else { 12 }, style, 2)?;
                        let rule = context.param(8, style, 3)?;
                        let axis = context.param(22, style, 2)?;
                        let mut middle = Vec::new();
                        if thickness == 0 {
                            let delta = half(
                                if style.is_display() { 7 } else { 3 } * rule
                                    - ((up - n.depth) - (d.height - down)),
                            );
                            if delta > 0 {
                                up += delta;
                                down += delta;
                            }
                            middle.push(
                                context
                                    .typesetting
                                    .builder
                                    .new_kern(up - n.depth - d.height + down)?,
                            );
                        } else {
                            let clear = if style.is_display() { 3 } else { 1 } * thickness;
                            let delta = half(thickness);
                            up += (clear - ((up - n.depth) - (axis + delta))).max(0);
                            down += (clear - ((axis - delta) - (d.height - down))).max(0);
                            middle.push(
                                context
                                    .typesetting
                                    .builder
                                    .new_kern(up - n.depth - axis - delta)?,
                            );
                            middle.push(context.rule(width, thickness)?);
                            middle.push(
                                context
                                    .typesetting
                                    .builder
                                    .new_kern(axis - delta - d.height + down)?,
                            );
                        }
                        let mut parts = vec![num];
                        parts.extend(middle);
                        parts.push(den);
                        let pointer = context.vbox(&parts)?;
                        let node = context.typesetting.builder.node_mut(pointer)?;
                        node.height = up + n.height;
                        node.depth = d.depth + down;
                        let desired =
                            context.param(if style.is_display() { 20 } else { 21 }, style, 2)?;
                        let left = context.delimiter_box(left, style, desired)?;
                        let right = context.delimiter_box(right, style, desired)?;
                        let result = context.hbox(&[left, pointer, right])?;
                        renderer.complete(nodes_value(vec![result]));
                        Ok(())
                    },
                );
                Ok(())
            },
        );
        Ok(())
    }
    fn scripts_begin(
        &mut self,
        context: &mut MathContext<'_, '_>,
        atom: Atom,
        pointers: Vec<i64>,
        delta: i64,
        style: MathStyle,
    ) -> Result<()> {
        let (mut up, down) = if pointers
            .first()
            .map(|&pointer| {
                context
                    .typesetting
                    .builder
                    .node(pointer)
                    .map(|node| node.node_type == NodeKind::Char)
            })
            .transpose()?
            .unwrap_or(false)
        {
            (0, 0)
        } else {
            let temporary = context.hbox(&pointers)?;
            let node = context.typesetting.builder.node(temporary)?.clone();
            let small = if style.is_script() {
                MathStyle::Scriptscript
            } else {
                MathStyle::Script
            };
            let values = (
                node.height - context.param(18, small, 2)?,
                node.depth + context.param(19, small, 2)?,
            );
            context.typesetting.builder.node_mut(temporary)?.list_ptr = C::NULL;
            context.typesetting.builder.release(temporary, false)?;
            values
        };
        let xheight = context.param(5, style, 2)?.abs();
        let space = parameter(context.typesetting.env, "scriptspace", 0);
        if atom.sup.is_none() {
            self.defer(
                Request::Clean(atom.sub, style.subscript()),
                move |renderer, context, value| {
                    let pointer = value.box_pointer()?;
                    let shift = down
                        .max(context.param(16, style, 2)?)
                        .max(context.typesetting.builder.node(pointer)?.height - 4 * xheight / 5);
                    let node = context.typesetting.builder.node_mut(pointer)?;
                    node.width += space;
                    node.shift = shift;
                    let mut pointers = pointers;
                    pointers.push(pointer);
                    renderer.complete(nodes_value(pointers));
                    Ok(())
                },
            );
        } else {
            self.defer(
                Request::Clean(atom.sup, style.superscript()),
                move |renderer, context, value| {
                    let pointer = value.box_pointer()?;
                    context.typesetting.builder.node_mut(pointer)?.width += space;
                    let sup = context.typesetting.builder.node(pointer)?.clone();
                    up = up
                        .max(context.param(
                            if style.is_cramped() {
                                15
                            } else if style.is_display() {
                                13
                            } else {
                                14
                            },
                            style,
                            2,
                        )?)
                        .max(sup.depth + xheight / 4);
                    if atom.sub.is_none() {
                        context.typesetting.builder.node_mut(pointer)?.shift = -up;
                        let mut pointers = pointers;
                        pointers.push(pointer);
                        renderer.complete(nodes_value(pointers));
                    } else {
                        renderer.defer(
                            Request::Clean(atom.sub, style.subscript()),
                            move |renderer, context, value| {
                                let sub_pointer = value.box_pointer()?;
                                context.typesetting.builder.node_mut(sub_pointer)?.width += space;
                                let sub = context.typesetting.builder.node(sub_pointer)?.clone();
                                let mut down = down.max(context.param(17, style, 2)?);
                                let clearance = 4 * context.param(8, style, 3)?
                                    - ((up - sup.depth) - (sub.height - down));
                                if clearance > 0 {
                                    down += clearance;
                                    let extra = 4 * xheight / 5 - (up - sup.depth);
                                    if extra > 0 {
                                        up += extra;
                                        down -= extra;
                                    }
                                }
                                context.typesetting.builder.node_mut(pointer)?.shift = delta;
                                let gap = context
                                    .typesetting
                                    .builder
                                    .new_kern(up - sup.depth - sub.height + down)?;
                                let pointer = context.vbox(&[pointer, gap, sub_pointer])?;
                                context.typesetting.builder.node_mut(pointer)?.shift = down;
                                let mut pointers = pointers;
                                pointers.push(pointer);
                                renderer.complete(nodes_value(pointers));
                                Ok(())
                            },
                        );
                    }
                    Ok(())
                },
            );
        }
        Ok(())
    }
    fn atom_regular(
        &mut self,
        context: &mut MathContext<'_, '_>,
        atom: Atom,
        style: MathStyle,
    ) -> Result<()> {
        if atom.kind == AtomKind::Operator {
            self.operations.push(Operation::Request(Request::Operator(
                atom.clone(),
                atom.nucleus,
                style,
            )));
            return Ok(());
        }
        if let Some((family, code)) = char_field(&atom.nucleus) {
            let (pointer, mut delta) = context.character(family, code, style)?;
            if pointer != C::NULL {
                context.typesetting.builder.node_mut(pointer)?.origins = atom
                    .nucleus
                    .as_ref()
                    .unwrap()
                    .origins()
                    .iter()
                    .cloned()
                    .map(Some)
                    .collect();
            }
            let mut pointers = if pointer == C::NULL {
                Vec::new()
            } else {
                vec![pointer]
            };
            if pointer != C::NULL && atom.math_text {
                let font = context.typesetting.builder.node(pointer)?.font;
                if context.typesetting.fonts.items[font].get_parameter(2) != 0 {
                    delta = 0;
                }
            }
            if delta != 0 && atom.sub.is_none() {
                pointers.push(context.typesetting.builder.new_kern(delta)?);
                delta = 0;
            }
            self.finish_atom(atom, pointers, delta, style);
            return Ok(());
        }
        if atom.nucleus.is_none() {
            self.finish_atom(atom, Vec::new(), 0, style);
        } else {
            self.defer(
                Request::Clean(atom.nucleus.clone(), style),
                move |renderer, _, value| {
                    renderer.finish_atom(atom, vec![value.box_pointer()?], 0, style);
                    Ok(())
                },
            );
        }
        Ok(())
    }
    fn finish_atom(&mut self, atom: Atom, pointers: Vec<i64>, delta: i64, style: MathStyle) {
        if atom.sup.is_some() || atom.sub.is_some() {
            self.operations.push(Operation::Request(Request::Scripts(
                atom, pointers, delta, style,
            )));
        } else {
            self.complete(nodes_value(pointers));
        }
    }
    fn atom_begin(
        &mut self,
        context: &mut MathContext<'_, '_>,
        mut atom: Atom,
        style: MathStyle,
    ) -> Result<()> {
        // Generated rules/delimiters inherit the owning construct. Children
        // keep their own precise character origins.
        if let Some(source) = atom.source.clone() {
            self.operations.push(Operation::Resume(Box::new(
                move |renderer, context, value| {
                    let nodes = value.nodes()?;
                    let mut pending = nodes.nodes.clone();
                    let mut seen = BTreeSet::new();
                    while let Some(pointer) = pending.pop() {
                        if pointer == C::NULL || !seen.insert(pointer) {
                            continue;
                        }
                        let node = context.typesetting.builder.node(pointer)?.clone();
                        if node.list_ptr != C::NULL {
                            pending.extend(context.typesetting.builder.siblings(node.list_ptr)?);
                        }
                        if node.origins.is_empty()
                            && matches!(
                                node.node_type,
                                NodeKind::Char | NodeKind::Ligature | NodeKind::Rule
                            )
                        {
                            context.typesetting.builder.node_mut(pointer)?.origins =
                                smallvec::smallvec![Some(source.clone())];
                        }
                    }
                    renderer.complete(MathValue::Nodes(nodes));
                    Ok(())
                },
            )));
        }
        match atom.command.clone() {
            MathCommand::Fraction { .. } => self
                .operations
                .push(Operation::Request(Request::Fraction(atom, style))),
            MathCommand::Delimited { left, right } => {
                let atoms = match atom.nucleus.as_deref() {
                    Some(MathField::List(atoms)) => atoms.clone(),
                    _ => return Err(Error::value("Delimited atom without inner list")),
                };
                self.defer(
                    Request::Render {
                        atoms,
                        style,
                        penalties: false,
                        allow_middles: true,
                    },
                    move |renderer, context, value| {
                        let mut inner = value.nodes()?;
                        let temporary = context.hbox(&inner.nodes)?;
                        let node = context.typesetting.builder.node(temporary)?.clone();
                        let axis = context.param(22, style, 2)?;
                        let extent = (node.height - axis).max(node.depth + axis);
                        let desired = (trunc_div(extent as i128, 500)? as i64
                            * parameter(context.typesetting.env, "delimiterfactor", 901))
                        .max(
                            2 * extent
                                - parameter(context.typesetting.env, "delimitershortfall", 0),
                        );
                        context.typesetting.builder.node_mut(temporary)?.list_ptr = C::NULL;
                        context.typesetting.builder.release(temporary, false)?;
                        for (pointer, code) in inner.middles {
                            let replacement = context.delimiter_box(code, style, desired)?;
                            for item in &mut inner.nodes {
                                if *item == pointer {
                                    *item = replacement;
                                }
                            }
                            context.typesetting.builder.release(pointer, false)?;
                        }
                        let mut pointers = vec![context.delimiter_box(left, style, desired)?];
                        pointers.extend(inner.nodes);
                        pointers.push(context.delimiter_box(right, style, desired)?);
                        let group = context.hbox(&pointers)?;
                        renderer.finish_atom(atom, vec![group], 0, style);
                        Ok(())
                    },
                );
            }
            MathCommand::Radical(delimiter) => {
                self.defer(
                    Request::Clean(atom.nucleus.clone(), style.cramped()),
                    move |renderer, context, value| {
                        let pointer = value.box_pointer()?;
                        let node = context.typesetting.builder.node(pointer)?.clone();
                        let rule = context.param(8, style, 3)?;
                        let mut clear = rule
                            + if style.is_display() {
                                context.param(5, style, 2)?.abs() / 4
                            } else {
                                rule.abs() / 4
                            };
                        let symbol = context.delimiter_box(
                            delimiter,
                            style,
                            node.height + node.depth + clear + rule,
                        )?;
                        let symbol_node = context.typesetting.builder.node(symbol)?.clone();
                        let excess = symbol_node.depth - (node.height + node.depth + clear);
                        if excess > 0 {
                            clear += half(excess);
                        }
                        context.typesetting.builder.node_mut(symbol)?.shift =
                            -(node.height + clear);
                        let bar = context.overbar(pointer, clear, symbol_node.height)?;
                        let nucleus = context.hbox(&[symbol, bar])?;
                        atom.nucleus = Some(Arc::new(MathField::Box(nucleus)));
                        atom.command = MathCommand::Nucleus;
                        renderer.atom_regular(context, atom, style)
                    },
                );
            }
            MathCommand::Overline => {
                self.defer(
                    Request::Clean(atom.nucleus.clone(), style.cramped()),
                    move |renderer, context, value| {
                        let thickness = context.param(8, style, 3)?;
                        let pointer =
                            context.overbar(value.box_pointer()?, 3 * thickness, thickness)?;
                        atom.nucleus = Some(Arc::new(MathField::Box(pointer)));
                        atom.command = MathCommand::Nucleus;
                        renderer.atom_regular(context, atom, style)
                    },
                );
            }
            MathCommand::Underline => {
                self.defer(
                    Request::Clean(atom.nucleus.clone(), style),
                    move |renderer, context, value| {
                        let pointer = value.box_pointer()?;
                        let node = context.typesetting.builder.node(pointer)?.clone();
                        let rule = context.param(8, style, 3)?;
                        let gap = context.typesetting.builder.new_kern(3 * rule)?;
                        let rule_pointer = context.rule(node.width, rule)?;
                        let result = context.vbox(&[pointer, gap, rule_pointer])?;
                        let box_node = context.typesetting.builder.node_mut(result)?;
                        let total = box_node.height + box_node.depth + rule;
                        box_node.height = node.height;
                        box_node.depth = total - node.height;
                        atom.nucleus = Some(Arc::new(MathField::Box(result)));
                        atom.command = MathCommand::Nucleus;
                        renderer.atom_regular(context, atom, style)
                    },
                );
            }
            MathCommand::Vcenter => {
                let Some(MathField::Box(pointer)) = atom.nucleus.as_deref() else {
                    return Err(Error::value("Vcenter needs a box"));
                };
                let axis = context.param(22, style, 2)?;
                let node = context.typesetting.builder.node_mut(*pointer)?;
                let total = node.height + node.depth;
                node.height = axis + half(total);
                node.depth = total - node.height;
                self.atom_regular(context, atom, style)?;
            }
            MathCommand::Accent(_) => {
                let request = Request::Accent(atom.clone(), style);
                if char_field(&atom.nucleus).is_some() {
                    atom.sup = None;
                    atom.sub = None;
                }
                self.defer(request, move |renderer, context, value| {
                    atom.nucleus = value.field()?;
                    atom.command = MathCommand::Nucleus;
                    renderer.atom_regular(context, atom, style)
                });
            }
            _ => self.atom_regular(context, atom, style)?,
        }
        Ok(())
    }
    fn accent_begin(
        &mut self,
        context: &mut MathContext<'_, '_>,
        atom: Atom,
        style: MathStyle,
    ) -> Result<()> {
        let MathCommand::Accent(code) = atom.command else {
            return Err(Error::value("Expected accent noad"));
        };
        let accent = MathFamilies::char_field(context.typesetting.env, code);
        let MathField::Character { family, code, .. } = accent else {
            unreachable!()
        };
        let font = MathFamilies::family(context.typesetting.env, style.font_size(), family);
        let mut skew = 0;
        if let Some((family, character)) = char_field(&atom.nucleus) {
            let base = MathFamilies::family(context.typesetting.env, style.font_size(), family);
            let info = &context.typesetting.fonts.items[base];
            skew = try_lig_kern(info, character, info.skewchar as u32)?
                .and_then(|matched| matched.kern)
                .unwrap_or(0);
        }
        self.defer(
            Request::Clean(atom.nucleus.clone(), style.cramped()),
            move |renderer, context, value| {
                let pointer = value.box_pointer()?;
                let node = context.typesetting.builder.node(pointer)?.clone();
                let info = &context.typesetting.fonts.items[font];
                let mut code = code;
                let mut seen = BTreeSet::new();
                loop {
                    let next = info
                        .tfm_data
                        .as_ref()
                        .and_then(|tfm| tfm.char_tags.get(&code))
                        .copied()
                        .unwrap_or((CharacterTag::None, 0));
                    if next.0 != CharacterTag::NextLarger {
                        break;
                    }
                    if !seen.insert(code) {
                        return Err(Error::value("Cyclic accent variants"));
                    }
                    let candidate = next.1 as u32;
                    if !info.has_character(candidate) || info.get_char_width(candidate) > node.width
                    {
                        break;
                    }
                    code = candidate;
                }
                let remove = node.height.min(info.get_parameter(5));
                if char_field(&atom.nucleus).is_some() && (atom.sup.is_some() || atom.sub.is_some())
                {
                    context.typesetting.builder.release(pointer, false)?;
                    let field = Some(Arc::new(MathField::List(vec![Atom {
                        nucleus: atom.nucleus,
                        sup: atom.sup,
                        sub: atom.sub,
                        ..Atom::default()
                    }])));
                    renderer.defer(
                        Request::Clean(field, style),
                        move |renderer, context, value| {
                            let pointer = value.box_pointer()?;
                            let height = context.typesetting.builder.node(pointer)?.height;
                            let result = Self::finish_accent(
                                context,
                                pointer,
                                font,
                                code,
                                skew,
                                node.width,
                                height,
                                remove + height - node.height,
                            )?;
                            renderer
                                .complete(MathValue::Field(Some(Arc::new(MathField::Box(result)))));
                            Ok(())
                        },
                    );
                } else {
                    let result = Self::finish_accent(
                        context,
                        pointer,
                        font,
                        code,
                        skew,
                        node.width,
                        node.height,
                        remove,
                    )?;
                    renderer.complete(MathValue::Field(Some(Arc::new(MathField::Box(result)))));
                }
                Ok(())
            },
        );
        Ok(())
    }
    fn finish_accent(
        context: &mut MathContext<'_, '_>,
        pointer: i64,
        font: usize,
        code: u32,
        skew: i64,
        width: i64,
        height: i64,
        remove: i64,
    ) -> Result<i64> {
        let info = &context.typesetting.fonts.items[font];
        let accent_width = info.get_char_width(code) + info.get_char_italic(code);
        let character =
            context
                .typesetting
                .builder
                .new_char_node(context.typesetting.fonts, font, code)?;
        let accent = context.hbox(&[character])?;
        let node = context.typesetting.builder.node_mut(accent)?;
        node.shift = skew + half(width - accent_width);
        node.width = 0;
        let gap = context.typesetting.builder.new_kern(-remove)?;
        let result = context.vbox(&[accent, gap, pointer])?;
        let width = context.typesetting.builder.node(pointer)?.width;
        let node = context.typesetting.builder.node(result)?.clone();
        context.typesetting.builder.node_mut(result)?.width = width;
        if node.height < height {
            let first = context.typesetting.builder.new_kern(height - node.height)?;
            context.typesetting.builder.node_mut(first)?.link = node.list_ptr;
            let node = context.typesetting.builder.node_mut(result)?;
            node.list_ptr = first;
            node.height = height;
        }
        Ok(result)
    }
    fn operator_begin(
        &mut self,
        context: &mut MathContext<'_, '_>,
        atom: Atom,
        nucleus: Field,
        style: MathStyle,
    ) -> Result<()> {
        let limits = atom.limits == 1 || atom.limits == 0 && style.is_display();
        let mut delta = 0;
        let mut field = nucleus;
        let character = if let Some((family, mut code)) = char_field(&field) {
            let font = MathFamilies::family(context.typesetting.env, style.font_size(), family);
            let info = &context.typesetting.fonts.items[font];
            if style.is_display() {
                if let Some(&(CharacterTag::NextLarger, next)) = info
                    .tfm_data
                    .as_ref()
                    .and_then(|tfm| tfm.char_tags.get(&code))
                {
                    code = next as u32;
                }
            }
            delta = info.get_char_italic(code);
            let origins = field
                .as_ref()
                .map(|f| f.origins().to_vec())
                .unwrap_or_default();
            field = Some(Arc::new(MathField::Character {
                family,
                code,
                origins,
            }));
            true
        } else {
            false
        };
        self.defer(
            Request::Clean(field, style),
            move |renderer, context, value| {
                let mut pointer = value.box_pointer()?;
                if character {
                    let axis = context.param(22, style, 2)?;
                    let node = context.typesetting.builder.node_mut(pointer)?;
                    if atom.sub.is_some() && !limits {
                        node.width -= delta;
                    }
                    node.shift = half(node.height - node.depth) - axis;
                }
                if !limits {
                    renderer.finish_atom(atom, vec![pointer], delta, style);
                    return Ok(());
                }
                if context.typesetting.builder.node(pointer)?.shift != 0 {
                    pointer = context.hbox(&[pointer])?;
                }
                renderer.defer(
                    Request::Clean(atom.sup.clone(), style.superscript()),
                    move |renderer, _, value| {
                        let sup = value.box_pointer()?;
                        renderer.defer(
                            Request::Clean(atom.sub.clone(), style.subscript()),
                            move |renderer, context, value| {
                                let sub = value.box_pointer()?;
                                let width = [pointer, sup, sub]
                                    .into_iter()
                                    .map(|pointer| {
                                        context
                                            .typesetting
                                            .builder
                                            .node(pointer)
                                            .map(|node| node.width)
                                    })
                                    .collect::<Result<Vec<_>>>()?
                                    .into_iter()
                                    .max()
                                    .unwrap();
                                let pointer = context.rebox(pointer, width)?;
                                let sup = context.rebox(sup, width)?;
                                let sub = context.rebox(sub, width)?;
                                context.typesetting.builder.node_mut(sup)?.shift = half(delta);
                                context.typesetting.builder.node_mut(sub)?.shift = -half(delta);
                                let top = context.typesetting.builder.node(sup)?.clone();
                                let bottom = context.typesetting.builder.node(sub)?.clone();
                                let center = context.typesetting.builder.node(pointer)?;
                                let (mut height, mut depth) = (center.height, center.depth);
                                let mut parts = Vec::new();
                                let pad = context.param(13, style, 3)?;
                                if atom.sup.is_some() {
                                    let gap = (context.param(11, style, 3)? - top.depth)
                                        .max(context.param(9, style, 3)?);
                                    parts.push(context.typesetting.builder.new_kern(pad)?);
                                    parts.push(sup);
                                    parts.push(context.typesetting.builder.new_kern(gap)?);
                                    height += pad + top.height + top.depth + gap;
                                } else {
                                    context.typesetting.builder.release(sup, false)?;
                                }
                                parts.push(pointer);
                                if atom.sub.is_some() {
                                    let gap = (context.param(12, style, 3)? - bottom.height)
                                        .max(context.param(10, style, 3)?);
                                    parts.push(context.typesetting.builder.new_kern(gap)?);
                                    parts.push(sub);
                                    parts.push(context.typesetting.builder.new_kern(pad)?);
                                    depth += pad + bottom.height + bottom.depth + gap;
                                } else {
                                    context.typesetting.builder.release(sub, false)?;
                                }
                                let result = context.vbox(&parts)?;
                                let node = context.typesetting.builder.node_mut(result)?;
                                node.width = width;
                                node.height = height;
                                node.depth = depth;
                                renderer.complete(nodes_value(vec![result]));
                                Ok(())
                            },
                        );
                        Ok(())
                    },
                );
                Ok(())
            },
        );
        Ok(())
    }
}
