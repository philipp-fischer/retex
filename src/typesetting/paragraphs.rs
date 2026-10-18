//! Active/passive line breaking and transfer of alternatives (`paragraphs.py`).
use super::font_expansion::{list_capacity, node_capacity, node_capacity_parts};
use super::hyphenation::{Hyphenation, WordReplacement};
use super::layout::{Layout, TypesettingContext};
use super::model::Node;
use super::nodes::{glue_parameter, parameter};
use super::protrusion;
use crate::core::characters::LanguageProfile;
use crate::core::constants::Constants as C;
use crate::core::enums::NodeKind;
use crate::core::errors::{Error, Result};
use crate::core::values::{GlueSpec, MAX_DIMEN, badness};
use crate::language::expressions::quotient;
use crate::language::records::{Quantity, ValueIndex, ValueKey};
use crate::language::state::Environment;
use crate::typesetting::model::NodeFields;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum BreakKind {
    #[default]
    Glue,
    Math,
    Penalty,
    Discretionary,
    Kern,
    End,
}
impl BreakKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Glue => "glue",
            Self::Math => "math",
            Self::Penalty => "penalty",
            Self::Discretionary => "discretionary",
            Self::Kern => "kern",
            Self::End => "end",
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Break {
    pub position: usize,
    pub end: usize,
    pub start: usize,
    pub penalty: i64,
    pub hyphenated: bool,
    pub pre: Vec<i64>,
    pub post: Vec<i64>,
    pub kind: BreakKind,
}
#[derive(Clone, Debug)]
pub struct Active {
    pub br: Option<Break>,
    pub lines: i64,
    pub fitness: i64,
    pub demerits: i64,
    pub previous: Option<Arc<Active>>,
    pub badness: i64,
    pub artificial: bool,
    pub shortfall: i64,
    pub glue_capacity: i64,
    pub last_adjustment: i64,
}
impl Default for Active {
    fn default() -> Self {
        Self {
            br: None,
            lines: 0,
            fitness: 2,
            demerits: 0,
            previous: None,
            badness: 0,
            artificial: false,
            shortfall: 0,
            glue_capacity: 0,
            last_adjustment: 0,
        }
    }
}
#[derive(Clone, Debug)]
pub enum ParagraphEvent {
    Break {
        line: usize,
        kind: BreakKind,
        penalty: i64,
        hyphenated: bool,
        node: Option<i64>,
    },
    Pass {
        name: &'static str,
        threshold: i64,
        emergency: i64,
        final_pass: bool,
        found: bool,
        looseness: i64,
    },
    Line {
        number: i64,
        pass: &'static str,
        state: Arc<Active>,
        indent: i64,
        width: i64,
        packed: Node,
    },
}
pub struct Paragraph {
    pub optimal_mode: bool,
    pub initial_hyphenation: (i64, i64, i64),
    pub items: Vec<i64>,
    pub initial_start: usize,
    pub breaks: Vec<Break>,
    pub prefix: Vec<[i64; 8]>,
    pub alternatives: BTreeMap<Break, (i64, i64)>,
    pub alternative_capacity: BTreeMap<Break, [(i64, i64); 2]>,
    pub expansion_step: i64,
    pub expansion_stretch: i64,
    pub expansion_shrink: i64,
    pub background: [i64; 8],
    pub last_line_fit: bool,
    pub fill_width: [i64; 3],
    pub events: Vec<ParagraphEvent>,
}
fn shape(env: &Environment) -> &[(i64, i64)] {
    match env.values.get(&ValueKey {
        table: "parshape".into(),
        index: ValueIndex::Number(0),
    }) {
        Some(Quantity::Shape(shape)) => shape,
        _ => &[],
    }
}
impl Paragraph {
    pub fn new(context: &mut TypesettingContext<'_>, head: i64, optimal: bool) -> Result<Self> {
        let mut items = context.builder.siblings(head)?;
        let mut initial_start = 0;
        if !optimal {
            while initial_start < items.len()
                && context.builder.node(items[initial_start])?.node_type == NodeKind::Glue
            {
                initial_start += 1;
            }
        } else {
            if items.last().is_some_and(|&pointer| {
                context
                    .builder
                    .node(pointer)
                    .is_ok_and(|node| node.node_type == NodeKind::Glue)
            }) {
                context.builder.release(items.pop().unwrap(), false)?;
                if let Some(&last) = items.last() {
                    context.builder.node_mut(last)?.link = C::NULL;
                }
            }
            items.push(context.builder.allocate(Node::from(NodeFields {
                node_type: NodeKind::Penalty,
                penalty: 10000,
                ..NodeFields::default()
            }))?);
            items.push(context.builder.new_glue(
                glue_parameter(context.env, "parfillskip"),
                "parfillskip",
                context.fonts.current,
            )?);
        }
        let mut paragraph = Self {
            optimal_mode: optimal,
            initial_hyphenation: context.builder.context.initial_hyphenation,
            items,
            initial_start,
            breaks: Vec::new(),
            prefix: Vec::new(),
            alternatives: BTreeMap::new(),
            alternative_capacity: BTreeMap::new(),
            expansion_step: 1,
            expansion_stretch: 0,
            expansion_shrink: 0,
            background: [0; 8],
            last_line_fit: false,
            fill_width: [0; 3],
            events: Vec::new(),
        };
        paragraph.build_breaks(context)?;
        Ok(paragraph)
    }
    pub fn prune(&self, context: &TypesettingContext<'_>, mut start: usize) -> Result<usize> {
        while start < self.items.len() {
            let node = context.builder.node(self.items[start])?;
            if matches!(node.node_type, NodeKind::Glue | NodeKind::Math)
                || node.node_type == NodeKind::Kern && node.kern_kind == "explicit"
                || node.node_type == NodeKind::Penalty && node.penalty > -10000
            {
                start += 1;
            } else {
                break;
            }
        }
        Ok(start)
    }
    pub fn build_breaks(&mut self, context: &TypesettingContext<'_>) -> Result<()> {
        self.breaks.clear();
        let mut auto = true;
        for (index, &pointer) in self.items.iter().enumerate() {
            let node = context.builder.node(pointer)?;
            let followed_by_glue = self
                .items
                .get(index + 1)
                .map(|&pointer| {
                    context
                        .builder
                        .node(pointer)
                        .map(|node| node.node_type == NodeKind::Glue)
                })
                .transpose()?
                .unwrap_or(false);
            let mut br = Break {
                position: index,
                end: index,
                ..Break::default()
            };
            if node.node_type == NodeKind::Math {
                auto = !node.automatic;
                if !auto || !followed_by_glue {
                    continue;
                }
                br.kind = BreakKind::Math;
                br.start = self.prune(context, index + 1)?;
            } else if node.node_type == NodeKind::Penalty && node.penalty < 10000 {
                br.kind = BreakKind::Penalty;
                br.penalty = node.penalty;
                br.start = self.prune(context, index + 1)?;
            } else if node.node_type == NodeKind::Disc {
                br.kind = BreakKind::Discretionary;
                br.pre = context.builder.siblings(node.pre_break)?;
                br.post = context.builder.siblings(node.post_break)?;
                br.penalty = parameter(
                    context.env,
                    if br.pre.is_empty() {
                        "exhyphenpenalty"
                    } else {
                        "hyphenpenalty"
                    },
                    50,
                );
                br.hyphenated = true;
                br.start = if br.post.is_empty() {
                    self.prune(context, index + 1)?
                } else {
                    index + 1
                };
            } else if node.node_type == NodeKind::Glue && index != 0 && auto {
                let previous = context.builder.node(self.items[index - 1])?;
                if !matches!(
                    previous.node_type,
                    NodeKind::Char
                        | NodeKind::Ligature
                        | NodeKind::Hlist
                        | NodeKind::Vlist
                        | NodeKind::Rule
                        | NodeKind::Disc
                        | NodeKind::Whatsit
                        | NodeKind::Mark
                        | NodeKind::Insert
                        | NodeKind::Adjust
                ) && !(previous.node_type == NodeKind::Kern && previous.kern_kind != "explicit")
                {
                    continue;
                }
                br.start = self.prune(context, index + 1)?;
            } else if auto
                && node.node_type == NodeKind::Kern
                && node.kern_kind == "explicit"
                && followed_by_glue
            {
                br.kind = BreakKind::Kern;
                br.start = self.prune(context, index + 1)?;
            } else {
                continue;
            }
            self.breaks.push(br);
        }
        let mut end = self.items.len();
        while !self.optimal_mode
            && end != 0
            && context.builder.node(self.items[end - 1])?.node_type == NodeKind::Glue
        {
            end -= 1;
        }
        self.breaks.push(Break {
            position: self.items.len(),
            end,
            start: self.items.len(),
            penalty: -10000,
            hyphenated: true,
            kind: BreakKind::End,
            ..Break::default()
        });
        Ok(())
    }
    pub fn width(
        &self,
        context: &TypesettingContext<'_>,
        previous: Option<&Break>,
        current: &Break,
    ) -> Result<i64> {
        let start = previous.map_or(self.initial_start, |br| br.start);
        let post = previous.map_or(&[][..], |br| br.post.as_slice());
        self.items[start.min(current.end)..current.end]
            .iter()
            .chain(&current.pre)
            .chain(post)
            .try_fold(0, |total, &pointer| {
                Ok(total + context.builder.node(pointer)?.width)
            })
    }
    pub fn greedy(&mut self, context: &mut TypesettingContext<'_>, hsize: i64) -> Result<Vec<i64>> {
        let mut chosen = Vec::new();
        let mut previous: Option<Break> = None;
        while previous.as_ref().is_none_or(|br| br.kind != BreakKind::End) {
            let choices: Vec<_> = self
                .breaks
                .iter()
                .filter(|br| {
                    previous
                        .as_ref()
                        .is_none_or(|previous| br.position > previous.position)
                        && br.penalty < 10000
                })
                .collect();
            let mut feasible = None;
            let mut selected = None;
            for &candidate in &choices {
                let width = self.width(context, previous.as_ref(), candidate)?;
                if width <= hsize {
                    feasible = Some(candidate);
                }
                if candidate.penalty <= -10000 {
                    selected = Some(if width <= hsize || feasible.is_none() {
                        candidate
                    } else {
                        feasible.unwrap()
                    });
                    break;
                }
                if width > hsize {
                    selected = Some(feasible.unwrap_or(candidate));
                    break;
                }
            }
            let selected = selected
                .or_else(|| choices.last().copied())
                .ok_or_else(|| Error::value("Missing final paragraph break"))?
                .clone();
            previous = Some(selected.clone());
            chosen.push(selected);
        }
        self.materialize(context, &chosen, false)
    }
    pub fn materialize(
        &mut self,
        context: &mut TypesettingContext<'_>,
        chosen: &[Break],
        keep_empty: bool,
    ) -> Result<Vec<i64>> {
        for &pointer in &self.items {
            context.builder.node_mut(pointer)?.link = C::NULL;
        }
        let mut lines = Vec::new();
        let mut retained = BTreeSet::new();
        let mut previous: Option<&Break> = None;
        for br in chosen {
            let mut current = Vec::new();
            if let Some(previous) = previous.filter(|br| !br.post.is_empty()) {
                context
                    .builder
                    .node_mut(self.items[previous.position])?
                    .post_break = C::NULL;
                current.extend_from_slice(&previous.post);
            }
            let start = previous.map_or(self.initial_start, |br| br.start);
            for &pointer in &self.items[start.min(br.end)..br.end] {
                let node = context.builder.node(pointer)?;
                if node.node_type == NodeKind::Disc {
                    current.extend(context.builder.siblings(node.list_ptr)?);
                    context.builder.node_mut(pointer)?.list_ptr = C::NULL;
                } else {
                    current.push(pointer);
                }
            }
            if !br.pre.is_empty() {
                context.builder.node_mut(self.items[br.position])?.pre_break = C::NULL;
                current.extend_from_slice(&br.pre);
            }
            if matches!(br.kind, BreakKind::Kern | BreakKind::Math) {
                let pointer = self.items[br.position];
                context.builder.node_mut(pointer)?.width = 0;
                current.push(pointer);
            }
            for pair in current.windows(2) {
                context.builder.node_mut(pair[0])?.link = pair[1];
            }
            if let Some(&last) = current.last() {
                context.builder.node_mut(last)?.link = C::NULL;
                retained.extend(current.iter().copied());
            }
            if !current.is_empty() || keep_empty {
                lines.push(current.first().copied().unwrap_or(C::NULL));
            }
            self.events.push(ParagraphEvent::Break {
                line: lines.len(),
                kind: br.kind,
                penalty: br.penalty,
                hyphenated: br.hyphenated,
                node: self.items.get(br.position).copied(),
            });
            previous = Some(br);
        }
        for &pointer in &self.items {
            if !retained.contains(&pointer) {
                context.builder.release(pointer, false)?;
            }
        }
        Ok(lines)
    }
    fn metrics(
        context: &mut TypesettingContext<'_>,
        node: &Node,
        previous: Option<&Node>,
        following: Option<&Node>,
    ) -> Result<[i64; 8]> {
        let mut vector = Self::basic_metrics(node)?;
        if parameter(context.env, "pdfadjustspacing", 0) > 1 {
            (vector[6], vector[7]) = node_capacity(context, node, previous, following)?;
        }
        Ok(vector)
    }
    fn basic_metrics(node: &Node) -> Result<[i64; 8]> {
        let mut vector = [0; 8];
        vector[0] = node.width;
        if let Some(spec) = node.glue {
            if spec.shrink != 0 && spec.shrink_order != 0 {
                return Err(Error::value("Infinite glue shrinkage in paragraph")
                    .with_issue(crate::core::recovery::Issue::GlueShrinkage));
            }
            vector[1 + spec.stretch_order as usize] = spec.stretch;
            vector[5] = spec.shrink;
        }
        Ok(vector)
    }
    pub fn line_geometry(env: &Environment, hsize: i64, number: i64) -> (i64, i64) {
        let shape = shape(env);
        if !shape.is_empty() {
            return shape[number.min(shape.len() as i64).max(1) as usize - 1];
        }
        let after = parameter(env, "hangafter", 1);
        let indent = parameter(env, "hangindent", 0);
        let hanging = if after >= 0 {
            number > after
        } else {
            number <= -after
        };
        if hanging {
            (indent.max(0), hsize - indent.abs())
        } else {
            (0, hsize)
        }
    }
    fn easy_line(env: &Environment) -> i64 {
        let shape = shape(env);
        if !shape.is_empty() {
            shape.len() as i64 - 1
        } else if parameter(env, "hangindent", 0) != 0 {
            parameter(env, "hangafter", 1).abs()
        } else {
            0
        }
    }
    fn alternative_width(
        context: &TypesettingContext<'_>,
        pointers: &[i64],
        automatic: bool,
    ) -> Result<i64> {
        let nodes = pointers
            .iter()
            .map(|&pointer| context.builder.node(pointer))
            .collect::<Result<Vec<_>>>()?;
        let width = nodes.iter().map(|node| node.width).sum();
        if !automatic || !context.fonts.exact_repro || nodes.is_empty() {
            return Ok(width);
        }
        let font_id = nodes[0].font;
        if nodes
            .iter()
            .any(|node| node.shaped.is_none() || node.font != font_id)
        {
            return Ok(width);
        }
        let font = &context.fonts.items[font_id];
        let mut text: String = nodes
            .iter()
            .map(|node| node.shaped.as_ref().unwrap().text.as_str())
            .collect();
        if font.has_character(font.hyphenchar as u32)
            && text
                .chars()
                .last()
                .is_some_and(|code| code as i64 == font.hyphenchar)
        {
            text.pop();
            return font.unicode_font.as_ref().unwrap().hyphenated_width(
                &text,
                font.hyphenchar as u32,
                !font.ligatures_disabled,
            );
        }
        Ok(width)
    }
    pub fn measure(&mut self, context: &mut TypesettingContext<'_>) -> Result<()> {
        self.prefix.clear();
        self.prefix.reserve(self.items.len() + 1);
        self.prefix.push([0; 8]);
        let mut policies = Vec::new();
        // Capacity evaluation mutates font variants but not node material.
        // Resolve nodes once and borrow their neighbors. Invalid identities
        // retain the original loop and its error precedence.
        let expand = parameter(context.env, "pdfadjustspacing", 0) > 1;
        let nodes = self
            .items
            .iter()
            .map(|&pointer| context.builder.node(pointer))
            .collect::<Result<Vec<_>>>()
            .ok();
        if let Some(nodes) = nodes {
            for (index, &node) in nodes.iter().enumerate() {
                if expand && node.glyph() {
                    if let Some(policy) = context.fonts.items[node.font].expansion_policy {
                        if !policies.contains(&policy) {
                            policies.push(policy);
                        }
                    }
                }
                let mut metrics = Self::basic_metrics(node)?;
                if expand {
                    (metrics[6], metrics[7]) = node_capacity_parts(
                        context.fonts,
                        context.builder,
                        node,
                        index.checked_sub(1).map(|index| nodes[index]),
                        nodes.get(index + 1).copied(),
                    )?;
                }
                let previous = self.prefix.last().unwrap();
                self.prefix.push(std::array::from_fn(|index| {
                    previous[index] + metrics[index]
                }));
            }
        } else {
            for (index, &pointer) in self.items.iter().enumerate() {
                let node = context.builder.node(pointer)?.clone();
                if parameter(context.env, "pdfadjustspacing", 0) > 1 && node.glyph() {
                    if let Some(policy) = context.fonts.items[node.font].expansion_policy {
                        if !policies.contains(&policy) {
                            policies.push(policy);
                        }
                    }
                }
                let previous = index
                    .checked_sub(1)
                    .map(|index| context.builder.node(self.items[index]).cloned())
                    .transpose()?;
                let following = self
                    .items
                    .get(index + 1)
                    .map(|&pointer| context.builder.node(pointer).cloned())
                    .transpose()?;
                let metrics = Self::metrics(context, &node, previous.as_ref(), following.as_ref())?;
                let previous = self.prefix.last().unwrap();
                self.prefix.push(std::array::from_fn(|index| {
                    previous[index] + metrics[index]
                }));
            }
        }
        self.measure_alternatives(context, policies)
    }
    /// Maintain existing prefix sums during one hyphenation pass. Only the
    /// replacement word's node dimensions changed. Expansion and any uncertain
    /// range use the complete original calculation; no edit state is reused.
    fn measure_replacement(
        &mut self,
        context: &mut TypesettingContext<'_>,
        word: WordReplacement,
    ) -> Result<()> {
        if parameter(context.env, "pdfadjustspacing", 0) > 1 {
            return self.measure(context);
        }
        let new_end = word.begin.checked_add(word.new_len);
        let old_count = self
            .items
            .len()
            .checked_sub(word.new_len)
            .and_then(|count| {
                word.old_end
                    .checked_sub(word.begin)
                    .and_then(|len| count.checked_add(len))
            });
        if new_end.is_none_or(|end| end > self.items.len())
            || old_count.and_then(|count| count.checked_add(1)) != Some(self.prefix.len())
            || word.old_end >= self.prefix.len()
        {
            return self.measure(context);
        }
        let metrics = self.items[word.begin..new_end.unwrap()]
            .iter()
            .map(|&pointer| context.builder.node(pointer).and_then(Self::basic_metrics))
            .collect::<Result<Vec<_>>>();
        let Ok(metrics) = metrics else {
            return self.measure(context);
        };
        let old = self.prefix[word.old_end];
        let tail = self.prefix.split_off(word.old_end + 1);
        self.prefix.truncate(word.begin + 1);
        for metric in metrics {
            let previous = self.prefix.last().unwrap();
            self.prefix
                .push(std::array::from_fn(|index| previous[index] + metric[index]));
        }
        let after = *self.prefix.last().unwrap();
        let delta: [i64; 8] = std::array::from_fn(|index| after[index] - old[index]);
        self.prefix.extend(
            tail.into_iter()
                .map(|entry| std::array::from_fn(|index| entry[index] + delta[index])),
        );
        #[cfg(debug_assertions)]
        {
            // Verify the optimized sums against complete node measurement at
            // every visited word in native debug validation.
            let mut expected = vec![[0i64; 8]];
            for &pointer in &self.items {
                let metric = Self::basic_metrics(context.builder.node(pointer)?)?;
                let previous = expected.last().unwrap();
                expected.push(std::array::from_fn(|index| previous[index] + metric[index]));
            }
            debug_assert_eq!(self.prefix, expected, "Hyphenated word prefix sums");
        }
        self.measure_alternatives(context, Vec::new())
    }
    fn measure_alternatives(
        &mut self,
        context: &mut TypesettingContext<'_>,
        policies: Vec<crate::fonts::expansion::ExpansionPolicy>,
    ) -> Result<()> {
        self.alternatives.clear();
        for br in &self.breaks {
            let automatic = br.kind == BreakKind::Discretionary
                && context.builder.node(self.items[br.position])?.automatic;
            self.alternatives.insert(
                br.clone(),
                (
                    Self::alternative_width(context, &br.pre, automatic)?,
                    Self::alternative_width(context, &br.post, false)?,
                ),
            );
        }
        if let Some(first) = policies.first() {
            let stretches: BTreeSet<_> = policies
                .iter()
                .map(|policy| policy.stretch)
                .filter(|&limit| limit != 0)
                .collect();
            let shrinks: BTreeSet<_> = policies
                .iter()
                .map(|policy| policy.shrink)
                .filter(|&limit| limit != 0)
                .collect();
            if policies.iter().any(|policy| policy.step != first.step)
                || stretches.len() > 1
                || shrinks.len() > 1
            {
                return Err(Error::value(
                    "Fonts in one paragraph require matching expansion steps and limits",
                )
                .with_issue(crate::core::recovery::Issue::FontExpansion));
            }
            self.expansion_step = first.step;
            self.expansion_stretch = policies.iter().map(|policy| policy.stretch).max().unwrap();
            self.expansion_shrink = policies.iter().map(|policy| policy.shrink).max().unwrap();
        } else {
            self.expansion_step = 1;
            self.expansion_stretch = 0;
            self.expansion_shrink = 0;
        }
        self.alternative_capacity.clear();
        if parameter(context.env, "pdfadjustspacing", 0) > 1 {
            for br in &self.breaks {
                self.alternative_capacity.insert(
                    br.clone(),
                    [
                        list_capacity(context, br.pre.clone())?,
                        list_capacity(context, br.post.clone())?,
                    ],
                );
            }
        }
        Ok(())
    }
    pub fn optimal(
        &mut self,
        layout: &mut Layout,
        context: &mut TypesettingContext<'_>,
        hyphenation: &mut Hyphenation,
        profile: LanguageProfile,
        prev_graf: i64,
    ) -> Result<Vec<i64>> {
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::enabled().then(|| {
            let region = crate::latency_diagnostics::paragraph_region(&self.items, context.builder);
            let guard = crate::latency_diagnostics::phase(
                "paragraph_layout",
                &format!("nodes={} prev_graf={prev_graf}", self.items.len()),
                Some(region),
            );
            crate::latency_diagnostics::count("material_nodes", self.items.len());
            guard
        });
        self.measure(context)?;
        let left = glue_parameter(context.env, "leftskip");
        let right = glue_parameter(context.env, "rightskip");
        let lm = Self::metrics(
            context,
            &Node::from(NodeFields {
                width: left.width,
                glue: Some(left),
                ..NodeFields::default()
            }),
            None,
            None,
        )?;
        let rm = Self::metrics(
            context,
            &Node::from(NodeFields {
                width: right.width,
                glue: Some(right),
                ..NodeFields::default()
            }),
            None,
            None,
        )?;
        self.background = std::array::from_fn(|index| lm[index] + rm[index]);
        let fill = context
            .builder
            .node(*self.items.last().unwrap())?
            .glue
            .ok_or_else(|| Error::value("Missing paragraph fill glue"))?;
        self.last_line_fit = parameter(context.env, "lastlinefit", 0) > 0
            && fill.stretch > 0
            && fill.stretch_order > 0
            && self.background[2..5].iter().all(|&amount| amount == 0);
        self.fill_width = [0; 3];
        if self.last_line_fit {
            self.fill_width[fill.stretch_order as usize - 1] = fill.stretch;
        }
        let mut passes = Vec::new();
        let pretolerance = parameter(context.env, "pretolerance", 100);
        if pretolerance >= 0 {
            passes.push(("pretolerance", pretolerance, 0, false));
        }
        let emergency = parameter(context.env, "emergencystretch", 0).max(0);
        let tolerance = parameter(context.env, "tolerance", 200);
        passes.push(("tolerance", tolerance, 0, emergency == 0));
        if emergency > 0 {
            passes.push(("emergency", tolerance, emergency, true));
        }
        let mut selected = None;
        let mut pass_name = "";
        for (name, threshold, stretch, final_pass) in passes {
            let (best, actual) = self.solve(
                layout,
                context,
                hyphenation,
                profile,
                threshold.min(10000),
                stretch,
                final_pass,
                prev_graf,
                name != "pretolerance",
            )?;
            self.events.push(ParagraphEvent::Pass {
                name,
                threshold,
                emergency: stretch,
                final_pass,
                found: best.is_some(),
                looseness: actual,
            });
            if parameter(context.env, "tracingparagraphs", 0) > 0 {
                crate::resources::host::console_line(
                    true,
                    format!(
                        "Paragraph {name} pass: tolerance {threshold}, emergency stretch {stretch}sp, feasible={}",
                        best.is_some()
                    ),
                );
            }
            pass_name = name;
            selected = best;
            if selected.is_some()
                && (actual == parameter(context.env, "looseness", 0) || final_pass)
            {
                break;
            }
        }
        let best = selected
            .ok_or_else(|| Error::value("Paragraph optimizer failed to retain a final break"))?;
        if self.last_line_fit && best.shortfall != 0 {
            let tail = context.builder.node_mut(*self.items.last().unwrap())?;
            let spec = tail.glue.as_mut().unwrap();
            spec.width += best.shortfall - best.last_adjustment;
            spec.stretch = 0;
            tail.width = spec.width;
        }
        let mut states = Vec::new();
        let mut state = best;
        while state.br.is_some() {
            states.push(state.clone());
            state = state
                .previous
                .clone()
                .ok_or_else(|| Error::value("Missing passive paragraph state"))?;
        }
        states.reverse();
        let chosen: Vec<_> = states
            .iter()
            .map(|state| state.br.clone().unwrap())
            .collect();
        let heads = self.materialize(context, &chosen, true)?;
        let mut boxes = Vec::new();
        for (index, (&initial_head, state)) in heads.iter().zip(&states).enumerate() {
            let number = prev_graf + index as i64 + 1;
            let (left, width) = Self::line_geometry(context.env, layout.hsize, number);
            let mut head = initial_head;
            if parameter(context.env, "pdfprotrudechars", 0) > 0 {
                head = protrusion::add_margins(context, head)?;
            }
            let leftskip = glue_parameter(context.env, "leftskip");
            if leftskip != GlueSpec::default() {
                let pointer =
                    context
                        .builder
                        .new_glue(leftskip, "leftskip", context.fonts.current)?;
                context.builder.node_mut(pointer)?.link = head;
                head = pointer;
            }
            let right = context.builder.new_glue(
                glue_parameter(context.env, "rightskip"),
                "rightskip",
                context.fonts.current,
            )?;
            if head == C::NULL {
                head = right;
            } else {
                let last = *context.builder.siblings(head)?.last().unwrap();
                context.builder.node_mut(last)?.link = right;
            }
            let expand = parameter(context.env, "pdfadjustspacing", 0) > 0;
            let pointer = layout.hpack(context, head, Some(width), false, expand)?;
            let box_node = context.builder.node_mut(pointer)?;
            box_node.shift = left;
            box_node.line_number = number as usize;
            box_node.discretionary_break =
                state.br.as_ref().unwrap().kind == BreakKind::Discretionary;
            let ignored = parameter(context.env, "pdfignoreddimen", -1000 * C::UNITY);
            for (name, height, active) in [
                ("pdfeachlineheight", true, true),
                ("pdfeachlinedepth", false, true),
                ("pdffirstlineheight", true, index == 0),
                ("pdflastlinedepth", false, index + 1 == heads.len()),
            ] {
                let value = parameter(context.env, name, ignored);
                if active && value != ignored {
                    if height {
                        box_node.height = value;
                    } else {
                        box_node.depth = value;
                    }
                }
            }
            boxes.push(pointer);
            self.events.push(ParagraphEvent::Line {
                number,
                pass: pass_name,
                state: state.clone(),
                indent: left,
                width,
                packed: box_node.clone(),
            });
            if parameter(context.env, "tracingparagraphs", 0) > 0 {
                crate::resources::host::console_line(
                    true,
                    format!(
                        "Paragraph line {number}: {}, badness {}, fitness {}, total demerits {}",
                        state.br.as_ref().unwrap().kind.name(),
                        state.badness,
                        state.fitness,
                        state.demerits
                    ),
                );
            }
        }
        Ok(boxes)
    }
    pub fn solve(
        &mut self,
        layout: &Layout,
        context: &mut TypesettingContext<'_>,
        hyphenation: &mut Hyphenation,
        profile: LanguageProfile,
        threshold: i64,
        emergency: i64,
        final_pass: bool,
        prev_graf: i64,
        hyphenating: bool,
    ) -> Result<(Option<Arc<Active>>, i64)> {
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::enabled().then(|| {
            crate::latency_diagnostics::phase(
                "paragraph_solve",
                &format!("hyphenating={hyphenating} threshold={threshold} emergency={emergency}"),
                None,
            )
        });
        let mut active = vec![Arc::new(Active {
            lines: prev_graf,
            ..Active::default()
        })];
        let easy = Self::easy_line(context.env);
        let loose = parameter(context.env, "looseness", 0);
        // No assignments or macro execution occur inside this search. Resolve
        // its scalar parameters and line shape once instead of allocating keys
        // for each active candidate; hyphenation still executes normally.
        let adjust_spacing = parameter(context.env, "pdfadjustspacing", 0) > 1;
        let protrude_chars = parameter(context.env, "pdfprotrudechars", 0) > 1;
        let line_penalty = parameter(context.env, "linepenalty", 10);
        let final_hyphen_demerits = parameter(context.env, "finalhyphendemerits", 0);
        let double_hyphen_demerits = parameter(context.env, "doublehyphendemerits", 0);
        let adjacent_demerits = parameter(context.env, "adjdemerits", 10000);
        let last_line_fit = parameter(context.env, "lastlinefit", 0);
        let line_shape = shape(context.env).to_vec();
        let hang_after = parameter(context.env, "hangafter", 1);
        let hang_indent = parameter(context.env, "hangindent", 0);
        let line_width = |number: i64| {
            if !line_shape.is_empty() {
                line_shape[number.min(line_shape.len() as i64).max(1) as usize - 1].1
            } else if if hang_after >= 0 {
                number > hang_after
            } else {
                number <= -hang_after
            } {
                layout.hsize - hang_indent.abs()
            } else {
                layout.hsize
            }
        };
        let mut language_state = self.initial_hyphenation;
        if hyphenating {
            hyphenation.ready = true;
        }
        let mut cursor = 0;
        let mut auto = true;
        let mut lookup: BTreeMap<_, _> = self
            .breaks
            .iter()
            .map(|br| (br.position, br.clone()))
            .collect();
        loop {
            let node = self
                .items
                .get(cursor)
                .map(|&pointer| {
                    context.builder.node(pointer).map(|node| {
                        (
                            node.node_type,
                            node.whatsit.is_empty(),
                            node.language,
                            node.left_hyphen_min,
                            node.right_hyphen_min,
                            node.automatic,
                        )
                    })
                })
                .transpose()?;
            if let Some((kind, empty_whatsit, language, left_min, right_min, automatic)) = node {
                if kind == NodeKind::Whatsit && empty_whatsit {
                    language_state = (language, left_min, right_min);
                }
                if kind == NodeKind::Math {
                    auto = !automatic;
                }
            }
            if let Some(br) = lookup.get(&cursor).filter(|br| br.penalty < 10000) {
                let br = br.clone();
                let mut survivors = Vec::new();
                let mut best: BTreeMap<(i64, i64), Arc<Active>> = BTreeMap::new();
                for (index, state) in active.iter().enumerate() {
                    let start = state.br.as_ref().map_or(0, |br| br.start);
                    let mut vector: [i64; 8] = std::array::from_fn(|index| {
                        self.prefix[br.end][index] - self.prefix[start][index]
                            + self.background[index]
                    });
                    vector[0] += self.alternatives[&br].0;
                    if let Some(previous) = &state.br {
                        vector[0] += self.alternatives[previous].1;
                    }
                    if adjust_spacing {
                        let capacities = self.alternative_capacity[&br];
                        vector[6] += capacities[0].0;
                        vector[7] += capacities[0].1;
                        if let Some(previous) = &state.br {
                            let capacities = self.alternative_capacity[previous];
                            vector[6] += capacities[1].0;
                            vector[7] += capacities[1].1;
                        }
                    }
                    vector[1] += emergency;
                    let width = line_width(state.lines + 1);
                    let mut difference = width - vector[0];
                    if protrude_chars {
                        let pointers = &self.items[start.min(br.end)..br.end];
                        let left = if let Some(&pointer) =
                            state.br.as_ref().and_then(|br| br.post.first())
                        {
                            Some(context.builder.node(pointer)?.clone())
                        } else {
                            protrusion::edge(context.builder, pointers, false)?
                        };
                        let right = if let Some(&pointer) = br.pre.last() {
                            Some(context.builder.node(pointer)?.clone())
                        } else {
                            protrusion::edge(context.builder, pointers, true)?
                        };
                        difference += protrusion::amount(context.fonts, left.as_ref(), false)?
                            + protrusion::amount(context.fonts, right.as_ref(), true)?;
                    }
                    if adjust_spacing && difference != 0 {
                        let available = vector[if difference > 0 { 6 } else { 7 }];
                        let limit = if difference > 0 {
                            self.expansion_stretch
                        } else {
                            self.expansion_shrink
                        };
                        if available > 0 && limit != 0 {
                            if available > difference.abs() {
                                let adjusted = (available / (limit / self.expansion_step)) / 2;
                                difference = if difference > 0 { adjusted } else { -adjusted };
                            } else {
                                difference += if difference > 0 {
                                    -available
                                } else {
                                    available
                                };
                            }
                        }
                    }
                    let mut shortfall = difference;
                    let mut capacity = if difference > 0 {
                        vector[1]
                    } else if difference < 0 {
                        vector[5]
                    } else {
                        0
                    };
                    let mut adjustment = None;
                    if self.last_line_fit
                        && difference > 0
                        && vector[2..5].iter().any(|&value| value != 0)
                    {
                        if br.kind == BreakKind::End
                            && vector[2..5] == self.fill_width
                            && state.shortfall != 0
                            && state.glue_capacity > 0
                        {
                            let available = if state.shortfall > 0 {
                                vector[1]
                            } else {
                                vector[5]
                            };
                            if available > 0 {
                                let mut amount = (quotient(
                                    available as i128 * state.shortfall as i128,
                                    state.glue_capacity as i128,
                                )? as i64)
                                    .clamp(-MAX_DIMEN, MAX_DIMEN);
                                let fit = last_line_fit;
                                if fit < 1000 {
                                    amount = quotient(amount as i128 * fit as i128, 1000)? as i64;
                                }
                                amount = if amount > 0 {
                                    amount.min(difference)
                                } else {
                                    amount.max(-vector[5])
                                };
                                if amount != 0 {
                                    adjustment = Some(amount);
                                }
                            }
                        }
                        if adjustment.is_none() {
                            shortfall = 0;
                            capacity = 0;
                        }
                    } else if br.kind == BreakKind::End {
                        shortfall = 0;
                        capacity = 0;
                    }
                    let (b, fitness) = if let Some(amount) = adjustment {
                        let b =
                            badness(amount.abs(), if amount > 0 { vector[1] } else { vector[5] });
                        (
                            b,
                            if amount > 0 {
                                if b > 99 {
                                    0
                                } else if b > 12 {
                                    1
                                } else {
                                    2
                                }
                            } else if b > 12 {
                                3
                            } else {
                                2
                            },
                        )
                    } else if difference > 0 {
                        let b = if vector[2..5].iter().any(|&value| value != 0) {
                            0
                        } else {
                            badness(difference, vector[1])
                        };
                        (
                            b,
                            if b > 99 {
                                0
                            } else if b > 12 {
                                1
                            } else {
                                2
                            },
                        )
                    } else {
                        let b = if -difference > vector[5] {
                            10001
                        } else {
                            badness(-difference, vector[5])
                        };
                        (b, if b > 12 { 3 } else { 2 })
                    };
                    let forced = br.penalty <= -10000;
                    let artificial = final_pass
                        && (b > 10000 || forced)
                        && survivors.is_empty()
                        && index + 1 == active.len()
                        && best.is_empty();
                    if !forced && b <= 10000 {
                        survivors.push(state.clone());
                    }
                    if b > threshold && !artificial {
                        continue;
                    }
                    let mut d = 0;
                    if !artificial {
                        d = line_penalty + b;
                        d = if d.abs() >= 10000 { 100_000_000 } else { d * d };
                        if br.penalty > 0 {
                            d += br.penalty * br.penalty;
                        } else if br.penalty > -10000 {
                            d -= br.penalty * br.penalty;
                        }
                        if br.hyphenated && state.br.as_ref().is_some_and(|br| br.hyphenated) {
                            d += if br.kind == BreakKind::End {
                                final_hyphen_demerits
                            } else {
                                double_hyphen_demerits
                            };
                        }
                        if (fitness - state.fitness).abs() > 1 {
                            d += adjacent_demerits;
                        }
                    }
                    let total = state.demerits + d;
                    let group = if loose != 0 {
                        state.lines + 1
                    } else {
                        (state.lines + 1).min(easy)
                    };
                    let key = (group, fitness);
                    if total < 1_073_741_823
                        && best
                            .get(&key)
                            .is_none_or(|previous| total <= previous.demerits)
                    {
                        best.insert(
                            key,
                            Arc::new(Active {
                                br: Some(br.clone()),
                                lines: state.lines + 1,
                                fitness,
                                demerits: total,
                                previous: Some(state.clone()),
                                badness: b,
                                artificial,
                                shortfall,
                                glue_capacity: capacity,
                                last_adjustment: adjustment.unwrap_or(0),
                            }),
                        );
                    }
                }
                let mut champions: BTreeMap<i64, Vec<Arc<Active>>> = BTreeMap::new();
                for group in best.keys().map(|key| key.0).collect::<BTreeSet<_>>() {
                    let minimum = best
                        .iter()
                        .filter(|(key, _)| key.0 == group)
                        .map(|(_, state)| state.demerits)
                        .min()
                        .unwrap();
                    champions.insert(
                        group,
                        (0..4)
                            .filter_map(|fitness| {
                                best.get(&(group, fitness))
                                    .filter(|state| {
                                        state.demerits <= minimum + adjacent_demerits.abs()
                                    })
                                    .cloned()
                            })
                            .collect(),
                    );
                }
                let category = |state: &Active| {
                    if loose != 0 {
                        state.lines + 1
                    } else {
                        (state.lines + 1).min(easy)
                    }
                };
                let groups: BTreeSet<_> = champions
                    .keys()
                    .copied()
                    .chain(survivors.iter().map(|state| category(state)))
                    .collect();
                active.clear();
                for group in groups {
                    active.extend(
                        survivors
                            .iter()
                            .filter(|state| category(state) == group)
                            .cloned(),
                    );
                    if let Some(champions) = champions.remove(&group) {
                        active.extend(champions);
                    }
                }
                if active.is_empty() {
                    return Ok((None, 0));
                }
                if br.kind == BreakKind::End {
                    let base = active.iter().min_by_key(|state| state.demerits).unwrap();
                    let candidates: Vec<_> = active
                        .iter()
                        .filter(|state| {
                            let difference = state.lines - base.lines;
                            if loose >= 0 {
                                0 <= difference && difference <= loose
                            } else {
                                loose <= difference && difference <= 0
                            }
                        })
                        .collect();
                    let desired = if loose >= 0 {
                        candidates.iter().map(|state| state.lines).max()
                    } else {
                        candidates.iter().map(|state| state.lines).min()
                    }
                    .unwrap();
                    let selected = candidates
                        .into_iter()
                        .filter(|state| state.lines == desired)
                        .min_by_key(|state| state.demerits)
                        .unwrap();
                    return Ok((Some(selected.clone()), selected.lines - base.lines));
                }
            }
            let Some((kind, ..)) = node else {
                break;
            };
            if hyphenating && auto && kind == NodeKind::Glue {
                let (state, replacement) = hyphenation.after_glue_replacement(
                    context,
                    profile,
                    &mut self.items,
                    cursor,
                    language_state,
                )?;
                language_state = state;
                if let Some(replacement) = replacement {
                    self.build_breaks(context)?;
                    self.measure_replacement(context, replacement)?;
                    lookup = self
                        .breaks
                        .iter()
                        .map(|br| (br.position, br.clone()))
                        .collect();
                }
            }
            cursor += 1;
        }
        Ok((None, 0))
    }
}
