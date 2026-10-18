//! Expandable PDF file/string/regex queries (`pdf_commands.py`).
use super::posix_regex::{MatchingBackend, matching_backend};
use super::services::ListServices;
use crate::core::characters::LanguageProfile;
use crate::core::constants::Constants as C;
use crate::core::enums::NodeKind;
use crate::core::errors::{Error, Result};
use crate::core::units::print_scaled;
use crate::pdf::graphics::ColorStack;
use crate::pdf::image_records::ImageUse;
use crate::typesetting::protrusion::margin_query;
use md5::{Digest, Md5};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
pub const CONVERSIONS: [&str; 25] = [
    "pdfstrcmp",
    "pdfescapestring",
    "pdfescapename",
    "pdfescapehex",
    "pdfunescapehex",
    "pdfmdfivesum",
    "pdffilesize",
    "pdffiledump",
    "pdffilemoddate",
    "pdfcreationdate",
    "pdfmatch",
    "pdflastmatch",
    "pdfuniformdeviate",
    "pdfnormaldeviate",
    "pdfinsertht",
    "pdftexbanner",
    "pdffontname",
    "pdffontobjnum",
    "pdffontsize",
    "pdfpageref",
    "pdfcolorstackinit",
    "pdfxformname",
    "pdfximagebbox",
    "leftmarginkern",
    "rightmarginkern",
];
pub fn pdf_date(timestamp: SystemTime) -> String {
    pdf_local_date(crate::resources::host::local_date(timestamp))
}
fn pdf_timestamp(timestamp: chrono::DateTime<chrono::Utc>) -> String {
    pdf_local_date(crate::resources::host::local_timestamp(timestamp))
}
fn pdf_local_date(date: chrono::DateTime<chrono::FixedOffset>) -> String {
    let offset = date.format("%z").to_string();
    format!(
        "D:{}{}",
        date.format("%Y%m%d%H%M%S"),
        if offset == "+0000" {
            "Z".into()
        } else {
            format!("{}'{}'", &offset[..3], &offset[3..])
        }
    )
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}
pub fn text_bytes(text: &str, unicode: bool) -> Result<Vec<u8>> {
    if unicode {
        Ok(text.as_bytes().to_vec())
    } else {
        text.chars()
            .map(|character| {
                u8::try_from(character as u32)
                    .map_err(|_| Error::value("Non-octet PDF conversion operand"))
            })
            .collect()
    }
}
pub trait PdfUtilityServices: ListServices {
    fn exact_repro(&self) -> bool;
    fn define_unicode(&mut self, glyph: &str, value: &str);
    fn image(&self, number: usize) -> Option<&ImageUse>;
    fn form_ordinal(&self, number: usize) -> Option<usize>;
    fn add_color_stack(&mut self, stack: ColorStack) -> usize;
    fn page_reference(&mut self, page: i64) -> Result<usize>;
    fn font_reference(&mut self, font: usize) -> Result<(usize, usize)>;
    fn uniform_deviate(&mut self, extent: i64) -> String;
    fn normal_deviate(&mut self) -> Result<i64>;
    fn insertion_size(&self, register: i64) -> Option<i64>;
}
#[derive(Clone)]
pub struct PDFUtilities {
    pub creation_time: SystemTime,
    pub regex: Option<MatchingBackend>,
    pub last_matches: Vec<(i64, i64)>,
    pub matched_text: String,
    pub match_succeeded: bool,
}
impl Default for PDFUtilities {
    fn default() -> Self {
        Self {
            creation_time: crate::resources::host::now(),
            regex: None,
            last_matches: Vec::new(),
            matched_text: String::new(),
            match_succeeded: false,
        }
    }
}
impl PDFUtilities {
    pub fn define_unicode(services: &mut impl PdfUtilityServices) -> Result<()> {
        let glyph = services.text()?;
        let value = services.text()?;
        services.define_unicode(&glyph, &value);
        Ok(())
    }
    pub fn file(services: &mut impl PdfUtilityServices) -> Result<Option<PathBuf>> {
        let directory = services
            .input()
            .cursor
            .name
            .as_ref()
            .and_then(|name| Path::new(name).parent())
            .map(Path::to_path_buf);
        let text = services.text()?;
        let unicode = services.input().profile == LanguageProfile::Unicode;
        let name = if unicode {
            text.into()
        } else {
            crate::resources::search::filesystem_name(&text)?
        };
        services.input_mut().resources.resolve(
            name.to_str()
                .ok_or_else(|| Error::value("Non-UTF-8 file query name"))?,
            directory.as_deref(),
            ".tex",
            true,
            true,
        )
    }
    pub fn convert(
        &mut self,
        services: &mut impl PdfUtilityServices,
        name: &str,
    ) -> Result<String> {
        let unicode = services.input().profile == LanguageProfile::Unicode;
        match name {
            "leftmarginkern" | "rightmarginkern" => {
                let register = services.scan_register(None)?;
                let pointer = services
                    .env()
                    .box_registers
                    .get(&register)
                    .copied()
                    .unwrap_or(C::NULL);
                if pointer == C::NULL
                    || services.builder().node(pointer)?.node_type != NodeKind::Hlist
                {
                    return Ok("0pt".into());
                }
                let head = services.builder().node(pointer)?.list_ptr;
                return Ok(format!(
                    "{}pt",
                    print_scaled(margin_query(
                        services.builder(),
                        head,
                        name == "rightmarginkern"
                    )?)
                ));
            }
            "pdfximagebbox" => {
                let number = services.scan_int()?;
                let selector = services.scan_int()?;
                let image = services.image(number as usize).ok_or_else(|| {
                    Error::value("Cannot find referenced PDF image")
                        .with_issue(crate::core::recovery::Issue::PdfOperation)
                })?;
                if !(1..=4).contains(&selector) {
                    return Err(Error::value(
                        "PDF image bounding-box selector must be in 1..4",
                    ));
                }
                let value = if image.format == "PDF" {
                    image.bbox_sp[selector as usize - 1]
                } else {
                    0
                };
                return Ok(format!("{}pt", print_scaled(value)));
            }
            "pdfxformname" => {
                let number = services.scan_int()?;
                return Ok(services
                    .form_ordinal(number as usize)
                    .ok_or_else(|| {
                        Error::value("Cannot find referenced PDF form")
                            .with_issue(crate::core::recovery::Issue::PdfOperation)
                    })?
                    .to_string());
            }
            "pdfcolorstackinit" => {
                let page_start = services.scan_keyword("page")?;
                let mode = if services.scan_keyword("direct")? {
                    "direct"
                } else if services.scan_keyword("page")? {
                    "page"
                } else {
                    "origin"
                };
                let text = services.text()?;
                return Ok(services
                    .add_color_stack(ColorStack::new(text, mode.into(), page_start))
                    .to_string());
            }
            "pdfpageref" => {
                let page = services.scan_int()?;
                return Ok(services.page_reference(page)?.to_string());
            }
            "pdffontname" | "pdffontobjnum" | "pdffontsize" => {
                let number = services.scan_font_ident()?;
                if number == 0 {
                    return Err(Error::value("Invalid PDF font identifier"));
                }
                if name == "pdffontsize" {
                    return Ok(format!(
                        "{}pt",
                        print_scaled(services.fonts().items[number].size)
                    ));
                }
                let (font, object) = services.font_reference(number)?;
                return Ok(if name == "pdffontname" { font } else { object }.to_string());
            }
            "pdftexbanner" => return Ok(C::PDFTEX_FULL_BANNER.into()),
            "pdfuniformdeviate" => {
                let extent = services.scan_int()?;
                return Ok(services.uniform_deviate(extent));
            }
            "pdfnormaldeviate" => return Ok(services.normal_deviate()?.to_string()),
            "pdfinsertht" => {
                let register = services.scan_register(None)?;
                return Ok(format!(
                    "{}pt",
                    services
                        .insertion_size(register)
                        .map(print_scaled)
                        .unwrap_or_else(|| "0".into())
                ));
            }
            "pdfmatch" => {
                if self.regex.is_none() {
                    self.regex = Some(matching_backend(unicode, services.exact_repro())?)
                }
                let ignore_case = services.scan_keyword("icase")?;
                let count = if services.scan_keyword("subcount")? {
                    services.scan_int()?
                } else {
                    -1
                };
                let count = if count < 0 { 10 } else { count as usize };
                let pattern = services.text()?;
                let text = services.text()?;
                let result = self
                    .regex
                    .unwrap()
                    .matches(&pattern, &text, count, ignore_case)?;
                if let Some(error) = result.compile_error {
                    services.warning(&format!("\\pdfmatch: {error}"));
                    return Ok("-1".into());
                }
                self.last_matches = result.spans;
                self.matched_text = text.split('\0').next().unwrap_or("").into();
                self.match_succeeded = result.matched.unwrap_or(false);
                return Ok(if self.match_succeeded { "1" } else { "0" }.into());
            }
            "pdflastmatch" => {
                let index = services.scan_int()?;
                if index < 0 {
                    return Err(Error::value("Bad match number"));
                }
                if self.match_succeeded {
                    if let Some(&(start, end)) = self.last_matches.get(index as usize) {
                        if start >= 0 && end >= start {
                            let value = self
                                .matched_text
                                .chars()
                                .skip(start as usize)
                                .take((end - start) as usize)
                                .collect::<String>();
                            return Ok(format!("{start}->{value}"));
                        }
                    }
                }
                return Ok("-1->".into());
            }
            "pdfstrcmp" => {
                let left = services.text()?;
                let right = services.text()?;
                let relation = left
                    .split('\0')
                    .next()
                    .unwrap()
                    .cmp(right.split('\0').next().unwrap());
                return Ok(match relation {
                    std::cmp::Ordering::Less => "-1",
                    std::cmp::Ordering::Equal => "0",
                    std::cmp::Ordering::Greater => "1",
                }
                .into());
            }
            "pdfmdfivesum" => {
                let bytes = if services.scan_keyword("file")? {
                    let Some(path) = Self::file(services)? else {
                        return Ok(String::new());
                    };
                    crate::resources::host::read(path)?
                } else {
                    text_bytes(&services.text()?, unicode)?
                };
                return Ok(hex(&Md5::digest(bytes)));
            }
            "pdffilesize" => {
                return Ok(if let Some(path) = Self::file(services)? {
                    crate::resources::host::metadata(&path)?.len().to_string()
                } else {
                    String::new()
                });
            }
            "pdffiledump" => {
                let offset = if services.scan_keyword("offset")? {
                    services.scan_int()?
                } else {
                    0
                };
                let length = if services.scan_keyword("length")? {
                    services.scan_int()?
                } else {
                    0
                };
                if offset < 0 || length < 0 {
                    return Err(Error::value("Bad file offset or dump length"));
                }
                let Some(path) = Self::file(services)? else {
                    return Ok(String::new());
                };
                let mut file = crate::resources::host::open_read(path)?;
                file.seek(SeekFrom::Start(offset as u64))?;
                let mut bytes = Vec::new();
                file.take(length as u64).read_to_end(&mut bytes)?;
                return Ok(hex(&bytes));
            }
            "pdffilemoddate" => {
                crate::resources::host::observe_time();
                return Ok(if let Some(path) = Self::file(services)? {
                    pdf_timestamp(crate::resources::host::metadata(&path)?.modified_timestamp()?)
                } else {
                    String::new()
                });
            }
            "pdfcreationdate" => {
                crate::resources::host::observe_time();
                return Ok(pdf_date(self.creation_time));
            }
            _ => {}
        }
        let value = services.text()?;
        if name == "pdfescapehex" {
            return Ok(hex(&text_bytes(&value, unicode)?));
        }
        if name == "pdfunescapehex" {
            let mut digits = value
                .chars()
                .filter(|character| character.is_ascii_hexdigit())
                .collect::<String>();
            if digits.len() % 2 != 0 {
                digits.push('0')
            }
            let bytes = digits
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect::<Vec<_>>();
            return if unicode {
                String::from_utf8(bytes).map_err(|error| Error::value(error.to_string()))
            } else {
                Ok(bytes.into_iter().map(char::from).collect())
            };
        }
        let bytes = text_bytes(&value, unicode)?;
        if name == "pdfescapename" {
            return Ok(bytes
                .into_iter()
                .filter(|&byte| byte != 0)
                .map(|byte| {
                    if (33..=126).contains(&byte) && !b"#%()/<>[]{}".contains(&byte) {
                        char::from(byte).to_string()
                    } else {
                        format!("#{byte:02X}")
                    }
                })
                .collect());
        }
        if name == "pdfescapestring" {
            return Ok(bytes
                .into_iter()
                .map(|byte| {
                    if !(33..=126).contains(&byte) {
                        format!("\\{byte:03o}")
                    } else if b"()\\".contains(&byte) {
                        format!("\\{}", char::from(byte))
                    } else {
                        char::from(byte).to_string()
                    }
                })
                .collect());
        }
        Err(Error::value("Unknown PDF conversion"))
    }
}
