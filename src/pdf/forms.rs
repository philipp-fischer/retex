//! Retained TeX boxes converted once to reusable Form XObjects.
use crate::{
    core::{
        constants::Constants as C,
        errors::{Error, Result},
        units::sp_to_bp,
    },
    pdf::{resources::PDFResources, writer::PdfWriter},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Form {
    pub number: usize,
    pub ordinal: usize,
    pub box_ptr: i64,
    pub width: i64,
    pub height: i64,
    pub depth: i64,
    pub attributes: String,
    pub extra_resources: String,
    pub resources: usize,
    pub xobjects: BTreeMap<String, usize>,
    pub objects: BTreeSet<usize>,
    pub codes: BTreeMap<usize, BTreeSet<u32>>,
    pub data: Option<Arc<[u8]>>,
    pub emitting: bool,
    pub group: usize,
    pub compress_level: i64,
}
impl Form {
    pub fn new(
        number: usize,
        ordinal: usize,
        box_ptr: i64,
        width: i64,
        height: i64,
        depth: i64,
    ) -> Self {
        Self {
            number,
            ordinal,
            box_ptr,
            width,
            height,
            depth,
            attributes: String::new(),
            extra_resources: String::new(),
            resources: 0,
            xobjects: BTreeMap::new(),
            objects: BTreeSet::new(),
            codes: BTreeMap::new(),
            data: None,
            emitting: false,
            group: 0,
            compress_level: 0,
        }
    }
}
pub trait FormServices {
    fn check_version(&mut self) -> Result<()>;
    fn precision(&mut self) -> usize;
    fn compression(&self) -> i64;
    fn reserve(&mut self) -> usize;
    fn capture_form(&mut self, form: &mut Form) -> Result<()>;
    fn release_box(&mut self, pointer: i64);
}
pub fn emit(s: &mut impl FormServices, form: &mut Form) -> Result<()> {
    let mut execution = FormEmission::new();
    let mut returned = None;
    loop {
        match execution.resume(s, form, returned.take())? {
            FormStep::Complete => return Ok(()),
            FormStep::Capture => returned = Some(s.capture_form(form)),
        }
    }
}
pub(crate) enum FormStep {
    Capture,
    Complete,
}
pub(crate) struct FormEmission {
    phase: u8,
}
impl FormEmission {
    pub(crate) fn new() -> Self {
        Self { phase: 0 }
    }
    pub(crate) fn resume(
        &mut self,
        s: &mut impl FormServices,
        form: &mut Form,
        returned: Option<Result<()>>,
    ) -> Result<FormStep> {
        match self.phase {
            0 => {
                if returned.is_some() {
                    return Err(Error::runtime("Unrequested form capture result"));
                }
                if form.emitting {
                    return Err(Error::value("Cycle in PDF form references")
                        .with_issue(crate::core::recovery::Issue::PdfOperation));
                }
                if form.data.is_some() {
                    self.phase = 2;
                    return Ok(FormStep::Complete);
                }
                s.check_version()?;
                s.precision();
                form.compress_level = s.compression();
                form.resources = s.reserve();
                form.emitting = true;
                self.phase = 1;
                Ok(FormStep::Capture)
            }
            1 => {
                let result =
                    returned.ok_or_else(|| Error::runtime("Missing form capture result"))?;
                let pointer = std::mem::replace(&mut form.box_ptr, C::NULL);
                form.emitting = false;
                s.release_box(pointer);
                self.phase = 2;
                result?;
                Ok(FormStep::Complete)
            }
            _ => Err(Error::runtime("Form emission already completed")),
        }
    }
}
pub fn write(
    forms: &BTreeMap<usize, Form>,
    w: &mut PdfWriter,
    r: &PDFResources,
    precision: usize,
    suffix: &str,
) -> Result<BTreeSet<usize>> {
    let mut free = BTreeSet::new();
    for form in forms.values() {
        let Some(data) = &form.data else {
            free.insert(form.number);
            if form.resources != 0 {
                free.insert(form.resources);
            }
            continue;
        };
        let group = if form.group != 0 {
            format!("/Group {} 0 R", form.group)
        } else {
            String::new()
        };
        w.add_stream_object(data,&format!("/Type /XObject /Subtype /Form /FormType 1 /BBox [0 0 {:.*} {:.*}] /Matrix [1 0 0 1 0 0] /Resources {} 0 R {group} {}",precision,sp_to_bp(form.width),precision,sp_to_bp(form.height+form.depth),form.resources,form.attributes),Some(form.number),Some(form.compress_level))?;
        let fonts = r
            .font_groups
            .iter()
            .filter(|g| g.members.iter().any(|id| form.codes.contains_key(id)))
            .map(|g| format!("/F{}{suffix} {} 0 R", g.font, g.object_number))
            .collect::<Vec<_>>()
            .join(" ");
        let objects = form
            .xobjects
            .iter()
            .map(|(name, n)| format!("/{name} {n} 0 R"))
            .collect::<Vec<_>>()
            .join(" ");
        w.add_text_object(
            &format!(
                "<< /Font << {fonts} >> /XObject << {objects} >> {} >>",
                form.extra_resources
            ),
            Some(form.resources),
            1,
        )?;
    }
    Ok(free)
}
