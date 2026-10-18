//! Wasm lacks Serde's Unix/Windows OsString implementations. Preserve the
//! native wire tags and reject names the current browser path type cannot hold.
//! Byte-path catalog integration is still required for non-UTF-8 resource names.
use serde::{Deserialize, Deserializer, Serializer, de::Error as _};
use std::ffi::OsString;

#[derive(Deserialize)]
enum NativeName {
    Unix(Vec<u8>),
    Windows(Vec<u16>),
}

pub fn serialize<S: Serializer>(name: &OsString, serializer: S) -> Result<S::Ok, S::Error> {
    let name = name.to_str().ok_or_else(|| {
        serde::ser::Error::custom("Browser resource names currently require valid Unicode")
    })?;
    serializer.serialize_newtype_variant("OsString", 0, "Unix", name.as_bytes())
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<OsString, D::Error> {
    match NativeName::deserialize(deserializer)? {
        NativeName::Unix(bytes) => String::from_utf8(bytes)
            .map(OsString::from)
            .map_err(|_| D::Error::custom("Browser cannot restore a non-UTF-8 native filename")),
        NativeName::Windows(units) => String::from_utf16(&units)
            .map(OsString::from)
            .map_err(|_| D::Error::custom("Browser cannot restore an unpaired UTF-16 filename")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Debug, Serialize, Deserialize)]
    struct WireName(#[serde(with = "super")] OsString);

    #[test]
    #[cfg(unix)]
    fn preserves_native_json_and_binary_tags() {
        let name = OsString::from("résumé😀.aux");
        assert_eq!(
            serde_json::to_value(WireName(name.clone())).unwrap(),
            serde_json::to_value(&name).unwrap()
        );
        let bytes = bincode::serialize(&name).unwrap();
        assert_eq!(bincode::serialize(&WireName(name.clone())).unwrap(), bytes);
        assert_eq!(bincode::deserialize::<WireName>(&bytes).unwrap().0, name);
    }

    #[test]
    #[cfg(unix)]
    fn rejects_unrepresentable_bytes_explicitly() {
        use std::os::unix::ffi::OsStringExt;
        let bytes = bincode::serialize(&OsString::from_vec(vec![0xff])).unwrap();
        assert!(
            bincode::deserialize::<WireName>(&bytes)
                .unwrap_err()
                .to_string()
                .contains("non-UTF-8")
        );
    }
}
