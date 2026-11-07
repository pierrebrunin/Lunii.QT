//! Track list of an album on the device (`album.json` in the story directory):
//! the screens only hold the titles as pictures, this file keeps them as text
//! to display and edit the album, with the duration of each track (measured
//! when its audio is written).

use std::path::Path;

use anyhow::{Context as _, Result};
use serde_json::{Value, json};

use super::{CHAPTER_PREFIX, COVER_SOUND, Chapter, track_asset};

/// Name of the file in the story directory. Plain JSON, not ciphered: the
/// devices ignore it, and the plain exports leave it out.
pub const FILE_ALBUM: &str = "album.json";

/// Titles of an album on the device.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlbumInfo {
    /// one title per track, empty when unknown (albums created before this
    /// file existed)
    pub tracks: Vec<String>,
    /// sub-folders (empty: a single menu with all the tracks, or unknown)
    pub chapters: Vec<Chapter>,
    /// duration of each track in seconds, measured when its audio was written
    /// (0, or missing: unknown)
    pub durations: Vec<u32>,
}

impl AlbumInfo {
    /// Duration of the whole album, if the one of every track is known.
    pub fn total_duration(&self) -> Option<u32> {
        (!self.tracks.is_empty()
            && self.durations.len() == self.tracks.len()
            && self.durations.iter().all(|&d| d > 0))
        .then(|| self.durations.iter().sum())
    }

    fn to_json(&self) -> Value {
        let mut v = json!({"version": 1, "tracks": self.tracks});
        if self.durations.iter().any(|&d| d > 0) {
            v["durations"] = json!(self.durations);
        }
        if !self.chapters.is_empty() {
            v["chapters"] = self
                .chapters
                .iter()
                .map(|c| json!({"title": c.title, "len": c.len}))
                .collect();
        }
        v
    }

    fn from_json(v: &Value) -> Option<Self> {
        let tracks = v
            .get("tracks")?
            .as_array()?
            .iter()
            .map(|t| t.as_str().unwrap_or_default().to_string())
            .collect();
        let chapters = v
            .get("chapters")
            .and_then(Value::as_array)
            .map(|cs| {
                cs.iter()
                    .filter_map(|c| {
                        Some(Chapter {
                            title: c.get("title")?.as_str()?.to_string(),
                            len: c.get("len")?.as_u64()? as usize,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let durations = v
            .get("durations")
            .and_then(Value::as_array)
            .map(|ds| ds.iter().map(|d| d.as_u64().unwrap_or(0) as u32).collect())
            .unwrap_or_default();
        Some(Self {
            tracks,
            chapters,
            durations,
        })
    }
}

/// Position of a track from the name of its audio (`TRK00001.mp3` → 0).
pub fn track_number(audio: &str) -> Option<usize> {
    let digits = audio.strip_prefix("TRK")?.strip_suffix(".mp3")?;
    if digits.len() != 5 {
        return None;
    }
    digits.parse::<usize>().ok()?.checked_sub(1)
}

/// The titles saved with the album, if any.
pub fn read(story_dir: &Path) -> Option<AlbumInfo> {
    let data = std::fs::read(story_dir.join(FILE_ALBUM)).ok()?;
    AlbumInfo::from_json(&serde_json::from_slice(&data).ok()?)
}

pub fn write(story_dir: &Path, info: &AlbumInfo) -> Result<()> {
    let path = story_dir.join(FILE_ALBUM);
    std::fs::write(&path, serde_json::to_vec_pretty(&info.to_json())?)
        .with_context(|| format!("unable to write {}", path.display()))
}

/// The titles of an album story created by Lunii, from its `story.json`
/// (`None` for other stories).
pub fn from_story_json(story: &Value) -> Option<AlbumInfo> {
    let nodes = story.get("stageNodes")?.as_array()?;
    let cover = nodes.first()?;
    if cover.get("audio")?.as_str()? != COVER_SOUND {
        return None;
    }
    // position in the name of the numbered assets (`TRK00001.mp3` → 0)
    let number = |name: &str, prefix: &str, ext: &str| -> Option<usize> {
        let digits = name.strip_prefix(prefix)?.strip_suffix(ext)?;
        if digits.len() != 5 {
            return None;
        }
        digits.parse::<usize>().ok()?.checked_sub(1)
    };
    let mut tracks: Vec<String> = Vec::new();
    let mut chapters: Vec<String> = Vec::new();
    let set = |list: &mut Vec<String>, i: usize, name: &str| {
        if list.len() <= i {
            list.resize(i + 1, String::new());
        }
        list[i] = name.to_string();
    };
    for node in nodes {
        let name = node.get("name").and_then(Value::as_str).unwrap_or_default();
        let audio = node
            .get("audio")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let image = node
            .get("image")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(i) = number(audio, "TRK", ".mp3") {
            set(&mut tracks, i, name);
        } else if let Some(c) = number(image, CHAPTER_PREFIX, ".png") {
            set(&mut chapters, c, name);
        }
    }
    if tracks.is_empty() {
        return None;
    }
    // number of tracks of each chapter: its menu (`tracks0`...)
    let actions = story.get("actionNodes").and_then(Value::as_array);
    let chapters = chapters
        .into_iter()
        .enumerate()
        .map(|(c, title)| {
            let id = format!("tracks{c}");
            let len = actions
                .and_then(|a| {
                    a.iter()
                        .find(|n| n.get("id").and_then(Value::as_str) == Some(&id))
                })
                .and_then(|n| n.get("options")?.as_array().map(Vec::len))
                .unwrap_or(0);
            Chapter { title, len }
        })
        .collect();
    Some(AlbumInfo {
        tracks,
        chapters,
        durations: Vec::new(),
    })
}

/// The tracks of an album story created by Lunii, from its files on the
/// device: the saved titles, empty titles when they are unknown. `None` for
/// other stories.
pub fn on_device(story_dir: &Path) -> Option<AlbumInfo> {
    let device_file = |dir: &str, asset: &str| {
        story_dir
            .join(dir)
            .join("000")
            .join(crate::story::normalized_name(asset))
    };
    if !device_file("sf", COVER_SOUND).is_file() {
        return None;
    }
    let count = (0..)
        .take_while(|&i| device_file("sf", &track_asset("TRK", i, "mp3")).is_file())
        .count();
    if count == 0 {
        return None;
    }
    let saved = read(story_dir).unwrap_or_default();
    let mut tracks = saved.tracks;
    tracks.resize(count, String::new());
    let mut durations = saved.durations;
    durations.resize(count, 0);
    let chapters = if saved.chapters.iter().map(|c| c.len).sum::<usize>() == count {
        saved.chapters
    } else {
        Vec::new()
    };
    Some(AlbumInfo {
        tracks,
        chapters,
        durations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::album::{chapters_story_json, story_json};
    use uuid::Uuid;

    #[test]
    fn titles_from_story_json() {
        let titles = ["Un", "Deux", "Trois"];
        let info = from_story_json(&story_json(Uuid::from_u128(1), "Album", "", &titles)).unwrap();
        assert_eq!(info.tracks, titles);
        assert!(info.chapters.is_empty());

        let chapters = [
            Chapter {
                title: "CD 1".into(),
                len: 1,
            },
            Chapter {
                title: "CD 2".into(),
                len: 2,
            },
        ];
        let json = chapters_story_json(Uuid::from_u128(1), "Album", "", &chapters, &titles);
        let info = from_story_json(&json).unwrap();
        assert_eq!(info.tracks, titles);
        assert_eq!(info.chapters, chapters);

        // a single story (podcast episode) is not an album
        let story = crate::album::SingleStory {
            uuid: Uuid::from_u128(2),
            title: "Épisode",
            description: "",
            header: "",
            footer: "",
            thumbnail: None,
            audio: Path::new("x.mp3"),
        };
        assert_eq!(from_story_json(&story.story_json()), None);
    }

    #[test]
    fn read_write() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()), None);
        let info = AlbumInfo {
            tracks: vec!["Un".into(), "Deux".into()],
            chapters: vec![Chapter {
                title: "CD".into(),
                len: 2,
            }],
            durations: vec![65, 0],
        };
        write(dir.path(), &info).unwrap();
        assert_eq!(read(dir.path()).as_ref(), Some(&info));
        // a duration unknown: no total
        assert_eq!(info.total_duration(), None);
        let known = AlbumInfo {
            durations: vec![65, 120],
            ..info
        };
        assert_eq!(known.total_duration(), Some(185));
        // files written before the durations: read without them
        std::fs::write(
            dir.path().join(FILE_ALBUM),
            r#"{"version":1,"tracks":["Un"]}"#,
        )
        .unwrap();
        assert_eq!(read(dir.path()).unwrap().durations, Vec::<u32>::new());
    }
}
