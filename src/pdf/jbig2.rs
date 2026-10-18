//! JBIG2 framing and transitive global segment dependencies (`pdf/jbig2.py`).
use crate::core::errors::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
pub const SIGNATURE: &[u8] = b"\x97JB2\r\n\x1a\n";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SegmentType {
    Page = 48,
    EndPage = 49,
    EndStripe = 50,
    EndFile = 51,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Segment {
    pub number: u32,
    pub kind: u8,
    pub page: u32,
    pub references: Vec<u32>,
    pub before_page: Vec<u8>,
    pub page_bytes: usize,
    pub payload: Vec<u8>,
}
impl Segment {
    pub fn encoded(&self) -> Vec<u8> {
        let mut result = self.before_page.clone();
        let page = u32::from(self.page != 0).to_be_bytes();
        result.extend(&page[4 - self.page_bytes..]);
        result.extend((self.payload.len() as u32).to_be_bytes());
        result.extend(&self.payload);
        result
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub width: u32,
    pub height: u32,
    pub dpi: (u32, u32),
    pub segments: Vec<Segment>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Jbig2 {
    pub pages: BTreeMap<u32, Page>,
    pub globals: BTreeMap<u32, Segment>,
    pub global_order: Vec<u32>,
    pub declared_pages: u32,
}
impl Jbig2 {
    pub fn dependencies(&self, page: u32) -> Result<BTreeSet<u32>> {
        let page = self.pages.get(&page).ok_or_else(|| {
            Error::value("Unknown JBIG2 page").with_issue(crate::core::recovery::Issue::Image)
        })?;
        let mut pending: Vec<_> = page
            .segments
            .iter()
            .flat_map(|segment| segment.references.iter().copied())
            .collect();
        let mut needed = BTreeSet::new();
        while let Some(number) = pending.pop() {
            if needed.contains(&number) {
                continue;
            }
            if let Some(segment) = self.globals.get(&number) {
                needed.insert(number);
                pending.extend(&segment.references);
            }
        }
        Ok(needed)
    }
    pub fn global_data(&self, needed: &BTreeSet<u32>) -> Vec<u8> {
        self.global_order
            .iter()
            .filter(|number| needed.contains(number))
            .flat_map(|number| self.globals[number].encoded())
            .collect()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GlobalUse {
    pub number: usize,
    pub source: Arc<[u8]>,
    pub needed: BTreeSet<u32>,
}
struct Reader<'a> {
    source: &'a [u8],
    position: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, size: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(size)
            .filter(|&end| end <= self.source.len())
            .ok_or_else(|| {
                Error::value(format!("Truncated JBIG2 field at byte {}", self.position))
            })?;
        let value = &self.source[self.position..end];
        self.position = end;
        Ok(value)
    }
    fn integer(&mut self, size: usize) -> Result<u32> {
        let mut value = 0;
        for &byte in self.take(size)? {
            value = (value << 8) | byte as u32;
        }
        Ok(value)
    }
}
pub fn read(source: &[u8]) -> Result<Jbig2> {
    let mut reader = Reader {
        source,
        position: 0,
    };
    if reader.take(SIGNATURE.len())? != SIGNATURE {
        return Err(
            Error::value("Invalid JBIG2 signature").with_issue(crate::core::recovery::Issue::Image)
        );
    }
    let flags = reader.integer(1)?;
    let sequential = flags & 1 != 0;
    let declared_pages = if flags & 2 != 0 {
        0
    } else {
        reader.integer(4)?
    };
    let mut headers = Vec::new();
    let mut segments = Vec::new();
    while reader.position < source.len() {
        let start = reader.position;
        let number = reader.integer(4)?;
        let flags = reader.integer(1)? as u8;
        let kind = flags & 0x3f;
        if kind == 51 && sequential {
            break;
        }
        let first_count = reader.integer(1)?;
        let mut count = first_count >> 5;
        if count == 7 {
            count = (first_count << 24 | reader.integer(3)?) & 0x1fffffff;
            reader.take((count as usize + 8) / 8)?;
        } else if count == 5 || count == 6 {
            return Err(Error::value("Invalid JBIG2 referred-segment count")
                .with_issue(crate::core::recovery::Issue::Image));
        }
        let reference_bytes = if number <= 256 {
            1
        } else if number <= 65536 {
            2
        } else {
            4
        };
        let references = (0..count)
            .map(|_| reader.integer(reference_bytes))
            .collect::<Result<Vec<_>>>()?;
        let page_start = reader.position;
        let page_bytes = if flags & 0x40 != 0 { 4 } else { 1 };
        let page = reader.integer(page_bytes)?;
        let size = reader.integer(4)?;
        if size == u32::MAX {
            return Err(
                Error::value("JBIG2 inclusion requires a known segment length")
                    .with_issue(crate::core::recovery::Issue::Image),
            );
        }
        let mut segment = Segment {
            number,
            kind,
            page,
            references,
            before_page: source[start..page_start].to_vec(),
            page_bytes,
            payload: Vec::new(),
        };
        if sequential {
            segment.payload = reader.take(size as usize)?.to_vec();
            segments.push(segment);
        } else {
            headers.push((segment, size));
        }
        if kind == 51 {
            break;
        }
    }
    if !sequential {
        for (mut segment, size) in headers {
            segment.payload = reader.take(size as usize)?.to_vec();
            segments.push(segment);
        }
    }
    let mut page_segments: BTreeMap<u32, Vec<Segment>> = BTreeMap::new();
    let mut information = BTreeMap::new();
    let mut globals = BTreeMap::new();
    let mut global_order = Vec::new();
    for segment in segments {
        if segment.kind == 51 {
            continue;
        }
        if segment.page == 0 {
            if !globals.contains_key(&segment.number) {
                global_order.push(segment.number);
            }
            globals.insert(segment.number, segment);
            continue;
        }
        if segment.kind == 48 {
            if segment.payload.len() < 19 {
                return Err(Error::value("Truncated JBIG2 page information")
                    .with_issue(crate::core::recovery::Issue::Image));
            }
            let mut data = Reader {
                source: &segment.payload,
                position: 0,
            };
            let (width, height, xr, yr) = (
                data.integer(4)?,
                data.integer(4)?,
                data.integer(4)?,
                data.integer(4)?,
            );
            data.take(1)?;
            let striping = data.integer(2)?;
            if width == 0 || height == 0 {
                return Err(Error::value("JBIG2 image dimensions must be positive")
                    .with_issue(crate::core::recovery::Issue::Image));
            }
            information.insert(segment.page, [width, height, xr, yr, striping]);
        } else if segment.kind == 50 {
            if segment.payload.len() != 4 || !information.contains_key(&segment.page) {
                return Err(Error::value("Invalid JBIG2 stripe information")
                    .with_issue(crate::core::recovery::Issue::Image));
            }
            let information = information.get_mut(&segment.page).expect("page metadata");
            if information[4] & 0x8000 != 0 {
                information[1] =
                    u32::from_be_bytes(segment.payload[..4].try_into().expect("stripe value"));
            }
        }
        if segment.kind != 49 {
            page_segments.entry(segment.page).or_default().push(segment);
        }
    }
    if page_segments.keys().copied().collect::<BTreeSet<_>>()
        != information.keys().copied().collect()
    {
        return Err(Error::value("JBIG2 page information is missing")
            .with_issue(crate::core::recovery::Issue::Image));
    }
    let mut pages = BTreeMap::new();
    for (number, [width, height, xr, yr, _]) in information {
        pages.insert(
            number,
            Page {
                width,
                height,
                dpi: (
                    (xr as f64 * 0.0254 + 0.5) as u32,
                    (yr as f64 * 0.0254 + 0.5) as u32,
                ),
                segments: page_segments.remove(&number).expect("page segments"),
            },
        );
    }
    if pages.is_empty() {
        return Err(Error::value("JBIG2 file has no image pages")
            .with_issue(crate::core::recovery::Issue::Image));
    }
    Ok(Jbig2 {
        pages,
        globals,
        global_order,
        declared_pages,
    })
}
