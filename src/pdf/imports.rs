//! Imported resource graph identities outlive readers and preserve cycles.
use crate::core::errors::{Error, Result};
use crate::pdf::{parsing::PdfValue, resources::PDFResources};
use std::collections::BTreeMap;
use std::sync::Arc;
pub type SourceKey = (String, String);
pub type ImportReferences = BTreeMap<(SourceKey, u32, u16), usize>;
pub type ImportObjects = BTreeMap<usize, Option<Arc<[u8]>>>;
pub struct PDFImportGraph<'a> {
    pub resources: &'a mut PDFResources,
    pub objects: &'a mut ImportObjects,
    pub references: &'a mut ImportReferences,
}
impl PDFImportGraph<'_> {
    pub fn clone(
        &mut self,
        value: &PdfValue,
        source: &SourceKey,
        reader: &lopdf::Document,
        replacement: &mut dyn FnMut(
            &mut Self,
            &PdfValue,
            &SourceKey,
            &lopdf::Document,
        ) -> Result<Option<Vec<u8>>>,
    ) -> Result<PdfValue> {
        Ok(match value {
            PdfValue::Reference(id, generation) => {
                let key = (source.clone(), *id, *generation);
                let number = if let Some(number) = self.references.get(&key) {
                    *number
                } else {
                    let number = self.resources.reserve_object();
                    self.references.insert(key, number);
                    self.objects.insert(number, None);
                    let original = from_object(
                        reader
                            .objects
                            .get(&(*id, *generation))
                            .ok_or_else(|| Error::value("Missing imported PDF reference"))?,
                    )?;
                    let data = if let Some(data) = replacement(self, &original, source, reader)? {
                        data
                    } else {
                        self.clone(&original, source, reader, replacement)?
                            .serialize()?
                    };
                    self.objects.insert(number, Some(data.into()));
                    number
                };
                PdfValue::Reference(number as u32, 0)
            }
            PdfValue::Array(items) => PdfValue::Array(
                items
                    .iter()
                    .map(|v| self.clone(v, source, reader, replacement))
                    .collect::<Result<_>>()?,
            ),
            PdfValue::Dictionary(d) => {
                PdfValue::Dictionary(self.clone_dictionary(d, source, reader, replacement)?)
            }
            PdfValue::Stream { dictionary, data } => PdfValue::Stream {
                dictionary: self.clone_dictionary(dictionary, source, reader, replacement)?,
                data: data.clone(),
            },
            v => v.clone(),
        })
    }
    fn clone_dictionary(
        &mut self,
        d: &[(String, PdfValue)],
        source: &SourceKey,
        reader: &lopdf::Document,
        replacement: &mut dyn FnMut(
            &mut Self,
            &PdfValue,
            &SourceKey,
            &lopdf::Document,
        ) -> Result<Option<Vec<u8>>>,
    ) -> Result<Vec<(String, PdfValue)>> {
        d.iter()
            .map(|(k, v)| Ok((k.clone(), self.clone(v, source, reader, replacement)?)))
            .collect()
    }
    pub fn copy(
        &mut self,
        value: &PdfValue,
        key: &SourceKey,
        reader: &lopdf::Document,
    ) -> Result<PdfValue> {
        self.clone(value, key, reader, &mut |_, _, _, _| Ok(None))
    }
}
pub fn from_object(object: &lopdf::Object) -> Result<PdfValue> {
    use lopdf::Object;
    Ok(match object {
        Object::Null => PdfValue::Null,
        Object::Boolean(b) => PdfValue::Boolean(*b),
        Object::Integer(n) => PdfValue::Integer(*n),
        Object::Real(n) => PdfValue::Real(*n as f64),
        Object::Name(s) => PdfValue::Name(s.iter().map(|&c| char::from(c)).collect()),
        Object::String(s, _) => PdfValue::String(s.clone()),
        Object::Array(a) => PdfValue::Array(a.iter().map(from_object).collect::<Result<_>>()?),
        Object::Dictionary(d) => PdfValue::Dictionary(
            d.iter()
                .map(|(k, v)| Ok((k.iter().map(|&c| char::from(c)).collect(), from_object(v)?)))
                .collect::<Result<_>>()?,
        ),
        Object::Reference((n, g)) => PdfValue::Reference(*n, *g),
        Object::Stream(s) => {
            let PdfValue::Dictionary(dictionary) =
                from_object(&Object::Dictionary(s.dict.clone()))?
            else {
                unreachable!()
            };
            PdfValue::Stream {
                dictionary,
                data: s.content.clone(),
            }
        }
    })
}
pub fn resolve<'a>(value: &'a PdfValue, reader: &lopdf::Document) -> Result<PdfValue> {
    match value {
        PdfValue::Reference(n, g) => from_object(
            reader
                .get_object((*n, *g))
                .map_err(|e| Error::value(e.to_string()))?,
        ),
        _ => Ok(value.clone()),
    }
}
pub fn stream_data(value: &PdfValue, reader: &lopdf::Document) -> Result<Vec<u8>> {
    let value = resolve(value, reader)?;
    let PdfValue::Stream { dictionary, data } = value else {
        return Err(Error::value("Expected imported PDF stream"));
    };
    let mut d = lopdf::Dictionary::new();
    for (k, v) in dictionary {
        d.set(k.as_bytes(), to_object(&v)?)
    }
    let stream = lopdf::Stream::new(d, data);
    stream
        .decompressed_content()
        .or_else(|_| {
            if stream.dict.has(b"Filter") {
                Err(lopdf::Error::ObjectNotFound((0, 0)))
            } else {
                Ok(stream.content.clone())
            }
        })
        .map_err(|e| Error::value(e.to_string()))
}
pub fn to_object(value: &PdfValue) -> Result<lopdf::Object> {
    use lopdf::{Dictionary, Object, Stream, StringFormat};
    Ok(match value {
        PdfValue::Null => Object::Null,
        PdfValue::Boolean(b) => Object::Boolean(*b),
        PdfValue::Integer(n) => Object::Integer(*n),
        PdfValue::Real(n) => Object::Real(*n as f32),
        PdfValue::Name(s) => Object::Name(crate::pdf::writer::latin1(s)?),
        PdfValue::String(s) => Object::String(s.clone(), StringFormat::Literal),
        PdfValue::Reference(n, g) => Object::Reference((*n, *g)),
        PdfValue::Array(a) => Object::Array(a.iter().map(to_object).collect::<Result<_>>()?),
        PdfValue::Dictionary(d) | PdfValue::Stream { dictionary: d, .. } => {
            let mut dict = Dictionary::new();
            for (k, v) in d {
                dict.set(k.as_bytes(), to_object(v)?)
            }
            if let PdfValue::Stream { data, .. } = value {
                Object::Stream(Stream::new(dict, data.clone()))
            } else {
                Object::Dictionary(dict)
            }
        }
    })
}
