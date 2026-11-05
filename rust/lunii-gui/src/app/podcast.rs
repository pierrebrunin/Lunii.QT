//! The podcast import dialog: loading a feed, choosing the episodes and the
//! import mode.

use std::collections::HashSet;

use super::App;
use crate::EpisodeRow;
use crate::i18n::{tr, trf};
use crate::worker::{PodcastView, Task};

impl App {
    pub(super) fn podcast_load(&self) {
        let ui = self.ui();
        let url = ui.get_podcast_url().trim().to_string();
        if url.is_empty() || self.state.borrow().worker.is_some() {
            return;
        }
        self.state.borrow_mut().podcast = None;
        ui.set_podcast_filter("".into());
        self.rebuild_podcast_rows();
        ui.set_podcast_loading(true);
        ui.set_podcast_header("".into());
        self.launch(Task::FetchPodcast { url });
    }

    pub(super) fn podcast_loaded(&self, result: Result<Box<PodcastView>, String>) {
        let ui = self.ui();
        ui.set_podcast_loading(false);
        match result {
            Ok(view) => {
                // an album by default, on every device
                ui.set_podcast_mode(0);
                self.state.borrow_mut().podcast = Some((view, HashSet::new()));
                self.update_podcast_header();
            }
            Err(e) => {
                ui.set_podcast_header(format!("🛑 {e}").into());
                self.state.borrow_mut().podcast = None;
            }
        }
        self.rebuild_podcast_rows();
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
                                let date = lunii_core::podcast::display_date(&e.date);
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
        let Some((view, selected)) = self.state.borrow_mut().podcast.take() else {
            return;
        };
        // chronological order in the album
        let mut indexes: Vec<usize> = selected.into_iter().collect();
        indexes.sort();
        let episodes = indexes
            .iter()
            .map(|&i| view.podcast.episodes[i].clone())
            .collect();
        self.rebuild_podcast_rows();
        self.status(&tr("Importing podcast..."));
        self.show_log();
        let update_size = self.ui().get_show_size();
        let as_stories = self.ui().get_podcast_mode() == 1;
        self.launch(Task::ImportPodcast {
            podcast: Box::new(view.podcast),
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
