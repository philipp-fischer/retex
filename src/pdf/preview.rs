//! Retained paint input from prepared display lists, without PDF serialization.
//! Unsupported outline/paint capabilities fail explicitly. This initial scene
//! supports Type1 and SFNT paths; bitmap/standard-14 and imported graphics remain.
use crate::{
    Engine,
    core::{
        errors::{Error, Result},
        units::sp_to_bp,
    },
    fonts::{
        formats::type1_outlines::OutlineCommand, materialization::FontResource,
        outlines::OutlineCache,
    },
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Clone, Serialize)]
pub struct PreviewFont {
    pub source_sha256: String,
    pub bytes_per_code: u8,
    pub matrix: [f64; 6],
    pub widths: BTreeMap<u32, f64>,
    #[serde(serialize_with = "font_wire::serialize_paths")]
    pub paths: BTreeMap<u32, Arc<[OutlineCommand]>>,
    pub path_ids: BTreeMap<u32, String>,
}

// OutlineCache has already validated the resource/content identity. Preserve
// its immutable path allocation when caching the exact JSON; neither pixels
// nor matching glyph names authorize this shortcut. Keeping a strong owner
// prevents pointer reuse and makes later Arc::make_mut changes independent.
mod font_wire {
    use super::*;
    use std::{
        cell::RefCell,
        collections::{HashMap, VecDeque},
    };
    const MAX_BYTES: usize = 8 * 1024 * 1024;
    struct Identity {
        source: std::sync::Weak<FontResource>,
        digest: String,
    }
    #[derive(Default)]
    struct Identities {
        entries: HashMap<usize, Identity>,
        order: VecDeque<usize>,
        hits: usize,
        misses: usize,
        hashed: usize,
    }
    thread_local! { static IDENTITIES: RefCell<Identities> = RefCell::new(Identities::default()); }
    pub(super) fn identity_statistics() -> (usize, usize, usize, usize) {
        IDENTITIES.with(|c| {
            let c = c.borrow();
            (
                c.entries.len() * (std::mem::size_of::<FontResource>() + 512),
                c.hits,
                c.misses,
                c.hashed,
            )
        })
    }
    pub(super) fn source_identity(source: &Arc<FontResource>) -> String {
        let began = crate::source_map::profile_start();
        let result = IDENTITIES.with(|cache| {
            let mut c = cache.borrow_mut();
            let key = Arc::as_ptr(source) as usize;
            if let Some(entry) = c.entries.get(&key).filter(|e| {
                e.source
                    .upgrade()
                    .is_some_and(|old| Arc::ptr_eq(&old, source))
            }) {
                let digest = entry.digest.clone();
                c.hits += 1;
                return digest;
            }
            c.misses += 1;
            c.hashed += source.data().len();
            #[cfg(feature = "latency-diagnostics")]
            crate::latency_diagnostics::count("hashed_source_bytes", source.data().len());
            let digest = format!("{:x}", Sha256::digest(source.data()));
            while c.entries.len() >= 64
                || (c.entries.len() + 1) * (std::mem::size_of::<FontResource>() + 512) > 128 * 1024
            {
                let Some(old) = c.order.pop_front() else {
                    break;
                };
                c.entries.remove(&old);
            }
            c.order.push_back(key);
            c.entries.insert(
                key,
                Identity {
                    source: Arc::downgrade(source),
                    digest: digest.clone(),
                },
            );
            digest
        });
        crate::source_map::record_font_identity_time(began);
        result
    }
    struct Entry {
        path: Arc<[OutlineCommand]>,
        wire: Arc<serde_json::value::RawValue>,
        bytes: usize,
    }
    #[derive(Default)]
    struct Cache {
        entries: HashMap<usize, Entry>,
        order: VecDeque<usize>,
        bytes: usize,
        hits: usize,
        misses: usize,
    }
    thread_local! { static CACHE: RefCell<Cache> = RefCell::new(Cache::default()); }
    pub(super) fn statistics() -> (usize, usize, usize) {
        CACHE.with(|c| {
            let c = c.borrow();
            (c.bytes, c.hits, c.misses)
        })
    }
    fn encoded(
        path: &Arc<[OutlineCommand]>,
    ) -> std::result::Result<Arc<serde_json::value::RawValue>, serde_json::Error> {
        CACHE.with(|cache| {
            let mut c = cache.borrow_mut();
            let key = Arc::as_ptr(path) as *const OutlineCommand as usize;
            if let Some(entry) = c.entries.get(&key).filter(|e| Arc::ptr_eq(&e.path, path)) {
                let wire = entry.wire.clone();
                c.hits += 1;
                return Ok(wire);
            }
            c.misses += 1;
            let wire: Arc<serde_json::value::RawValue> =
                serde_json::value::to_raw_value(path)?.into();
            let bytes = std::mem::size_of_val(path.as_ref()) + wire.get().len() + 128;
            if bytes <= MAX_BYTES {
                while c.bytes + bytes > MAX_BYTES || c.entries.len() >= 2048 {
                    let Some(old) = c.order.pop_front() else {
                        break;
                    };
                    if let Some(entry) = c.entries.remove(&old) {
                        c.bytes -= entry.bytes;
                    }
                }
                c.bytes += bytes;
                c.order.push_back(key);
                c.entries.insert(
                    key,
                    Entry {
                        path: path.clone(),
                        wire: wire.clone(),
                        bytes,
                    },
                );
            }
            Ok(wire)
        })
    }
    pub(super) fn serialize_paths<S: serde::Serializer>(
        paths: &BTreeMap<u32, Arc<[OutlineCommand]>>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::{
            Serialize,
            ser::{Error, SerializeMap},
        };
        if !serializer.is_human_readable() {
            return paths.serialize(serializer);
        }
        let mut map = serializer.serialize_map(Some(paths.len()))?;
        for (code, path) in paths {
            map.serialize_entry(code, &encoded(path).map_err(S::Error::custom)?)?;
        }
        map.end()
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn immutable_paths_reuse_exact_wire_bytes_and_copy_on_write_replacements_do_not() {
            #[derive(Serialize)]
            struct Cached<'a>(
                #[serde(serialize_with = "serialize_paths")]
                &'a BTreeMap<u32, Arc<[OutlineCommand]>>,
            );
            let path: Arc<[OutlineCommand]> = vec![
                OutlineCommand::MoveTo { x: 1.25, y: 2.0 },
                OutlineCommand::LineTo { x: 3.0, y: 4.0 },
            ]
            .into();
            let paths = BTreeMap::from([(65, path.clone())]);
            let expected = serde_json::to_string(&paths).unwrap();
            assert_eq!(serde_json::to_string(&Cached(&paths)).unwrap(), expected);
            let before = statistics();
            assert_eq!(serde_json::to_string(&Cached(&paths)).unwrap(), expected);
            assert!(statistics().1 > before.1);
            let mut changed = path.clone();
            Arc::make_mut(&mut changed)[0] = OutlineCommand::MoveTo { x: 9.0, y: 2.0 };
            let replacement = BTreeMap::from([(65, changed)]);
            let actual = serde_json::to_string(&Cached(&replacement)).unwrap();
            assert_ne!(actual, expected);
            assert_eq!(actual, serde_json::to_string(&replacement).unwrap());
            assert_eq!(
                bincode::serialize(&Cached(&paths)).unwrap(),
                bincode::serialize(&paths).unwrap()
            );
            assert!(statistics().0 <= MAX_BYTES);
        }
        #[test]
        fn font_digests_reuse_only_live_immutable_resource_owners() {
            use crate::fonts::formats::bitmap::{BitmapKind, BitmapResource};
            let mut source = Arc::new(FontResource::Bitmap(BitmapResource {
                data: b"first".to_vec(),
                name: "font".into(),
                glyphs: BTreeMap::new(),
                dpi: 72,
                scale: 1.0,
                kind: BitmapKind::Pk,
                encoding: vec![],
            }));
            let original = source_identity(&source);
            let before = identity_statistics();
            assert_eq!(source_identity(&source), original);
            assert!(identity_statistics().1 > before.1);
            if let FontResource::Bitmap(bitmap) = Arc::make_mut(&mut source) {
                bitmap.data = b"other".to_vec();
            }
            assert_ne!(source_identity(&source), original);
            assert_eq!(
                source_identity(&source),
                format!("{:x}", Sha256::digest(b"other"))
            );
            let weak = Arc::downgrade(&source);
            drop(source);
            assert!(weak.upgrade().is_none());
            assert!(identity_statistics().0 <= 128 * 1024);
        }
    }
}
pub(crate) fn font_wire_statistics() -> (usize, usize, usize) {
    font_wire::statistics()
}
pub(crate) fn font_identity_statistics() -> (usize, usize, usize, usize) {
    font_wire::identity_statistics()
}
#[derive(Clone, Serialize)]
pub struct PreviewPage {
    pub source_map: Arc<crate::source_map::SourceMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provisional_regions: Option<Vec<crate::paragraph_preview::RegionMap>>,
    pub page: usize,
    pub width_bp: f64,
    pub height_bp: f64,
    pub magnification: i64,
    pub operators: Vec<String>,
    pub content_sha256: String,
}
#[derive(Clone, Serialize)]
pub struct RetainedPreview {
    pub schema_version: u32,
    pub complete: bool,
    pub fonts: BTreeMap<String, PreviewFont>,
    pub images: BTreeMap<String, super::preview_images::PreviewImage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_keys: Option<Vec<String>>,
    pub pages: Vec<PreviewPage>,
}

impl RetainedPreview {
    /// Separate raster bytes from the small JSON metadata at the browser ABI.
    /// Offsets describe RGBA and optional display-converted RGBA respectively.
    pub fn pack_pixels(&mut self) -> (BTreeMap<String, [usize; 4]>, Vec<u8>) {
        let mut spans = BTreeMap::new();
        let mut pixels = Vec::new();
        for (name, image) in &mut self.images {
            let offset = pixels.len();
            let length = image.rgba.len();
            pixels.extend_from_slice(&image.rgba);
            image.rgba = Arc::from([]);
            let display_offset = pixels.len();
            if let Some(color) = &mut image.device_cmyk {
                pixels.extend_from_slice(&color.display_rgba);
                color.display_rgba = Arc::from([]);
            }
            spans.insert(
                name.clone(),
                [
                    offset,
                    length,
                    display_offset,
                    pixels.len() - display_offset,
                ],
            );
        }
        (spans, pixels)
    }
}

// Observe the actual PDF paint stream, including text in literal operators.
// Emitted TeX glyph records alone cannot describe a page's font dependencies.
struct PaintCodes<'a> {
    code_widths: &'a BTreeMap<String, u8>,
    font: Option<String>,
    stack: Vec<Option<String>>,
    codes: BTreeMap<String, BTreeSet<u32>>,
}
impl<'a> PaintCodes<'a> {
    fn new(code_widths: &'a BTreeMap<String, u8>) -> Self {
        Self {
            code_widths,
            font: None,
            stack: Vec::new(),
            codes: BTreeMap::new(),
        }
    }
    fn show(&mut self, value: &lopdf::Object) -> Result<()> {
        let name = self
            .font
            .as_ref()
            .ok_or_else(|| Error::value("Preview text has no selected font"))?;
        let unit = *self
            .code_widths
            .get(name)
            .ok_or_else(|| Error::value(format!("Unprovisioned preview font {name}")))?
            as usize;
        let bytes = value
            .as_str()
            .map_err(|_| Error::value("Preview text string required"))?;
        if ![1, 2].contains(&unit) || bytes.len() % unit != 0 {
            return Err(Error::value("Invalid preview character code width"));
        }
        let used = self.codes.entry(name.clone()).or_default();
        for code in bytes.chunks_exact(unit) {
            used.insert(if unit == 1 {
                code[0] as u32
            } else {
                u16::from_be_bytes([code[0], code[1]]) as u32
            });
        }
        Ok(())
    }
    fn observe(&mut self, operation: &lopdf::content::Operation) -> Result<()> {
        use lopdf::Object;
        let operands = &operation.operands;
        match operation.operator.as_str() {
            "q" => {
                if self.stack.len() >= 256 {
                    return Err(Error::value("Preview graphics stack budget exceeded"));
                }
                self.stack.push(self.font.clone());
            }
            "Q" => {
                self.font = self
                    .stack
                    .pop()
                    .ok_or_else(|| Error::value("Unmatched preview graphics restore"))?;
            }
            "Tf" if operands.len() == 2 => {
                let name = operands[0]
                    .as_name()
                    .map_err(|_| Error::value("Invalid preview font name"))?;
                self.font = Some(name.iter().map(|&byte| char::from(byte)).collect());
            }
            "Tj" | "'" if operands.len() == 1 => self.show(&operands[0])?,
            "\"" if operands.len() == 3 => self.show(&operands[2])?,
            "TJ" if operands.len() == 1 => {
                for value in operands[0]
                    .as_array()
                    .map_err(|_| Error::value("Preview TJ array required"))?
                {
                    match value {
                        Object::Integer(_) | Object::Real(_) => {}
                        _ => self.show(value)?,
                    }
                }
            }
            "Tf" | "Tj" | "TJ" | "'" | "\"" => {
                return Err(Error::value("Invalid preview text operand arity"));
            }
            _ => {}
        }
        Ok(())
    }
    fn save(&mut self) -> Result<()> {
        if self.stack.len() >= 256 {
            return Err(Error::value("Preview graphics stack budget exceeded"));
        }
        self.stack.push(self.font.clone());
        Ok(())
    }
    fn restore(&mut self) -> Result<()> {
        self.font = self
            .stack
            .pop()
            .ok_or_else(|| Error::value("Unmatched preview graphics restore"))?;
        Ok(())
    }
}
fn paint_codes(
    operators: &[String],
    code_widths: &BTreeMap<String, u8>,
) -> Result<BTreeMap<String, BTreeSet<u32>>> {
    let bytes = crate::pdf::writer::latin1(&operators.join("\n"))?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err(Error::value("Preview operator byte budget exceeded"));
    }
    let content = lopdf::content::Content::decode(&bytes)
        .map_err(|error| Error::value(format!("Invalid preview paint stream: {error}")))?;
    let mut codes = PaintCodes::new(code_widths);
    for operation in content.operations {
        codes.observe(&operation)?;
    }
    Ok(codes.codes)
}

// Retain source decimal spelling: lopdf's decoded Real is f32, so encoding
// decoded operations would change TeX's prepared coordinates. Composite PDF
// values are one token; words inside strings, dictionaries or arrays cannot Do.
fn paint_tokens(bytes: &[u8]) -> Result<Vec<std::ops::Range<usize>>> {
    fn delimiter(byte: u8) -> bool {
        byte.is_ascii_whitespace()
            || matches!(
                byte,
                0 | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'/' | b'%'
            )
    }
    fn space(bytes: &[u8], at: &mut usize) {
        loop {
            while *at < bytes.len() && (bytes[*at].is_ascii_whitespace() || bytes[*at] == 0) {
                *at += 1;
            }
            if bytes.get(*at) != Some(&b'%') {
                break;
            }
            while *at < bytes.len() && !matches!(bytes[*at], b'\r' | b'\n') {
                *at += 1;
            }
        }
    }
    fn value(bytes: &[u8], at: &mut usize, depth: usize) -> Result<()> {
        if depth >= 256 {
            return Err(Error::value("Preview PDF nesting budget exceeded"));
        }
        match bytes[*at] {
            b'(' => {
                *at += 1;
                let mut nested = 1usize;
                while *at < bytes.len() {
                    let byte = bytes[*at];
                    *at += 1;
                    match byte {
                        b'\\' => {
                            if *at < bytes.len() {
                                *at += 1;
                            }
                        }
                        b'(' => {
                            nested += 1;
                            if nested > 256 {
                                return Err(Error::value("Preview PDF nesting budget exceeded"));
                            }
                        }
                        b')' => {
                            nested -= 1;
                            if nested == 0 {
                                return Ok(());
                            }
                        }
                        _ => {}
                    }
                }
            }
            b'[' | b'<' if bytes[*at] == b'[' || bytes.get(*at + 1) == Some(&b'<') => {
                let dictionary = bytes[*at] == b'<';
                *at += if dictionary { 2 } else { 1 };
                loop {
                    space(bytes, at);
                    if *at >= bytes.len() {
                        break;
                    }
                    if !dictionary && bytes[*at] == b']' {
                        *at += 1;
                        return Ok(());
                    }
                    if dictionary && bytes[*at..].starts_with(b">>") {
                        *at += 2;
                        return Ok(());
                    }
                    value(bytes, at, depth + 1)?;
                }
            }
            b'<' => {
                *at += 1;
                while *at < bytes.len() {
                    let byte = bytes[*at];
                    *at += 1;
                    if byte == b'>' {
                        return Ok(());
                    }
                }
            }
            b')' | b']' | b'>' => return Err(Error::value("Unmatched preview PDF delimiter")),
            _ => {
                if bytes[*at] == b'/' {
                    *at += 1;
                }
                while *at < bytes.len() && !delimiter(bytes[*at]) {
                    *at += 1;
                }
                return Ok(());
            }
        }
        Err(Error::value("Incomplete preview PDF value"))
    }
    let mut at = 0;
    let mut tokens = Vec::new();
    loop {
        space(bytes, &mut at);
        if at == bytes.len() {
            return Ok(tokens);
        }
        let start = at;
        value(bytes, &mut at, 0)?;
        if tokens.len() >= 1_000_000 {
            return Err(Error::value("Preview PDF token budget exceeded"));
        }
        tokens.push(start..at);
    }
}

// Expand retained Form XObjects only for painting. PDF export keeps the original
// streams/resources. The private boundary operators preserve the caller's path
// and text matrices, which PDF q/Q alone do not save.
fn form_operators(
    engine: &PreviewContext<'_>,
    bytes: &[u8],
    xobjects: &BTreeMap<String, usize>,
    active: &mut BTreeSet<usize>,
    remaining: &mut usize,
    images: &mut BTreeMap<String, super::preview_images::PreviewImage>,
    codes: &mut PaintCodes<'_>,
) -> Result<Vec<String>> {
    use lopdf::{Object, content::Content};
    if bytes.len() > *remaining {
        return Err(Error::value("Preview form byte budget exceeded"));
    }
    *remaining -= bytes.len();
    let content = Content::decode(bytes)
        .map_err(|error| Error::value(format!("Invalid preview paint stream: {error}")))?;
    let spans = paint_tokens(bytes)?;
    let mut result = Vec::new();
    let mut start = 0;
    let mut operations = content.operations.into_iter();
    for span in spans {
        let token = &bytes[span.clone()];
        if matches!(token.first(), Some(b'/' | b'(' | b'<' | b'['))
            || matches!(token, b"true" | b"false" | b"null")
            || std::str::from_utf8(token).is_ok_and(|text| text.parse::<f64>().is_ok())
        {
            continue;
        }
        let operation = operations
            .next()
            .ok_or_else(|| Error::value("Invalid preview operator boundary"))?;
        if operation.operator.as_bytes() != token {
            return Err(Error::value("Invalid preview operator boundary"));
        }
        if matches!(
            operation.operator.as_str(),
            "retexFormBegin" | "retexFormEnd" | "retexImage"
        ) {
            return Err(Error::value(
                "Reserved preview form boundary in source paint",
            ));
        }
        if operation.operator != "Do" {
            codes.observe(&operation)?;
            result.push(
                bytes[start..span.end]
                    .iter()
                    .map(|&byte| char::from(byte))
                    .collect(),
            );
            start = span.end;
            continue;
        }
        start = span.end;
        let name = match operation.operands.as_slice() {
            [Object::Name(name)] => name
                .iter()
                .map(|&byte| char::from(byte))
                .collect::<String>(),
            _ => return Err(Error::value("Invalid preview XObject operand")),
        };
        let number = xobjects
            .get(&name)
            .ok_or_else(|| Error::value(format!("Unprovisioned preview XObject {name}")))?;
        if let Some(image) = engine.backend.document.images.get(number) {
            if image.format == "PDF" {
                let attributes = super::parsing::dictionary(&image.attributes)?;
                if attributes.entries()?.iter().any(|(key, _)| {
                    matches!(
                        key.as_str(),
                        "Type"
                            | "Subtype"
                            | "BBox"
                            | "Matrix"
                            | "Resources"
                            | "Group"
                            | "OC"
                            | "Ref"
                    )
                }) {
                    return Err(Error::value(
                        "Preview imported image dictionary override is not implemented",
                    ));
                }
                if active.len() >= 64 || !active.insert(*number) {
                    return Err(Error::value("Preview form recursion budget exceeded"));
                }
                let dictionary = super::parsing::dictionary(&image.dictionary)?;
                result.extend(imported_form_operators(
                    engine,
                    &dictionary,
                    image
                        .data
                        .as_deref()
                        .ok_or_else(|| Error::value("Unemitted preview PDF image"))?,
                    active,
                    remaining,
                    images,
                    codes,
                )?);
                active.remove(number);
                continue;
            }
            let (identity, image) = super::preview_images::capture_cached(
                &engine.backend.document,
                image,
                &mut engine.images.borrow_mut(),
            )?;
            retain_image(images, identity.clone(), image)?;
            result.push(format!("/{identity} retexImage"));
            continue;
        }
        if let Some(Some(bytes)) = engine.backend.document.imported_objects.get(number) {
            if bytes.len() > 32 * 1024 * 1024 || active.len() >= 64 || !active.insert(*number) {
                return Err(Error::value("Preview imported XObject budget exceeded"));
            }
            let value = super::parsing::parse_indirect_body(bytes)?;
            match value.get("Subtype") {
                Some(super::parsing::PdfValue::Name(name)) if name == "Form" => {
                    let data = super::preview_images::raw_stream(&value)?;
                    result.extend(imported_form_operators(
                        engine, &value, &data, active, remaining, images, codes,
                    )?);
                }
                Some(super::parsing::PdfValue::Name(name)) if name == "Image" => {
                    let super::parsing::PdfValue::Stream { dictionary, data } = value else {
                        return Err(Error::value("Preview imported image stream required"));
                    };
                    let (identity, image) = engine.images.borrow_mut().capture_stream(
                        &engine.backend.document,
                        &super::parsing::PdfValue::Dictionary(dictionary),
                        &data,
                    )?;
                    retain_image(images, identity.clone(), image)?;
                    result.push(format!("/{identity} retexImage"));
                }
                _ => return Err(Error::value("Unsupported preview imported XObject subtype")),
            }
            active.remove(number);
            continue;
        }
        let form = engine
            .backend
            .document
            .forms
            .get(number)
            .ok_or_else(|| Error::value("Image/imported XObject preview is not implemented"))?;
        if active.len() >= 64 || !active.insert(*number) {
            return Err(Error::value("Preview form recursion budget exceeded"));
        }
        if form.group != 0 {
            return Err(Error::value(
                "Transparency group preview is not implemented",
            ));
        }
        let dictionary = |source: &str| -> Result<lopdf::Dictionary> {
            let bytes = crate::pdf::writer::latin1(&format!("<< {source} >> retexDictionary"))?;
            let parsed =
                Content::decode(&bytes).map_err(|error| Error::value(error.to_string()))?;
            match parsed.operations.as_slice() {
                [operation] if operation.operator == "retexDictionary" => {
                    match operation.operands.as_slice() {
                        [Object::Dictionary(value)] => Ok(value.clone()),
                        _ => Err(Error::value("Invalid preview form dictionary")),
                    }
                }
                _ => Err(Error::value("Invalid preview form dictionary")),
            }
        };
        let attributes = dictionary(&form.attributes)?;
        let resources = dictionary(&form.extra_resources)?;
        for key in [b"Resources".as_slice(), b"Group", b"OC", b"Ref"] {
            if attributes.has(key) {
                return Err(Error::value(
                    "Preview form resource/group override is not implemented",
                ));
            }
        }
        for key in [b"Font".as_slice(), b"XObject"] {
            if resources.has(key) {
                return Err(Error::value(
                    "Preview form font/XObject override is not implemented",
                ));
            }
        }
        let attribute_bytes = crate::pdf::writer::latin1(&form.attributes)?;
        let tokens = paint_tokens(&attribute_bytes)?;
        let mut raw_attributes = BTreeMap::new();
        let mut at = 0;
        while at < tokens.len() {
            let key = &attribute_bytes[tokens[at].clone()];
            let parsed = Content::decode(&[key, b" retexKey"].concat())
                .map_err(|error| Error::value(error.to_string()))?;
            let name = parsed
                .operations
                .first()
                .and_then(|op| op.operands.first())
                .and_then(|value| value.as_name().ok())
                .ok_or_else(|| Error::value("Invalid preview form attribute name"))?;
            let span = tokens
                .get(at + 1)
                .ok_or_else(|| Error::value("Missing preview form attribute value"))?;
            raw_attributes.insert(name.to_vec(), &attribute_bytes[span.clone()]);
            at += 2;
            // A PDF indirect reference occupies three lexical values.
            if tokens
                .get(at + 1)
                .is_some_and(|span| &attribute_bytes[span.clone()] == b"R")
            {
                at += 2;
            }
        }
        let array = |key: &[u8], default: &[f64]| -> Result<Vec<f64>> {
            let Some(raw) = raw_attributes.get(key) else {
                return Ok(default.to_vec());
            };
            if raw.first() != Some(&b'[') || raw.last() != Some(&b']') {
                return Err(Error::value("Invalid preview form geometry"));
            }
            let raw = &raw[1..raw.len() - 1];
            let tokens = paint_tokens(raw)?;
            if tokens.len() != default.len() {
                return Err(Error::value("Invalid preview form geometry"));
            }
            tokens
                .iter()
                .map(|span| {
                    let value = std::str::from_utf8(&raw[span.clone()])
                        .ok()
                        .and_then(|text| text.parse::<f64>().ok())
                        .filter(|value| value.is_finite())
                        .ok_or_else(|| Error::value("Invalid preview form geometry"))?;
                    Ok(value)
                })
                .collect()
        };
        // Use the same decimal precision as the exported Form dictionary.
        let precision = engine
            .backend
            .document
            .decimal_digits
            .ok_or_else(|| Error::value("Missing preview form precision"))?;
        let rounded = |value| {
            format!("{:.*}", precision, sp_to_bp(value))
                .parse::<f64>()
                .unwrap()
        };
        let bbox = array(
            b"BBox",
            &[
                0.0,
                0.0,
                rounded(form.width),
                rounded(form.height + form.depth),
            ],
        )?;
        let matrix = array(b"Matrix", &[1.0, 0.0, 0.0, 1.0, 0.0, 0.0])?;
        codes.save()?;
        result.push("retexFormBegin q".into());
        result.push(format!(
            "{} cm",
            matrix
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        ));
        result.push(format!(
            "{} {} {} {} re W n",
            bbox[0],
            bbox[1],
            bbox[2] - bbox[0],
            bbox[3] - bbox[1]
        ));
        result.extend(form_operators(
            engine,
            form.data
                .as_deref()
                .ok_or_else(|| Error::value("Uncaptured preview form"))?,
            &form.xobjects,
            active,
            remaining,
            images,
            codes,
        )?);
        codes.restore()?;
        result.push("Q retexFormEnd".into());
        active.remove(number);
    }
    if operations.next().is_some() {
        return Err(Error::value("Missing preview operator boundary"));
    }
    Ok(result)
}

fn retain_image(
    images: &mut BTreeMap<String, super::preview_images::PreviewImage>,
    identity: String,
    image: super::preview_images::PreviewImage,
) -> Result<()> {
    let total = images.values().map(|image| image.byte_len()).sum::<usize>();
    if !images.contains_key(&identity)
        && (images.len() >= 256 || total + image.byte_len() > 32 * 1024 * 1024)
    {
        return Err(Error::value("Retained preview image budget exceeded"));
    }
    images.entry(identity).or_insert(image);
    Ok(())
}

fn imported_form_operators(
    engine: &PreviewContext<'_>,
    dictionary: &super::parsing::PdfValue,
    data: &[u8],
    active: &mut BTreeSet<usize>,
    remaining: &mut usize,
    images: &mut BTreeMap<String, super::preview_images::PreviewImage>,
    codes: &mut PaintCodes<'_>,
) -> Result<Vec<String>> {
    use super::parsing::PdfValue;
    let document = &engine.backend.document;
    for key in ["Group", "OC", "Ref"] {
        if dictionary.get(key).is_some() {
            return Err(Error::value(format!(
                "Preview imported form {key} is not implemented"
            )));
        }
    }
    let resources = dictionary
        .get("Resources")
        .map(|value| super::preview_images::imported(document, value))
        .transpose()?
        .unwrap_or(PdfValue::Dictionary(Vec::new()));
    resources.entries()?;
    if let Some(fonts) = resources.get("Font") {
        if !super::preview_images::imported(document, fonts)?
            .entries()?
            .is_empty()
        {
            return Err(Error::value(
                "Preview imported font resources are not implemented",
            ));
        }
    }
    let mut xobjects = BTreeMap::new();
    if let Some(value) = resources.get("XObject") {
        let value = super::preview_images::imported(document, value)?;
        for (name, value) in value.entries()? {
            let PdfValue::Reference(number, 0) = value else {
                return Err(Error::value("Preview imported XObject reference required"));
            };
            xobjects.insert(name.clone(), *number as usize);
        }
    }
    let array = |key, default: Option<&[f64]>| -> Result<Vec<f64>> {
        match dictionary.get(key) {
            Some(PdfValue::Array(values)) => values
                .iter()
                .map(|value| {
                    let n = value.number()?;
                    if !n.is_finite() {
                        return Err(Error::value("Invalid preview imported geometry"));
                    }
                    Ok(n)
                })
                .collect(),
            None => default
                .map(|v| v.to_vec())
                .ok_or_else(|| Error::value("Missing preview imported bounding box")),
            _ => Err(Error::value("Invalid preview imported geometry array")),
        }
    };
    let bbox = array("BBox", None)?;
    let matrix = array("Matrix", Some(&[1., 0., 0., 1., 0., 0.]))?;
    if bbox.len() != 4 || matrix.len() != 6 {
        return Err(Error::value("Invalid preview imported geometry length"));
    }
    codes.save()?;
    let mut operators = vec![
        "retexFormBegin q".into(),
        format!(
            "{} cm",
            matrix
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        ),
        format!(
            "{} {} {} {} re W n",
            bbox[0],
            bbox[1],
            bbox[2] - bbox[0],
            bbox[3] - bbox[1]
        ),
    ];
    operators.extend(form_operators(
        engine, data, &xobjects, active, remaining, images, codes,
    )?);
    codes.restore()?;
    operators.push("Q retexFormEnd".into());
    Ok(operators)
}

struct PreviewContext<'a> {
    backend: &'a super::backend::PDFBackend,
    fonts: &'a crate::fonts::catalog::FontCatalog,
    pages: &'a [crate::typesetting::display::PageDisplayList],
    complete: bool,
    images: std::cell::RefCell<&'a mut super::preview_images::ImageCache>,
}
pub fn capture(engine: &Engine, cache: &mut OutlineCache) -> Result<RetainedPreview> {
    capture_with_images(
        engine,
        cache,
        &mut super::preview_images::ImageCache::default(),
    )
}
pub fn capture_with_images(
    engine: &Engine,
    cache: &mut OutlineCache,
    images: &mut super::preview_images::ImageCache,
) -> Result<RetainedPreview> {
    if engine.status() == "running" {
        return Err(Error::value("Cannot capture an unfinished preview"));
    }
    capture_context(
        &PreviewContext {
            backend: &engine.core.backend,
            fonts: &engine.fonts,
            pages: &engine.core.output_pages,
            complete: engine.status() == "complete",
            images: std::cell::RefCell::new(images),
        },
        cache,
    )
}
/// Read-only observation of one fully shipped page in the current execution.
/// Preparation has isolated font/PDF/register state; it publishes no TeX effects.
pub fn capture_page(
    engine: &Engine,
    page: usize,
    cache: &mut OutlineCache,
) -> Result<RetainedPreview> {
    capture_page_with_images(
        engine,
        page,
        cache,
        &mut super::preview_images::ImageCache::default(),
    )
}
pub fn capture_page_with_images(
    engine: &Engine,
    page: usize,
    cache: &mut OutlineCache,
    images: &mut super::preview_images::ImageCache,
) -> Result<RetainedPreview> {
    let (backend, fonts, prepared) = engine.prepare_preview_page(page)?;
    let result = capture_context(
        &PreviewContext {
            backend: &backend,
            fonts: &fonts,
            pages: std::slice::from_ref(&prepared),
            complete: false,
            images: std::cell::RefCell::new(images),
        },
        cache,
    );
    #[cfg(feature = "latency-diagnostics")]
    {
        let _drop = crate::latency_diagnostics::phase(
            "preview_observer_drop",
            "prepared page, fonts and backend",
            None,
        );
        drop(prepared);
        drop(fonts);
        drop(backend);
    }
    result
}

pub(crate) fn capture_prepared(
    backend: &super::backend::PDFBackend,
    fonts: &crate::fonts::catalog::FontCatalog,
    page: &crate::typesetting::display::PageDisplayList,
    cache: &mut OutlineCache,
    images: &mut super::preview_images::ImageCache,
) -> Result<RetainedPreview> {
    capture_context(
        &PreviewContext {
            backend,
            fonts,
            pages: std::slice::from_ref(page),
            complete: false,
            images: std::cell::RefCell::new(images),
        },
        cache,
    )
}

fn capture_context(
    engine: &PreviewContext<'_>,
    cache: &mut OutlineCache,
) -> Result<RetainedPreview> {
    let suffix = engine
        .backend
        .document
        .resource_suffix
        .as_deref()
        .unwrap_or("");
    let code_widths = engine
        .backend
        .resources
        .font_groups
        .iter()
        .map(|group| {
            (
                format!("F{}{suffix}", group.font),
                if engine.fonts.items[group.font].unicode_font.is_some() {
                    2
                } else {
                    1
                },
            )
        })
        .collect();
    let mut images = BTreeMap::new();
    let mut page_codes = Vec::new();
    let page_operators = engine
        .pages
        .iter()
        .map(|page| {
            let operators = page
                .lines
                .iter()
                .flat_map(|line| line.operators.iter().cloned())
                .collect::<Vec<_>>();
            let bytes = crate::pdf::writer::latin1(&operators.join("\n"))?;
            let resources = engine.backend.resources.pages.get(page.page - 1);
            if let Some(resources) = resources.filter(|resources| !resources.xobjects.is_empty()) {
                let mut codes = PaintCodes::new(&code_widths);
                let operators = form_operators(
                    engine,
                    &bytes,
                    &resources.xobjects,
                    &mut BTreeSet::new(),
                    &mut (32 * 1024 * 1024),
                    &mut images,
                    &mut codes,
                )?;
                // The expanded stream still has the same total byte budget as
                // the independent paint-code scan, including form wrappers.
                if operators
                    .iter()
                    .map(|s| s.chars().count() + 1)
                    .sum::<usize>()
                    .saturating_sub(1)
                    > 32 * 1024 * 1024
                {
                    return Err(Error::value("Preview operator byte budget exceeded"));
                }
                page_codes.push(codes.codes);
                Ok(operators)
            } else {
                page_codes.push(paint_codes(&operators, &code_widths)?);
                Ok(operators)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let mut fonts = BTreeMap::new();
    let mut path_bytes = 0usize;
    let mut glyph_count = 0usize;
    for group in &engine.backend.resources.font_groups {
        let font = &engine.fonts.items[group.font];
        let name = format!("F{}{suffix}", group.font);
        let codes = page_codes
            .iter()
            .filter_map(|page| page.get(&name))
            .flatten()
            .copied()
            .collect::<BTreeSet<_>>();
        if !page_codes.iter().any(|page| page.contains_key(&name)) {
            continue;
        }
        #[cfg(feature = "latency-diagnostics")]
        let _diag = crate::latency_diagnostics::phase("preview_font_preparation", &font.name, None);
        let mut paths = BTreeMap::new();
        let mut widths = BTreeMap::new();
        let mut path_ids = BTreeMap::new();
        let (identity, matrix, bytes_per_code) = if let Some(program) = &font.unicode_font {
            let identity = program.sha256.clone();
            for (&(gid, ref _text), &cid) in &group.unicode_codes {
                if !codes.contains(&(cid as u32)) {
                    continue;
                }
                paths.insert(cid as u32, cache.sfnt(&identity, &program.data, gid)?);
                path_ids.insert(cid as u32, format!("{identity}:gid:{gid}"));
                let width = format!(
                    "{:.6}",
                    1000.0 * program.advances[gid as usize] as f64 / program.upem as f64
                )
                .parse::<f64>()
                .map_err(|e| Error::value(e.to_string()))?;
                widths.insert(cid as u32, width / 1000.0);
            }
            (
                identity,
                [
                    1.0 / program.upem as f64,
                    0.0,
                    0.0,
                    1.0 / program.upem as f64,
                    0.0,
                    0.0,
                ],
                2,
            )
        } else {
            let mut declared = engine
                .pages
                .iter()
                .flat_map(|page| &page.lines)
                .flat_map(|line| &line.emitted)
                .filter(|glyph| group.members.contains(&glyph.font))
                .filter_map(|glyph| glyph.code)
                .collect::<BTreeSet<_>>();
            declared.extend(font.materialization.included_codes.iter().copied());
            for form in engine
                .backend
                .document
                .forms
                .values()
                .filter(|form| form.data.is_some())
            {
                for (font, codes) in &form.codes {
                    if group.members.contains(font) {
                        declared.extend(codes.iter().copied());
                    }
                }
            }
            if !codes.is_subset(&declared) {
                return Err(Error::value(
                    "Preview text uses undeclared font codes; literal subset/width semantics are not verified",
                ));
            }
            let owner=font.materialization.resource.as_ref().ok_or_else(||Error::value(format!("Preview needs the physical outline for font '{}'; standard-14/nonembedded preview fonts are not provisioned",font.name)))?;
            let resource = owner.as_ref();
            #[cfg(feature = "latency-diagnostics")]
            let hash_diag =
                crate::latency_diagnostics::phase("preview_font_identity_hash", &font.name, None);
            let identity = font_wire::source_identity(owner);
            #[cfg(feature = "latency-diagnostics")]
            drop(hash_diag);
            for code in codes {
                let (name, _) = engine.fonts.glyph_name(group.font, code);
                let name =
                    name.ok_or_else(|| Error::value("Preview font has no verified glyph name"))?;
                let path = match resource {
                    FontResource::Type1(resource) => cache.type1(&identity, resource, &name)?,
                    FontResource::Sfnt(resource) => {
                        let gid = resource
                            .glyph_names
                            .iter()
                            .position(|g| g == &name)
                            .ok_or_else(|| {
                                Error::value(format!("Missing preview SFNT glyph {name}"))
                            })?;
                        cache.sfnt(
                            &identity,
                            &resource.data,
                            u16::try_from(gid)
                                .map_err(|_| Error::value("Preview glyph ID overflow"))?,
                        )?
                    }
                    FontResource::Bitmap(_) => {
                        return Err(Error::value(
                            "Retained bitmap font painting is not implemented; PDF export remains available",
                        ));
                    }
                };
                paths.insert(code, path);
                path_ids.insert(code, format!("{identity}:name:{name}"));
                widths.insert(code, font.pdf_char_width(code)? / 1000.0);
            }
            let matrix = match resource {
                FontResource::Type1(resource) => resource.matrix,
                FontResource::Sfnt(resource) => {
                    let face = rustybuzz::ttf_parser::Face::parse(&resource.data, 0)
                        .map_err(|e| Error::value(format!("Invalid preview SFNT: {e:?}")))?;
                    let factor = 1.0 / face.units_per_em() as f64;
                    [factor, 0.0, 0.0, factor, 0.0, 0.0]
                }
                FontResource::Bitmap(_) => unreachable!(),
            };
            (identity, matrix, 1)
        };
        for path in paths.values() {
            path_bytes = path_bytes
                .checked_add(std::mem::size_of_val(path.as_ref()))
                .ok_or_else(|| Error::value("Preview path byte count overflow"))?;
            glyph_count += 1;
        }
        if path_bytes > 32 * 1024 * 1024 || glyph_count > 4096 || fonts.len() >= 256 {
            return Err(Error::value("Retained preview path/font budget exceeded"));
        }
        fonts.insert(
            name,
            PreviewFont {
                source_sha256: identity,
                bytes_per_code,
                matrix,
                widths,
                paths,
                path_ids,
            },
        );
    }
    let mut pages = Vec::with_capacity(engine.pages.len());
    for ((page, operators), _codes) in engine.pages.iter().zip(page_operators).zip(page_codes) {
        let content_sha256 = page_identity(page, &operators, &fonts)?;
        pages.push(PreviewPage {
            source_map: Arc::new(crate::source_map::SourceMap::page(page)),
            provisional_regions: None,
            page: page.page,
            width_bp: sp_to_bp(page.width_sp),
            height_bp: sp_to_bp(page.height_sp),
            magnification: page.magnification,
            operators,
            content_sha256,
        });
    }
    Ok(RetainedPreview {
        image_keys: None,
        schema_version: 2,
        complete: engine.complete,
        fonts,
        images,
        pages,
    })
}

pub(crate) fn page_identity(
    page: &crate::typesetting::display::PageDisplayList,
    operators: &[String],
    fonts: &BTreeMap<String, PreviewFont>,
) -> Result<String> {
    let widths = fonts
        .iter()
        .map(|(n, f)| (n.clone(), f.bytes_per_code))
        .collect();
    let codes = paint_codes(operators, &widths)?;
    let mut dependencies = BTreeMap::new();
    for (name, codes) in &codes {
        let font = &fonts[name];
        let mut glyphs = BTreeMap::new();
        for code in codes {
            let width = font.widths.get(code).ok_or_else(|| {
                Error::value(format!("Missing original preview width for code {code}"))
            })?;
            let path = font.path_ids.get(code).ok_or_else(|| {
                Error::value(format!("Missing original preview path for code {code}"))
            })?;
            glyphs.insert(code, (width, path));
        }
        dependencies.insert(
            name,
            (
                &font.source_sha256,
                font.bytes_per_code,
                font.matrix,
                glyphs,
            ),
        );
    }
    let font_identity =
        serde_json::to_vec(&dependencies).map_err(|e| Error::value(e.to_string()))?;
    let mut hash = Sha256::new();
    hash.update(b"retex-rs2-page-paint-v2\0");
    hash.update(&font_identity);
    hash.update(page.width_sp.to_le_bytes());
    hash.update(page.height_sp.to_le_bytes());
    hash.update(page.magnification.to_le_bytes());
    for operator in operators {
        // Fixed width makes the identity independent of native/Wasm usize.
        hash.update((operator.len() as u64).to_le_bytes());
        hash.update(operator.as_bytes());
    }
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_dependencies_follow_pdf_strings_and_saved_font_state() {
        let widths = BTreeMap::from([("F1".into(), 1), ("F2".into(), 2)]);
        let operators = vec![
            r#"BT /F1 10 Tf [(A\102) -120 <43>] TJ q /F2 12 Tf <0102> Tj Q (D) ' 0 0 (E) " ET"#
                .into(),
        ];
        assert_eq!(
            paint_codes(&operators, &widths).unwrap(),
            BTreeMap::from([
                ("F1".into(), BTreeSet::from([65, 66, 67, 68, 69])),
                ("F2".into(), BTreeSet::from([258])),
            ])
        );
        for source in ["BT (A) Tj ET", "BT /F2 10 Tf <01> Tj ET", "Q"] {
            assert!(paint_codes(&[source.into()], &widths).is_err());
        }
    }

    #[test]
    fn form_code_collection_matches_independent_flattened_stream_scan() {
        let mut engine = crate::Engine::new(crate::core::config::EngineConfig {
            configuration_paths: Some(vec![]),
            ..Default::default()
        })
        .unwrap();
        engine.core.backend.document.decimal_digits = Some(5);
        let mut inner = super::super::forms::Form::new(2, 2, 0, 100, 100, 0);
        inner.data = Some(Arc::from(br#"/F2 12 Tf <0102> Tj"#.as_slice()));
        let mut outer = super::super::forms::Form::new(1, 1, 0, 100, 100, 0);
        outer.data = Some(Arc::from(br#"(B) Tj /Inner Do (C) '"#.as_slice()));
        outer.xobjects.insert("Inner".into(), 2);
        engine.core.backend.document.forms.insert(1, outer);
        engine.core.backend.document.forms.insert(2, inner);
        let widths = BTreeMap::from([("F1".into(), 1), ("F2".into(), 2)]);
        let mut images = super::super::preview_images::ImageCache::default();
        let context = PreviewContext {
            backend: &engine.core.backend,
            fonts: &engine.fonts,
            pages: &[],
            complete: false,
            images: std::cell::RefCell::new(&mut images),
        };
        let mut codes = PaintCodes::new(&widths);
        let operators = form_operators(
            &context,
            br#"0.123456789 cm /F1 10 Tf (A) Tj /Outer Do [(D) -120 (E)] TJ"#,
            &BTreeMap::from([("Outer".into(), 1)]),
            &mut BTreeSet::new(),
            &mut (32 * 1024 * 1024),
            &mut BTreeMap::new(),
            &mut codes,
        )
        .unwrap();
        assert_eq!(codes.codes, paint_codes(&operators, &widths).unwrap());
        assert_eq!(
            codes.codes,
            BTreeMap::from([
                ("F1".into(), BTreeSet::from([65, 66, 67, 68, 69])),
                ("F2".into(), BTreeSet::from([258]))
            ])
        );
        assert!(operators[0].contains("0.123456789"));
        assert!(codes.stack.is_empty());
    }

    #[test]
    fn form_boundaries_ignore_words_inside_pdf_values_and_keep_decimal_bytes() {
        let source =
            br"0.123456789 cm /F#6f#6fm Do (Do \(Do\)) Tj [<446f> (Do)] TJ << /Do (Do) >> BDC % Do
Q";
        let tokens = paint_tokens(source)
            .unwrap()
            .into_iter()
            .map(|span| &source[span])
            .collect::<Vec<_>>();
        assert_eq!(
            tokens,
            vec![
                b"0.123456789".as_slice(),
                b"cm",
                b"/F#6f#6fm",
                b"Do",
                br"(Do \(Do\))",
                b"Tj",
                b"[<446f> (Do)]",
                b"TJ",
                b"<< /Do (Do) >>",
                b"BDC",
                b"Q"
            ]
        );
        for malformed in [b"(X".as_slice(), b"[X", b"<< /X 1", b"]", b"<123"] {
            assert!(paint_tokens(malformed).is_err());
        }
    }
}

/// Split only at lexical PDF instruction boundaries, preserving decimal and
/// string spelling. Used to map a pure native paint interval through a cached
/// page whose unrelated forms/images have already been prepared.
pub(crate) fn instruction_chunks(operators: &[String]) -> Result<Vec<String>> {
    let bytes = crate::pdf::writer::latin1(&operators.join("\n"))?;
    let mut result = Vec::new();
    let mut start = 0;
    for span in paint_tokens(&bytes)? {
        let token = &bytes[span.clone()];
        if matches!(token.first(), Some(b'/' | b'(' | b'<' | b'['))
            || matches!(token, b"true" | b"false" | b"null")
            || std::str::from_utf8(token).is_ok_and(|s| s.parse::<f64>().is_ok())
        {
            continue;
        }
        result.push(
            bytes[start..span.end]
                .iter()
                .map(|&b| char::from(b))
                .collect(),
        );
        start = span.end;
    }
    if bytes[start..].iter().any(|b| !b.is_ascii_whitespace()) {
        return Err(Error::value("Incomplete local paint instruction"));
    }
    Ok(result)
}
