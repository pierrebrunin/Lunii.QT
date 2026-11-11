//! Followed podcasts and imported episodes (`~/.lunii/podcasts.json`).

use super::*;

pub(super) fn state_file() -> PathBuf {
    cfg_dir().join("podcasts.json")
}

pub(super) fn load_state() -> BTreeMap<String, Value> {
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

/// The followed podcasts: added in the Podcasts view, imported as albums or
/// found on a device, by title.
pub fn saved_podcasts() -> Vec<SavedPodcast> {
    let mut list: Vec<SavedPodcast> = load_state()
        .into_iter()
        .filter(|(_, p)| !unfollowed(p))
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

/// A podcast no longer followed: kept, so that its album found on a device
/// does not follow it again.
fn unfollowed(entry: &Value) -> bool {
    entry.get("unfollowed").and_then(Value::as_bool) == Some(true)
}

/// Feed address of the followed podcast that `podcast` is, if it is followed
/// under another address (a feed reached through a page, Apple Podcasts or a
/// redirect): the addresses declared by the feeds, or for a podcast followed
/// before they were recorded, the same title and an episode already imported.
fn followed_feed(state: &BTreeMap<String, Value>, podcast: &Podcast) -> Option<String> {
    let addresses: Vec<&str> = std::iter::once(podcast.feed_url.as_str())
        .chain(podcast.aliases.iter().map(String::as_str))
        .collect();
    let strings = |entry: &Value, name: &str| -> Vec<String> {
        entry
            .get(name)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let entries = || {
        state
            .values()
            .filter_map(|e| Some((e, e.get("feed")?.as_str()?.to_string())))
    };
    if entries().any(|(_, feed)| feed == podcast.feed_url) {
        return Some(podcast.feed_url.clone());
    }
    let declared = entries().find(|(e, feed)| {
        addresses.contains(&feed.as_str())
            || strings(e, "aliases")
                .iter()
                .any(|a| addresses.contains(&a.as_str()))
    });
    if let Some((_, feed)) = declared {
        return Some(feed);
    }
    let title = podcast.title.trim().to_lowercase();
    entries()
        .find(|(e, _)| {
            e.get("title")
                .and_then(Value::as_str)
                .map(|t| t.trim().to_lowercase())
                == Some(title.clone())
                && strings(e, "episodes")
                    .iter()
                    .any(|g| podcast.episodes.iter().any(|ep| &ep.guid == g))
        })
        .map(|(_, feed)| feed)
}

/// Gives `podcast` the address under which it is already followed, if it
/// is: the same album, the episodes already imported known.
pub fn match_followed(podcast: &mut Podcast) {
    if let Some(feed) = followed_feed(&load_state(), podcast)
        && feed != podcast.feed_url
    {
        let previous = std::mem::replace(&mut podcast.feed_url, feed);
        podcast.aliases.retain(|a| *a != podcast.feed_url);
        if !podcast.aliases.contains(&previous) {
            podcast.aliases.push(previous);
        }
    }
}

/// Entry of `podcast` in the state: its address, title, imported episodes
/// and the other addresses known (the earlier ones kept).
fn entry(previous: Option<&Value>, podcast: &Podcast, episodes: Value) -> Value {
    let mut aliases: Vec<String> = previous
        .and_then(|p| p.get("aliases"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    for a in &podcast.aliases {
        if *a != podcast.feed_url && !aliases.contains(a) {
            aliases.push(a.clone());
        }
    }
    json!({"feed": podcast.feed_url, "aliases": aliases, "title": podcast.title, "episodes": episodes})
}

/// Adds `podcast` to the followed podcasts (kept if already followed, even
/// under another address).
pub fn follow(podcast: &Podcast) -> Result<()> {
    let mut podcast = podcast.clone();
    let mut state = load_state();
    if let Some(feed) = followed_feed(&state, &podcast) {
        podcast.feed_url = feed;
    }
    let key = podcast.album_uuid().to_string();
    let previous = state.get(&key);
    let episodes = previous
        .and_then(|p| p.get("episodes"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let value = entry(previous, &podcast, episodes);
    state.insert(key, value);
    save_state(&state)
}

/// Removes a podcast from the followed podcasts (its album on a device is
/// kept, but no longer updated). Remembered as such: its album found on a
/// device does not follow it again.
pub fn unfollow(album: &Uuid) -> Result<()> {
    let mut state = load_state();
    if let Some(entry) = state.get_mut(&album.to_string())
        && let Some(map) = entry.as_object_mut()
    {
        map.insert("unfollowed".into(), true.into());
        save_state(&state)?;
    }
    Ok(())
}

/// Follows the podcast of an album found on a device, unless it is known
/// (followed, or no longer followed, under any of its addresses). `true` if
/// it is followed from now on.
pub fn follow_album(source: &crate::album::meta::PodcastSource, title: &str) -> Result<bool> {
    let podcast = Podcast {
        feed_url: source.feed.clone(),
        aliases: source.aliases.clone(),
        title: title.to_string(),
        author: None,
        image_url: None,
        episodes: Vec::new(),
    };
    let mut state = load_state();
    if followed_feed(&state, &podcast).is_some() {
        return Ok(false);
    }
    state.insert(
        podcast.album_uuid().to_string(),
        entry(None, &podcast, json!([])),
    );
    save_state(&state)?;
    Ok(true)
}

pub(super) fn save_state(state: &BTreeMap<String, Value>) -> Result<()> {
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
    let value = entry(state.get(&uuid.to_string()), podcast, json!(known));
    state.insert(uuid.to_string(), value);
    save_state(&state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn podcast(feed: &str, aliases: &[&str], title: &str, guids: &[&str]) -> Podcast {
        Podcast {
            feed_url: feed.into(),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            title: title.into(),
            author: None,
            image_url: None,
            episodes: guids
                .iter()
                .map(|g| Episode {
                    guid: g.to_string(),
                    title: "?".into(),
                    date: String::new(),
                    duration: None,
                    url: format!("https://ex/{g}.mp3"),
                    size: None,
                })
                .collect(),
        }
    }

    fn state(entries: &[(&str, Value)]) -> BTreeMap<String, Value> {
        entries
            .iter()
            .map(|(feed, e)| (album_uuid(feed).to_string(), e.clone()))
            .collect()
    }

    /// Radio France: the short address of the page, the long one of Apple
    /// Podcasts, both feeds declaring the long one.
    #[test]
    fn same_podcast_other_address() {
        let short = "https://rf/podcast_1.xml";
        let long = "https://rf/abc/podcast_1.xml";
        // followed from the page (short), then found by the search (long)
        let s = state(&[(
            short,
            json!({"feed": short, "aliases": [long], "title": "Caillou", "episodes": []}),
        )]);
        let found = podcast(long, &[], "Caillou", &["a"]);
        assert_eq!(followed_feed(&s, &found).as_deref(), Some(short));
        // followed from the search (long), then from the page (short)
        let s = state(&[(
            long,
            json!({"feed": long, "title": "Caillou", "episodes": []}),
        )]);
        let found = podcast(short, &[long], "Caillou", &["a"]);
        assert_eq!(followed_feed(&s, &found).as_deref(), Some(long));
        // followed before the addresses were recorded: same title and an
        // episode already imported
        let s = state(&[(
            short,
            json!({"feed": short, "title": "Caillou", "episodes": ["a"]}),
        )]);
        let found = podcast(long, &[], "caillou ", &["a", "b"]);
        assert_eq!(followed_feed(&s, &found).as_deref(), Some(short));
        // same title, nothing in common: another podcast
        let found = podcast(long, &[], "Caillou", &["z"]);
        assert_eq!(followed_feed(&s, &found), None);
        // not followed
        assert_eq!(followed_feed(&BTreeMap::new(), &found), None);
    }
}
