//! MP3 albums created by Lunii: their track list in the details panel,
//! where tracks are moved, removed or added, then applied in one go.

use lunii_core::album::{self, AlbumEntry, Chapter, meta::AlbumInfo};
use slint::ModelRc;
use uuid::Uuid;

use super::App;
use crate::AlbumTrackRow;
use crate::i18n::{tr, trf};
use crate::worker::Task;

/// The album shown in the details panel.
pub(super) struct AlbumEdit {
    uuid: Uuid,
    /// titles on the device (empty when unknown)
    titles: Vec<String>,
    /// sub-folders: read only
    chapters: Vec<Chapter>,
    editable: bool,
    /// track list being edited, with the titles to display
    tracks: Vec<(AlbumEntry, String)>,
}

impl AlbumEdit {
    fn new(uuid: Uuid, info: AlbumInfo, editable: bool) -> Self {
        let mut edit = Self {
            uuid,
            titles: info.tracks,
            chapters: info.chapters,
            editable,
            tracks: Vec::new(),
        };
        edit.revert();
        edit
    }

    /// The track list of the device.
    fn revert(&mut self) {
        self.tracks = self
            .titles
            .iter()
            .enumerate()
            .map(|(i, title)| (AlbumEntry::Existing(i), track_title(title, i)))
            .collect();
    }

    /// The track list differs from the device.
    fn modified(&self) -> bool {
        self.tracks.len() != self.titles.len()
            || self
                .tracks
                .iter()
                .enumerate()
                .any(|(i, (e, _))| !matches!(e, AlbumEntry::Existing(j) if *j == i))
    }

    fn rows(&self) -> Vec<AlbumTrackRow> {
        let row = |number: usize, title: &str, new: bool| AlbumTrackRow {
            number: format!("{number}.").into(),
            title: title.into(),
            new,
            header: false,
        };
        if self.editable || self.chapters.is_empty() {
            return self
                .tracks
                .iter()
                .enumerate()
                .map(|(i, (e, title))| row(i + 1, title, matches!(e, AlbumEntry::New(_))))
                .collect();
        }
        // sub-folders: their title, then their tracks
        let mut rows = Vec::new();
        let mut start = 0;
        for chapter in &self.chapters {
            rows.push(AlbumTrackRow {
                number: "".into(),
                title: chapter.title.as_str().into(),
                new: false,
                header: true,
            });
            let end = (start + chapter.len).min(self.tracks.len());
            for (k, (_, title)) in self.tracks[start..end].iter().enumerate() {
                rows.push(row(k + 1, title, false));
            }
            start = end;
        }
        rows
    }
}

/// Title of a track, a placeholder when it is unknown (albums imported before
/// the titles were saved).
fn track_title(title: &str, index: usize) -> String {
    if title.is_empty() {
        trf("Track {}", &[&(index + 1).to_string()])
    } else {
        title.to_string()
    }
}

impl App {
    /// Shows the tracks of `uuid` in the details panel if it is an album
    /// (`false` otherwise, or when the device is busy).
    pub(super) fn album_load(&self, uuid: Uuid) -> bool {
        let found = (|| {
            let dev = self.state.borrow().device.clone()?;
            let dev = Self::lock_device(&dev).ok()?;
            let story = dev.stories().get(&uuid.to_string())?.clone();
            let info = dev.album_info(&story)?;
            Some(AlbumEdit::new(
                uuid,
                info,
                dev.album_track_count(&story).is_some(),
            ))
        })();
        let ui = self.ui();
        let shown = found.is_some();
        if let Some(edit) = &found {
            ui.set_album_editable(edit.editable);
        }
        self.state.borrow_mut().album = found;
        ui.set_album_current(-1);
        self.album_refresh();
        ui.set_album_visible(shown);
        shown
    }

    pub(super) fn album_hide(&self) {
        self.state.borrow_mut().album = None;
        self.ui().set_album_visible(false);
    }

    fn album_refresh(&self) {
        let (rows, modified) = self
            .state
            .borrow()
            .album
            .as_ref()
            .map(|a| (a.rows(), a.modified()))
            .unwrap_or_default();
        let ui = self.ui();
        ui.set_album_rows(ModelRc::new(slint::VecModel::from(rows)));
        ui.set_album_modified(modified);
    }

    /// Changes the track list (`change` returns the new current row).
    fn album_change(&self, change: impl FnOnce(&mut Vec<(AlbumEntry, String)>, i32) -> i32) {
        let ui = self.ui();
        let current = {
            let mut st = self.state.borrow_mut();
            let Some(album) = st.album.as_mut().filter(|a| a.editable) else {
                return;
            };
            change(&mut album.tracks, ui.get_album_current())
        };
        self.album_refresh();
        ui.set_album_current(current);
    }

    pub(super) fn album_move(&self, delta: i32) {
        self.album_change(|tracks, current| {
            let target = current + delta;
            if current < 0 || target < 0 || target as usize >= tracks.len() {
                return current;
            }
            tracks.swap(current as usize, target as usize);
            target
        });
    }

    pub(super) fn album_remove(&self) {
        self.album_change(|tracks, current| {
            if current < 0 || current as usize >= tracks.len() || tracks.len() < 2 {
                return current;
            }
            tracks.remove(current as usize);
            current.min(tracks.len() as i32 - 1)
        });
    }

    pub(super) fn album_revert(&self) {
        if let Some(album) = self.state.borrow_mut().album.as_mut() {
            album.revert();
        }
        self.ui().set_album_current(-1);
        self.album_refresh();
    }

    pub(super) fn album_add_files(&self) {
        let Some(files) = rfd::FileDialog::new()
            .set_title(tr("MP3 files to add to the album"))
            .add_filter("MP3", &["mp3", "MP3"])
            .pick_files()
        else {
            return;
        };
        let added = match album::scan_files(&files) {
            Ok(a) => a.tracks,
            Err(e) => {
                self.status(&format!("🛑 {e:#}"));
                return;
            }
        };
        self.album_change(|tracks, _| {
            tracks.extend(added.into_iter().map(|t| {
                let title = t.title.clone();
                (AlbumEntry::New(t), title)
            }));
            tracks.len() as i32 - 1
        });
    }

    pub(super) fn album_apply(&self) {
        let Some((uuid, entries)) = self.state.borrow().album.as_ref().map(|a| {
            (
                a.uuid,
                a.tracks.iter().map(|(e, _)| e.clone()).collect::<Vec<_>>(),
            )
        }) else {
            return;
        };
        let update_size = self.ui().get_show_size();
        self.launch(Task::EditAlbum {
            uuid,
            entries,
            update_size,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(edit: &AlbumEdit) -> Vec<String> {
        edit.rows().iter().map(|r| r.title.to_string()).collect()
    }

    #[test]
    fn rows_and_changes() {
        let info = AlbumInfo {
            tracks: vec!["Un".into(), String::new()],
            chapters: Vec::new(),
        };
        let mut edit = AlbumEdit::new(Uuid::nil(), info, true);
        assert_eq!(titles(&edit), ["Un", "Track 2"]);
        assert!(!edit.modified());
        edit.tracks.swap(0, 1);
        assert!(edit.modified());
        edit.revert();
        assert!(!edit.modified());
        edit.tracks.pop();
        assert!(edit.modified());

        let info = AlbumInfo {
            tracks: vec!["Un".into(), "Deux".into(), "Trois".into()],
            chapters: vec![
                Chapter {
                    title: "CD 1".into(),
                    len: 1,
                },
                Chapter {
                    title: "CD 2".into(),
                    len: 2,
                },
            ],
        };
        let edit = AlbumEdit::new(Uuid::nil(), info, false);
        let rows = edit.rows();
        assert_eq!(titles(&edit), ["CD 1", "Un", "CD 2", "Deux", "Trois"]);
        assert!(rows[0].header && !rows[1].header);
        assert_eq!(rows[4].number, "2.");
    }
}
