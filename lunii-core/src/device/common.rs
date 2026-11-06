//! Helpers shared by Lunii and Flam devices.

use std::fs::File;
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context as _, Result, bail};
use unicode_normalization::UnicodeNormalization;
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;

use crate::report::{Context, check_abort};

/// Makes a story title usable as a file name (ASCII only, reserved chars replaced).
pub fn secure_filename(filename: &str) -> String {
    const INVALID_FILE_CHARS: &str = "/\\?%*:|\"<>";
    filename
        .nfkd()
        .map(|c| match c {
            '\u{0141}' => 'L',
            '\u{0142}' => 'l',
            c if INVALID_FILE_CHARS.contains(c) => '_',
            c => c,
        })
        .filter(char::is_ascii)
        .collect()
}

/// Thousands separator like Python's `{:,}`.
pub fn group_thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Writes `data` in 10KB chunks, reporting speed and progress every 250ms.
pub fn write_with_progress(ctx: &Context, target: &Path, data: &[u8]) -> Result<()> {
    const TIME_SPAN_S: f64 = 0.250;
    const BLOCK_SIZE: usize = 10 * 1024;

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let fname = target
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    let total = data.len() as u64;

    let mut f =
        File::create(target).with_context(|| format!("unable to create {}", target.display()))?;
    let mut written = 0u64;
    let mut last_written = 0u64;
    let mut last_emit = Instant::now();

    for chunk in data.chunks(BLOCK_SIZE) {
        f.write_all(chunk)?;
        written += chunk.len() as u64;
        ctx.add_written(chunk.len() as u64);
        let elapsed = last_emit.elapsed().as_secs_f64();
        if elapsed >= TIME_SPAN_S || written == total {
            let speed = if elapsed > 0.0 {
                ((written - last_written) as f64 / elapsed) as u64 / 1024
            } else {
                0
            };
            ctx.file_progress(&format!("{} KB/s", group_thousands(speed)), written, total);
            ctx.debug(format!(
                "Progress on {fname} - {} / {} Bytes ( {} KB/s )",
                group_thousands(written),
                group_thousands(total),
                group_thousands(speed)
            ));
            last_emit = Instant::now();
            last_written = written;
        }
    }
    f.flush()?;
    Ok(())
}

/// Writes a whole file, creating its parent directory.
pub fn write_file(target: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(target, data).with_context(|| format!("unable to write {}", target.display()))
}

/// Names of the sub-directories of `dir` (`None` if `dir` does not exist).
pub fn list_subdirs(dir: &Path) -> Option<Vec<String>> {
    let rd = std::fs::read_dir(dir).ok()?;
    Some(
        rd.filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect(),
    )
}

/// Total size of the files inside `dir`.
pub fn dir_size(dir: &Path) -> u64 {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// All the files under `dir`, as (absolute path, relative path with `/` separators).
pub fn walk_files(dir: &Path) -> Vec<(PathBuf, String)> {
    WalkDir::new(dir)
        .sort_by_file_name()
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            let rel = e
                .path()
                .strip_prefix(dir)
                .unwrap_or(e.path())
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join("/");
            (e.into_path(), rel)
        })
        .collect()
}

/// Free space available on the file system holding `path`.
pub fn free_space(path: &Path) -> Result<u64> {
    Ok(fs4::available_space(path)?)
}

/// Removes the given directories (if they exist).
pub fn remove_dirs(dirs: &[PathBuf]) -> Result<()> {
    for d in dirs {
        if d.is_dir() {
            std::fs::remove_dir_all(d)
                .with_context(|| format!("unable to remove {}", d.display()))?;
        }
    }
    Ok(())
}

/// Moves `src` directory inside `dst_parent` (like `shutil.move(src, dst_parent)`).
pub fn move_dir_into(src: &Path, dst_parent: &Path) -> Result<()> {
    std::fs::create_dir_all(dst_parent)?;
    let name = src.file_name().context("invalid source directory")?;
    let dst = dst_parent.join(name);
    if dst.exists() {
        anyhow::bail!("Destination path '{}' already exists", dst.display());
    }
    if std::fs::rename(src, &dst).is_err() {
        // cross device move: copy then delete
        copy_dir(src, &dst)?;
        std::fs::remove_dir_all(src)?;
    }
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Zip writer (stored, like Python's `zipfile.ZipFile(path, 'w')`), removing the
/// partial file if not finished.
pub struct StoryZip {
    path: PathBuf,
    writer: Option<zip::ZipWriter<File>>,
}

impl StoryZip {
    pub fn create(path: &Path) -> Result<Self> {
        let file = File::create(path)
            .with_context(|| format!("failed to create ZIP - {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            writer: Some(zip::ZipWriter::new(file)),
        })
    }

    pub fn add(&mut self, name: &str, data: &[u8]) -> Result<()> {
        let w = self.writer.as_mut().unwrap();
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(data.len() as u64 >= 0xFFFF_FFFF);
        w.start_file(name, options)?;
        w.write_all(data)?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<PathBuf> {
        let mut f = self.writer.take().unwrap().finish()?;
        f.flush()?;
        let _ = f.stream_position();
        Ok(self.path.clone())
    }
}

impl Drop for StoryZip {
    fn drop(&mut self) {
        if self.writer.take().is_some() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Iterates over a list, reporting story progress and checking abort requests.
pub fn progress_iter<'a, T>(
    ctx: &'a Context,
    short_uuid: &'a str,
    items: &'a [T],
) -> impl Iterator<Item = Result<(usize, &'a T)>> + 'a {
    items.iter().enumerate().map(move |(i, item)| {
        ctx.story_progress(short_uuid, i, items.len());
        check_abort(ctx)?;
        Ok((i, item))
    })
}

/// Python's `str.removesuffix`.
pub fn remove_suffix<'a>(s: &'a str, suffix: &str) -> &'a str {
    s.strip_suffix(suffix).unwrap_or(s)
}

/// Splits a `/` path into (dirname, basename) like `os.path` (posix).
pub fn split_path(file: &str) -> (&str, &str) {
    match file.rfind('/') {
        Some(p) => {
            let dir = file[..p].trim_end_matches('/');
            (
                if dir.is_empty() && p > 0 {
                    &file[..1]
                } else {
                    dir
                },
                &file[p + 1..],
            )
        }
        None => ("", file),
    }
}

pub fn join_path(dir: &str, base: &str) -> String {
    if dir.is_empty() {
        base.to_string()
    } else if dir.ends_with('/') {
        format!("{dir}{base}")
    } else {
        format!("{dir}/{base}")
    }
}

/// Python slicing `s[n:]` on an ASCII-ish string.
pub fn skip_chars(s: &str, n: usize) -> String {
    s.chars().skip(n).collect()
}

/// Appends `tracks` to an MP3 album story created by Lunii (see
/// [`crate::album`]): writes the screens and audio of the new tracks after the
/// existing ones, and returns the story of the whole album, whose index files
/// (`ri`, `si`, `li`, `ni`) the caller writes last: until then (abort,
/// error...) the album stays as it was.
///
/// `device_path(name, is_image)` gives the file path in the story directory of
/// a normalized asset name, `cipher(asset, data)` the data to write.
pub(crate) fn write_album_tracks(
    ctx: &Context,
    story_dir: &Path,
    story: &crate::story::Story,
    tracks: &[crate::album::Track],
    device_path: impl Fn(&str, bool) -> String,
    cipher: impl Fn(&str, &[u8]) -> Result<Vec<u8>>,
) -> Result<crate::story::StudioStory> {
    use crate::{album, convert::image, story};

    let count = album::device_track_count(story_dir)
        .context("This story is not an MP3 album created by Lunii")?;
    if tracks.is_empty() {
        bail!("No MP3 file to add");
    }
    let transcoding = crate::constants::transcoding_supported();
    if album::needs_transcoding(tracks) && !transcoding {
        bail!(
            "These MP3 files must be converted to mono 44.1 kHz: FFMPEG is required (Tools menu)"
        );
    }

    let font = album::font()?;
    let title = story.name();
    let short = story.short_uuid();
    let total = tracks.len() * 2;
    for (k, track) in tracks.iter().enumerate() {
        let index = count + k;
        ctx.info(format!("🎵 Track {} : {}", index + 1, track.title));
        for (a, asset) in album::track_assets(&font, &title, track, index)?
            .into_iter()
            .enumerate()
        {
            ctx.story_progress(&short, k * 2 + a, total);
            check_abort(ctx)?;
            let source_len = asset.data.len() as u64;
            let path = device_path(&story::normalized_name(&asset.name), asset.is_image());
            let data = if asset.is_image() {
                image::image_to_bitmap_rle4(&asset.data)?
            } else {
                convert_studio_audio(ctx, &asset.name, &path, asset.data, transcoding)?
            };
            let ciphered = cipher(&asset.name, &data)?;
            write_with_progress(ctx, &story_dir.join(&path), &ciphered)?;
            ctx.count_as_source(source_len, ciphered.len() as u64);
        }
    }

    let mut titles: Vec<String> = (1..=count).map(|i| i.to_string()).collect();
    titles.extend(tracks.iter().map(|t| t.title.clone()));
    let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
    story::StudioStory::load(&album::story_json(story.uuid, &title, "", &titles), true)
}

/// Checks a story once written: every screen and sound referenced by the
/// index files is on the device and not empty, and the index files read back
/// as written (`index`: name in the story directory, ciphered content). Catches
/// a device unplugged or full during the import.
pub(crate) fn verify_story_files(
    ctx: &Context,
    story_dir: &Path,
    studio: &crate::story::StudioStory,
    index: &[(&str, Vec<u8>)],
) -> Result<()> {
    ctx.info("Checking the written files...");
    let assets = studio
        .ri
        .values()
        .map(|(name, _)| ("rf", name))
        .chain(studio.si.values().map(|(name, _)| ("sf", name)));
    for (dir, name) in assets {
        let path = story_dir.join(dir).join("000").join(name);
        let ok = std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() > 0);
        anyhow::ensure!(
            ok,
            "Check after writing failed: {dir}/000/{name} is missing or empty"
        );
    }
    for (name, expected) in index {
        let data = std::fs::read(story_dir.join(name)).unwrap_or_default();
        anyhow::ensure!(
            &data == expected,
            "Check after writing failed: the index file '{name}' differs from what was written"
        );
    }
    Ok(())
}

/// Albums created before v3.1.4 played an excerpt of the tracks in the menu
/// (`EXC00001`...): no longer referenced once the index files are regenerated,
/// removed.
pub(crate) fn remove_album_excerpts(story_dir: &Path, track_count: usize) {
    for i in 0..track_count {
        let excerpt = crate::story::normalized_name(&format!("EXC{:05}.mp3", i + 1));
        let _ = std::fs::remove_file(story_dir.join("sf").join("000").join(excerpt));
    }
}

/// Transcodes / cleans a STUdio audio asset when necessary.
pub(crate) fn convert_studio_audio(
    ctx: &Context,
    file: &str,
    new_name: &str,
    mut data: Vec<u8>,
    transcoding: bool,
) -> Result<Vec<u8>> {
    if crate::convert::audio::transcoding_required(file, &data) {
        if !transcoding {
            bail!(
                "STUdio story with non MP3 audio file. You need FFMPEG tool to import such kind of story, refer to README.md"
            );
        }
        ctx.warn(format!(
            "⌛ Transcoding audio {new_name} : {} KB ...",
            data.len() / 1024
        ));
        ctx.file_progress("⌛ FFMPEG", 0, 0);
        data = crate::convert::audio::audio_to_mp3(&data)?;
        ctx.file_progress("", 0, 0);
    }
    if crate::convert::audio::tags_removal_required(&data) {
        ctx.warn(format!("⌛ Removing tags from audio {new_name}"));
        data = crate::convert::audio::mp3_tag_cleanup(&data);
    }
    Ok(data)
}

/// Registers the metadata / thumbnail of a third-party plain archive.
pub(crate) fn import_plain_metadata(
    archive: &crate::archive::Archive,
    uuid: &uuid::Uuid,
) -> Result<()> {
    if archive.contains(crate::constants::FILE_META) {
        let meta: serde_json::Value =
            serde_json::from_slice(&archive.read(crate::constants::FILE_META)?)?;
        let meta_uuid = meta
            .get("uuid")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if meta_uuid.to_uppercase() != crate::db::db_key(uuid) {
            bail!("Metadata UUID mismatch ({meta_uuid})");
        }
        crate::db::thirdparty_add_story(
            uuid,
            meta.get("title").and_then(serde_json::Value::as_str),
            meta.get("description").and_then(serde_json::Value::as_str),
        )?;
    }
    if archive.contains(crate::constants::FILE_THUMB) {
        crate::db::thirdparty_add_thumb(uuid, &archive.read(crate::constants::FILE_THUMB)?)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Writing STUdio stories (shared by Lunii and Flam)
// ---------------------------------------------------------------------------

/// What differs between the devices when writing a story made of STUdio
/// assets (STUdio archives, MP3 albums): the ciphering and the authorization
/// files. The rest of the import is [`import_studio`] / [`append_album_tracks`].
pub(crate) trait StoryWriter: super::Device {
    /// Ciphers a story file: an asset (named as in the archive) or an index
    /// file (named by its path).
    fn cipher_story_file(&self, file: &str, data: &[u8]) -> Result<Vec<u8>>;

    /// Writes the authorization and metadata files of a story, once its index
    /// files are written (`ri`: written content of `ri`). `new_story`: `false`
    /// when tracks were appended to an album.
    fn seal_story(
        &mut self,
        ctx: &Context,
        story_dir: &Path,
        story: &crate::story::Story,
        ri: &[u8],
        new_story: bool,
    ) -> Result<()>;
}

/// Path of a STUdio asset in a story directory, from its normalized name
/// (8 characters, upper case): `rf/000/XXXXXXXX` (screens), `sf/000/...` (sounds).
pub(crate) fn studio_asset_path(name: &str, is_image: bool) -> String {
    format!("{}/000/{name}", if is_image { "rf" } else { "sf" })
}

/// Writes the `ri`, `si`, `li` and `ni` files of a story (`ri` first),
/// returns their written (ciphered) content.
pub(crate) fn write_index_files<D: StoryWriter + ?Sized>(
    dev: &D,
    story_dir: &Path,
    studio: &crate::story::StudioStory,
) -> Result<Vec<(&'static str, Vec<u8>)>> {
    let mut written = Vec::with_capacity(4);
    for (name, data) in [
        ("ri", studio.ri_data()),
        ("si", studio.si_data()),
        ("li", studio.li_data()),
        ("ni", studio.ni_data()?),
    ] {
        let path = story_dir.join(name);
        let ciphered = dev.cipher_story_file(&path.to_string_lossy(), &data)?;
        write_file(&path, &ciphered)?;
        written.push((name, ciphered));
    }
    Ok(written)
}

/// Imports a STUdio archive (`story.json` + `assets/`): screens turned into
/// bitmaps, sounds converted when needed, everything ciphered for `dev`, then
/// the index files, the authorization files and a check of the written files.
pub(crate) fn import_studio<D: StoryWriter>(dev: &mut D, ctx: &Context, path: &Path) -> Result<()> {
    use crate::constants::{FILE_STUDIO_JSON, FILE_STUDIO_THUMB, FILE_UUID};
    use crate::story::{Story, StudioStory};

    let archive = crate::archive::Archive::open(path)?;
    if archive.contains(FILE_UUID) {
        bail!("plain.pk format detected ! Unable to add this story.");
    }
    if !archive.contains(FILE_STUDIO_JSON) {
        bail!("missing 'story.json'. Unable to add this story.");
    }
    let story_json: serde_json::Value = serde_json::from_slice(&archive.read(FILE_STUDIO_JSON)?)?;
    let transcoding = crate::constants::transcoding_supported();
    let studio = StudioStory::load(&story_json, transcoding)?;
    if !studio.compatible {
        bail!(
            "STUdio story with non MP3 audio file. You need FFMPEG tool to import such kind of story, refer to README.md"
        );
    }

    crate::db::thirdparty_add_story(&studio.uuid, Some(&studio.title), Some(&studio.description))?;
    dev.check_not_loaded(&studio.uuid)?;

    let short = studio.short_uuid();
    let story_dir = dev.content_dir().join(dev.story_dir_name(&studio.uuid));
    std::fs::create_dir_all(&story_dir)?;

    let files = archive.file_names();
    for (index, entry) in files.iter().enumerate() {
        ctx.story_progress(&short, index, files.len());
        if ctx.is_aborted() {
            return Err(dev.abort_cleanup(ctx, &studio.uuid));
        }
        if entry.ends_with(FILE_STUDIO_JSON) {
            continue;
        }
        if entry.ends_with(FILE_STUDIO_THUMB) {
            crate::db::thirdparty_add_thumb(&studio.uuid, &archive.read(entry)?)?;
            continue;
        }
        if !entry.starts_with("assets") {
            continue;
        }

        let data = archive.read(entry)?;
        let source_len = data.len() as u64;
        let file = skip_chars(entry, 7);
        let (new_name, data) = if let Some((name, _)) = studio.ri.get(&file) {
            (
                studio_asset_path(name, true),
                crate::convert::image::image_to_bitmap_rle4(&data)?,
            )
        } else if let Some((name, _)) = studio.si.get(&file) {
            let new_name = studio_asset_path(name, false);
            let data = convert_studio_audio(ctx, &file, &new_name, data, transcoding)?;
            (new_name, data)
        } else {
            continue;
        };

        let ciphered = dev.cipher_story_file(&file, &data)?;
        ctx.debug(format!("File {}/{} > {new_name}", index + 1, files.len()));
        write_with_progress(ctx, &story_dir.join(&new_name), &ciphered)?;
        ctx.count_as_source(source_len, ciphered.len() as u64);
    }

    let index = write_index_files(dev, &story_dir, &studio)?;
    if studio.nm {
        ctx.info("Night mode file creation...");
        std::fs::write(story_dir.join("nm"), b"")?;
    }
    let story = Story::with_flags(studio.uuid, false, studio.nm);
    dev.seal_story(ctx, &story_dir, &story, &index[0].1, true)?;
    if let Err(e) = verify_story_files(ctx, &story_dir, &studio, &index) {
        // a damaged story would not play: removed, to be imported again
        let _ = dev.clean_up_story_dir(&studio.uuid);
        return Err(e);
    }
    dev.stories_mut().push(story);
    dev.update_pack_index()
}

/// Appends MP3 files to an album story created by Lunii (see
/// [`write_album_tracks`]), returns the number of added tracks.
pub(crate) fn append_album_tracks<D: StoryWriter>(
    dev: &mut D,
    ctx: &Context,
    uuid: &uuid::Uuid,
    tracks: &[crate::album::Track],
) -> Result<usize> {
    let story = dev
        .stories()
        .get(&uuid.to_string())
        .cloned()
        .context("This story is not present on your storyteller")?;
    let story_dir = dev.story_path(&story);
    let studio = write_album_tracks(
        ctx,
        &story_dir,
        &story,
        tracks,
        studio_asset_path,
        |a, d| dev.cipher_story_file(a, d),
    )?;

    // index files last
    let index = write_index_files(dev, &story_dir, &studio)?;
    dev.seal_story(ctx, &story_dir, &story, &index[0].1, false)?;
    remove_album_excerpts(&story_dir, studio.ri.len());
    verify_story_files(ctx, &story_dir, &studio, &index)
        .context("The album may be damaged, remove it and import it again")?;
    Ok(tracks.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn story_check() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context::default();
        let titles = ["Un", "Deux"];
        let json = crate::album::story_json(uuid::Uuid::from_u128(7), "Album", "", &titles);
        let studio = crate::story::StudioStory::load(&json, false).unwrap();
        for (sub, map) in [("rf", &studio.ri), ("sf", &studio.si)] {
            for (name, _) in map.values() {
                write_file(&dir.path().join(sub).join("000").join(name), b"data").unwrap();
            }
        }
        write_file(&dir.path().join("ri"), &studio.ri_data()).unwrap();
        let index = [("ri", studio.ri_data())];
        verify_story_files(&ctx, dir.path(), &studio, &index).unwrap();

        // an index file truncated
        write_file(&dir.path().join("ri"), &studio.ri_data()[..10]).unwrap();
        assert!(verify_story_files(&ctx, dir.path(), &studio, &index).is_err());
        write_file(&dir.path().join("ri"), &studio.ri_data()).unwrap();
        // an empty sound
        let (name, _) = studio.si.values().last().unwrap();
        write_file(&dir.path().join("sf/000").join(name), b"").unwrap();
        let err = verify_story_files(&ctx, dir.path(), &studio, &index).unwrap_err();
        assert!(err.to_string().contains(name.as_str()), "{err}");
    }

    #[test]
    fn secure() {
        assert_eq!(
            secure_filename("Le Château: été / hiver?"),
            "Le Chateau_ ete _ hiver_"
        );
        assert_eq!(secure_filename("Łódź"), "Lodz");
    }

    #[test]
    fn thousands() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(1234567), "1,234,567");
        assert_eq!(group_thousands(123), "123");
    }

    #[test]
    fn paths() {
        assert_eq!(split_path("rf/000/abc"), ("rf/000", "abc"));
        assert_eq!(split_path("abc"), ("", "abc"));
        assert_eq!(join_path("", "abc"), "abc");
    }
}
