//! The Podcasts view: the followed podcasts (their feeds loaded in the
//! background), the episodes of the selected one and their import.

use std::collections::{HashMap, HashSet};

use lunii_core::podcast::{self, Podcast, SavedPodcast};
use slint::ModelRc;
use uuid::Uuid;

use super::{App, sink};
use crate::i18n::{tr, trf};
use crate::worker::{self, Task};
use crate::{EpisodeRow, FollowedRow};

/// The podcast displayed, with what is already on the device.
pub struct PodcastView {
    pub podcast: Podcast,
    /// its album is on the device: the episodes will be appended
    pub on_device: bool,
    /// episodes already in the album
    pub imported: HashSet<String>,
    /// episodes already on the device as stories of their own
    pub stories: HashSet<String>,
}

#[derive(Default)]
pub(super) struct Podcasts {
    followed: Vec<SavedPodcast>,
    /// feeds by address: `None` while loading
    feeds: HashMap<String, Option<Result<Podcast, String>>>,
    /// address typed to follow a podcast, being loaded
    adding: Option<String>,
}

impl App {
    /// The Podcasts view is shown: followed podcasts reloaded, their feeds
    /// loaded once.
    pub(super) fn podcasts_show(&self) {
        let followed = podcast::saved_podcasts();
        let to_load: Vec<String> = {
            let mut st = self.state.borrow_mut();
            let p = &mut st.podcasts;
            let urls: Vec<String> = followed
                .iter()
                .map(|f| f.feed_url.clone())
                .filter(|u| !p.feeds.contains_key(u))
                .collect();
            for u in &urls {
                p.feeds.insert(u.clone(), None);
            }
            p.followed = followed;
            urls
        };
        for url in to_load {
            worker::fetch_feed(url, false, sink());
        }
        let current = self.ui().get_podcast_current();
        if current >= self.state.borrow().podcasts.followed.len() as i32 {
            self.ui().set_podcast_current(-1);
        }
        self.rebuild_followed();
        self.podcast_show_current();
    }

    /// Ctrl+P: the Podcasts view, the address field focused.
    pub(super) fn podcasts_focus_address(&self) {
        let ui = self.ui();
        ui.set_view(1);
        self.podcasts_show();
        // once the view is created
        let weak = self.ui.clone();
        slint::Timer::single_shot(std::time::Duration::from_millis(50), move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_podcast_focus_token(ui.get_podcast_focus_token() + 1);
            }
        });
    }

    fn album_on_device(&self, uuid: &Uuid) -> bool {
        self.state
            .borrow()
            .stories
            .iter()
            .any(|s| s.uuid == *uuid && s.album)
    }

    fn rebuild_followed(&self) {
        let rows: Vec<FollowedRow> = {
            let st = self.state.borrow();
            st.podcasts
                .followed
                .iter()
                .map(|f| {
                    let (info, news) = match st.podcasts.feeds.get(&f.feed_url) {
                        Some(Some(Ok(p))) => self.feed_info(p),
                        Some(Some(Err(_))) => (format!("⚠ {}", tr("Unavailable")), false),
                        _ => (tr("Loading..."), false),
                    };
                    FollowedRow {
                        title: f.title.as_str().into(),
                        info: info.into(),
                        news,
                    }
                })
                .collect()
        };
        self.ui()
            .set_podcast_followed(ModelRc::new(slint::VecModel::from(rows)));
    }

    /// Line under a followed podcast: its new episodes if its album is on
    /// the device, its number of episodes otherwise.
    fn feed_info(&self, p: &Podcast) -> (String, bool) {
        if !self.album_on_device(&p.album_uuid()) {
            return (trf("{} episodes", &[&p.episodes.len().to_string()]), false);
        }
        match podcast::new_episodes(p).len() {
            0 => (tr("Up to date"), false),
            n => (trf("{} new episode(s)", &[&n.to_string()]), true),
        }
    }

    pub(super) fn podcast_select(&self, index: i32) {
        self.ui().set_podcast_current(index);
        self.podcast_show_current();
    }

    /// Displays the episodes of the selected podcast (or its state).
    fn podcast_show_current(&self) {
        let ui = self.ui();
        let current = ui.get_podcast_current();
        let (entry, feed) = {
            let st = self.state.borrow();
            let Some(entry) = usize::try_from(current)
                .ok()
                .and_then(|i| st.podcasts.followed.get(i))
                .cloned()
            else {
                drop(st);
                if self.state.borrow().podcasts.adding.is_none() {
                    ui.set_podcast_loading(false);
                }
                self.state.borrow_mut().podcast = None;
                self.rebuild_podcast_rows();
                return;
            };
            let feed = st.podcasts.feeds.get(&entry.feed_url).cloned().flatten();
            (entry, feed)
        };
        match feed {
            None => {
                ui.set_podcast_loading(true);
                ui.set_podcast_header(entry.title.into());
                self.state.borrow_mut().podcast = None;
            }
            Some(Err(e)) => {
                ui.set_podcast_loading(false);
                ui.set_podcast_header(format!("🛑 {} : {e}", entry.title).into());
                self.state.borrow_mut().podcast = None;
            }
            Some(Ok(p)) => {
                ui.set_podcast_loading(false);
                let view = self.podcast_view(p);
                let mut st = self.state.borrow_mut();
                // same podcast: the selection is kept
                let selected = match st.podcast.take() {
                    Some((old, selected)) if old.podcast.feed_url == view.podcast.feed_url => {
                        selected
                    }
                    _ => {
                        ui.set_podcast_mode(0);
                        ui.set_podcast_filter("".into());
                        HashSet::new()
                    }
                };
                st.podcast = Some((Box::new(view), selected));
                drop(st);
                self.podcast_mode_changed();
                return;
            }
        }
        self.rebuild_podcast_rows();
    }

    /// What of `podcast` is on the device.
    fn podcast_view(&self, podcast: Podcast) -> PodcastView {
        let uuid = podcast.album_uuid();
        let on_device = self.album_on_device(&uuid);
        let stories = {
            let st = self.state.borrow();
            let on_device: HashSet<Uuid> = st.stories.iter().map(|s| s.uuid).collect();
            podcast
                .episodes
                .iter()
                .filter(|e| on_device.contains(&podcast.episode_uuid(e)))
                .map(|e| e.guid.clone())
                .collect()
        };
        PodcastView {
            imported: if on_device {
                podcast::imported_episodes(&uuid)
            } else {
                HashSet::new()
            },
            on_device,
            stories,
            podcast,
        }
    }

    /// ↻: the feed of the selected podcast loaded again.
    pub(super) fn podcast_refresh(&self) {
        let current = self.ui().get_podcast_current();
        let url = {
            let mut st = self.state.borrow_mut();
            let Some(url) = usize::try_from(current)
                .ok()
                .and_then(|i| st.podcasts.followed.get(i))
                .map(|f| f.feed_url.clone())
            else {
                return;
            };
            st.podcasts.feeds.insert(url.clone(), None);
            url
        };
        worker::fetch_feed(url, false, sink());
        self.rebuild_followed();
        self.podcast_show_current();
    }

    /// Follows the podcast of the typed address, once its feed is loaded.
    pub(super) fn podcast_add(&self) {
        let ui = self.ui();
        let url = ui.get_podcast_url().trim().to_string();
        if url.is_empty() || self.state.borrow().podcasts.adding.is_some() {
            return;
        }
        self.state.borrow_mut().podcasts.adding = Some(url.clone());
        ui.set_podcast_adding(true);
        worker::fetch_feed(url, true, sink());
    }

    pub(super) fn feed_loaded(&self, url: String, result: Result<Box<Podcast>, String>) {
        let ui = self.ui();
        let added = self.state.borrow().podcasts.adding.as_deref() == Some(url.as_str());
        if added {
            self.state.borrow_mut().podcasts.adding = None;
            ui.set_podcast_adding(false);
            match result {
                Ok(p) => {
                    if let Err(e) = podcast::follow(&p) {
                        self.status(&format!("🛑 {e:#}"));
                    }
                    let feed_url = p.feed_url.clone();
                    self.state
                        .borrow_mut()
                        .podcasts
                        .feeds
                        .insert(feed_url.clone(), Some(Ok(*p)));
                    ui.set_podcast_url("".into());
                    self.podcasts_show();
                    let index = self
                        .state
                        .borrow()
                        .podcasts
                        .followed
                        .iter()
                        .position(|f| f.feed_url == feed_url);
                    self.podcast_select(index.map_or(-1, |i| i as i32));
                }
                Err(e) => {
                    ui.set_podcast_current(-1);
                    self.state.borrow_mut().podcast = None;
                    self.rebuild_podcast_rows();
                    ui.set_podcast_loading(false);
                    ui.set_podcast_header(format!("🛑 {e}").into());
                }
            }
            return;
        }
        self.state
            .borrow_mut()
            .podcasts
            .feeds
            .insert(url.clone(), Some(result.map(|p| *p)));
        self.rebuild_followed();
        let current = ui.get_podcast_current();
        let shown = usize::try_from(current)
            .ok()
            .and_then(|i| self.state.borrow().podcasts.followed.get(i).cloned())
            .is_some_and(|f| f.feed_url == url);
        if shown {
            self.podcast_show_current();
        }
    }

    pub(super) fn podcast_unfollow(&self) {
        let current = self.ui().get_podcast_current();
        let Some(entry) = usize::try_from(current)
            .ok()
            .and_then(|i| self.state.borrow().podcasts.followed.get(i).cloned())
        else {
            return;
        };
        if let Err(e) = podcast::unfollow(&entry.album) {
            self.status(&format!("🛑 {e:#}"));
            return;
        }
        self.status(&trf("'{}' is no longer followed", &[&entry.title]));
        self.ui().set_podcast_current(-1);
        self.ui().set_podcast_header("".into());
        self.podcasts_show();
    }

    /// After a task (import, update): what is on the device changed.
    pub(super) fn podcasts_refresh_device(&self) {
        if self.ui().get_view() == 1 {
            self.podcasts_show();
        }
    }

    pub(super) fn update_podcast_header(&self) {
        let ui = self.ui();
        let st = self.state.borrow();
        let Some((view, _)) = st.podcast.as_ref() else {
            return;
        };
        let p = &view.podcast;
        let mut header = trf(
            "{} : {} episodes",
            &[&p.title, &p.episodes.len().to_string()],
        );
        if ui.get_podcast_mode() == 1 {
            if !view.stories.is_empty() {
                header.push_str(" · ");
                header.push_str(&trf(
                    "{} episode(s) already on the device",
                    &[&view.stories.len().to_string()],
                ));
            }
        } else if view.on_device {
            header.push_str(" · ");
            header.push_str(&trf(
                "album on the device, the episodes will be appended ({} already imported)",
                &[&view.imported.len().to_string()],
            ));
        }
        ui.set_podcast_header(header.into());
    }

    /// Episodes already on the device, for the import mode chosen.
    pub(super) fn podcast_present(&self, view: &PodcastView) -> HashSet<String> {
        if self.ui().get_podcast_mode() == 1 {
            view.stories.clone()
        } else {
            view.imported.clone()
        }
    }

    pub(super) fn podcast_mode_changed(&self) {
        if let Some((view, selected)) = self.state.borrow_mut().podcast.as_mut() {
            let present = self.podcast_present(view);
            selected.retain(|&i| !present.contains(&view.podcast.episodes[i].guid));
        }
        self.update_podcast_header();
        self.rebuild_podcast_rows();
    }

    /// Indexes of the episodes matching the filter, newest first.
    pub(super) fn podcast_visible(&self) -> Vec<usize> {
        let filter = self.ui().get_podcast_filter().to_lowercase();
        let st = self.state.borrow();
        let Some((view, _)) = st.podcast.as_ref() else {
            return Vec::new();
        };
        (0..view.podcast.episodes.len())
            .rev()
            .filter(|&i| {
                filter.is_empty()
                    || view.podcast.episodes[i]
                        .title
                        .to_lowercase()
                        .contains(&filter)
            })
            .collect()
    }

    pub(super) fn rebuild_podcast_rows(&self) {
        let visible = self.podcast_visible();
        let (rows, count) = {
            let st = self.state.borrow();
            match st.podcast.as_ref() {
                Some((view, selected)) => {
                    let present = self.podcast_present(view);
                    (
                        visible
                            .iter()
                            .map(|&i| {
                                let e = &view.podcast.episodes[i];
                                let date = podcast::display_date(&e.date);
                                let info =
                                    [date, e.duration.map(format_duration).unwrap_or_default()]
                                        .into_iter()
                                        .filter(|s| !s.is_empty())
                                        .collect::<Vec<_>>()
                                        .join(" · ");
                                EpisodeRow {
                                    index: i as i32,
                                    title: e.title.clone().into(),
                                    info: info.into(),
                                    checked: selected.contains(&i),
                                    present: present.contains(&e.guid),
                                }
                            })
                            .collect(),
                        selected.len(),
                    )
                }
                None => (Vec::new(), 0),
            }
        };
        self.podcast_rows.set_vec(rows);
        self.ui().set_podcast_selected_count(count as i32);
    }

    pub(super) fn podcast_toggle(&self, index: usize) {
        if let Some((view, selected)) = self.state.borrow_mut().podcast.as_mut()
            && let Some(e) = view.podcast.episodes.get(index)
            && !self.podcast_present(view).contains(&e.guid)
            && !selected.remove(&index)
        {
            selected.insert(index);
        }
        self.rebuild_podcast_rows();
    }

    /// Selects (or unselects) the displayed episodes not imported yet.
    pub(super) fn podcast_select_all(&self, all: bool) {
        let visible = self.podcast_visible();
        if let Some((view, selected)) = self.state.borrow_mut().podcast.as_mut() {
            let present = self.podcast_present(view);
            for i in visible {
                if present.contains(&view.podcast.episodes[i].guid) {
                    continue;
                }
                if all {
                    selected.insert(i);
                } else {
                    selected.remove(&i);
                }
            }
        }
        self.rebuild_podcast_rows();
    }

    pub(super) fn podcast_import(&self) {
        let Some((podcast, episodes)) =
            self.state
                .borrow_mut()
                .podcast
                .as_mut()
                .map(|(view, selected)| {
                    // chronological order in the album
                    let mut indexes: Vec<usize> = selected.drain().collect();
                    indexes.sort();
                    let episodes = indexes
                        .iter()
                        .map(|&i| view.podcast.episodes[i].clone())
                        .collect::<Vec<_>>();
                    (view.podcast.clone(), episodes)
                })
        else {
            return;
        };
        if episodes.is_empty() {
            return;
        }
        // followed from now on (imported as stories: not recorded otherwise)
        let _ = podcast::follow(&podcast);
        self.rebuild_podcast_rows();
        self.status(&tr("Importing podcast..."));
        self.show_log();
        let update_size = self.ui().get_show_size();
        let as_stories = self.ui().get_podcast_mode() == 1;
        self.launch(Task::ImportPodcast {
            podcast: Box::new(podcast),
            episodes,
            as_stories,
            update_size,
        });
    }
}

/// Episode duration: `8 min`, `1 h 02`.
fn format_duration(secs: u32) -> String {
    let m = secs.div_ceil(60);
    if m < 60 {
        format!("{m} min")
    } else {
        format!("{} h {:02}", m / 60, m % 60)
    }
}
