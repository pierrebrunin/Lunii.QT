//! Duration of the audio of a story on a device, estimated from its MP3 files
//! (`sf/` directory) without reading them whole.
//!
//! The first 512 bytes of each file are ciphered on the device: the frames
//! are looked for after them. The duration of a file is its size divided by
//! the mean size of a frame (several consecutive frames are read, so variable
//! bitrate files are estimated too), times the duration of a frame.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Ciphered head of the files on the device.
const CIPHERED: u64 = 512;
/// Bytes read from each file to measure its frames.
const SAMPLE: usize = 32 * 1024;

/// An MPEG audio frame header.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Frame {
    /// MPEG version and layer (frames of another kind end a run)
    kind: (u8, u8),
    sample_rate: u32,
    /// bytes, header included
    length: usize,
    /// samples per channel
    samples: u32,
}

fn frame(h: &[u8]) -> Option<Frame> {
    if h.len() < 4 || h[0] != 0xFF || h[1] & 0xE0 != 0xE0 {
        return None;
    }
    let version = (h[1] >> 3) & 0x03; // 0: 2.5, 2: 2, 3: 1
    let layer = (h[1] >> 1) & 0x03; // 1: III, 2: II, 3: I
    let bitrate_index = (h[2] >> 4) as usize;
    let rate_index = ((h[2] >> 2) & 0x03) as usize;
    let padding = ((h[2] >> 1) & 0x01) as usize;
    if version == 1 || layer == 0 || bitrate_index == 0 || bitrate_index == 15 || rate_index == 3 {
        return None;
    }
    const V1_L1: [u32; 15] = [
        0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
    ];
    const V1_L2: [u32; 15] = [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
    ];
    const V1_L3: [u32; 15] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    const V2_L1: [u32; 15] = [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
    ];
    const V2_L23: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
    let mpeg1 = version == 3;
    let kbps = match (mpeg1, layer) {
        (true, 3) => V1_L1,
        (true, 2) => V1_L2,
        (true, _) => V1_L3,
        (false, 3) => V2_L1,
        (false, _) => V2_L23,
    }[bitrate_index];
    let sample_rate = [44_100, 48_000, 32_000][rate_index]
        >> match version {
            3 => 0,
            2 => 1,
            _ => 2,
        };
    let bitrate = kbps as usize * 1000;
    let rate = sample_rate as usize;
    let (length, samples) = match layer {
        3 => ((12 * bitrate / rate + padding) * 4, 384),
        2 => (144 * bitrate / rate + padding, 1152),
        _ if mpeg1 => (144 * bitrate / rate + padding, 1152),
        _ => (72 * bitrate / rate + padding, 576),
    };
    Some(Frame {
        kind: (version, layer),
        sample_rate,
        length,
        samples,
    })
}

/// Duration in seconds of `total` bytes of MPEG audio, measured on `data` (a
/// part of them): the first run of at least 3 consecutive frames found.
fn estimate(data: &[u8], total: u64) -> Option<f64> {
    for start in 0..data.len().saturating_sub(4) {
        let Some(first) = frame(&data[start..]) else {
            continue;
        };
        let (mut pos, mut frames, mut bytes, mut seconds) = (start, 0u32, 0usize, 0f64);
        while let Some(f) = data.get(pos..).and_then(frame) {
            if f.kind != first.kind
                || f.sample_rate != first.sample_rate
                || pos + f.length > data.len()
            {
                break;
            }
            frames += 1;
            bytes += f.length;
            seconds += f.samples as f64 / f.sample_rate as f64;
            pos += f.length;
        }
        if frames >= 3 {
            return Some(total as f64 * seconds / bytes as f64);
        }
    }
    None
}

/// Duration in seconds of an MP3 file of the device.
fn file_duration(path: &Path) -> Option<f64> {
    let mut file = std::fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let skip = if size > CIPHERED + 1024 { CIPHERED } else { 0 };
    file.seek(SeekFrom::Start(skip)).ok()?;
    let mut data = Vec::with_capacity(SAMPLE);
    file.take(SAMPLE as u64).read_to_end(&mut data).ok()?;
    estimate(&data, size)
}

/// Duration in seconds of the audio of a story (its `sf/` directory), if its
/// files could be measured.
pub fn story_duration(story_dir: &Path) -> Option<u32> {
    let mut total = 0f64;
    let mut found = false;
    let mut dirs = vec![story_dir.join("sf")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if let Some(d) = file_duration(&path) {
                total += d;
                found = true;
            }
        }
    }
    found.then_some(total.round() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` MPEG-1 layer III frames, 44.1 kHz, `kbps`.
    fn frames(n: usize, kbps: u32) -> Vec<u8> {
        let index = [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ]
        .iter()
        .position(|&b| b == kbps)
        .unwrap() as u8;
        let len = 144 * kbps as usize * 1000 / 44_100;
        let mut out = Vec::new();
        for _ in 0..n {
            let mut f = vec![0x55u8; len];
            f[..4].copy_from_slice(&[0xFF, 0xFB, index << 4, 0xC4]);
            out.extend(f);
        }
        out
    }

    #[test]
    fn frame_headers() {
        let f = frame(&[0xFF, 0xFB, 0x90, 0x64]).unwrap();
        assert_eq!((f.sample_rate, f.length, f.samples), (44_100, 417, 1152));
        // padding
        assert_eq!(frame(&[0xFF, 0xFB, 0x92, 0x64]).unwrap().length, 418);
        // MPEG-2 layer III, 64 kbps, 22.05 kHz
        let f = frame(&[0xFF, 0xF3, 0x80, 0x64]).unwrap();
        assert_eq!((f.sample_rate, f.length, f.samples), (22_050, 208, 576));
        // invalid: no sync, free bitrate, reserved rate
        assert!(frame(&[0xFF, 0x0B, 0x90, 0]).is_none());
        assert!(frame(&[0xFF, 0xFB, 0x00, 0]).is_none());
        assert!(frame(&[0xFF, 0xFB, 0x9C, 0]).is_none());
    }

    #[test]
    fn story_on_device() {
        let dir = tempfile::tempdir().unwrap();
        let sf = dir.path().join("sf/000");
        std::fs::create_dir_all(&sf).unwrap();
        // ciphered head, then 10 minutes at 128 kbps
        let n = (600.0 * 44_100.0 / 1152.0) as usize;
        let mut a = vec![0x13u8; 512];
        a.extend(frames(n, 128));
        std::fs::write(sf.join("00000001"), &a).unwrap();
        // 1 minute at 64 kbps
        let n2 = (60.0 * 44_100.0 / 1152.0) as usize;
        let mut b = vec![0x27u8; 512];
        b.extend(frames(n2, 64));
        std::fs::write(sf.join("00000002"), &b).unwrap();
        // not audio
        std::fs::write(sf.join("00000003"), vec![0u8; 4096]).unwrap();
        let d = story_duration(dir.path()).unwrap();
        assert!((659..=661).contains(&d), "{d}");
        // no audio at all
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(story_duration(empty.path()), None);
    }
}
