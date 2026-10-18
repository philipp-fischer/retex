//! Owned PDF objects, zlib streams, object streams and cross references.
//! Direct translation of `pdf/writer.py`; reservation order remains observable.
use crate::core::errors::{Error, Result};
use flate2::{Compression, write::ZlibEncoder};
use regex::{Regex, bytes::Regex as ByteRegex};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PdfObject {
    pub body: Vec<u8>,
    pub compression_priority: i64,
}
pub const OBJECTS_PER_STREAM: usize = 100;
pub struct PdfWriter {
    pub objects: Vec<Option<PdfObject>>,
    pub compress_level: u32,
    pub object_level: i64,
    pub object_stream_numbers: Vec<usize>,
}
impl PdfWriter {
    pub fn new(reserved_objects: usize, compress_level: i64, object_level: i64) -> Self {
        Self {
            objects: vec![None; reserved_objects],
            compress_level: compress_level.clamp(0, 9) as u32,
            object_level: object_level.clamp(0, 3),
            object_stream_numbers: Vec::new(),
        }
    }
    pub fn reserve_object(&mut self) -> usize {
        self.objects.push(None);
        self.objects.len()
    }
    pub fn add_object(
        &mut self,
        body: impl Into<Vec<u8>>,
        number: Option<usize>,
        mut priority: i64,
    ) -> Result<usize> {
        let number = number.unwrap_or_else(|| self.reserve_object());
        let body = body.into();
        if ByteRegex::new(r">>\s*stream[\r\n]")
            .expect("PDF stream syntax")
            .is_match(&body)
        {
            priority = 0;
        }
        let slot = number
            .checked_sub(1)
            .and_then(|n| self.objects.get_mut(n))
            .ok_or_else(|| Error::value("PDF object number is not reserved"))?;
        *slot = Some(PdfObject {
            body,
            compression_priority: priority,
        });
        Ok(number)
    }
    pub fn add_text_object(
        &mut self,
        content: &str,
        number: Option<usize>,
        priority: i64,
    ) -> Result<usize> {
        self.add_object(latin1(content)?, number, priority)
    }
    pub fn add_stream_object(
        &mut self,
        data: &[u8],
        dictionary: &str,
        number: Option<usize>,
        compress_level: Option<i64>,
    ) -> Result<usize> {
        let level = compress_level.map_or(self.compress_level, |n| n.clamp(0, 9) as u32);
        let mut dictionary = dictionary.to_string();
        let mut data = data.to_vec();
        if level != 0
            && !Regex::new(r"/Filter\b")
                .expect("PDF filter syntax")
                .is_match(&dictionary)
        {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(level));
            encoder.write_all(&data)?;
            data = encoder.finish()?;
            dictionary.push_str(" /Filter /FlateDecode");
        }
        let mut body = latin1(&format!(
            "<< /Length {} {} >>\nstream\n",
            data.len(),
            dictionary
        ))?;
        body.extend(data);
        body.extend(b"\nendstream");
        self.add_object(body, number, 0)
    }
    pub fn object_stream(&mut self) -> Result<BTreeMap<usize, (usize, usize)>> {
        let selected: Vec<_> = self
            .objects
            .iter()
            .enumerate()
            .filter_map(|(n, object)| {
                object
                    .as_ref()
                    .filter(|object| {
                        0 < object.compression_priority
                            && object.compression_priority <= self.object_level
                    })
                    .map(|object| (n + 1, object.clone()))
            })
            .collect();
        let mut entries = BTreeMap::new();
        for (batch, group) in selected.chunks(OBJECTS_PER_STREAM).enumerate() {
            let number = self
                .object_stream_numbers
                .get(batch)
                .copied()
                .unwrap_or_else(|| self.reserve_object());
            let mut header = Vec::new();
            let mut bodies = Vec::new();
            let mut offset = 0;
            for (index, (identity, object)) in group.iter().enumerate() {
                header.push(format!("{identity} {offset}"));
                bodies.extend_from_slice(&object.body);
                bodies.push(b'\n');
                offset += object.body.len() + 1;
                entries.insert(*identity, (number, index));
            }
            let mut prefix = (header.join(" ") + "\n").into_bytes();
            let first = prefix.len();
            prefix.extend(bodies);
            self.add_stream_object(
                &prefix,
                &format!("/Type /ObjStm /N {} /First {first}", group.len()),
                Some(number),
                None,
            )?;
        }
        Ok(entries)
    }
    pub fn write_pdf(
        &mut self,
        filename: impl AsRef<Path>,
        catalog_num: usize,
        version: (u8, u8),
        trailer: &str,
        free_objects: &[usize],
    ) -> Result<()> {
        // Reservation errors are diagnosed before opening the destination.
        self.validate_reservations(free_objects)?;
        let bytes = self.serialize(catalog_num, version, trailer, free_objects)?;
        let mut output = crate::resources::host::open_output(filename, false)?;
        output.write_all(&bytes)?;
        Ok(())
    }
    fn validate_reservations(&self, free_objects: &[usize]) -> Result<()> {
        let free: HashSet<_> = free_objects.iter().copied().collect();
        if self
            .objects
            .iter()
            .enumerate()
            .any(|(index, obj)| obj.is_none() && !free.contains(&(index + 1)))
        {
            return Err(Error::value(
                "An allocated PDF object has not been serialized",
            ));
        }
        Ok(())
    }
    pub fn serialize(
        &mut self,
        catalog_num: usize,
        version: (u8, u8),
        trailer: &str,
        free_objects: &[usize],
    ) -> Result<Vec<u8>> {
        self.validate_reservations(free_objects)?;
        let packed = if self.object_level != 0 && version >= (1, 5) {
            self.object_stream()?
        } else {
            BTreeMap::new()
        };
        let xref_number = (!packed.is_empty()).then(|| self.reserve_object());
        let mut output = format!("%PDF-{}.{}\n", version.0, version.1).into_bytes();
        output.extend(b"%\xe2\xe3\xcf\xd3\n");
        let mut offsets = BTreeMap::new();
        for (index, object) in self.objects.iter().enumerate() {
            let number = index + 1;
            if let Some(object) = object {
                if !packed.contains_key(&number) {
                    offsets.insert(number, output.len());
                    write_object(&mut output, number, object);
                }
            }
        }
        let xref_offset = output.len();
        let size = self.objects.len() + 1;
        let mut free = vec![0];
        free.extend((1..size).filter(|n| {
            !offsets.contains_key(n) && !packed.contains_key(n) && Some(*n) != xref_number
        }));
        let following: BTreeMap<_, _> = free
            .iter()
            .copied()
            .zip(free.iter().copied().skip(1).chain(std::iter::once(0)))
            .collect();
        if let Some(xref_number) = xref_number {
            offsets.insert(xref_number, xref_offset);
            let mut entries = Vec::new();
            for number in 0..size {
                let (kind, offset, generation) = if let Some(&next) = following.get(&number) {
                    (0_u8, next as u64, if number == 0 { 65535_u32 } else { 0 })
                } else if let Some(&(stream, index)) = packed.get(&number) {
                    (2, stream as u64, index as u32)
                } else {
                    (1, offsets[&number] as u64, 0)
                };
                entries.push(kind);
                entries.extend(offset.to_be_bytes());
                entries.extend(generation.to_be_bytes());
            }
            self.add_stream_object(
                &entries,
                &format!("/Type /XRef /Size {size} /W [1 8 4] /Root {catalog_num} 0 R {trailer}"),
                Some(xref_number),
                None,
            )?;
            write_object(
                &mut output,
                xref_number,
                self.objects[xref_number - 1]
                    .as_ref()
                    .expect("serialized xref"),
            );
        } else {
            output.extend(format!("xref\n0 {size}\n").as_bytes());
            for number in 0..size {
                if let Some(&next) = following.get(&number) {
                    output.extend(
                        format!("{next:010} {:05} f \n", if number == 0 { 65535 } else { 0 })
                            .as_bytes(),
                    );
                } else {
                    output.extend(format!("{:010} 00000 n \n", offsets[&number]).as_bytes());
                }
            }
            output.extend(latin1(&format!(
                "trailer\n<< /Size {size} /Root {catalog_num} 0 R {trailer} >>\n"
            ))?);
        }
        output.extend(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        Ok(output)
    }
}
fn write_object(output: &mut Vec<u8>, number: usize, object: &PdfObject) {
    output.extend(format!("{number} 0 obj\n").as_bytes());
    output.extend(&object.body);
    output.extend(b"\nendobj\n");
}
pub fn latin1(text: &str) -> Result<Vec<u8>> {
    text.chars()
        .map(|c| u8::try_from(c as u32).map_err(|_| Error::value("PDF syntax is not Latin-1")))
        .collect()
}
