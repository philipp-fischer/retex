//! Stable scanner continuations at owned command boundaries. No expansion
//! machine or assignment scanner is suspended inside the reusable segment.
use super::*;
#[derive(Clone)]
pub(super) struct Boundary {
    pub frames: Vec<InputState>,
    physical: usize,
    pub current: CurrentToken,
    pub path: String,
    pub line: usize,
    pub origin_line: usize,
    pub position: usize,
    pub align: i64,
    pub first: usize,
    pub job: Option<String>,
    pub warning: i64,
    pub eof: std::collections::BTreeSet<usize>,
    pub max_buffer: usize,
    pub max_stack: usize,
    pub peak: usize,
}
impl Boundary {
    pub fn read(view: &EngineView<'_>) -> Option<Self> {
        let input = &view.core.input;
        if input.in_open != 1
            || input.scanner_status != ScannerStatus::Normal
            || input.scanning_filename
            || input.read_line_only
            || input.end_input_requested
            || input.log_file.is_some()
            || (input.alignment_handler.is_some() && cell_guard(view).is_none())
        {
            return None;
        }
        let mut frames = input.input_stack.clone();
        frames.push(input.cursor.clone());
        let physical = frames
            .iter()
            .rposition(|f| f.token_list.is_none() && f.name.is_some())?;
        if frames.len() > 64
            || frames[..physical]
                .iter()
                .any(|f| f.name.is_some() || f.token_list.is_some())
            || frames[physical + 1..]
                .iter()
                .any(|f| f.token_list.is_none())
            || frames
                .iter()
                .map(|f| f.token_list.as_ref().map_or(0, |t| t.len()))
                .sum::<usize>()
                > 8192
        {
            return None;
        }
        let path = frames[physical].name.clone()?;
        let line = frames[physical].line;
        let mut origin = input.source();
        let mut origin_line = line;
        for _ in 0..128 {
            let Some(span) = origin else { break };
            if span.file.as_ref() == path && span.expansion.is_none() && span.call_site.is_none() {
                origin_line = origin_line.min(span.line);
            }
            origin = span.call_site.clone();
        }
        let stream = input.input_file.first()?.as_ref()?;
        stream.catalog_bytes()?;
        Some(Self {
            frames,
            physical,
            current: input.current.clone(),
            path,
            line,
            origin_line,
            position: stream.byte_position()?,
            align: input.align_state,
            first: input.first,
            job: input.job_name.clone(),
            warning: input.warning_cs,
            eof: input.eof_seen.clone(),
            max_buffer: input.max_buf_stack,
            max_stack: input.max_in_stack,
            peak: input.peak_frame_depth,
        })
    }
    pub fn matches(&self, now: &Self) -> Option<isize> {
        #[cfg(feature = "latency-diagnostics")]
        let _guard =
            crate::latency_diagnostics::phase("group_scanner_guard", "scanner continuations", None);
        let delta = now.line as isize - self.line as isize;
        if self.path != now.path
            || self.align != now.align
            || self.first != now.first
            || self.job != now.job
            || self.warning != now.warning
            || self.eof != now.eof
            || self.current.command != now.current.command
            || self.current.value != now.current.value
            || self.current.token != now.current.token
            || self.current.control_sequence != now.current.control_sequence
            || self.frames.len() != now.frames.len()
        {
            return None;
        }
        if self.physical != now.physical {
            return None;
        }
        // These physical-line fields are unaffected by source-line rebasing.
        // Reject an impossible candidate before cloning/rebasing its potentially
        // large suspended macro provenance. The full comparison still follows.
        let (old, new) = (&self.frames[self.physical], &now.frames[now.physical]);
        if old.loc != new.loc
            || old.limit != new.limit
            || old.buffer != new.buffer
            || old.columns != new.columns
            || old.byte_columns != new.byte_columns
        {
            return None;
        }
        for (i, (old, new)) in self.frames.iter().zip(&now.frames).enumerate() {
            let old = rebase_frame(
                old.clone(),
                &self.path,
                self.origin_line,
                self.line,
                if i < self.physical { 0 } else { delta },
            );
            if old != *new {
                return None;
            }
        }
        Some(delta)
    }
    pub fn restore(
        &self,
        view: &mut EngineView<'_>,
        origin: usize,
        end: usize,
        delta: isize,
        position: usize,
    ) -> Result<()> {
        let mut frames = self
            .frames
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, f)| {
                rebase_frame(
                    f,
                    &self.path,
                    origin,
                    end,
                    if i < self.physical { 0 } else { delta },
                )
            })
            .collect::<Vec<_>>();
        view.core.input.cursor = frames
            .pop()
            .ok_or_else(|| Error::runtime("Empty reused scanner continuation"))?;
        view.core.input.input_stack = frames;
        view.core.input.current = self.current.clone();
        view.core.input.align_state = self.align;
        view.core.input.first = self.first;
        view.core.input.job_name = self.job.clone();
        view.core.input.warning_cs = self.warning;
        view.core.input.eof_seen = self.eof.clone();
        view.core.input.max_buf_stack = view.core.input.max_buf_stack.max(self.max_buffer);
        view.core.input.max_in_stack = view.core.input.max_in_stack.max(self.max_stack);
        view.core.input.peak_frame_depth = view.core.input.peak_frame_depth.max(self.peak);
        view.builder.source = view.core.input.source();
        view.core.input.input_file[0]
            .as_mut()
            .unwrap()
            .seek_catalog(position)?;
        Ok(())
    }
    pub fn bytes(&self) -> Option<usize> {
        retained_frames(&self.frames)
    }
    pub fn tokens(&self) -> impl Iterator<Item = Token> + '_ {
        self.frames
            .iter()
            .flat_map(|f| f.token_list.iter().flat_map(|t| t.iter().copied()))
    }
}

// Account retained immutable storage once per observation. Allocation identity
// is used only to avoid charging the same Arc repeatedly, never to establish
// semantic equivalence. Validation still compares complete frame contents.
fn retained_frames(frames: &[InputState]) -> Option<usize> {
    let mut bytes = frames.len() * std::mem::size_of::<InputState>();
    let mut pending = Vec::new();
    for f in frames {
        bytes += f.name.as_ref().map_or(0, String::len)
            + f.token_list
                .as_ref()
                .map_or(0, |t| t.len() * std::mem::size_of::<Token>())
            + f.buffer.as_ref().map_or(0, |v| v.len() * 4)
            + f.columns
                .as_ref()
                .map_or(0, |v| v.len() * std::mem::size_of::<usize>())
            + f.byte_columns
                .as_ref()
                .map_or(0, |v| v.len() * std::mem::size_of::<usize>())
            + f.token_sources.as_ref().map_or(0, |v| v.len() * 16);
        pending.extend(f.source.iter());
        if let Some(sources) = &f.token_sources {
            pending.extend(sources.iter().flatten());
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    while let Some(source) = pending.pop() {
        if !seen.insert(Arc::as_ptr(source) as usize) {
            continue;
        }
        if seen.len() > 8192 {
            return None;
        }
        bytes = bytes.checked_add(
            std::mem::size_of::<SourceSpan>()
                + source.file.len()
                + source.expansion.as_ref().map_or(0, |s| s.len())
                + 64,
        )?;
        if bytes > 1024 * 1024 {
            return None;
        }
        pending.extend(source.call_site.iter());
    }
    Some(bytes)
}

// Charge the retained immutable definition DAG rather than its recursively
// serialized wire form. Allocation identity is ONLY an accounting deduplication;
// every semantic dependency still uses complete meaning_matches validation.
// Owned fields are destructured exhaustively so additions require an audit.
pub(super) fn retained_meanings(
    meanings: &BTreeMap<Arc<str>, Option<Arc<Equivalence>>>,
) -> Option<usize> {
    use crate::language::tokens::{MacroDefinition, MacroParameters};
    use std::{collections::HashSet, mem::size_of};
    let mut bytes = meanings.len().checked_mul(128)?;
    let mut equivalences = HashSet::new();
    let mut definitions = HashSet::new();
    let mut token_arrays = HashSet::new();
    let mut source_arrays = HashSet::new();
    let mut sources = Vec::new();
    for (key, value) in meanings {
        bytes = bytes.checked_add(key.len() + 64)?;
        if let Some(value) = value {
            if !equivalences.insert(Arc::as_ptr(value) as usize) {
                continue;
            }
            bytes = bytes.checked_add(size_of::<Equivalence>() + 64)?;
            if let CommandOperand::Macro(definition) = &value.value {
                if !definitions.insert(Arc::as_ptr(definition) as usize) {
                    continue;
                }
                let MacroDefinition {
                    body,
                    parameters,
                    protected: _,
                    source,
                    long: _,
                    outer: _,
                    body_sources,
                } = definition.as_ref();
                bytes = bytes.checked_add(size_of::<MacroDefinition>() + 64)?;
                if token_arrays.insert(Arc::as_ptr(body) as *const () as usize) {
                    bytes = bytes.checked_add(body.len().checked_mul(size_of::<Token>())? + 64)?;
                }
                if let Some(MacroParameters {
                    prefix,
                    delimiters,
                    hash_brace: _,
                    match_characters,
                }) = parameters
                {
                    bytes =
                        bytes.checked_add(prefix.capacity().checked_mul(size_of::<Token>())?)?;
                    bytes = bytes
                        .checked_add(delimiters.capacity().checked_mul(size_of::<Vec<Token>>())?)?;
                    for delimiter in delimiters {
                        bytes = bytes
                            .checked_add(delimiter.capacity().checked_mul(size_of::<Token>())?)?;
                    }
                    bytes = bytes
                        .checked_add(match_characters.capacity().checked_mul(size_of::<u32>())?)?;
                }
                sources.extend(source.iter());
                if let Some(body_sources) = body_sources {
                    if source_arrays.insert(Arc::as_ptr(body_sources) as *const () as usize) {
                        bytes = bytes.checked_add(
                            body_sources
                                .len()
                                .checked_mul(size_of::<Option<Arc<SourceSpan>>>())?
                                + 64,
                        )?;
                        sources.extend(body_sources.iter().flatten());
                    }
                }
            } else {
                bytes = bytes.checked_add(
                    (bincode::serialized_size(&value.value).ok()? as usize).checked_mul(4)? + 128,
                )?;
            }
        }
        if bytes > BUDGET || sources.len() > 131072 {
            return None;
        }
    }
    let mut seen = HashSet::new();
    while let Some(source) = sources.pop() {
        if !seen.insert(Arc::as_ptr(source) as usize) {
            continue;
        }
        if seen.len() > 65536 {
            return None;
        }
        let SourceSpan {
            file,
            line: _,
            column: _,
            expansion,
            call_site,
            call_site_truncated: _,
            byte_column: _,
            end_byte_column: _,
        } = source.as_ref();
        bytes = bytes.checked_add(
            size_of::<SourceSpan>()
                + 128
                + file.len()
                + expansion.as_ref().map_or(0, |s| s.len() + 64),
        )?;
        if bytes > BUDGET {
            return None;
        }
        sources.extend(call_site.iter());
    }
    (bytes <= BUDGET).then_some(bytes)
}

#[cfg(test)]
mod meaning_retention_tests {
    use super::*;
    use crate::{
        core::enums::Command,
        language::{records::MeaningCommand, tokens::MacroDefinition},
    };
    fn definition(sources: Vec<Option<Arc<SourceSpan>>>) -> Arc<Equivalence> {
        Arc::new(Equivalence {
            command: MeaningCommand::Operation(Command::Macro),
            level: 1,
            value: CommandOperand::Macro(Arc::new(MacroDefinition {
                body: vec![Token::Packed(1); sources.len()].into(),
                parameters: None,
                protected: false,
                source: None,
                long: false,
                outer: false,
                body_sources: Some(sources.into()),
            })),
        })
    }
    #[test]
    fn shared_provenance_is_charged_once_but_distinct_retained_sources_are_charged() {
        let mut source = Arc::new(SourceSpan::new("definition.tex", 1, 1));
        for _ in 0..32 {
            let mut next = SourceSpan::new("definition.tex", 2, 1);
            next.call_site = Some(source);
            source = Arc::new(next);
        }
        let mut meanings = BTreeMap::from([(
            Arc::from("word"),
            Some(definition(vec![Some(source); 1024])),
        )]);
        let shared = retained_meanings(&meanings).unwrap();
        assert!(shared * 8 < bincode::serialized_size(&meanings).unwrap() as usize);
        let original = meanings.clone();
        // A complete semantic difference remains a difference. Accounting's
        // identity sets neither compare meanings nor authorize execution reuse.
        meanings.insert(
            Arc::from("word"),
            Some(definition(
                (0..1024)
                    .map(|i| Some(Arc::new(SourceSpan::new("definition.tex", i + 1, 1))))
                    .collect(),
            )),
        );
        assert!(retained_meanings(&meanings).unwrap() > shared);
        assert!(!meaning_matches(
            original["word"].as_ref(),
            meanings["word"].as_ref()
        ));
    }
    #[test]
    fn retained_definition_budget_is_enforced() {
        let meanings = BTreeMap::from([(
            Arc::from("large"),
            Some(definition(vec![
                None;
                BUDGET / std::mem::size_of::<Token>() + 1
            ])),
        )]);
        assert!(retained_meanings(&meanings).is_none());
    }
}
