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
    state.insert(
        uuid.to_string(),
        json!({"feed": podcast.feed_url, "title": podcast.title, "episodes": known}),
    );
    save_state(&state)
}
