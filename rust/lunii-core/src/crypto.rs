//! Ciphering primitives: Lunii flavoured XXTEA and AES-CBC (no padding).
//!
//! Only the header (first bytes) of story files is ciphered on the devices,
//! hence the `len` parameter of every helper.

use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use anyhow::{Result, bail};

const DELTA: u32 = 0x9E37_79B9;

/// Number of XXTEA rounds used by Lunii (`1 + 52 / n` instead of the usual `6 + 52 / n`).
pub fn lunii_tea_rounds(len_bytes: usize) -> u32 {
    let n = len_bytes / 4;
    if n == 0 {
        return 1;
    }
    (1 + 52 / n) as u32
}

fn key_words(key: &[u8]) -> Result<[u32; 4]> {
    if key.len() != 16 {
        bail!("XXTEA key must be 16 bytes long (got {})", key.len());
    }
    let mut k = [0u32; 4];
    for (i, w) in k.iter_mut().enumerate() {
        *w = u32::from_le_bytes(key[i * 4..i * 4 + 4].try_into().unwrap());
    }
    Ok(k)
}

fn to_words(data: &[u8]) -> Result<Vec<u32>> {
    if !data.len().is_multiple_of(4) {
        bail!(
            "XXTEA data length must be a multiple of 4 (got {})",
            data.len()
        );
    }
    Ok(data
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect())
}

fn from_words(words: &[u32], out: &mut [u8]) {
    for (i, w) in words.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
}

#[inline]
fn mx(sum: u32, y: u32, z: u32, p: usize, e: usize, k: &[u32; 4]) -> u32 {
    (((z >> 5) ^ (y << 2)).wrapping_add((y >> 3) ^ (z << 4)))
        ^ ((sum ^ y).wrapping_add(k[(p & 3) ^ e] ^ z))
}

fn btea_encrypt(v: &mut [u32], k: &[u32; 4], rounds: u32) {
    let n = v.len();
    if n < 2 {
        return;
    }
    let mut z = v[n - 1];
    let mut sum = 0u32;
    for _ in 0..rounds {
        sum = sum.wrapping_add(DELTA);
        let e = ((sum >> 2) & 3) as usize;
        for p in 0..n {
            let y = v[(p + 1) % n];
            v[p] = v[p].wrapping_add(mx(sum, y, z, p, e, k));
            z = v[p];
        }
    }
}

fn btea_decrypt(v: &mut [u32], k: &[u32; 4], rounds: u32) {
    let n = v.len();
    if n < 2 {
        return;
    }
    let mut sum = rounds.wrapping_mul(DELTA);
    let mut y = v[0];
    for _ in 0..rounds {
        let e = ((sum >> 2) & 3) as usize;
        for p in (1..n).rev() {
            let z = v[p - 1];
            v[p] = v[p].wrapping_sub(mx(sum, y, z, p, e, k));
            y = v[p];
        }
        let z = v[n - 1];
        v[0] = v[0].wrapping_sub(mx(sum, y, z, 0, e, k));
        y = v[0];
        sum = sum.wrapping_sub(DELTA);
    }
}

/// Raw XXTEA decryption of a whole buffer with Lunii rounds.
pub fn xxtea_decrypt(data: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    let k = key_words(key)?;
    let mut v = to_words(data)?;
    btea_decrypt(&mut v, &k, lunii_tea_rounds(data.len()));
    let mut out = vec![0u8; data.len()];
    from_words(&v, &mut out);
    Ok(out)
}

/// Raw XXTEA encryption of a whole buffer with Lunii rounds.
pub fn xxtea_encrypt(data: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    let k = key_words(key)?;
    let mut v = to_words(data)?;
    btea_encrypt(&mut v, &k, lunii_tea_rounds(data.len()));
    let mut out = vec![0u8; data.len()];
    from_words(&v, &mut out);
    Ok(out)
}

/// Deciphers the first `len` bytes of `buffer` (or less if the buffer is shorter).
pub fn xxtea_decipher(buffer: &[u8], key: &[u8], len: usize) -> Result<Vec<u8>> {
    let len = len.min(buffer.len());
    let mut out = buffer.to_vec();
    if len > 0 {
        let plain = xxtea_decrypt(&buffer[..len], key)?;
        out[..len].copy_from_slice(&plain);
    }
    Ok(out)
}

/// Ciphers the first `len` bytes of `buffer` (or less if the buffer is shorter).
pub fn xxtea_cipher(buffer: &[u8], key: &[u8], len: usize) -> Result<Vec<u8>> {
    let len = len.min(buffer.len());
    let mut out = buffer.to_vec();
    if len > 0 {
        let ciphered = xxtea_encrypt(&buffer[..len], key)?;
        out[..len].copy_from_slice(&ciphered);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// AES-CBC without padding
// ---------------------------------------------------------------------------

enum AnyAes {
    A128(aes::Aes128),
    A192(aes::Aes192),
    A256(aes::Aes256),
}

impl AnyAes {
    fn new(key: &[u8]) -> Result<Self> {
        Ok(match key.len() {
            16 => AnyAes::A128(aes::Aes128::new_from_slice(key).unwrap()),
            24 => AnyAes::A192(aes::Aes192::new_from_slice(key).unwrap()),
            32 => AnyAes::A256(aes::Aes256::new_from_slice(key).unwrap()),
            n => bail!("Incorrect AES key length ({n} bytes)"),
        })
    }

    fn encrypt_block(&self, block: &mut [u8; 16]) {
        let b = block.into();
        match self {
            AnyAes::A128(c) => c.encrypt_block(b),
            AnyAes::A192(c) => c.encrypt_block(b),
            AnyAes::A256(c) => c.encrypt_block(b),
        }
    }

    fn decrypt_block(&self, block: &mut [u8; 16]) {
        let b = block.into();
        match self {
            AnyAes::A128(c) => c.decrypt_block(b),
            AnyAes::A192(c) => c.decrypt_block(b),
            AnyAes::A256(c) => c.decrypt_block(b),
        }
    }
}

fn check_iv(iv: &[u8]) -> Result<[u8; 16]> {
    iv.try_into()
        .map_err(|_| anyhow::anyhow!("Incorrect IV length ({} bytes)", iv.len()))
}

/// AES-CBC encryption, `data` length must be a multiple of 16.
pub fn aes_cbc_encrypt(data: &[u8], key: &[u8], iv: &[u8]) -> Result<Vec<u8>> {
    if !data.len().is_multiple_of(16) {
        bail!("Data must be aligned to block boundary in ECB mode");
    }
    let cipher = AnyAes::new(key)?;
    let mut prev = check_iv(iv)?;
    let mut out = data.to_vec();
    for block in out.as_chunks_mut::<16>().0 {
        for (b, p) in block.iter_mut().zip(prev.iter()) {
            *b ^= p;
        }
        cipher.encrypt_block(block);
        prev.copy_from_slice(block);
    }
    Ok(out)
}

/// AES-CBC decryption, `data` length must be a multiple of 16.
pub fn aes_cbc_decrypt(data: &[u8], key: &[u8], iv: &[u8]) -> Result<Vec<u8>> {
    if !data.len().is_multiple_of(16) {
        bail!("Data must be padded to 16 byte boundary in CBC mode");
    }
    let cipher = AnyAes::new(key)?;
    let mut prev = check_iv(iv)?;
    let mut out = data.to_vec();
    for block in out.as_chunks_mut::<16>().0 {
        let mut current = [0u8; 16];
        current.copy_from_slice(block);
        cipher.decrypt_block(block);
        for (b, p) in block.iter_mut().zip(prev.iter()) {
            *b ^= p;
        }
        prev = current;
    }
    Ok(out)
}

/// Deciphers the first `len` bytes of `buffer` (or less if the buffer is shorter).
pub fn aes_decipher(buffer: &[u8], key: &[u8], iv: &[u8], len: usize) -> Result<Vec<u8>> {
    let len = len.min(buffer.len());
    let mut out = buffer.to_vec();
    if len > 0 {
        let plain = aes_cbc_decrypt(&buffer[..len], key, iv)?;
        out[..len].copy_from_slice(&plain);
    }
    Ok(out)
}

/// Ciphers the first `len` bytes of `buffer`.
///
/// When the buffer is shorter than `len` and not aligned on 16 bytes, it is padded
/// with zeros first (the resulting buffer is then longer than the input).
pub fn aes_cipher(buffer: &[u8], key: &[u8], iv: &[u8], len: usize) -> Result<Vec<u8>> {
    let mut out = buffer.to_vec();
    let mut len = len.min(out.len());
    if !len.is_multiple_of(16) {
        let padlen = 16 - out.len() % 16;
        out.resize(out.len() + padlen, 0);
        len += padlen;
    }
    if len > 0 {
        let ciphered = aes_cbc_encrypt(&out[..len], key, iv)?;
        out[..len].copy_from_slice(&ciphered);
    }
    Ok(out)
}

/// Reverses the byte order of each 32 bits word. `None` if not a multiple of 4.
pub fn reverse_bytes(input: &[u8]) -> Option<Vec<u8>> {
    if !input.len().is_multiple_of(4) {
        return None;
    }
    Some(
        input
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|c| c.iter().rev().copied())
            .collect(),
    )
}

/// Lower case hexadecimal representation (like `binascii.hexlify`).
pub fn hexlify(data: &[u8]) -> Vec<u8> {
    hex_string(data).into_bytes()
}

pub fn hex_string(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

/// Hexadecimal with a space between each byte (debug display).
pub fn hex_spaced(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parses an hexadecimal string (like `binascii.unhexlify`).
pub fn unhexlify(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        bail!("Odd-length string");
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            s.get(i..i + 2)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| anyhow::anyhow!("Non-hexadecimal digit found"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xxtea_roundtrip() {
        let key = crate::constants::lunii_generic_key();
        let data: Vec<u8> = (0..512u32).map(|i| (i * 7) as u8).collect();
        let c = xxtea_encrypt(&data, &key).unwrap();
        assert_ne!(c, data);
        assert_eq!(xxtea_decrypt(&c, &key).unwrap(), data);
    }

    #[test]
    fn aes_roundtrip() {
        let key = [1u8; 16];
        let iv = [2u8; 16];
        let data: Vec<u8> = (0..64u8).collect();
        let c = aes_cbc_encrypt(&data, &key, &iv).unwrap();
        assert_eq!(aes_cbc_decrypt(&c, &key, &iv).unwrap(), data);
    }

    #[test]
    fn aes_cipher_pads_short_buffers() {
        let out = aes_cipher(&[0xAA; 20], &[1u8; 16], &[0u8; 16], 512).unwrap();
        assert_eq!(out.len(), 32);
    }

    #[test]
    fn reverse() {
        assert_eq!(
            reverse_bytes(&[1, 2, 3, 4, 5, 6, 7, 8]).unwrap(),
            vec![4, 3, 2, 1, 8, 7, 6, 5]
        );
        assert!(reverse_bytes(&[1, 2, 3]).is_none());
    }

    #[test]
    fn hex() {
        assert_eq!(unhexlify("0aFF").unwrap(), vec![0x0a, 0xff]);
        assert_eq!(hex_string(&[0x0a, 0xff]), "0aff");
    }
}
