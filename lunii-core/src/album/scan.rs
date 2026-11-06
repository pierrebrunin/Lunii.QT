//! Reading a folder of MP3 files: tags, order, chapters (sub-folders), cover.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use id3::TagLike;

use super::{Album, Chapter, Track};
use crate::constants::FILE_STUDIO_JSON;

pub(super) fn is_mp3(path: &Path) -> bool {
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

/// Sub-folders of `dir` holding MP3 files, in natural order (`CD 2` before
/// `CD 10`).
pub fn chapter_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !mp3_files(p).is_empty())
        .collect();
    dirs.sort_by_cached_key(|d| natural_key(&d.file_name().unwrap_or_default().to_string_lossy()));
    dirs
}

/// Sort key comparing the numbers in a name by value.
pub(super) fn natural_key(name: &str) -> Vec<(String, u64)> {
    let mut key = Vec::new();
    let mut text = String::new();
    let mut chars = name.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            let mut n = c.to_digit(10).unwrap() as u64;
            while let Some(d) = chars.peek().and_then(|d| d.to_digit(10)) {
                n = n.saturating_mul(10).saturating_add(d as u64);
                chars.next();
            }
            key.push((std::mem::take(&mut text), n));
        } else {
            text.extend(c.to_lowercase());
        }
    }
    key.push((text, u64::MAX));
    key
}

/// A folder is an MP3 album if it holds MP3 files (or sub-folders of MP3
/// files) and is not a story folder.
pub fn is_album_dir(dir: &Path) -> bool {
    dir.is_dir()
        && !dir.join(FILE_STUDIO_JSON).exists()
        && !dir.join("ni").exists()
        && !dir.join("uuid.bin").exists()
        && (!mp3_files(dir).is_empty() || !chapter_dirs(dir).is_empty())
}

/// Cover image file of a folder: `cover`, `folder`, `front`, `album` (jpg,
/// png...), or its only image.
pub fn folder_cover(dir: &Path) -> Option<Vec<u8>> {
    const NAMES: [&str; 4] = ["cover", "folder", "front", "album"];
    const EXTS: [&str; 6] = ["jpg", "jpeg", "png", "webp", "bmp", "gif"];
    let entries = std::fs::read_dir(dir).ok()?;
    let mut images: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|e| EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)))
        })
        .collect();
    images.sort();
    let stem = |p: &Path| {
        p.file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    let chosen = NAMES
        .iter()
        .find_map(|n| images.iter().find(|p| stem(p) == *n))
        .or_else(|| (images.len() == 1).then(|| &images[0]))?;
    let data = std::fs::read(chosen).ok()?;
    image::load_from_memory(&data).is_ok().then_some(data)
}

pub(super) fn clean(s: Option<&str>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub(super) fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().replace('_', " ").trim().to_string())
        .unwrap_or_default()
}

pub(super) fn dir_name(dir: &Path) -> Option<String> {
    dir.file_name()
        .map(|n| n.to_string_lossy().trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Reads the tags of the MP3 files of `dir` and sorts them (disc, track
/// number, file name). Without MP3 files in `dir`, its sub-folders of MP3
/// files are the chapters of the album.
pub fn scan(dir: &Path) -> Result<Album> {
    let files = mp3_files(dir);
    if files.is_empty() {
        return scan_chapters(dir);
    }
    let mut album = scan_files(&files)?;
    if album.title.is_empty() {
        album.title = dir_name(dir).unwrap_or_else(|| "Album".into());
    }
    if album.cover.is_none() {
        album.cover = folder_cover(dir);
    }
    Ok(album)
}

pub(super) fn scan_chapters(dir: &Path) -> Result<Album> {
    let dirs = chapter_dirs(dir);
    if dirs.is_empty() {
        bail!("no MP3 file in {}", dir.display());
    }
    let mut parts = Vec::with_capacity(dirs.len());
    for d in &dirs {
        parts.push((d, scan_files(&mp3_files(d))?));
    }
    // the tag title if all the chapters share it (CDs of one album), the
    // folder name otherwise (several albums of an artist...)
    let first_title = &parts[0].1.title;
    let title = if !first_title.is_empty() && parts.iter().all(|(_, a)| &a.title == first_title) {
        first_title.clone()
    } else {
        dir_name(dir).unwrap_or_else(|| "Album".into())
    };
    let artist = parts.iter().find_map(|(_, a)| a.artist.clone());
    let cover = folder_cover(dir)
        .or_else(|| parts.iter().find_map(|(_, a)| a.cover.clone()))
        .or_else(|| dirs.iter().find_map(|d| folder_cover(d)));
    let mut tracks = Vec::new();
    let mut chapters = Vec::with_capacity(parts.len());
    for (d, part) in parts {
        chapters.push(Chapter {
            title: dir_name(d).unwrap_or_default(),
            len: part.tracks.len(),
        });
        tracks.extend(part.tracks);
    }
    Ok(Album {
        uuid: None,
        title,
        artist,
        cover,
        tracks,
        chapters,
    })
}

/// Reads the tags of MP3 files and sorts them (disc, track number, file name).
/// The album title is empty when no tag gives it.
pub fn scan_files(files: &[PathBuf]) -> Result<Album> {
    let mut album_title = None;
    let mut artist = None;
    let mut cover = None;
    let mut tracks = Vec::with_capacity(files.len());
    for path in files.iter().cloned() {
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

    Ok(Album {
        uuid: None,
        title: album_title.unwrap_or_default(),
        artist,
        cover,
        tracks,
        chapters: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut names = vec!["CD 10", "cd 2", "CD 1", "Bonus"];
        names.sort_by_cached_key(|n| natural_key(n));
        assert_eq!(names, ["Bonus", "CD 1", "cd 2", "CD 10"]);
    }
}
