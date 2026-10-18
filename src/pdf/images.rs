//! Image dimensions, reusable raster streams and copied PDF page graphs.
use super::navigation::Dimensions;
use crate::{
    core::{
        errors::{Error, Result},
        units::{SP_PER_PT, bp_to_sp},
        values::round_ratio,
    },
    pdf::{
        document::{DocumentServices, PDFDocument},
        graphics::Affine,
        image_records::ImageUse,
        imports::{self, PDFImportGraph, SourceKey},
        jbig2,
        parsing::{PdfValue, octet_string},
        raster,
        resources::PDFResources,
        writer::PdfWriter,
    },
};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
pub const PAGE_BOXES: &[(&str, &str)] = &[
    ("mediabox", "MediaBox"),
    ("cropbox", "CropBox"),
    ("bleedbox", "BleedBox"),
    ("trimbox", "TrimBox"),
    ("artbox", "ArtBox"),
];
pub fn page_box(code: i64) -> Option<&'static str> {
    if code == 0 {
        Some("cropbox")
    } else {
        PAGE_BOXES.get(code as usize - 1).map(|v| v.0)
    }
}
pub fn historical_dimension(value: f64) -> i64 {
    let value = (value as f32) as f64;
    let scaled = value * 803.0 * SP_PER_PT as f64 / 800.0;
    scaled.signum() as i64 * (scaled.abs() + 0.5).floor() as i64
}
pub fn image_ratio(value: i64, numerator: i64, denominator: i64) -> Result<i64> {
    let product = value as i128 * numerator as i128;
    Ok(product.signum() as i64 * round_ratio(product.abs(), denominator as i128)? as i64)
}
pub fn dimensions(nw: i64, nh: i64, rw: i64, rh: i64, s: &Dimensions) -> Result<(i64, i64, i64)> {
    let (mut w, mut h, mut d) = (
        s.get("width").copied(),
        s.get("height").copied(),
        s.get("depth").copied(),
    );
    if w.is_none() && h.is_none() && d.is_none() {
        return Ok((nw, nh, 0));
    }
    if w.is_none() {
        if h.is_none() {
            w = Some(image_ratio(nh, rw, rh)?);
            h = Some(nh - d.unwrap_or(0))
        } else {
            d = Some(d.unwrap_or(0));
            w = Some(image_ratio(h.unwrap() + d.unwrap(), rw, rh)?)
        }
    } else if h.is_none() {
        d = Some(d.unwrap_or(0));
        h = Some(image_ratio(w.unwrap(), rh, rw)? - d.unwrap())
    } else {
        d = Some(d.unwrap_or(0))
    }
    Ok((w.unwrap(), h.unwrap(), d.unwrap_or(0)))
}
pub fn source_key(image: &ImageUse) -> SourceKey {
    (
        image.filename.clone(),
        format!("{:x}", Sha256::digest(&image.source)),
    )
}
pub fn reader(image: &ImageUse) -> Result<lopdf::Document> {
    lopdf::Document::load_mem(&image.source)
        .map_err(|e| Error::value(format!("Cannot read included PDF: {e}")))
}
fn inherited(pdf: &lopdf::Document, page: lopdf::ObjectId, key: &[u8]) -> Result<Option<PdfValue>> {
    let mut pointer = page;
    let mut visited = BTreeSet::new();
    while visited.insert(pointer) {
        let dict = pdf
            .get_dictionary(pointer)
            .map_err(|e| Error::value(e.to_string()))?;
        if let Ok(value) = dict.get(key) {
            return imports::from_object(value).map(Some);
        }
        let Ok(parent) = dict.get(b"Parent") else {
            break;
        };
        let Ok(parent) = parent.as_reference() else {
            break;
        };
        pointer = parent;
    }
    Ok(None)
}
pub struct ImageOptions {
    pub attributes: String,
    pub page: i64,
    pub named: Option<String>,
    pub pagebox: String,
    pub colorspace: i64,
}
impl Default for ImageOptions {
    fn default() -> Self {
        Self {
            attributes: String::new(),
            page: 1,
            named: None,
            pagebox: "cropbox".into(),
            colorspace: 0,
        }
    }
}
fn named_page(
    pdf: &lopdf::Document,
    name: &str,
    pages: &[(u32, lopdf::ObjectId)],
) -> Result<usize> {
    let catalog = pdf.catalog().map_err(|e| Error::value(e.to_string()))?;
    let mut candidates = Vec::new();
    if let Ok(dests) = catalog.get(b"Dests") {
        let d = imports::resolve(&imports::from_object(dests)?, pdf)?;
        if let Some(dest) = d.get(name) {
            candidates.push(dest.clone())
        }
    }
    if let Ok(names) = catalog.get(b"Names") {
        let d = imports::resolve(&imports::from_object(names)?, pdf)?;
        if let Some(dests) = d.get("Dests") {
            let mut pending = vec![dests.clone()];
            while let Some(v) = pending.pop() {
                let d = imports::resolve(&v, pdf)?;
                if let Some(names) = d.get("Names") {
                    let PdfValue::Array(items) = imports::resolve(names, pdf)? else {
                        return Err(Error::value("Invalid PDF names tree")
                            .with_issue(crate::core::recovery::Issue::Image));
                    };
                    for pair in items.chunks_exact(2) {
                        let id = match &pair[0] {
                            PdfValue::String(bytes) => {
                                bytes.iter().map(|&c| char::from(c)).collect::<String>()
                            }
                            PdfValue::Name(s) => s.clone(),
                            _ => continue,
                        };
                        if id == name {
                            candidates.push(pair[1].clone())
                        }
                    }
                }
                if let Some(kids) = d.get("Kids") {
                    if let PdfValue::Array(kids) = imports::resolve(kids, pdf)? {
                        pending.extend(kids)
                    }
                }
            }
        }
    }
    for value in candidates {
        let value = imports::resolve(&value, pdf)?;
        let array = if let Some(v) = value.get("D") {
            imports::resolve(v, pdf)?
        } else {
            value
        };
        if let PdfValue::Array(array) = array {
            if let Some(PdfValue::Reference(n, g)) = array.first() {
                if let Some(i) = pages.iter().position(|(_, id)| id == &(*n, *g)) {
                    return Ok(i + 1);
                }
            }
        }
    }
    Err(Error::value(format!(
        "PDF named destination not found: {name}"
    )))
}
pub fn load(
    doc: &mut PDFDocument,
    r: &mut PDFResources,
    s: &mut impl DocumentServices,
    number: usize,
    filename: String,
    source: Vec<u8>,
    spec: &Dimensions,
    mut options: ImageOptions,
) -> Result<ImageUse> {
    let ordinal = doc
        .images
        .get(&number)
        .map_or(doc.images.len() + 1, |i| i.ordinal);
    let mut image;
    if source.starts_with(b"%PDF-") {
        image = ImageUse::new(
            number,
            ordinal,
            filename,
            source,
            "PDF".into(),
            (0, 0),
            (0, 0, 0),
        );
        let pdf = reader(&image)?;
        let pages = pdf.get_pages().into_iter().collect::<Vec<_>>();
        if let Some(name) = &options.named {
            options.page = named_page(&pdf, name, &pages)? as i64
        }
        let selected = options
            .page
            .checked_sub(1)
            .and_then(|page| usize::try_from(page).ok())
            .and_then(|index| pages.get(index))
            .ok_or_else(|| {
                Error::value("Included PDF page number out of range")
                    .with_issue(crate::core::recovery::Issue::Image)
            })?
            .1;
        let target = doc.check_version(s)?;
        let source_version = pdf
            .version
            .split('.')
            .map(str::parse::<u8>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| {
                Error::value("Invalid included PDF version")
                    .with_issue(crate::core::recovery::Issue::Image)
            })?;
        if source_version.as_slice() > [target.0, target.1].as_slice() {
            let message = format!(
                "Included PDF version %PDF-{} exceeds output version",
                pdf.version
            );
            if s.parameter("pdfinclusionerrorlevel") != 0 {
                return Err(Error::value(message));
            }
            s.warning(&message)
        }
        let media = inherited(&pdf, selected, b"MediaBox")?.ok_or_else(|| {
            Error::value("Included PDF page has no MediaBox")
                .with_issue(crate::core::recovery::Issue::Image)
        })?;
        let crop = inherited(&pdf, selected, b"CropBox")?.unwrap_or_else(|| media.clone());
        let name = PAGE_BOXES
            .iter()
            .find(|(name, _)| *name == options.pagebox)
            .ok_or_else(|| {
                Error::value("Unknown PDF page box").with_issue(crate::core::recovery::Issue::Image)
            })?
            .1;
        let box_value = inherited(&pdf, selected, name.as_bytes())?.unwrap_or(
            if options.pagebox == "mediabox" {
                media
            } else {
                crop
            },
        );
        let PdfValue::Array(box_values) = imports::resolve(&box_value, &pdf)? else {
            return Err(Error::value("Invalid PDF page box")
                .with_issue(crate::core::recovery::Issue::Image));
        };
        let coords = box_values
            .iter()
            .map(PdfValue::number)
            .collect::<Result<Vec<_>>>()?;
        let [left, bottom, right, top] = <[f64; 4]>::try_from(coords).map_err(|_| {
            Error::value("Invalid PDF page box").with_issue(crate::core::recovery::Issue::Image)
        })?;
        image.bbox = [
            left.min(right),
            bottom.min(top),
            left.max(right),
            bottom.max(top),
        ];
        image.rotation = inherited(&pdf, selected, b"Rotate")?
            .map(|v| imports::resolve(&v, &pdf)?.number())
            .transpose()?
            .unwrap_or(0.0) as i64
            % 360;
        let convert = if s.exact_repro() {
            historical_dimension
        } else {
            bp_to_sp
        };
        let (mut width, mut height) =
            (convert((right - left).abs()), convert((top - bottom).abs()));
        let (x, y) = (convert(image.bbox[0]), convert(image.bbox[1]));
        image.bbox_sp = [x, y, x + width, y + height];
        if matches!(image.rotation, 90 | 270) {
            std::mem::swap(&mut width, &mut height)
        }
        image.natural_width = width;
        image.natural_height = height;
        (image.width, image.height, image.depth) = dimensions(width, height, width, height, spec)?;
        image.page = options.page as usize;
        image.pages = pages.len();
        if inherited(&pdf, selected, b"Group")?.is_some() {
            image.group = -1;
        }
    } else {
        let (format, depth, width, height, dpi, alpha, pages) =
            if source.starts_with(jbig2::SIGNATURE) {
                if doc.check_version(s)? < (1, 4) {
                    return Err(Error::value("JBIG2 images require PDF 1.4 or later")
                        .with_issue(crate::core::recovery::Issue::Image));
                }
                let parsed = jbig2::read(&source)?;
                let info = u32::try_from(options.page)
                    .ok()
                    .and_then(|page| parsed.pages.get(&page))
                    .ok_or_else(|| {
                        Error::value("Included JBIG2 page number out of range")
                            .with_issue(crate::core::recovery::Issue::Image)
                    })?;
                (
                    "JBIG2".into(),
                    0,
                    info.width as i64,
                    info.height as i64,
                    (info.dpi.0 as i64, info.dpi.1 as i64),
                    false,
                    parsed.declared_pages as usize,
                )
            } else {
                let info = raster::metadata(&source)?;
                (
                    info.format,
                    info.depth as u32,
                    info.width as i64,
                    info.height as i64,
                    info.dpi,
                    info.alpha,
                    1,
                )
            };
        let (mut xr, mut yr) = dpi;
        let default = s.parameter("pdfimageresolution").max(0);
        if xr == 0 || yr == 0 {
            xr = if default == 0 { 72 } else { default };
            yr = xr;
        }
        let hundred = 7227 * SP_PER_PT;
        let nw = image_ratio(hundred, width, 100 * xr)?;
        let nh = image_ratio(hundred, height, 100 * yr)?;
        let size = dimensions(nw, nh, width, height, spec)?;
        image = ImageUse::new(number, ordinal, filename, source, format, (nw, nh), size);
        image.color_depth = depth;
        image.pages = pages;
        if image.format == "JBIG2" {
            image.page = options.page as usize
        }
        if image.format == "PNG" && alpha && doc.check_version(s)? >= (1, 4) {
            if doc.transparency_group == 0 {
                doc.transparency_group = r.reserve_object()
            }
            image.group = doc.transparency_group as i64;
        }
    }
    image.attributes = options.attributes;
    image.colorspace = options.colorspace;
    doc.last_image = number;
    doc.last_image_pages = image.pages;
    doc.last_image_depth = image.color_depth;
    doc.images.insert(number, image.clone());
    Ok(image)
}
pub fn pdf_matrix(image: &ImageUse) -> Affine {
    let [left, bottom, right, top] = image.bbox;
    match image.rotation {
        90 => Affine {
            a: 0.0,
            b: -1.0,
            c: 1.0,
            d: 0.0,
            e: left - bottom,
            f: bottom + right,
        },
        180 => Affine {
            a: -1.0,
            b: 0.0,
            c: 0.0,
            d: -1.0,
            e: left + right,
            f: bottom + top,
        },
        270 => Affine {
            a: 0.0,
            b: 1.0,
            c: -1.0,
            d: 0.0,
            e: left + top,
            f: bottom - left,
        },
        _ => Affine::default(),
    }
}
pub trait ImageServices: DocumentServices {
    fn replace_inclusion_font(
        &mut self,
        g: &mut PDFImportGraph<'_>,
        v: &PdfValue,
        key: &SourceKey,
        reader: &lopdf::Document,
        copy_fonts: bool,
    ) -> Result<Option<Vec<u8>>>;
}
pub fn emit(
    doc: &mut PDFDocument,
    r: &mut PDFResources,
    s: &mut impl ImageServices,
    number: usize,
) -> Result<()> {
    #[cfg(feature = "latency-diagnostics")]
    let _diag = crate::latency_diagnostics::enabled().then(|| {
        crate::latency_diagnostics::phase("image_emit", &doc.images[&number].filename, None)
    });
    if doc.images[&number].data.is_some() {
        return Ok(());
    }
    doc.check_version(s)?;
    let mut image = doc.images[&number].clone();
    let policy = doc.frozen_image_policy.unwrap();
    if image.format == "PDF" {
        let pdf = reader(&image)?;
        let key = source_key(&image);
        let selected = *pdf
            .get_pages()
            .values()
            .nth(image.page - 1)
            .ok_or_else(|| {
                Error::value("Included PDF page number out of range")
                    .with_issue(crate::core::recovery::Issue::Image)
            })?;
        let mut g = PDFImportGraph {
            resources: r,
            objects: &mut doc.imported_objects,
            references: &mut doc.imported_references,
        };
        let mut replacement =
            |g: &mut PDFImportGraph<'_>, v: &PdfValue, key: &SourceKey, pdf: &lopdf::Document| {
                s.replace_inclusion_font(g, v, key, pdf, policy.copy_fonts)
            };
        let source_resources =
            inherited(&pdf, selected, b"Resources")?.unwrap_or(PdfValue::Dictionary(Vec::new()));
        let resources = g
            .clone(&source_resources, &key, &pdf, &mut replacement)?
            .serialize()?;
        let resources = resources.iter().map(|&c| char::from(c)).collect::<String>();
        let bbox = image
            .bbox
            .iter()
            .map(|v| format!("{v:.8}"))
            .collect::<Vec<_>>()
            .join(" ");
        let m = pdf_matrix(&image);
        let matrix = [m.a, m.b, m.c, m.d, m.e, m.f]
            .iter()
            .map(f64::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        image.dictionary = format!(
            "/Type /XObject /Subtype /Form /FormType 1 /BBox [{bbox}] /Matrix [{matrix}] /Resources {resources}"
        );
        // Read controls before creating a mutable replacement closure again.
        let suppress = s.parameter("pdfsuppressptexinfo");
        if suppress & 2 == 0 {
            image.dictionary.push_str(&format!(
                " /PTEX.FileName {}",
                octet_string(image.filename.as_bytes())
            ))
        }
        if suppress & 4 == 0 {
            image
                .dictionary
                .push_str(&format!(" /PTEX.PageNumber {}", image.page))
        }
        let mut replacement =
            |g: &mut PDFImportGraph<'_>, v: &PdfValue, key: &SourceKey, pdf: &lopdf::Document| {
                s.replace_inclusion_font(g, v, key, pdf, policy.copy_fonts)
            };
        if suppress & 8 == 0 {
            if let Ok(info) = pdf.trailer.get(b"Info") {
                let value = g
                    .clone(&imports::from_object(info)?, &key, &pdf, &mut replacement)?
                    .serialize()?;
                image.dictionary.push_str(&format!(
                    " /PTEX.InfoDict {}",
                    value.iter().map(|&c| char::from(c)).collect::<String>()
                ));
            }
        }
        let dict = pdf
            .get_dictionary(selected)
            .map_err(|e| Error::value(e.to_string()))?;
        for name in ["LastModified", "Metadata", "PieceInfo", "SeparationInfo"] {
            if let Ok(v) = dict.get(name.as_bytes()) {
                let value = g
                    .clone(&imports::from_object(v)?, &key, &pdf, &mut replacement)?
                    .serialize()?;
                image.dictionary.push_str(&format!(
                    " /{name} {}",
                    value.iter().map(|&c| char::from(c)).collect::<String>()
                ));
            }
        }
        if let Ok(group) = dict.get(b"Group") {
            let v = imports::from_object(group)?;
            let original = if image.group > 0 {
                imports::resolve(&v, &pdf)?
            } else {
                v
            };
            let value = g
                .clone(&original, &key, &pdf, &mut replacement)?
                .serialize()?;
            if image.group > 0 {
                g.objects.insert(image.group as usize, Some(value.into()));
                image
                    .dictionary
                    .push_str(&format!(" /Group {} 0 R", image.group))
            } else {
                image.dictionary.push_str(&format!(
                    " /Group {}",
                    value.iter().map(|&c| char::from(c)).collect::<String>()
                ));
                if s.parameter("pdfsuppresswarningpagegroup") == 0 {
                    s.warning(
                        "PDF inclusion: multiple pdfs with page group included in a single page",
                    )
                }
            }
        }
        image.data = Some(
            pdf.get_page_content(selected)
                .map_err(|e| Error::value(e.to_string()))?
                .into(),
        );
    } else if image.format == "JBIG2" {
        let parsed = jbig2::read(&image.source)?;
        let page = &parsed.pages[&(image.page as u32)];
        image.data = Some(
            page.segments
                .iter()
                .flat_map(|seg| seg.encoded())
                .collect::<Vec<_>>()
                .into(),
        );
        image.dictionary = format!(
            "/Type /XObject /Subtype /Image /Width {} /Height {} /BitsPerComponent 1 /ColorSpace /DeviceGray /Filter [/JBIG2Decode]",
            page.width, page.height
        );
        if !parsed.globals.is_empty() {
            let key = source_key(&image);
            if !doc.jbig2_globals.contains_key(&key) {
                let number = r.reserve_object();
                doc.jbig2_globals.insert(
                    key.clone(),
                    jbig2::GlobalUse {
                        number,
                        source: image.source.clone(),
                        needed: BTreeSet::new(),
                    },
                );
            }
            let use_ = doc.jbig2_globals.get_mut(&key).unwrap();
            use_.needed.extend(parsed.dependencies(image.page as u32)?);
            image.dictionary.push_str(&format!(
                " /DecodeParms [<< /JBIG2Globals {} 0 R >>]",
                use_.number
            ));
        }
    } else {
        let info = raster::metadata(&image.source)?;
        let gamma = if image.format == "JPEG" {
            None
        } else {
            if policy.apply_gamma {
                if policy.gamma <= 0 {
                    return Err(Error::value(
                        "Display gamma must be positive when correction is enabled",
                    )
                    .with_issue(crate::core::recovery::Issue::Image));
                }
                let chunks = raster::png_chunks(&image.source)?;
                let file = if let Some(chunk) = chunks.iter().find(|c| c.kind == *b"gAMA") {
                    if chunk.data.len() != 4 {
                        return Err(Error::value("Invalid PNG gamma")
                            .with_issue(crate::core::recovery::Issue::Image));
                    }
                    u32::from_be_bytes(chunk.data[..4].try_into().unwrap()) as f64 / 100000.0
                } else if policy.image_gamma > 0 {
                    1000.0 / policy.image_gamma as f64
                } else {
                    return Err(Error::value(
                        "Image gamma must be positive when correction is enabled",
                    )
                    .with_issue(crate::core::recovery::Issue::Image));
                };
                Some(1.0 / (file * policy.gamma as f64 / 1000.0))
            } else {
                None
            }
        };
        let payload = doc
            .raster_cache
            .lock()
            .map_err(|_| Error::value("Raster cache unavailable"))?
            .decode(
                &image.source,
                image.format == "JPEG",
                policy.high_color,
                policy.allow_alpha,
                gamma,
            )?;
        let mut colorspace = payload.colorspace;
        if let Some(palette) = payload.palette {
            let n = r.reserve_object();
            colorspace = format!("[/Indexed /DeviceRGB {} {n} 0 R]", palette.len() / 3 - 1);
            doc.imported_objects.insert(
                n,
                Some(
                    PdfValue::Stream {
                        dictionary: Vec::new(),
                        data: palette.to_vec(),
                    }
                    .serialize()?
                    .into(),
                ),
            );
        }
        if image.colorspace != 0 {
            colorspace = format!("{} 0 R", image.colorspace)
        }
        image.dictionary = format!(
            "/Type /XObject /Subtype /Image /Width {} /Height {} /BitsPerComponent {} /ColorSpace {colorspace} {}",
            info.width, info.height, payload.depth, payload.filter
        );
        if let Some(mask) = payload.mask {
            let n = r.reserve_object();
            let dictionary = vec![
                ("Type".into(), PdfValue::Name("XObject".into())),
                ("Subtype".into(), PdfValue::Name("Image".into())),
                ("ColorSpace".into(), PdfValue::Name("DeviceGray".into())),
                ("Width".into(), PdfValue::Integer(info.width as i64)),
                ("Height".into(), PdfValue::Integer(info.height as i64)),
                ("BitsPerComponent".into(), PdfValue::Integer(8)),
            ];
            doc.imported_objects.insert(
                n,
                Some(
                    PdfValue::Stream {
                        dictionary,
                        data: mask.to_vec(),
                    }
                    .serialize()?
                    .into(),
                ),
            );
            image.dictionary.push_str(&format!(" /SMask {n} 0 R"));
        }
        image.data = Some(payload.data.into())
    }
    image.dictionary.push(' ');
    image.dictionary.push_str(&image.attributes);
    doc.images.insert(number, image);
    Ok(())
}
pub fn write(doc: &PDFDocument, w: &mut PdfWriter) -> Result<BTreeSet<usize>> {
    let mut free = BTreeSet::new();
    for use_ in doc.jbig2_globals.values() {
        w.add_stream_object(
            &jbig2::read(&use_.source)?.global_data(&use_.needed),
            "",
            Some(use_.number),
            None,
        )?;
    }
    for image in doc.images.values() {
        if let Some(data) = &image.data {
            w.add_stream_object(data, &image.dictionary, Some(image.number), None)?;
        } else {
            free.insert(image.number);
            if image.group > 0 && image.group as usize != doc.transparency_group {
                free.insert(image.group as usize);
            }
        }
    }
    for (&n, data) in &doc.imported_objects {
        if let Some(data) = data {
            w.add_object(data.as_ref().to_vec(), Some(n), 1)?;
        } else {
            free.insert(n);
        }
    }
    if doc.transparency_group != 0 {
        if doc
            .images
            .values()
            .any(|i| i.group == doc.transparency_group as i64 && i.data.is_some())
        {
            w.add_text_object(
                "<< /S /Transparency /CS /DeviceRGB /I true >>",
                Some(doc.transparency_group),
                2,
            )?;
        } else {
            free.insert(doc.transparency_group);
        }
    }
    Ok(free)
}
