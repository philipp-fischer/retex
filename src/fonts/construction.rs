//! Bounded semantic receipts for classic TFM construction. These describe
//! newly owned font slots, not PDF font objects or outline-cache entries.
use super::{
    catalog::{FontCatalog, FontFactory},
    model::FontInfo,
};
use crate::core::{characters::LanguageProfile, persistence::SharedTable};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    path::PathBuf,
    rc::Rc,
};

#[derive(Clone)]
pub(crate) struct Policy {
    loaded: BTreeMap<(String, i64), usize>,
    fallback: SharedTable<String, super::maps::FontMapEntry>,
    directory: PathBuf,
    encoding: Option<String>,
    explicit: bool,
    ini: bool,
    exact: bool,
    bytes: usize,
}
impl Policy {
    fn new(fonts: &FontCatalog) -> Option<Self> {
        if fonts.loaded.len() > 4096 || fonts.fallback_maps.fonts.len() > 4096 {
            return None;
        }
        let mut bytes = fonts
            .loaded
            .keys()
            .map(|(name, _)| name.len() + 256)
            .sum::<usize>()
            + 1024;
        for (key, value) in &fonts.fallback_maps.fonts {
            bytes = bytes.checked_add(
                key.len()
                    + value.tfm_name.len()
                    + 512
                    + value.ps_name.as_ref().map_or(0, String::len)
                    + value.encoding.as_ref().map_or(0, String::len)
                    + value.fontfile.as_ref().map_or(0, String::len)
                    + value.options.as_ref().map_or(0, String::len)
                    + value.directory.as_ref().map_or(0, String::len)
                    + value.subfont.as_ref().map_or(0, |v| v.len() * 4),
            )?;
            if bytes > 4 * 1024 * 1024 {
                return None;
            }
        }
        if bytes > 4 * 1024 * 1024 {
            return None;
        }
        Some(Self {
            loaded: fonts.loaded.clone(),
            fallback: fonts.fallback_maps.fonts.clone(),
            directory: fonts.directory.clone(),
            encoding: fonts.encoding_file.clone(),
            explicit: fonts.explicit_directory,
            ini: fonts.ini,
            exact: fonts.exact_repro,
            bytes,
        })
    }
    fn same_policy(&self, fonts: &FontCatalog) -> bool {
        std::ptr::fn_addr_eq(fonts.factory, super::resources::create_font as FontFactory)
            && fonts.language_profile == LanguageProfile::Pdftex
            && self.directory == fonts.directory
            && self.encoding == fonts.encoding_file
            && self.explicit == fonts.explicit_directory
            && self.ini == fonts.ini
            && self.exact == fonts.exact_repro
            && (self
                .fallback
                .shares_storage_with(&fonts.fallback_maps.fonts)
                || self.fallback == fonts.fallback_maps.fonts)
    }
    pub fn matches(&self, fonts: &FontCatalog) -> bool {
        self.same_policy(fonts) && self.loaded == fonts.loaded
    }
}
#[derive(Clone)]
pub(crate) struct Load {
    pub order: u64,
    pub created: Option<usize>,
    loaded: Vec<((String, i64), usize)>,
}
pub(crate) struct Scope {
    pub base: usize,
    sequence: Rc<Cell<u64>>,
    unsupported: Cell<bool>,
    policy: RefCell<Option<Policy>>,
    loads: RefCell<Vec<Load>>,
    pub reads: RefCell<BTreeMap<usize, FontInfo>>,
}
pub(crate) struct Start {
    order: u64,
    count: usize,
    loaded: BTreeMap<(String, i64), usize>,
}
pub(crate) fn detached(font: &FontInfo) -> FontInfo {
    let mut font = font.clone();
    let micro = font.micro_codes.borrow().clone();
    font.micro_codes = Rc::new(RefCell::new(micro));
    if let Some(encoding) = &mut font.materialization.encoding {
        let copy = encoding.borrow().clone();
        *encoding = Rc::new(RefCell::new(copy));
    }
    font
}
impl Scope {
    pub fn new(base: usize, sequence: Rc<Cell<u64>>) -> Rc<Self> {
        Rc::new(Self {
            base,
            sequence,
            unsupported: Cell::new(false),
            policy: Default::default(),
            loads: Default::default(),
            reads: Default::default(),
        })
    }
    pub fn start(&self, fonts: &FontCatalog) -> Option<Start> {
        if self.unsupported.get()
            || self.loads.borrow().len() >= 64
            || !std::ptr::fn_addr_eq(fonts.factory, super::resources::create_font as FontFactory)
            || fonts.language_profile != LanguageProfile::Pdftex
            || self.base > 9000
        {
            self.unsupported.set(true);
            return None;
        }
        if self.policy.borrow().is_none() {
            let Some(policy) = Policy::new(fonts) else {
                self.unsupported.set(true);
                return None;
            };
            *self.policy.borrow_mut() = Some(policy);
        }
        let order = self.sequence.get();
        self.sequence.set(order + 1);
        Some(Start {
            order,
            count: fonts.items.len(),
            loaded: fonts.loaded.clone(),
        })
    }
    pub fn end(&self, start: Option<Start>, fonts: &FontCatalog, number: Option<usize>) {
        let Some(start) = start else {
            self.unsupported.set(true);
            return;
        };
        let Some(number) = number else {
            self.unsupported.set(true);
            return;
        };
        if self.sequence.get() != start.order + 1
            || fonts.items.len() > self.base + 64
            || fonts.items.len() > start.count + 1
            || fonts.items.len() < start.count
            || start
                .loaded
                .iter()
                .any(|(k, v)| fonts.loaded.get(k) != Some(v))
        {
            self.unsupported.set(true);
            return;
        }
        let created = if fonts.items.len() == start.count + 1 {
            if number != start.count {
                self.unsupported.set(true);
                return;
            }
            Some(number)
        } else {
            self.observe(fonts, number);
            None
        };
        let loaded = fonts
            .loaded
            .iter()
            .filter(|(k, _)| !start.loaded.contains_key(*k))
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        self.loads.borrow_mut().push(Load {
            order: start.order,
            created,
            loaded,
        });
    }
    pub fn observe(&self, fonts: &FontCatalog, number: usize) {
        if number >= self.base {
            return;
        }
        let mut reads = self.reads.borrow_mut();
        if !reads.contains_key(&number) {
            if reads.len() >= 64 {
                self.unsupported.set(true);
                return;
            }
            if let Some(font) = fonts.items.get(number) {
                if font.unicode_font.is_some()
                    || font.expansion_policy.is_some()
                    || font
                        .materialization
                        .resource_error
                        .as_ref()
                        .is_some_and(|s| s.len() > 65536)
                {
                    self.unsupported.set(true);
                    return;
                }
                reads.insert(number, metric_snapshot(font));
            } else {
                self.unsupported.set(true);
            }
        }
    }
    pub fn can_mutate(&self, number: usize) -> bool {
        number >= self.base
            && self
                .loads
                .borrow()
                .iter()
                .any(|l| l.created == Some(number))
    }
    pub fn finish(&self, fonts: &FontCatalog) -> Option<Recipe> {
        if self.unsupported.get() {
            return None;
        }
        let loads = self.loads.borrow().clone();
        let created = loads.iter().filter_map(|l| l.created).collect::<Vec<_>>();
        if created.iter().copied().ne(self.base..fonts.items.len()) {
            return None;
        }
        let policy = self.policy.borrow().clone();
        if let Some(policy) = &policy {
            if !policy.same_policy(fonts) {
                return None;
            }
            let mut expected = policy.loaded.clone();
            for load in &loads {
                expected.extend(load.loaded.iter().cloned());
            }
            if expected != fonts.loaded {
                return None;
            }
        }
        let mut outputs = BTreeMap::new();
        let mut retained = policy.as_ref().map_or(0, |p| p.bytes) + 2048;
        for n in created {
            let font = &fonts.items[n];
            // PDF binding/emission and virtual expansion stay ordinary. A font
            // carrying those ownership/effect states is not a portable receipt.
            if font.unicode_font.is_some()
                || font.expansion_policy.is_some()
                || font.expansion_base.is_some()
                || font.virtual_font.is_some()
                || font.materialization.map_bound
                || font.materialization.resource_ready
                || font.materialization.map_identity != 0
                || font.materialization.resource.is_some()
            {
                return None;
            }
            if let Some(enc) = &font.materialization.encoding {
                let enc = enc.borrow();
                if enc.glyph_names.iter().map(|s| s.len() + 32).sum::<usize>() > 1024 * 1024 {
                    return None;
                }
            }
            retained = retained.checked_add(bincode::serialized_size(font).ok()? as usize * 4)?;
            if retained > 8 * 1024 * 1024 {
                return None;
            }
            outputs.insert(n, detached(font));
        }
        Some(Recipe {
            policy,
            loads,
            outputs,
            retained,
        })
    }
}
pub(crate) struct Recipe {
    pub policy: Option<Policy>,
    pub loads: Vec<Load>,
    pub outputs: BTreeMap<usize, FontInfo>,
    retained: usize,
}
impl Recipe {
    pub fn empty() -> Self {
        Self {
            policy: None,
            loads: Vec::new(),
            outputs: BTreeMap::new(),
            retained: 0,
        }
    }

    pub fn matches(&self, fonts: &FontCatalog) -> bool {
        self.policy.as_ref().is_none_or(|p| p.matches(fonts))
    }
    pub fn apply(&self, load: &Load, fonts: &mut FontCatalog) {
        if let Some(n) = load.created {
            debug_assert_eq!(fonts.items.len(), n);
            fonts.items.push(detached(&self.outputs[&n]));
        }
        fonts.loaded.extend(load.loaded.iter().cloned());
    }
    pub fn bytes(&self) -> Option<usize> {
        Some(self.retained)
    }
}

fn metric_snapshot(font: &FontInfo) -> FontInfo {
    FontInfo {
        materialization: super::materialization::FontMaterialization {
            resource_error: font.materialization.resource_error.clone(),
            ..Default::default()
        },
        name: font.name.clone(),
        size: font.size.clone(),
        tfm_widths: font.tfm_widths.clone(),
        tfm_heights: font.tfm_heights.clone(),
        tfm_depths: font.tfm_depths.clone(),
        tfm_lig_kern: font.tfm_lig_kern.clone(),
        tfm_lig_start: font.tfm_lig_start.clone(),
        boundary_char: font.boundary_char.clone(),
        boundary_label: font.boundary_label.clone(),
        false_boundary_char: font.false_boundary_char.clone(),
        tfm_kerns: font.tfm_kerns.clone(),
        tfm_design_size: font.tfm_design_size.clone(),
        tfm_data: font.tfm_data.clone(),
        afm: font.afm.clone(),
        char_width: font.char_width.clone(),
        char_height: font.char_height.clone(),
        char_depth: font.char_depth.clone(),
        space_width: font.space_width.clone(),
        parameter_overrides: font.parameter_overrides.clone(),
        tfm_file: font.tfm_file.clone(),
        parameter_count: font.parameter_count.clone(),
        hyphenchar: font.hyphenchar.clone(),
        skewchar: font.skewchar.clone(),
        dynamic: font.dynamic.clone(),
        null_font: font.null_font.clone(),
        virtual_font: font.virtual_font.clone(),
        unicode_font: font.unicode_font.clone(),
        virtual_checked: font.virtual_checked.clone(),
        ligatures_disabled: font.ligatures_disabled.clone(),
        micro_codes: Rc::new(RefCell::new(font.micro_codes.borrow().clone())),
        width_overrides: font.width_overrides.clone(),
        letterspacing: font.letterspacing.clone(),
        italic_overrides: font.italic_overrides.clone(),
        expansion_policy: font.expansion_policy.clone(),
        expansion_base: font.expansion_base.clone(),
        expansion_ratio: font.expansion_ratio.clone(),
        expansion_variants: font.expansion_variants.clone(),
    }
}
