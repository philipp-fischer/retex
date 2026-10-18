//! PDF identities reserved at font and page use boundaries (`resources.py`).
use super::graphics::LiteralPaint;
use super::writer::PdfWriter;
use crate::core::errors::{Error, Result};
use crate::fonts::catalog::FontCatalog;
use crate::fonts::unicode::ShapedGlyph;
use crate::language::state::Environment;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FontUse {
    pub font: usize,
    pub object_number: usize,
    pub members: BTreeSet<usize>,
    pub unicode_codes: BTreeMap<(u16, String), u16>,
}
impl FontUse {
    pub fn unicode_code(&mut self, glyph: &ShapedGlyph) -> Result<u16> {
        let key = (
            u16::try_from(glyph.glyph_id)
                .map_err(|_| Error::value("Glyph index exceeds SFNT range"))?,
            glyph.text.clone(),
        );
        if let Some(&code) = self.unicode_codes.get(&key) {
            return Ok(code);
        }
        let cid = u16::try_from(self.unicode_codes.len() + 1).map_err(|_| {
            Error::value("A PDF Identity-H font resource exceeds its two-byte CID range")
        })?;
        self.unicode_codes.insert(key, cid);
        Ok(cid)
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PageUse {
    pub resources: usize,
    pub page: usize,
    pub content: usize,
    pub parent: usize,
    pub attributes: String,
    pub extra_resources: String,
    pub startup: Vec<LiteralPaint>,
    pub xobjects: BTreeMap<String, usize>,
    pub objects: BTreeSet<usize>,
    pub group: usize,
    pub annotations: Vec<usize>,
    pub beads: Vec<usize>,
    pub compress_level: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BindingKey {
    Map(usize),
    Unicode(String),
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PDFResources {
    pub reserved_objects: usize,
    pub scalable_fonts: BTreeMap<(String, BindingKey), usize>,
    pub fonts: BTreeMap<usize, usize>,
    pub font_groups: Vec<FontUse>,
    pub pages: Vec<PageUse>,
    pub page_groups: Vec<(usize, Vec<usize>)>,
    pub page_numbers: BTreeMap<i64, usize>,
}
impl PDFResources {
    pub fn reserve_object(&mut self) -> usize {
        self.reserved_objects += 1;
        self.reserved_objects
    }
    pub fn use_font(
        &mut self,
        mut number: usize,
        catalog: &mut FontCatalog,
        env: &mut Environment,
        warn: &mut dyn FnMut(&str),
    ) -> Result<usize> {
        if number == 0 {
            return Err(Error::value("Invalid PDF font identifier"));
        }
        let mut expansion_members = Vec::new();
        let mut visiting = BTreeSet::new();
        while let Some(base) = catalog.items[number].expansion_base.filter(|_| {
            catalog.items[number]
                .expansion_policy
                .is_some_and(|policy| policy.automatic)
        }) {
            if !visiting.insert(number) {
                return Err(Error::value("Cycle in PDF expansion font resources"));
            }
            expansion_members.push(number);
            number = base;
        }
        let suppress =
            crate::typesetting::nodes::parameter(env, "pdfsuppresswarningdupmap", 0) != 0;
        catalog.bind(number, suppress, warn)?;
        let font = &catalog.items[number];
        if font.virtual_font.is_some() {
            return Err(Error::value(
                "PDF font command cannot be used with virtual font",
            ));
        }
        let group = if let Some(&group) = self.fonts.get(&number) {
            group
        } else {
            catalog.note_pdf_use(&mut env.values, warn);
            let font = &catalog.items[number];
            let scalable = font.materialization.pdf_kind == "scalable";
            let key = (
                font.name.clone(),
                if let Some(font) = &font.unicode_font {
                    BindingKey::Unicode(font.sha256.clone())
                } else {
                    BindingKey::Map(font.materialization.map_identity)
                },
            );
            let group = if scalable {
                self.scalable_fonts.get(&key).copied()
            } else {
                None
            };
            let group = if let Some(group) = group {
                self.font_groups[group].members.insert(number);
                group
            } else {
                let object_number = self.reserve_object();
                let group = self.font_groups.len();
                self.font_groups.push(FontUse {
                    font: number,
                    object_number,
                    members: BTreeSet::from([number]),
                    unicode_codes: BTreeMap::new(),
                });
                if scalable {
                    self.scalable_fonts.insert(key, group);
                }
                group
            };
            self.fonts.insert(number, group);
            group
        };
        for number in expansion_members {
            self.font_groups[group].members.insert(number);
            self.fonts.insert(number, group);
        }
        Ok(group)
    }
    pub fn page_reference(&mut self, number: i64) -> Result<usize> {
        if number < 1 {
            return Err(Error::value("Invalid PDF page number"));
        }
        if let Some(&page) = self.page_numbers.get(&number) {
            return Ok(page);
        }
        let page = self.reserve_object();
        self.page_numbers.insert(number, page);
        Ok(page)
    }
    pub fn begin_page(&mut self) -> Result<usize> {
        let resources = self.reserve_object();
        let page = self.page_reference(self.pages.len() as i64 + 1)?;
        let content = self.reserve_object();
        self.pages.push(PageUse {
            resources,
            page,
            content,
            ..PageUse::default()
        });
        Ok(self.pages.len() - 1)
    }
    pub fn finish_page(&mut self, index: usize) -> Result<()> {
        if index >= self.pages.len() {
            return Err(Error::value("Invalid PDF page reservation"));
        }
        if self
            .page_groups
            .last()
            .is_none_or(|(_, children)| children.len() == 6)
        {
            let parent = self.reserve_object();
            self.page_groups.push((parent, Vec::new()));
        }
        let (parent, children) = self.page_groups.last_mut().unwrap();
        children.push(self.pages[index].page);
        self.pages[index].parent = *parent;
        Ok(())
    }
    pub fn writer(&self) -> PdfWriter {
        PdfWriter::new(self.reserved_objects, 0, 0)
    }
}
