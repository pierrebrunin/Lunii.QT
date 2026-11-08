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
//! Every node has a sound, as in the official stories: the cover and the menu
//! entries share one second of silence (nothing is heard while turning the
//! wheel).
//!
//! A folder without MP3 files but with sub-folders of MP3 files (CDs,
//! chapters, seasons...) becomes a story with two menus: the sub-folders (the
//! chapters), then their tracks. At the end of the last track of a chapter, the
//! next chapter starts. Tracks can't be appended to such albums.
//!
//! Titles and cover come from the ID3 tags (file name / folder name otherwise,
//! `cover.jpg`, `folder.jpg`... for the cover),
//! the screens are generated (320x240, gray levels). The generated archive goes
//! through the regular STUdio import: ciphering, transcoding to mono 44.1 kHz,
//! thumbnail.

use std::io::Write;
use std::path::{Path, PathBuf};

use ab_glyph::FontRef;
use anyhow::{Context as _, Result};
use image::GrayImage;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::constants::{FILE_STUDIO_JSON, FILE_STUDIO_THUMB};
use crate::convert::audio::{self, AudioOptions, silent_mp3};
use crate::report::{Context, check_abort};

mod layout;
pub mod meta;
mod scan;
mod screens;

pub use layout::{chapters_story_json, story_json};
pub use scan::{chapter_dirs, folder_cover, is_album_dir, mp3_files, scan, scan_files};
pub use screens::font;
use screens::*;

/// Asset names: the devices use 8 characters file names (`ri` / `si` hold
/// fixed 12 bytes entries, `000\XXXXXXXX`).
pub(super) const COVER_IMAGE: &str = "IMGCOVER.png";
pub(super) const COVER_SOUND: &str = "SNDCOVER.mp3";

/// Audio of a single story: not named like the album tracks (`TRK00001`), so
/// it isn't taken for an album (see [`device_track_count`]).
const EPISODE_SOUND: &str = "EPISODE1.mp3";

/// Screen of a chapter menu entry (`CHP00001`): only in albums made of
/// sub-folders, see [`device_track_count`].
pub(super) const CHAPTER_PREFIX: &str = "CHP";

pub(crate) fn track_asset(prefix: &str, index: usize, ext: &str) -> String {
    format!("{prefix}{:05}.{ext}", index + 1)
}

/// Namespace of the album story UUIDs (v5): the same album gives the same UUID,
/// so importing it twice is detected.
pub(crate) const ALBUM_NAMESPACE: Uuid = Uuid::from_u128(0x4c75_6e69_6951_5441_6c62_756d_4d50_3300);

#[derive(Debug, Clone)]
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub disc: Option<u32>,
    pub number: Option<u32>,
}

/// A sub-folder of an album: `len` tracks, following the previous chapters'
/// ones in [`Album::tracks`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    pub title: String,
    pub len: usize,
}

#[derive(Debug, Clone)]
pub struct Album {
    /// forced story UUID (podcasts: derived from the feed), see [`Album::uuid`]
    pub uuid: Option<Uuid>,
    pub title: String,
    pub artist: Option<String>,
    /// front cover found in the tags (any image format)
    pub cover: Option<Vec<u8>>,
    pub tracks: Vec<Track>,
    /// sub-folders (empty: a single menu with all the tracks)
    pub chapters: Vec<Chapter>,
}

impl Album {
    /// Stable UUID: the forced one, or from the album title, artist and track
    /// titles.
    pub fn uuid(&self) -> Uuid {
        if let Some(uuid) = self.uuid {
            return uuid;
        }
        let mut key = format!("{}\n{}", self.title, self.artist.as_deref().unwrap_or(""));
        for t in &self.tracks {
            key.push('\n');
            key.push_str(&t.title);
        }
        for c in &self.chapters {
            key.push_str("\nchapter ");
            key.push_str(&c.title);
        }
        Uuid::new_v5(&ALBUM_NAMESPACE, key.as_bytes())
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
        let titles: Vec<&str> = self.tracks.iter().map(|t| t.title.as_str()).collect();
        if self.chapters.is_empty() {
            return story_json(self.uuid(), &self.title, &self.description(), &titles);
        }
        chapters_story_json(
            self.uuid(),
            &self.title,
            &self.description(),
            &self.chapters,
            &titles,
        )
    }

    /// For each track: its chapter (index) and its number in the chapter.
    fn track_positions(&self) -> Vec<(Option<usize>, usize)> {
        if self.chapters.is_empty() {
            return (0..self.tracks.len()).map(|i| (None, i + 1)).collect();
        }
        self.chapters
            .iter()
            .enumerate()
            .flat_map(|(c, ch)| (0..ch.len).map(move |i| (Some(c), i + 1)))
            .collect()
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

    /// Writes the STUdio archive (`story.json`, `thumbnail.png`, `assets/`).
    pub fn write_studio_zip(&self, out: &Path) -> Result<()> {
        let font = font()?;
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
        for (c, chapter) in self.chapters.iter().enumerate() {
            let screen = text_screen(&font, &self.title, &chapter.title, &(c + 1).to_string());
            add(
                &format!("assets/{}", track_asset(CHAPTER_PREFIX, c, "png")),
                &png(&screen)?,
            )?;
        }
        for (i, (track, (chapter, number))) in
            self.tracks.iter().zip(self.track_positions()).enumerate()
        {
            let header = chapter.map_or(self.title.as_str(), |c| self.chapters[c].title.as_str());
            for asset in numbered_track_assets(&font, header, track, i, number)? {
                add(&format!("assets/{}", asset.name), &asset.data)?;
            }
        }
        zip.finish()?;
        Ok(())
    }
}

/// A story made of a single track (a podcast episode): its screen, OK plays
/// the track, then the device goes back to the story selection.
pub struct SingleStory<'a> {
    pub uuid: Uuid,
    pub title: &'a str,
    pub description: &'a str,
    /// small text above the title on the screen (podcast name)
    pub header: &'a str,
    /// small text below the title (date)
    pub footer: &'a str,
    /// picture of the story in the application and the Flam list (any image
    /// format), the screen otherwise
    pub thumbnail: Option<&'a [u8]>,
    pub audio: &'a Path,
}

impl SingleStory<'_> {
    pub fn story_json(&self) -> Value {
        let play = Uuid::new_v5(&self.uuid, b"play").to_string();
        let controls = |wheel, ok, home, pause, autoplay| json!({"wheel": wheel, "ok": ok, "home": home, "pause": pause, "autoplay": autoplay});
        json!({
            "format": "v1",
            "version": 1,
            "title": self.title,
            "description": self.description,
            "nightModeAvailable": false,
            "stageNodes": [
                {
                    "uuid": self.uuid.to_string(),
                    "type": "cover",
                    "name": self.title,
                    "image": COVER_IMAGE,
                    "audio": COVER_SOUND,
                    "okTransition": {"actionNode": "play", "optionIndex": 0},
                    "homeTransition": null,
                    "controlSettings": controls(true, true, false, false, false),
                    "squareOne": true,
                },
                {
                    "uuid": play,
                    "name": self.title,
                    "image": COVER_IMAGE,
                    "audio": EPISODE_SOUND,
                    // end of the track: back to the story selection
                    "okTransition": null,
                    "homeTransition": null,
                    "controlSettings": controls(false, true, true, true, true),
                },
            ],
            "actionNodes": [{"id": "play", "options": [play]}],
        })
    }

    /// Writes the STUdio archive.
    pub fn write_studio_zip(&self, out: &Path) -> Result<()> {
        let font = font()?;
        let screen = png(&text_screen(&font, self.header, self.title, self.footer))?;
        let audio = track_audio(self.audio)?;
        let file = std::fs::File::create(out)
            .with_context(|| format!("unable to create {}", out.display()))?;
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let thumbnail = self.thumbnail.unwrap_or(&screen);
        for (name, data) in [
            (
                FILE_STUDIO_JSON.to_string(),
                serde_json::to_vec_pretty(&self.story_json())?,
            ),
            (FILE_STUDIO_THUMB.to_string(), thumbnail.to_vec()),
            (format!("assets/{COVER_IMAGE}"), screen.clone()),
            (format!("assets/{COVER_SOUND}"), silent_mp3(1.0)),
            (format!("assets/{EPISODE_SOUND}"), audio),
        ] {
            zip.start_file(name, opts)?;
            zip.write_all(&data)?;
        }
        zip.finish()?;
        Ok(())
    }
}

/// A track of an album being edited on the device (see
/// [`crate::device::Device::edit_album`]).
#[derive(Debug, Clone)]
pub enum AlbumEntry {
    /// the track at this position (0 based) on the device
    Existing(usize),
    /// an MP3 file to add
    New(Track),
}

/// A file of a track, in its source format (PNG screen, MP3 sounds), named as
/// in the STUdio archive.
pub struct Asset {
    pub name: String,
    pub data: Vec<u8>,
}

impl Asset {
    pub fn is_image(&self) -> bool {
        self.name.ends_with(".png")
    }
}

/// Screen and audio of the track at `index` (0 based).
pub fn track_assets(
    font: &FontRef,
    album_title: &str,
    track: &Track,
    index: usize,
) -> Result<Vec<Asset>> {
    numbered_track_assets(font, album_title, track, index, index + 1)
}

/// Screen (`header` above the title, `number` below) and audio of the track
/// at `index` (0 based).
fn numbered_track_assets(
    font: &FontRef,
    header: &str,
    track: &Track,
    index: usize,
    number: usize,
) -> Result<Vec<Asset>> {
    let audio = track_audio(&track.path)?;
    Ok(vec![
        Asset {
            name: track_asset("IMG", index, "png"),
            data: png(&track_screen(font, header, &track.title, number))?,
        },
        Asset {
            name: track_asset("TRK", index, "mp3"),
            data: audio,
        },
    ])
}

/// Screen of the track at `index` (0 based): `header` above the title, the
/// number below.
pub fn track_screen_asset(
    font: &FontRef,
    header: &str,
    title: &str,
    index: usize,
) -> Result<Asset> {
    Ok(Asset {
        name: track_asset("IMG", index, "png"),
        data: png(&track_screen(font, header, title, index + 1))?,
    })
}

fn track_audio(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("unable to read {}", path.display()))
}

/// The tracks, processed as `opts` requires: with the volume normalized,
/// converted copies written in `dir` (the titles are kept).
pub fn prepare_tracks(
    ctx: &Context,
    tracks: &[Track],
    opts: AudioOptions,
    dir: &Path,
) -> Result<Vec<Track>> {
    if !opts.normalize() {
        return Ok(tracks.to_vec());
    }
    std::fs::create_dir_all(dir)?;
    let mut out = Vec::with_capacity(tracks.len());
    for (i, track) in tracks.iter().enumerate() {
        check_abort(ctx)?;
        ctx.info(format!("🔊 Normalizing the volume : {}", track.title));
        ctx.file_progress("⌛ MP3", i as u64, tracks.len() as u64);
        let data = audio::normalized_mp3(&track_audio(&track.path)?).with_context(|| {
            format!("unable to normalize the volume of {}", track.path.display())
        })?;
        let path = dir.join(format!("normalized{i:05}.mp3"));
        std::fs::write(&path, data)?;
        out.push(Track {
            path,
            ..track.clone()
        });
    }
    ctx.file_progress("", 0, 0);
    Ok(out)
}

/// Number of tracks of an album story created by Lunii, from its files on
/// the device (`sf/000/SNDCOVER`, `sf/000/TRK00001`...). `None` for other
/// stories, and for the albums made of chapters (`rf/000/CHP00001`): tracks
/// can't be appended to them.
pub fn device_track_count(story_dir: &Path) -> Option<usize> {
    let sf = story_dir.join("sf").join("000");
    let device_name = |asset: &str| crate::story::normalized_name(asset);
    if !sf.join(device_name(COVER_SOUND)).is_file()
        || story_dir
            .join("rf")
            .join("000")
            .join(device_name(&track_asset(CHAPTER_PREFIX, 0, "png")))
            .is_file()
    {
        return None;
    }
    let count = (0..)
        .take_while(|&i| {
            sf.join(device_name(&track_asset("TRK", i, "mp3")))
                .is_file()
        })
        .count();
    (count > 0).then_some(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::image::{LUNII_H, LUNII_W};
    use crate::story::StudioStory;

    fn album(n: usize) -> Album {
        Album {
            uuid: None,
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
            chapters: Vec::new(),
        }
    }

    #[test]
    fn chapters_story_json_is_valid_studio() {
        let mut a = album(5);
        a.chapters = vec![
            Chapter {
                title: "CD 1".into(),
                len: 2,
            },
            Chapter {
                title: "CD 2".into(),
                len: 3,
            },
        ];
        assert_ne!(a.uuid(), album(5).uuid());
        let s = StudioStory::load(&a.story_json()).unwrap();
        // cover + 2 chapter screens + 5 track screens; one silence + 5 tracks
        assert_eq!(s.ri.len(), 8);
        assert_eq!(s.si.len(), 6);
        let ni = s.ni_data().unwrap();
        // cover + 2 chapters + 5 menu entries + 5 playback nodes
        assert_eq!(u32::from_le_bytes(ni[12..16].try_into().unwrap()), 13);
        assert_eq!(
            a.track_positions(),
            [
                (Some(0), 1),
                (Some(0), 2),
                (Some(1), 1),
                (Some(1), 2),
                (Some(1), 3)
            ]
        );
    }

    #[test]
    fn story_json_is_valid_studio() {
        let a = album(3);
        let s = StudioStory::load(&a.story_json()).unwrap();
        assert_eq!(s.uuid, a.uuid());
        // cover + 3 track screens; one silence (cover + menu) + 3 tracks
        assert_eq!(s.ri.len(), 4);
        assert_eq!(s.si.len(), 4);
        // all transitions resolve
        s.ni_data().unwrap();
        // fixed size entries: `000\` + 8 characters
        assert_eq!(s.ri_data().len(), 12 * s.ri.len());
        assert_eq!(s.si_data().len(), 12 * s.si.len());
    }

    #[test]
    fn single_story_is_valid_studio() {
        let story = SingleStory {
            uuid: Uuid::from_u128(42),
            title: "Un épisode",
            description: "Podcast",
            header: "Podcast",
            footer: "24/07/2026",
            thumbnail: None,
            audio: Path::new("x.mp3"),
        };
        let s = StudioStory::load(&story.story_json()).unwrap();
        assert_eq!(s.uuid, story.uuid);
        assert_eq!((s.ri.len(), s.si.len()), (1, 2));
        let ni = s.ni_data().unwrap();
        assert_eq!(u32::from_le_bytes(ni[12..16].try_into().unwrap()), 2);
        assert_eq!(s.si_data().len(), 24);
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
        for img in [
            a.cover_image(&font),
            track_screen(&font, &a.title, &a.tracks[11].title, 12),
        ] {
            assert_eq!(img.dimensions(), (LUNII_W, LUNII_H));
            let lit = img.pixels().filter(|p| p[0] > 128).count();
            assert!(lit > 500, "{lit} lit pixels");
        }
    }
}
