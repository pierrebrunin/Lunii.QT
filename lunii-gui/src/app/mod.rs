//! Main window controller (with the night mode and log panels).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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
#[cfg(feature = "screenshot")]
pub(crate) use progress::transfer_text;
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
    transfer_timer: slint::Timer,
    /// looks for plugged / unplugged devices
    watch_timer: slint::Timer,
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

/// `done / total` for a progress bar (0 without a total).
fn ratio(done: usize, total: usize) -> f32 {
    if total > 0 {
        done as f32 / total as f32
    } else {
        0.0
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

/// `true` if `latest` is a newer release than `current`, pre-release aware:
/// `4.0.0-beta.1` (semver) or `3.2.0a1` (older Python releases).
pub fn is_newer(current: &str, latest: &str) -> bool {
    /// Version numbers and pre-release (alpha < beta < rc, then its number).
    fn parse(v: &str) -> (Vec<u32>, Option<(u8, u32)>) {
        let v = v.trim_start_matches('v');
        let (main, pre) = match v.split_once('-') {
            Some((m, p)) => (m, Some(p)),
            None => match v.find(|c: char| c.is_ascii_alphabetic()) {
                Some(i) => (&v[..i], Some(&v[i..])),
                None => (v, None),
            },
        };
        let pre = pre.map(|p| {
            let label = p.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
            let n = p[label.len()..]
                .trim_start_matches('.')
                .parse()
                .unwrap_or(0);
            let rank = match label {
                "a" | "alpha" => 0,
                "b" | "beta" => 1,
                _ => 2,
            };
            (rank, n)
        });
        (
            main.split('.').map(|p| p.parse().unwrap_or(0)).collect(),
            pre,
        )
    }
    let (c, ca) = parse(current);
    let (l, la) = parse(latest);
    match l.cmp(&c) {
        std::cmp::Ordering::Greater => la.is_none() || ca.is_some(),
        std::cmp::Ordering::Less => false,
        // same version: pre-release -> stable, or newer pre-release
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
            transfer_timer: slint::Timer::default(),
            watch_timer: slint::Timer::default(),
        });
        APP.with(|a| *a.borrow_mut() = Some(app.clone()));

        ui.set_app_version(APP_VERSION.into());
        ui.set_rows(ModelRc::from(app.rows.clone()));
        ui.set_devices(ModelRc::from(app.devices_model.clone()));
        let normalize = lunii_core::settings::get_bool("normalize_volume");
        ui.set_normalize_volume(normalize);
        ui.set_show_duration(!lunii_core::settings::get_bool("hide_duration"));
        ui.set_show_db(lunii_core::settings::get_bool("show_db"));
        ui.set_show_nm(lunii_core::settings::get_bool("show_nm"));
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
        // `on!(setter, |app, arguments...| body)`: a callback of the window,
        // run while the application is alive
        macro_rules! on {
            ($setter:ident, |$app:ident $(, $arg:ident)*| $body:expr) => {{
                let weak = Rc::downgrade(self);
                ui.$setter(move |$($arg),*| {
                    if let Some($app) = weak.upgrade() {
                        $body;
                    }
                });
            }};
        }

        on!(on_action, |app, name| app.action(name));
        on!(on_device_selected, |app, index| app.select_device(index));
        on!(on_filter_edited, |app| {
            app.state.borrow_mut().list.selected.clear();
            app.rebuild_rows();
        });
        on!(on_row_pointer, |app, row, ctrl, shift, right| {
            app.row_pointer(row as usize, ctrl, shift, right)
        });
        on!(on_key_nav, |app, delta, shift| app.key_nav(delta, shift));
        on!(on_rows_dropped, |app, gap| app
            .drop_selection(gap.max(0) as usize));
        on!(on_select_all, |app| {
            {
                let mut st = app.state.borrow_mut();
                let list = &mut st.list;
                list.selected = list.visible.iter().map(|&i| list.stories[i].uuid).collect();
            }
            app.selection_changed();
        });
        on!(on_abort, |app| app.abort());
        on!(on_details_width_changed, |app| {
            let width = app.ui().get_details_width();
            if let Err(e) = lunii_core::settings::set_number("details_width", width.into()) {
                app.log(Level::Warn, &format!("{e:#}"));
            }
        });
        on!(on_device_info_clicked, |app| app.show_device_info());
        on!(on_device_info_copy, |app| app.copy_device_info());
        on!(on_device_open_folder, |app| {
            if let Some(dev) = app.state.borrow().devices.info.as_ref() {
                let _ = open::that(&dev.mount);
            }
        });
        on!(on_confirm_remove_accepted, |app| {
            if std::mem::take(&mut app.state.borrow_mut().podcasts.confirm_removal) {
                app.podcast_import();
            } else {
                let uuids = app.selected_uuids();
                app.launch(Task::Remove { uuids });
            }
        });
        on!(on_login_accepted, |app| app.firmware_login());
        on!(on_night_mode_accepted, |app| app.save_night_mode());

        // Podcasts view
        on!(on_view_changed, |app| app.podcasts_refresh_device());
        on!(on_podcast_add, |app| app.podcast_add_open());
        on!(on_podcast_add_submit, |app| app.podcast_add_submit());
        on!(on_podcast_add_follow, |app| app.podcast_add_follow());
        on!(on_podcast_add_kids_toggled, |app| app
            .podcast_add_kids_toggled());
        on!(on_podcast_add_choose, |app, index| app
            .podcast_add_choose(index));
        on!(on_podcast_refresh, |app| app.podcast_refresh());
        on!(on_podcast_copy_page, |app| app.podcast_copy_page());
        on!(on_podcast_unfollow, |app| app.podcast_unfollow());
        on!(on_podcast_unfollow_accepted, |app, remove| app
            .podcast_unfollow_accepted(remove));
        on!(on_podcast_select, |app, index| app.podcast_select(index));
        on!(on_podcast_mode_changed, |app| app.podcast_mode_changed());
        on!(on_podcast_accepted, |app| app.podcast_apply());
        on!(on_podcast_sort_changed, |app| app.rebuild_podcast_rows());
        on!(on_podcast_filter_edited, |app| app.rebuild_podcast_rows());
        on!(on_podcast_toggle, |app, index| app
            .podcast_toggle(index as usize));
        on!(on_podcast_select_all, |app, all| app
            .podcast_select_all(all));
        // album tracks (details panel)
        on!(on_album_remove, |app| app.album_remove());
        on!(on_album_add_files, |app| app.album_add_files());
        on!(on_album_apply, |app| app.album_apply());
        on!(on_album_revert, |app| app.album_revert());
        on!(on_album_move, |app, delta| app.album_move(delta));
        on!(on_album_drop, |app, from, gap| {
            app.album_drop(from.max(0) as usize, gap.max(0) as usize)
        });
        ui.on_open_url(|url| {
            let _ = open::that(url.as_str());
        });

        // log panel
        on!(on_log_filter_changed, |app| app.rebuild_log());
        on!(on_log_clear, |app| {
            app.state.borrow_mut().log.lines.clear();
            app.rebuild_log();
        });
        on!(on_log_copy, |app| app.copy_log());
        ui.on_log_open_folder(|| {
            let _ = open::that(logfile::log_dir());
        });

        // close: abort current task
        let w = Rc::downgrade(self);
        ui.window().on_close_requested(move || {
            if let Some(app) = w.upgrade() {
                app.request_abort();
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
    // Worker
    // -----------------------------------------------------------------------

    pub fn handle_event(self: &Rc<Self>, event: Event) {
        let ui = self.ui();
        match event {
            Event::Log(level, msg) => self.log(level, &msg),
            Event::Status(msg) => self.status(&msg),
            Event::DevicesFound(scan, devices, labels) => self.devices_found(scan, devices, labels),
            Event::DevicesWatched(scan, devices, labels) => {
                self.devices_watched(scan, devices, labels)
            }
            Event::DeviceOpened(scan, opened) => self.device_opened(scan, opened),
            Event::ShowLog => self.show_log(),
            Event::Total(current, total) | Event::Episode(current, total) => {
                let text = if matches!(event, Event::Episode(..)) {
                    trf(
                        "Episode {}/{}",
                        &[&(current + 1).to_string(), &total.to_string()],
                    )
                } else {
                    format!("Total {}/{total}", current + 1)
                };
                ui.set_total_visible(true);
                ui.set_total_text(text.into());
                ui.set_total_progress(ratio(current + 1, total));
            }
            Event::ImportPlan(total) => self.start_transfer(total),
            Event::Feed(url, result) => self.feed_loaded(url, result),
            Event::PodcastSearch(token, results) => self.podcast_add_found(token, results),
            Event::PodcastChecked(token, result) => self.podcast_add_checked(token, result),
            Event::PodcastArtwork(token, index, data) => {
                self.podcast_add_artwork(token, index, &data)
            }
            Event::Story(uuid, current, total) => {
                ui.set_story_visible(true);
                ui.set_story_text(uuid.into());
                ui.set_story_progress(ratio(current + 1, total));
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
                self.stop_transfer();
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
        let w = Rc::downgrade(self);
        self.watch_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(2),
            move || {
                if let Some(app) = w.upgrade() {
                    app.watch_devices();
                }
            },
        );
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
        assert!(is_newer("v4.0.0-beta.1", "v4.0.0"), "beta vs stable");
        assert!(is_newer("v4.0.0-beta.1", "v4.0.0-beta.2"), "beta vs beta");
        assert!(is_newer("v4.0.0-beta.2", "v4.0.0-rc.1"), "beta vs rc");
        assert!(is_newer("v4.0.0-alpha.3", "v4.0.0-beta.1"), "alpha vs beta");
        assert!(!is_newer("v4.0.0-beta.1", "v3.1.4"), "beta vs older stable");
        assert!(!is_newer("v4.0.0", "v4.0.1-beta.1"), "stable vs beta");
        assert!(
            is_newer("v4.0.0-beta.1", "v4.0.1-beta.1"),
            "beta vs next beta"
        );
    }
}
