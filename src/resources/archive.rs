//! Bounded, filesystem-free decoding of TeX Live runtime archives.
use crate::core::errors::{Error, Result};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    io::{Cursor, Read},
    path::Component,
};
const MAX_ARCHIVE: usize = 64 * 1024 * 1024;
const MAX_UNPACKED: usize = 128 * 1024 * 1024;

pub fn decode_xz(bytes: &[u8], limit: usize) -> Result<Vec<u8>> {
    if bytes.len() > MAX_ARCHIVE || limit == 0 || limit > MAX_UNPACKED {
        return Err(Error::value("TeX Live archive size limit exceeded"));
    }
    let decoder = lzma_rust2::XzReader::new_mem_limit(Cursor::new(bytes), true, 64 * 1024);
    let mut output = Vec::new();
    decoder
        .take(limit as u64 + 1)
        .read_to_end(&mut output)
        .map_err(|e| Error::value(format!("Invalid TeX Live XZ data: {e}")))?;
    if output.len() > limit {
        return Err(Error::value("TeX Live decompressed size limit exceeded"));
    }
    Ok(output)
}
#[derive(Serialize)]
pub struct ArchiveEntry {
    pub path: String,
    pub offset: usize,
    pub length: usize,
}
pub struct RuntimeArchive {
    pub manifest: Vec<ArchiveEntry>,
    pub bytes: Vec<u8>,
}
fn runtime_path(path: &std::path::Path) -> Result<Option<String>> {
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(Error::value("Unsafe TeX Live archive path"));
    }
    let path = path
        .strip_prefix("RELOC")
        .or_else(|_| path.strip_prefix("texmf-dist"))
        .unwrap_or(path);
    let path = path
        .to_str()
        .ok_or_else(|| Error::value("Non-UTF-8 TeX Live archive path"))?;
    let font = path.starts_with("fonts/")
        && [".tfm", ".vf", ".pfb", ".enc", ".map", ".sfd"]
            .iter()
            .any(|suffix| path.ends_with(suffix));
    Ok((path.starts_with("tex/") || font).then(|| path.to_owned()))
}
pub fn unpack_tar(bytes: &[u8], expected: &[String]) -> Result<RuntimeArchive> {
    if bytes.len() > MAX_UNPACKED || expected.len() > 16000 {
        return Err(Error::value("TeX Live runtime manifest limit exceeded"));
    }
    let allowed: BTreeSet<_> = expected.iter().cloned().collect();
    if allowed.len() != expected.len()
        || expected.iter().any(|p| {
            runtime_path(std::path::Path::new(p))
                .ok()
                .flatten()
                .as_deref()
                != Some(p)
        })
    {
        return Err(Error::value("Invalid TeX Live runtime manifest"));
    }
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let mut result = RuntimeArchive {
        manifest: Vec::new(),
        bytes: Vec::new(),
    };
    let mut seen = BTreeSet::new();
    for (count, entry) in archive.entries()?.enumerate() {
        if count >= 32000 {
            return Err(Error::value("TeX Live archive entry limit exceeded"));
        }
        let mut entry = entry?;
        if entry.header().entry_type().is_dir() {
            continue;
        }
        let Some(path) = runtime_path(&entry.path()?)? else {
            continue;
        };
        if !allowed.contains(&path)
            || !seen.insert(path.clone())
            || !entry.header().entry_type().is_file()
        {
            return Err(Error::value(format!(
                "Unexpected TeX Live runtime member: {path}"
            )));
        }
        let length = usize::try_from(entry.size())
            .map_err(|_| Error::value("TeX Live member size overflow"))?;
        if length > MAX_ARCHIVE.saturating_sub(result.bytes.len()) {
            return Err(Error::value("TeX Live runtime size limit exceeded"));
        }
        let offset = result.bytes.len();
        entry.read_to_end(&mut result.bytes)?;
        if result.bytes.len() - offset != length {
            return Err(Error::value("Truncated TeX Live runtime member"));
        }
        result.manifest.push(ArchiveEntry {
            path,
            offset,
            length,
        });
    }
    if seen != allowed {
        return Err(Error::value(
            "TeX Live archive differs from its runtime manifest",
        ));
    }
    Ok(result)
}
pub fn unpack_xz(bytes: &[u8], expected: &[String]) -> Result<RuntimeArchive> {
    unpack_tar(&decode_xz(bytes, MAX_UNPACKED)?, expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn archive(files: &[(&str, &[u8], tar::EntryType)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, data, kind) in files {
            let mut header = tar::Header::new_ustar();
            header.set_mode(0o644);
            header.set_size(data.len() as u64);
            header.set_entry_type(*kind);
            header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
            header.set_cksum();
            builder.append(&header, *data).unwrap();
        }
        builder.into_inner().unwrap()
    }
    #[test]
    fn runtime_members_are_verified_and_repacked_without_filesystem_access() {
        let bytes = archive(&[
            (
                "RELOC/tex/latex/example/example.sty",
                b"hello",
                tar::EntryType::Regular,
            ),
            (
                "tlpkg/tlpobj/example.tlpobj",
                b"metadata",
                tar::EntryType::Regular,
            ),
        ]);
        let result = unpack_tar(&bytes, &["tex/latex/example/example.sty".into()]).unwrap();
        assert_eq!(result.bytes, b"hello");
        assert_eq!(result.manifest[0].offset, 0);
        assert_eq!(result.manifest[0].length, 5);
    }
    #[test]
    fn missing_extra_duplicate_link_and_escaping_members_are_rejected() {
        let expected = ["tex/latex/example.sty".into()];
        for files in [
            vec![],
            vec![(
                "RELOC/tex/latex/other.sty",
                b"a".as_slice(),
                tar::EntryType::Regular,
            )],
            vec![(
                "RELOC/tex/latex/example.sty",
                b"a".as_slice(),
                tar::EntryType::Symlink,
            )],
            vec![(
                "../tex/latex/example.sty",
                b"a".as_slice(),
                tar::EntryType::Regular,
            )],
            vec![
                (
                    "RELOC/tex/latex/example.sty",
                    b"a".as_slice(),
                    tar::EntryType::Regular
                );
                2
            ],
        ] {
            assert!(unpack_tar(&archive(&files), &expected).is_err());
        }
    }
    #[test]
    fn xz_checksum_and_output_limits_are_enforced() {
        let mut data = vec![
            253, 55, 122, 88, 90, 0, 0, 4, 230, 214, 180, 70, 2, 0, 33, 1, 22, 0, 0, 0, 116, 47,
            229, 163, 1, 0, 16, 84, 101, 88, 32, 76, 105, 118, 101, 32, 109, 101, 116, 97, 100, 97,
            116, 97, 0, 0, 0, 0, 143, 122, 0, 233, 72, 150, 204, 166, 0, 1, 41, 17, 50, 10, 112,
            14, 31, 182, 243, 125, 1, 0, 0, 0, 0, 4, 89, 90,
        ];
        assert_eq!(decode_xz(&data, 100).unwrap(), b"TeX Live metadata");
        assert!(decode_xz(&data, 3).is_err());
        data[30] ^= 1;
        assert!(decode_xz(&data, 100).is_err());
    }
}
