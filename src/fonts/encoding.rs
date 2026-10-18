//! Declarative PostScript byte encoding vectors (`fonts/encoding.py`).
use super::formats::type1::{PS_NAME, ps_tokens};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{cell::RefCell, collections::BTreeMap, path::Path};
thread_local! { static ENCODINGS: RefCell<BTreeMap<(bool,[u8;32]),EncFileParser>> = RefCell::new(BTreeMap::new()); }

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EncFileParser {
    pub encoding_name: Option<String>,
    pub glyph_names: Vec<String>,
}
impl EncFileParser {
    pub fn parse(&mut self, path: impl AsRef<Path>, strict: bool) -> bool {
        let Ok(bytes) = crate::resources::host::read(path) else {
            return false;
        };
        let key = (strict, Sha256::digest(&bytes).into());
        if let Some(cached) = ENCODINGS.with(|cache| cache.borrow().get(&key).cloned()) {
            *self = cached;
            return true;
        }
        let content = bytes.iter().map(|&b| char::from(b)).collect::<String>();
        let valid = self.parse_text(&content, strict);
        if valid {
            let size = self
                .glyph_names
                .iter()
                .map(|n| n.len() + std::mem::size_of::<String>())
                .sum::<usize>()
                + self.encoding_name.as_ref().map_or(0, |n| n.len());
            if size <= 32 * 1024 {
                ENCODINGS.with(|cache| {
                    let mut cache = cache.borrow_mut();
                    if cache.len() >= 128 {
                        cache.pop_first();
                    }
                    cache.insert(key, self.clone());
                });
            }
        }
        valid
    }

    pub fn parse_text(&mut self, content: &str, strict: bool) -> bool {
        let comments = Regex::new(r"(?m)%.*$").expect("PostScript comment grammar");
        let content = comments.replace_all(content, "");
        let pattern =
            Regex::new(&format!(r"(?s)/({PS_NAME})\s*\[(.*?)\]")).expect("encoding grammar");
        let Some(matched) = pattern.captures(&content) else {
            return false;
        };
        self.encoding_name = Some(matched[1].into());
        let valid = Regex::new(&format!(r"^/{PS_NAME}$")).expect("PostScript glyph name");
        let mut glyphs = Vec::new();
        for word in ps_tokens(&matched[2]) {
            if !valid.is_match(&word) {
                return false;
            }
            glyphs.push(word[1..].to_string());
        }
        if glyphs.len() > 256 || (strict && glyphs.len() != 256) {
            return false;
        }
        glyphs.resize(256, ".notdef".into());
        self.glyph_names = glyphs;
        true
    }
    pub fn get_glyph_name(&self, code: i64) -> &str {
        usize::try_from(code)
            .ok()
            .and_then(|i| self.glyph_names.get(i))
            .map_or(".notdef", String::as_str)
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    #[test]
    fn cached_encoding_reads_current_bytes_matches_strictness_and_detaches_mutable_vectors() {
        let file =
            std::env::temp_dir().join(format!("retex-encoding-cache-{}.enc", std::process::id()));
        std::fs::write(&file, "/Fixture [ /A /B ] def").unwrap();
        let mut a = EncFileParser::default();
        assert!(a.parse(&file, false));
        a.glyph_names[0] = "changed".into();
        let mut b = EncFileParser::default();
        assert!(b.parse(&file, false));
        assert_eq!(b.get_glyph_name(0), "A");
        assert!(!b.parse(&file, true));
        std::fs::write(&file, "/Replacement [ /C /D ] def").unwrap();
        assert!(b.parse(&file, false));
        assert_eq!(b.encoding_name.as_deref(), Some("Replacement"));
        assert_eq!(b.get_glyph_name(0), "C");
        for i in 0..140 {
            std::fs::write(&file, format!("/Encoding{i} [ /C ] def")).unwrap();
            assert!(b.parse(&file, false));
        }
        assert!(ENCODINGS.with(|cache| cache.borrow().len()) <= 128);
        std::fs::remove_file(file).unwrap();
    }
}
