//! RSS 2.0 (with the iTunes tags) and Atom feeds.

use super::*;

pub(super) const ITUNES: &str = "http://www.itunes.com/dtds/podcast-1.0.dtd";
pub(super) const ATOM: &str = "http://www.w3.org/2005/Atom";

pub(super) fn month(name: &str) -> Option<u32> {
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
pub(super) fn parse_date(date: &str) -> (String, String) {
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
pub(super) fn parse_duration(d: &str) -> Option<u32> {
    let mut secs = 0u32;
    for part in d.trim().split(':') {
        secs = secs
            .checked_mul(60)?
            .checked_add(part.trim().parse::<f32>().ok()? as u32)?;
    }
    Some(secs)
}

/// `itunes:<name>` child of `node`.
pub(super) fn itunes<'a, 'i>(
    node: roxmltree::Node<'a, 'i>,
    name: &str,
) -> Option<roxmltree::Node<'a, 'i>> {
    node.children().find(|c| c.has_tag_name((ITUNES, name)))
}

/// `<atom:link rel="self" href="…">` of a channel or Atom feed.
fn self_link(node: roxmltree::Node) -> Option<String> {
    node.children()
        .find(|c| c.has_tag_name((ATOM, "link")) && c.attribute("rel") == Some("self"))
        .and_then(|l| l.attribute("href"))
        .map(str::to_string)
}

/// Web addresses among `declared`, other than `feed_url`, without duplicates.
fn aliases(feed_url: &str, declared: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for url in declared.map(|u| u.trim().to_string()) {
        if (url.starts_with("https://") || url.starts_with("http://"))
            && url != feed_url
            && !out.contains(&url)
        {
            out.push(url);
        }
    }
    out
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
                link: child_text(item, "link").filter(|l| is_web(l)),
            },
        ));
    }
    // feeds list the newest first: reversed, then sorted by date (stable)
    episodes.reverse();
    episodes.sort_by(|a, b| a.0.cmp(&b.0));

    let declared = itunes(channel, "new-feed-url")
        .and_then(|n| n.text())
        .map(str::to_string)
        .into_iter()
        .chain(self_link(channel));
    Ok(Podcast {
        feed_url: feed_url.to_string(),
        aliases: aliases(feed_url, declared),
        title: child_text(channel, "title").unwrap_or_else(|| "Podcast".into()),
        author: itunes(channel, "author")
            .and_then(|a| a.text())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty()),
        image_url,
        page_url: child_text(channel, "link").filter(|l| is_web(l)),
        episodes: episodes.into_iter().map(|(_, e)| e).collect(),
    })
}

/// An `http(s)://` address.
fn is_web(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

/// `<link rel="alternate">` (the default relation) of an Atom feed or entry.
fn alternate_link(node: roxmltree::Node) -> Option<String> {
    node.children()
        .filter(|c| c.has_tag_name((ATOM, "link")))
        .find(|c| c.attribute("rel").is_none_or(|r| r == "alternate"))
        .and_then(|c| c.attribute("href"))
        .filter(|l| is_web(l))
        .map(str::to_string)
}

/// Atom feed (RFC 4287): entries with a `<link rel="enclosure">`.
pub(super) fn parse_atom(feed_url: &str, feed: roxmltree::Node) -> Result<Podcast> {
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
                link: alternate_link(entry),
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
        aliases: aliases(feed_url, self_link(feed).into_iter()),
        title: atom(feed, "title").unwrap_or_else(|| "Podcast".into()),
        author: itunes(feed, "author")
            .and_then(|a| a.text())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .or_else(|| author(feed)),
        image_url,
        page_url: alternate_link(feed),
        episodes: episodes.into_iter().map(|(_, e)| e).collect(),
    })
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
        assert!(looks_like_feed(FEED));
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

    /// The web page of the podcast: `<link>` of an RSS channel (not the
    /// `<atom:link>` of the feed itself), `rel="alternate"` in Atom.
    #[test]
    fn page_address() {
        let rss = r#"<rss xmlns:atom="http://www.w3.org/2005/Atom"><channel><title>T</title>
            <atom:link rel="self" href="https://ex.org/feed.xml"/>
            <link>https://ex.org/podcast</link></channel></rss>"#;
        let p = parse_feed("https://ex.org/feed.xml", rss).unwrap();
        assert_eq!(p.page_url.as_deref(), Some("https://ex.org/podcast"));
        let rss = r#"<rss><channel><title>T</title><item><title>E</title>
            <link>https://ex.org/podcast/e</link><enclosure url="https://ex.org/e.mp3"/>
            </item></channel></rss>"#;
        let p = parse_feed("https://ex.org/feed.xml", rss).unwrap();
        assert_eq!(
            p.episodes[0].link.as_deref(),
            Some("https://ex.org/podcast/e")
        );
        let rss = "<rss><channel><title>T</title><link>/podcast</link></channel></rss>";
        assert_eq!(
            parse_feed("https://ex.org/f.xml", rss).unwrap().page_url,
            None
        );
        let atom = r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>T</title>
            <link rel="self" href="https://ex.org/atom.xml"/>
            <link href="https://ex.org/page"/></feed>"#;
        let p = parse_feed("https://ex.org/atom.xml", atom).unwrap();
        assert_eq!(p.page_url.as_deref(), Some("https://ex.org/page"));
    }

    /// The other addresses a feed declares: the same podcast elsewhere.
    #[test]
    fn declared_addresses() {
        let xml = r#"<rss xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd"
            xmlns:atom="http://www.w3.org/2005/Atom"><channel><title>T</title>
            <atom:link rel="self" href="https://rf/podcast_1.xml"/>
            <itunes:new-feed-url> https://rf/abc/podcast_1.xml </itunes:new-feed-url>
            </channel></rss>"#;
        let short = parse_feed("https://rf/podcast_1.xml", xml).unwrap();
        assert_eq!(short.aliases, ["https://rf/abc/podcast_1.xml"]);
        let long = parse_feed("https://rf/abc/podcast_1.xml", xml).unwrap();
        assert_eq!(long.aliases, ["https://rf/podcast_1.xml"]);
    }
}
