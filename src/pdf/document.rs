//! Document controls freeze at actual writes; raw objects retain their emission boundary.
use crate::{
    core::{
        constants::Constants as C,
        errors::{Error, Result},
        units::sp_to_bp,
    },
    pdf::{
        extraction::ExtractionSpaces,
        forms::Form,
        graphics::{Affine, ColorStack, LiteralPaint},
        image_records::{ImagePolicy, ImageUse},
        imports::{ImportObjects, ImportReferences, SourceKey},
        jbig2::GlobalUse,
        navigation::{Navigation, NavigationContext},
        resources::{PDFResources, PageUse},
        writer::{PdfWriter, latin1},
    },
    typesetting::model::PdfEffect,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::SystemTime,
};
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SourceObject {
    pub text: Option<String>,
    pub stream: bool,
    pub attributes: String,
    pub directory: String,
    pub file: bool,
    pub data: Option<Arc<[u8]>>,
    pub compress_level: Option<i64>,
}
pub trait DocumentServices {
    fn parameter(&self, name: &str) -> i64;
    fn token_parameter(&mut self, name: &str) -> Result<String>;
    fn set_parameter(&mut self, name: &str, value: i64);
    fn exact_repro(&self) -> bool;
    fn decimal_assigned(&self) -> bool;
    fn warning(&mut self, text: &str);
    fn resolve_file(&mut self, name: &str, directory: &str) -> Result<Vec<u8>>;
    fn job_name(&self) -> String;
    fn dummy_space_font(&mut self) -> Result<usize>;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PDFDocument {
    pub objects: BTreeMap<usize, SourceObject>,
    pub last_object: usize,
    pub return_value: i64,
    pub last_x: i64,
    pub last_y: i64,
    pub version: Option<(u8, u8)>,
    pub decimal_digits: Option<usize>,
    pub draft: Option<bool>,
    pub object_compression: i64,
    pub object_stream_numbers: Vec<usize>,
    pub object_stream_count: usize,
    pub space_font_name: String,
    pub space_font: Option<usize>,
    pub extraction: ExtractionSpaces,
    pub page_height: i64,
    pub current_page: Option<PageUse>,
    pub info: String,
    pub catalog: String,
    pub names: String,
    pub trailer: String,
    pub trailer_id: String,
    pub open_action: String,
    pub open_action_number: usize,
    pub colors: Vec<ColorStack>,
    pub saved_positions: Vec<(i64, i64)>,
    pub shipping_page: bool,
    pub forms: BTreeMap<usize, Form>,
    pub last_form: usize,
    pub images: BTreeMap<usize, ImageUse>,
    #[serde(skip)]
    pub raster_cache: Arc<std::sync::Mutex<super::raster::RasterCache>>,
    pub last_image: usize,
    pub last_image_pages: usize,
    pub last_image_depth: u32,
    pub imported_objects: ImportObjects,
    pub imported_references: ImportReferences,
    pub transparency_group: usize,
    pub frozen_image_policy: Option<ImagePolicy>,
    pub jbig2_globals: BTreeMap<SourceKey, GlobalUse>,
    pub resource_suffix: Option<String>,
    pub navigation: Navigation,
}
impl Default for PDFDocument {
    fn default() -> Self {
        Self {
            objects: BTreeMap::new(),
            last_object: 0,
            return_value: 0,
            last_x: 0,
            last_y: 0,
            version: None,
            decimal_digits: None,
            draft: None,
            object_compression: 0,
            object_stream_numbers: Vec::new(),
            object_stream_count: 0,
            space_font_name: "pdftexspace".into(),
            space_font: None,
            extraction: ExtractionSpaces::default(),
            page_height: 0,
            current_page: None,
            info: String::new(),
            catalog: String::new(),
            names: String::new(),
            trailer: String::new(),
            trailer_id: String::new(),
            open_action: String::new(),
            open_action_number: 0,
            colors: vec![ColorStack::new("0 g 0 G".into(), "direct".into(), true)],
            saved_positions: Vec::new(),
            shipping_page: true,
            forms: BTreeMap::new(),
            last_form: 0,
            images: BTreeMap::new(),
            raster_cache: Default::default(),
            last_image: 0,
            last_image_pages: 0,
            last_image_depth: 0,
            imported_objects: BTreeMap::new(),
            imported_references: BTreeMap::new(),
            transparency_group: 0,
            frozen_image_policy: None,
            jbig2_globals: BTreeMap::new(),
            resource_suffix: None,
            navigation: Navigation::default(),
        }
    }
}
impl PDFDocument {
    pub fn reset_job(&mut self) {
        self.version = None;
        self.decimal_digits = None;
        self.draft = None;
        self.last_x = 0;
        self.last_y = 0;
        self.current_page = None;
        self.saved_positions.clear();
        self.navigation.reset_traversal();
    }
    pub fn form_roots(&self) -> Vec<i64> {
        let mut roots = Vec::new();
        self.append_form_roots(&mut roots);
        roots
    }
    pub(crate) fn append_form_roots(&self, roots: &mut Vec<i64>) {
        roots.extend(
            self.forms
                .values()
                .map(|f| f.box_ptr)
                .filter(|&p| p != C::NULL),
        );
    }
    pub fn check_version(&mut self, s: &mut impl DocumentServices) -> Result<(u8, u8)> {
        let major = s.parameter("pdfmajorversion");
        let minor = s.parameter("pdfminorversion");
        if major < 1 || major > 255 || !(0..=9).contains(&minor) {
            return Err(Error::value(
                "Invalid PDF version: major must be positive and minor in 0..9",
            ));
        }
        let version = (major as u8, minor as u8);
        let draft = s.parameter("pdfdraftmode") > 0;
        if self.version.is_none() {
            self.version = Some(version);
            self.object_compression = s.parameter("pdfobjcompresslevel").clamp(0, 3);
            if self.object_compression != 0 && version < (1, 5) {
                s.warning("Object streams require PDF 1.5; compression disabled");
                self.object_compression = 0
            }
            self.draft = Some(draft);
            if self.frozen_image_policy.is_none() {
                self.frozen_image_policy = Some(ImagePolicy {
                    high_color: s.parameter("pdfimagehicolor") > 0 && version >= (1, 5),
                    allow_alpha: version >= (1, 4),
                    apply_gamma: s.parameter("pdfimageapplygamma") > 0,
                    gamma: s.parameter("pdfgamma"),
                    image_gamma: s.parameter("pdfimagegamma"),
                    copy_fonts: s.parameter("pdfinclusioncopyfonts") > 0,
                });
            }
        } else if self.version != Some(version) {
            return Err(
                Error::value("PDF version cannot be changed after data is written")
                    .with_issue(crate::core::recovery::Issue::PdfOperation),
            );
        }
        if self.draft != Some(draft) {
            return Err(Error::value(
                "PDF draft mode cannot change after data is written",
            ));
        }
        if draft {
            s.set_parameter("pdfcompresslevel", 0);
            self.object_compression = 0
        }
        Ok(version)
    }
    pub fn precision(&mut self, s: &impl DocumentServices) -> usize {
        *self.decimal_digits.get_or_insert_with(|| {
            if s.exact_repro() || s.decimal_assigned() {
                s.parameter("pdfdecimaldigits").clamp(0, 4) as usize
            } else {
                9
            }
        })
    }
    pub fn compression(s: &impl DocumentServices) -> i64 {
        s.parameter("pdfcompresslevel").clamp(0, 9)
    }
    pub fn reserve_object_stream(&mut self, r: &mut PDFResources, count: usize) {
        if self.object_compression != 0 {
            for _ in 0..count {
                if self.object_stream_count % super::writer::OBJECTS_PER_STREAM == 0 {
                    self.object_stream_numbers.push(r.reserve_object())
                }
                self.object_stream_count += 1
            }
        }
    }
    pub fn reserve(
        &mut self,
        s: &mut impl DocumentServices,
        r: &mut PDFResources,
        mut number: Option<usize>,
    ) -> usize {
        if number.is_some_and(|n| !self.objects.contains_key(&n) || self.objects[&n].text.is_some())
        {
            s.warning(r"\pdfobj: invalid object number being ignored");
            self.return_value = -1;
            number = None
        }
        number.unwrap_or_else(|| {
            let n = r.reserve_object();
            self.objects.insert(n, SourceObject::default());
            n
        })
    }
    pub fn emit(
        &mut self,
        s: &mut impl DocumentServices,
        r: &mut PDFResources,
        n: usize,
    ) -> Result<()> {
        let item = self.objects.get(&n).ok_or_else(|| {
            Error::value("Cannot find referenced PDF object")
                .with_issue(crate::core::recovery::Issue::PdfOperation)
        })?;
        if item.data.is_some() {
            return Ok(());
        }
        let text = item.text.clone().ok_or_else(|| {
            Error::value("Referenced PDF object has no contents")
                .with_issue(crate::core::recovery::Issue::PdfOperation)
        })?;
        let file = item.file;
        let directory = item.directory.clone();
        let stream = item.stream;
        self.check_version(s)?;
        let level = Self::compression(s);
        if !stream {
            self.reserve_object_stream(r, 1)
        }
        let data = if file {
            s.resolve_file(&text, &directory)?
        } else {
            latin1(&text)?
        };
        let item = self.objects.get_mut(&n).unwrap();
        item.data = Some(data.into());
        item.compress_level = Some(level);
        Ok(())
    }
    pub fn resource_name(&mut self, s: &impl DocumentServices, base: &str) -> String {
        let suffix = self.resource_suffix.get_or_insert_with(|| {
            if s.parameter("pdfuniqueresname") == 0 {
                return String::new();
            }
            let stamp = format!(
                "{}:{}",
                s.job_name(),
                crate::resources::host::local_date(crate::resources::host::now()).to_rfc3339()
            );
            let mut value = crc32fast::hash(stamp.as_bytes());
            let alphabet = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
            let mut out = String::new();
            for _ in 0..6 {
                out.push(alphabet[(value % 62) as usize] as char);
                value /= 62
            }
            out
        });
        format!("{base}{suffix}")
    }
    pub fn begin_page(
        &mut self,
        s: &mut impl DocumentServices,
        r: &mut PDFResources,
        height: i64,
        x: i64,
        y: i64,
    ) -> Result<()> {
        self.check_version(s)?;
        self.precision(s);
        self.extraction.reset_cursor();
        self.page_height = height;
        let index = r.begin_page()?;
        let mut page = r.pages[index].clone();
        page.compress_level = Self::compression(s);
        page.startup = self
            .colors
            .iter()
            .filter(|stack| {
                stack.page_start
                    && stack
                        .page
                        .last()
                        .is_some_and(|text| text != "" && text != "0 g 0 G")
            })
            .map(|stack| {
                LiteralPaint::new(stack.mode.clone(), stack.page.last().unwrap().clone(), x, y)
            })
            .collect();
        self.current_page = Some(page);
        self.saved_positions.clear();
        self.navigation.matrix = Affine::default();
        self.navigation.saved_matrices.clear();
        self.navigation.last_bead = None;
        Ok(())
    }
    pub fn finish_page(
        &mut self,
        s: &mut impl DocumentServices,
        r: &mut PDFResources,
    ) -> Result<()> {
        if !self.saved_positions.is_empty() {
            return Err(Error::value("Unmatched PDF save at end of shipment")
                .with_issue(crate::core::recovery::Issue::PdfOperation));
        }
        let attributes = s.token_parameter("pdfpageattr")?;
        let extra = s.token_parameter("pdfpageresources")?;
        self.reserve_object_stream(r, 2);
        let mut page = self
            .current_page
            .take()
            .ok_or_else(|| Error::runtime("Missing shipment page"))?;
        page.attributes = attributes;
        page.extra_resources = extra;
        let index = r.pages.len() - 1;
        r.pages[index] = page.clone();
        r.finish_page(index)?;
        page.parent = r.pages[index].parent;
        self.navigation.finish_page(r, &mut page)?;
        r.pages[index] = page;
        Ok(())
    }
    pub fn ship(
        &mut self,
        s: &mut impl DocumentServices,
        r: &mut PDFResources,
        effect: &PdfEffect,
        x: i64,
        y: i64,
        leaders: bool,
        source: Option<i64>,
        mag: i64,
    ) -> Result<Option<(String, String)>> {
        use PdfEffect::*;
        if matches!(effect, ExtractionOn | ExtractionOff | FakeSpace) {
            self.extraction.active = true
        }
        match effect {
            ExtractionOn => {
                self.extraction.enabled = true;
                return Ok(Some(("space_on".into(), String::new())));
            }
            ExtractionOff => {
                self.extraction.enabled = false;
                return Ok(Some(("space_off".into(), String::new())));
            }
            FakeSpace => {
                let font = s.dummy_space_font()?;
                return Ok(Some(("fake_space".into(), font.to_string())));
            }
            Annotation(_) | StartLink(_) | EndLink | Destination(_) | Thread(_)
            | StartThread(_) | EndThread | RunningLinkOn | RunningLinkOff => {
                let precision = self.precision(s);
                let link_margin = s.parameter("pdflinkmargin");
                let dest_margin = s.parameter("pdfdestmargin");
                let thread_margin = s.parameter("pdfthreadmargin");
                let suppress_duplicate = s.parameter("pdfsuppresswarningdupdest") > 0;
                let exact_repro = s.exact_repro();
                let page = self
                    .current_page
                    .as_mut()
                    .ok_or_else(|| Error::value("PDF region outside shipment"))?;
                let mut warn = |text: &str| s.warning(text);
                let mut c = NavigationContext {
                    resources: r,
                    page,
                    page_height: self.page_height,
                    shipping_page: self.shipping_page,
                    exact_repro,
                    link_margin,
                    dest_margin,
                    thread_margin,
                    suppress_duplicate,
                    precision,
                    magnification: mag,
                    warning: &mut warn,
                };
                self.navigation
                    .ship(&mut c, effect, x, y, leaders, source)?;
                return Ok(None);
            }
            SavePosition => {
                self.last_x = x;
                self.last_y = self.page_height - y
            }
            RefObject(n) => {
                self.current_page.as_mut().unwrap().objects.insert(*n);
            }
            RefForm(n) => {
                let ordinal = self.forms[n].ordinal;
                let name = self.resource_name(s, &format!("Fm{ordinal}"));
                self.current_page
                    .as_mut()
                    .unwrap()
                    .xobjects
                    .insert(name.clone(), *n);
                return Ok(Some(("xform".into(), name)));
            }
            RefImage(n) => {
                let ordinal = self.images[n].ordinal;
                let name = self.resource_name(s, &format!("Im{ordinal}"));
                let image = self.images.get_mut(n).unwrap();
                let page = self.current_page.as_mut().unwrap();
                page.xobjects.insert(name, *n);
                if image.group == -1 && page.group == 0 {
                    image.group = r.reserve_object() as i64
                }
                if image.group > 0 && page.group == 0 {
                    page.group = image.group as usize
                }
                return Ok(Some(("image".into(), n.to_string())));
            }
            ColorStack {
                number,
                action,
                text,
            } => {
                let mut warn = |text: &str| s.warning(text);
                return self.colors[*number]
                    .apply(action.as_str(), text, self.shipping_page, &mut warn)
                    .map(Some);
            }
            SetMatrix(text) => {
                self.navigation.matrix =
                    Affine::around(text, sp_to_bp(x), sp_to_bp(self.page_height - y))?
                        .then(self.navigation.matrix);
                return Ok(Some(("matrix".into(), text.clone())));
            }
            Save => {
                self.saved_positions.push((x, y));
                self.navigation.saved_matrices.push(self.navigation.matrix);
                return Ok(Some(("save".into(), "q".into())));
            }
            Restore => {
                if let Some(position) = self.saved_positions.pop() {
                    if position != (x, y) {
                        s.warning("PDF restore is at a different position")
                    }
                } else {
                    s.warning("PDF restore has no matching save")
                }
                if let Some(matrix) = self.navigation.saved_matrices.pop() {
                    self.navigation.matrix = matrix
                }
                return Ok(Some(("restore".into(), "Q".into())));
            }
            _ => {}
        }
        Ok(None)
    }
    pub fn write_objects(
        &self,
        w: &mut PdfWriter,
        r: &PDFResources,
        precision: usize,
    ) -> Result<BTreeSet<usize>> {
        let mut free = BTreeSet::new();
        for (&n, item) in &self.objects {
            if let Some(data) = &item.data {
                if item.stream {
                    w.add_stream_object(data, &item.attributes, Some(n), item.compress_level)?;
                } else {
                    w.add_object(data.as_ref().to_vec(), Some(n), 1)?;
                }
            } else {
                free.insert(n);
            }
        }
        free.extend(
            r.page_numbers
                .iter()
                .filter(|(page, _)| **page > r.pages.len() as i64)
                .map(|(_, n)| *n),
        );
        if !self.open_action.is_empty() {
            w.add_text_object(&self.open_action, Some(self.open_action_number), 1)?;
        }
        free.extend(super::forms::write(
            &self.forms,
            w,
            r,
            precision,
            self.resource_suffix.as_deref().unwrap_or(""),
        )?);
        free.extend(super::images::write(self, w)?);
        Ok(free)
    }
    pub fn information(&self, s: &impl DocumentServices, creation: SystemTime) -> Result<String> {
        let mut text = self.info.clone();
        let supplied = super::parsing::dictionary(&self.info)?;
        let mut defaults = vec![
            ("Producer", "(pdfTeX-1.40.25)".into()),
            ("Creator", "(TeX)".into()),
        ];
        if s.parameter("pdfinfoomitdate") == 0 {
            let date = crate::language::pdf_commands::pdf_date(creation);
            defaults.push(("CreationDate", format!("({date})")));
            defaults.push(("ModDate", format!("({date})")));
        }
        for (name, value) in defaults {
            if supplied.get(name).is_none() {
                text.push_str(&format!(" /{name} {value}"))
            }
        }
        Ok(text)
    }
}
