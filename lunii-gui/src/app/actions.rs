//! Menu entries and shortcuts (`Action`).

use super::{App, RELEASES_URL, dirs_home, has_supported_ext};
use crate::i18n::tr;
use crate::worker::Task;
use crate::{Action, DialogKind};
use lunii_core::Level;
use lunii_core::constants::*;
use lunii_core::device::{self};
use std::path::PathBuf;
use std::rc::Rc;

impl App {
    pub(crate) fn action(self: &Rc<Self>, action: Action) {
        let ui = self.ui();
        match action {
            Action::RefreshDevices => self.refresh_devices(),
            Action::OpenDevice => self.open_device_dialog(),
            Action::ImportDb => {
                let studio_dir = dirs_home().join(".studio").join("db");
                if let Some(file) = rfd::FileDialog::new()
                    .set_title(tr("Open STUdio DB"))
                    .set_directory(studio_dir)
                    .add_filter("STUdio DB (unofficial.json)", &["json"])
                    .add_filter(tr("All files"), &["*"])
                    .pick_file()
                {
                    self.launch(Task::DbImport(file));
                }
            }
            Action::RefreshDb => {
                self.status(&tr("Fetching official Lunii DB..."));
                self.launch(Task::RefreshDb);
            }
            Action::Exit => {
                if let Some((_, ctx)) = self.state.borrow().worker.as_ref() {
                    ctx.request_abort();
                }
                let _ = slint::quit_event_loop();
            }
            Action::MoveTop => self.move_selection(-2),
            Action::MoveUp => self.move_selection(-1),
            Action::MoveDown => self.move_selection(1),
            Action::MoveBottom => self.move_selection(2),
            Action::Hide => {
                let ids = self.selected_ids();
                if self.device_op(|d, ctx| d.toggle_hidden(ctx, &ids)) {
                    self.status(&tr("✅ Stories updated..."));
                } else {
                    self.status(&tr("🛑 Unable to update story ..."));
                    self.show_log();
                }
            }
            Action::NightMode => {
                let ids = self.selected_ids();
                let ok = self.device_op(|d, _| match d.as_lunii_mut() {
                    Some(l) => l.toggle_night_mode(&ids),
                    None => Ok(()),
                });
                if ok {
                    self.status(&tr("✅ Stories updated..."));
                } else {
                    self.status(&tr("🛑 Unable to update story ..."));
                    self.show_log();
                }
            }
            Action::Import => {
                let Some(files) = rfd::FileDialog::new()
                    .set_title(tr("Open Stories"))
                    .add_filter(tr("All supported"), &["pk", "7z", "zip"])
                    .add_filter(tr("PK files"), &["pk"])
                    .add_filter(tr("Archive files"), &["7z", "zip"])
                    .add_filter(tr("All files"), &["*"])
                    .pick_files()
                else {
                    return;
                };
                self.import(files);
            }
            Action::ImportPodcast => self.podcasts_focus_address(),
            Action::NormalizeVolume => {
                let enabled = ui.get_normalize_volume();
                if let Err(e) = lunii_core::settings::set_bool("normalize_volume", enabled) {
                    log::warn!("unable to save the settings: {e:#}");
                }
            }
            Action::ImportAlbum => {
                let Some(dirs) = rfd::FileDialog::new()
                    .set_title(tr("Folders of MP3 files"))
                    .pick_folders()
                else {
                    return;
                };
                let (albums, others): (Vec<_>, Vec<_>) = dirs
                    .into_iter()
                    .partition(|d| lunii_core::album::is_album_dir(d));
                if !others.is_empty() {
                    self.status(&tr("🛑 No MP3 file in this folder"));
                }
                self.import(albums);
            }
            Action::AddToAlbum => {
                let Some(uuid) = self.state.borrow().selected.iter().next().copied() else {
                    return;
                };
                let Some(files) = rfd::FileDialog::new()
                    .set_title(tr("MP3 files to add to the album"))
                    .add_filter("MP3", &["mp3", "MP3"])
                    .pick_files()
                else {
                    return;
                };
                let update_size = ui.get_show_size();
                self.launch(Task::AddToAlbum {
                    uuid,
                    files,
                    update_size,
                });
            }
            // the tracks are edited in the details panel
            Action::EditAlbum => {
                ui.set_show_details(true);
                self.state.borrow_mut().details_uuid = None;
                self.update_details();
            }
            Action::Export | Action::ExportAll => {
                let uuids = if action == Action::Export {
                    self.selected_uuids()
                } else {
                    let st = self.state.borrow();
                    st.visible
                        .iter()
                        .map(|&i| st.stories[i].str_uuid.clone())
                        .collect()
                };
                if uuids.is_empty() {
                    return;
                }
                let title = format!("Output Directory for {} stories", uuids.len());
                match rfd::FileDialog::new().set_title(title).pick_folder() {
                    Some(out) => self.launch(Task::Export { uuids, out }),
                    None if action == Action::Export => self.status(&tr("🛑 Export cancelled")),
                    None => self.status(&tr("🛑 Export All cancelled")),
                }
            }
            Action::Remove => {
                let names: Vec<String> = {
                    let st = self.state.borrow();
                    st.visible
                        .iter()
                        .map(|&i| &st.stories[i])
                        .filter(|s| st.selected.contains(&s.uuid))
                        .map(|s| s.name.clone())
                        .collect()
                };
                if names.is_empty() {
                    return;
                }
                let mut message = tr("You are requesting to delete : \n");
                for n in &names {
                    message += &format!("- {n}\n");
                }
                if message.len() > 512 {
                    message = message.chars().take(512).collect::<String>() + "...";
                    message += &tr("\n(and too many others)\n");
                }
                message += &tr("\nDo you want to proceed ?");
                ui.set_confirm_message(message.into());
                ui.set_dialog(DialogKind::ConfirmRemove);
            }
            Action::ShowSize => self.ensure_sizes(),
            Action::ShowDetails => {
                self.state.borrow_mut().details_uuid = None;
                self.update_details();
            }
            Action::ShowLog => {
                if ui.get_log_visible() {
                    ui.set_log_visible(false);
                } else {
                    self.show_log();
                }
            }
            Action::GetFirmware => {
                ui.set_login_password("".into());
                ui.set_dialog(DialogKind::Login);
            }
            Action::FindLost | Action::RecoverLost | Action::RemoveLost => {
                self.show_log();
                self.launch(match action {
                    Action::FindLost => Task::Recover { dry_run: true },
                    Action::RecoverLost => Task::Recover { dry_run: false },
                    _ => Task::Cleanup,
                });
            }
            Action::Ffmpeg => self.launch(Task::Ffmpeg),
            Action::Update => {
                let _ = open::that(RELEASES_URL);
            }
            Action::About => ui.set_dialog(DialogKind::About),
            Action::NightModeSettings => self.open_night_mode(),
        }
    }

    pub(super) fn open_device_dialog(&self) {
        let Some(dir) = rfd::FileDialog::new()
            .set_title(tr("Open Lunii device"))
            .pick_folder()
        else {
            return;
        };
        if !device::is_lunii(&dir) && !device::is_flam(&dir) {
            self.status(&tr("Not a Lunii, nor Flam or unsupported one 😥"));
            return;
        }
        let index = {
            let st = self.state.borrow();
            st.devices.iter().position(|d| d == &dir)
        };
        let index = match index {
            Some(i) => i,
            None => {
                let mut devices = self.state.borrow().devices.clone();
                devices.push(dir);
                self.set_devices(devices);
                self.state.borrow().devices.len() - 1
            }
        };
        self.set_device_index(index as i32);
        self.select_device(index as i32);
    }

    pub(crate) fn import(&self, files: Vec<PathBuf>) {
        if files.is_empty() {
            return;
        }
        self.status(&tr("Importing stories..."));
        let update_size = self.ui().get_show_size();
        self.launch(Task::Import { files, update_size });
    }

    pub(super) fn files_dropped(&self) {
        let files = std::mem::take(&mut self.state.borrow_mut().pending_drop);
        let (version, can_import) = match self.state.borrow().dev.as_ref() {
            Some(d) => (d.version, d.can_import),
            None => return,
        };
        if self.ui().get_busy() {
            return;
        }
        let extensions = if version == DeviceVersion::FlamV1 {
            FLAM_SUPPORTED_EXT
        } else {
            LUNII_SUPPORTED_EXT
        };
        if !files
            .iter()
            .all(|f| has_supported_ext(f, extensions) || lunii_core::album::is_album_dir(f))
        {
            self.status(&tr("🛑 Unsupported file type"));
            return;
        }
        if !can_import {
            self.status(&tr(
                "🛑 Unable to import story, missing story key for Lunii v3",
            ));
            return;
        }
        self.import(files);
    }

    pub(super) fn open_night_mode(&self) {
        let Some(config) = self
            .state
            .borrow()
            .dev
            .as_ref()
            .and_then(|d| d.config.clone())
        else {
            return;
        };
        let ui = self.ui();
        ui.set_nm_enabled(config[LUNII_CFGPOS_NM_ENABLED] != 0);
        ui.set_nm_limit(config[LUNII_CFGPOS_NM_VOL_LIMIT] != 0);
        ui.set_nm_autoplay(config[LUNII_CFGPOS_NM_AUTOPLAY] != 0);
        ui.set_nm_max_stories((config[LUNII_CFGPOS_NM_STORYCOUNT] as i32).clamp(1, 8));
        ui.set_nm_turnoff(config[LUNII_CFGPOS_NM_TURNOFF_NM] != 0);
        ui.set_dialog(DialogKind::NightMode);
    }

    pub(super) fn save_night_mode(&self) {
        let ui = self.ui();
        let Some(dev) = self.state.borrow().device.clone() else {
            return;
        };
        let result = {
            let Ok(mut dev) = Self::lock_device(&dev) else {
                self.status(&tr(
                    "⏳ The storyteller is busy, try again once the current task is done",
                ));
                return;
            };
            let Some(lunii) = dev.as_lunii_mut() else {
                return;
            };
            lunii.config[LUNII_CFGPOS_NM_ENABLED] = ui.get_nm_enabled() as u16;
            lunii.config[LUNII_CFGPOS_NM_VOL_LIMIT] = if ui.get_nm_limit() { 30 } else { 0 };
            lunii.config[LUNII_CFGPOS_NM_AUTOPLAY] = ui.get_nm_autoplay() as u16;
            lunii.config[LUNII_CFGPOS_NM_STORYCOUNT] = ui.get_nm_max_stories() as u16;
            lunii.config[LUNII_CFGPOS_NM_TURNOFF_NM] = ui.get_nm_turnoff() as u16;
            lunii.update_config().map(|_| lunii.config.clone())
        };
        match result {
            Ok(config) => {
                ui.set_nm_active(config[LUNII_CFGPOS_NM_ENABLED] == 1);
                if let Some(d) = self.state.borrow_mut().dev.as_mut() {
                    d.config = Some(config);
                }
            }
            Err(e) => {
                self.log(Level::Error, &format!("{e:#}"));
                self.show_log();
            }
        }
    }

    pub(super) fn firmware_login(&self) {
        let ui = self.ui();
        let Some((version, snu)) = self
            .state
            .borrow()
            .dev
            .as_ref()
            .map(|d| (d.version, d.snu.clone()))
        else {
            return;
        };
        let email = ui.get_login_email().to_string();
        let password = ui.get_login_password().to_string();
        ui.set_login_password("".into());
        self.launch(Task::FirmwareLogin {
            email,
            password,
            version,
            snu,
        });
    }

    /// Asks where to save each firmware, then downloads them.
    pub(super) fn firmware_targets(&self) {
        let Some((auth, files)) = self.state.borrow_mut().fw.take() else {
            return;
        };
        let Some((version, snu)) = self
            .state
            .borrow()
            .dev
            .as_ref()
            .map(|d| (d.version, d.snu.clone()))
        else {
            return;
        };
        let (filter_name, ext) = if version == DeviceVersion::FlamV1 {
            ("Flam Firmware", "enc")
        } else {
            ("Lunii Firmware", "bin")
        };
        let mut targets = Vec::new();
        for (index, name) in files.iter().enumerate() {
            let target = rfd::FileDialog::new()
                .set_directory(cfg_dir())
                .set_file_name(name)
                .add_filter(filter_name, &[ext])
                .add_filter(tr("All files"), &["*"])
                .save_file();
            match target {
                Some(t) => targets.push((t, index != 0)),
                None => {
                    self.status("");
                    return;
                }
            }
        }
        self.launch(Task::FirmwareDownload {
            auth,
            version,
            snu,
            targets,
        });
    }
}
