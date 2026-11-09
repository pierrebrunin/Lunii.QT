//! The Podcasts view: the followed podcasts (their feeds loaded in the
//! background), the episodes of the selected one and their import.

use std::collections::{HashMap, HashSet};

use std::rc::Rc;

use lunii_core::podcast::{self, Podcast, SavedPodcast, SearchResult};
use slint::{Model as _, ModelRc, VecModel};
use uuid::Uuid;

use super::{App, decode_image, format_duration, sink};
use crate::i18n::{tr, trf};
use crate::worker::{self, Task};
use crate::{DialogKind, EpisodeRow, FollowedRow, PodcastResult};

/// The podcast displayed, with what is already on the device.
pub struct PodcastView {
    pub podcast: Podcast,
    /// its album is on the device: the episodes will be appended
    pub on_device: bool,
    /// episodes already in the album
    pub imported: HashSet<String>,
    /// episodes already on the device as stories of their own
    pub stories: HashSet<String>,
    /// episodes of the album whose track is known (`album.json` on the
    /// device: by episode, or by title for albums imported before): they can
    /// be removed
    pub album_removable: HashSet<String>,
    /// episodes on the device unchecked: to remove
    pub removed: HashSet<usize>,
}

#[derive(Default)]
pub(super) struct Podcasts {
    followed: Vec<SavedPodcast>,
    /// feeds by address: `None` while loading
    feeds: HashMap<String, Option<Result<Podcast, String>>>,
    /// "Add a podcast" dialog
    add: AddDialog,
    /// podcast being unfollowed, with its album or stories on the device
    unfollow: Option<(SavedPodcast, Vec<String>)>,
}

/// "Add a podcast" dialog: a search (several results) or an address (one).
#[derive(Default)]
struct AddDialog {
    /// number of the last request: the answers of the previous ones are ignored
    token: u32,
    /// search results (empty for an address)
    results: Vec<SearchResult>,
    /// request of these results (their covers keep coming once one is chosen)
    search_token: u32,
    /// rows displayed
    rows: Rc<VecModel<PodcastResult>>,
    /// feed checked, followed by "Follow"
    podcast: Option<Podcast>,
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
            worker::fetch_feed(url, sink());
        }
        let current = self.ui().get_podcast_current();
        if current >= self.state.borrow().podcasts.followed.len() as i32 {
            self.ui().set_podcast_current(-1);
        }
        self.rebuild_followed();
        self.podcast_show_current();
    }

    /// Ctrl+P: the Podcasts view, with the dialog adding a podcast.
    pub(super) fn podcasts_add_from_menu(&self) {
        let ui = self.ui();
        if ui.get_view() != 1 {
            ui.set_view(1);
            self.podcasts_show();
        }
        self.podcast_add_open();
    }

    fn album_on_device(&self, uuid: &Uuid) -> bool {
        self.state
            .borrow()
            .stories
            .iter()
            .any(|s| s.uuid == *uuid && s.album)
    }

    /// Episodes in the album of `podcast` on the device: as its `album.json`
    /// records them, or as this computer remembers them (albums imported
    /// before, or device busy).
    fn imported_in_album(&self, podcast: &Podcast) -> HashSet<String> {
        self.album_episodes(podcast).0
    }

    /// See [`Self::imported_in_album`]; and those whose track is known on
    /// the device, that can be removed.
    fn album_episodes(&self, podcast: &Podcast) -> (HashSet<String>, HashSet<String>) {
        let uuid = podcast.album_uuid();
        let info = (|| {
            let dev = self.state.borrow().device.clone()?;
            let dev = Self::lock_device(&dev).ok()?;
            let story = dev.stories().get(&uuid.to_string())?.clone();
            dev.album_info(&story)
        })();
        let Some(info) = info else {
            return (podcast::imported_episodes(&uuid), HashSet::new());
        };
        if let Some(episodes) = podcast::album_episodes(&info) {
            return (episodes.clone(), episodes);
        }
        // imported before the episodes were recorded on the device: their
        // tracks found by title
        let by_title: HashSet<String> = podcast
            .episodes
            .iter()
            .filter(|e| info.tracks.contains(&e.title))
            .map(|e| e.guid.clone())
            .collect();
        let mut imported = podcast::imported_episodes(&uuid);
        imported.extend(by_title.iter().cloned());
        (imported, by_title)
    }

    /// The podcasts of the albums on the device, followed if they are not
    /// known yet: imported from another computer.
    pub(super) fn follow_device_podcasts(&self) {
        let found: Vec<(lunii_core::album::meta::PodcastSource, String)> = (|| {
            let dev = self.state.borrow().device.clone()?;
            let dev = Self::lock_device(&dev).ok()?;
            Some(
                dev.stories()
                    .0
                    .iter()
                    .filter_map(|s| Some((dev.album_info(s)?.podcast?, s.name())))
                    .collect(),
            )
        })()
        .unwrap_or_default();
        let mut added = false;
        for (source, title) in found {
            match podcast::follow_album(&source, &title) {
                Ok(true) => {
                    added = true;
                    self.log(
                        lunii_core::Level::Info,
                        &trf("Podcast found on the device, followed: {}", &[&title]),
                    );
                }
                Ok(false) => {}
                Err(e) => self.log(lunii_core::Level::Warn, &format!("{e:#}")),
            }
        }
        if added && self.ui().get_view() == 1 {
            self.podcasts_show();
        }
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
        match podcast::new_episodes(p, &self.imported_in_album(p)).len() {
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
                ui.set_podcast_loading(false);
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
                let mut view = self.podcast_view(p);
                let mut st = self.state.borrow_mut();
                // same podcast: the selection is kept
                let selected = match st.podcast.take() {
                    Some((old, selected)) if old.podcast.feed_url == view.podcast.feed_url => {
                        view.removed = old.removed;
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
        let (imported, album_removable) = if on_device {
            self.album_episodes(&podcast)
        } else {
            (HashSet::new(), HashSet::new())
        };
        PodcastView {
            imported,
            on_device,
            stories,
            album_removable,
            removed: HashSet::new(),
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
        worker::fetch_feed(url, sink());
        self.rebuild_followed();
        self.podcast_show_current();
    }

    pub(super) fn feed_loaded(&self, url: String, result: Result<Box<Podcast>, String>) {
        let ui = self.ui();
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

    // ------------------------------------------------------------ add dialog

    /// "+": the dialog adding a podcast, empty.
    pub(super) fn podcast_add_open(&self) {
        let ui = self.ui();
        let rows = {
            let mut st = self.state.borrow_mut();
            let add = &mut st.podcasts.add;
            add.token += 1;
            add.results.clear();
            add.podcast = None;
            add.rows.set_vec(Vec::new());
            add.rows.clone()
        };
        ui.set_podcast_add_results(ModelRc::from(rows));
        ui.set_podcast_add_query("".into());
        ui.set_podcast_add_selected(-1);
        ui.set_podcast_add_working(false);
        ui.set_podcast_add_can_follow(false);
        self.podcast_add_message("", false);
        ui.set_dialog(DialogKind::AddPodcast);
    }

    fn podcast_add_message(&self, message: &str, error: bool) {
        let ui = self.ui();
        ui.set_podcast_add_message(message.into());
        ui.set_podcast_add_error(error);
    }

    /// A new request: the answers of the previous one will be ignored.
    fn podcast_add_request(&self) -> u32 {
        let mut st = self.state.borrow_mut();
        let add = &mut st.podcasts.add;
        add.token += 1;
        add.podcast = None;
        add.token
    }

    /// Enter / 🔍: an address is loaded, words are searched.
    pub(super) fn podcast_add_submit(&self) {
        let ui = self.ui();
        let query = ui.get_podcast_add_query().trim().to_string();
        if query.is_empty() {
            return;
        }
        let token = self.podcast_add_request();
        {
            let mut st = self.state.borrow_mut();
            st.podcasts.add.results.clear();
            st.podcasts.add.rows.set_vec(Vec::new());
        }
        ui.set_podcast_add_selected(-1);
        ui.set_podcast_add_can_follow(false);
        ui.set_podcast_add_working(true);
        if podcast::is_address(&query) {
            self.podcast_add_message(&tr("Checking the address..."), false);
            worker::check_feed(token, query, true, sink());
        } else {
            self.podcast_add_message(&tr("Searching..."), false);
            worker::search_podcasts(token, query, store_country(), sink());
        }
    }

    pub(super) fn podcast_add_found(&self, token: u32, results: Result<Vec<SearchResult>, String>) {
        if token != self.state.borrow().podcasts.add.token {
            return;
        }
        self.ui().set_podcast_add_working(false);
        let results = match results {
            Ok(results) => results,
            Err(e) => {
                self.podcast_add_message(&format!("🛑 {e}"), true);
                return;
            }
        };
        if results.is_empty() {
            self.podcast_add_message(&tr("No podcast found"), true);
        } else {
            self.podcast_add_message(&tr("Choose a podcast in the list."), false);
        }
        let rows: Vec<PodcastResult> = results
            .iter()
            .map(|r| {
                let mut info: Vec<String> = r.author.iter().cloned().collect();
                if let Some(n) = r.episodes {
                    info.push(trf("{} episodes", &[&n.to_string()]));
                }
                if r.feed_url.is_none() && r.apple_url.is_none() {
                    info.push(tr("feed not public"));
                }
                PodcastResult {
                    title: r.title.as_str().into(),
                    info: info.join(" · ").into(),
                    image: Default::default(),
                    // no feed in the answer: found from the Apple Podcasts page
                    available: r.feed_url.is_some() || r.apple_url.is_some(),
                }
            })
            .collect();
        let mut st = self.state.borrow_mut();
        st.podcasts.add.rows.set_vec(rows);
        st.podcasts.add.results = results;
        st.podcasts.add.search_token = token;
    }

    /// A search result clicked: its feed is checked.
    pub(super) fn podcast_add_choose(&self, index: i32) {
        let Some((address, page)) = usize::try_from(index).ok().and_then(|i| {
            self.state
                .borrow()
                .podcasts
                .add
                .results
                .get(i)
                .and_then(|r| {
                    // (address, may be a page or an Apple Podcasts link)
                    r.feed_url
                        .clone()
                        .map(|f| (f, false))
                        .or_else(|| r.apple_url.clone().map(|a| (a, true)))
                })
        }) else {
            return;
        };
        let token = self.podcast_add_request();
        let ui = self.ui();
        ui.set_podcast_add_selected(index);
        ui.set_podcast_add_can_follow(false);
        ui.set_podcast_add_working(true);
        self.podcast_add_message(&tr("Loading..."), false);
        worker::check_feed(token, address, page, sink());
    }

    pub(super) fn podcast_add_checked(&self, token: u32, result: Result<Box<Podcast>, String>) {
        if token != self.state.borrow().podcasts.add.token {
            return;
        }
        let ui = self.ui();
        ui.set_podcast_add_working(false);
        let p = match result {
            Ok(p) => *p,
            Err(e) => {
                self.podcast_add_message(&format!("🛑 {e}"), true);
                return;
            }
        };
        let mut st = self.state.borrow_mut();
        let followed = st
            .podcasts
            .followed
            .iter()
            .any(|f| f.feed_url == p.feed_url);
        let add = &mut st.podcasts.add;
        // an address: the podcast found is the only row
        if add.results.is_empty() {
            let info: Vec<String> = p
                .author
                .iter()
                .cloned()
                .chain([trf("{} episodes", &[&p.episodes.len().to_string()])])
                .collect();
            add.rows.set_vec(vec![PodcastResult {
                title: p.title.as_str().into(),
                info: info.join(" · ").into(),
                image: Default::default(),
                available: true,
            }]);
            ui.set_podcast_add_selected(0);
        }
        let (message, error) = if p.episodes.is_empty() {
            (tr("This feed has no episode with audio"), true)
        } else {
            let mut m = trf(
                "{} : {} episodes",
                &[&p.title, &p.episodes.len().to_string()],
            );
            if followed {
                m.push_str(" · ");
                m.push_str(&tr("already followed"));
            }
            (m, false)
        };
        ui.set_podcast_add_can_follow(!error);
        add.podcast = (!error).then_some(p);
        drop(st);
        self.podcast_add_message(&message, error);
    }

    pub(super) fn podcast_add_artwork(&self, token: u32, index: usize, data: &[u8]) {
        let st = self.state.borrow();
        let add = &st.podcasts.add;
        // the covers of the results shown, or the one of the podcast at an
        // address (its only row; not the one of a result being checked)
        let wanted = if add.results.is_empty() {
            token == add.token
        } else {
            token == add.search_token
        };
        if !wanted {
            return;
        }
        if let (Some(mut row), Some(image)) = (add.rows.row_data(index), decode_image(data)) {
            row.image = image;
            add.rows.set_row_data(index, row);
        }
    }

    /// "Follow": the podcast checked is followed and selected.
    pub(super) fn podcast_add_follow(&self) {
        let Some(p) = self.state.borrow_mut().podcasts.add.podcast.take() else {
            return;
        };
        let ui = self.ui();
        ui.set_dialog(DialogKind::None);
        if let Err(e) = podcast::follow(&p) {
            self.status(&format!("🛑 {e:#}"));
            return;
        }
        let feed_url = p.feed_url.clone();
        self.state
            .borrow_mut()
            .podcasts
            .feeds
            .insert(feed_url.clone(), Some(Ok(p)));
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

    /// "Unfollow": what of the podcast is on the device is removed or kept,
    /// as asked; nothing on it, the podcast is unfollowed right away.
    pub(super) fn podcast_unfollow(&self) {
        let current = self.ui().get_podcast_current();
        let Some(entry) = usize::try_from(current)
            .ok()
            .and_then(|i| self.state.borrow().podcasts.followed.get(i).cloned())
        else {
            return;
        };
        let on_device = self.podcast_stories_on_device(&entry);
        if on_device.is_empty() {
            self.podcast_unfollow_done(&entry);
            return;
        }
        let mut message = trf("'{}' is on the device :\n", &[&entry.title]);
        for (_, name) in &on_device {
            message += &format!("- {name}\n");
        }
        if message.chars().count() > 512 {
            message = message.chars().take(512).collect::<String>() + "...";
            message += &tr("\n(and too many others)\n");
        }
        message += &tr("\nRemove it from the device too ?");
        let uuids = on_device.into_iter().map(|(uuid, _)| uuid).collect();
        self.state.borrow_mut().podcasts.unfollow = Some((entry, uuids));
        let ui = self.ui();
        ui.set_confirm_message(message.into());
        ui.set_dialog(DialogKind::UnfollowPodcast);
    }

    /// The podcast unfollowed after the dialog: its album or stories removed
    /// from the device if `remove`.
    pub(super) fn podcast_unfollow_accepted(&self, remove: bool) {
        let Some((entry, uuids)) = self.state.borrow_mut().podcasts.unfollow.take() else {
            return;
        };
        if !self.podcast_unfollow_done(&entry) {
            return;
        }
        if remove {
            self.launch(Task::Remove { uuids });
        }
    }

    /// Album and stories of `entry` on the device (uuid, name): the stories
    /// of its episodes known once its feed is loaded.
    fn podcast_stories_on_device(&self, entry: &SavedPodcast) -> Vec<(String, String)> {
        let st = self.state.borrow();
        let episodes: HashSet<Uuid> = match st.podcasts.feeds.get(&entry.feed_url) {
            Some(Some(Ok(p))) => p.episodes.iter().map(|e| p.episode_uuid(e)).collect(),
            _ => HashSet::new(),
        };
        st.stories
            .iter()
            .filter(|s| s.uuid == entry.album || episodes.contains(&s.uuid))
            .map(|s| (s.str_uuid.clone(), s.name.clone()))
            .collect()
    }

    /// `entry` no longer followed: false if it could not be recorded.
    fn podcast_unfollow_done(&self, entry: &SavedPodcast) -> bool {
        if let Err(e) = podcast::unfollow(&entry.album) {
            self.status(&format!("🛑 {e:#}"));
            return false;
        }
        self.status(&trf("'{}' is no longer followed", &[&entry.title]));
        self.ui().set_podcast_current(-1);
        self.ui().set_podcast_header("".into());
        self.podcasts_show();
        true
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

    /// The episode `index` on the device can be unchecked, to remove it.
    fn podcast_removable(&self, view: &PodcastView, index: usize) -> bool {
        self.ui().get_podcast_mode() == 1
            || view
                .album_removable
                .contains(&view.podcast.episodes[index].guid)
    }

    pub(super) fn podcast_mode_changed(&self) {
        if let Some((view, selected)) = self.state.borrow_mut().podcast.as_mut() {
            let present = self.podcast_present(view);
            let episodes = &view.podcast.episodes;
            selected.retain(|&i| !present.contains(&episodes[i].guid));
            let removed: HashSet<usize> = view
                .removed
                .iter()
                .copied()
                .filter(|&i| present.contains(&episodes[i].guid) && self.podcast_removable(view, i))
                .collect();
            view.removed = removed;
        }
        self.update_podcast_header();
        self.rebuild_podcast_rows();
    }

    /// Indexes of the episodes matching the filter, in the order of the
    /// column chosen (newest first by default).
    pub(super) fn podcast_visible(&self) -> Vec<usize> {
        let ui = self.ui();
        let filter = ui.get_podcast_filter().to_lowercase();
        let st = self.state.borrow();
        let Some((view, _)) = st.podcast.as_ref() else {
            return Vec::new();
        };
        let episodes = &view.podcast.episodes;
        let mut visible: Vec<usize> = (0..episodes.len())
            .rev()
            .filter(|&i| filter.is_empty() || episodes[i].title.to_lowercase().contains(&filter))
            .collect();
        let present = self.podcast_present(view);
        let column = SortColumn::from(ui.get_podcast_sort_column());
        // stable: the same keys stay newest first
        visible.sort_by(|&a, &b| {
            let (ea, eb) = (&episodes[a], &episodes[b]);
            let order = match column {
                SortColumn::Title => ea.title.to_lowercase().cmp(&eb.title.to_lowercase()),
                SortColumn::Duration => ea.duration.cmp(&eb.duration),
                SortColumn::Date => ea.date.cmp(&eb.date).then(a.cmp(&b)),
                SortColumn::Device => present.contains(&ea.guid).cmp(&present.contains(&eb.guid)),
            };
            if ui.get_podcast_sort_ascending() {
                order
            } else {
                order.reverse()
            }
        });
        visible
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
                                EpisodeRow {
                                    index: i as i32,
                                    title: e.title.clone().into(),
                                    duration: e
                                        .duration
                                        .map(format_duration)
                                        .unwrap_or_default()
                                        .into(),
                                    date: podcast::display_date(&e.date).into(),
                                    checked: selected.contains(&i),
                                    present: present.contains(&e.guid),
                                    removable: self.podcast_removable(view, i),
                                    remove: view.removed.contains(&i),
                                }
                            })
                            .collect(),
                        (selected.len(), view.removed.len()),
                    )
                }
                None => (Vec::new(), (0, 0)),
            }
        };
        self.podcast_rows.set_vec(rows);
        self.ui().set_podcast_selected_count(count.0 as i32);
        self.ui().set_podcast_remove_count(count.1 as i32);
    }

    /// An episode checked or unchecked: to import if it is not on the
    /// device, to remove (unchecked) if it is.
    pub(super) fn podcast_toggle(&self, index: usize) {
        if let Some((view, selected)) = self.state.borrow_mut().podcast.as_mut()
            && let Some(e) = view.podcast.episodes.get(index)
        {
            let set = if !self.podcast_present(view).contains(&e.guid) {
                selected
            } else if self.podcast_removable(view, index) {
                &mut view.removed
            } else {
                return;
            };
            if !set.remove(&index) {
                set.insert(index);
            }
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

    /// "Import": the episodes unchecked removed after a confirmation, then
    /// the checked ones imported.
    pub(super) fn podcast_apply(&self) {
        let titles: Vec<String> = {
            let st = self.state.borrow();
            let Some((view, _)) = st.podcast.as_ref() else {
                return;
            };
            let mut indexes: Vec<usize> = view.removed.iter().copied().collect();
            indexes.sort();
            indexes
                .iter()
                .map(|&i| view.podcast.episodes[i].title.clone())
                .collect()
        };
        if titles.is_empty() {
            self.podcast_import();
            return;
        }
        let mut message = trf(
            "You are requesting to remove from the device {} episode(s) :\n",
            &[&titles.len().to_string()],
        );
        for t in &titles {
            message += &format!("- {t}\n");
        }
        if message.chars().count() > 512 {
            message = message.chars().take(512).collect::<String>() + "...";
            message += &tr("\n(and too many others)\n");
        }
        message += &tr("\nDo you want to proceed ?");
        self.state.borrow_mut().confirm_podcast = true;
        let ui = self.ui();
        ui.set_confirm_message(message.into());
        ui.set_dialog(DialogKind::ConfirmRemove);
    }

    pub(super) fn podcast_import(&self) {
        let Some((podcast, episodes, remove)) =
            self.state
                .borrow_mut()
                .podcast
                .as_mut()
                .map(|(view, selected)| {
                    // chronological order in the album
                    let pick = |set: &mut HashSet<usize>| {
                        let mut indexes: Vec<usize> = set.drain().collect();
                        indexes.sort();
                        indexes
                            .iter()
                            .map(|&i| view.podcast.episodes[i].clone())
                            .collect::<Vec<_>>()
                    };
                    let episodes = pick(selected);
                    let remove = pick(&mut view.removed);
                    (view.podcast.clone(), episodes, remove)
                })
        else {
            return;
        };
        if episodes.is_empty() && remove.is_empty() {
            return;
        }
        // followed from now on (imported as stories: not recorded otherwise)
        let _ = podcast::follow(&podcast);
        self.rebuild_podcast_rows();
        self.status(&if episodes.is_empty() {
            tr("Removing episodes...")
        } else {
            tr("Importing podcast...")
        });
        self.show_log();
        let update_size = self.ui().get_show_size();
        let as_stories = self.ui().get_podcast_mode() == 1;
        self.launch(Task::ImportPodcast {
            podcast: Box::new(podcast),
            episodes,
            remove,
            as_stories,
            update_size,
        });
    }
}

/// Column the episodes are sorted by (`sort-column` of the view).
enum SortColumn {
    Title,
    Duration,
    Date,
    Device,
}

impl From<i32> for SortColumn {
    fn from(column: i32) -> Self {
        match column {
            0 => Self::Title,
            1 => Self::Duration,
            3 => Self::Device,
            _ => Self::Date,
        }
    }
}

/// Apple Podcasts catalog searched: the region of the user locale (`fr_FR`,
/// `fr-CA`), or `fr` / `us` from the language.
fn store_country() -> String {
    country_of(&crate::i18n::system_language())
}

fn country_of(locale: &str) -> String {
    let mut parts = locale.split(['-', '_', '.', '@']);
    let language = parts.next().unwrap_or("").to_ascii_lowercase();
    match parts.next() {
        Some(region) if region.len() == 2 && region.bytes().all(|b| b.is_ascii_alphabetic()) => {
            region.to_ascii_lowercase()
        }
        _ if language == "fr" => "fr".into(),
        _ => "us".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_store_country() {
        assert_eq!(country_of("fr_FR.UTF-8"), "fr");
        assert_eq!(country_of("fr-CA"), "ca");
        assert_eq!(country_of("fr"), "fr");
        assert_eq!(country_of("en_GB"), "gb");
        assert_eq!(country_of("en"), "us");
        assert_eq!(country_of("C"), "us");
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(10), "1 min");
        assert_eq!(format_duration(200), "3 min");
        assert_eq!(format_duration(8 * 60), "8 min");
        assert_eq!(format_duration(3723), "1 h 02");
        assert_eq!(format_duration(3600 + 30 * 60), "1 h 30");
    }
}
