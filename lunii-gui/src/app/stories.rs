//! The story list: rows, selection, details, sizes and durations.

use super::*;

/// Duration of a story or an episode: `8 min`, `1 h 02` (nearest minute, at
/// least 1).
pub(super) fn format_duration(secs: u32) -> String {
    let m = ((secs + 30) / 60).max(1);
    if m < 60 {
        format!("{m} min")
    } else {
        format!("{} h {:02}", m / 60, m % 60)
    }
}

impl App {
    /// Measures in the background the duration of the stories that have none
    /// in the database (once per story: kept for the session).
    pub(super) fn ensure_durations(&self) {
        if !self.ui().get_show_duration() {
            return;
        }
        let (device, missing) = {
            let st = self.state.borrow();
            if st.measuring {
                return;
            }
            let missing: HashSet<Uuid> = st
                .stories
                .iter()
                .filter(|s| s.duration.is_none() && !st.durations.contains_key(&s.uuid))
                .map(|s| s.uuid)
                .collect();
            (st.device.clone(), missing)
        };
        let Some(device) = device.filter(|_| !missing.is_empty()) else {
            return;
        };
        // a task is running: measured once it is done (its end lists the stories)
        let Ok(dev) = Self::lock_device(&device) else {
            return;
        };
        let dirs: Vec<(Uuid, PathBuf)> = dev
            .stories()
            .iter()
            .filter(|s| missing.contains(&s.uuid))
            .map(|s| (s.uuid, dev.story_path(s)))
            .collect();
        drop(dev);
        let generation = {
            let mut st = self.state.borrow_mut();
            st.measuring = true;
            st.durations_generation
        };
        worker::measure_durations(generation, dirs, sink());
    }

    /// The stories may have changed (task, quick change, other device): their
    /// durations are measured again.
    pub(super) fn forget_durations(&self) {
        {
            let mut st = self.state.borrow_mut();
            if st.durations.is_empty() && !st.measuring {
                return;
            }
            st.durations.clear();
            st.durations_generation += 1;
        }
        self.rebuild_rows();
    }

    pub(super) fn rebuild_rows(&self) {
        let ui = self.ui();
        let filter = ui.get_filter().to_lowercase();
        let rows: Vec<StoryRow> = {
            let mut st = self.state.borrow_mut();
            let visible: Vec<usize> = st
                .stories
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    filter.is_empty()
                        || s.name.to_lowercase().contains(&filter)
                        || s.str_uuid.to_lowercase().contains(&filter)
                })
                .map(|(i, _)| i)
                .collect();
            // keep only displayed stories in the selection
            let shown: HashSet<Uuid> = visible.iter().map(|&i| st.stories[i].uuid).collect();
            st.selected.retain(|u| shown.contains(u));
            st.visible = visible;
            if st.current.is_some_and(|c| c >= st.visible.len()) {
                st.current = st.visible.len().checked_sub(1);
            }
            // a shift selection starts again from the clicked row
            if st.anchor.is_some_and(|a| a >= st.visible.len()) {
                st.anchor = None;
            }
            st.visible
                .iter()
                .map(|&i| {
                    let s = &st.stories[i];
                    StoryRow {
                        name: if s.name == STORY_UNKNOWN {
                            tr(STORY_UNKNOWN)
                        } else {
                            s.name.clone()
                        }
                        .into(),
                        uuid: s.str_uuid.clone().into(),
                        size: s
                            .size
                            .map(lunii_core::format::size)
                            .unwrap_or_default()
                            .into(),
                        duration: s
                            .duration
                            .or_else(|| st.durations.get(&s.uuid).copied().flatten())
                            .map(format_duration)
                            .unwrap_or_default()
                            .into(),
                        night_mode: s.night_mode,
                        official: s.official,
                        hidden: s.hidden,
                        selected: st.selected.contains(&s.uuid),
                    }
                })
                .collect()
        };
        self.rows.set_vec(rows);
        self.selection_changed();
        self.ensure_durations();
    }

    pub(crate) fn row_pointer(&self, row: usize, ctrl: bool, shift: bool, right: bool) {
        {
            let mut st = self.state.borrow_mut();
            let Some(&index) = st.visible.get(row) else {
                return;
            };
            let uuid = st.stories[index].uuid;
            if right {
                if !st.selected.contains(&uuid) {
                    st.selected = HashSet::from([uuid]);
                    st.anchor = Some(row);
                }
            } else if shift {
                // (kept in the list by rebuild_rows: checked anyway)
                let anchor = st.anchor.filter(|&a| a < st.visible.len()).unwrap_or(row);
                let (a, b) = (anchor.min(row), anchor.max(row));
                let range: Vec<Uuid> = st.visible[a..=b]
                    .iter()
                    .map(|&i| st.stories[i].uuid)
                    .collect();
                st.selected = range.into_iter().collect();
            } else if ctrl {
                if !st.selected.remove(&uuid) {
                    st.selected.insert(uuid);
                }
                st.anchor = Some(row);
            } else {
                st.selected = HashSet::from([uuid]);
                st.anchor = Some(row);
            }
            st.current = Some(row);
        }
        self.selection_changed();
    }

    pub(super) fn key_nav(&self, delta: i32, shift: bool) {
        let row = {
            let mut st = self.state.borrow_mut();
            if st.visible.is_empty() {
                return;
            }
            let last = st.visible.len() as i64 - 1;
            let row = match st.current {
                Some(c) => (c as i64 + delta as i64).clamp(0, last) as usize,
                None => 0,
            };
            if shift {
                // (kept in the list by rebuild_rows: checked anyway)
                let anchor = st.anchor.filter(|&a| a < st.visible.len()).unwrap_or(row);
                let (a, b) = (anchor.min(row), anchor.max(row));
                let range: Vec<Uuid> = st.visible[a..=b]
                    .iter()
                    .map(|&i| st.stories[i].uuid)
                    .collect();
                st.selected = range.into_iter().collect();
            } else {
                let uuid = st.stories[st.visible[row]].uuid;
                st.selected = HashSet::from([uuid]);
                st.anchor = Some(row);
            }
            st.current = Some(row);
            row
        };
        self.selection_changed();
        self.ui().invoke_ensure_visible(row as i32);
    }

    /// Selected stories (UUID strings), in display order.
    pub(super) fn selected_uuids(&self) -> Vec<String> {
        let st = self.state.borrow();
        st.visible
            .iter()
            .map(|&i| &st.stories[i])
            .filter(|s| st.selected.contains(&s.uuid))
            .map(|s| s.str_uuid.clone())
            .collect()
    }

    pub(super) fn selected_ids(&self) -> Vec<Uuid> {
        let st = self.state.borrow();
        st.visible
            .iter()
            .map(|&i| st.stories[i].uuid)
            .filter(|u| st.selected.contains(u))
            .collect()
    }

    /// Updates rows highlight, menu states and details panel.
    pub(super) fn selection_changed(&self) {
        let ui = self.ui();
        let (
            current,
            has_selection,
            can_export,
            can_import,
            can_export_all,
            can_add_to_album,
            can_add_mp3,
        ) = {
            let st = self.state.borrow();
            for (row, &i) in st.visible.iter().enumerate() {
                let selected = st.selected.contains(&st.stories[i].uuid);
                if let Some(mut data) = self.rows.row_data(row)
                    && data.selected != selected
                {
                    data.selected = selected;
                    self.rows.set_row_data(row, data);
                }
            }
            let can_export = st
                .stories
                .iter()
                .any(|s| st.selected.contains(&s.uuid) && s.exportable);
            let dev = st.dev.as_ref();
            let can_import = dev.is_some_and(|d| d.can_import);
            // a single album selected
            let can_add_to_album = can_import
                && st.selected.len() == 1
                && st
                    .stories
                    .iter()
                    .any(|s| st.selected.contains(&s.uuid) && s.album);
            // not to the album of a podcast: its episodes only
            let can_add_mp3 = can_add_to_album
                && !st
                    .stories
                    .iter()
                    .any(|s| st.selected.contains(&s.uuid) && s.podcast);
            (
                st.current.map(|c| c as i32).unwrap_or(-1),
                !st.selected.is_empty(),
                can_export,
                can_import,
                dev.is_some_and(|d| d.export_all) && !st.stories.is_empty(),
                can_add_to_album,
                can_add_mp3,
            )
        };
        ui.set_current_row(current);
        ui.set_has_selection(has_selection);
        ui.set_selected_count(self.state.borrow().selected.len() as i32);
        ui.set_can_export(can_export);
        ui.set_can_import(can_import);
        ui.set_can_export_all(can_export_all);
        ui.set_can_add_to_album(can_add_to_album);
        ui.set_can_add_mp3(can_add_mp3);
        self.update_device_status();
        self.update_details();
    }

    pub(super) fn update_details(&self) {
        let ui = self.ui();
        let single = {
            let st = self.state.borrow();
            if st.selected.len() == 1 && ui.get_show_details() {
                st.stories
                    .iter()
                    .find(|s| st.selected.contains(&s.uuid))
                    .map(|s| (s.uuid, s.hidden))
            } else {
                None
            }
        };
        let Some((uuid, hidden)) = single else {
            ui.set_details_visible(false);
            self.state.borrow_mut().details_uuid = None;
            self.album_hide();
            return;
        };
        ui.set_details_dimmed(hidden);
        if self.state.borrow().details_uuid == Some(uuid) {
            return;
        }
        self.state.borrow_mut().details_uuid = Some(uuid);
        self.album_load(uuid);
        worker::fetch_details(uuid, sink());
    }

    pub(super) fn details_ready(&self, uuid: Uuid, desc: String, picture: Option<Vec<u8>>) {
        if self.state.borrow().details_uuid != Some(uuid) {
            return;
        }
        let ui = self.ui();
        let image = picture.as_deref().and_then(decode_image);
        let no_desc = desc.is_empty() || desc == DESC_NOT_FOUND;
        let album = ui.get_album_visible();
        // shown even without a description: the panel keeps its place
        let desc = match (no_desc, album) {
            (true, true) => String::new(),
            (true, false) => tr(DESC_NOT_FOUND),
            (false, _) => desc,
        };
        ui.set_description(desc.into());
        ui.set_has_picture(image.is_some());
        ui.set_picture(image.unwrap_or_default());
        ui.set_details_visible(true);
    }

    pub(super) fn move_selection(&self, offset: i32) {
        let ids = self.selected_ids();
        if ids.is_empty() {
            return;
        }
        self.device_op(|d, _| d.move_stories(&ids, offset));
        // keep the first moved story in view
        let row = {
            let st = self.state.borrow();
            st.visible
                .iter()
                .position(|&i| ids.contains(&st.stories[i].uuid))
        };
        if let Some(row) = row {
            self.state.borrow_mut().current = Some(row);
            self.state.borrow_mut().anchor = Some(row);
            self.selection_changed();
            self.ui().invoke_ensure_visible(row as i32);
        }
    }

    /// Selected stories dragged with the mouse, dropped before the row `gap`
    /// of the list displayed (after the last one: `gap` = number of rows).
    pub(super) fn drop_selection(&self, gap: usize) {
        let ids = self.selected_ids();
        let before = {
            let st = self.state.borrow();
            match st.visible.get(gap) {
                Some(&i) => Some(st.stories[i].uuid),
                // after the last row shown: before the story that follows it
                // (the list may be filtered)
                None => st
                    .visible
                    .last()
                    .and_then(|&i| st.stories.get(i + 1))
                    .map(|s| s.uuid),
            }
        };
        if ids.is_empty() {
            return;
        }
        self.device_op(|d, _| d.move_stories_before(&ids, before));
        let row = {
            let st = self.state.borrow();
            st.visible
                .iter()
                .position(|&i| ids.contains(&st.stories[i].uuid))
        };
        if let Some(row) = row {
            let mut st = self.state.borrow_mut();
            st.current = Some(row);
            st.anchor = Some(row);
            drop(st);
            self.selection_changed();
        }
    }

    /// Computes the missing story sizes when they are displayed. A task
    /// running: done when it ends (see `Event::Finished`).
    pub(super) fn ensure_sizes(&self) {
        let needed = {
            let st = self.state.borrow();
            self.ui().get_show_size()
                && st.device.is_some()
                && st.worker.is_none()
                && st.stories.iter().any(|s| s.size.is_none())
        };
        if needed {
            self.launch(Task::Size);
            self.state.borrow_mut().sizing = true;
        }
    }
}
