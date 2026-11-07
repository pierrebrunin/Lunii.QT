//! Podcasts (RSS feeds) → MP3 albums.
//!
//! A podcast becomes an album story (see [`crate::album`]), its episodes the
//! tracks, in chronological order. The album UUID is derived from the feed
//! address: the episodes of a podcast already on the device are appended to its
//! album. The episodes already imported are remembered in
//! `~/.lunii/podcasts.json`.
//!
//! Only standards are used, no site in particular. The address can be:
//! - the feed itself (RSS 2.0 with the iTunes tags, or Atom);
//! - a web page announcing it (`<link rel="alternate" type="application/rss+xml">`);
//! - an Apple Podcasts link (its feed is given by the iTunes lookup API);
//! - a web page without feed listing episode pages (schema.org `ItemList`,
//!   following `<link rel="next">`), each episode page describing its audio
//!   (schema.org `…Episode` with an `AudioObject`).

use std::collections::{BTreeMap, HashMap, HashSet};
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

// ---------------------------------------------------------------------------
// HTML
// ---------------------------------------------------------------------------

/// Attributes of the `<{name} ...>` tags of `page` (names in lower case,
/// entities decoded), in order.
fn html_tags(page: &str, name: &str) -> Vec<HashMap<String, String>> {
    // same byte positions: ASCII only changes
    let lower = page.to_ascii_lowercase();
    let open = format!("<{name}");
    let mut tags = Vec::new();
    let mut from = 0;
    while let Some(pos) = lower[from..].find(&open) {
        let start = from + pos + open.len();
        from = start;
        if !page[start..].starts_with(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>') {
            continue;
        }
        let Some(end) = page[start..].find('>') else {
            break;
        };
        tags.push(html_attributes(&page[start..start + end]));
        from = start + end;
    }
    tags
}

fn html_attributes(mut rest: &str) -> HashMap<String, String> {
    let mut attributes = HashMap::new();
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '/');
        if rest.is_empty() {
            break;
        }
        let name_end = rest
            .find(|c: char| c.is_ascii_whitespace() || c == '=' || c == '/')
            .unwrap_or(rest.len());
        let name = rest[..name_end].to_ascii_lowercase();
        rest = rest[name_end..].trim_start();
        let mut value = String::new();
        if let Some(after) = rest.strip_prefix('=') {
            let after = after.trim_start();
            let (raw, remaining) = match after.chars().next() {
                Some(q @ ('"' | '\'')) => {
                    let body = &after[1..];
                    let end = body.find(q).unwrap_or(body.len());
                    (&body[..end], &body[(end + 1).min(body.len())..])
                }
                _ => {
                    let end = after
                        .find(|c: char| c.is_ascii_whitespace())
                        .unwrap_or(after.len());
                    (&after[..end], &after[end..])
                }
            };
            value = html_unescape(raw);
            rest = remaining;
        }
        if !name.is_empty() {
            attributes.entry(name).or_insert(value);
        }
    }
    attributes
}

fn html_unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// `href` made absolute against the address of its page.
fn absolute(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.starts_with("http://") || href.starts_with("https://") {
        return Some(href.to_string());
    }
    let (scheme, rest) = base.split_once("://")?;
    if let Some(net) = href.strip_prefix("//") {
        return Some(format!("{scheme}://{net}"));
    }
    let host = rest.split(['/', '?', '#']).next()?;
    if href.starts_with('/') {
        return Some(format!("{scheme}://{host}{href}"));
    }
    if href.is_empty() || href.starts_with('#') || href.contains(':') {
        return None;
    }
    // relative to the directory of the page
    let path = rest[host.len()..].split(['?', '#']).next().unwrap_or("");
    let dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    let dir = if dir.is_empty() { "/" } else { dir };
    Some(format!("{scheme}://{host}{dir}{href}"))
}

/// `href` of the `<link>` tags of `page` whose `rel` contains `rel` (and of
/// type `kind`, if given).
fn links<'a>(
    page_url: &'a str,
    tags: &'a [HashMap<String, String>],
    rel: &'a str,
    kind: Option<&'a str>,
) -> impl Iterator<Item = String> + 'a {
    tags.iter()
        .filter(move |l| {
            l.get("rel").is_some_and(|r| {
                r.split_ascii_whitespace()
                    .any(|r| r.eq_ignore_ascii_case(rel))
            }) && kind.is_none_or(|k| l.get("type").is_some_and(|t| t.eq_ignore_ascii_case(k)))
        })
        .filter_map(move |l| absolute(page_url, l.get("href")?))
}

/// Feed address announced by a web page (RSS first, then Atom).
fn feed_in_page(page_url: &str, page: &str) -> Option<String> {
    let tags = html_tags(page, "link");
    ["application/rss+xml", "application/atom+xml"]
        .into_iter()
        .find_map(|kind| links(page_url, &tags, "alternate", Some(kind)).next())
}

/// Next page of a paginated list (`<link rel="next">`).
fn next_page(page_url: &str, page: &str) -> Option<String> {
    links(page_url, &html_tags(page, "link"), "next", None).next()
}

/// `content` of `<meta property="og:{name}">` (Open Graph).
fn og_meta(page: &str, name: &str) -> Option<String> {
    let property = format!("og:{name}");
    html_tags(page, "meta")
        .into_iter()
        .find(|m| {
            m.get("property")
                .or_else(|| m.get("name"))
                .map(String::as_str)
                == Some(property.as_str())
        })
        .and_then(|m| m.get("content").cloned())
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
}

/// `(id, store country)` of an Apple Podcasts link
/// (`https://podcasts.apple.com/fr/podcast/contes-du-soir/id1469858852`).
fn apple_podcast_id(address: &str) -> Option<(String, Option<String>)> {
    let rest = address.split_once("://")?.1;
    let (host, path) = rest.split_once('/')?;
    if host != "podcasts.apple.com" && host != "itunes.apple.com" {
        return None;
    }
    let path = path.split(['?', '#']).next()?;
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let id = segments.iter().rev().find_map(|s| {
        s.strip_prefix("id")
            .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
    })?;
    let country = segments
        .first()
        .filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_alphabetic()))
        .map(|c| c.to_ascii_lowercase());
    Some((id.to_string(), country))
}

/// Feed address of an Apple Podcasts podcast (iTunes lookup API).
fn apple_feed_url(id: &str, country: Option<&str>) -> Result<String> {
    let mut url = format!("https://itunes.apple.com/lookup?id={id}");
    if let Some(country) = country {
        url.push_str(&format!("&country={country}"));
    }
    if let Some(feed) = parse_lookup(&get_text(&url)?)? {
        return Ok(feed);
    }
    // not given by the API (some publishers): its Apple Podcasts page has it
    let page = format!(
        "https://podcasts.apple.com/{}/podcast/id{id}",
        country.unwrap_or("us")
    );
    match get_text(&page).ok().and_then(|p| apple_page_feed(&p, id)) {
        Some(feed) => Ok(feed),
        None => bail!(
            "Apple Podcasts does not give the RSS feed of this podcast: paste the address of its feed or of its page"
        ),
    }
}

/// Feed address in an answer of the iTunes lookup API (`None`: the podcast
/// is known, without its feed).
fn parse_lookup(json: &str) -> Result<Option<String>> {
    let data: Value = serde_json::from_str(json).context("Invalid answer of Apple Podcasts")?;
    let result = data["results"]
        .as_array()
        .and_then(|r| r.iter().find(|r| r["kind"] == "podcast"))
        .context("Podcast not found on Apple Podcasts")?;
    Ok(result["feedUrl"]
        .as_str()
        .filter(|feed| feed.starts_with("http"))
        .map(str::to_string))
}

/// Feed address of the podcast `id` in its Apple Podcasts page: the data of
/// the page give it next to its id (`"adamId":"…","feedUrl":"…"`), among
/// those of the other podcasts shown.
fn apple_page_feed(page: &str, id: &str) -> Option<String> {
    let key = format!("\"adamId\":\"{id}\"");
    let mut from = 0;
    while let Some(pos) = page[from..].find(&key) {
        let at = from + pos;
        // the fields of the same object
        let start = page[..at].rfind('{').unwrap_or(0);
        let end = page[at..].find('}').map_or(page.len(), |e| at + e);
        let object = &page[start..end];
        if let Some(f) = object.find("\"feedUrl\":\"") {
            let value = &object[f + "\"feedUrl\":\"".len()..];
            let feed = value[..value.find('"').unwrap_or(value.len())]
                .replace("\\u002F", "/")
                .replace("\\/", "/");
            if feed.starts_with("http") {
                return Some(feed);
            }
        }
        from = at + key.len();
    }
    None
}

/// A podcast found by [`search`].
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub title: String,
    pub author: Option<String>,
    /// `None`: the publisher does not give its feed to the API (it is then
    /// read from the Apple Podcasts page, see `apple_url`)
    pub feed_url: Option<String>,
    /// its Apple Podcasts page
    pub apple_url: Option<String>,
    /// small cover (100×100)
    pub artwork_url: Option<String>,
    pub episodes: Option<u32>,
}

/// `true` when `text` is an address (to load) rather than words (to search).
pub fn is_address(text: &str) -> bool {
    let text = text.trim();
    text.starts_with("http://") || text.starts_with("https://")
}

/// Searches podcasts by name in the Apple Podcasts catalog of `country`
/// (`fr`, `us`...): free API, no key, about 20 requests per minute.
pub fn search(term: &str, country: &str) -> Result<Vec<SearchResult>> {
    let url = format!(
        "https://itunes.apple.com/search?media=podcast&entity=podcast&limit=30&country={}&term={}",
        query_encode(country),
        query_encode(term.trim())
    );
    parse_search(&get_text(&url)?)
}

/// Percent-encoding of a query value.
fn query_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn parse_search(json: &str) -> Result<Vec<SearchResult>> {
    let data: Value = serde_json::from_str(json).context("Invalid answer of Apple Podcasts")?;
    let text = |r: &Value, name: &str| {
        r[name]
            .as_str()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    Ok(data["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["kind"] == "podcast")
        .filter_map(|r| {
            Some(SearchResult {
                title: text(r, "collectionName").or_else(|| text(r, "trackName"))?,
                author: text(r, "artistName"),
                feed_url: text(r, "feedUrl").filter(|u| u.starts_with("http")),
                apple_url: text(r, "collectionViewUrl").filter(|u| u.starts_with("https://")),
                artwork_url: text(r, "artworkUrl100").or_else(|| text(r, "artworkUrl60")),
                episodes: r["trackCount"].as_u64().map(|n| n as u32),
            })
        })
        .collect())
}

/// Small picture (search result cover), if any.
pub fn download_artwork(url: &str) -> Option<Vec<u8>> {
    let resp = http::agent(Duration::from_secs(15)).get(url).call().ok()?;
    http::bytes(resp).ok()
}

/// Loads a podcast from its feed address, or from a page announcing it.
pub fn fetch(address: &str) -> Result<Podcast> {
    let address = address.trim();
    if !address.starts_with("http://") && !address.starts_with("https://") {
        bail!("The podcast address must start with https://");
    }
    if let Some((id, country)) = apple_podcast_id(address) {
        let feed_url = apple_feed_url(&id, country.as_deref())?;
        return parse_feed(&feed_url, &get_text(&feed_url)?);
    }
    let body = get_text(address)?;
    if looks_like_feed(&body) {
        return parse_feed(address, &body);
    }
    match feed_in_page(address, &body) {
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
// Pages without feed (schema.org)
// ---------------------------------------------------------------------------

/// The schema.org objects of a page (`<script type="application/ld+json">`,
/// `@graph` and arrays flattened).
fn json_ld(page: &str) -> Vec<Value> {
    let lower = page.to_ascii_lowercase();
    let mut objects = Vec::new();
    let mut from = 0;
    while let Some(pos) = lower[from..].find("<script") {
        let start = from + pos + "<script".len();
        let Some(open) = page[start..].find('>') else {
            break;
        };
        let Some(close) = lower[start..].find("</script>") else {
            break;
        };
        from = start + close + "</script>".len();
        if open > close {
            continue;
        }
        let attributes = html_attributes(&page[start..start + open]);
        if !attributes
            .get("type")
            .is_some_and(|t| t.eq_ignore_ascii_case("application/ld+json"))
        {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&page[start + open + 1..start + close])
        else {
            continue;
        };
        let mut stack = vec![value];
        while let Some(value) = stack.pop() {
            match value {
                Value::Array(items) => stack.extend(items.into_iter().rev()),
                Value::Object(ref o) if o.get("@graph").is_some_and(Value::is_array) => {
                    stack.push(o["@graph"].clone())
                }
                value => objects.push(value),
            }
        }
    }
    objects
}

/// `@type` of a schema.org object (a name or a list of names).
fn types(object: &Value) -> Vec<&str> {
    match object.get("@type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

/// The objects of a type matching `kind`.
fn of_type<'a>(
    objects: &'a [Value],
    kind: impl Fn(&str) -> bool + 'a,
) -> impl Iterator<Item = &'a Value> + 'a {
    objects
        .iter()
        .filter(move |o| types(o).into_iter().any(&kind))
}

fn str_field(object: &Value, name: &str) -> Option<String> {
    object
        .get(name)
        .and_then(Value::as_str)
        .map(|s| s.replace('\u{a0}', " ").trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The shortest of the names a page gives to a thing: pages often add the name
/// of the site (`Title | Site`) or words for search engines to one of them.
fn shortest_name(names: impl IntoIterator<Item = String>) -> Option<String> {
    names
        .into_iter()
        .map(|n| n.split(" | ").next().unwrap_or(&n).trim().to_string())
        .filter(|n| n.chars().count() >= 2)
        .min_by_key(|n| n.chars().count())
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

/// The audio of an episode (`AudioObject` with a `contentUrl`).
fn episode_audio(episode: &Value) -> Option<&Value> {
    ["associatedMedia", "audio", "mainEntity", "encoding"]
        .iter()
        .filter_map(|field| episode.get(*field))
        .flat_map(|v| match v {
            Value::Array(items) => items.iter().collect(),
            v => vec![v],
        })
        .find(|media| str_field(media, "contentUrl").is_some())
}

/// The episode described by an episode page (schema.org `RadioEpisode`,
/// `PodcastEpisode`... and its audio).
fn page_episode(url: &str, page: &str) -> Option<Episode> {
    let objects = json_ld(page);
    let (episode, audio) =
        of_type(&objects, |t| t.ends_with("Episode")).find_map(|e| Some((e, episode_audio(e)?)))?;
    // "Title : episode 1/4 of ... | Site": the shortest name
    let title = shortest_name(
        of_type(&objects, |t| t.ends_with("Article") || t == "BlogPosting")
            .filter_map(|a| str_field(a, "headline"))
            .chain(str_field(episode, "name"))
            .chain(of_type(&objects, |t| t == "WebPage").filter_map(|p| str_field(p, "name")))
            .chain(og_meta(page, "title")),
    )
    .unwrap_or_else(|| "?".into());
    let date = ["datePublished", "dateCreated", "uploadDate"]
        .iter()
        .find_map(|f| str_field(episode, f).or_else(|| str_field(audio, f)))
        .map(|d| d.chars().take(10).collect())
        .unwrap_or_default();
    Some(Episode {
        guid: url.to_string(),
        title,
        date,
        duration: str_field(audio, "duration")
            .or_else(|| str_field(episode, "duration"))
            .and_then(|d| parse_iso_duration(&d)),
        url: str_field(audio, "contentUrl")?,
        size: None,
    })
}

/// The pages listed by a page (`ItemList`), in order.
fn listed_pages(page_url: &str, page: &str) -> Vec<String> {
    of_type(&json_ld(page), |t| t == "ItemList")
        .filter_map(|l| l.get("itemListElement").and_then(Value::as_array))
        .flatten()
        .filter_map(|item| {
            str_field(item, "url")
                .or_else(|| {
                    item.get("item")
                        .and_then(|i| str_field(i, "url").or_else(|| str_field(i, "@id")))
                })
                .or_else(|| str_field(item, "@id"))
        })
        .filter_map(|u| absolute(page_url, &u))
        .collect()
}

/// A podcast made from a page without feed that lists episode pages (and its
/// next pages): each episode is read from its page. The listed pages that are
/// not episodes (other series...) are skipped.
fn page_podcast(page_url: &str, page: &str) -> Result<Podcast> {
    let mut urls = listed_pages(page_url, page);
    let mut visited = HashSet::from([page_url.to_string()]);
    let mut current = (page_url.to_string(), page.to_string());
    for _ in 0..20 {
        let Some(next) = next_page(&current.0, &current.1) else {
            break;
        };
        if !visited.insert(next.clone()) {
            break;
        }
        match get_text(&next) {
            Ok(body) => {
                urls.extend(listed_pages(&next, &body));
                current = (next, body);
            }
            Err(e) => {
                log::warn!("{e:#}");
                break;
            }
        }
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
    let title = shortest_name(
        of_type(&objects, |t| t == "WebPage" || t.ends_with("Series"))
            .filter_map(|p| str_field(p, "name"))
            .chain(og_meta(page, "title")),
    )
    .unwrap_or_else(|| "Podcast".into());
    Ok(Podcast {
        feed_url: page_url.to_string(),
        title,
        author: og_meta(page, "site_name"),
        image_url: og_meta(page, "image").and_then(|i| absolute(page_url, &i)),
        episodes,
    })
}

const ITUNES: &str = "http://www.itunes.com/dtds/podcast-1.0.dtd";
const ATOM: &str = "http://www.w3.org/2005/Atom";

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
    if doc.root_element().has_tag_name((ATOM, "feed")) {
        return parse_atom(feed_url, doc.root_element());
    }
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

/// Atom feed (RFC 4287): entries with a `<link rel="enclosure">`.
fn parse_atom(feed_url: &str, feed: roxmltree::Node) -> Result<Podcast> {
    let atom = |node: roxmltree::Node, name: &str| {
        node.children()
            .find(|c| c.has_tag_name((ATOM, name)))
            .and_then(|c| c.text())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    };
    let author = |node: roxmltree::Node| {
        node.children()
            .find(|c| c.has_tag_name((ATOM, "author")))
            .and_then(|a| atom(a, "name"))
    };

    let mut episodes: Vec<(String, Episode)> = Vec::new();
    for entry in feed.children().filter(|c| c.has_tag_name((ATOM, "entry"))) {
        let Some(enclosure) = entry
            .children()
            .find(|c| c.has_tag_name((ATOM, "link")) && c.attribute("rel") == Some("enclosure"))
        else {
            continue;
        };
        let Some(url) = enclosure.attribute("href") else {
            continue;
        };
        // RFC 3339 (`2026-07-24T04:42:00+02:00`)
        let stamp = atom(entry, "published")
            .or_else(|| atom(entry, "updated"))
            .unwrap_or_default();
        let date = stamp
            .get(..10)
            .filter(|d| d.as_bytes()[4] == b'-' && d.as_bytes()[7] == b'-')
            .unwrap_or("")
            .to_string();
        episodes.push((
            stamp.clone(),
            Episode {
                guid: atom(entry, "id").unwrap_or_else(|| url.to_string()),
                title: atom(entry, "title").unwrap_or_else(|| "?".into()),
                date,
                duration: itunes(entry, "duration")
                    .and_then(|d| d.text())
                    .and_then(parse_duration),
                url: url.to_string(),
                size: enclosure.attribute("length").and_then(|l| l.parse().ok()),
            },
        ));
    }
    episodes.reverse();
    episodes.sort_by(|a, b| a.0.cmp(&b.0));

    let image_url = itunes(feed, "image")
        .and_then(|i| i.attribute("href"))
        .map(str::to_string)
        .or_else(|| atom(feed, "logo"))
        .or_else(|| atom(feed, "icon"));
    Ok(Podcast {
        feed_url: feed_url.to_string(),
        title: atom(feed, "title").unwrap_or_else(|| "Podcast".into()),
        author: itunes(feed, "author")
            .and_then(|a| a.text())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .or_else(|| author(feed)),
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
/// Archive of the episode `index` imported as a story: numbered, as titles
/// may be the same, or start alike (cut to 60 characters).
fn story_zip_name(index: usize, title: &str) -> String {
    format!("{:04} {}.zip", index + 1, file_name(title))
}

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
  <title>Contes du monde</title>
  <itunes:author>Radio Exemple</itunes:author>
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
        assert_eq!(p.title, "Contes du monde");
        assert_eq!(p.author.as_deref(), Some("Radio Exemple"));
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

    /// What real feeds often contain.
    #[test]
    fn feed_usual_content() {
        let xml = "\u{feff}<?xml version=\"1.0\"?>
<rss version=\"2.0\"><channel>
  <title><![CDATA[Contes & comptines]]></title>
  <image><url>https://example.org/logo.png</url></image>
  <item>
    <title>L&apos;ogre &amp; le chat</title>
    <enclosure url=\"https://example.org/1.mp3?a=1&amp;b=2\" type=\"audio/mpeg\"/>
  </item>
</channel></rss>";
        // byte order mark
        assert!(looks_like_feed(xml));
        let p = parse_feed("https://example.org/feed", xml).unwrap();
        assert_eq!(p.title, "Contes & comptines");
        assert_eq!(p.author, None);
        assert_eq!(p.image_url.as_deref(), Some("https://example.org/logo.png"));
        let e = &p.episodes[0];
        assert_eq!(e.title, "L'ogre & le chat");
        assert_eq!(e.url, "https://example.org/1.mp3?a=1&b=2");
        // no date, duration nor size
        assert_eq!((e.date.as_str(), e.duration, e.size), ("", None, None));
    }

    #[test]
    fn invalid_sources() {
        // not XML, XML but not a feed
        assert!(parse_feed("https://ex/f", "<rss><channel>").is_err());
        assert!(parse_feed("https://ex/f", "<html><body/></html>").is_err());
        // a page without feed nor list of episodes: error without network
        let e = page_podcast("https://ex/page", "<html><title>x</title></html>")
            .unwrap_err()
            .to_string();
        assert_eq!(e, "No podcast feed found at this address");
        assert!(!looks_like_feed("<!DOCTYPE html><html>"));
    }

    #[test]
    fn feed_discovery() {
        let page = "https://example.org/podcasts/contes";
        let rss = r#"<head><link rel="alternate" type="application/rss+xml" title="x" href="https://example.org/feed?a=1&amp;b=2"></head>"#;
        assert_eq!(
            feed_in_page(page, rss).as_deref(),
            Some("https://example.org/feed?a=1&b=2")
        );
        // any attribute order, relative address, Atom
        let atom = r#"<LINK href='/atom.xml' type="application/atom+xml" rel="alternate"/>"#;
        assert_eq!(
            feed_in_page(page, atom).as_deref(),
            Some("https://example.org/atom.xml")
        );
        // RSS first
        let both = format!("{atom}{rss}");
        assert_eq!(
            feed_in_page(page, &both).as_deref(),
            Some("https://example.org/feed?a=1&b=2")
        );
        assert_eq!(
            feed_in_page(page, r#"<link rel="stylesheet" href="/a.css">"#),
            None
        );
        assert_eq!(feed_in_page(page, "<html></html>"), None);
        assert!(looks_like_feed(FEED));
    }

    #[test]
    fn html() {
        let base = "https://example.org/a/b/page?p=1";
        assert_eq!(
            absolute(base, "c.html").as_deref(),
            Some("https://example.org/a/b/c.html")
        );
        assert_eq!(
            absolute(base, "/c").as_deref(),
            Some("https://example.org/c")
        );
        assert_eq!(
            absolute(base, "//cdn.org/c").as_deref(),
            Some("https://cdn.org/c")
        );
        assert_eq!(
            absolute(base, "?p=2").as_deref(),
            Some("https://example.org/a/b/?p=2")
        );
        assert_eq!(absolute(base, "mailto:x@y"), None);
        let page = r#"<meta content="Contes &amp; légendes" property="og:title">
<meta name="og:image" content="/cover.jpg"><link rel="next" href="?p=2">"#;
        assert_eq!(og_meta(page, "title").as_deref(), Some("Contes & légendes"));
        assert_eq!(og_meta(page, "image").as_deref(), Some("/cover.jpg"));
        assert_eq!(
            next_page(base, page).as_deref(),
            Some("https://example.org/a/b/?p=2")
        );
        assert_eq!(
            shortest_name(["Écouter Contes – Podcast".into(), "Contes | Site".into()]).as_deref(),
            Some("Contes")
        );
    }

    #[test]
    fn atom_feed() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd">
  <title>Contes du soir</title>
  <author><name>Radio Exemple</name></author>
  <logo>https://example.org/logo.png</logo>
  <entry>
    <title>Le Petit Poucet</title>
    <id>urn:uuid:2</id>
    <published>2026-07-24T04:42:00+02:00</published>
    <itunes:duration>12:30</itunes:duration>
    <link rel="alternate" href="https://example.org/2.html"/>
    <link rel="enclosure" type="audio/mpeg" length="1234" href="https://example.org/2.mp3"/>
  </entry>
  <entry>
    <title>Sans audio</title>
    <link href="https://example.org/x.html"/>
  </entry>
  <entry>
    <title>Cendrillon</title>
    <updated>2026-07-20T08:00:00Z</updated>
    <link rel="enclosure" href="https://example.org/1.mp3"/>
  </entry>
</feed>"#;
        assert!(looks_like_feed(xml));
        let p = parse_feed("https://example.org/atom", xml).unwrap();
        assert_eq!(p.title, "Contes du soir");
        assert_eq!(p.author.as_deref(), Some("Radio Exemple"));
        assert_eq!(p.image_url.as_deref(), Some("https://example.org/logo.png"));
        assert_eq!(p.episodes.len(), 2);
        assert_eq!(p.episodes[0].title, "Cendrillon");
        assert_eq!(p.episodes[0].date, "2026-07-20");
        assert_eq!(p.episodes[0].guid, "https://example.org/1.mp3");
        assert_eq!(p.episodes[1].guid, "urn:uuid:2");
        assert_eq!(p.episodes[1].url, "https://example.org/2.mp3");
        assert_eq!(p.episodes[1].duration, Some(750));
        assert_eq!(p.episodes[1].size, Some(1234));
    }

    #[test]
    fn apple_search() {
        let json = r#"{"resultCount":3,"results":[
 {"kind":"podcast","collectionName":"Un podcast à soi","artistName":"ARTE Radio",
  "feedUrl":"https://www.arteradio.com/xml","artworkUrl100":"https://img/100.jpg","trackCount":69},
 {"kind":"podcast","collectionName":"Contes du soir","artistName":"Radio Exemple","trackCount":203,
  "collectionViewUrl":"https://podcasts.apple.com/fr/podcast/contes-du-soir/id42?uo=4"},
 {"kind":"podcast-episode","trackName":"Un épisode"}]}"#;
        let r = parse_search(json).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].title, "Un podcast à soi");
        assert_eq!(r[0].author.as_deref(), Some("ARTE Radio"));
        assert_eq!(
            r[0].feed_url.as_deref(),
            Some("https://www.arteradio.com/xml")
        );
        assert_eq!(r[0].artwork_url.as_deref(), Some("https://img/100.jpg"));
        assert_eq!(r[0].episodes, Some(69));
        // feed not given to the API: its Apple Podcasts page
        assert_eq!(r[1].feed_url, None);
        assert_eq!(
            r[1].apple_url.as_deref(),
            Some("https://podcasts.apple.com/fr/podcast/contes-du-soir/id42?uo=4")
        );
        assert!(parse_search("<html>").is_err());
        assert_eq!(
            query_encode("contes à lire & co"),
            "contes+%C3%A0+lire+%26+co"
        );
        assert!(is_address(" https://x"));
        assert!(!is_address("contes du soir"));
    }

    #[test]
    fn apple_lookup() {
        let found =
            r#"{"resultCount":1,"results":[{"kind":"podcast","feedUrl":"https://ex/feed.xml"}]}"#;
        assert_eq!(
            parse_lookup(found).unwrap().as_deref(),
            Some("https://ex/feed.xml")
        );
        // publisher not giving its feed to the API, unknown id, not JSON
        let hidden = r#"{"resultCount":1,"results":[{"kind":"podcast","collectionName":"x"}]}"#;
        assert_eq!(parse_lookup(hidden).unwrap(), None);
        let e = parse_lookup(r#"{"resultCount":0,"results":[]}"#)
            .unwrap_err()
            .to_string();
        assert_eq!(e, "Podcast not found on Apple Podcasts");
        assert!(parse_lookup("<html>").is_err());
    }

    /// The feed in an Apple Podcasts page, among those of other podcasts.
    #[test]
    fn apple_page() {
        let page = r#"<script>{"shelves":[{"items":[
 {"title":"Autre podcast","adamId":"111","feedUrl":"https://ex/autre.xml","showType":"episodic"},
 {"podcastOffer":{"title":"Contes","adamId":"1811582936","feedUrl":"https:\/\/ex\/contes.xml","storeUrl":"https://podcasts.apple.com/x"}},
 {"title":"Sans flux","adamId":"222","showType":"episodic"}]}]}</script>"#;
        assert_eq!(
            apple_page_feed(page, "1811582936").as_deref(),
            Some("https://ex/contes.xml")
        );
        assert_eq!(
            apple_page_feed(page, "111").as_deref(),
            Some("https://ex/autre.xml")
        );
        assert_eq!(apple_page_feed(page, "222"), None);
        assert_eq!(apple_page_feed(page, "333"), None);
    }

    #[test]
    fn apple_podcasts_links() {
        assert_eq!(
            apple_podcast_id("https://podcasts.apple.com/fr/podcast/contes-du-soir/id1469858852"),
            Some(("1469858852".into(), Some("fr".into())))
        );
        assert_eq!(
            apple_podcast_id("https://podcasts.apple.com/us/podcast/x/id123?i=1000700000000"),
            Some(("123".into(), Some("us".into())))
        );
        assert_eq!(
            apple_podcast_id("https://podcasts.apple.com/podcast/id42"),
            Some(("42".into(), None))
        );
        assert_eq!(apple_podcast_id("https://example.org/id42"), None);
        assert_eq!(
            apple_podcast_id("https://podcasts.apple.com/fr/podcast/x"),
            None
        );
    }

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
    fn schema_org_pages() {
        // a radio episode page: the audio in `mainEntity`, the short title in
        // the article headline
        let page = r#"<head><meta property="og:image" content="https://img/x.jpg">
<script type="application/ld+json" data-x="1">{"@context":"https://schema.org","@graph":[
 {"@type":"NewsArticle","headline":"Une étrange coïncidence"},
 {"@type":"RadioEpisode","name":"Une étrange coïncidence : épisode 1/4 du feuilleton | Radio Exemple",
  "dateCreated":"2023-06-24T08:06:00.000Z",
  "mainEntity":{"@type":"AudioObject","contentUrl":"https://media/a.m4a","duration":"P0Y0M0DT0H22M50S"}}
]}</script></head>"#;
        let e = page_episode("https://ex/ep-1", page).unwrap();
        assert_eq!(e.title, "Une étrange coïncidence");
        assert_eq!(e.date, "2023-06-24");
        assert_eq!(e.duration, Some(22 * 60 + 50));
        assert_eq!(e.url, "https://media/a.m4a");
        assert_eq!(e.guid, "https://ex/ep-1");
        assert_eq!(og_meta(page, "image").as_deref(), Some("https://img/x.jpg"));
        // a podcast episode page: `associatedMedia`, a list of types
        let page = r#"<script type="application/ld+json">[{"@type":["PodcastEpisode","CreativeWork"],
 "name":"Le Petit Poucet","datePublished":"2026-07-24",
 "associatedMedia":{"@type":"MediaObject","contentUrl":"https://media/b.mp3","duration":"PT12M"}}]</script>"#;
        let e = page_episode("https://ex/ep-2", page).unwrap();
        assert_eq!(e.title, "Le Petit Poucet");
        assert_eq!(e.date, "2026-07-24");
        assert_eq!(e.duration, Some(720));
        assert_eq!(e.url, "https://media/b.mp3");
        // an invalid JSON-LD block is skipped
        let broken = format!(r#"<script type="application/ld+json">{{"@type":</script>{page}"#);
        assert_eq!(
            page_episode("https://ex/ep-2", &broken).unwrap().url,
            "https://media/b.mp3"
        );
        // a series page: no audio, but the list of its episodes
        let series = r#"<script type="application/ld+json">{"@type":"ItemList","itemListElement":[
 {"@type":"ListItem","position":1,"url":"https://ex/ep-1"},
 {"@type":"ListItem","position":2,"item":{"@id":"/ep-2"}}]}</script>"#;
        assert!(page_episode("https://ex/serie", series).is_none());
        assert_eq!(
            listed_pages("https://ex/serie", series),
            ["https://ex/ep-1", "https://ex/ep-2"]
        );
        assert_eq!(parse_iso_duration("PT1H2M3S"), Some(3723));
    }
}
