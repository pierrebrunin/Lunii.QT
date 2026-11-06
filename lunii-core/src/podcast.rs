//! Podcasts (RSS feeds) → MP3 albums.
//!
//! A podcast becomes an album story (see [`crate::album`]), its episodes the
//! tracks, in chronological order. The album UUID is derived from the feed
//! address: the episodes of a podcast already on the device are appended to its
//! album. The episodes already imported are remembered in
//! `~/.lunii/podcasts.json`.
//!
//! The address can be the feed itself or a web page announcing it (Radio
//! France podcast pages, `<link rel="alternate" type="application/rss+xml">`),
//! or a Radio France series or selection page, without feed: its episodes are
//! then read from their own pages.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::album::{self, ALBUM_NAMESPACE, Track};
use crate::constants::{cfg_dir, ensure_cfg_dir};
use crate::convert::audio::{self, AudioOptions};
use crate::http;
use crate::report::{Context, check_abort};

#[derive(Debug, Clone)]
pub struct Episode {
    /// `<guid>`, or the audio address
    pub guid: String,
    pub title: String,
    /// publication date, `YYYY-MM-DD` (empty if unknown)
    pub date: String,
    pub duration: Option<u32>,
    pub url: String,
    pub size: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Podcast {
    pub feed_url: String,
    pub title: String,
    pub author: Option<String>,
    pub image_url: Option<String>,
    /// oldest first
    pub episodes: Vec<Episode>,
}

impl Podcast {
    /// UUID of the album of this podcast.
    pub fn album_uuid(&self) -> Uuid {
        album_uuid(&self.feed_url)
    }

    /// UUID of the story of an episode.
    pub fn episode_uuid(&self, episode: &Episode) -> Uuid {
        episode_uuid(&self.feed_url, &episode.guid)
    }
}

pub fn album_uuid(feed_url: &str) -> Uuid {
    Uuid::new_v5(&ALBUM_NAMESPACE, format!("podcast\n{feed_url}").as_bytes())
}

/// UUID of the story of an episode (one story per episode): stable, so the
/// episodes already on a device are recognized.
pub fn episode_uuid(feed_url: &str, guid: &str) -> Uuid {
    Uuid::new_v5(
        &ALBUM_NAMESPACE,
        format!("podcast episode\n{feed_url}\n{guid}").as_bytes(),
    )
}

// ---------------------------------------------------------------------------
// Feed
// ---------------------------------------------------------------------------

fn get_text(url: &str) -> Result<String> {
    let resp = http::agent(Duration::from_secs(30))
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (Lunii)")
        .call()
        .with_context(|| format!("unable to load {url}"))?;
    let data = http::bytes(resp)?;
    Ok(String::from_utf8_lossy(&data).into_owned())
}

fn looks_like_feed(body: &str) -> bool {
    let head: String = body.chars().take(2048).collect();
    head.contains("<rss") || head.contains("<feed")
}

/// Feed address announced by a web page.
fn feed_in_page(page: &str) -> Option<String> {
    // Radio France pages embed the feed address
    if let Some(start) = page.find("https://radiofrance-podcast.net/") {
        let rest = &page[start..];
        if let Some(end) = rest.find(".xml") {
            let url = &rest[..end + 4];
            if !url.contains(['"', '\'', '<', '>', ' ', '\\']) {
                return Some(url.to_string());
            }
        }
    }
    // standard feed announcement
    let mut rest = page;
    while let Some(pos) = rest.find("application/rss+xml") {
        let tag_start = rest[..pos].rfind('<').unwrap_or(0);
        let tag_end = rest[pos..].find('>').map_or(rest.len(), |e| pos + e);
        let tag = &rest[tag_start..tag_end];
        if let Some(h) = tag.find("href=") {
            let value = &tag[h + 5..];
            let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'');
            let href = match quote {
                Some(q) => value[1..].split(q).next(),
                None => value.split([' ', '>']).next(),
            };
            if let Some(href) = href.filter(|h| h.starts_with("http")) {
                return Some(href.to_string());
            }
        }
        rest = &rest[pos + 1..];
    }
    None
}

/// Loads a podcast from its feed address, or from a page announcing it.
pub fn fetch(address: &str) -> Result<Podcast> {
    let address = address.trim();
    if !address.starts_with("http://") && !address.starts_with("https://") {
        bail!("The podcast address must start with https://");
    }
    let body = get_text(address)?;
    if looks_like_feed(&body) {
        return parse_feed(address, &body);
    }
    match feed_in_page(&body) {
        Some(feed_url) => parse_feed(&feed_url, &get_text(&feed_url)?),
        None => page_podcast(address, &body),
    }
}

/// Loads a followed podcast from its address: a feed, or a page listing its
/// episodes (see [`page_podcast`]).
pub fn parse_feed_url(feed_url: &str) -> Result<Podcast> {
    let body = get_text(feed_url)?;
    if looks_like_feed(&body) {
        parse_feed(feed_url, &body)
    } else {
        page_podcast(feed_url, &body)
    }
}

// ---------------------------------------------------------------------------
// Pages without feed (Radio France series and selections)
// ---------------------------------------------------------------------------

/// The schema.org objects of a page (`<script type="application/ld+json">`,
/// `@graph` flattened).
fn json_ld(page: &str) -> Vec<Value> {
    const START: &str = "<script type=\"application/ld+json\"";
    let mut objects = Vec::new();
    let mut rest = page;
    while let Some(pos) = rest.find(START) {
        rest = &rest[pos + START.len()..];
        let Some(open) = rest.find('>') else { break };
        let Some(close) = rest.find("</script>") else {
            break;
        };
        if open < close
            && let Ok(value) = serde_json::from_str::<Value>(&rest[open + 1..close])
        {
            match value.get("@graph").and_then(Value::as_array) {
                Some(graph) => objects.extend(graph.iter().cloned()),
                None => objects.push(value),
            }
        }
        rest = &rest[close..];
    }
    objects
}

fn of_type<'a>(objects: &'a [Value], kind: &str) -> impl Iterator<Item = &'a Value> {
    objects
        .iter()
        .filter(move |o| o.get("@type").and_then(Value::as_str) == Some(kind))
}

fn str_field(object: &Value, name: &str) -> Option<String> {
    object
        .get(name)
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// `content` of `<meta property="og:{name}">`.
fn og_meta(page: &str, name: &str) -> Option<String> {
    let tag = format!("<meta property=\"og:{name}\" content=\"");
    let start = page.find(&tag)? + tag.len();
    let value = &page[start..start + page[start..].find('"')?];
    Some(value.trim().to_string()).filter(|v| !v.is_empty())
}

/// ISO 8601 duration (`P0Y0M0DT0H22M50S`) in seconds.
fn parse_iso_duration(value: &str) -> Option<u32> {
    let time = value.split_once('T')?.1;
    let mut total = 0;
    let mut number = String::new();
    for c in time.chars() {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        let n: u32 = number.parse().ok()?;
        number.clear();
        total += n * match c {
            'H' => 3600,
            'M' => 60,
            'S' => 1,
            _ => return None,
        };
    }
    Some(total)
}

/// The episode described by an episode page (`RadioEpisode` and its audio).
fn page_episode(url: &str, page: &str) -> Option<Episode> {
    let objects = json_ld(page);
    let episode = of_type(&objects, "RadioEpisode").find(|e| {
        e.pointer("/mainEntity/contentUrl")
            .and_then(Value::as_str)
            .is_some()
    })?;
    let audio = episode.get("mainEntity")?;
    // "Title : épisode 1/4 du podcast ... | France Culture": the article headline
    let title = of_type(&objects, "NewsArticle")
        .find_map(|a| str_field(a, "headline"))
        .or_else(|| {
            str_field(episode, "name").map(|n| n.split(" | ").next().unwrap_or(&n).to_string())
        })
        .unwrap_or_else(|| "?".into());
    Some(Episode {
        guid: url.to_string(),
        title,
        date: str_field(episode, "dateCreated")
            .map(|d| d.chars().take(10).collect())
            .unwrap_or_default(),
        duration: str_field(audio, "duration").and_then(|d| parse_iso_duration(&d)),
        url: str_field(audio, "contentUrl")?,
        size: None,
    })
}

/// The pages listed by a page (`ItemList`), in order.
fn listed_pages(page: &str) -> Vec<String> {
    of_type(&json_ld(page), "ItemList")
        .filter_map(|l| l.get("itemListElement").and_then(Value::as_array))
        .flatten()
        .filter_map(|item| str_field(item, "url"))
        .collect()
}

/// A podcast made from a page without feed that lists episode pages
/// (Radio France series and selections): each episode is read from its page.
/// The listed pages that are not episodes (other series) are skipped.
fn page_podcast(page_url: &str, page: &str) -> Result<Podcast> {
    let mut urls = listed_pages(page);
    // next pages of the list: `?p=2`...
    let base = page_url.split(['?', '#']).next().unwrap_or(page_url);
    let path = base
        .split_once("://")
        .map_or(base, |(_, p)| &p[p.find('/').unwrap_or(p.len())..]);
    let mut next = 2;
    let mut current = page.to_string();
    while next <= 20 && current.contains(&format!("{path}?p={next}\"")) {
        current = get_text(&format!("{base}?p={next}"))?;
        urls.extend(listed_pages(&current));
        next += 1;
    }
    let mut seen = HashSet::new();
    urls.retain(|u| seen.insert(u.clone()));
    if urls.is_empty() {
        bail!("No podcast feed found at this address");
    }

    let episodes: Vec<Option<Episode>> = std::thread::scope(|scope| {
        let handles: Vec<_> = urls
            .iter()
            .map(|url| scope.spawn(move || get_text(url).ok().and_then(|p| page_episode(url, &p))))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().ok().flatten())
            .collect()
    });
    let skipped = episodes.iter().filter(|e| e.is_none()).count();
    let episodes: Vec<Episode> = episodes.into_iter().flatten().collect();
    if episodes.is_empty() {
        bail!("No podcast feed found at this address");
    }
    if skipped > 0 {
        log::warn!("{page_url}: {skipped} listed page(s) without audio skipped");
    }

    let objects = json_ld(page);
    let title = of_type(&objects, "WebPage")
        .filter_map(|p| str_field(p, "name"))
        .find(|n| !n.starts_with("Écouter"))
        .or_else(|| og_meta(page, "title"))
        .unwrap_or_else(|| "Podcast".into());
    Ok(Podcast {
        feed_url: page_url.to_string(),
        title,
        author: None,
        image_url: og_meta(page, "image"),
        episodes,
    })
}

const ITUNES: &str = "http://www.itunes.com/dtds/podcast-1.0.dtd";

fn month(name: &str) -> Option<u32> {
    let months = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let lower = name.to_lowercase();
    months
        .iter()
        .position(|m| lower.starts_with(m))
        .map(|i| i as u32 + 1)
}

/// RFC 2822 date (`Fri, 24 Jul 2026 04:42:00 +0200`) → (`2026-07-24`, sort key).
fn parse_date(date: &str) -> (String, String) {
    let parts: Vec<&str> = date.split_whitespace().collect();
    let parts = if parts.first().is_some_and(|p| p.ends_with(',')) {
        &parts[1..]
    } else {
        &parts[..]
    };
    match parts {
        [day, mon, year, rest @ ..] => {
            match (day.parse::<u32>(), month(mon), year.parse::<u32>()) {
                (Ok(d), Some(m), Ok(y)) => {
                    let iso = format!("{y:04}-{m:02}-{d:02}");
                    let time = rest.first().copied().unwrap_or("");
                    (iso.clone(), format!("{iso} {time}"))
                }
                _ => (String::new(), String::new()),
            }
        }
        _ => (String::new(), String::new()),
    }
}

/// `3600`, `59:30`, `1:02:03` → seconds.
fn parse_duration(d: &str) -> Option<u32> {
    let mut secs = 0u32;
    for part in d.trim().split(':') {
        secs = secs
            .checked_mul(60)?
            .checked_add(part.trim().parse::<f32>().ok()? as u32)?;
    }
    Some(secs)
}

/// `itunes:<name>` child of `node`.
fn itunes<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    node.children().find(|c| c.has_tag_name((ITUNES, name)))
}

pub fn parse_feed(feed_url: &str, xml: &str) -> Result<Podcast> {
    let doc = roxmltree::Document::parse(xml).context("Invalid podcast feed")?;
    let channel = doc
        .descendants()
        .find(|n| n.has_tag_name("channel"))
        .context("Invalid podcast feed (no channel)")?;
    let child_text = |node: roxmltree::Node, name: &str| {
        node.children()
            .find(|c| c.tag_name().name() == name && c.tag_name().namespace().is_none())
            .and_then(|c| c.text())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    };

    let image_url = itunes(channel, "image")
        .and_then(|i| i.attribute("href"))
        .map(str::to_string)
        .or_else(|| {
            channel
                .children()
                .find(|c| c.has_tag_name("image"))
                .and_then(|i| child_text(i, "url"))
        });

    let mut episodes: Vec<(String, Episode)> = Vec::new();
    for item in channel.children().filter(|c| c.has_tag_name("item")) {
        let Some(enclosure) = item.children().find(|c| c.has_tag_name("enclosure")) else {
            continue;
        };
        let Some(url) = enclosure.attribute("url") else {
            continue;
        };
        let (date, key) = child_text(item, "pubDate")
            .map(|d| parse_date(&d))
            .unwrap_or_default();
        episodes.push((
            key,
            Episode {
                guid: child_text(item, "guid").unwrap_or_else(|| url.to_string()),
                title: child_text(item, "title").unwrap_or_else(|| "?".into()),
                date,
                duration: itunes(item, "duration")
                    .and_then(|d| d.text())
                    .and_then(parse_duration),
                url: url.to_string(),
                size: enclosure.attribute("length").and_then(|l| l.parse().ok()),
            },
        ));
    }
    // feeds list the newest first: reversed, then sorted by date (stable)
    episodes.reverse();
    episodes.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(Podcast {
        feed_url: feed_url.to_string(),
        title: child_text(channel, "title").unwrap_or_else(|| "Podcast".into()),
        author: itunes(channel, "author")
            .and_then(|a| a.text())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty()),
        image_url,
        episodes: episodes.into_iter().map(|(_, e)| e).collect(),
    })
}

// ---------------------------------------------------------------------------
// Download
// ---------------------------------------------------------------------------

/// Podcast image (album cover), if any.
pub fn download_cover(podcast: &Podcast) -> Option<Vec<u8>> {
    let url = podcast.image_url.as_deref()?;
    let resp = http::agent(Duration::from_secs(20)).get(url).call().ok()?;
    http::bytes(resp).ok()
}

/// Downloads the episodes into `dir`, converted to mono 44.1 kHz MP3 when
/// needed or with the volume normalized (ffmpeg), and returns them as album
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
            ctx.file_progress("⌛ FFMPEG", 0, 0);
            let mp3 = audio::normalized_mp3(&data)
                .with_context(|| format!("unable to convert '{}'", ep.title))?;
            ctx.file_progress("", 0, 0);
            mp3
        } else if audio::transcoding_required(&ep.url, &data) {
            ctx.file_progress("⌛ FFMPEG", 0, 0);
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

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

/// `2026-07-24` → `24/07/2026`.
pub fn display_date(iso: &str) -> String {
    match iso.split('-').collect::<Vec<_>>()[..] {
        [y, m, d] => format!("{d}/{m}/{y}"),
        _ => String::new(),
    }
}

/// A title usable as a file name (shown in the import messages).
fn file_name(title: &str) -> String {
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
    let needs_ffmpeg = episodes.iter().any(|e| {
        !e.url
            .to_lowercase()
            .split('?')
            .next()
            .unwrap_or("")
            .ends_with(".mp3")
    });
    if needs_ffmpeg && !crate::constants::transcoding_supported() {
        bail!("These episodes must be converted to MP3: FFMPEG is required (Tools menu)");
    }
    let tracks = download_episodes(ctx, episodes, dir, mode.audio, on_episode)?;

    if mode.as_stories {
        let cover = download_cover(podcast);
        let mut archives = Vec::with_capacity(tracks.len());
        for (ep, track) in episodes.iter().zip(&tracks) {
            let zip = dir.join(format!("{}.zip", file_name(&ep.title)));
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

// ---------------------------------------------------------------------------
// Imported episodes (~/.lunii/podcasts.json)
// ---------------------------------------------------------------------------

fn state_file() -> PathBuf {
    cfg_dir().join("podcasts.json")
}

fn load_state() -> BTreeMap<String, Value> {
    std::fs::read(state_file())
        .ok()
        .and_then(|d| serde_json::from_slice(&d).ok())
        .unwrap_or_default()
}

/// GUIDs of the episodes already imported in the album `uuid`.
pub fn imported_episodes(uuid: &Uuid) -> HashSet<String> {
    load_state()
        .get(&uuid.to_string())
        .and_then(|p| p.get("episodes"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// A podcast imported as an album, from `~/.lunii/podcasts.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedPodcast {
    pub album: Uuid,
    pub feed_url: String,
    pub title: String,
}

/// The followed podcasts: added in the Podcasts view or imported as albums
/// (on any device), by title.
pub fn saved_podcasts() -> Vec<SavedPodcast> {
    let mut list: Vec<SavedPodcast> = load_state()
        .into_iter()
        .filter_map(|(uuid, p)| {
            Some(SavedPodcast {
                album: Uuid::parse_str(&uuid).ok()?,
                feed_url: p.get("feed")?.as_str()?.to_string(),
                title: p
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("Podcast")
                    .to_string(),
            })
        })
        .collect();
    list.sort_by_cached_key(|p| p.title.to_lowercase());
    list
}

/// Episodes of `podcast` published after the latest one imported in its
/// album, oldest first (the older ones were left out on purpose). All of
/// them if none was imported.
pub fn new_episodes(podcast: &Podcast) -> Vec<Episode> {
    let imported = imported_episodes(&podcast.album_uuid());
    // episodes are oldest first: after the last imported one
    let start = podcast
        .episodes
        .iter()
        .rposition(|e| imported.contains(&e.guid))
        .map_or(0, |i| i + 1);
    podcast.episodes[start..]
        .iter()
        .filter(|e| !imported.contains(&e.guid))
        .cloned()
        .collect()
}

/// Adds `podcast` to the followed podcasts (kept if already followed).
pub fn follow(podcast: &Podcast) -> Result<()> {
    let mut state = load_state();
    let key = podcast.album_uuid().to_string();
    let episodes = state
        .get(&key)
        .and_then(|p| p.get("episodes"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    state.insert(
        key,
        json!({"feed": podcast.feed_url, "title": podcast.title, "episodes": episodes}),
    );
    save_state(&state)
}

/// Removes a podcast from the followed podcasts (its album on a device is
/// kept, but no longer updated).
pub fn unfollow(album: &Uuid) -> Result<()> {
    let mut state = load_state();
    if state.remove(&album.to_string()).is_some() {
        save_state(&state)?;
    }
    Ok(())
}

fn save_state(state: &BTreeMap<String, Value>) -> Result<()> {
    ensure_cfg_dir()?;
    std::fs::write(state_file(), serde_json::to_vec_pretty(state)?)?;
    Ok(())
}

/// Records the episodes of the album (`reset`: a new album, previous records dropped).
pub fn record_imported(podcast: &Podcast, guids: &[String], reset: bool) -> Result<()> {
    let uuid = podcast.album_uuid();
    let mut known = if reset {
        Vec::new()
    } else {
        let mut v: Vec<String> = imported_episodes(&uuid).into_iter().collect();
        v.sort();
        v
    };
    for g in guids {
        if !known.contains(g) {
            known.push(g.clone());
        }
    }
    let mut state = load_state();
    state.insert(
        uuid.to_string(),
        json!({"feed": podcast.feed_url, "title": podcast.title, "episodes": known}),
    );
    save_state(&state)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd">
<channel>
  <title>Les Odyssées</title>
  <itunes:author>France Inter</itunes:author>
  <itunes:image href="https://example.org/cover.jpg"/>
  <item>
    <title>Alice au pays des merveilles</title>
    <guid isPermaLink="false">ep-2</guid>
    <pubDate>Thu, 23 Jul 2026 04:42:00 +0200</pubDate>
    <itunes:duration>1:02:03</itunes:duration>
    <enclosure url="https://example.org/2.m4a" length="15520000" type="audio/x-m4a"/>
  </item>
  <item>
    <title>Pas d'audio</title>
  </item>
  <item>
    <title>L'Île au trésor</title>
    <pubDate>Wed, 22 Jul 2026 04:42:00 +0200</pubDate>
    <itunes:duration>1800</itunes:duration>
    <enclosure url="https://example.org/1.m4a" length="15632000" type="audio/x-m4a"/>
  </item>
</channel>
</rss>"#;

    #[test]
    fn feed_parsing() {
        let p = parse_feed("https://example.org/feed.xml", FEED).unwrap();
        assert_eq!(p.title, "Les Odyssées");
        assert_eq!(p.author.as_deref(), Some("France Inter"));
        assert_eq!(
            p.image_url.as_deref(),
            Some("https://example.org/cover.jpg")
        );
        // without audio: skipped; oldest first
        assert_eq!(p.episodes.len(), 2);
        assert_eq!(p.episodes[0].title, "L'Île au trésor");
        assert_eq!(p.episodes[0].date, "2026-07-22");
        assert_eq!(p.episodes[0].duration, Some(1800));
        // no guid: the audio address
        assert_eq!(p.episodes[0].guid, "https://example.org/1.m4a");
        assert_eq!(p.episodes[1].guid, "ep-2");
        assert_eq!(p.episodes[1].duration, Some(3723));
        assert_eq!(p.episodes[1].size, Some(15_520_000));
        assert_eq!(p.album_uuid(), album_uuid("https://example.org/feed.xml"));
    }

    #[test]
    fn feed_discovery() {
        let rf = r#"<script>{"url":"https://radiofrance-podcast.net/podcast09/podcast_c361798b.xml"}</script>"#;
        assert_eq!(
            feed_in_page(rf).as_deref(),
            Some("https://radiofrance-podcast.net/podcast09/podcast_c361798b.xml")
        );
        let std = r#"<head><link rel="alternate" type="application/rss+xml" title="x" href="https://example.org/feed"></head>"#;
        assert_eq!(
            feed_in_page(std).as_deref(),
            Some("https://example.org/feed")
        );
        assert_eq!(feed_in_page("<html></html>"), None);
        assert!(looks_like_feed(FEED));
    }

    #[test]
    fn names() {
        assert_eq!(display_date("2026-07-24"), "24/07/2026");
        assert_eq!(display_date(""), "");
        assert_eq!(file_name("L'Île / au trésor ?"), "L'Île _ au trésor _");
        assert_eq!(file_name("  "), "episode");
    }

    #[test]
    fn dates() {
        assert_eq!(
            parse_date("Fri, 24 Jul 2026 04:42:00 +0200").0,
            "2026-07-24"
        );
        assert_eq!(parse_date("3 Jan 2025 10:00:00 GMT").0, "2025-01-03");
        assert_eq!(parse_date("n'importe quoi").0, "");
        assert_eq!(parse_duration("59:30"), Some(3570));
        assert_eq!(parse_duration("abc"), None);
    }

    #[test]
    fn radio_france_episode_page() {
        let page = r#"<head><meta property="og:image" content="https://img/x.jpg">
<script type="application/ld+json" data-x="1">{"@context":"https://schema.org","@graph":[
 {"@type":"NewsArticle","headline":"Une étrange coïncidence"},
 {"@type":"RadioEpisode","name":"Une étrange coïncidence : épisode 1/4 | France Culture",
  "dateCreated":"2023-06-24T08:06:00.000Z",
  "mainEntity":{"@type":"AudioObject","contentUrl":"https://media/a.m4a","duration":"P0Y0M0DT0H22M50S"}}
]}</script></head>"#;
        let e = page_episode("https://rf/ep-1", page).unwrap();
        assert_eq!(e.title, "Une étrange coïncidence");
        assert_eq!(e.date, "2023-06-24");
        assert_eq!(e.duration, Some(22 * 60 + 50));
        assert_eq!(e.url, "https://media/a.m4a");
        assert_eq!(e.guid, "https://rf/ep-1");
        assert_eq!(og_meta(page, "image").as_deref(), Some("https://img/x.jpg"));
        // a series page: no audio, but the list of its episodes
        let series = r#"<script type="application/ld+json">{"@type":"ItemList","itemListElement":[
 {"@type":"ListItem","position":1,"url":"https://rf/ep-1"},{"@type":"ListItem","position":2,"url":"https://rf/ep-2"}]}</script>"#;
        assert!(page_episode("https://rf/serie", series).is_none());
        assert_eq!(listed_pages(series), ["https://rf/ep-1", "https://rf/ep-2"]);
        assert_eq!(parse_iso_duration("PT1H2M3S"), Some(3723));
    }
}
