//! Podcasts of web pages without feed, read from their schema.org data.

use super::*;

/// The schema.org objects of a page (`<script type="application/ld+json">`,
/// `@graph` and arrays flattened).
pub(super) fn json_ld(page: &str) -> Vec<Value> {
    let mut objects = Vec::new();
    for value in script_json(page, "application/ld+json") {
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

/// The JSON contents of the `<script type="{kind}">` of a page (invalid ones
/// skipped).
pub(super) fn script_json(page: &str, kind: &str) -> Vec<Value> {
    let lower = page.to_ascii_lowercase();
    let mut values = Vec::new();
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
            .is_some_and(|t| t.eq_ignore_ascii_case(kind))
        {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(&page[start + open + 1..start + close]) {
            values.push(value);
        }
    }
    values
}

/// `@type` of a schema.org object (a name or a list of names).
pub(super) fn types(object: &Value) -> Vec<&str> {
    match object.get("@type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

/// The objects of a type matching `kind`.
pub(super) fn of_type<'a>(
    objects: &'a [Value],
    kind: impl Fn(&str) -> bool + 'a,
) -> impl Iterator<Item = &'a Value> + 'a {
    objects
        .iter()
        .filter(move |o| types(o).into_iter().any(&kind))
}

pub(super) fn str_field(object: &Value, name: &str) -> Option<String> {
    object
        .get(name)
        .and_then(Value::as_str)
        .map(|s| s.replace('\u{a0}', " ").trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The shortest of the names a page gives to a thing: pages often add the name
/// of the site (`Title | Site`) or words for search engines to one of them.
pub(super) fn shortest_name(names: impl IntoIterator<Item = String>) -> Option<String> {
    names
        .into_iter()
        .map(|n| n.split(" | ").next().unwrap_or(&n).trim().to_string())
        .filter(|n| n.chars().count() >= 2)
        .min_by_key(|n| n.chars().count())
}

/// ISO 8601 duration (`P0Y0M0DT0H22M50S`, `PT22M50.5S`, `P1DT2H`) in seconds.
pub(super) fn parse_iso_duration(value: &str) -> Option<u32> {
    let rest = value.trim().strip_prefix('P')?;
    let (date, time) = rest.split_once('T').unwrap_or((rest, ""));
    let mut total = 0f64;
    let mut parts = 0;
    for (text, in_time) in [(date, false), (time, true)] {
        let mut number = String::new();
        for c in text.chars() {
            if c.is_ascii_digit() || c == '.' || c == ',' {
                number.push(if c == ',' { '.' } else { c });
                continue;
            }
            let n: f64 = number.parse().ok()?;
            number.clear();
            total += n * match (c, in_time) {
                ('Y', false) => 365.0 * 86400.0,
                ('M', false) => 30.0 * 86400.0,
                ('W', false) => 7.0 * 86400.0,
                ('D', false) => 86400.0,
                ('H', true) => 3600.0,
                ('M', true) => 60.0,
                ('S', true) => 1.0,
                _ => return None,
            };
            parts += 1;
        }
        // a number without its unit
        if !number.is_empty() {
            return None;
        }
    }
    (parts > 0).then(|| total.round() as u32)
}

/// The audio of an episode (`AudioObject` with a `contentUrl`).
pub(super) fn episode_audio(episode: &Value) -> Option<&Value> {
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
pub(super) fn page_episode(url: &str, page: &str) -> Option<Episode> {
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
pub(super) fn listed_pages(page_url: &str, page: &str) -> Vec<String> {
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

/// Episode pages loaded at the same time.
pub(super) const PAGES_AT_ONCE: usize = 6;

/// A podcast made from a page without feed that lists episode pages (and its
/// next pages): each episode is read from its page. The listed pages that are
/// not episodes (other series...) are skipped.
pub(super) fn page_podcast(page_url: &str, page: &str) -> Result<Podcast> {
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

    // a few pages at a time: a site may refuse too many requests at once
    let mut episodes: Vec<Option<Episode>> = Vec::with_capacity(urls.len());
    for chunk in urls.chunks(PAGES_AT_ONCE) {
        std::thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|url| {
                    scope.spawn(move || get_text(url).ok().and_then(|p| page_episode(url, &p)))
                })
                .collect();
            episodes.extend(handles.into_iter().map(|h| h.join().ok().flatten()));
        });
    }
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

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(parse_iso_duration("PT22M50.5S"), Some(1371));
        assert_eq!(parse_iso_duration("P1DT2H"), Some(93_600));
        assert_eq!(parse_iso_duration("P0Y0M0DT0H22M50S"), Some(1370));
        assert_eq!(parse_iso_duration("PT"), None);
        assert_eq!(parse_iso_duration("PT12"), None);
        assert_eq!(parse_iso_duration("22:50"), None);
    }
}
