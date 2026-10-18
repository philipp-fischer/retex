//! Math packing, glyphs and delimiter recipes (`math_construction.py`).
use super::layout::{Layout, TypesettingContext};
use super::math_atoms::half;
use super::math_codes::DelimiterCode;
use super::math_families::MathFamilies;
use super::model::Node;
use super::nodes::parameter;
use super::vertical::Vertical;
use crate::core::constants::Constants as C;
use crate::core::enums::{MathStyle, NodeKind};
use crate::core::errors::{Error, Result};
use crate::core::values::{GlueSpec, MAX_DIMEN};
use crate::fonts::formats::tfm::CharacterTag;
use crate::typesetting::model::NodeFields;
use std::collections::BTreeSet;
pub struct MathContext<'a, 'b> {
    pub typesetting: &'a mut TypesettingContext<'b>,
    pub layout: &'a mut Layout,
}
impl MathContext<'_, '_> {
    pub fn param(&self, index: usize, style: MathStyle, family: i64) -> Result<i64> {
        MathFamilies::param(
            self.typesetting.env,
            self.typesetting.fonts,
            index,
            style,
            family,
        )
    }
    pub fn hbox(&mut self, pointers: &[i64]) -> Result<i64> {
        let head = Vertical::link(self.typesetting.builder, pointers)?;
        self.layout
            .hpack(self.typesetting, head, None, false, false)
    }
    pub fn vbox(&mut self, pointers: &[i64]) -> Result<i64> {
        let head = Vertical::link(self.typesetting.builder, pointers)?;
        self.layout
            .vpack(self.typesetting, head, false, None, false, MAX_DIMEN)
    }
    pub fn character(&mut self, family: i64, code: u32, style: MathStyle) -> Result<(i64, i64)> {
        let font = MathFamilies::family(self.typesetting.env, style.font_size(), family);
        if font == 0 {
            return Err(Error::value(format!("Undefined math font family {family}")));
        }
        let info = &self.typesetting.fonts.items[font];
        if !info.has_character(code) {
            (self.typesetting.warn)(&format!("Missing character: {code} in font {}", info.name));
            return Ok((C::NULL, 0));
        }
        let italic = info.get_char_italic(code);
        Ok((
            self.typesetting
                .builder
                .new_char_node(self.typesetting.fonts, font, code)?,
            italic,
        ))
    }
    pub fn rebox(&mut self, mut pointer: i64, width: i64) -> Result<i64> {
        let mut node = self.typesetting.builder.node(pointer)?.clone();
        if node.width == width || node.list_ptr == C::NULL {
            self.typesetting.builder.node_mut(pointer)?.width = width;
            return Ok(pointer);
        }
        if node.node_type == NodeKind::Vlist {
            pointer = self.hbox(&[pointer])?;
            node = self.typesetting.builder.node(pointer)?.clone();
        }
        let mut children = self.typesetting.builder.siblings(node.list_ptr)?;
        if children.len() == 1
            && self.typesetting.builder.node(children[0])?.node_type == NodeKind::Char
        {
            let difference = node.width - self.typesetting.builder.node(children[0])?.width;
            if difference != 0 {
                children.push(self.typesetting.builder.new_kern(difference)?);
            }
        }
        self.typesetting.builder.node_mut(pointer)?.list_ptr = C::NULL;
        self.typesetting.builder.release(pointer, false)?;
        let spec = GlueSpec {
            stretch: C::UNITY,
            shrink: C::UNITY,
            stretch_order: 1,
            shrink_order: 1,
            ..GlueSpec::default()
        };
        let mut pointers = vec![self.typesetting.builder.new_glue(
            spec,
            "normal",
            self.typesetting.fonts.current,
        )?];
        pointers.extend(children);
        pointers.push(self.typesetting.builder.new_glue(
            spec,
            "normal",
            self.typesetting.fonts.current,
        )?);
        let head = Vertical::link(self.typesetting.builder, &pointers)?;
        self.layout
            .hpack(self.typesetting, head, Some(width), false, false)
    }
    pub fn rule(&mut self, width: i64, thickness: i64) -> Result<i64> {
        self.typesetting.builder.allocate(Node::from(NodeFields {
            node_type: NodeKind::Rule,
            width,
            height: thickness,
            ..NodeFields::default()
        }))
    }
    pub fn overbar(&mut self, pointer: i64, clearance: i64, thickness: i64) -> Result<i64> {
        let width = self.typesetting.builder.node(pointer)?.width;
        let first = self.typesetting.builder.new_kern(thickness)?;
        let rule = self.rule(width, thickness)?;
        let gap = self.typesetting.builder.new_kern(clearance)?;
        self.vbox(&[first, rule, gap, pointer])
    }
    fn delimiter_charbox(&mut self, font: usize, code: u32) -> Result<i64> {
        let info = &self.typesetting.fonts.items[font];
        let (width, height, depth) = (
            info.get_char_width(code) + info.get_char_italic(code),
            info.get_char_height(code),
            info.get_char_depth(code),
        );
        let child = self
            .typesetting
            .builder
            .new_char_node(self.typesetting.fonts, font, code)?;
        self.typesetting.builder.allocate(Node::from(NodeFields {
            width,
            height,
            depth,
            list_ptr: child,
            ..NodeFields::default()
        }))
    }
    pub fn delimiter_box(
        &mut self,
        code: DelimiterCode,
        style: MathStyle,
        desired: i64,
    ) -> Result<i64> {
        let mut best = None;
        let mut largest = 0;
        'candidates: for (family, character) in code.candidates() {
            if family == 0 && character == 0 {
                continue;
            }
            for level in (0..=style.font_size()).rev() {
                let font = MathFamilies::family(self.typesetting.env, level, family);
                if font == 0 {
                    continue;
                }
                let info = &self.typesetting.fonts.items[font];
                let mut code = character;
                let mut seen = BTreeSet::new();
                while info.has_character(code) {
                    if !seen.insert(code) {
                        return Err(Error::value("Cyclic delimiter variants"));
                    }
                    let tfm = info
                        .tfm_data
                        .as_ref()
                        .ok_or_else(|| Error::value("Math delimiter font has no TFM data"))?;
                    let (tag, next) = tfm
                        .char_tags
                        .get(&code)
                        .copied()
                        .unwrap_or((CharacterTag::None, 0));
                    let extent = info.get_char_height(code) + info.get_char_depth(code);
                    if tag == CharacterTag::Extensible {
                        best = Some((font, code, true));
                        break 'candidates;
                    }
                    if extent > largest {
                        best = Some((font, code, false));
                        largest = extent;
                        if largest >= desired {
                            break 'candidates;
                        }
                    }
                    if tag != CharacterTag::NextLarger {
                        break;
                    }
                    code = next as u32;
                }
            }
        }
        let pointer = if let Some((font, code, extensible)) = best {
            if !extensible {
                self.delimiter_charbox(font, code)?
            } else {
                let info = &self.typesetting.fonts.items[font];
                let [top, middle, bottom, repeat] = info
                    .tfm_data
                    .as_ref()
                    .unwrap()
                    .extensible
                    .get(&code)
                    .copied()
                    .ok_or_else(|| Error::value("Missing extensible recipe"))?;
                let unit = info.get_char_height(repeat as u32) + info.get_char_depth(repeat as u32);
                let mut extent: i64 = [top, middle, bottom]
                    .into_iter()
                    .filter(|&code| code != 0)
                    .map(|code| {
                        info.get_char_height(code as u32) + info.get_char_depth(code as u32)
                    })
                    .sum();
                let contribution = unit * if middle != 0 { 2 } else { 1 };
                let count = if contribution > 0 && desired > extent {
                    ((desired - extent + contribution - 1) / contribution) as usize
                } else {
                    0
                };
                extent += count as i64 * contribution;
                let width =
                    info.get_char_width(repeat as u32) + info.get_char_italic(repeat as u32);
                let mut parts = Vec::new();
                if top != 0 {
                    parts.push(top);
                }
                parts.extend(std::iter::repeat_n(repeat, count));
                if middle != 0 {
                    parts.push(middle);
                    parts.extend(std::iter::repeat_n(repeat, count));
                }
                if bottom != 0 {
                    parts.push(bottom);
                }
                let height = parts
                    .first()
                    .map_or(0, |&code| info.get_char_height(code as u32));
                let mut boxes = Vec::new();
                for code in parts {
                    boxes.push(self.delimiter_charbox(font, code as u32)?);
                }
                let pointer = self.vbox(&boxes)?;
                let node = self.typesetting.builder.node_mut(pointer)?;
                node.width = width;
                node.height = height;
                node.depth = extent - height;
                pointer
            }
        } else {
            let width = parameter(self.typesetting.env, "nulldelimiterspace", 0);
            self.typesetting.builder.allocate(Node::from(NodeFields {
                width,
                ..NodeFields::default()
            }))?
        };
        let axis = self.param(22, style, 2)?;
        let node = self.typesetting.builder.node_mut(pointer)?;
        node.shift = half(node.height - node.depth) - axis;
        Ok(pointer)
    }
}
