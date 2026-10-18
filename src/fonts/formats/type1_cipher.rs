//! Sequential Type1 feedback cipher (`formats/type1_cipher.py`).
pub const EEXEC_CIPHER_SEED: u16 = 55_665;
pub const CHARSTRING_CIPHER_SEED: u16 = 4_330;
pub const CIPHER_MULTIPLIER: u32 = 52_845;
pub const CIPHER_INCREMENT: u32 = 22_719;
pub const CIPHER_KEY_MASK: u32 = 0xffff;
pub const CIPHER_HIGH_BYTE_SHIFT: u32 = 8;

pub fn iter_cipher<I: IntoIterator<Item = u8>>(
    data: I,
    seed: u16,
    encrypt: bool,
) -> impl Iterator<Item = u8> {
    let mut seed = u32::from(seed);
    data.into_iter().map(move |byte| {
        let transformed = byte ^ (seed >> CIPHER_HIGH_BYTE_SHIFT) as u8;
        seed = ((u32::from(if encrypt { transformed } else { byte }) + seed) * CIPHER_MULTIPLIER
            + CIPHER_INCREMENT)
            & CIPHER_KEY_MASK;
        transformed
    })
}
pub fn cipher(data: &[u8], seed: u16, encrypt: bool) -> Vec<u8> {
    iter_cipher(data.iter().copied(), seed, encrypt).collect()
}
