//! The bits of HTML read in web pages: tags and their attributes, addresses,
//! Open Graph, JSON scripts.

use super::*;

/// Attributes of the `<{name} ...>` tags of `page` (names in lower case,
/// entities decoded), in order.
pub(super) fn html_tags(page: &str, name: &str) -> Vec<HashMap<String, String>> {
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

pub(super) fn html_attributes(mut rest: &str) -> HashMap<String, String> {
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

pub(super) fn html_unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// `href` made absolute against the address of its page.
pub(super) fn absolute(base: &str, href: &str) -> Option<String> {
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
pub(super) fn links<'a>(
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
pub(super) fn feed_in_page(page_url: &str, page: &str) -> Option<String> {
    let tags = html_tags(page, "link");
    ["application/rss+xml", "application/atom+xml"]
        .into_iter()
        .find_map(|kind| links(page_url, &tags, "alternate", Some(kind)).next())
}

/// Next page of a paginated list (`<link rel="next">`).
pub(super) fn next_page(page_url: &str, page: &str) -> Option<String> {
    links(page_url, &html_tags(page, "link"), "next", None).next()
}

/// `content` of `<meta property="og:{name}">` (Open Graph).
pub(super) fn og_meta(page: &str, name: &str) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
