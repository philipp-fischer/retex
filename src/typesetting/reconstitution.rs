//! Synchronized ligature and shaped-cluster discretionary branches (`reconstitution.py`).
use super::ligature_cycles::PumpingGuard;
use super::nodes::try_lig_kern;
use crate::core::errors::{Error, Result};
use crate::core::source::SourceSpan;
use crate::fonts::ligatures::LigatureAction;
use crate::fonts::model::FontInfo;
use crate::fonts::unicode::ShapedGlyph;
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Glyph {
    pub code: u32,
    pub components: Vec<u32>,
    pub origins: Vec<Option<Arc<SourceSpan>>>,
    pub ligature: bool,
    pub left: bool,
    pub right: bool,
    pub shaped: Option<Arc<ShapedGlyph>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Discretionary {
    pub pre: Vec<Specification>,
    pub post: Vec<Specification>,
    pub replacement: Vec<Specification>,
    pub position: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Specification {
    Glyph(Glyph),
    Kern(i64),
    Discretionary(Discretionary),
}
pub struct Reconstitutor<'a> {
    pub font: &'a FontInfo,
    pub letters: Vec<Option<u32>>,
    pub origins: Vec<Vec<Option<Arc<SourceSpan>>>>,
    pub levels: Vec<u8>,
    pub initial: Vec<Specification>,
    pub initial_ligature: bool,
    pub initial_left: bool,
    pub steps: usize,
}
pub fn unique_origins(
    groups: impl IntoIterator<Item = Option<Arc<SourceSpan>>>,
) -> Vec<Option<Arc<SourceSpan>>> {
    let mut seen = HashSet::new();
    groups
        .into_iter()
        .filter(|origin| seen.insert(origin.clone()))
        .collect()
}
fn wrap(
    output: &mut Vec<Specification>,
    mark: usize,
    left: u32,
    ligature: &mut bool,
    left_hit: &mut bool,
    right_hit: &mut bool,
    use_right: bool,
    empty_stack: bool,
) {
    if !*ligature {
        return;
    }
    let glyphs: Vec<_> = output[mark..]
        .iter()
        .filter_map(|item| {
            if let Specification::Glyph(glyph) = item {
                Some(glyph)
            } else {
                None
            }
        })
        .collect();
    let components = glyphs
        .iter()
        .flat_map(|glyph| glyph.components.iter().copied())
        .collect();
    let origins = unique_origins(
        glyphs
            .iter()
            .flat_map(|glyph| glyph.origins.iter().cloned()),
    );
    let hit = use_right && empty_stack;
    output.truncate(mark);
    output.push(Specification::Glyph(Glyph {
        code: left,
        components,
        origins,
        ligature: true,
        left: *left_hit,
        right: hit,
        shaped: None,
    }));
    *ligature = false;
    *left_hit = false;
    if hit {
        *right_hit = false;
    }
}
impl Reconstitutor<'_> {
    fn letter(&self, index: usize) -> Result<u32> {
        self.letters
            .get(index)
            .copied()
            .flatten()
            .ok_or_else(|| Error::value("Invalid reconstruction source index"))
    }
    pub fn character(&self, index: usize) -> Result<Specification> {
        let code = self.letter(index)?;
        Ok(Specification::Glyph(Glyph {
            code,
            components: vec![code],
            origins: self.origins[index].clone(),
            ligature: false,
            left: false,
            right: false,
            shaped: None,
        }))
    }
    fn set_right(&self, j: usize, end: usize, boundary: u32, hyphen: u32) -> Result<(u32, u32)> {
        Ok((
            if j < end {
                self.letter(j + 1)?
            } else {
                boundary
            },
            if self.levels[j] % 2 != 0 { hyphen } else { 256 },
        ))
    }
    fn pop(
        &self,
        stack: &mut VecDeque<(u32, Option<usize>)>,
        output: &mut Vec<Specification>,
        j: &mut usize,
        right: &mut u32,
        probe: &mut u32,
        end: usize,
        boundary: u32,
        hyphen: u32,
    ) -> Result<()> {
        let (_, original) = stack
            .pop_front()
            .ok_or_else(|| Error::value("Empty reconstruction stack"))?;
        if let Some(original) = original {
            output.push(self.character(original)?);
            *j += 1;
        }
        if let Some(&(code, _)) = stack.front() {
            *right = code;
        } else {
            (*right, *probe) = self.set_right(*j, end, boundary, hyphen)?;
        }
        Ok(())
    }
    pub fn cut(
        &mut self,
        start: usize,
        end: usize,
        mut boundary: u32,
        mut hyphen: u32,
    ) -> Result<(usize, Vec<Specification>, usize)> {
        if self.font.unicode_font.is_some() {
            return self.unicode_cut(start, end, hyphen);
        }
        let mut j = start;
        let mut left = self.letter(j)?;
        let mut output = Vec::new();
        let mut mark = 0;
        let mut ligature = j == 0 && self.initial_ligature;
        let mut left_hit = ligature && self.initial_left;
        let mut right_hit = false;
        let mut stack: VecDeque<(u32, Option<usize>)> = VecDeque::new();
        let mut guard = PumpingGuard::new("hyphenation ligature program");
        let mut passed = 0;
        if j == 0 {
            output.extend(self.initial.clone());
        } else if left < 256 {
            output.push(self.character(j)?);
        }
        let (mut right, mut probe) = self.set_right(j, end, boundary, hyphen)?;
        loop {
            self.steps += 1;
            let matched = try_lig_kern(self.font, left, if probe < 256 { probe } else { right })?;
            if probe < 256 {
                if matched.is_some() {
                    passed = j;
                    hyphen = 256;
                }
                probe = 256;
                continue;
            }
            let mut kern = 0;
            let mut done = matched.is_none();
            if let Some(matched) = matched {
                if hyphen < 256 && self.levels[j] % 2 != 0 {
                    passed = j;
                    hyphen = 256;
                }
                if let Some(value) = matched.kern {
                    kern = value;
                    done = true;
                } else {
                    let code = matched
                        .character
                        .ok_or_else(|| Error::value("Missing ligature character"))?;
                    let behavior = matched
                        .operation
                        .ok_or_else(|| Error::value("Missing ligature operation"))?
                        .behavior();
                    guard.visit(
                        (
                            left,
                            right,
                            hyphen,
                            j,
                            boundary,
                            passed,
                            stack.front().copied(),
                            ligature,
                            left_hit,
                            right_hit,
                        ),
                        stack.len(),
                    )?;
                    if left == 256 {
                        left_hit = true;
                    }
                    if j == end && stack.is_empty() {
                        right_hit = true;
                    }
                    if behavior.action == LigatureAction::ReplaceLeft {
                        left = code;
                        ligature = true;
                    } else if behavior.action == LigatureAction::ReplaceRight {
                        right = code;
                        if let Some(item) = stack.front_mut() {
                            item.0 = code;
                        } else {
                            stack.push_back((code, (j < end).then_some(j + 1)));
                            if j == end {
                                boundary = 256;
                            }
                        }
                    } else if behavior.action == LigatureAction::InsertBetween
                        && !behavior.pass_left_character
                    {
                        right = code;
                        stack.push_front((code, None));
                    } else if behavior.pass_left_character {
                        wrap(
                            &mut output,
                            mark,
                            left,
                            &mut ligature,
                            &mut left_hit,
                            &mut right_hit,
                            false,
                            stack.is_empty(),
                        );
                        mark = output.len();
                        left = code;
                        ligature = true;
                    } else {
                        left = code;
                        ligature = true;
                        if !stack.is_empty() {
                            self.pop(
                                &mut stack,
                                &mut output,
                                &mut j,
                                &mut right,
                                &mut probe,
                                end,
                                boundary,
                                hyphen,
                            )?;
                        } else if j == end {
                            done = true;
                        } else {
                            output.push(self.character(j + 1)?);
                            j += 1;
                            (right, probe) = self.set_right(j, end, boundary, hyphen)?;
                        }
                    }
                    if behavior.finish_hyphenation_pair {
                        done = true;
                    }
                }
            }
            if !done {
                continue;
            }
            let use_right = right_hit;
            wrap(
                &mut output,
                mark,
                left,
                &mut ligature,
                &mut left_hit,
                &mut right_hit,
                use_right,
                stack.is_empty(),
            );
            if kern != 0 {
                output.push(Specification::Kern(kern));
            }
            if stack.is_empty() {
                return Ok((j, output, passed));
            }
            mark = output.len();
            left = stack.front().unwrap().0;
            ligature = true;
            self.pop(
                &mut stack,
                &mut output,
                &mut j,
                &mut right,
                &mut probe,
                end,
                boundary,
                hyphen,
            )?;
        }
    }
    pub fn reconstruct(
        &mut self,
        start: usize,
        boundary: u32,
        warn: &mut dyn FnMut(&str),
    ) -> Result<Vec<Specification>> {
        let end = self
            .letters
            .len()
            .checked_sub(1)
            .ok_or_else(|| Error::value("Empty reconstruction"))?;
        let mut result = Vec::new();
        let mut j = start;
        while j <= end {
            let mut l = j;
            let (consumed, mut replacement, mut passed) =
                self.cut(j, end, boundary, self.font.hyphenchar as u32)?;
            j = consumed + 1;
            if passed == 0 {
                result.append(&mut replacement);
                if self.levels[j - 1] % 2 == 0 {
                    continue;
                }
                l = j;
                passed = j - 1;
            }
            loop {
                let position = passed;
                self.levels[position] = 0;
                let mut pre = Vec::new();
                let mut post = Vec::new();
                let mut i = position;
                let hyphen_exists = self.font.has_character(self.font.hyphenchar as u32);
                let saved;
                if hyphen_exists {
                    i += 1;
                    saved = Some((self.letters[i], self.origins[i].clone()));
                    self.letters[i] = Some(self.font.hyphenchar as u32);
                    self.origins[i] = self.origins[position].clone();
                } else {
                    saved = None;
                    warn(&format!(
                        "Missing character: {} in font {}",
                        self.font.hyphenchar, self.font.name
                    ));
                }
                while l <= i {
                    let (consumed, mut nodes, _) = self.cut(l, i, self.font.boundary_char, 256)?;
                    l = consumed + 1;
                    pre.append(&mut nodes);
                }
                if let Some((letter, origins)) = saved {
                    self.letters[i] = letter;
                    self.origins[i] = origins;
                    l = i;
                }
                let mut restore = None;
                if self.font.boundary_label.is_some() {
                    l = l
                        .checked_sub(1)
                        .ok_or_else(|| Error::value("Invalid boundary reconstruction"))?;
                    restore = Some((l, self.letters[l]));
                    self.letters[l] = Some(256);
                }
                while l < j {
                    loop {
                        let (consumed, mut nodes, _) = self.cut(l, end, boundary, 256)?;
                        l = consumed + 1;
                        post.append(&mut nodes);
                        if let Some((at, value)) = restore.take() {
                            self.letters[at] = value;
                        }
                        if l >= j {
                            break;
                        }
                    }
                    while l > j {
                        let (consumed, mut nodes, _) = self.cut(j, end, boundary, 256)?;
                        j = consumed + 1;
                        replacement.append(&mut nodes);
                    }
                }
                if replacement.len() > 127 {
                    result.append(&mut replacement);
                } else {
                    result.push(Specification::Discretionary(Discretionary {
                        pre,
                        post,
                        replacement: std::mem::take(&mut replacement),
                        position,
                    }));
                }
                passed = j - 1;
                if self.levels[passed] % 2 == 0 {
                    break;
                }
            }
        }
        Ok(result)
    }
    fn unicode_cut(
        &self,
        start: usize,
        end: usize,
        hyphen: u32,
    ) -> Result<(usize, Vec<Specification>, usize)> {
        if self.letters[start].is_none() {
            return Ok((start, Vec::new(), 0));
        }
        let text: String = self.letters[start..=end]
            .iter()
            .map(|code| {
                code.and_then(char::from_u32)
                    .ok_or_else(|| Error::value("Invalid Unicode reconstruction character"))
            })
            .collect::<Result<_>>()?;
        let font = self.font.unicode_font.as_ref().unwrap();
        let shaped = font.shape(&text, !self.font.ligatures_disabled)?;
        let first = shaped
            .first()
            .ok_or_else(|| Error::value("Empty shaped reconstruction"))?;
        let cluster: Vec<_> = shaped
            .iter()
            .filter(|glyph| glyph.cluster_start == 0)
            .collect();
        let consumed = start + first.cluster_end - 1;
        let origins = unique_origins(self.origins[start..=consumed].iter().flatten().cloned());
        let result = cluster
            .iter()
            .map(|glyph| {
                Specification::Glyph(Glyph {
                    code: glyph.text.chars().next().map_or(0, u32::from),
                    components: glyph.text.chars().map(u32::from).collect(),
                    origins: origins.clone(),
                    ligature: true,
                    left: false,
                    right: false,
                    shaped: Some(Arc::new((**glyph).clone())),
                })
            })
            .collect();
        let mut passed = 0;
        if hyphen as i64 == self.font.hyphenchar {
            for position in start..=consumed {
                if self.levels[position] % 2 == 0 {
                    continue;
                }
                let mut prefix: String = text.chars().take(position - start + 1).collect();
                prefix.push(
                    char::from_u32(hyphen)
                        .ok_or_else(|| Error::value("Invalid hyphen character"))?,
                );
                let probe = font.shape(&prefix, !self.font.ligatures_disabled)?;
                let signature = |glyph: &ShapedGlyph| {
                    (
                        glyph.glyph_id,
                        glyph.advance_sp,
                        glyph.x_offset_sp,
                        glyph.y_offset_sp,
                    )
                };
                let same = cluster.iter().map(|glyph| signature(glyph)).eq(probe
                    .iter()
                    .filter(|glyph| glyph.cluster_start == 0)
                    .map(signature));
                if position < consumed || !same {
                    passed = position;
                    break;
                }
            }
        }
        Ok((consumed, result, passed))
    }
}
