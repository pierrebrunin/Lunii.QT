//! Audio checks and transcoding (port of `pkg/api/convert_audio.py`).
//!
//! The MP3 analysis replaces `mutagen`: it only extracts what the storytellers
//! care about (sample rate, channel mode, bitrate mode and ID3 tags).

use std::io::Read;
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};

use crate::constants::which_ffmpeg;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitrateMode {
    Unknown,
    Cbr,
    Vbr,
    Abr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mp3Info {
    pub sample_rate: u32,
    pub mono: bool,
    pub bitrate_mode: BitrateMode,
}

/// Size of the ID3v2 tag at `pos` (header included), if any.
fn id3v2_size(data: &[u8], pos: usize) -> Option<usize> {
    let h = data.get(pos..pos + 10)?;
    if &h[0..3] != b"ID3" || h[6..10].iter().any(|b| b & 0x80 != 0) {
        return None;
    }
    let size = h[6..10]
        .iter()
        .fold(0usize, |acc, b| (acc << 7) | *b as usize);
    let footer = if h[5] & 0x10 != 0 { 10 } else { 0 };
    Some(10 + size + footer)
}

/// Offset of the first byte after the leading ID3v2 tags.
fn skip_id3v2(data: &[u8]) -> usize {
    let mut pos = 0;
    while let Some(size) = id3v2_size(data, pos) {
        pos += size;
    }
    pos.min(data.len())
}

fn has_id3v1(data: &[u8]) -> bool {
    data.len() >= 128 && &data[data.len() - 128..data.len() - 125] == b"TAG"
}

struct FrameHeader {
    mpeg1: bool,
    layer: u8,
    sample_rate: u32,
    mono: bool,
    frame_len: usize,
}

fn parse_frame_header(h: &[u8]) -> Option<FrameHeader> {
    if h.len() < 4 || h[0] != 0xFF || h[1] & 0xE0 != 0xE0 {
        return None;
    }
    let version = (h[1] >> 3) & 3; // 0: MPEG2.5, 2: MPEG2, 3: MPEG1
    let layer = match (h[1] >> 1) & 3 {
        3 => 1,
        2 => 2,
        1 => 3,
        _ => return None,
    };
    if version == 1 {
        return None;
    }
    let bitrate_idx = (h[2] >> 4) as usize;
    let sr_idx = ((h[2] >> 2) & 3) as usize;
    if bitrate_idx == 0 || bitrate_idx == 15 || sr_idx == 3 {
        return None;
    }
    let padding = ((h[2] >> 1) & 1) as usize;
    let mono = (h[3] >> 6) == 3;

    let mpeg1 = version == 3;
    let sample_rate = match version {
        3 => [44100, 48000, 32000][sr_idx],
        2 => [22050, 24000, 16000][sr_idx],
        _ => [11025, 12000, 8000][sr_idx],
    };
    const BR_V1_L1: [u32; 15] = [
        0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
    ];
    const BR_V1_L2: [u32; 15] = [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
    ];
    const BR_V1_L3: [u32; 15] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    const BR_V2_L1: [u32; 15] = [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
    ];
    const BR_V2_L23: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
    let bitrate = 1000
        * match (mpeg1, layer) {
            (true, 1) => BR_V1_L1,
            (true, 2) => BR_V1_L2,
            (true, _) => BR_V1_L3,
            (false, 1) => BR_V2_L1,
            (false, _) => BR_V2_L23,
        }[bitrate_idx] as usize;
    let sr = sample_rate as usize;
    let frame_len = match layer {
        1 => (12 * bitrate / sr + padding) * 4,
        3 if !mpeg1 => 72 * bitrate / sr + padding,
        _ => 144 * bitrate / sr + padding,
    };
    Some(FrameHeader {
        mpeg1,
        layer,
        sample_rate,
        mono,
        frame_len,
    })
}

fn bitrate_mode(data: &[u8], pos: usize, h: &FrameHeader) -> BitrateMode {
    if h.layer == 3 {
        let side_info = match (h.mpeg1, h.mono) {
            (true, true) => 17,
            (true, false) => 32,
            (false, true) => 9,
            (false, false) => 17,
        };
        let xing = pos + 4 + side_info;
        if let Some(tag) = data.get(xing..xing + 8)
            && (&tag[0..4] == b"Xing" || &tag[0..4] == b"Info")
        {
            let is_info = &tag[0..4] == b"Info";
            let flags = u32::from_be_bytes(tag[4..8].try_into().unwrap());
            let mut off = xing + 8;
            if flags & 1 != 0 {
                off += 4;
            }
            if flags & 2 != 0 {
                off += 4;
            }
            if flags & 4 != 0 {
                off += 100;
            }
            let has_scale = flags & 8 != 0;
            if has_scale {
                off += 4;
            }
            if let Some(lame) = data.get(off..off + 10)
                && (lame.starts_with(b"LAME") || lame.starts_with(b"L3.99"))
            {
                match lame[9] & 0x0F {
                    1 | 8 => return BitrateMode::Cbr,
                    2 | 9 => return BitrateMode::Abr,
                    3..=6 => return BitrateMode::Vbr,
                    _ => {}
                }
            }
            if is_info {
                return BitrateMode::Cbr;
            }
            if has_scale {
                return BitrateMode::Vbr;
            }
            return BitrateMode::Unknown;
        }
    }
    if data.get(pos + 36..pos + 40) == Some(b"VBRI") {
        return BitrateMode::Vbr;
    }
    BitrateMode::Unknown
}

/// Parses the first valid MPEG audio frame.
pub fn mp3_info(data: &[u8]) -> Result<Mp3Info> {
    let start = skip_id3v2(data);
    let mut pos = start;
    while pos + 4 <= data.len() {
        if let Some(h) = parse_frame_header(&data[pos..]) {
            // the next frame must also be valid (or be the end of data)
            let next = pos + h.frame_len;
            let next_ok = next + 4 > data.len() || parse_frame_header(&data[next..]).is_some();
            if h.frame_len > 0 && next_ok {
                return Ok(Mp3Info {
                    sample_rate: h.sample_rate,
                    mono: h.mono,
                    bitrate_mode: bitrate_mode(data, pos, &h),
                });
            }
        }
        pos += 1;
    }
    bail!("can't sync to MPEG frame")
}

/// `seconds` of silence: MPEG-1 layer III, 32 kbps, 44.1 kHz, mono (empty
/// frames decode to silence).
pub fn silent_mp3(seconds: f32) -> Vec<u8> {
    const FRAME_LEN: usize = 144 * 32_000 / 44_100;
    let mut frame = [0u8; FRAME_LEN];
    frame[..4].copy_from_slice(&[0xFF, 0xFB, 0x10, 0xC4]);
    let frames = (seconds * 44_100.0 / 1152.0).ceil().max(1.0) as usize;
    frame.repeat(frames)
}

/// Are there ID3 tags to be removed?
pub fn tags_removal_required(data: &[u8]) -> bool {
    if let Some(size) = id3v2_size(data, 0) {
        // at least one frame (and not only padding)
        if data
            .get(10..size.min(data.len()))
            .map(|body| body.first().is_some_and(|b| *b != 0))
            .unwrap_or(false)
        {
            return true;
        }
    }
    if has_id3v1(data) {
        let tag = &data[data.len() - 128..];
        let text_present = tag[3..125].iter().any(|b| *b != 0 && *b != b' ');
        return text_present || tag[127] != 0xFF;
    }
    false
}

/// Is a conversion to mono 44.1KHz MP3 needed?
pub fn transcoding_required(filename: &str, data: &[u8]) -> bool {
    if !filename.to_lowercase().ends_with(".mp3") {
        return true;
    }
    match mp3_info(data) {
        Ok(info) => info.sample_rate < 44100 || info.bitrate_mode == BitrateMode::Abr || !info.mono,
        // unreadable MP3, let ffmpeg fix it
        Err(_) => true,
    }
}

/// Removes ID3v2 (start) and ID3v1 (end) tags.
pub fn mp3_tag_cleanup(data: &[u8]) -> Vec<u8> {
    let start = id3v2_size(data, 0).unwrap_or(0).min(data.len());
    let mut end = data.len();
    if has_id3v1(data) && end - 128 >= start {
        end -= 128;
    }
    data[start..end].to_vec()
}

/// Removes its file on drop.
struct TempFile(std::path::PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Transcodes any audio to mono 44.1KHz VBR MP3 without tags, using ffmpeg.
///
/// The input goes through a temporary file, not a pipe: MP4 / M4A files (the
/// podcasts) often have their index at the end, ffmpeg needs to seek to read
/// them. From a pipe it outputs a few bytes and still exits successfully, so
/// the output is checked.
pub fn audio_to_mp3(data: &[u8]) -> Result<Vec<u8>> {
    transcode(data, None)
}

/// Processing of the audio of the albums and podcasts tracks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioOptions {
    /// same loudness for every track (see [`normalized_mp3`])
    pub normalize_volume: bool,
}

impl AudioOptions {
    /// Is the volume normalized? Needs ffmpeg (skipped without it).
    pub fn normalize(&self) -> bool {
        self.normalize_volume && which_ffmpeg().is_some()
    }
}

/// Loudness target of [`normalized_mp3`] (EBU R128, single pass): -16 LUFS,
/// as the podcasts, true peak -1.5 dB.
const LOUDNORM: &str = "loudnorm=I=-16:TP=-1.5:LRA=11";

/// Like [`audio_to_mp3`], with the volume normalized: the tracks of an album
/// coming from different sources play at the same level.
pub fn normalized_mp3(data: &[u8]) -> Result<Vec<u8>> {
    transcode(data, Some(LOUDNORM))
}

fn transcode(data: &[u8], filter: Option<&str>) -> Result<Vec<u8>> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    let ffmpeg = which_ffmpeg().context("FFMPEG is not available")?;
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let input =
        TempFile(std::env::temp_dir().join(format!("lunii-ffmpeg-{}-{n}", std::process::id())));
    std::fs::write(&input.0, data).context("unable to write the ffmpeg input file")?;

    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-threads", "0", "-i"])
        .arg(&input.0);
    if let Some(filter) = filter {
        cmd.args(["-af", filter]);
    }
    cmd.args([
        "-map",
        "0:a",
        "-codec",
        "libmp3lame",
        "-ar",
        "44100",
        "-ac",
        "1",
        "-aq",
        "5",
    ])
    .args([
        "-map_metadata",
        "-1",
        "-write_xing",
        "0",
        "-id3v2_version",
        "0",
        "-f",
        "mp3",
        "pipe:1",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn().context("unable to start ffmpeg")?;
    // stderr read in parallel: a full stderr pipe would block ffmpeg
    let mut stderr_pipe = child.stderr.take().unwrap();
    let stderr_reader = std::thread::spawn(move || {
        let mut stderr = String::new();
        let _ = stderr_pipe.read_to_string(&mut stderr);
        stderr
    });
    let mut stdout = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut stdout)?;
    let status = child.wait()?;
    let stderr = stderr_reader.join().unwrap_or_default();

    if !status.success() {
        bail!("ffmpeg error: {}", stderr.trim());
    }
    if mp3_info(&stdout).is_err() {
        bail!("ffmpeg produced no audio: {}", stderr.trim());
    }
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M4A with its index (moov) at the end, as the Radio France podcasts: it
    /// can't be read from a pipe.
    #[test]
    fn m4a_to_mp3() {
        let Some(ffmpeg) = which_ffmpeg() else {
            eprintln!("ffmpeg not available, test skipped");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let m4a = dir.path().join("in.m4a");
        let ok = Command::new(ffmpeg)
            .args([
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=3",
            ])
            .args(["-ac", "2", "-ar", "48000", "-c:a", "aac"])
            .arg(&m4a)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let mp3 = audio_to_mp3(&std::fs::read(&m4a).unwrap()).unwrap();
        let info = mp3_info(&mp3).unwrap();
        assert!(info.mono && info.sample_rate == 44100);
        assert!(mp3.len() > 10_000, "{} bytes", mp3.len());
        // not audio: an error, not an empty MP3
        assert!(audio_to_mp3(b"definitely not audio").is_err());
    }

    /// A quiet sine wave comes out louder once normalized.
    #[test]
    fn loudness_normalization() {
        let Some(ffmpeg) = which_ffmpeg() else {
            eprintln!("ffmpeg not available, test skipped");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("quiet.wav");
        let ok = Command::new(&ffmpeg)
            .args(["-loglevel", "error", "-f", "lavfi", "-i"])
            .arg("sine=frequency=440:duration=4,volume=0.02")
            .arg(&wav)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let data = std::fs::read(&wav).unwrap();
        let plain = audio_to_mp3(&data).unwrap();
        let normalized = normalized_mp3(&data).unwrap();
        let info = mp3_info(&normalized).unwrap();
        assert!(info.mono && info.sample_rate == 44100);
        // mean volume measured by ffmpeg (volumedetect)
        let mean = |mp3: &[u8]| {
            let f = dir.path().join("m.mp3");
            std::fs::write(&f, mp3).unwrap();
            let out = Command::new(&ffmpeg)
                .args(["-hide_banner", "-i"])
                .arg(&f)
                .args(["-af", "volumedetect", "-f", "null", "-"])
                .output()
                .unwrap();
            let err = String::from_utf8_lossy(&out.stderr).to_string();
            let line = err
                .lines()
                .find(|l| l.contains("mean_volume"))
                .unwrap()
                .to_string();
            line.split("mean_volume:")
                .nth(1)
                .unwrap()
                .trim()
                .trim_end_matches(" dB")
                .parse::<f32>()
                .unwrap()
        };
        let (before, after) = (mean(&plain), mean(&normalized));
        assert!(after > before + 15.0, "{before} dB -> {after} dB");
    }

    #[test]
    fn silence() {
        let silence = silent_mp3(1.0);
        assert_eq!(silence.len(), 39 * 104);
        let info = mp3_info(&silence).unwrap();
        assert!(info.mono && info.sample_rate == 44100);
        assert!(!transcoding_required("s.mp3", &silence));
    }

    /// Builds a minimal MPEG1 Layer3 stream (128kbps, 44.1KHz).
    fn frames(mono: bool, count: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..count {
            let mut f = vec![0u8; 417];
            f[0] = 0xFF;
            f[1] = 0xFB;
            f[2] = 0x90;
            f[3] = if mono { 0xC0 } else { 0x00 };
            out.extend(f);
        }
        out
    }

    #[test]
    fn parse_mono_stereo() {
        let info = mp3_info(&frames(true, 3)).unwrap();
        assert_eq!(info.sample_rate, 44100);
        assert!(info.mono);
        assert!(!transcoding_required("a.mp3", &frames(true, 3)));
        assert!(transcoding_required("a.mp3", &frames(false, 3)));
        assert!(transcoding_required("a.ogg", &frames(true, 3)));
    }

    #[test]
    fn tags() {
        let mut data = b"ID3\x03\x00\x00\x00\x00\x00\x0ATIT2\x00\x00\x00\x00\x00\x00".to_vec();
        data.extend(frames(true, 2));
        assert!(tags_removal_required(&data));
        let clean = mp3_tag_cleanup(&data);
        assert_eq!(clean, frames(true, 2));
        assert!(!tags_removal_required(&clean));
        assert!(mp3_info(&data).is_ok());
    }
}
