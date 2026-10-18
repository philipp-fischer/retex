//! PDF byte-syntax escaping (`pdf/syntax.py`).
use crate::core::errors::{Error, Result};
pub fn pdf_name(name: &str) -> Result<String> {
    let mut result = String::new();
    for character in name.chars() {
        let c = character as u32;
        if c > 255 {
            return Err(Error::value("PDF name is not Latin-1"));
        }
        if (33..=126).contains(&c) && !"#%()/<>[]{}".contains(character) {
            result.push(character);
        } else {
            result.push_str(&format!("#{c:02X}"));
        }
    }
    Ok(result)
}
pub fn character_string(code: u32) -> String {
    if code < 32 || code >= 127 {
        format!("\\{code:03o}")
    } else {
        let character = char::from(code as u8);
        if "\\()".contains(character) {
            format!("\\{character}")
        } else {
            character.to_string()
        }
    }
}
