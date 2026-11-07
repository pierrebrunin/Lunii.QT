//! Story databases (official Lunii DB + third-party / STUdio DB) and thumbnails cache.
//!
//! The files are shared with earlier versions (`~/.lunii/official.db`,
//! `~/.lunii/third-party.db`, `~/.lunii/cache/`).

use std::sync::{LazyLock, Mutex, RwLock};
use std::time::Duration;

use anyhow::{Context as _, Result};
use indexmap::IndexMap;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::constants::*;
use crate::http;

const EMBEDDED_THIRD_PARTY_DB: &str = include_str!("../../res/unofficial.json");

#[derive(Default)]
struct Databases {
    official: IndexMap<String, Value>,
    third_party: IndexMap<String, Value>,
}

static DB: LazyLock<RwLock<Databases>> = LazyLock::new(Default::default);

/// Serializes the read-modify-write cycles on the database files.
static FILE_LOCK: Mutex<()> = Mutex::new(());

/// Key used in the in-memory databases: upper case hyphenated UUID.
pub fn db_key(uuid: &Uuid) -> String {
    uuid.hyphenated().to_string().to_uppercase()
}

fn fetch_official_db() -> Result<()> {
    let agent = http::agent(Duration::from_secs(30));
    let token = http::json(agent.get(OFFICIAL_TOKEN_URL).call()?)?;
    let auth_token = token["response"]["token"]["server"]
        .as_str()
        .context("no token in Lunii guest response")?
        .to_string();

    let resp = http::json(
        agent
            .get(OFFICIAL_DB_URL)
            .header("Application-Sender", "luniistore_desktop")
            .header("Accept", "application/json")
            .header("X-AUTH-TOKEN", &auth_token)
            .call()?,
    )?;

    let db = resp.get("response").cloned().unwrap_or(Value::Null);
    std::fs::write(file_official_db(), serde_json::to_vec(&db)?)?;
    Ok(())
}

fn load_db_file(path: &std::path::Path) -> Option<IndexMap<String, Value>> {
    let content = std::fs::read(path).ok()?;
    let parsed: Map<String, Value> = match serde_json::from_slice(&content) {
        Ok(m) => m,
        Err(_) => {
            // corrupted file, removing it
            let _ = std::fs::remove_file(path);
            return None;
        }
    };
    let mut out = IndexMap::new();
    for (_, value) in parsed {
        let Some(uuid) = value.get("uuid").and_then(Value::as_str) else {
            let _ = std::fs::remove_file(path);
            return None;
        };
        out.insert(uuid.to_uppercase(), value);
    }
    Some(out)
}

/// Loads the story databases, downloading the official one if missing (or if `reload`).
///
/// Returns `false` if the official database could not be fetched.
pub fn load_db(reload: bool) -> bool {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load_db_unlocked(reload)
}

fn load_db_unlocked(reload: bool) -> bool {
    let mut ret = true;

    if reload || !file_official_db().is_file() {
        let _ = ensure_cfg_dir();
        if let Err(e) = fetch_official_db() {
            log::warn!(target: "lunii", "Failed to fetch official DB: {e}");
            ret = false;
        }
    }

    let official = load_db_file(&file_official_db()).unwrap_or_default();

    let third_party_file = file_third_party_db();
    if !third_party_file.is_file() {
        let _ = ensure_cfg_dir();
        let _ = std::fs::write(&third_party_file, EMBEDDED_THIRD_PARTY_DB);
    }
    let third_party = load_db_file(&third_party_file).unwrap_or_default();

    let mut db = DB.write().unwrap();
    db.official = official;
    db.third_party = third_party;
    ret
}

/// Adds (or updates) a story in the third-party database.
pub fn thirdparty_add_story(
    uuid: &Uuid,
    title: Option<&str>,
    description: Option<&str>,
) -> Result<()> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = file_third_party_db();
    let mut stories: Map<String, Value> = Map::new();
    if path.is_file() {
        match std::fs::read(&path)
            .ok()
            .and_then(|c| serde_json::from_slice(&c).ok())
        {
            Some(m) => stories = m,
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    stories.insert(
        uuid.simple().to_string(),
        json!({
            "uuid": uuid.hyphenated().to_string(),
            "title": title,
            "description": description,
        }),
    );

    ensure_cfg_dir()?;
    std::fs::write(&path, serde_json::to_vec(&Value::Object(stories))?)?;

    load_db_unlocked(false);
    Ok(())
}

/// Stores a thumbnail in the cache (only if not already present).
pub fn thirdparty_add_thumb(uuid: &Uuid, image_data: &[u8]) -> Result<()> {
    let dir = cache_dir();
    std::fs::create_dir_all(&dir)?;
    let res_file = dir.join(db_key(uuid));
    if !res_file.is_file() {
        std::fs::write(res_file, image_data)?;
    }
    Ok(())
}

fn first_key(v: &Value) -> Option<&str> {
    v.as_object()?.keys().next().map(String::as_str)
}

fn is_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
    }
}

fn localized_field<'a>(entry: &'a Value, field: &str) -> Option<Option<&'a str>> {
    if is_truthy(entry.get("locales_available")) && is_truthy(entry.get("localized_infos")) {
        let locale = first_key(&entry["locales_available"])?;
        return Some(
            entry["localized_infos"]
                .get(locale)
                .and_then(|i| i.get(field))
                .and_then(Value::as_str),
        );
    }
    None
}

fn lookup<T>(uuid: &Uuid, f: impl Fn(&Value) -> Option<T>) -> Option<T> {
    let key = db_key(uuid);
    let db = DB.read().unwrap();
    for table in [&db.official, &db.third_party] {
        if let Some(entry) = table.get(&key)
            && let Some(v) = f(entry)
        {
            return Some(v);
        }
    }
    None
}

/// Story title, from the official database first, then the third-party one.
pub fn story_name(uuid: &Uuid) -> String {
    lookup(uuid, |entry| match localized_field(entry, "title") {
        Some(title) => title.map(str::to_string),
        None => entry
            .get("title")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_string),
    })
    .unwrap_or_else(|| STORY_UNKNOWN.to_string())
}

/// Removes the leading HTML tags of official descriptions.
fn strip_leading_html(mut desc: &str) -> &str {
    while desc.trim_start().starts_with('<') {
        match desc.find('>') {
            Some(pos) => desc = desc[pos + 1..].trim_start(),
            None => break,
        }
    }
    desc
}

/// Text of an HTML description, for display: line breaks and list items
/// kept, the other tags removed, entities decoded.
pub fn plain_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        text.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>') else {
            text.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let tag = rest[start + 1..start + end].trim().to_ascii_lowercase();
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        let closing = tag.starts_with('/');
        match name.as_str() {
            "br" => text.push('\n'),
            "p" | "div" | "ul" | "ol" if closing => text.push('\n'),
            "li" if !closing => text.push_str("\n• "),
            _ => {}
        }
        rest = &rest[start + end + 1..];
    }
    text.push_str(rest);
    let text = text
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    // trimmed lines, at most one empty line in a row
    let mut out = String::new();
    let mut empty = 0;
    for line in text.lines().map(str::trim) {
        if line.is_empty() {
            empty += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if empty > 0 { "\n\n" } else { "\n" });
        }
        out.push_str(line);
        empty = 0;
    }
    out
}

pub fn story_description(uuid: &Uuid) -> String {
    lookup(uuid, |entry| match localized_field(entry, "description") {
        Some(desc) => desc.map(|d| strip_leading_html(d).to_string()),
        None => entry
            .get("description")
            .and_then(Value::as_str)
            .filter(|d| !d.is_empty())
            .map(str::to_string),
    })
    .unwrap_or_else(|| DESC_NOT_FOUND.to_string())
}

/// Duration of a story in seconds, as given by the database (official
/// stories: `duration` in milliseconds).
pub fn story_duration(uuid: &Uuid) -> Option<u32> {
    lookup(uuid, |entry| {
        entry
            .get("duration")
            .and_then(Value::as_u64)
            .filter(|&ms| ms > 0)
            .map(|ms| ms.div_ceil(1000) as u32)
    })
}

pub fn story_author(uuid: &Uuid) -> String {
    lookup(uuid, |entry| {
        if is_truthy(entry.get("authors")) {
            let key = first_key(&entry["authors"])?;
            entry["authors"][key]
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
        } else {
            entry
                .get("author")
                .and_then(Value::as_str)
                .filter(|a| !a.is_empty())
                .map(str::to_string)
        }
    })
    .unwrap_or_else(|| AUTHOR_NOT_FOUND.to_string())
}

pub fn is_official(uuid: &Uuid) -> bool {
    DB.read().unwrap().official.contains_key(&db_key(uuid))
}

pub fn picture_url(uuid: &Uuid) -> Option<String> {
    let db = DB.read().unwrap();
    let entry = db.official.get(&db_key(uuid))?;
    let locale = first_key(entry.get("locales_available")?)?;
    let url = entry["localized_infos"]
        .get(locale)?
        .get("image")?
        .get("image_url")?
        .as_str()?;
    Some(format!("{OFFICIAL_IMAGE_BASE_URL}{url}"))
}

/// Third-party metadata as JSON (stored in `_metadata.json` of plain exports).
pub fn story_meta(uuid: &Uuid) -> Option<String> {
    let db = DB.read().unwrap();
    db.third_party.get(&db_key(uuid)).map(|m| m.to_string())
}

/// Story picture, from the cache or downloaded (official stories only).
pub fn story_picture(uuid: &Uuid, reload: bool) -> Option<Vec<u8>> {
    let dir = cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    let res_file = dir.join(db_key(uuid));

    let mut image_data = None;
    if (reload || !res_file.is_file())
        && let Some(url) = picture_url(uuid)
    {
        let data = http::agent(Duration::from_secs(2))
            .get(&url)
            .call()
            .map_err(anyhow::Error::from)
            .and_then(http::bytes);
        if let Ok(data) = data {
            let _ = std::fs::write(&res_file, &data);
            image_data = Some(data);
        }
    }

    if image_data.is_none() && res_file.is_file() {
        image_data = std::fs::read(&res_file).ok();
    }
    image_data
}

/// First full UUID (any database) containing `partial` (case insensitive).
pub fn find_full_uuid(partial: &str) -> Option<String> {
    let needle = partial.to_uppercase();
    let db = DB.read().unwrap();
    db.official
        .keys()
        .find(|k| k.contains(&needle))
        .or_else(|| db.third_party.keys().find(|k| k.contains(&needle)))
        .cloned()
}

/// Number of entries in (official, third-party) databases.
pub fn db_sizes() -> (usize, usize) {
    let db = DB.read().unwrap();
    (db.official.len(), db.third_party.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_strip() {
        assert_eq!(
            strip_leading_html("<p><b>Hello</b> world"),
            "Hello</b> world"
        );
        assert_eq!(strip_leading_html("  plain"), "  plain");
    }

    #[test]
    fn html_to_text() {
        assert_eq!(
            plain_text("Aider ?<br>Mais <b>en duo</b>&nbsp;!<br/><br/>Fin<br />"),
            "Aider ?\nMais en duo !\n\nFin"
        );
        assert_eq!(
            plain_text("<p>Un</p><p>Deux &amp; trois</p><ul><li>a</li><li>b</li></ul>"),
            "Un\nDeux & trois\n\n• a\n• b"
        );
        assert_eq!(
            plain_text(r#"Voir <a href="https://x/y.pdf">les cartes</a>"#),
            "Voir les cartes"
        );
        assert_eq!(plain_text("pas de balise"), "pas de balise");
        assert_eq!(plain_text("a < b"), "a < b");
    }

    #[test]
    fn embedded_db_is_valid_json() {
        let v: Value = serde_json::from_str(EMBEDDED_THIRD_PARTY_DB).unwrap();
        assert!(v.is_object());
    }
}
