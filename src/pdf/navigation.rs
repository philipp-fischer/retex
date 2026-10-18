//! Semantic PDF navigation records and their shipment geometry.
use crate::{
    core::{
        enums::NodeKind,
        errors::{Error, Result},
        units::{bp_to_sp, sp_to_bp},
    },
    pdf::{
        graphics::Affine,
        parsing::{source_string, string},
        resources::{PDFResources, PageUse},
        writer::PdfWriter,
    },
    typesetting::model::{Node, PdfEffect},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Identifier {
    Name(String),
    Number(i64),
}
impl fmt::Display for Identifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(s) => s.fmt(f),
            Self::Number(n) => n.fmt(f),
        }
    }
}
pub type Dimensions = BTreeMap<String, i64>;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ActionTarget {
    Identifier(Identifier),
    Page(i64, String),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Action {
    pub kind: String,
    pub destination: Option<ActionTarget>,
    pub file: Option<String>,
    pub window: Option<bool>,
    pub structure: Option<Identifier>,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnnotationTemplate {
    pub number: usize,
    pub spec: Dimensions,
    pub attributes: String,
    pub action: Option<Action>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Annotation {
    pub number: usize,
    pub rectangle: [f64; 4],
    pub text: String,
    pub action: Option<Action>,
    pub link: bool,
    pub shared: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Destination {
    pub number: usize,
    pub identifier: Identifier,
    pub structured: bool,
    pub array: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DestinationEffect {
    pub identifier: Identifier,
    pub structured: usize,
    pub view: String,
    pub zoom: Option<i64>,
    pub spec: Dimensions,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LinkSpan {
    pub template: AnnotationTemplate,
    pub level: usize,
    pub annotation: usize,
    pub origin: (i64, i64),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Outline {
    pub number: usize,
    pub action_number: usize,
    pub title_number: usize,
    pub count: i64,
    pub attributes: String,
    pub children: Vec<usize>,
    pub parent: Option<usize>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThreadTemplate {
    pub identifier: Identifier,
    pub spec: Dimensions,
    pub attributes: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bead {
    pub number: usize,
    pub page: usize,
    pub rectangle: [f64; 4],
    pub attributes: String,
    pub rectangle_number: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Thread {
    pub number: usize,
    pub identifier: Identifier,
    pub beads: Vec<usize>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct BoxContext {
    pub kind: NodeKind,
    pub width: i64,
    pub height: i64,
    pub depth: i64,
    pub x: i64,
    pub y: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Navigation {
    pub annotations: BTreeMap<usize, Option<Annotation>>,
    pub annotation_templates: BTreeMap<usize, AnnotationTemplate>,
    pub destination_effects: Vec<DestinationEffect>,
    pub thread_templates: Vec<ThreadTemplate>,
    pub destinations: BTreeMap<(bool, Identifier), Destination>,
    pub threads: BTreeMap<Identifier, Thread>,
    pub beads: BTreeMap<usize, Bead>,
    pub outlines: Vec<usize>,
    pub outline_entries: Vec<Outline>,
    pub outline_parents: Vec<usize>,
    pub outline_objects: BTreeMap<usize, String>,
    pub last_annotation: usize,
    pub last_link: usize,
    pub links: Vec<LinkSpan>,
    pub running_links: bool,
    pub boxes: Vec<BoxContext>,
    pub thread_span: Option<(ThreadTemplate, usize)>,
    pub last_bead: Option<usize>,
    pub matrix: Affine,
    pub saved_matrices: Vec<Affine>,
    pub reused_leader_regions: BTreeMap<i64, [f64; 4]>,
}
impl Default for Navigation {
    fn default() -> Self {
        Self {
            annotations: BTreeMap::new(),
            annotation_templates: BTreeMap::new(),
            destination_effects: Vec::new(),
            thread_templates: Vec::new(),
            destinations: BTreeMap::new(),
            threads: BTreeMap::new(),
            beads: BTreeMap::new(),
            outlines: Vec::new(),
            outline_entries: Vec::new(),
            outline_parents: Vec::new(),
            outline_objects: BTreeMap::new(),
            last_annotation: 0,
            last_link: 0,
            links: Vec::new(),
            running_links: true,
            boxes: Vec::new(),
            thread_span: None,
            last_bead: None,
            matrix: Affine::default(),
            saved_matrices: Vec::new(),
            reused_leader_regions: BTreeMap::new(),
        }
    }
}
pub struct NavigationContext<'a> {
    pub resources: &'a mut PDFResources,
    pub page: &'a mut PageUse,
    pub page_height: i64,
    pub shipping_page: bool,
    pub exact_repro: bool,
    pub link_margin: i64,
    pub dest_margin: i64,
    pub thread_margin: i64,
    pub suppress_duplicate: bool,
    pub precision: usize,
    pub magnification: i64,
    pub warning: &'a mut dyn FnMut(&str),
}
impl Navigation {
    pub fn reset_traversal(&mut self) {
        self.links.clear();
        self.boxes.clear();
        self.thread_span = None;
        self.last_bead = None;
        self.matrix = Affine::default();
        self.saved_matrices.clear();
        self.reused_leader_regions.clear();
        self.running_links = true
    }
    pub fn destination(
        &mut self,
        r: &mut PDFResources,
        id: Identifier,
        structured: bool,
    ) -> &mut Destination {
        let key = (structured, id.clone());
        if !self.destinations.contains_key(&key) {
            let number = r.reserve_object();
            self.destinations.insert(
                key.clone(),
                Destination {
                    number,
                    identifier: id,
                    structured,
                    array: None,
                },
            );
        }
        self.destinations.get_mut(&key).unwrap()
    }
    pub fn thread(&mut self, r: &mut PDFResources, id: Identifier) -> &mut Thread {
        if !self.threads.contains_key(&id) {
            let number = r.reserve_object();
            self.threads.insert(
                id.clone(),
                Thread {
                    number,
                    identifier: id.clone(),
                    beads: Vec::new(),
                },
            );
        }
        self.threads.get_mut(&id).unwrap()
    }
    pub fn action_string(&mut self, r: &mut PDFResources, a: &Action) -> Result<String> {
        if a.kind == "user" {
            return Ok(a.text.clone());
        }
        let remote = a.file.is_some();
        let mut fields = Vec::new();
        if let Some(file) = &a.file {
            fields.push(format!(
                "/F {}",
                if file.starts_with('(') && file.ends_with(')') {
                    file.clone()
                } else {
                    string(file)?
                }
            ));
            if let Some(window) = a.window {
                fields.push(format!("/NewWindow {window}"));
            }
        }
        match a.destination.as_ref() {
            Some(ActionTarget::Page(page, options)) => {
                let target = if remote {
                    (page - 1).to_string()
                } else {
                    format!("{} 0 R", r.page_reference(*page)?)
                };
                fields.push(format!(
                    "/S /{} /D [{target} {options}]",
                    if remote { "GoToR" } else { "GoTo" }
                ));
            }
            Some(ActionTarget::Identifier(id)) => {
                let number = if remote {
                    0
                } else if a.kind == "thread" {
                    self.thread(r, id.clone()).number
                } else {
                    self.destination(r, id.clone(), false).number
                };
                let target = match id {
                    Identifier::Name(s) => string(s)?,
                    Identifier::Number(n) => {
                        if remote {
                            n.to_string()
                        } else {
                            format!("{number} 0 R")
                        }
                    }
                };
                fields.push(format!(
                    "/S /{} /D {target}",
                    if a.kind == "thread" {
                        "Thread"
                    } else if remote {
                        "GoToR"
                    } else {
                        "GoTo"
                    }
                ));
            }
            None => {}
        }
        if let Some(id) = &a.structure {
            let target = if remote {
                id.to_string()
            } else {
                format!("{} 0 R", self.destination(r, id.clone(), true).number)
            };
            fields.push(format!("/SD {target}"));
        }
        Ok(format!("<< {} >>", fields.join(" ")))
    }
    pub fn parent(&self) -> Result<BoxContext> {
        self.boxes
            .last()
            .copied()
            .ok_or_else(|| Error::value("PDF region has no containing box"))
    }
    pub fn rectangle(
        &self,
        spec: &Dimensions,
        x: i64,
        y: i64,
        margin: i64,
        height: i64,
    ) -> Result<[f64; 4]> {
        let p = self.parent()?;
        let right = spec.get("width").map_or(p.x + p.width, |w| x + w);
        let top = spec.get("height").map_or(p.y - p.height, |h| y - h);
        let bottom = spec.get("depth").map_or(p.y + p.depth, |d| y + d);
        if self.matrix == Affine::default() {
            return Ok([
                sp_to_bp(x - margin),
                sp_to_bp(height - bottom - margin),
                sp_to_bp(right + margin),
                sp_to_bp(height - top + margin),
            ]);
        }
        let mut xs = Vec::new();
        let mut ys = Vec::new();
        for px in [x, right] {
            for py in [top, bottom] {
                let (a, b) = self.matrix.point(sp_to_bp(px), sp_to_bp(height - py));
                xs.push(a);
                ys.push(b)
            }
        }
        let m = sp_to_bp(margin);
        Ok([
            xs.iter().copied().fold(f64::INFINITY, f64::min) - m,
            ys.iter().copied().fold(f64::INFINITY, f64::min) - m,
            xs.iter().copied().fold(f64::NEG_INFINITY, f64::max) + m,
            ys.iter().copied().fold(f64::NEG_INFINITY, f64::max) + m,
        ])
    }
    fn annotation(
        &mut self,
        c: &mut NavigationContext<'_>,
        t: &AnnotationTemplate,
        x: i64,
        y: i64,
        continuation: bool,
        shared: Option<i64>,
    ) -> Result<usize> {
        let n = if continuation || self.annotations.get(&t.number).is_some_and(Option::is_some) {
            c.resources.reserve_object()
        } else {
            t.number
        };
        let rectangle = self.rectangle(
            &t.spec,
            x,
            y,
            if t.action.is_some() { c.link_margin } else { 0 },
            c.page_height,
        )?;
        if let Some(id) = shared {
            self.reused_leader_regions.insert(id, rectangle);
        }
        self.annotations.insert(
            n,
            Some(Annotation {
                number: n,
                rectangle,
                text: t.attributes.clone(),
                action: t.action.clone(),
                link: t.action.is_some(),
                shared,
            }),
        );
        c.page.annotations.push(n);
        Ok(n)
    }
    pub fn enter_box(
        &mut self,
        c: &mut NavigationContext<'_>,
        node: &Node,
        x: i64,
        y: i64,
        leaders: bool,
    ) -> Result<()> {
        let p = BoxContext {
            kind: node.node_type,
            width: node.width,
            height: node.height,
            depth: node.depth,
            x,
            y,
        };
        self.boxes.push(p);
        if leaders || !c.shipping_page {
            return Ok(());
        }
        let level = self.boxes.len();
        if p.kind == NodeKind::Hlist && self.running_links {
            for i in 0..self.links.len() {
                if self.links[i].level == level {
                    let t = self.links[i].template.clone();
                    let n = self.annotation(c, &t, x, y, true, None)?;
                    self.links[i].annotation = n;
                    self.links[i].origin = (x, y)
                }
            }
        }
        if p.kind == NodeKind::Vlist && self.last_bead.is_some() {
            if let Some((t, l)) = self.thread_span.clone() {
                if l == level && !t.spec.contains_key("depth") {
                    self.bead(
                        c,
                        &ThreadTemplate {
                            attributes: String::new(),
                            ..t
                        },
                        x,
                        y - p.height,
                    )?
                }
            }
        }
        Ok(())
    }
    pub fn leave_box(&mut self) {
        self.boxes.pop();
    }
    pub fn bead(
        &mut self,
        c: &mut NavigationContext<'_>,
        t: &ThreadTemplate,
        x: i64,
        y: i64,
    ) -> Result<()> {
        self.thread(c.resources, t.identifier.clone());
        let n = c.resources.reserve_object();
        let rectangle = self.rectangle(&t.spec, x, y, c.thread_margin, c.page_height)?;
        self.beads.insert(
            n,
            Bead {
                number: n,
                page: c.page.page,
                rectangle,
                attributes: t.attributes.clone(),
                rectangle_number: 0,
            },
        );
        self.threads.get_mut(&t.identifier).unwrap().beads.push(n);
        c.page.beads.push(n);
        self.last_bead = Some(n);
        Ok(())
    }
    pub fn ship(
        &mut self,
        c: &mut NavigationContext<'_>,
        effect: &PdfEffect,
        x: i64,
        y: i64,
        leaders: bool,
        source_node: Option<i64>,
    ) -> Result<()> {
        match effect {
            PdfEffect::RunningLinkOn => {
                self.running_links = true;
                return Ok(());
            }
            PdfEffect::RunningLinkOff => {
                self.running_links = false;
                return Ok(());
            }
            _ => {}
        }
        if !c.shipping_page {
            return Err(Error::value(
                "PDF document structures cannot be inside a form",
            ));
        }
        if leaders && !matches!(effect, PdfEffect::StartLink(_) | PdfEffect::EndLink) {
            return Ok(());
        }
        match effect {
            PdfEffect::Annotation(n) => {
                let t = self.annotation_templates[n].clone();
                self.annotation(c, &t, x, y, false, None)?;
            }
            PdfEffect::StartLink(n) => {
                if self.parent()?.kind != NodeKind::Hlist {
                    return Err(Error::value("PDF link start must be in a horizontal box"));
                }
                let mut t = self.annotation_templates[n].clone();
                let shared = if leaders && c.exact_repro {
                    source_node
                } else {
                    None
                };
                if let Some(rect) = shared.and_then(|id| self.reused_leader_regions.get(&id)) {
                    t.spec = Dimensions::from([
                        ("width".into(), bp_to_sp(rect[0])),
                        ("height".into(), c.page_height - bp_to_sp(rect[3])),
                        ("depth".into(), bp_to_sp(rect[2])),
                    ]);
                }
                let number = self.annotation(c, &t, x, y, false, shared)?;
                self.links.push(LinkSpan {
                    template: t,
                    level: self.boxes.len(),
                    annotation: number,
                    origin: (x, y),
                });
            }
            PdfEffect::EndLink => {
                let span = self.links.pop().ok_or_else(|| {
                    Error::value("PDF link end has no matching start")
                        .with_issue(crate::core::recovery::Issue::PdfOperation)
                })?;
                if span.level != self.boxes.len() {
                    (c.warning)("PDF link ended at a different box nesting level")
                }
                if !span.template.spec.contains_key("width") {
                    let (sx, sy) = span.origin;
                    let a = self.annotations[&span.annotation].as_ref().unwrap();
                    let mut rectangle = a.rectangle;
                    let shared = a.shared;
                    if self.matrix == Affine::default() {
                        rectangle[2] = sp_to_bp(x + c.link_margin)
                    } else {
                        let mut spec = span.template.spec.clone();
                        spec.insert("width".into(), x - sx + c.link_margin);
                        rectangle = self.rectangle(&spec, sx, sy, c.link_margin, c.page_height)?;
                    }
                    self.annotations
                        .get_mut(&span.annotation)
                        .unwrap()
                        .as_mut()
                        .unwrap()
                        .rectangle = rectangle;
                    if let Some(id) = shared {
                        self.reused_leader_regions.insert(id, rectangle);
                    }
                }
            }
            PdfEffect::Destination(index) => {
                let t = self.destination_effects[*index].clone();
                let d = self.destination(c.resources, t.identifier.clone(), t.structured != 0);
                if d.array.is_some() {
                    if !c.suppress_duplicate {
                        (c.warning)(&format!(
                            "Duplicate PDF destination ignored: {}",
                            t.identifier
                        ));
                    }
                    return Ok(());
                }
                let page = if t.structured != 0 {
                    t.structured
                } else {
                    c.page.page
                };
                let (px, py) = self.matrix.point(sp_to_bp(x), sp_to_bp(c.page_height - y));
                let args = match t.view.as_str() {
                    "XYZ" => format!(
                        " {} {} {}",
                        number(px, c.precision, c.magnification, true),
                        number(py, c.precision, c.magnification, true),
                        t.zoom
                            .map_or_else(|| "null".into(), |z| (z as f64 / 1000.0).to_string())
                    ),
                    "FitH" | "FitBH" => {
                        format!(" {}", number(py, c.precision, c.magnification, true))
                    }
                    "FitV" | "FitBV" => {
                        format!(" {}", number(px, c.precision, c.magnification, true))
                    }
                    "FitR" => {
                        let rect = self.rectangle(&t.spec, x, y, c.dest_margin, c.page_height)?;
                        format!(
                            " {}",
                            rect.iter()
                                .map(|v| number(*v, c.precision, c.magnification, true))
                                .collect::<Vec<_>>()
                                .join(" ")
                        )
                    }
                    _ => String::new(),
                };
                self.destination(c.resources, t.identifier, t.structured != 0)
                    .array = Some(format!("[{page} 0 R /{}{args}]", t.view));
            }
            PdfEffect::Thread(i) | PdfEffect::StartThread(i) => {
                let t = self.thread_templates[*i].clone();
                if matches!(effect, PdfEffect::StartThread(_)) {
                    if self.parent()?.kind != NodeKind::Vlist {
                        return Err(Error::value("PDF article start must be in a vertical box"));
                    }
                    self.thread_span = Some((t.clone(), self.boxes.len()));
                }
                self.bead(c, &t, x, y)?;
            }
            PdfEffect::EndThread => {
                let (t, l) = self.thread_span.clone().ok_or_else(|| {
                    Error::value("PDF article end has no matching start")
                        .with_issue(crate::core::recovery::Issue::PdfOperation)
                })?;
                if l != self.boxes.len() {
                    return Err(Error::value(
                        "PDF article ended at a different box nesting level",
                    ));
                }
                if !t.spec.contains_key("depth") {
                    if let Some(n) = self.last_bead {
                        self.beads.get_mut(&n).unwrap().rectangle[1] =
                            sp_to_bp(c.page_height - y - c.thread_margin);
                    }
                }
                self.thread_span = None;
                self.last_bead = None;
            }
            _ => {}
        }
        Ok(())
    }
    pub fn finish_page(&mut self, r: &mut PDFResources, p: &mut PageUse) -> Result<()> {
        for &n in &p.annotations {
            let action = self
                .annotations
                .get_mut(&n)
                .and_then(Option::as_mut)
                .and_then(|a| a.action.take());
            if let Some(a) = action {
                let text = if a.kind == "user" {
                    format!(" {}", a.text)
                } else {
                    format!(" /Subtype /Link /A {}", self.action_string(r, &a)?)
                };
                self.annotations
                    .get_mut(&n)
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .text
                    .push_str(&text);
            }
        }
        for n in &p.beads {
            self.beads.get_mut(n).unwrap().rectangle_number = r.reserve_object();
        }
        p.annotations
            .sort_by_key(|n| self.annotations[n].as_ref().unwrap().link);
        Ok(())
    }
    pub fn outline(
        &mut self,
        r: &mut PDFResources,
        a: Action,
        title: String,
        count: i64,
        attributes: String,
    ) -> Result<()> {
        let an = r.reserve_object();
        let text = self.action_string(r, &a)?;
        self.outline_objects.insert(an, text);
        let n = r.reserve_object();
        let tn = r.reserve_object();
        self.outline_objects.insert(tn, source_string(&title));
        let parent = self.outline_parents.last().copied();
        let index = self.outline_entries.len();
        self.outline_entries.push(Outline {
            number: n,
            action_number: an,
            title_number: tn,
            count,
            attributes,
            children: Vec::new(),
            parent,
        });
        if let Some(parent) = parent {
            self.outline_entries[parent].children.push(index)
        } else {
            self.outlines.push(index)
        }
        if count != 0 {
            self.outline_parents.push(index)
        } else {
            while let Some(&last) = self.outline_parents.last() {
                let e = &self.outline_entries[last];
                if e.children.len() >= e.count.unsigned_abs() as usize {
                    self.outline_parents.pop();
                } else {
                    break;
                }
            }
        }
        Ok(())
    }
    fn visible(&self, roots: &[usize]) -> usize {
        let mut pending = roots.to_vec();
        let mut count = 0;
        while let Some(i) = pending.pop() {
            count += 1;
            if self.outline_entries[i].count > 0 {
                pending.extend(&self.outline_entries[i].children)
            }
        }
        count
    }
    pub fn write(
        &mut self,
        w: &mut PdfWriter,
        first_page: usize,
        page_size: (i64, i64),
        precision: usize,
        mag: i64,
        warn: &mut dyn FnMut(&str),
    ) -> Result<(String, String, BTreeSet<usize>)> {
        let mut free = BTreeSet::new();
        for (&n, a) in &self.annotations {
            if let Some(a) = a {
                let rect = a
                    .shared
                    .and_then(|id| self.reused_leader_regions.get(&id))
                    .unwrap_or(&a.rectangle);
                w.add_text_object(
                    &format!(
                        "<< /Type /Annot {} /Rect {} >>",
                        a.text,
                        rect_text(rect, precision, mag, true)
                    ),
                    Some(n),
                    1,
                )?;
            } else {
                free.insert(n);
            }
        }
        let mut named = Vec::new();
        for d in self.destinations.values_mut() {
            if d.array.is_none() {
                warn(&format!(
                    "PDF destination referenced but missing: {}",
                    d.identifier
                ));
                d.array = Some(format!("[{first_page} 0 R /Fit]"));
            }
            let array = d.array.as_ref().unwrap();
            if let Identifier::Name(s) = &d.identifier {
                if !d.structured {
                    w.add_text_object(&format!("<< /D {array} >>"), Some(d.number), 1)?;
                    named.push((crate::pdf::writer::latin1(s)?, s.clone(), d.number));
                    continue;
                }
            }
            w.add_text_object(array, Some(d.number), 1)?;
        }
        let mut names = String::new();
        if !named.is_empty() {
            named.sort();
            let entries = named
                .iter()
                .map(|(_, s, n)| Ok(format!("{} {n} 0 R", string(s)?)))
                .collect::<Result<Vec<_>>>()?
                .join(" ");
            let n = w.add_text_object(&format!("<< /Names [{entries}] >>"), None, 1)?;
            names = format!("/Dests {n} 0 R");
        }
        for (&n, text) in &self.outline_objects {
            w.add_text_object(text, Some(n), 1)?;
        }
        let mut catalog = String::new();
        if !self.outlines.is_empty() {
            let root = w.reserve_object();
            let mut pending = vec![(self.outlines.clone(), root)];
            while let Some((siblings, parent)) = pending.pop() {
                for (pos, &i) in siblings.iter().enumerate() {
                    let e = &self.outline_entries[i];
                    let previous = if pos > 0 {
                        format!(
                            "/Prev {} 0 R",
                            self.outline_entries[siblings[pos - 1]].number
                        )
                    } else {
                        String::new()
                    };
                    let next = if pos + 1 < siblings.len() {
                        format!(
                            "/Next {} 0 R",
                            self.outline_entries[siblings[pos + 1]].number
                        )
                    } else {
                        String::new()
                    };
                    let children = if !e.children.is_empty() {
                        let visible = self.visible(&e.children) as i64;
                        pending.push((e.children.clone(), e.number));
                        format!(
                            "/First {} 0 R /Last {} 0 R /Count {}",
                            self.outline_entries[e.children[0]].number,
                            self.outline_entries[*e.children.last().unwrap()].number,
                            if e.count > 0 { visible } else { -visible }
                        )
                    } else {
                        String::new()
                    };
                    w.add_text_object(&format!("<< /Title {} 0 R /A {} 0 R /Parent {parent} 0 R {previous} {next} {children} {} >>",e.title_number,e.action_number,e.attributes),Some(e.number),1)?;
                }
            }
            w.add_text_object(
                &format!(
                    "<< /Type /Outlines /First {} 0 R /Last {} 0 R /Count {} >>",
                    self.outline_entries[self.outlines[0]].number,
                    self.outline_entries[*self.outlines.last().unwrap()].number,
                    self.visible(&self.outlines)
                ),
                Some(root),
                1,
            )?;
            catalog = format!("/Outlines {root} 0 R");
        }
        for t in self.threads.values() {
            if t.beads.is_empty() {
                warn(&format!(
                    "PDF article referenced but missing: {}",
                    t.identifier
                ));
                let n = w.reserve_object();
                let rectangle = rect_text(
                    &[0.0, 0.0, sp_to_bp(page_size.0), sp_to_bp(page_size.1)],
                    precision,
                    mag,
                    false,
                );
                w.add_text_object(
                    &format!(
                        "<< /T {} 0 R /N {n} 0 R /V {n} 0 R /P {first_page} 0 R /R {rectangle} >>",
                        t.number
                    ),
                    Some(n),
                    1,
                )?;
                w.add_text_object(
                    &format!(
                        "<< /I << /Title {} >> /F {n} 0 R >>",
                        string(&t.identifier.to_string())?
                    ),
                    Some(t.number),
                    1,
                )?;
                continue;
            }
            let attrs = t
                .beads
                .iter()
                .rev()
                .map(|n| &self.beads[n].attributes)
                .find(|s| !s.is_empty())
                .cloned()
                .unwrap_or(format!(
                    "/I << /Title {} >>",
                    string(&t.identifier.to_string())?
                ));
            w.add_text_object(
                &format!("<< {attrs} /F {} 0 R >>", t.beads[0]),
                Some(t.number),
                1,
            )?;
            for (i, n) in t.beads.iter().enumerate() {
                let b = &self.beads[n];
                let previous = t.beads[(i + t.beads.len() - 1) % t.beads.len()];
                let next = t.beads[(i + 1) % t.beads.len()];
                let owner = if i == 0 {
                    format!("/T {} 0 R", t.number)
                } else {
                    String::new()
                };
                w.add_text_object(
                    &format!(
                        "<< {owner} /V {previous} 0 R /N {next} 0 R /P {} 0 R /R {} 0 R >>",
                        b.page, b.rectangle_number
                    ),
                    Some(*n),
                    1,
                )?;
                w.add_text_object(
                    &rect_text(&b.rectangle, precision, mag, true),
                    Some(b.rectangle_number),
                    1,
                )?;
            }
        }
        if !self.threads.is_empty() {
            catalog.push_str(&format!(
                " /Threads [{}]",
                self.threads
                    .values()
                    .map(|t| format!("{} 0 R", t.number))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
        Ok((catalog, names, free))
    }
}
fn number(value: f64, precision: usize, mag: i64, magnify: bool) -> String {
    format!(
        "{:.*}",
        precision,
        value * if magnify { mag as f64 / 1000.0 } else { 1.0 }
    )
}
fn rect_text(rect: &[f64; 4], precision: usize, mag: i64, magnify: bool) -> String {
    format!(
        "[{}]",
        rect.iter()
            .map(|&v| number(v, precision, mag, magnify))
            .collect::<Vec<_>>()
            .join(" ")
    )
}
