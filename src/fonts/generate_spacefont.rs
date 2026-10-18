//! Generate the implementation-owned empty Type1 extraction-space font.
use crate::{
    core::errors::Result,
    fonts::formats::type1_cipher::{CHARSTRING_CIPHER_SEED, EEXEC_CIPHER_SEED, cipher},
};
use std::path::Path;
pub fn generate(directory: &Path) -> Result<()> {
    let dimensions: [u16; 12] = [21, 2, 32, 32, 2, 1, 1, 1, 0, 0, 0, 7];
    let words: [i32; 15] = [
        0,
        10 << 20,
        1 << 24,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        1 << 20,
        0,
    ];
    let mut tfm = Vec::new();
    for n in dimensions {
        tfm.extend(n.to_be_bytes())
    }
    for n in words {
        tfm.extend(n.to_be_bytes())
    }
    std::fs::write(directory.join("pdftexspace.tfm"), tfm)?;
    let header=b"%!PS-AdobeFont-1.0: PdfTeX-Space 1.0\n10 dict begin\n/FontName /PdfTeX-Space def\n/FontType 1 def /PaintType 0 def\n/FontMatrix [0.001 0 0 0.001 0 0] readonly def\n/FontBBox [0 0 0 0] readonly def\n/Encoding 256 array 0 1 255 {1 index exch /.notdef put} for dup 32 /space put readonly def\ncurrentdict end\ncurrentfile eexec\n";
    let mut private = vec![0, 0, 0, 0];
    private.extend(b"dup /Private 10 dict dup begin\n/RD {string currentfile exch readstring pop} bind def\n/ND {noaccess def} bind def\n/NP {noaccess put} bind def\n/BlueValues [] def /MinFeature [16 16] def /password 5839 def /lenIV 4 def /Subrs 0 array def\n2 index /CharStrings 2 dict dup begin\n");
    let encoded = cipher(
        &[0, 0, 0, 0, 139, 139, 13, 14],
        CHARSTRING_CIPHER_SEED,
        true,
    );
    for name in [".notdef", "space"] {
        private.extend(format!("/{name} {} RD ", encoded.len()).as_bytes());
        private.extend(&encoded);
        private.extend(b" ND\n");
    }
    private.extend(b"end end readonly put noaccess put\ndup /FontName get exch definefont pop\nmark currentfile closefile\n");
    let encrypted = cipher(&private, EEXEC_CIPHER_SEED, true);
    let mut trailer = b"\n".to_vec();
    for _ in 0..8 {
        trailer.extend(vec![b'0'; 64]);
        trailer.push(b'\n')
    }
    trailer.extend(b"cleartomark\n");
    let mut pfb = Vec::new();
    for (kind, data) in [
        (1, header.as_slice()),
        (2, encrypted.as_slice()),
        (1, trailer.as_slice()),
    ] {
        pfb.extend([0x80, kind]);
        pfb.extend((data.len() as u32).to_le_bytes());
        pfb.extend(data);
    }
    pfb.extend([0x80, 3]);
    std::fs::write(directory.join("pdftexspace.pfb"), pfb)?;
    Ok(())
}
