//! Apple Podcasts: links, search and feed of a podcast (iTunes API, or its
//! page when the API does not give it).

use super::*;

/// `(id, store country)` of an Apple Podcasts link
/// (`https://podcasts.apple.com/fr/podcast/contes-du-soir/id1469858852`).
pub(super) fn apple_podcast_id(address: &str) -> Option<(String, Option<String>)> {
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
pub(super) fn apple_feed_url(id: &str, country: Option<&str>) -> Result<String> {
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
pub(super) fn parse_lookup(json: &str) -> Result<Option<String>> {
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

/// Feed address of the podcast `id` in its Apple Podcasts page: the JSON data
/// of the page hold, among those of the other podcasts shown, an object with
/// its id (`adamId`) and its `feedUrl`.
pub(super) fn apple_page_feed(page: &str, id: &str) -> Option<String> {
    let mut stack = script_json(page, "application/json");
    while let Some(value) = stack.pop() {
        match value {
            Value::Array(items) => stack.extend(items),
            Value::Object(object) => {
                let same_id = match object.get("adamId") {
                    Some(Value::String(a)) => a == id,
                    Some(Value::Number(n)) => n.to_string() == id,
                    _ => false,
                };
                if let (true, Some(Value::String(feed))) = (same_id, object.get("feedUrl"))
                    && feed.starts_with("http")
                {
                    return Some(feed.clone());
                }
                stack.extend(object.into_iter().map(|(_, v)| v));
            }
            _ => {}
        }
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
pub(super) fn query_encode(value: &str) -> String {
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

pub(super) fn parse_search(json: &str) -> Result<Vec<SearchResult>> {
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

#[cfg(test)]
mod tests {
    use super::*;
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
        let page = r#"<script type="application/json" id="serialized-server-data">{"shelves":[{"items":[
 {"title":"Autre podcast","adamId":"111","feedUrl":"https://ex/autre.xml"},
 {"related":{"feedUrl":"https://ex/voisin.xml"},"podcastOffer":{"title":"Contes","adamId":"1811582936",
   "artwork":{"url":"https://img"},"feedUrl":"https:\/\/ex\/contes.xml?a=1\u0026b=2"}},
 {"title":"Sans flux","adamId":222}]}]}</script>
<script type="application/json">{invalide</script>"#;
        // nested objects around, escaped characters
        assert_eq!(
            apple_page_feed(page, "1811582936").as_deref(),
            Some("https://ex/contes.xml?a=1&b=2")
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
}
