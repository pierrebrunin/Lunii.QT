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

mod apple;
mod feed;
mod html;
mod import;
mod page;
mod state;

pub use apple::*;
pub use feed::*;
pub use import::*;
pub use state::*;
// private to the module
use html::*;
use page::*;

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
    /// other addresses of the feed, declared by it (`<itunes:new-feed-url>`,
    /// `<atom:link rel="self">`): the same podcast reached from elsewhere
    pub aliases: Vec<String>,
    pub title: String,
    pub author: Option<String>,
    pub image_url: Option<String>,
    /// web page of the podcast (`<link>` of the feed, or the page read)
    pub page_url: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
