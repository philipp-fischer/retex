//! Bounded checked substitution of pure paragraph material. This never restores
//! post-output state: the resident engine continues the ordinary revision.
use super::*;
use crate::checked_material::{Observer, Parameter, Shape};
use crate::core::enums::{CharacterCategory as Cat, Command};
use crate::language::input::CurrentToken;
use crate::typesetting::nodes::ListContext;

#[derive(Clone)]
pub(crate) struct Origin {
    pub id: u64,
    pub offset: usize,
    pub source: Arc<[u8]>,
    pub path: String,
    pub safe_start: bool,
    pub direct_par: bool,
    pub fence: bool,
    pub expansion_epoch: u64,
}
#[derive(Clone)]
pub(crate) struct Ready {
    pub origin: Origin,
    pub anchors: Vec<Origin>,
    pub source_lines: (usize, usize),
    pub end: usize,
    pub literal_end: usize,
    pub frontier_context: Option<ListContext>,
    pub tail_reads: Vec<crate::checked_material::TailObservation>,
    pub roots: Vec<i64>,
    pub nodes_count: usize,
    pub font_runs: Vec<Vec<usize>>,
    pub fonts: Vec<super::reuse::FontGuard>,
    pub current_font: usize,
    pub shapes: Vec<Shape>,
    pub penalties: Vec<i64>,
    pub context: ListContext,
    pub badness: i64,
    pub last_line: Option<i64>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub candidate: usize,
}
pub(crate) struct Frontier {
    pub candidate: usize,
    pub anchors: Vec<Origin>,
    pub end: usize,
    pub context: ListContext,
    pub rejected: bool,
}
fn control_context(mut c: ListContext) -> ListContext {
    c.cur_list_head = C::NULL;
    c.cur_list_tail = C::NULL;
    c.cur_q = C::NULL;
    c.cur_l_origins.clear();
    c.lig_origins.clear();
    if let Some(item) = &mut c.cur_item {
        item.origins.clear();
    }
    for item in &mut c.lig_stack {
        item.origins.clear();
    }
    for (_, origin) in &mut c.unicode_run {
        *origin = None;
    }
    c
}
fn literal_anchors(
    anchors: &[Origin],
    input: &InputProcessor,
    env: &Environment,
    end: usize,
    direct: bool,
) -> Vec<Origin> {
    anchors
        .iter()
        .filter(|o| {
            o.safe_start
                && (!direct || o.direct_par)
                && o.expansion_epoch == env.local_expansion_epoch.get()
                && o.source.get(o.offset..end).is_some_and(|fragment| {
                    !fragment.is_empty()
                        && fragment.iter().all(|&b| {
                            if matches!(b, b'\n' | b'\r' | b' ' | b'\t') {
                                return true;
                            }
                            (32..127).contains(&b)
                                && matches!(
                                    input.category_code(env, b as u32),
                                    Ok(Cat::Letter | Cat::OtherChar)
                                )
                        })
                })
        })
        .cloned()
        .collect()
}
#[derive(Default)]
pub(crate) struct Local {
    pub trace_enabled: bool,
    pub trace: Vec<serde_json::Value>,
    pub trace_dropped: usize,
    pub anchors: Vec<Origin>,
    pub frontier: Option<Frontier>,
    pub next_candidate: usize,
    pub parent: Option<Origin>,
    pub fence_seen: bool,
    pub checkpoints: usize,
    pub last_reject: String,
    pub paragraphs: usize,
    pub rejected_commands: std::collections::BTreeMap<String, usize>,
    pub rejected: bool,
    pub evidence: Observer,
    pub ready: Vec<Ready>,
    pub pending: Option<Ready>,
    pub misses: std::collections::BTreeMap<String, usize>,
}
impl Local {
    pub fn trace(&mut self, value: serde_json::Value) {
        if self.trace.len() < 2048 {
            self.trace.push(value);
        } else {
            self.trace_dropped += 1;
        }
    }
    pub fn checkpoint(
        &mut self,
        id: u64,
        offset: usize,
        source: Arc<[u8]>,
        path: String,
        env: &Environment,
        input: &crate::language::input::InputProcessor,
        quiescent: bool,
    ) {
        self.checkpoints += 1;
        if self.anchors.len() >= 64 {
            self.anchors.remove(0);
        }
        self.anchors.push(Origin {
            id,
            offset,
            source,
            path: path.clone(),
            expansion_epoch: env.local_expansion_epoch.get(),
            fence: false,
            direct_par: env
                .symbol_keys
                .get(&input.par_loc)
                .and_then(|key| env.meanings.raw().get(key.as_str()))
                .is_some_and(|m| {
                    m.command
                        == crate::language::records::MeaningCommand::Operation(Command::ParEnd)
                }),
            safe_start: quiescent
                && input.in_open == 1
                && std::iter::once(&input.cursor)
                    .chain(input.input_stack.iter())
                    .any(|f| f.token_list.is_none() && f.name.as_deref() == Some(path.as_str()))
                && std::iter::once(&input.cursor)
                    .chain(input.input_stack.iter())
                    .all(|f| {
                        f.token_list.as_ref().is_none_or(|tokens| {
                            tokens.iter().all(|&t| {
                                matches!(
                                    crate::language::character_tokens::token_category(t),
                                    Some(Cat::Letter | Cat::OtherChar | Cat::Spacer)
                                )
                            })
                        })
                    })
                && input.scanner_status == crate::language::input::ScannerStatus::Normal,
        });
        self.parent = self.anchors.last().cloned();
        self.fence_seen = false;
        self.rejected = false;
        env.local_prefix_armed
            .set(quiescent && self.frontier.is_none());
    }
    pub fn literal_fence(&mut self, input: &InputProcessor, env: &Environment) {
        if self.frontier.is_some() || self.fence_seen {
            return;
        }
        let Some(parent) = self.parent.clone() else {
            return;
        };
        let source = input.cursor.source.as_ref();
        if !source.is_some_and(|s| s.file.as_ref() == parent.path && s.expansion.is_none()) {
            return;
        }
        if !std::iter::once(&input.cursor)
            .chain(input.input_stack.iter())
            .all(|f| {
                f.token_list.as_ref().is_none_or(|tokens| {
                    tokens.iter().all(|&t| {
                        matches!(
                            crate::language::character_tokens::token_category(t),
                            Some(Cat::Letter | Cat::OtherChar | Cat::Spacer)
                        )
                    })
                })
            })
        {
            return;
        }
        let Some(offset) = input
            .input_file
            .first()
            .and_then(Option::as_ref)
            .and_then(|s| s.byte_position())
        else {
            return;
        };
        self.fence_seen = true;
        let mut fence = parent;
        fence.offset = offset;
        fence.expansion_epoch = env.local_expansion_epoch.get();
        fence.safe_start = true;
        fence.fence = true;
        if self.anchors.len() >= 64 {
            self.anchors.remove(0);
        }
        self.anchors.push(fence);
        self.rejected = false;
        env.local_prefix_armed.set(true);
    }
    pub fn command(&mut self, current: &CurrentToken) {
        if let Some(frontier) = &mut self.frontier {
            let allowed =
                matches!(
                    current.category(),
                    Some(Cat::Letter | Cat::OtherChar | Cat::Spacer)
                ) || matches!(current.operation(), Some(Command::ParEnd | Command::Relax))
                    || (current.is_command(Command::RemoveItem)
                        && matches!(
                            &current.value,
                            crate::language::records::CommandOperand::NodeKind(
                                crate::core::enums::NodeKind::Glue
                                    | crate::core::enums::NodeKind::Kern
                                    | crate::core::enums::NodeKind::Penalty
                            )
                        ));
            if !allowed {
                frontier.rejected = true;
            }
            return;
        }
        if self.anchors.is_empty() {
            return;
        }
        if !(matches!(
            current.category(),
            Some(Cat::Letter | Cat::OtherChar | Cat::Spacer)
        ) || current.is_command(Command::ParEnd))
        {
            *self
                .rejected_commands
                .entry(format!("{:?}", current.operation()))
                .or_default() += 1;
            self.last_reject = format!("{:?}:cs={}", current.operation(), current.control_sequence);
            self.rejected = true;
            self.anchors.clear();
        }
    }
    fn miss(&mut self, reason: &str) {
        *self.misses.entry(reason.into()).or_default() += 1;
        if self.trace_enabled {
            self.trace(serde_json::json!({"event":"candidate_rejected","reason":reason,"last_command":self.last_reject}));
        }
    }
}
impl EngineView<'_> {
    pub(super) fn local_paragraph(&mut self, roots: &[i64], penalties: &[i64]) {
        if !self.core.config.exact_local_updates {
            return;
        }
        self.core.local.trace_enabled = self.core.config.exact_local_trace;
        let _validation = self
            .builder
            .nodes
            .contract(crate::checked_material::Consumer::Validation);
        self.core.local.paragraphs += 1;
        let frontier = self.core.local.frontier.take();
        if let Some(f) = &frontier {
            self.core
                .local
                .evidence
                .borrow_mut()
                .tail_open
                .remove(&f.candidate);
            if f.rejected {
                self.core.local.miss("paragraph_end_command");
                return;
            }
        }

        if self.core.config.expansion_limit.is_some() {
            self.core.local.miss("execution_limit");
            return;
        }
        if self.core.config.trace_enabled
            || self.core.trace.observer.is_some()
            || ["tracingcommands", "tracingparagraphs"].iter().any(|name| {
                self.env
                    .values
                    .raw()
                    .get(&ValueKey::parameter(*name))
                    .is_some_and(|q| matches!(q,Quantity::Integer(n) if *n>0))
            })
        {
            self.core.local.miss("content_tracing");
            return;
        }
        let local = &mut self.core.local;
        if local.ready.len() >= 32
            || local.next_candidate >= 128
            || self.fonts.items.len() > 512
            || roots.is_empty()
            || roots.len() > 64
        {
            local.miss("candidate_bound");
            return;
        }
        let Some(end) = self
            .core
            .input
            .input_file
            .first()
            .and_then(Option::as_ref)
            .and_then(|s| s.byte_position())
        else {
            return;
        };
        let literal_end = frontier.as_ref().map_or(end, |f| f.end);
        let anchors = frontier.as_ref().map_or_else(
            || literal_anchors(&local.anchors, &self.core.input, self.env, end, true),
            |f| f.anchors.clone(),
        );
        let Some(origin) = anchors.first().cloned() else {
            local.miss("no_literal_anchor");
            return;
        };
        let candidate = if let Some(f) = &frontier {
            f.candidate
        } else {
            let id = local.next_candidate;
            local.next_candidate += 1;
            id
        };

        let mut tags = std::collections::BTreeMap::new();
        let mut shapes = Vec::new();
        let mut font_runs = Vec::new();
        let mut source_lines = (usize::MAX, 0);
        for (line, &root) in roots.iter().enumerate() {
            let Ok(node) = self.builder.node(root) else {
                return;
            };
            if node.node_type != crate::core::enums::NodeKind::Hlist {
                local.miss("non_line_root");
                return;
            }
            shapes.push(Shape::new(node));
            let mut runs = Vec::new();
            let mut pending = vec![(root, true, false)];
            while let Some((id, is_root, siblings)) = pending.pop() {
                if id == C::NULL {
                    continue;
                }
                if tags.len() >= 8192 || tags.contains_key(&id) {
                    local.miss("graph_bound_or_alias");
                    return;
                }
                if local
                    .evidence
                    .borrow()
                    .tags
                    .get(&id)
                    .is_some_and(|tag| tag.candidate != candidate)
                {
                    local.miss("overlapping_material_parameters");
                    return;
                }
                let Ok(n) = self.builder.node(id) else { return };
                if n.glyph() {
                    if self.fonts.items[n.font].unicode_font.is_some() {
                        local.miss("unicode_material");
                        return;
                    }
                    if runs.last() != Some(&n.font) {
                        runs.push(n.font);
                    }
                }
                if !crate::checked_material::pure(n) {
                    local.miss("material_effect_or_unsupported");
                    return;
                }
                if n.origins.iter().flatten().any(|s| s.call_site_truncated) {
                    local.miss("truncated_source");
                    return;
                }
                for source in n.origins.iter().flatten() {
                    if source.file.as_ref() == origin.path {
                        source_lines.0 = source_lines.0.min(source.line);
                        source_lines.1 = source_lines.1.max(source.line);
                    }
                }
                tags.insert(
                    id,
                    Parameter {
                        candidate,
                        line,
                        root: is_root,
                    },
                );
                if siblings {
                    pending.push((n.link, false, true));
                }
                pending.push((n.list_ptr, false, true));
            }
            font_runs.push(runs);
        }
        let ready = Ready {
            literal_end,
            frontier_context: frontier.map(|f| f.context),
            tail_reads: local
                .evidence
                .borrow()
                .tail_reads
                .get(&candidate)
                .cloned()
                .unwrap_or_default(),
            fonts: self
                .fonts
                .items
                .iter()
                .map(super::reuse::FontGuard::new)
                .collect(),
            current_font: self.fonts.current,
            font_runs,
            nodes_count: tags.len(),
            origin,
            anchors,
            source_lines,
            end,
            roots: roots.to_vec(),
            shapes,
            penalties: penalties.to_vec(),
            context: self.builder.context.clone(),
            badness: self.layout.last_badness_value(),
            last_line: self.math.last_line_value(),
            warnings: self.core.diagnostics.warnings.clone(),
            errors: self.core.diagnostics.errors.clone(),
            candidate,
        };
        self.layout
            .parameterize_badness(local.evidence.clone(), candidate);
        self.math
            .parameterize_last_line(local.evidence.clone(), candidate);
        local.evidence.borrow_mut().tags.extend(tags);
        self.builder.nodes.observer = Some(local.evidence.clone());
        local.ready.push(ready.clone());
        local.pending = Some(ready);
    }
}

impl EvalView<'_> {
    pub(super) fn local_material_boundary(&mut self) {
        if !self.core.config.exact_local_updates
            || self.core.local.frontier.is_some()
            || !self.builder.context.vertical_paragraph
            || self.builder.context.mode != crate::core::enums::ListMode::Horizontal
        {
            return;
        }
        let Some(end) = self
            .core
            .input
            .input_file
            .first()
            .and_then(Option::as_ref)
            .and_then(|s| s.byte_position())
        else {
            return;
        };
        let anchors = literal_anchors(
            &self.core.local.anchors,
            &self.core.input,
            self.env,
            end,
            false,
        );
        if anchors.is_empty() || self.core.local.next_candidate >= 128 {
            return;
        }
        let candidate = self.core.local.next_candidate;
        self.core.local.next_candidate += 1;
        let mut tags = std::collections::BTreeMap::new();
        let mut pending = vec![self.builder.context.cur_list_head];
        while let Some(id) = pending.pop() {
            if id == C::NULL {
                continue;
            }
            if tags.len() >= 8192 || tags.contains_key(&id) {
                return;
            }
            let overlaps = self
                .core
                .local
                .evidence
                .borrow()
                .tags
                .get(&id)
                .is_some_and(|tag| tag.candidate != candidate);
            if overlaps {
                self.core.local.miss("overlapping_material_parameters");
                return;
            }
            let Ok(node) = self.builder.node(id) else {
                return;
            };
            if !crate::checked_material::pure(node) {
                return;
            }
            tags.insert(
                id,
                Parameter {
                    candidate,
                    line: 0,
                    root: false,
                },
            );
            pending.push(node.link);
            pending.push(node.list_ptr);
        }
        let local = &mut self.core.local;
        local.evidence.borrow_mut().tags.extend(tags);
        local.evidence.borrow_mut().tail_open.insert(candidate);
        self.builder.nodes.observer = Some(local.evidence.clone());
        local.frontier = Some(Frontier {
            candidate,
            anchors,
            end,
            context: control_context(self.builder.context.clone()),
            rejected: false,
        });
    }
}
