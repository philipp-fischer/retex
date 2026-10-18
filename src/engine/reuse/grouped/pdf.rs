//! Construction recipes allocate fresh PDF ownership. Numeric references are
//! admitted only when the allocator entry state matches; a changed namespace
//! falls back instead of guessing how numbers embedded in TeX tokens map.
use super::*;
use crate::pdf::{
    backend::PDFBackend,
    image_records::{ImagePolicy, ImageUse},
};
#[derive(Clone, PartialEq)]
pub(super) struct Scalars {
    version: Option<(u8, u8)>,
    precision: Option<usize>,
    draft: Option<bool>,
    compression: i64,
    transparency: usize,
    policy: Option<ImagePolicy>,
    last: usize,
    pages: usize,
    depth: u32,
}
impl Scalars {
    fn read(pdf: &PDFBackend) -> Self {
        let d = &pdf.document;
        Self {
            version: d.version,
            precision: d.decimal_digits,
            draft: d.draft,
            compression: d.object_compression,
            transparency: d.transparency_group,
            policy: d.frozen_image_policy,
            last: d.last_image,
            pages: d.last_image_pages,
            depth: d.last_image_depth,
        }
    }
    fn apply(&self, pdf: &mut PDFBackend) {
        let d = &mut pdf.document;
        d.version = self.version;
        d.decimal_digits = self.precision;
        d.draft = self.draft;
        d.object_compression = self.compression;
        d.transparency_group = self.transparency;
        d.frozen_image_policy = self.policy;
        d.last_image = self.last;
        d.last_image_pages = self.pages;
        d.last_image_depth = self.depth;
    }
}
// A dependency on an already existing image must not keep its potentially
// large raw/encoded payload alive in every entry. Weak witnesses are exact:
// expiry rejects reuse, and distinct live allocations compare complete bytes.
#[derive(Clone)]
struct ImageGuard {
    metadata: ImageUse,
    source: std::sync::Weak<[u8]>,
    data: Option<std::sync::Weak<[u8]>>,
    held: Option<(Arc<[u8]>, Option<Arc<[u8]>>)>,
}
impl ImageGuard {
    fn new(image: &ImageUse, compact: bool) -> Self {
        let mut metadata = image.clone();
        metadata.source = Arc::from([]);
        metadata.data = None;
        Self {
            metadata,
            source: Arc::downgrade(&image.source),
            data: image.data.as_ref().map(Arc::downgrade),
            held: (!compact).then(|| (image.source.clone(), image.data.clone())),
        }
    }
    fn matches(&self, image: &ImageUse) -> bool {
        let Some(source) = self.source.upgrade() else {
            return false;
        };
        if !(Arc::ptr_eq(&source, &image.source) || source == image.source) {
            return false;
        }
        match (&self.data, &image.data) {
            (None, None) => {}
            (Some(old), Some(now)) => {
                let Some(old) = old.upgrade() else {
                    return false;
                };
                if !(Arc::ptr_eq(&old, now) || old == *now) {
                    return false;
                }
            }
            _ => return false,
        }
        // Comparing the complete normalized record keeps this conservative if
        // ImageUse gains new fields. This clones only metadata, never bytes.
        let mut metadata = image.clone();
        metadata.source = self.metadata.source.clone();
        metadata.data = None;
        self.metadata == metadata
    }
    fn bytes(&self) -> usize {
        if let Some((source, data)) = &self.held {
            return source.len() + data.as_ref().map_or(0, |v| v.len()) + 2048;
        }
        (bincode::serialized_size(&self.metadata).unwrap_or(u64::MAX / 4) as usize)
            .saturating_mul(4)
            + 128
    }
}
#[derive(Clone)]
pub(super) struct PdfGuard {
    reserved: usize,
    images: BTreeMap<usize, ImageGuard>,
    scalars: Scalars,
    forms: usize,
    objects: usize,
    imports: usize,
    colors: usize,
}
pub(super) struct Recipe {
    additional: usize,
    images: Vec<(usize, ImageUse)>,
    scalars: Scalars,
}
impl PdfGuard {
    pub fn new(pdf: &PDFBackend, compact: bool) -> Self {
        Self {
            reserved: pdf.resources.reserved_objects,
            images: pdf
                .document
                .images
                .iter()
                .map(|(&id, image)| (id, ImageGuard::new(image, compact)))
                .collect(),
            scalars: Scalars::read(pdf),
            forms: pdf.document.forms.len(),
            objects: pdf.document.objects.len(),
            imports: pdf.document.imported_objects.len(),
            colors: pdf.document.colors.len(),
        }
    }
    pub fn matches(&self, pdf: &PDFBackend) -> bool {
        self.reserved == pdf.resources.reserved_objects
            && self.images.len() == pdf.document.images.len()
            && self.images.iter().all(|(id, old)| {
                pdf.document
                    .images
                    .get(id)
                    .is_some_and(|now| old.matches(now))
            })
            && self.scalars == Scalars::read(pdf)
            && self.forms == pdf.document.forms.len()
            && self.objects == pdf.document.objects.len()
            && self.imports == pdf.document.imported_objects.len()
            && self.colors == pdf.document.colors.len()
    }
    pub fn finish(&self, pdf: &PDFBackend) -> Option<Recipe> {
        if self.forms != pdf.document.forms.len()
            || self.objects != pdf.document.objects.len()
            || self.imports != pdf.document.imported_objects.len()
            || self.colors != pdf.document.colors.len()
            || self.images.iter().any(|(key, value)| {
                pdf.document
                    .images
                    .get(key)
                    .is_none_or(|now| !value.matches(now))
            })
        {
            return None;
        }
        let additional = pdf.resources.reserved_objects.checked_sub(self.reserved)?;
        if additional > 256 {
            return None;
        }
        let images = pdf
            .document
            .images
            .iter()
            .filter(|(id, _)| !self.images.contains_key(id))
            .map(|(&id, v)| (id, v.clone()))
            .collect::<Vec<_>>();
        // Emitted images have mutable writer/import state. Shipment remains
        // ordinary execution, including decoding, placement and deferred PDF effects.
        if images.iter().any(|(id, i)| {
            *id <= self.reserved
                || *id > pdf.resources.reserved_objects
                || i.data.is_some()
                || !i.dictionary.is_empty()
        }) {
            return None;
        }
        let mut owned = images
            .iter()
            .map(|(id, _)| *id)
            .collect::<std::collections::BTreeSet<_>>();
        if pdf.document.transparency_group != self.scalars.transparency {
            owned.insert(pdf.document.transparency_group);
        }
        if owned.len() != additional
            || owned
                .iter()
                .copied()
                .ne(self.reserved + 1..=pdf.resources.reserved_objects)
        {
            return None;
        }
        Some(Recipe {
            additional,
            images,
            scalars: Scalars::read(pdf),
        })
    }
    pub fn bytes(&self) -> usize {
        self.images.values().map(ImageGuard::bytes).sum::<usize>() + 512
    }
}
impl Recipe {
    pub fn bytes(&self) -> usize {
        self.images
            .iter()
            .map(|(_, i)| i.source.len() + 2048)
            .sum::<usize>()
            + 512
    }
    pub fn apply(&self, pdf: &mut PDFBackend) -> BTreeMap<usize, usize> {
        let mut map = pdf
            .document
            .images
            .keys()
            .map(|&id| (id, id))
            .collect::<BTreeMap<_, _>>();
        let base = pdf.resources.reserved_objects;
        for offset in 1..=self.additional {
            let id = pdf.resources.reserve_object();
            debug_assert_eq!(id, base + offset);
            map.insert(base + offset, id);
        }
        for (old, image) in &self.images {
            let mut image = image.clone();
            image.number = map[old];
            if image.group > 0 {
                image.group = map
                    .get(&(image.group as usize))
                    .copied()
                    .unwrap_or(image.group as usize) as i64;
            }
            pdf.document.images.insert(image.number, image);
        }
        self.scalars.apply(pdf);
        map
    }
}

#[cfg(test)]
mod guard_tests {
    use super::*;
    #[test]
    fn prior_images_are_weak_exact_dependencies_and_expiry_only_causes_misses() {
        let mut image = ImageUse::new(
            1,
            1,
            "figure.png".into(),
            vec![1; 1024 * 1024],
            "PNG".into(),
            (10, 20),
            (10, 20, 0),
        );
        image.data = Some(vec![2; 1024 * 1024].into());
        let guard = ImageGuard::new(&image, true);
        let held = ImageGuard::new(&image, false);
        assert!(guard.bytes() < 4096);
        assert!(guard.matches(&image));
        let mut different = image.clone();
        different.width += 1;
        assert!(!guard.matches(&different));
        different = image.clone();
        different.source = vec![9; 1024 * 1024].into();
        assert!(!guard.matches(&different));
        different = image.clone();
        different.data = Some(vec![9; 1024 * 1024].into());
        assert!(!guard.matches(&different));
        let mut equal = image.clone();
        equal.source = image.source.to_vec().into();
        equal.data = image.data.as_ref().map(|a| a.to_vec().into());
        assert!(guard.matches(&equal));
        drop(different);
        drop(image);
        assert!(held.matches(&equal));
        drop(held);
        assert!(!guard.matches(&equal));
    }
}
