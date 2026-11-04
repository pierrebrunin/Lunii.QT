//! MP3 album → STUdio story, to play any MP3 files (music, audio books…) on
//! the devices.
//!
//! A folder of MP3 files becomes one story:
//!
//! ```text
//! cover ──OK──▶ menu (wheel: one entry per track, the screen shows its title)
//!                 └─OK──▶ track i ──end / OK──▶ track i+1 ── … ──▶ back to the menu
//!                           └─HOME──▶ back to the menu, on track i
//! ```
//!
//! Every node has a sound, the firmware expects one: one second of silence for
//! the cover, the first seconds of the track for the menu entries (heard while
//! turning the wheel).
//!
//! Titles and cover come from the ID3 tags (file name / folder name otherwise),
//! the screens are generated (320x240, gray levels). The generated archive goes
//! through the regular STUdio import: ciphering, transcoding to mono 44.1 kHz
//! (ffmpeg), thumbnail.

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use anyhow::{Context as _, Result, bail};
use id3::TagLike;
use image::{DynamicImage, GrayImage, ImageFormat, Luma, imageops::FilterType};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::constants::{FILE_STUDIO_JSON, FILE_STUDIO_THUMB};
use crate::convert::audio::{mp3_excerpt, silent_mp3};
use crate::convert::image::{LUNII_H, LUNII_W};

/// Asset names: the devices use 8 characters file names (`ri` / `si` hold
/// fixed 12 bytes entries, `000\XXXXXXXX`).
const COVER_IMAGE: &str = "IMGCOVER.png";
const COVER_SOUND: &str = "SNDCOVER.mp3";

fn track_asset(prefix: &str, index: usize, ext: &str) -> String {
    format!("{prefix}{:05}.{ext}", index + 1)
}

/// Length of the excerpt played on a menu entry.
const EXCERPT_SECONDS: f32 = 5.0;

const FONT: &[u8] = include_bytes!("../../../res/fonts/DejaVuSans-Bold.ttf");

/// Namespace of the album story UUIDs (v5): the same album gives the same UUID,
/// so importing it twice is detected.
const ALBUM_NAMESPACE: Uuid = Uuid::from_u128(0x4c75_6e69_6951_5441_6c62_756d_4d50_3300);

#[derive(Debug, Clone)]
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub disc: Option<u32>,
    pub number: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct Album {
    pub title: String,
    pub artist: Option<String>,
    /// front cover found in the tags (any image format)
    pub cover: Option<Vec<u8>>,
    pub tracks: Vec<Track>,
}

fn is_mp3(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("mp3"))
}

/// MP3 files directly inside `dir` (not recursive).
pub fn mp3_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_mp3(p))
        .collect();
    files.sort();
    files
}

/// A folder is an MP3 album if it holds MP3 files and is not a story folder.
pub fn is_album_dir(dir: &Path) -> bool {
    dir.is_dir()
        && !dir.join(FILE_STUDIO_JSON).exists()
        && !dir.join("ni").exists()
        && !dir.join("uuid.bin").exists()
        && !mp3_files(dir).is_empty()
}

fn clean(s: Option<&str>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().replace('_', " ").trim().to_string())
        .unwrap_or_default()
}

/// Reads the tags of the MP3 files of `dir` and sorts them (disc, track
/// number, file name).
pub fn scan(dir: &Path) -> Result<Album> {
    let files = mp3_files(dir);
    if files.is_empty() {
        bail!("no MP3 file in {}", dir.display());
    }

    let mut album_title = None;
    let mut artist = None;
    let mut cover = None;
    let mut tracks = Vec::with_capacity(files.len());
    for path in files {
        let tag = id3::Tag::read_from_path(&path).ok();
        let title = tag.as_ref().and_then(|t| clean(t.title()));
        if let Some(t) = &tag {
            album_title = album_title.or_else(|| clean(t.album()));
            artist = artist.or_else(|| clean(t.album_artist()).or_else(|| clean(t.artist())));
            if cover.is_none() {
                let mut pictures: Vec<_> = t.pictures().collect();
                // the front cover first
                pictures.sort_by_key(|p| p.picture_type != id3::frame::PictureType::CoverFront);
                cover = pictures.first().map(|p| p.data.clone());
            }
        }
        tracks.push(Track {
            title: title.unwrap_or_else(|| file_stem(&path)),
            disc: tag.as_ref().and_then(|t| t.disc()),
            number: tag.as_ref().and_then(|t| t.track()),
            path,
        });
    }
    tracks.sort_by(|a, b| {
        (a.disc.unwrap_or(0), a.number.unwrap_or(u32::MAX), &a.path).cmp(&(
            b.disc.unwrap_or(0),
            b.number.unwrap_or(u32::MAX),
            &b.path,
        ))
    });

    let title = album_title
        .or_else(|| {
            dir.file_name()
                .map(|n| n.to_string_lossy().trim().to_string())
        })
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Album".into());
    Ok(Album {
        title,
        artist,
        cover,
        tracks,
    })
}

impl Album {
    /// Stable UUID: album title, artist and track titles.
    pub fn uuid(&self) -> Uuid {
        let mut key = format!("{}\n{}", self.title, self.artist.as_deref().unwrap_or(""));
        for t in &self.tracks {
            key.push('\n');
            key.push_str(&t.title);
        }
        Uuid::new_v5(&ALBUM_NAMESPACE, key.as_bytes())
    }

    /// Some track is not a mono 44.1 kHz MP3: ffmpeg is needed.
    pub fn needs_transcoding(&self) -> bool {
        self.tracks.iter().any(|t| {
            std::fs::read(&t.path)
                .map(|data| crate::convert::audio::transcoding_required("a.mp3", &data))
                .unwrap_or(true)
        })
    }

    /// Size of the audio files, for the remaining time estimation.
    pub fn audio_size(&self) -> u64 {
        self.tracks
            .iter()
            .filter_map(|t| std::fs::metadata(&t.path).ok())
            .map(|m| m.len())
            .sum()
    }

    /// The STUdio `story.json`.
    pub fn story_json(&self) -> Value {
        let n = self.tracks.len();
        // stable node UUIDs, derived from the story one
        let story_uuid = self.uuid();
        let node =
            |kind: &str, i: usize| Uuid::new_v5(&story_uuid, format!("{kind}{i}").as_bytes());
        let menu_uuid = |i| node("menu", i).to_string();
        let play_uuid = |i| node("play", i).to_string();
        let controls = |wheel, ok, home, pause, autoplay| json!({"wheel": wheel, "ok": ok, "home": home, "pause": pause, "autoplay": autoplay});
        let to =
            |action: String, option: usize| json!({"actionNode": action, "optionIndex": option});

        let mut stage_nodes = vec![json!({
            "uuid": story_uuid.to_string(),
            "type": "cover",
            "name": self.title,
            "image": COVER_IMAGE,
            "audio": COVER_SOUND,
            "okTransition": to("menu".into(), 0),
            "homeTransition": null,
            "controlSettings": controls(true, true, false, false, false),
            "squareOne": true,
        })];
        for (i, track) in self.tracks.iter().enumerate() {
            stage_nodes.push(json!({
                "uuid": menu_uuid(i),
                "name": track.title,
                "image": track_asset("IMG", i, "png"),
                "audio": track_asset("EXC", i, "mp3"),
                "okTransition": to(format!("play{i}"), 0),
                "homeTransition": null,
                "controlSettings": controls(true, true, true, false, false),
            }));
        }
        for (i, track) in self.tracks.iter().enumerate() {
            // end of the track (or OK): next one, the menu after the last
            let next = if i + 1 < n {
                to(format!("play{}", i + 1), 0)
            } else {
                to("menu".into(), 0)
            };
            stage_nodes.push(json!({
                "uuid": play_uuid(i),
                "name": track.title,
                "image": track_asset("IMG", i, "png"),
                "audio": track_asset("TRK", i, "mp3"),
                "okTransition": next,
                "homeTransition": to("menu".into(), i),
                "controlSettings": controls(false, true, true, true, true),
            }));
        }

        let mut action_nodes = vec![json!({
            "id": "menu",
            "options": (0..n).map(menu_uuid).collect::<Vec<_>>(),
        })];
        for i in 0..n {
            action_nodes.push(json!({"id": format!("play{i}"), "options": [play_uuid(i)]}));
        }

        json!({
            "format": "v1",
            "version": 1,
            "title": self.title,
            "description": self.description(),
            "nightModeAvailable": false,
            "stageNodes": stage_nodes,
            "actionNodes": action_nodes,
        })
    }

    fn description(&self) -> String {
        let count = format!("{} MP3", self.tracks.len());
        match &self.artist {
            Some(a) => format!("{a} - {count}"),
            None => count,
        }
    }

    /// Cover screen: the tag cover, or a generated one.
    fn cover_image(&self, font: &FontRef) -> GrayImage {
        if let Some(img) = self
            .cover
            .as_deref()
            .and_then(|c| image::load_from_memory(c).ok())
        {
            return fit_cover(&img);
        }
        let mut img = blank();
        draw_text_block(&mut img, font, &self.title, 20, 30, 190, 40.0);
        if let Some(artist) = &self.artist {
            draw_text_block(&mut img, font, artist, 20, 190, 40, 20.0);
        }
        img
    }

    /// Screen of a track: its title, the album and the position.
    fn track_image(&self, font: &FontRef, index: usize) -> GrayImage {
        let mut img = blank();
        draw_text_block(&mut img, font, &self.title, 10, 6, 28, 16.0);
        draw_text_block(&mut img, font, &self.tracks[index].title, 16, 44, 152, 36.0);
        let pos = format!("{} / {}", index + 1, self.tracks.len());
        draw_text_block(&mut img, font, &pos, 10, 204, 30, 20.0);
        img
    }

    /// Writes the STUdio archive (`story.json`, `thumbnail.png`, `assets/`).
    pub fn write_studio_zip(&self, out: &Path) -> Result<()> {
        let font = FontRef::try_from_slice(FONT).context("embedded font")?;
        let file = std::fs::File::create(out)
            .with_context(|| format!("unable to create {}", out.display()))?;
        let mut zip = zip::ZipWriter::new(file);
        // MP3 and PNG don't compress
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        let mut add = |name: &str, data: &[u8]| -> Result<()> {
            zip.start_file(name, opts)?;
            zip.write_all(data)?;
            Ok(())
        };

        add(
            FILE_STUDIO_JSON,
            &serde_json::to_vec_pretty(&self.story_json())?,
        )?;
        let cover = png(&self.cover_image(&font))?;
        add(FILE_STUDIO_THUMB, &cover)?;
        add(&format!("assets/{COVER_IMAGE}"), &cover)?;
        add(&format!("assets/{COVER_SOUND}"), &silent_mp3(1.0))?;
        for (i, track) in self.tracks.iter().enumerate() {
            add(
                &format!("assets/{}", track_asset("IMG", i, "png")),
                &png(&self.track_image(&font, i))?,
            )?;
            let audio = std::fs::read(&track.path)
                .with_context(|| format!("unable to read {}", track.path.display()))?;
            let excerpt = mp3_excerpt(&audio, EXCERPT_SECONDS).unwrap_or_else(|| silent_mp3(1.0));
            add(
                &format!("assets/{}", track_asset("EXC", i, "mp3")),
                &excerpt,
            )?;
            add(&format!("assets/{}", track_asset("TRK", i, "mp3")), &audio)?;
        }
        zip.finish()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Screens
// ---------------------------------------------------------------------------

const BACKGROUND: u8 = 0;
const FOREGROUND: u8 = 255;
const MARGIN: u32 = 12;

fn blank() -> GrayImage {
    GrayImage::from_pixel(LUNII_W, LUNII_H, Luma([BACKGROUND]))
}

fn png(img: &GrayImage) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png)?;
    Ok(out.into_inner())
}

/// Cover scaled to fit the screen, centered on a black background.
fn fit_cover(img: &DynamicImage) -> GrayImage {
    let scaled = img
        .resize(LUNII_W, LUNII_H, FilterType::CatmullRom)
        .to_luma8();
    let mut out = blank();
    let x = (LUNII_W - scaled.width()) / 2;
    let y = (LUNII_H - scaled.height()) / 2;
    image::imageops::overlay(&mut out, &scaled, x.into(), y.into());
    out
}

fn text_width(font: &FontRef, scale: PxScale, text: &str) -> f32 {
    let f = font.as_scaled(scale);
    let mut width = 0.0;
    let mut prev = None;
    for c in text.chars() {
        let id = f.glyph_id(c);
        if let Some(p) = prev {
            width += f.kern(p, id);
        }
        width += f.h_advance(id);
        prev = Some(id);
    }
    width
}

/// Word wrapping; a word too long for a line is cut.
fn wrap(font: &FontRef, scale: PxScale, text: &str, max_width: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };
        if text_width(font, scale, &candidate) <= max_width {
            line = candidate;
            continue;
        }
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        // the word alone may still be too long
        for c in word.chars() {
            line.push(c);
            if text_width(font, scale, &line) > max_width && line.chars().count() > 1 {
                let last = line.pop().unwrap();
                lines.push(std::mem::take(&mut line));
                line.push(last);
            }
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Draws `text` centered in the band `[top, top + height)`, as big as possible
/// up to `max_size` px (and down to 12 px, then lines are dropped with "…").
fn draw_text_block(
    img: &mut GrayImage,
    font: &FontRef,
    text: &str,
    max_lines: usize,
    top: u32,
    height: u32,
    max_size: f32,
) {
    let max_width = (LUNII_W - 2 * MARGIN) as f32;
    let mut size = max_size;
    let (scale, mut lines) = loop {
        let scale = PxScale::from(size);
        let lines = wrap(font, scale, text, max_width);
        let line_h = font.as_scaled(scale).height();
        let fits = lines.len() <= max_lines && line_h * lines.len() as f32 <= height as f32;
        if fits || size <= 12.0 {
            break (scale, lines);
        }
        size -= 2.0;
    };
    let f = font.as_scaled(scale);
    let line_h = f.height();
    let max_fit = ((height as f32 / line_h).floor() as usize).clamp(1, max_lines);
    if lines.len() > max_fit {
        lines.truncate(max_fit);
        if let Some(last) = lines.last_mut() {
            while !last.is_empty() && text_width(font, scale, &format!("{last}…")) > max_width {
                last.pop();
            }
            last.push('…');
        }
    }

    let block_h = line_h * lines.len() as f32;
    let mut y = top as f32 + (height as f32 - block_h).max(0.0) / 2.0;
    for line in &lines {
        let x = (LUNII_W as f32 - text_width(font, scale, line)) / 2.0;
        draw_line(img, font, scale, line, x, y + f.ascent());
        y += line_h;
    }
}

fn draw_line(
    img: &mut GrayImage,
    font: &FontRef,
    scale: PxScale,
    text: &str,
    x: f32,
    baseline: f32,
) {
    let f = font.as_scaled(scale);
    let mut caret = x;
    let mut prev = None;
    for c in text.chars() {
        let id = f.glyph_id(c);
        if let Some(p) = prev {
            caret += f.kern(p, id);
        }
        let glyph = id.with_scale_and_position(scale, point(caret, baseline));
        caret += f.h_advance(id);
        prev = Some(id);
        let Some(outline) = font.outline_glyph(glyph) else {
            continue;
        };
        let bounds = outline.px_bounds();
        outline.draw(|gx, gy, coverage| {
            let px = bounds.min.x as i32 + gx as i32;
            let py = bounds.min.y as i32 + gy as i32;
            if px < 0 || py < 0 || px >= LUNII_W as i32 || py >= LUNII_H as i32 {
                return;
            }
            let p = img.get_pixel_mut(px as u32, py as u32);
            let v = p[0] as f32 + (FOREGROUND as f32 - p[0] as f32) * coverage.clamp(0.0, 1.0);
            p[0] = v.round() as u8;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::story::StudioStory;

    fn album(n: usize) -> Album {
        Album {
            title: "Un très long titre d'album pour tester le retour à la ligne".into(),
            artist: Some("Artiste".into()),
            cover: None,
            tracks: (0..n)
                .map(|i| Track {
                    path: PathBuf::from(format!("{i}.mp3")),
                    title: format!("Piste numéro {}", i + 1),
                    disc: None,
                    number: Some(i as u32 + 1),
                })
                .collect(),
        }
    }

    #[test]
    fn story_json_is_valid_studio() {
        let a = album(3);
        let s = StudioStory::load(&a.story_json(), false).unwrap();
        assert!(s.compatible);
        assert_eq!(s.uuid, a.uuid());
        // cover + 3 track screens; cover silence + 3 excerpts + 3 tracks
        assert_eq!(s.ri.len(), 4);
        assert_eq!(s.si.len(), 7);
        // all transitions resolve
        s.ni_data().unwrap();
        // fixed size entries: `000\` + 8 characters
        assert_eq!(s.ri_data().len(), 12 * s.ri.len());
        assert_eq!(s.si_data().len(), 12 * s.si.len());
    }

    #[test]
    fn uuid_is_stable() {
        assert_eq!(album(2).uuid(), album(2).uuid());
        assert_ne!(album(2).uuid(), album(3).uuid());
    }

    #[test]
    fn screens_have_text() {
        let font = FontRef::try_from_slice(FONT).unwrap();
        let a = album(12);
        for img in [a.cover_image(&font), a.track_image(&font, 11)] {
            assert_eq!(img.dimensions(), (LUNII_W, LUNII_H));
            let lit = img.pixels().filter(|p| p[0] > 128).count();
            assert!(lit > 500, "{lit} lit pixels");
        }
    }

    #[test]
    fn wrapping() {
        let font = FontRef::try_from_slice(FONT).unwrap();
        let lines = wrap(
            &font,
            PxScale::from(36.0),
            "Un titre plutôt long pour un écran",
            296.0,
        );
        assert!(lines.len() > 1);
        assert!(
            lines
                .iter()
                .all(|l| text_width(&font, PxScale::from(36.0), l) <= 296.0)
        );
    }
}
