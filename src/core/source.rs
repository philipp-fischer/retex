//! Immutable physical and expansion provenance (`core/source.py`).
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceSpan {
    // Copies of immutable provenance share text, including bounded call chains.
    // Serde still writes ordinary strings, preserving existing format records.
    pub file: Arc<str>,
    pub line: usize,
    pub column: usize,
    pub expansion: Option<Arc<str>>,
    pub call_site: Option<Arc<SourceSpan>>,
    pub call_site_truncated: bool,
    // Physical byte columns are one-based. Niche storage keeps both endpoints
    // within the previous point-only provenance allocation size; wire integers
    // and Option tags retain their existing serialized representation.
    pub byte_column: Option<std::num::NonZeroUsize>,
    /// Exclusive physical byte end on this line. None denotes generated input
    /// without a known physical range, rather than a guessed one-character span.
    #[serde(default)]
    pub end_byte_column: Option<std::num::NonZeroUsize>,
}

impl SourceSpan {
    /// Preserve diagnostic context for generated tokens without claiming that
    /// the context's physical letters produced those tokens.
    pub fn without_range(source: &Arc<Self>) -> Arc<Self> {
        if source.end_byte_column.is_none() {
            return source.clone();
        }
        let mut point = (**source).clone();
        point.end_byte_column = None;
        Arc::new(point)
    }
    pub fn new(file: impl Into<String>, line: usize, column: usize) -> Self {
        Self::with_file(Arc::from(file.into()), line, column)
    }
    pub(crate) fn with_file(file: Arc<str>, line: usize, column: usize) -> Self {
        Self {
            file,
            line,
            column,
            expansion: None,
            call_site: None,
            call_site_truncated: false,
            byte_column: None,
            end_byte_column: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Serialize)]
    struct LegacyRangeSpan {
        file: Arc<str>,
        line: usize,
        column: usize,
        expansion: Option<Arc<str>>,
        call_site: Option<Arc<SourceSpan>>,
        call_site_truncated: bool,
        byte_column: Option<usize>,
        end_byte_column: Option<usize>,
    }
    #[allow(dead_code)]
    struct LegacyPointSpan {
        file: Arc<str>,
        line: usize,
        column: usize,
        expansion: Option<Arc<str>>,
        call_site: Option<Arc<SourceSpan>>,
        call_site_truncated: bool,
        byte_column: Option<usize>,
    }
    #[test]
    fn physical_range_storage_preserves_point_footprint_and_existing_wire_integers() {
        assert!(std::mem::size_of::<SourceSpan>() <= std::mem::size_of::<LegacyPointSpan>());
        let mut source = SourceSpan::new("/project/main.tex", 1, 1);
        source.byte_column = std::num::NonZeroUsize::new(1);
        source.end_byte_column = std::num::NonZeroUsize::new(5);
        let legacy = LegacyRangeSpan {
            file: source.file.clone(),
            line: 1,
            column: 1,
            expansion: None,
            call_site: None,
            call_site_truncated: false,
            byte_column: Some(1),
            end_byte_column: Some(5),
        };
        assert_eq!(
            serde_json::to_value(&source).unwrap(),
            serde_json::to_value(&legacy).unwrap()
        );
        let bytes = bincode::serialize(&legacy).unwrap();
        assert_eq!(bincode::serialize(&source).unwrap(), bytes);
        assert_eq!(bincode::deserialize::<SourceSpan>(&bytes).unwrap(), source);
    }
    #[test]
    fn inline_origin_lists_preserve_legacy_sequences_and_isolate_mutation() {
        let source = Arc::new(SourceSpan::new("/project/main.tex", 2, 3));
        let one: NodeOrigins = [Some(source.clone())].into_iter().collect();
        let mapped: MappedOrigins = [source.clone()].into_iter().collect();
        assert!(!one.spilled() && !mapped.spilled());
        assert!(
            std::mem::size_of::<NodeOrigins>()
                <= std::mem::size_of::<Vec<Option<Arc<SourceSpan>>>>()
        );
        assert!(
            std::mem::size_of::<MappedOrigins>() <= std::mem::size_of::<Vec<Arc<SourceSpan>>>()
        );
        let mut copy = one.clone();
        copy[0] = None;
        assert!(one[0].is_some());
        let legacy = vec![Some(source.clone()), None, Some(source)];
        let many: NodeOrigins = legacy.clone().into();
        assert_eq!(
            serde_json::to_value(&many).unwrap(),
            serde_json::to_value(&legacy).unwrap()
        );
        let wire = bincode::serialize(&legacy).unwrap();
        assert_eq!(bincode::serialize(&many).unwrap(), wire);
        assert_eq!(bincode::deserialize::<NodeOrigins>(&wire).unwrap(), many);
    }
}

/// Most glyphs have one physical origin; keep it inline while preserving the
/// sequence wire representation and ordinary copy-on-write node semantics.
pub type NodeOrigins = smallvec::SmallVec<[Option<Arc<SourceSpan>>; 1]>;
pub type MappedOrigins = smallvec::SmallVec<[Arc<SourceSpan>; 1]>;
