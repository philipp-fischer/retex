//! Language tries, Liang matching and candidate reconstruction (`hyphenation.py`).
use super::layout::TypesettingContext;
use super::model::Node;
use super::nodes::{integer, parameter};
use super::reconstitution::{Glyph, Reconstitutor, Specification};
use crate::core::characters::{LanguageProfile, unicode_case};
use crate::core::constants::Constants as C;
use crate::core::enums::{CharacterCategory as Cat, Command, ListMode, NodeKind};
use crate::core::errors::{Error, Result};
use crate::core::persistence::PersistentComponent;
use crate::language::input::{ExpandedInput, scan_left_brace};
use crate::language::records::{Quantity, ValueIndex, ValueKey};
use crate::language::state::Environment;
use crate::typesetting::model::NodeFields;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use unicode_segmentation::UnicodeSegmentation;

pub fn language(value: i64, native: bool) -> i64 {
    if native {
        if (1..=255).contains(&value) { value } else { 0 }
    } else {
        value.max(0)
    }
}
pub fn minimum(value: i64, native: bool) -> i64 {
    if native {
        value.clamp(1, 63)
    } else {
        value.max(1)
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Trie {
    pub children: BTreeMap<u32, Trie>,
    pub weights: Vec<u8>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SavedCodes {
    Octets(Vec<u32>),
    Unicode(BTreeMap<u32, u32>),
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Hyphenation {
    pub tries: crate::core::persistence::SharedTable<i64, Trie>,
    pub exceptions: crate::core::persistence::SharedTable<(i64, Vec<u32>), Vec<usize>>,
    pub saved_codes: crate::core::persistence::SharedTable<i64, SavedCodes>,
    pub ready: bool,
    pub events: Vec<HyphenationEvent>,
}
/// Material replaced during this paragraph's current execution, not an edit
/// cache or a correspondence between document revisions.
pub(crate) struct WordReplacement {
    pub begin: usize,
    pub old_end: usize,
    pub new_len: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum HyphenationEvent {
    Data {
        language: i64,
        patterns: bool,
    },
    Language {
        language: i64,
        left: i64,
        right: i64,
    },
    Word {
        language: i64,
        font: usize,
        letters: Vec<u32>,
        levels: Vec<u8>,
        left: i64,
        right: i64,
        positions: Vec<usize>,
    },
}
pub trait HyphenationInput: ExpandedInput {
    fn environment(&self) -> &Environment;
    fn scan_char_num(&mut self) -> Result<i64>;
}
impl Hyphenation {
    pub fn state(env: &Environment, profile: LanguageProfile) -> (i64, i64, i64) {
        let native = profile == LanguageProfile::Pdftex;
        (
            language(parameter(env, "language", 0), native),
            minimum(parameter(env, "lefthyphenmin", 2), native),
            minimum(parameter(env, "righthyphenmin", 3), native),
        )
    }
    pub fn lowercase(
        &self,
        env: &Environment,
        profile: LanguageProfile,
        code: u32,
        lang: i64,
        saved: bool,
    ) -> u32 {
        if saved && self.ready {
            if let Some(codes) = self.saved_codes.get(&lang) {
                return match codes {
                    SavedCodes::Unicode(codes) => codes
                        .get(&code)
                        .copied()
                        .unwrap_or_else(|| unicode_case(code, false)),
                    SavedCodes::Octets(codes) => codes.get(code as usize).copied().unwrap_or(0),
                };
            }
        }
        integer(
            env,
            &ValueKey {
                table: "lccode".into(),
                index: ValueIndex::Number(code as i64),
            },
            if profile == LanguageProfile::Unicode {
                unicode_case(code, false) as i64
            } else if (65..=90).contains(&code) {
                (code + 32) as i64
            } else if (97..=122).contains(&code) {
                code as i64
            } else {
                0
            },
        ) as u32
    }
    pub fn read(&mut self, input: &mut impl HyphenationInput, patterns: bool) -> Result<()> {
        if patterns && self.ready {
            return Err(Error::value(
                "Too late for patterns: hyphenation tables are already initialized",
            )
            .with_issue(crate::core::recovery::Issue::Hyphenation));
        }
        let mut lang = Self::state(input.environment(), input.input().profile).0;
        scan_left_brace(input)?;
        if !patterns {
            lang = Self::state(input.environment(), input.input().profile).0;
        }
        let mut letters = Vec::new();
        let mut levels = vec![0];
        let mut positions = Vec::new();
        let mut digit = false;
        let profile = input.input().profile;
        loop {
            input.expanded_token()?;
            let mut current = input.input().current.clone();
            let mut code = current.character().unwrap_or(-1);
            if !patterns && current.is_command(Command::CharNum) {
                code = input.scan_char_num()?;
                current.command =
                    crate::language::records::MeaningCommand::Operation(Command::CharGiven);
            }
            if current.is_category(Cat::Spacer) || current.is_category(Cat::RightBrace) {
                if patterns && !letters.is_empty() {
                    if letters[0] == 0 {
                        levels[0] = 0;
                    }
                    if letters.last() == Some(&0) {
                        *levels.last_mut().unwrap() = 0;
                    }
                    let mut trie = self.tries.entry(lang).or_default();
                    for &character in &letters {
                        trie = trie.children.entry(character).or_default();
                    }
                    if trie.weights.iter().any(|&weight| weight != 0) {
                        return Err(Error::value("Duplicate pattern")
                            .with_issue(crate::core::recovery::Issue::Hyphenation));
                    }
                    trie.weights = levels.clone();
                } else if !patterns && letters.len() > 1 {
                    self.exceptions
                        .insert((lang, letters.clone()), positions.clone());
                }
                if current.is_category(Cat::RightBrace) {
                    break;
                }
                letters.clear();
                levels = vec![0];
                positions.clear();
                digit = false;
            } else if current.is_category(Cat::Letter)
                || current.is_category(Cat::OtherChar)
                || !patterns && current.is_command(Command::CharGiven)
            {
                let room = profile == LanguageProfile::Unicode || letters.len() < 63;
                if patterns {
                    if !digit && (48..=57).contains(&code) {
                        if room {
                            *levels.last_mut().unwrap() = (code - 48) as u8;
                            digit = true;
                        }
                    } else {
                        let lower = if code == 46 {
                            0
                        } else {
                            self.lowercase(input.environment(), profile, code as u32, lang, false)
                        };
                        if lower == 0 && current.character() != Some(46) {
                            return Err(Error::value("Nonletter in patterns")
                                .with_issue(crate::core::recovery::Issue::Hyphenation));
                        }
                        if room {
                            letters.push(lower);
                            levels.push(0);
                            digit = false;
                        }
                    }
                } else if code == 45 {
                    if room {
                        positions.push(letters.len());
                    }
                } else {
                    let lower =
                        self.lowercase(input.environment(), profile, code as u32, lang, true);
                    if lower == 0 {
                        return Err(Error::value("Not a letter in hyphenation exception")
                            .with_issue(crate::core::recovery::Issue::Hyphenation));
                    }
                    if room {
                        letters.push(lower);
                    }
                }
            } else {
                return Err(Error::value(if patterns {
                    "Bad patterns"
                } else {
                    "Improper hyphenation exception"
                }));
            }
        }
        if patterns && parameter(input.environment(), "savinghyphcodes", 0) > 0 {
            let codes = if profile == LanguageProfile::Unicode {
                SavedCodes::Unicode(
                    input
                        .environment()
                        .values
                        .iter()
                        .filter_map(|(key, value)| match (&key.index, value) {
                            (ValueIndex::Number(code), Quantity::Integer(value))
                                if key.table == "lccode" =>
                            {
                                Some((*code as u32, *value as u32))
                            }
                            _ => None,
                        })
                        .collect(),
                )
            } else {
                SavedCodes::Octets(
                    (0..256)
                        .map(|code| self.lowercase(input.environment(), profile, code, lang, false))
                        .collect(),
                )
            };
            self.saved_codes.insert(lang, codes);
        }
        self.events.push(HyphenationEvent::Data {
            language: lang,
            patterns,
        });
        Ok(())
    }
    pub fn points(&self, word: &[u32], lang: i64, left: i64, right: i64) -> Vec<u8> {
        let size = word.len();
        let mut levels = vec![0; size + 1];
        if let Some(exception) = self.exceptions.get(&(lang, word.to_vec())) {
            for &position in exception {
                if position <= size {
                    levels[position] = 1;
                }
            }
        } else if let Some(root) = self.tries.get(&lang) {
            let mut text = vec![0];
            text.extend_from_slice(word);
            text.push(0);
            for start in 0..(size as i64 - right + 2).max(0) as usize {
                let mut trie = root;
                for &code in text.iter().skip(start) {
                    let Some(next) = trie.children.get(&code) else {
                        break;
                    };
                    trie = next;
                    for (offset, &weight) in trie.weights.iter().enumerate() {
                        let position = start as i64 + offset as i64 - 1;
                        if (0..=size as i64).contains(&position) {
                            levels[position as usize] = levels[position as usize].max(weight);
                        }
                    }
                }
            }
        }
        for (position, weight) in levels.iter_mut().enumerate() {
            if (position as i64) < left || position as i64 > size as i64 - right {
                *weight = 0;
            }
        }
        levels
    }
    pub fn fix_language(
        &mut self,
        context: &mut TypesettingContext<'_>,
        profile: LanguageProfile,
        explicit: Option<i64>,
    ) -> Result<()> {
        if explicit.is_none()
            && (context.builder.context.mode != ListMode::Horizontal
                || context.builder.context.restricted)
        {
            return Ok(());
        }
        if explicit.is_some() && context.builder.context.mode != ListMode::Horizontal {
            return Err(Error::value("Setlanguage outside horizontal mode"));
        }
        let (mut lang, left, right) = Self::state(context.env, profile);
        if let Some(explicit) = explicit {
            lang = language(explicit, profile == LanguageProfile::Pdftex);
        }
        if explicit.is_some() || lang != context.builder.context.current_language {
            context
                .builder
                .flush_lig_stack(context.fonts, context.env, true)?;
            let pointer = context.builder.allocate(Node::from(NodeFields {
                node_type: NodeKind::Whatsit,
                language: lang,
                left_hyphen_min: left,
                right_hyphen_min: right,
                ..NodeFields::default()
            }))?;
            context.builder.tail_append(pointer)?;
            context.builder.context.current_language = lang;
            self.events.push(HyphenationEvent::Language {
                language: lang,
                left,
                right,
            });
        }
        Ok(())
    }
    pub fn after_glue(
        &mut self,
        context: &mut TypesettingContext<'_>,
        profile: LanguageProfile,
        items: &mut Vec<i64>,
        index: usize,
        state: (i64, i64, i64),
    ) -> Result<((i64, i64, i64), bool)> {
        self.after_glue_replacement(context, profile, items, index, state)
            .map(|(state, replacement)| (state, replacement.is_some()))
    }
    pub(crate) fn after_glue_replacement(
        &mut self,
        context: &mut TypesettingContext<'_>,
        profile: LanguageProfile,
        items: &mut Vec<i64>,
        index: usize,
        state: (i64, i64, i64),
    ) -> Result<((i64, i64, i64), Option<WordReplacement>)> {
        let (mut lang, mut left, mut right) = state;
        let mut previous = index;
        let mut cursor = index + 1;
        while cursor < items.len() {
            let node = context.builder.node(items[cursor])?;
            let code = match node.node_type {
                NodeKind::Char => Some(node.char),
                NodeKind::Ligature => node.components.first().copied(),
                NodeKind::Whatsit => {
                    if node.whatsit.is_empty() {
                        (lang, left, right) =
                            (node.language, node.left_hyphen_min, node.right_hyphen_min);
                    }
                    None
                }
                NodeKind::Kern if node.kern_kind == "normal" => None,
                _ => return Ok(((lang, left, right), None)),
            };
            let lower = code.map_or(0, |code| {
                self.lowercase(context.env, profile, code, lang, true)
            });
            if lower != 0 {
                if Some(lower) != code && parameter(context.env, "uchyph", 0) <= 0 {
                    return Ok(((lang, left, right), None));
                }
                break;
            }
            previous = cursor;
            cursor += 1;
        }
        if cursor == items.len() || profile == LanguageProfile::Pdftex && left + right > 63 {
            return Ok(((lang, left, right), None));
        }
        let font_id = context.builder.node(items[cursor])?.font;
        let font = &context.fonts.items[font_id];
        if !profile.accepts(font.hyphenchar) {
            return Ok(((lang, left, right), None));
        }
        let mut letters = Vec::new();
        let mut lowers = Vec::new();
        let mut origins = Vec::new();
        let mut boundary = 256;
        let mut last = previous;
        while cursor < items.len() {
            let node = context.builder.node(items[cursor])?;
            if node.glyph() {
                if node.font != font_id {
                    break;
                }
                let components = if node.node_type == NodeKind::Char {
                    vec![node.char]
                } else {
                    node.components.clone()
                };
                if let Some(&code) = components.first() {
                    boundary = code;
                }
                let codes: Vec<_> = components
                    .iter()
                    .map(|&code| self.lowercase(context.env, profile, code, lang, true))
                    .collect();
                if codes.contains(&0)
                    || profile == LanguageProfile::Pdftex && letters.len() + codes.len() > 63
                {
                    break;
                }
                origins.extend(std::iter::repeat_n(node.origins.clone(), codes.len()));
                letters.extend(components);
                lowers.extend(codes);
                boundary = if node.right_boundary {
                    font.boundary_char
                } else {
                    256
                };
            } else if node.node_type == NodeKind::Kern && node.kern_kind == "normal" {
                boundary = font.boundary_char;
            } else {
                break;
            }
            last = cursor;
            cursor += 1;
        }
        if (letters.len() as i64) < left + right {
            return Ok(((lang, left, right), None));
        }
        for &pointer in &items[cursor..] {
            let node = context.builder.node(pointer)?;
            if matches!(
                node.node_type,
                NodeKind::Whatsit | NodeKind::Glue | NodeKind::Penalty
            ) || node.node_type == NodeKind::Kern && node.kern_kind == "explicit"
            {
                break;
            }
            if !node.glyph() && node.node_type != NodeKind::Kern
                || node.node_type == NodeKind::Kern && node.kern_kind != "normal"
            {
                return Ok(((lang, left, right), None));
            }
        }
        let mut levels = self.points(&lowers, lang, left, right);
        if font.unicode_font.is_some() {
            let text: String = letters
                .iter()
                .filter_map(|&code| char::from_u32(code))
                .collect();
            let mut end = 0;
            let boundaries: BTreeSet<_> = text
                .graphemes(true)
                .map(|grapheme| {
                    end += grapheme.chars().count();
                    end
                })
                .collect();
            for (index, weight) in levels.iter_mut().enumerate() {
                if !boundaries.contains(&index) {
                    *weight = 0;
                }
            }
        }
        if !levels.iter().any(|weight| weight % 2 != 0) {
            return Ok(((lang, left, right), None));
        }
        let before = context.builder.node(items[previous])?.clone();
        let mut begin = previous + 1;
        let mut start = 1;
        let mut first = if font.unicode_font.is_some() {
            None
        } else {
            Some(256)
        };
        let mut initial = Vec::new();
        let mut initial_ligature = false;
        let mut initial_left = false;
        if before.glyph() {
            start = 0;
            if before.font == font_id {
                begin = previous;
                first = Some(before.char);
                initial_ligature = before.node_type == NodeKind::Ligature;
                initial_left = before.left_boundary;
                let components = if initial_ligature {
                    before.components.clone()
                } else {
                    vec![before.char]
                };
                initial = components
                    .into_iter()
                    .map(|code| {
                        Specification::Glyph(Glyph {
                            code,
                            components: vec![code],
                            origins: before.origins.to_vec(),
                            ligature: false,
                            left: false,
                            right: false,
                            shaped: None,
                        })
                    })
                    .collect();
                if initial.is_empty() && initial_left {
                    first = Some(256);
                    initial_ligature = false;
                }
            }
        } else if context.builder.node(items[previous + 1])?.left_boundary {
            start = 0;
        }
        let mut reconstruction_letters = vec![first];
        reconstruction_letters.extend(letters.iter().copied().map(Some));
        let mut reconstruction_origins = vec![before.origins.to_vec()];
        reconstruction_origins.extend(origins.into_iter().map(|origins| origins.into_vec()));
        let mut reconstruction = Reconstitutor {
            font,
            letters: reconstruction_letters,
            origins: reconstruction_origins,
            levels: levels.clone(),
            initial,
            initial_ligature,
            initial_left,
            steps: 0,
        };
        let specifications = reconstruction.reconstruct(start, boundary, context.warn)?;
        let positions = specifications
            .iter()
            .filter_map(|spec| {
                if let Specification::Discretionary(spec) = spec {
                    Some(spec.position)
                } else {
                    None
                }
            })
            .collect();
        let new_items = Self::allocate(context, &specifications, font_id)?;
        let replacement = WordReplacement {
            begin,
            old_end: last + 1,
            new_len: new_items.len(),
        };
        let old_items: Vec<_> = items.splice(begin..=last, new_items).collect();
        for pointer in old_items {
            context.builder.node_mut(pointer)?.link = C::NULL;
            context.builder.release(pointer, false)?;
        }
        for pair in items.windows(2) {
            context.builder.node_mut(pair[0])?.link = pair[1];
        }
        if let Some(&last) = items.last() {
            context.builder.node_mut(last)?.link = C::NULL;
        }
        self.events.push(HyphenationEvent::Word {
            language: lang,
            font: font_id,
            letters,
            levels,
            left,
            right,
            positions,
        });
        Ok(((lang, left, right), Some(replacement)))
    }
    pub fn allocate(
        context: &mut TypesettingContext<'_>,
        specifications: &[Specification],
        font: usize,
    ) -> Result<Vec<i64>> {
        // The source branches contain only glyphs/kerns. An explicit work stack also
        // supports nested owned alternatives without consuming the Rust call stack.
        enum Work<'a> {
            List(&'a [Specification], Option<(i64, u8)>),
            Finish(i64),
        }
        let mut work = vec![Work::List(specifications, None)];
        let mut result = Vec::new();
        let mut allocated = Vec::new();
        let outcome = (|| -> Result<()> {
            while let Some(item) = work.pop() {
                match item {
                    Work::List(specifications, owner) => {
                        let mut children = Vec::new();
                        let mut branches = Vec::new();
                        for spec in specifications {
                            let pointer = match spec {
                                Specification::Glyph(spec) => {
                                    let pointer = if let Some(shaped) = &spec.shaped {
                                        context.builder.new_shaped_node(
                                            font,
                                            (**shaped).clone(),
                                            spec.origins.iter().filter_map(Clone::clone).collect(),
                                        )?
                                    } else {
                                        context.builder.new_char_node(
                                            context.fonts,
                                            font,
                                            spec.code,
                                        )?
                                    };
                                    let node = context.builder.node_mut(pointer)?;
                                    node.origins = spec.origins.clone().into();
                                    if spec.ligature {
                                        node.node_type = NodeKind::Ligature;
                                        node.components = spec.components.clone();
                                        node.left_boundary = spec.left;
                                        node.right_boundary = spec.right;
                                    }
                                    pointer
                                }
                                Specification::Kern(width) => context.builder.new_kern(*width)?,
                                Specification::Discretionary(spec) => {
                                    let pointer =
                                        context.builder.allocate(Node::from(NodeFields {
                                            node_type: NodeKind::Disc,
                                            automatic: true,
                                            hyphen_position: spec.position,
                                            ..NodeFields::default()
                                        }))?;
                                    branches.push((pointer, spec));
                                    pointer
                                }
                            };
                            allocated.push(pointer);
                            children.push(pointer);
                        }
                        if let Some((pointer, branch)) = owner {
                            for pair in children.windows(2) {
                                context.builder.node_mut(pair[0])?.link = pair[1];
                            }
                            let head = children.first().copied().unwrap_or(C::NULL);
                            let node = context.builder.node_mut(pointer)?;
                            match branch {
                                0 => node.pre_break = head,
                                1 => node.post_break = head,
                                _ => node.list_ptr = head,
                            }
                        } else {
                            result = children;
                        }
                        for (pointer, spec) in branches.into_iter().rev() {
                            work.push(Work::Finish(pointer));
                            work.push(Work::List(&spec.replacement, Some((pointer, 2))));
                            work.push(Work::List(&spec.post, Some((pointer, 1))));
                            work.push(Work::List(&spec.pre, Some((pointer, 0))));
                        }
                    }
                    Work::Finish(pointer) => {
                        let children = context
                            .builder
                            .siblings(context.builder.node(pointer)?.list_ptr)?;
                        let mut width = 0;
                        let mut height = 0;
                        let mut depth = 0;
                        for child in children {
                            let node = context.builder.node(child)?;
                            width += node.width;
                            height = height.max(node.height);
                            depth = depth.max(node.depth);
                        }
                        let node = context.builder.node_mut(pointer)?;
                        node.width = width;
                        node.height = height;
                        node.depth = depth;
                    }
                }
            }
            Ok(())
        })();
        if outcome.is_err() {
            for pointer in allocated {
                if context.builder.nodes.contains_key(&pointer) {
                    let _ = context.builder.release(pointer, false);
                }
            }
        }
        outcome?;
        Ok(result)
    }
}
impl PersistentComponent for Hyphenation {
    type State = Self;
    fn dump_state(&self) -> Self {
        Self {
            events: Vec::new(),
            ..self.clone()
        }
    }
    fn restore_state(&mut self, state: Self) -> Result<()> {
        *self = state;
        Ok(())
    }
}
