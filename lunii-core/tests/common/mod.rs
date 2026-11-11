//! Fake devices shared by the integration tests.

use std::path::{Path, PathBuf};

use lunii_core::constants::lunii_generic_key;
use lunii_core::crypto;

/// Lunii v2 (firmware 2.22) in `dir/lunii_v2`.
pub fn lunii_v2(dir: &Path) -> PathBuf {
    let mut md = vec![0u8; 512];
    md[0..2].copy_from_slice(&3u16.to_le_bytes());
    md[6..8].copy_from_slice(&2u16.to_le_bytes());
    md[8..10].copy_from_slice(&22u16.to_le_bytes());
    md[10..18].copy_from_slice(&[0, 0, 0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC]);
    md[18..20].copy_from_slice(&0x0483u16.to_le_bytes());
    md[20..22].copy_from_slice(&0xA341u16.to_le_bytes());
    let devkey: Vec<u8> = (0..=255u8).collect();
    md[0x100..0x200]
        .copy_from_slice(&crypto::xxtea_encrypt(&devkey, &lunii_generic_key()).unwrap());
    let root = dir.join("lunii_v2");
    std::fs::create_dir_all(root.join(".content")).unwrap();
    std::fs::write(root.join(".md"), md).unwrap();
    root
}

/// Flam (firmware 2.0.0) in `dir/flam`.
pub fn flam(dir: &Path) -> PathBuf {
    let root = dir.join("flam");
    std::fs::create_dir_all(root.join("str")).unwrap();
    std::fs::create_dir_all(root.join("etc/library")).unwrap();
    let mut mdf = vec![0u8; 0x80];
    let fw = b"main: 2.0.0-1\ncomm: 2.0.0-1";
    mdf[2..2 + fw.len()].copy_from_slice(fw);
    mdf[50..74].copy_from_slice(b"303A819E0123456789ABCDEF");
    mdf[74..76].copy_from_slice(&0x303Au16.to_le_bytes());
    mdf[76..78].copy_from_slice(&0x819Eu16.to_le_bytes());
    for (i, b) in mdf[0x4E..0x6E].iter_mut().enumerate() {
        *b = i as u8 * 3;
    }
    std::fs::write(root.join(".mdf"), mdf).unwrap();
    root
}
