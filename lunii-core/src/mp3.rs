//! Duration of the audio of a story on a device, estimated from its MP3 files
//! (`sf/` directory) without reading them whole.
//!
//! The first 512 bytes of each file are ciphered on the device: the frames
//! are looked for after them, in windows spread over the file. Each window
//! gives a bitrate (bytes of its frames for their duration); the duration of
//! the file is its size at the median of these bitrates, so that a quiet
//! intro or a different ending of a variable bitrate file does not count.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::convert::audio::parse_frame_header;

/// Ciphered head of the files on the device.
const CIPHERED: u64 = 512;
/// Bytes of each window.
const WINDOW: u64 = 8 * 1024;
/// Most windows read in a file.
const WINDOWS: u64 = 16;

/// Bitrate (bytes per second) of the first run of at least 3 consecutive
/// frames of the same kind found in `data`.
fn bitrate(data: &[u8]) -> Option<f64> {
    for start in 0..data.len().saturating_sub(4) {
        let Some(first) = parse_frame_header(&data[start..]) else {
            continue;
        };
        let (mut pos, mut frames, mut bytes, mut seconds) = (start, 0u32, 0usize, 0f64);
        while let Some(f) = data.get(pos..).and_then(parse_frame_header) {
            if (f.mpeg1, f.layer, f.sample_rate) != (first.mpeg1, first.layer, first.sample_rate)
                || f.frame_len == 0
                || pos + f.frame_len > data.len()
            {
                break;
            }
            frames += 1;
            bytes += f.frame_len;
            seconds += f.samples as f64 / f.sample_rate as f64;
            pos += f.frame_len;
        }
        if frames >= 3 {
            return Some(bytes as f64 / seconds);
        }
    }
    None
}

/// Duration in seconds of an MP3 file of the device.
fn file_duration(path: &Path) -> Option<f64> {
    let mut file = std::fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let head = if size > CIPHERED + 2 * WINDOW {
        CIPHERED
    } else {
        0
    };
    let audio = size - head;
    // small file: one window over all of it
    let (count, window) = if audio <= WINDOWS * WINDOW {
        (1, audio)
    } else {
        (WINDOWS, WINDOW)
    };
    let mut rates = Vec::new();
    for i in 0..count {
        let offset = head + i * (audio - window) / (count - 1).max(1);
        file.seek(SeekFrom::Start(offset)).ok()?;
        let mut data = Vec::with_capacity(window as usize);
        (&mut file).take(window).read_to_end(&mut data).ok()?;
        rates.extend(bitrate(&data));
    }
    if rates.is_empty() {
        return None;
    }
    rates.sort_by(f64::total_cmp);
    Some(size as f64 / rates[rates.len() / 2])
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

    /// `seconds` of MPEG-1 layer III frames, 44.1 kHz, `kbps`.
    fn frames(seconds: f64, kbps: u32) -> Vec<u8> {
        let index = [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ]
        .iter()
        .position(|&b| b == kbps)
        .unwrap() as u8;
        let len = 144 * kbps as usize * 1000 / 44_100;
        let n = (seconds * 44_100.0 / 1152.0) as usize;
        let mut out = Vec::new();
        for _ in 0..n {
            let mut f = vec![0x55u8; len];
            f[..4].copy_from_slice(&[0xFF, 0xFB, index << 4, 0xC4]);
            out.extend(f);
        }
        out
    }

    /// A file of the device: its ciphered head, then `audio`.
    fn device_file(path: &Path, audio: &[u8]) {
        let mut data = vec![0x13u8; 512];
        data.extend_from_slice(audio);
        std::fs::write(path, data).unwrap();
    }

    #[test]
    fn story_on_device() {
        let dir = tempfile::tempdir().unwrap();
        let sf = dir.path().join("sf/000");
        std::fs::create_dir_all(&sf).unwrap();
        // 10 minutes at 128 kbps, 1 minute at 64 kbps, then not audio
        device_file(&sf.join("00000001"), &frames(600.0, 128));
        device_file(&sf.join("00000002"), &frames(60.0, 64));
        std::fs::write(sf.join("00000003"), vec![0u8; 4096]).unwrap();
        let d = story_duration(dir.path()).unwrap();
        assert!((659..=661).contains(&d), "{d}");
        // short file (a menu sound): read whole
        let short = tempfile::tempdir().unwrap();
        let sf = short.path().join("sf/000");
        std::fs::create_dir_all(&sf).unwrap();
        device_file(&sf.join("00000001"), &frames(2.0, 64));
        assert_eq!(story_duration(short.path()), Some(2));
        // no audio at all
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(story_duration(empty.path()), None);
    }

    /// Variable bitrate: a quiet intro and ending do not count.
    #[test]
    fn variable_bitrate() {
        let dir = tempfile::tempdir().unwrap();
        let sf = dir.path().join("sf/000");
        std::fs::create_dir_all(&sf).unwrap();
        let mut audio = frames(5.0, 32);
        audio.extend(frames(1200.0, 128));
        audio.extend(frames(5.0, 32));
        device_file(&sf.join("00000001"), &audio);
        let d = story_duration(dir.path()).unwrap();
        // 1210 s; at the mean bitrate of the start it would be about 4 times longer
        assert!((1180..=1240).contains(&d), "{d}");
    }
}
