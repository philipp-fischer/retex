//! Persistable image data independent of decoder handles (`image_records.py`).
use serde::{Deserialize, Serialize};
use std::sync::Arc;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImageUse {
    pub number: usize,
    pub ordinal: usize,
    pub filename: String,
    pub source: Arc<[u8]>,
    pub format: String,
    pub natural_width: i64,
    pub natural_height: i64,
    pub width: i64,
    pub height: i64,
    pub depth: i64,
    pub pages: usize,
    pub color_depth: u32,
    pub page: usize,
    pub bbox: [f64; 4],
    pub bbox_sp: [i64; 4],
    pub rotation: i64,
    pub attributes: String,
    pub colorspace: i64,
    pub group: i64,
    pub data: Option<Arc<[u8]>>,
    pub dictionary: String,
}
impl ImageUse {
    pub fn new(
        number: usize,
        ordinal: usize,
        filename: String,
        source: Vec<u8>,
        format: String,
        natural: (i64, i64),
        dimensions: (i64, i64, i64),
    ) -> Self {
        Self {
            number,
            ordinal,
            filename,
            source: source.into(),
            format,
            natural_width: natural.0,
            natural_height: natural.1,
            width: dimensions.0,
            height: dimensions.1,
            depth: dimensions.2,
            pages: 1,
            color_depth: 0,
            page: 1,
            bbox: [0.0; 4],
            bbox_sp: [0; 4],
            rotation: 0,
            attributes: String::new(),
            colorspace: 0,
            group: 0,
            data: None,
            dictionary: String::new(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImagePolicy {
    pub high_color: bool,
    pub allow_alpha: bool,
    pub apply_gamma: bool,
    pub gamma: i64,
    pub image_gamma: i64,
    pub copy_fonts: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn immutable_payloads_share_storage_preserve_wire_bytes_and_isolate_replacement() {
        let raw = vec![1u8, 2, 3, 4];
        let mut image = ImageUse::new(
            1,
            1,
            "figure.png".into(),
            raw.clone(),
            "PNG".into(),
            (10, 20),
            (10, 20, 0),
        );
        image.data = Some(raw.clone().into());
        let mut copy = image.clone();
        assert!(Arc::ptr_eq(&image.source, &copy.source));
        assert!(Arc::ptr_eq(
            image.data.as_ref().unwrap(),
            copy.data.as_ref().unwrap()
        ));
        assert_eq!(
            bincode::serialize(&image.source).unwrap(),
            bincode::serialize(&raw).unwrap()
        );
        assert_eq!(
            bincode::serialize(&image.data).unwrap(),
            bincode::serialize(&Some(raw.clone())).unwrap()
        );
        copy.source = vec![9, 8].into();
        copy.data = Some(vec![7, 6].into());
        assert_eq!(image.source.as_ref(), raw);
        assert_eq!(image.data.as_deref().unwrap(), raw);
    }
}
