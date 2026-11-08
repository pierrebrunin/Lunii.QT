//! Download of the episodes and preparation of their import.

use super::*;

/// Podcast image (album cover), if any.
pub fn download_cover(podcast: &Podcast) -> Option<Vec<u8>> {
    let url = podcast.image_url.as_deref()?;
    let resp = http::agent(Duration::from_secs(20)).get(url).call().ok()?;
    http::bytes(resp).ok()
}

/// Downloads the episodes into `dir`, converted to mono 44.1 kHz MP3 when
/// needed or with the volume normalized, and returns them as album
/// tracks. `on_episode(i)` is called
/// before each one.
pub fn download_episodes(
    ctx: &Context,
    episodes: &[Episode],
    dir: &Path,
    opts: AudioOptions,
    mut on_episode: impl FnMut(usize),
) -> Result<Vec<Track>> {
    std::fs::create_dir_all(dir)?;
    let mut tracks = Vec::with_capacity(episodes.len());
    for (i, ep) in episodes.iter().enumerate() {
        check_abort(ctx)?;
        on_episode(i);
        ctx.info(format!("⬇ {} ({}/{})", ep.title, i + 1, episodes.len()));
        let source = dir.join(format!("episode{i:04}.src"));
        crate::tools::download(
            ctx,
            &ep.url,
            &source,
            &format!("⬇ {}/{}", i + 1, episodes.len()),
        )
        .with_context(|| format!("unable to download '{}'", ep.title))?;
        let data = std::fs::read(&source)?;
        let _ = std::fs::remove_file(&source);
        check_abort(ctx)?;

        let mp3 = if opts.normalize() {
            ctx.file_progress("⌛ MP3", 0, 0);
            let mp3 = audio::normalized_mp3(&data)
                .with_context(|| format!("unable to convert '{}'", ep.title))?;
            ctx.file_progress("", 0, 0);
            mp3
        } else if audio::transcoding_required(&ep.url, &data) {
            ctx.file_progress("⌛ MP3", 0, 0);
            let mp3 = audio::audio_to_mp3(&data)
                .with_context(|| format!("unable to convert '{}'", ep.title))?;
            ctx.file_progress("", 0, 0);
            mp3
        } else {
            data
        };
        let path = dir.join(format!("episode{i:04}.mp3"));
        std::fs::write(&path, &mp3)?;
        tracks.push(Track {
            path,
            title: ep.title.clone(),
            disc: None,
            number: None,
        });
    }
    Ok(tracks)
}

/// `2026-07-24` → `24/07/2026`.
pub fn display_date(iso: &str) -> String {
    match iso.split('-').collect::<Vec<_>>()[..] {
        [y, m, d] => format!("{d}/{m}/{y}"),
        _ => String::new(),
    }
}

/// A title usable as a file name (shown in the import messages).
/// Archive of the episode `index` imported as a story: numbered, as titles
/// may be the same, or start alike (cut to 60 characters).
pub(super) fn story_zip_name(index: usize, title: &str) -> String {
    format!("{:04} {}.zip", index + 1, file_name(title))
}

pub(super) fn file_name(title: &str) -> String {
    let name: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_'".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    match name.trim() {
        "" => "episode".into(),
        n => n.to_string(),
    }
}

/// What to do on the device to import episodes (see [`prepare_import`]).
#[derive(Debug)]
pub enum ImportPlan {
    /// one STUdio archive per episode, to import
    Stories(Vec<PathBuf>),
    /// the STUdio archive of a new album, to import
    NewAlbum(PathBuf),
    /// tracks to append to the album already on the device
    Append(Uuid, Vec<Track>),
}

/// How to import episodes (see [`prepare_import`]).
#[derive(Debug, Clone, Copy)]
pub struct ImportMode {
    /// one story per episode instead of the podcast album
    pub as_stories: bool,
    /// the podcast album is on the device: the episodes are appended to it
    pub album_on_device: bool,
    pub audio: AudioOptions,
}

/// Downloads `episodes` into `dir` and prepares their import.
/// `on_episode(i)` is called before each download.
pub fn prepare_import(
    ctx: &Context,
    podcast: &Podcast,
    episodes: &[Episode],
    mode: ImportMode,
    dir: &Path,
    on_episode: impl FnMut(usize),
) -> Result<ImportPlan> {
    let tracks = download_episodes(ctx, episodes, dir, mode.audio, on_episode)?;

    if mode.as_stories {
        let cover = download_cover(podcast);
        let mut archives = Vec::with_capacity(tracks.len());
        for (i, (ep, track)) in episodes.iter().zip(&tracks).enumerate() {
            let zip = dir.join(story_zip_name(i, &ep.title));
            album::SingleStory {
                uuid: podcast.episode_uuid(ep),
                title: &ep.title,
                description: &podcast.title,
                header: &podcast.title,
                footer: &display_date(&ep.date),
                thumbnail: cover.as_deref(),
                audio: &track.path,
            }
            .write_studio_zip(&zip)?;
            archives.push(zip);
        }
        return Ok(ImportPlan::Stories(archives));
    }
    let uuid = podcast.album_uuid();
    if mode.album_on_device {
        return Ok(ImportPlan::Append(uuid, tracks));
    }
    let album = album::Album {
        uuid: Some(uuid),
        title: podcast.title.clone(),
        artist: podcast.author.clone(),
        cover: download_cover(podcast),
        tracks,
        chapters: Vec::new(),
    };
    let zip = dir.join("album.zip");
    album.write_studio_zip(&zip)?;
    Ok(ImportPlan::NewAlbum(zip))
}

/// Remembers the episodes imported with `plan` in the podcast album.
pub fn record_plan(podcast: &Podcast, episodes: &[Episode], plan: &ImportPlan) -> Result<()> {
    let guids: Vec<String> = episodes.iter().map(|e| e.guid.clone()).collect();
    match plan {
        ImportPlan::Stories(_) => Ok(()),
        ImportPlan::NewAlbum(_) => record_imported(podcast, &guids, true),
        ImportPlan::Append(..) => record_imported(podcast, &guids, false),
    }
}

/// Episodes in the album of a podcast, as its `album.json` on the device
/// records them. `None` for an album imported before they were recorded
/// there: see [`imported_episodes`] (this computer only).
pub fn album_episodes(info: &album::meta::AlbumInfo) -> Option<HashSet<String>> {
    info.podcast.as_ref()?;
    Some(
        info.episodes
            .iter()
            .filter(|e| !e.is_empty())
            .cloned()
            .collect(),
    )
}

/// Records in `album.json` on the device the feed of the podcast and the
/// episodes of the tracks just imported with `plan` (the last ones of the
/// album). An album imported before: its earlier tracks matched by title to
/// the episodes this computer remembers.
pub fn record_on_device(
    dev: &dyn crate::device::Device,
    podcast: &Podcast,
    episodes: &[Episode],
    plan: &ImportPlan,
) -> Result<()> {
    if matches!(plan, ImportPlan::Stories(_)) {
        return Ok(());
    }
    let uuid = podcast.album_uuid();
    let story = dev
        .stories()
        .get(&uuid.to_string())
        .context("This story is not present on your storyteller")?;
    let dir = dev.story_path(story);
    let mut info =
        album::meta::on_device(&dir).context("This story is not an MP3 album created by Lunii")?;
    let count = info.tracks.len();
    info.episodes.resize(count, String::new());
    if info.podcast.is_none() {
        let known = imported_episodes(&uuid);
        for (title, guid) in info.tracks.iter().zip(info.episodes.iter_mut()) {
            if guid.is_empty()
                && let Some(ep) = podcast
                    .episodes
                    .iter()
                    .find(|e| e.title == *title && known.contains(&e.guid))
            {
                *guid = ep.guid.clone();
            }
        }
    }
    let first = count.saturating_sub(episodes.len());
    for (slot, ep) in info.episodes[first..].iter_mut().zip(episodes) {
        *slot = ep.guid.clone();
    }
    info.podcast = Some(album::meta::PodcastSource {
        feed: podcast.feed_url.clone(),
        aliases: podcast.aliases.clone(),
    });
    album::meta::write(&dir, &info)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names() {
        assert_eq!(display_date("2026-07-24"), "24/07/2026");
        assert_eq!(display_date(""), "");
        assert_eq!(file_name("L'Île / au trésor ?"), "L'Île _ au trésor _");
        assert_eq!(file_name("  "), "episode");
        // same titles: different archives
        assert_ne!(story_zip_name(0, "Bonus"), story_zip_name(1, "Bonus"));
        assert_eq!(story_zip_name(9, "Bonus"), "0010 Bonus.zip");
    }
}
