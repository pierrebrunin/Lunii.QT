//! Main window controller (with the night mode and log panels).

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lunii_core::constants::*;
use lunii_core::device::{self, common::free_space};
use lunii_core::{Context, Device, Level};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use uuid::Uuid;

use crate::i18n::{tr, trf};
use crate::logfile;
use crate::worker::{self, Event, GuiReporter, SharedDevice, Sink, StoryInfo, Task};
use crate::{EpisodeRow, LogLine, MainWindow, StoryRow, Theme};
use podcast::PodcastView;

mod actions;
mod album;
mod log_panel;
mod podcast;

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

/// Remaining time estimation of an import, from the bytes written to the device.
struct Eta {
    start: Instant,
    /// bytes expected for the whole import
    total: u64,
    /// `Context::progress()` when the import started
    base: u64,
    /// (time, bytes written) over the last `ETA_WINDOW`
    samples: VecDeque<(Instant, u64)>,
}

/// The speed is measured over this window: reactive, but smoothing the
/// pauses (file closing, ffmpeg transcoding...).
const ETA_WINDOW: Duration = Duration::from_secs(20);
/// No estimation before this delay: the first seconds aren't representative.
const ETA_WARMUP: Duration = Duration::from_secs(4);

impl Eta {
    /// Remaining duration, `None` while it can't be estimated yet.
    fn estimate(&mut self, now: Instant, done: u64) -> Option<Duration> {
        self.samples.push_back((now, done));
        while self.samples.len() > 2 && now.duration_since(self.samples[0].0) > ETA_WINDOW {
            self.samples.pop_front();
        }
        let elapsed = now.duration_since(self.start);
        if elapsed < ETA_WARMUP || done == 0 {
            return None;
        }
        let (t0, d0) = self.samples[0];
        let window = now.duration_since(t0).as_secs_f64();
        let mut rate = if window > 1.0 {
            (done - d0) as f64 / window
        } else {
            0.0
        };
        if rate <= 0.0 {
            // nothing written lately (transcoding...): average since the start
            rate = done as f64 / elapsed.as_secs_f64();
        }
        let remaining = self.total.saturating_sub(done) as f64;
        Some(Duration::from_secs_f64(remaining / rate))
    }
}

/// `45 s`, `3 min 20 s`, `1 h 05 min`: rounded, so it doesn't flicker.
fn format_eta(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{} s", s.div_ceil(5).max(1) * 5)
    } else if s < 3600 {
        let s = s.div_ceil(10) * 10;
        format!("{} min {:02} s", s / 60, s % 60)
    } else {
        let m = s.div_ceil(60);
        format!("{} h {:02} min", m / 60, m % 60)
    }
}

/// Cached details of the opened device.
struct DevInfo {
    mount: PathBuf,
    version: DeviceVersion,
    snu: String,
    info: String,
    hw_text: String,
    can_import: bool,
    /// export all possible (Flam, Lunii v1/v2, or v3 with device key)
    export_all: bool,
    config: Option<Vec<u16>>,
}

#[derive(Default)]
struct State {
    devices: Vec<PathBuf>,
    device: Option<SharedDevice>,
    dev: Option<DevInfo>,
    stories: Vec<StoryInfo>,
    /// indexes in `stories` of the displayed rows
    visible: Vec<usize>,
    selected: HashSet<Uuid>,
    anchor: Option<usize>,
    current: Option<usize>,
    details_uuid: Option<Uuid>,
    worker: Option<(std::thread::JoinHandle<()>, Context)>,
    pending_drop: Vec<PathBuf>,
    fw: Option<(lunii_core::firmware::AuthToken, Vec<String>)>,
    /// (level 0..3, formatted line)
    log: Vec<(i32, String)>,
    /// highest level logged while the log panel was hidden
    log_alert: i32,
    eta: Option<Eta>,
    /// podcast of the selection dialog, and its selected episodes
    podcast: Option<(Box<PodcastView>, HashSet<usize>)>,
    /// followed podcasts and their feeds (Podcasts view)
    podcasts: podcast::Podcasts,
    /// album shown in the details panel, with its pending changes
    album: Option<album::AlbumEdit>,
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
        ui.set_ffmpeg_present(which_ffmpeg().is_some());
        let normalize = lunii_core::settings::get_bool("normalize_volume");
        ui.set_normalize_volume(normalize);
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
            app.state.borrow_mut().selected.clear();
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
        ui.on_select_all(with(|app| {
            {
                let mut st = app.state.borrow_mut();
                let all: Vec<Uuid> = st.visible.iter().map(|&i| st.stories[i].uuid).collect();
                st.selected = all.into_iter().collect();
            }
            app.selection_changed();
        }));
        ui.on_abort(with(|app| app.abort()));
        ui.on_device_info_clicked(with(|app| app.copy_device_info()));
        ui.on_confirm_remove_accepted(with(|app| {
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
        ui.on_podcast_add(with(|app| app.podcast_add()));
        ui.on_podcast_refresh(with(|app| app.podcast_refresh()));
        ui.on_podcast_unfollow(with(|app| app.podcast_unfollow()));
        let w = Rc::downgrade(self);
        ui.on_podcast_select(move |index| {
            if let Some(app) = w.upgrade() {
                app.podcast_select(index);
            }
        });
        ui.on_podcast_filter_edited(with(|app| app.rebuild_podcast_rows()));
        ui.on_podcast_mode_changed(with(|app| app.podcast_mode_changed()));
        ui.on_podcast_accepted(with(|app| app.podcast_import()));
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
        ui.on_open_url(|url| {
            let _ = open::that(url.as_str());
        });

        // log panel
        ui.on_log_filter_changed(with(|app| app.rebuild_log()));
        ui.on_log_clear(with(|app| {
            app.state.borrow_mut().log.clear();
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
                && let Some((_, ctx)) = app.state.borrow().worker.as_ref()
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
                    let accept = app.state.borrow().device.is_some() && !app.ui().get_busy();
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

    fn update_device_status(&self) {
        let ui = self.ui();
        let st = self.state.borrow();
        let Some(dev) = st.dev.as_ref() else {
            ui.set_snu("".into());
            return;
        };
        ui.set_snu(dev.snu.clone().into());
        ui.set_hw_version(dev.hw_text.clone().into());
        let free = free_space(&dev.mount).unwrap_or(0) / 1024 / 1024;
        ui.set_free_space(format!("{free} MB").into());
        ui.set_low_space(free < 500);
        let count = st.visible.len();
        ui.set_story_count(if count > 0 {
            format!("{count} {}", tr("stories")).into()
        } else {
            "".into()
        });
    }

    fn copy_device_info(&self) {
        let Some(info) = self.state.borrow().dev.as_ref().map(|d| d.info.clone()) else {
            return;
        };
        self.log(Level::Info, &info);
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(info);
        }
        self.ui()
            .set_status(tr("Device info sent to clipboard.").into());
    }

    // -----------------------------------------------------------------------
    // Devices
    // -----------------------------------------------------------------------

    /// Selects a device entry in the combobox (index and displayed text).
    fn set_device_index(&self, index: i32) {
        let ui = self.ui();
        let text = self
            .state
            .borrow()
            .devices
            .get(index.max(0) as usize)
            .filter(|_| index >= 0)
            .map(|d| d.display().to_string());
        ui.set_device_index(index);
        ui.set_device_value(text.unwrap_or_default().into());
    }

    fn set_devices(&self, devices: Vec<PathBuf>) {
        self.devices_model.set_vec(
            devices
                .iter()
                .map(|d| SharedString::from(d.display().to_string()))
                .collect::<Vec<_>>(),
        );
        self.state.borrow_mut().devices = devices;
    }

    pub fn refresh_devices(&self) {
        // LUNII_DEVICES replaces the detection (tests, or devices mounted elsewhere)
        let devices = match std::env::var_os("LUNII_DEVICES") {
            Some(list) => std::env::split_paths(&list)
                .filter(|p| device::is_lunii(p) || device::is_flam(p))
                .collect(),
            None => device::find_devices(None),
        };
        self.close_device();
        self.set_devices(devices.clone());
        let ui = self.ui();
        self.set_device_index(-1);
        ui.set_device_icon(lunii_icon());
        self.status("");

        if devices.is_empty() {
            ui.set_device_placeholder(tr("No Lunii detected 😥").into());
            self.status(&tr("No Lunii detected 😥, try File/Open"));
        } else {
            ui.set_device_placeholder(tr("Select your Lunii").into());
            if devices.len() == 1 {
                self.set_device_index(0);
                self.select_device(0);
            }
        }
    }

    fn close_device(&self) {
        {
            let mut st = self.state.borrow_mut();
            st.device = None;
            st.dev = None;
            st.stories.clear();
            st.selected.clear();
            st.anchor = None;
            st.current = None;
            st.details_uuid = None;
        }
        let ui = self.ui();
        ui.set_device_ready(false);
        ui.set_nm_available(false);
        ui.set_nm_active(false);
        self.rebuild_rows();
    }

    pub(crate) fn select_device(&self, index: i32) {
        let Some(path) = self.state.borrow().devices.get(index as usize).cloned() else {
            return;
        };
        self.close_device();
        let ui = self.ui();

        if !device::is_lunii(&path) && !device::is_flam(&path) {
            self.log(
                Level::Error,
                &format!("{} is not a recognized device.", path.display()),
            );
            let mut devices = self.state.borrow().devices.clone();
            devices.remove(index as usize);
            self.set_devices(devices);
            self.set_device_index(-1);
            return;
        }

        let opened = device::open_device(&self.ctx(), &path);
        let dev = match opened {
            Ok(d) => d,
            Err(e) => {
                let error = tr("🛑 PermissionError : Unable to open this device");
                self.log(Level::Error, &format!("{e:#}"));
                self.status(&error);
                return;
            }
        };

        let version = dev.version();
        let is_flam = version == DeviceVersion::FlamV1;
        let lunii = dev.as_lunii();
        let hw_text = match version {
            DeviceVersion::LuniiV1 => format!(
                "Lunii v1, FW: {}",
                lunii.map(|l| l.firmware()).unwrap_or_default()
            ),
            DeviceVersion::LuniiV2 => format!(
                "Lunii v2, FW: {}",
                lunii.map(|l| l.firmware()).unwrap_or_default()
            ),
            DeviceVersion::LuniiV3 => format!(
                "Lunii v3, FW: {}",
                lunii.map(|l| l.firmware()).unwrap_or_default()
            ),
            DeviceVersion::FlamV1 => {
                let fw = dev
                    .info()
                    .lines()
                    .find_map(|l| l.strip_prefix("- Main firmware : ").map(str::to_string));
                format!("Flam v1, FW: {}", fw.unwrap_or_default())
            }
            _ => format!(
                "?v1/v2?, FW: {}",
                lunii.map(|l| l.firmware()).unwrap_or_default()
            ),
        };
        let export_all = is_flam
            || version < DeviceVersion::LuniiV3
            || lunii.is_some_and(|l| l.device_key.is_some());
        let info = DevInfo {
            mount: path.clone(),
            version,
            snu: dev.snu_str(),
            info: dev.info(),
            hw_text,
            can_import: dev.can_import(),
            export_all,
            config: lunii.map(|l| l.config.clone()),
        };
        let stories = worker::snapshot(dev.as_ref());
        let sizes_unknown = stories.iter().any(|s| s.size.is_none());
        let nm_active = info
            .config
            .as_ref()
            .is_some_and(|c| c[LUNII_CFGPOS_NM_ENABLED] == 1);

        {
            let mut st = self.state.borrow_mut();
            st.device = Some(Arc::new(Mutex::new(dev)));
            st.dev = Some(info);
            st.stories = stories;
        }

        ui.set_device_icon(if is_flam { flam_icon() } else { lunii_icon() });
        ui.set_is_flam(is_flam);
        ui.set_show_nm(!is_flam);
        ui.set_nm_available(!is_flam);
        ui.set_nm_active(nm_active);
        ui.set_device_ready(true);
        ui.set_list_placeholder(
            tr("Drop your story archives here, or use Stories / Import").into(),
        );
        self.rebuild_rows();
        self.status("");

        if ui.get_show_size() && sizes_unknown {
            self.launch(Task::Size);
        }
        // a Lunii v3 without story keys can't import: show why
        if version == DeviceVersion::LuniiV3
            && !self.state.borrow().dev.as_ref().unwrap().can_import
        {
            self.show_log();
        }
    }

    // -----------------------------------------------------------------------
    // Story list
    // -----------------------------------------------------------------------

    fn rebuild_rows(&self) {
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
                            .map(|b| format!("{:.1}MB", b as f64 / 1024.0 / 1024.0))
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
                let anchor = st.anchor.unwrap_or(row);
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

    fn key_nav(&self, delta: i32, shift: bool) {
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
                let anchor = st.anchor.unwrap_or(row);
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
    fn selected_uuids(&self) -> Vec<String> {
        let st = self.state.borrow();
        st.visible
            .iter()
            .map(|&i| &st.stories[i])
            .filter(|s| st.selected.contains(&s.uuid))
            .map(|s| s.str_uuid.clone())
            .collect()
    }

    fn selected_ids(&self) -> Vec<Uuid> {
        let st = self.state.borrow();
        st.visible
            .iter()
            .map(|&i| st.stories[i].uuid)
            .filter(|u| st.selected.contains(u))
            .collect()
    }

    /// Updates rows highlight, menu states and details panel.
    fn selection_changed(&self) {
        let ui = self.ui();
        let (current, has_selection, can_export, can_import, can_export_all, can_add_to_album) = {
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
            (
                st.current.map(|c| c as i32).unwrap_or(-1),
                !st.selected.is_empty(),
                can_export,
                can_import,
                dev.is_some_and(|d| d.export_all) && !st.stories.is_empty(),
                can_add_to_album,
            )
        };
        ui.set_current_row(current);
        ui.set_has_selection(has_selection);
        ui.set_can_export(can_export);
        ui.set_can_import(can_import);
        ui.set_can_export_all(can_export_all);
        ui.set_can_add_to_album(can_add_to_album);
        self.update_device_status();
        self.update_details();
    }

    fn update_details(&self) {
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

    fn details_ready(&self, uuid: Uuid, desc: String, picture: Option<Vec<u8>>) {
        if self.state.borrow().details_uuid != Some(uuid) {
            return;
        }
        let ui = self.ui();
        let image = picture.as_deref().and_then(decode_image);
        let no_desc = desc.is_empty() || desc == DESC_NOT_FOUND;
        let album = ui.get_album_visible();
        if no_desc && image.is_none() && !album {
            ui.set_details_visible(false);
            return;
        }
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

    /// The device, for a quick change on the UI thread. Never waits: `None`
    /// (and a message) while a task holds it, the UI would freeze until its end.
    pub(super) fn lock_device(
        dev: &SharedDevice,
    ) -> Result<std::sync::MutexGuard<'_, Box<dyn Device>>, std::sync::TryLockError<()>> {
        match dev.try_lock() {
            Ok(guard) => Ok(guard),
            // a task panicked holding it: the device state is still usable
            Err(std::sync::TryLockError::Poisoned(p)) => Ok(p.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => Err(std::sync::TryLockError::WouldBlock),
        }
    }

    /// Applies a quick change on the device (on the UI thread).
    fn device_op(&self, op: impl FnOnce(&mut dyn Device, &Context) -> anyhow::Result<()>) -> bool {
        let Some(dev) = self.state.borrow().device.clone() else {
            return false;
        };
        let ctx = self.ctx();
        let (result, stories) = {
            let Ok(mut dev) = Self::lock_device(&dev) else {
                self.status(&tr(
                    "⏳ The storyteller is busy, try again once the current task is done",
                ));
                return false;
            };
            let result = op(dev.as_mut(), &ctx);
            (result, worker::snapshot(dev.as_ref()))
        };
        self.state.borrow_mut().stories = stories;
        self.rebuild_rows();
        match result {
            Ok(()) => true,
            Err(e) => {
                self.log(Level::Error, &format!("{e:#}"));
                false
            }
        }
    }

    fn move_selection(&self, offset: i32) {
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

    // -----------------------------------------------------------------------
    // Worker
    // -----------------------------------------------------------------------

    fn launch(&self, task: Task) {
        if self.state.borrow().worker.is_some() {
            return;
        }
        let ctx = self.ctx();
        let device = self.state.borrow().device.clone();
        let audio = lunii_core::convert::audio::AudioOptions {
            normalize_volume: self.ui().get_normalize_volume(),
        };
        let handle = worker::spawn(task, device, audio, ctx.clone(), sink());
        self.state.borrow_mut().worker = Some((handle, ctx));
        let ui = self.ui();
        ui.set_busy(true);
        ui.set_abort_visible(true);
    }

    fn start_eta(self: &Rc<Self>, total: u64) {
        let Some(base) = self
            .state
            .borrow()
            .worker
            .as_ref()
            .map(|(_, c)| c.progress())
        else {
            return;
        };
        if total == 0 {
            return;
        }
        self.state.borrow_mut().eta = Some(Eta {
            start: Instant::now(),
            total,
            base,
            samples: VecDeque::new(),
        });
        self.update_eta();
        let w = Rc::downgrade(self);
        self.eta_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(1),
            move || {
                if let Some(app) = w.upgrade() {
                    app.update_eta();
                }
            },
        );
    }

    fn update_eta(&self) {
        let Some(written) = self
            .state
            .borrow()
            .worker
            .as_ref()
            .map(|(_, c)| c.progress())
        else {
            return;
        };
        let text = {
            let mut st = self.state.borrow_mut();
            let Some(eta) = st.eta.as_mut() else {
                return;
            };
            let done = written.saturating_sub(eta.base);
            if done >= eta.total {
                // last writes, database update...
                String::new()
            } else {
                match eta.estimate(Instant::now(), done) {
                    Some(d) => trf("Time left: {}", &[&format_eta(d)]),
                    None => tr("Estimating time left..."),
                }
            }
        };
        self.ui().set_eta_text(text.into());
    }

    fn stop_eta(&self) {
        self.eta_timer.stop();
        self.state.borrow_mut().eta = None;
        self.ui().set_eta_text("".into());
    }

    fn abort(&self) {
        if let Some((_, ctx)) = self.state.borrow().worker.as_ref() {
            ctx.request_abort();
        } else {
            return;
        }
        self.status(&tr("Abort requested, please wait..."));
    }

    pub fn handle_event(self: &Rc<Self>, event: Event) {
        let ui = self.ui();
        match event {
            Event::Log(level, msg) => self.log(level, &msg),
            Event::Status(msg) => self.status(&msg),
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
            Event::Stories(stories) => {
                self.state.borrow_mut().stories = stories;
                self.rebuild_rows();
            }
            Event::DbLoaded(ok) => {
                if !ok {
                    self.log(Level::Warn, "Failed to fetch official DB");
                }
                let device = self.state.borrow().device.clone();
                // busy: the task refreshes the list at its end
                if let (Some(dev), false) = (device, ui.get_busy())
                    && let Ok(dev) = Self::lock_device(&dev)
                {
                    let stories = worker::snapshot(dev.as_ref());
                    drop(dev);
                    self.state.borrow_mut().stories = stories;
                    self.rebuild_rows();
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
                self.state.borrow_mut().details_uuid = None;
                if let Some((handle, _)) = self.state.borrow_mut().worker.take() {
                    let _ = handle.join();
                }
                ui.set_busy(false);
                ui.set_total_visible(false);
                ui.set_story_visible(false);
                ui.set_file_visible(false);
                ui.set_abort_visible(false);
                ui.set_ffmpeg_present(which_ffmpeg().is_some());
                self.selection_changed();
                self.podcasts_refresh_device();
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
        let worker = self.state.borrow_mut().worker.take();
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

fn lunii_icon() -> slint::Image {
    decode_image(include_bytes!("../../ui/img/lunii.png")).unwrap_or_default()
}

fn flam_icon() -> slint::Image {
    decode_image(include_bytes!("../../ui/img/flam.png")).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{Eta, VecDeque, format_eta, is_newer};
    use std::time::{Duration, Instant};

    #[test]
    fn eta_format() {
        assert_eq!(format_eta(Duration::from_secs(0)), "5 s");
        assert_eq!(format_eta(Duration::from_secs(42)), "45 s");
        assert_eq!(format_eta(Duration::from_secs(200)), "3 min 20 s");
        assert_eq!(format_eta(Duration::from_secs(201)), "3 min 30 s");
        assert_eq!(format_eta(Duration::from_secs(3_900)), "1 h 05 min");
    }

    #[test]
    fn eta_estimate() {
        let t = Instant::now();
        let mut eta = Eta {
            start: t,
            total: 10_000_000,
            base: 0,
            samples: VecDeque::new(),
        };
        let at = |s: u64| t + Duration::from_secs(s);
        // warm-up: no estimation
        assert_eq!(eta.estimate(at(0), 0), None);
        assert_eq!(eta.estimate(at(2), 200_000), None);
        // 100 KB/s, 9.5 MB left
        let d = eta.estimate(at(5), 500_000).unwrap();
        assert_eq!(d.as_secs(), 95);
        // nothing written for 30 s (transcoding): average since the start
        let d = eta.estimate(at(35), 500_000).unwrap();
        assert!(d.as_secs() > 600, "{d:?}");
        // speed measured on the window, not since the start
        let d = eta.estimate(at(45), 2_500_000).unwrap();
        assert!(d.as_secs() < 100, "{d:?}");
    }

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
