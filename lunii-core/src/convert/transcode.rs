//! Audio conversion to the MP3 of the storytellers (mono, 44.1 kHz, no
//! tags), in pure Rust: any audio is decoded (Symphonia: MP3, AAC / M4A,
//! Vorbis, FLAC, WAV...; opus-pure for Opus, its packets read by Symphonia),
//! mixed to mono, resampled (Rubato), optionally normalized (EBU R128), then
//! encoded (rusty_mp3).

use anyhow::{Context as _, Result, bail};
use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Fft, FixedSync, Resampler};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::well_known::CODEC_ID_OPUS;
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as DecodeError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;

/// Sample rate of the storytellers.
const RATE: u32 = 44_100;
/// Constant bitrate of the MP3 produced (mono): smaller files than the earlier
/// ffmpeg conversion (VBR, about 72 kbps), with a better signal to noise ratio
/// on stories.
const KBPS: u32 = 64;
/// Loudness of the normalized tracks: -16 LUFS (as the podcasts), the peaks
/// at most at -1.5 dBTP.
const TARGET_LUFS: f64 = -16.0;
const MAX_PEAK_DB: f64 = -1.5;

/// Mono 44.1 kHz MP3 of any audio, the volume normalized if `normalize`.
pub(crate) fn to_mp3(data: &[u8], normalize: bool) -> Result<Vec<u8>> {
    let (mut samples, rate) = decode(data)?;
    if rate != RATE {
        samples = resample(samples, rate)?;
    }
    if normalize {
        normalize_loudness(&mut samples)?;
    }
    encode(&samples)
}

/// Decodes the first audio track, mixed to mono: its samples and their rate.
fn decode(data: &[u8]) -> Result<(Vec<f32>, u32)> {
    let source = MediaSourceStream::new(
        Box::new(std::io::Cursor::new(data.to_vec())),
        MediaSourceStreamOptions::default(),
    );
    let mut reader = symphonia::default::get_probe()
        .probe(
            &Hint::new(),
            source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| anyhow::anyhow!("unsupported audio format ({e})"))?;
    let track = reader
        .default_track(TrackType::Audio)
        .context("no audio track")?;
    let id = track.id;
    let Some(CodecParameters::Audio(params)) = &track.codec_params else {
        bail!("no audio track");
    };
    let mut decoder = match symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
    {
        Ok(d) => Decoder::Symphonia(d),
        Err(_) if params.codec == CODEC_ID_OPUS => {
            Decoder::Opus(Box::new(Opus::new(params, track.delay, track.padding)?))
        }
        Err(e) => bail!("unsupported audio codec ({e})"),
    };

    let (mut mono, mut rate, mut frame) = (Vec::new(), 0, Vec::new());
    loop {
        let packet = match reader.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            // truncated file: what was read is kept
            Err(DecodeError::IoError(_)) if !mono.is_empty() => break,
            Err(e) => bail!("unreadable audio ({e})"),
        };
        if packet.track_id != id {
            continue;
        }
        let channels = match &mut decoder {
            Decoder::Symphonia(d) => {
                let buffer = match d.decode(&packet) {
                    Ok(b) => b,
                    // a damaged packet is skipped, as players do
                    Err(DecodeError::DecodeError(_)) => continue,
                    Err(e) => bail!("unreadable audio ({e})"),
                };
                rate = buffer.spec().rate();
                buffer.copy_to_vec_interleaved(&mut frame);
                buffer.spec().channels().count().max(1)
            }
            Decoder::Opus(o) => {
                // a damaged packet is skipped, as players do
                if !o.decode(&packet.data, &mut frame) {
                    continue;
                }
                rate = Opus::RATE;
                o.channels
            }
        };
        // the channels are added at -3 dB each, as ffmpeg does: the same
        // loudness for a stereo recording of a mono voice
        let gain = 1.0 / (channels as f32).sqrt();
        mono.extend(
            frame
                .chunks_exact(channels)
                .map(|f| f.iter().sum::<f32>() * gain),
        );
    }
    if let Decoder::Opus(o) = &decoder {
        mono.truncate(mono.len().saturating_sub(o.padding));
    }
    if mono.is_empty() || rate == 0 {
        bail!("no audio decoded");
    }
    // the channels added may exceed full scale: lowered, not clipped
    let peak = mono.iter().fold(0f32, |m, s| m.max(s.abs()));
    if peak > 1.0 {
        mono.iter_mut().for_each(|s| *s /= peak);
    }
    Ok((mono, rate))
}

enum Decoder {
    Symphonia(Box<dyn AudioDecoder>),
    Opus(Box<Opus>),
}

/// Opus stream (Ogg, WebM / Matroska, MP4), its packets decoded by opus-pure:
/// mono or stereo, at 48 kHz.
struct Opus {
    decoder: opus_pure::OpusDecoder,
    channels: usize,
    /// samples at the start still to drop (encoder delay, `pre-skip`)
    skip: usize,
    /// samples at the end to drop (padding of the last packet)
    padding: usize,
    /// output gain of the stream header
    gain: f32,
    pcm: Vec<f32>,
}

impl Opus {
    const RATE: u32 = 48_000;
    /// longest packet: 120 ms
    const MAX_FRAME: usize = 5760;

    /// `delay` and `padding` as the container gives them, else from the
    /// Opus header (`OpusHead`, the codec extra data).
    fn new(
        params: &AudioCodecParameters,
        delay: Option<u32>,
        padding: Option<u32>,
    ) -> Result<Self> {
        // OpusHead: version, channels, pre-skip (LE u16), input rate, gain
        // (Q7.8 dB, LE i16), mapping family
        let head = params
            .extra_data
            .as_deref()
            .filter(|h| h.len() >= 19 && h.starts_with(b"OpusHead"));
        let channels = head
            .map(|h| h[9] as usize)
            .or_else(|| params.channels.as_ref().map(|c| c.count()))
            .unwrap_or(1);
        let family = head.map_or(0, |h| h[18]);
        if !(1..=2).contains(&channels) || family != 0 {
            bail!("unsupported audio codec (Opus, {channels} channels)");
        }
        let pre_skip = head.map(|h| u16::from_le_bytes([h[10], h[11]]) as u32);
        let gain_db = head.map_or(0.0, |h| i16::from_le_bytes([h[16], h[17]]) as f32 / 256.0);
        let decoder = opus_pure::OpusDecoder::new(Self::RATE as i32, channels)
            .map_err(|e| anyhow::anyhow!("unsupported audio codec (Opus: {e})"))?;
        Ok(Self {
            decoder,
            channels,
            skip: delay.or(pre_skip).unwrap_or(0) as usize,
            padding: padding.unwrap_or(0) as usize,
            gain: 10f32.powf(gain_db / 20.0),
            pcm: vec![0.0; Self::MAX_FRAME * channels],
        })
    }

    /// Decodes a packet into `frame` (interleaved), the start of the stream
    /// dropped. `false` if it is damaged.
    fn decode(&mut self, packet: &[u8], frame: &mut Vec<f32>) -> bool {
        let Ok(samples) = self.decoder.decode(packet, Self::MAX_FRAME, &mut self.pcm) else {
            return false;
        };
        let skipped = self.skip.min(samples);
        self.skip -= skipped;
        frame.clear();
        frame.extend(
            self.pcm[skipped * self.channels..samples * self.channels]
                .iter()
                .map(|s| s * self.gain),
        );
        true
    }
}

fn resample(samples: Vec<f32>, rate: u32) -> Result<Vec<f32>> {
    let mut resampler = Fft::<f32>::new(rate as usize, RATE as usize, 1024, 1, FixedSync::Both)
        .context("unable to resample the audio")?;
    let len = samples.len();
    let input = [samples];
    let adapter = SequentialSliceOfVecs::new(&input, 1, len)?;
    Ok(resampler
        .process_all(&adapter, len, None)
        .context("unable to resample the audio")?
        .take_data())
}

/// Brings the loudness to [`TARGET_LUFS`], the peaks kept under
/// [`MAX_PEAK_DB`]. Silence is left as is.
fn normalize_loudness(samples: &mut [f32]) -> Result<()> {
    use ebur128::{EbuR128, Mode};
    let mut meter = EbuR128::new(1, RATE, Mode::I | Mode::TRUE_PEAK)?;
    meter.add_frames_f32(samples)?;
    let loudness = meter.loudness_global()?;
    let peak = meter.true_peak(0)?;
    if !loudness.is_finite() || peak <= 0.0 {
        return Ok(());
    }
    let gain_db = (TARGET_LUFS - loudness).min(MAX_PEAK_DB - 20.0 * peak.log10());
    let gain = 10f64.powf(gain_db / 20.0) as f32;
    samples.iter_mut().for_each(|s| *s *= gain);
    Ok(())
}

/// Mono 44.1 kHz CBR MP3, without the Info tag frame: the storytellers want
/// audio frames only.
fn encode(samples: &[f32]) -> Result<Vec<u8>> {
    let mut encoder = rusty_mp3::Mp3Encoder::new(rusty_mp3::Mp3EncoderConfig {
        bitrate_kbps: KBPS,
        vbr_quality: None,
    });
    encoder
        .push_pcm_f32(samples, 1, RATE)
        .map_err(|e| anyhow::anyhow!("MP3 encoding error: {e:?}"))?;
    encoder.finish();
    let mut mp3 = Vec::with_capacity(samples.len() / 5);
    let mut first = true;
    while let Ok(packet) = encoder.next_packet() {
        let tag = first && (contains(&packet, b"Info") || contains(&packet, b"Xing"));
        first = false;
        if !tag {
            mp3.extend_from_slice(&packet);
        }
    }
    if mp3.is_empty() {
        bail!("MP3 encoding produced no audio");
    }
    Ok(mp3)
}

fn contains(data: &[u8], word: &[u8]) -> bool {
    data.windows(word.len()).any(|w| w == word)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::audio::{mp3_info, transcoding_required};

    /// 16 bits PCM WAV of a 440 Hz sine wave.
    fn wav(seconds: f32, rate: u32, channels: u16, amplitude: f32) -> Vec<u8> {
        let frames = (seconds * rate as f32) as usize;
        let mut pcm = Vec::with_capacity(frames * channels as usize * 2);
        for i in 0..frames {
            let v = amplitude * (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin();
            for _ in 0..channels {
                pcm.extend_from_slice(&((v * 32767.0) as i16).to_le_bytes());
            }
        }
        let block = channels as u32 * 2;
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * block).to_le_bytes());
        out.extend_from_slice(&(block as u16).to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        out.extend(pcm);
        out
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    /// Stereo 48 kHz: a mono 44.1 kHz MP3 of the same duration and loudness,
    /// that needs no other conversion.
    #[test]
    fn stereo_48k_to_device_mp3() {
        let mp3 = to_mp3(&wav(3.0, 48_000, 2, 0.5), false).unwrap();
        let info = mp3_info(&mp3).unwrap();
        assert!(info.mono && info.sample_rate == 44_100);
        assert!(!transcoding_required("a.mp3", &mp3));
        assert!(!contains(&mp3, b"Info") && !contains(&mp3, b"Xing"));
        assert_eq!(crate::mp3::audio_duration(&mp3), Some(3));
        // decoded again: two identical channels added at -3 dB each, a sine
        // of amplitude 0.5 * √2 (RMS 0.5)
        let (back, rate) = decode(&mp3).unwrap();
        assert_eq!(rate, 44_100);
        let level = rms(&back[44_100..88_200]);
        assert!((level - 0.5).abs() < 0.03, "{level}");
        // an MP3 converted again (stereo MP3 of a STUdio story)
        let again = to_mp3(&mp3, false).unwrap();
        assert_eq!(crate::mp3::audio_duration(&again), Some(3));
    }

    /// A quiet track comes out louder once normalized, without clipping.
    #[test]
    fn normalization() {
        let quiet = wav(4.0, 44_100, 1, 0.02);
        let plain = decode(&to_mp3(&quiet, false).unwrap()).unwrap().0;
        let loud = decode(&to_mp3(&quiet, true).unwrap()).unwrap().0;
        let (before, after) = (rms(&plain), rms(&loud));
        assert!(after > before * 5.0, "{before} -> {after}");
        assert!(loud.iter().all(|s| s.abs() <= 1.0));
    }

    /// Ogg Opus file of a 440 Hz sine wave, encoded by opus-pure.
    fn opus(seconds: f32, channels: usize, amplitude: f32) -> Vec<u8> {
        use opus_pure::{Application, OggOpusWriter, OpusEncoder, OpusHead};
        let mut encoder = OpusEncoder::new(48_000, channels, Application::Audio).unwrap();
        let head = OpusHead::for_encoder(&encoder, 48_000);
        let mut writer = OggOpusWriter::new(Vec::new(), head).unwrap();
        let frames = (seconds * 48_000.0) as usize / 960;
        let mut packet = vec![0u8; 4000];
        for f in 0..frames {
            let pcm: Vec<f32> = (0..960 * channels)
                .map(|i| {
                    let t = (f * 960 + i / channels) as f32 / 48_000.0;
                    amplitude * (t * 440.0 * std::f32::consts::TAU).sin()
                })
                .collect();
            let n = encoder.encode(&pcm, 960, &mut packet).unwrap();
            writer.write_packet(&packet[..n]).unwrap();
        }
        writer.finish().unwrap()
    }

    /// Opus (mono and stereo): an MP3 of the same duration and loudness.
    #[test]
    fn opus_to_device_mp3() {
        for channels in [1, 2] {
            let mp3 = to_mp3(&opus(3.0, channels, 0.5), false).unwrap();
            let info = mp3_info(&mp3).unwrap();
            assert!(info.mono && info.sample_rate == 44_100);
            assert_eq!(crate::mp3::audio_duration(&mp3), Some(3));
            let (back, _) = decode(&mp3).unwrap();
            // a sine of amplitude 0.5 (RMS 0.35), two channels at -3 dB each
            let expected = 0.5 / 2f32.sqrt() * (channels as f32).sqrt();
            let level = rms(&back[44_100..88_200]);
            assert!((level - expected).abs() < 0.03, "{channels}: {level}");
        }
    }

    #[test]
    fn not_audio() {
        assert!(to_mp3(b"definitely not audio", false).is_err());
        assert!(to_mp3(&[0u8; 4096], false).is_err());
    }
}
