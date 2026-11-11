//! Main window controller (with the night mode and log panels).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lunii_core::constants::*;
use lunii_core::device::{
    self,
    common::{free_space, total_space},
};
use lunii_core::{Context, Device, Level};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use uuid::Uuid;

use crate::i18n::{tr, trf};
use crate::logfile;
use crate::worker::{self, Event, GuiReporter, SharedDevice, Sink, StoryInfo, Task};
use crate::{DialogKind, EpisodeRow, InfoRow, LogLine, MainWindow, StoryRow, Theme};

mod actions;
mod album;
mod devices;
mod log_panel;
mod podcast;
mod progress;
mod stories;

pub(crate) use devices::DevInfo;
use stories::*;

pub const APP_VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));
const RELEASES_URL: &str = "https://github.com/o-daneel/Lunii.QT/releases/latest";

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

/// Sink delivering worker events to the UI thread.
fn sink() -> Sink {
    Arc::new(|event: Event| {
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = APP.with(|a| a.borrow().clone()) {
                app.handle_event(event);
            }
        });
    })
}

/// Messages of the `log` crate facade (see `logfile`), on the UI thread.
pub fn log_from_facade(level: Level, message: &str) {
    match APP.with(|a| a.borrow().clone()) {
        Some(app) => app.log(level, message),
        None => logfile::write(&format!("[{}] {message}", level_name(level_value(level)))),
    }
}

/// State of the window, by domain (each one defined with the code using it).
#[derive(Default)]
struct State {
    devices: devices::Devices,
    list: stories::List,
    durations: stories::Durations,
    task: progress::TaskState,
    log: log_panel::Log,
    /// followed podcasts and their feeds (Podcasts view)
    podcasts: podcast::Podcasts,
    /// album shown in the details panel, with its pending changes
    album: Option<album::AlbumEdit>,
    /// files dropped on the window, imported once the drop is over
    pending_drop: Vec<PathBuf>,
    /// firmware download: the login token and the files to download
    fw: Option<(lunii_core::firmware::AuthToken, Vec<String>)>,
}

pub struct App {
    ui: slint::Weak<MainWindow>,
    rows: Rc<VecModel<StoryRow>>,
    devices_model: Rc<VecModel<SharedString>>,
    log_model: Rc<VecModel<LogLine>>,
    podcast_rows: Rc<VecModel<EpisodeRow>>,
    state: RefCell<State>,
    drop_timer: slint::Timer,
    eta_timer: slint::Timer,
}

fn level_value(level: Level) -> i32 {
    match level {
        Level::Error => 3,
        Level::Warn => 2,
        Level::Info => 1,
        _ => 0,
    }
}

fn level_name(level: i32) -> &'static str {
    match level {
        3 => "ERROR",
        2 => "WARNING",
        1 => "INFO",
        _ => "DEBUG",
    }
}

fn decode_image(data: &[u8]) -> Option<slint::Image> {
    let img = image::load_from_memory(data).ok()?.to_rgba8();
    let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        img.as_raw(),
        img.width(),
        img.height(),
    );
    Some(slint::Image::from_rgba8(buffer))
}

fn has_supported_ext(path: &Path, extensions: &[&str]) -> bool {
    let name = path.to_string_lossy().to_lowercase();
    extensions.iter().any(|ext| name.ends_with(ext))
}

/// `true` if `latest` is a newer release than `current` (alpha aware: `3.2.0a1`).
pub fn is_newer(current: &str, latest: &str) -> bool {
    fn parse(v: &str) -> (Vec<u32>, Option<u32>) {
        let v = v.trim_start_matches('v');
        let (main, alpha) = match v.split_once('a') {
            Some((m, a)) => (m, a.parse().ok()),
            None => (v, None),
        };
        (
            main.split('.').map(|p| p.parse().unwrap_or(0)).collect(),
            alpha,
        )
    }
    let (c, ca) = parse(current);
    let (l, la) = parse(latest);
    match l.cmp(&c) {
        std::cmp::Ordering::Greater => la.is_none() || ca.is_some(),
        std::cmp::Ordering::Less => false,
        // same version: alpha -> stable, or newer alpha
        std::cmp::Ordering::Equal => match (ca, la) {
            (Some(_), None) => true,
            (Some(a), Some(b)) => b > a,
            _ => false,
        },
    }
}

impl App {
    pub fn new(ui: &MainWindow) -> Rc<App> {
        let app = Rc::new(App {
            ui: ui.as_weak(),
            rows: Rc::new(VecModel::default()),
            devices_model: Rc::new(VecModel::default()),
            log_model: Rc::new(VecModel::default()),
            podcast_rows: Rc::new(VecModel::default()),
            state: RefCell::new(State::default()),
            drop_timer: slint::Timer::default(),
            eta_timer: slint::Timer::default(),
        });
        APP.with(|a| *a.borrow_mut() = Some(app.clone()));

        ui.set_app_version(APP_VERSION.into());
        ui.set_rows(ModelRc::from(app.rows.clone()));
        ui.set_devices(ModelRc::from(app.devices_model.clone()));
        let normalize = lunii_core::settings::get_bool("normalize_volume");
        ui.set_normalize_volume(normalize);
        ui.set_show_duration(!lunii_core::settings::get_bool("hide_duration"));
        ui.set_show_db(lunii_core::settings::get_bool("show_db"));
        ui.set_show_uuid(lunii_core::settings::get_bool("show_uuid"));
        if let Some(width) = lunii_core::settings::get_number("details_width") {
            ui.set_details_width(width as f32);
        }
        ui.global::<Theme>().set_mono_font(
            if cfg!(windows) {
                "Consolas"
            } else if cfg!(target_os = "macos") {
                "Menlo"
            } else {
                "DejaVu Sans Mono"
            }
            .into(),
        );
        ui.set_log_lines(ModelRc::from(app.log_model.clone()));
        ui.set_podcast_rows(ModelRc::from(app.podcast_rows.clone()));
        ui.set_log_file_path(logfile::log_path().display().to_string().into());

        app.connect(ui);
        app
    }

    fn ui(&self) -> MainWindow {
        self.ui.upgrade().expect("main window alive")
    }

    fn ctx(&self) -> Context {
        Context::new(Arc::new(GuiReporter(sink())))
    }

    // -----------------------------------------------------------------------
    // Wiring
    // -----------------------------------------------------------------------

    fn connect(self: &Rc<Self>, ui: &MainWindow) {
        let weak = Rc::downgrade(self);
        let with = move |f: fn(&Rc<App>)| {
            let weak = weak.clone();
            move || {
                if let Some(app) = weak.upgrade() {
                    f(&app)
                }
            }
        };

        let w = Rc::downgrade(self);
        ui.on_action(move |name| {
            if let Some(app) = w.upgrade() {
                app.action(name);
            }
        });
        let w = Rc::downgrade(self);
        ui.on_device_selected(move |index| {
            if let Some(app) = w.upgrade() {
                app.select_device(index);
            }
        });
        ui.on_filter_edited(with(|app| {
            app.state.borrow_mut().list.selected.clear();
            app.rebuild_rows();
        }));
        let w = Rc::downgrade(self);
        ui.on_row_pointer(move |row, ctrl, shift, right| {
            if let Some(app) = w.upgrade() {
                app.row_pointer(row as usize, ctrl, shift, right);
            }
        });
        let w = Rc::downgrade(self);
        ui.on_key_nav(move |delta, shift| {
            if let Some(app) = w.upgrade() {
                app.key_nav(delta, shift);
            }
        });
        let w = Rc::downgrade(self);
        ui.on_rows_dropped(move |gap| {
            if let Some(app) = w.upgrade() {
                app.drop_selection(gap.max(0) as usize);
            }
        });
        ui.on_select_all(with(|app| {
            {
                let mut st = app.state.borrow_mut();
                let all: Vec<Uuid> = st
                    .list
                    .visible
                    .iter()
                    .map(|&i| st.list.stories[i].uuid)
                    .collect();
                st.list.selected = all.into_iter().collect();
            }
            app.selection_changed();
        }));
        ui.on_abort(with(|app| app.abort()));
        ui.on_details_width_changed(with(|app| {
            let width = app.ui().get_details_width();
            if let Err(e) = lunii_core::settings::set_number("details_width", width.into()) {
                app.log(Level::Warn, &format!("{e:#}"));
            }
        }));
        ui.on_device_info_clicked(with(|app| app.show_device_info()));
        ui.on_device_info_copy(with(|app| app.copy_device_info()));
        ui.on_device_open_folder(with(|app| {
            if let Some(dev) = app.state.borrow().devices.info.as_ref() {
                let _ = open::that(&dev.mount);
            }
        }));
        ui.on_confirm_remove_accepted(with(|app| {
            if std::mem::take(&mut app.state.borrow_mut().podcasts.confirm_removal) {
                app.podcast_import();
                return;
            }
            let uuids = app.selected_uuids();
            app.launch(Task::Remove { uuids });
        }));
        ui.on_login_accepted(with(|app| app.firmware_login()));
        ui.on_night_mode_accepted(with(|app| app.save_night_mode()));

        // Podcasts view
        ui.on_view_changed(with(|app| {
            if app.ui().get_view() == 1 {
                app.podcasts_show();
            }
        }));
        ui.on_podcast_add(with(|app| app.podcast_add_open()));
        ui.on_podcast_add_submit(with(|app| app.podcast_add_submit()));
        ui.on_podcast_add_follow(with(|app| app.podcast_add_follow()));
        let w = Rc::downgrade(self);
        ui.on_podcast_add_choose(move |index| {
            if let Some(app) = w.upgrade() {
                app.podcast_add_choose(index);
            }
        });
        ui.on_podcast_refresh(with(|app| app.podcast_refresh()));
        ui.on_podcast_unfollow(with(|app| app.podcast_unfollow()));
        let w = Rc::downgrade(self);
        ui.on_podcast_unfollow_accepted(move |remove| {
            if let Some(app) = w.upgrade() {
                app.podcast_unfollow_accepted(remove);
            }
        });
        let w = Rc::downgrade(self);
        ui.on_podcast_select(move |index| {
            if let Some(app) = w.upgrade() {
                app.podcast_select(index);
            }
        });
        ui.on_podcast_filter_edited(with(|app| app.rebuild_podcast_rows()));
        ui.on_podcast_mode_changed(with(|app| app.podcast_mode_changed()));
        ui.on_podcast_accepted(with(|app| app.podcast_apply()));
        ui.on_podcast_sort_changed(with(|app| app.rebuild_podcast_rows()));
        let w = Rc::downgrade(self);
        ui.on_podcast_toggle(move |index| {
            if let Some(app) = w.upgrade() {
                app.podcast_toggle(index as usize);
            }
        });
        let w = Rc::downgrade(self);
        ui.on_podcast_select_all(move |all| {
            if let Some(app) = w.upgrade() {
                app.podcast_select_all(all);
            }
        });
        // album tracks (details panel)
        ui.on_album_remove(with(|app| app.album_remove()));
        ui.on_album_add_files(with(|app| app.album_add_files()));
        ui.on_album_apply(with(|app| app.album_apply()));
        ui.on_album_revert(with(|app| app.album_revert()));
        let w = Rc::downgrade(self);
        ui.on_album_move(move |delta| {
            if let Some(app) = w.upgrade() {
                app.album_move(delta);
            }
        });
        let w = Rc::downgrade(self);
        ui.on_album_drop(move |from, gap| {
            if let Some(app) = w.upgrade() {
                app.album_drop(from.max(0) as usize, gap.max(0) as usize);
            }
        });
        ui.on_open_url(|url| {
            let _ = open::that(url.as_str());
        });

        // log panel
        ui.on_log_filter_changed(with(|app| app.rebuild_log()));
        ui.on_log_clear(with(|app| {
            app.state.borrow_mut().log.lines.clear();
            app.rebuild_log();
        }));
        ui.on_log_copy(with(|app| app.copy_log()));
        ui.on_log_open_folder(|| {
            let _ = open::that(logfile::log_dir());
        });

        // close: abort current task
        let w = Rc::downgrade(self);
        ui.window().on_close_requested(move || {
            if let Some(app) = w.upgrade()
                && let Some((_, ctx)) = app.state.borrow().task.worker.as_ref()
            {
                ctx.request_abort();
            }
            let _ = slint::quit_event_loop();
            slint::CloseRequestResponse::HideWindow
        });

        self.connect_winit(ui);
    }

    /// Files dropped from the file manager.
    fn connect_winit(self: &Rc<Self>, ui: &MainWindow) {
        use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
        let w = Rc::downgrade(self);
        ui.window().on_winit_window_event(move |_, event| {
            let Some(app) = w.upgrade() else {
                return EventResult::Propagate;
            };
            match event {
                WindowEvent::HoveredFile(_) => {
                    let accept =
                        app.state.borrow().devices.opened.is_some() && !app.ui().get_busy();
                    app.ui().set_drop_hover(accept);
                }
                WindowEvent::HoveredFileCancelled => app.ui().set_drop_hover(false),
                WindowEvent::DroppedFile(path) => {
                    app.ui().set_drop_hover(false);
                    app.state.borrow_mut().pending_drop.push(path.clone());
                    // a drop of several files comes as several events
                    let w2 = Rc::downgrade(&app);
                    app.drop_timer.start(
                        slint::TimerMode::SingleShot,
                        Duration::from_millis(150),
                        move || {
                            if let Some(app) = w2.upgrade() {
                                app.files_dropped();
                            }
                        },
                    );
                }
                _ => {}
            }
            EventResult::Propagate
        });
    }

    // -----------------------------------------------------------------------
    // Status bar
    // -----------------------------------------------------------------------

    /// Port of `sb_update`: status message (logged) + device details.
    fn status(&self, message: &str) {
        let ui = self.ui();
        ui.set_status(message.into());
        if !message.is_empty() {
            self.log(Level::Info, message);
        }
        self.update_device_status();
    }

    // -----------------------------------------------------------------------
    // Devices
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Story list
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Worker
    // -----------------------------------------------------------------------

    pub fn handle_event(self: &Rc<Self>, event: Event) {
        let ui = self.ui();
        match event {
            Event::Log(level, msg) => self.log(level, &msg),
            Event::Status(msg) => self.status(&msg),
            Event::DevicesFound(scan, devices, labels) => self.devices_found(scan, devices, labels),
            Event::DeviceOpened(scan, opened) => self.device_opened(scan, opened),
            Event::ShowLog => self.show_log(),
            Event::Total(current, total) => {
                ui.set_total_visible(true);
                ui.set_total_text(format!("Total {}/{total}", current + 1).into());
                ui.set_total_progress(if total > 0 {
                    (current + 1) as f32 / total as f32
                } else {
                    0.0
                });
            }
            Event::ImportPlan(total) => self.start_eta(total),
            Event::Feed(url, result) => self.feed_loaded(url, result),
            Event::PodcastSearch(token, results) => self.podcast_add_found(token, results),
            Event::PodcastChecked(token, result) => self.podcast_add_checked(token, result),
            Event::PodcastArtwork(token, index, data) => {
                self.podcast_add_artwork(token, index, &data)
            }
            Event::Story(uuid, current, total) => {
                ui.set_story_visible(true);
                ui.set_story_text(uuid.into());
                ui.set_story_progress(if total > 0 {
                    (current + 1) as f32 / total as f32
                } else {
                    0.0
                });
            }
            Event::File(label, current, total) => {
                ui.set_story_visible(true);
                ui.set_story_text(label.into());
                ui.set_file_visible(total > 0);
                ui.set_file_progress(if total > 0 && current < total {
                    current as f32 / total as f32
                } else {
                    0.0
                });
            }
            Event::Duration(generation, uuid, duration) => {
                // only its row changes (the list keeps its scrolling)
                let row = {
                    let mut st = self.state.borrow_mut();
                    if generation != st.durations.generation {
                        return;
                    }
                    st.durations.measured.insert(uuid, duration);
                    st.list
                        .visible
                        .iter()
                        .position(|&i| st.list.stories[i].uuid == uuid)
                };
                if let (Some(row), Some(secs)) = (row, duration)
                    && let Some(mut data) = self.rows.row_data(row)
                    && data.duration.is_empty()
                {
                    data.duration = format_duration(secs).into();
                    self.rows.set_row_data(row, data);
                }
            }
            Event::DurationsDone => {
                self.state.borrow_mut().durations.measuring = false;
                self.ensure_durations();
            }
            Event::DeviceStories(device, stories) => {
                // another device may have been opened meanwhile
                let current = self.state.borrow().devices.opened.clone();
                if current.is_some_and(|d| Arc::ptr_eq(&d, &device)) {
                    self.state.borrow_mut().list.stories = stories;
                    self.rebuild_rows();
                }
            }
            Event::Stories(stories) => {
                self.state.borrow_mut().list.stories = stories;
                self.rebuild_rows();
            }
            Event::DbLoaded(ok) => {
                if !ok {
                    self.log(Level::Warn, "Failed to fetch official DB");
                }
                let device = self.state.borrow().devices.opened.clone();
                // the names may have changed: list read again in the background
                // (busy: the task refreshes the list at its end)
                if let (Some(dev), false) = (device, ui.get_busy()) {
                    let sink = sink();
                    std::thread::spawn(move || {
                        let stories = match Self::lock_device(&dev) {
                            Ok(d) => worker::snapshot(d.as_ref()),
                            Err(_) => return,
                        };
                        sink(Event::DeviceStories(dev, stories));
                    });
                }
            }
            Event::DbRefreshed(ok) => {
                self.status(&if ok {
                    tr("👍 Lunii DB refreshed.")
                } else {
                    tr("🛑 Lunii DB failed.")
                });
            }
            Event::FirmwareFiles(auth, files) => {
                self.state.borrow_mut().fw = Some((auth, files));
            }
            Event::UpdateCheck(version) => self.update_checked(version),
            Event::Details(uuid, desc, picture) => self.details_ready(uuid, desc, picture),
            Event::Finished => {
                self.stop_eta();
                // the album shown may have changed
                self.state.borrow_mut().list.details_uuid = None;
                if let Some((handle, _)) = self.state.borrow_mut().task.worker.take() {
                    let _ = handle.join();
                }
                // a failed size computation is not retried in a loop
                let sized = std::mem::take(&mut self.state.borrow_mut().task.sizing);
                ui.set_busy(false);
                ui.set_total_visible(false);
                ui.set_story_visible(false);
                ui.set_file_visible(false);
                ui.set_abort_visible(false);
                self.selection_changed();
                self.podcasts_refresh_device();
                if !sized {
                    // the stories may have changed (album edited, episodes
                    // appended, story moved...)
                    self.forget_durations();
                    self.ensure_sizes();
                }
                // firmware login done: choose where to save the files
                if self.state.borrow().fw.is_some() {
                    let app = self.clone();
                    slint::Timer::single_shot(Duration::from_millis(1), move || {
                        app.firmware_targets()
                    });
                }
            }
        }
    }

    fn update_checked(&self, version: Option<String>) {
        let ui = self.ui();
        match version {
            Some(v) => {
                self.log(Level::Info, &format!("{} {v}", tr("Latest Github release")));
                if is_newer(APP_VERSION, &v) {
                    ui.set_latest_version(v.into());
                    ui.set_update_available(true);
                }
            }
            None => self.log(Level::Warn, &tr("🛑 Unable to fetch version from Github")),
        }
    }

    /// Joins the running task (application exit).
    pub fn shutdown(&self) {
        let worker = self.state.borrow_mut().task.worker.take();
        if let Some((handle, ctx)) = worker {
            ctx.request_abort();
            let _ = handle.join();
        }
    }

    pub fn start(self: &Rc<Self>) {
        let title = format!("Lunii {APP_VERSION}");
        self.log(Level::Info, &title);
        self.ui()
            .set_device_placeholder(tr("Select your Lunii").into());
        self.refresh_devices();
        // stories database (may download the official one): names are refreshed once loaded
        let db_sink = sink();
        std::thread::spawn(move || db_sink(Event::DbLoaded(lunii_core::db::load_db(false))));
        if std::env::var_os("LUNII_NO_UPDATE_CHECK").is_none() {
            worker::check_version(sink());
        }
    }
}

fn dirs_home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn versions() {
        assert!(is_newer("v3.1.4", "v3.1.5"));
        assert!(is_newer("v3.1.4", "v3.10.0"));
        assert!(!is_newer("v3.1.4", "v3.1.4"));
        assert!(
            !is_newer("v3.1.4", "v3.1.5a1"),
            "stable vs alpha: no update"
        );
        assert!(is_newer("v3.1.5a1", "v3.1.5"), "alpha vs stable");
        assert!(is_newer("v3.1.5a1", "v3.1.5a2"), "alpha vs alpha");
        assert!(!is_newer("v3.2.0", "v3.1.9"));
    }
}
