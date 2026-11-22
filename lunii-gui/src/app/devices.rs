//! The device: search, opening, information, quick changes (move, hide).

use super::*;

/// The devices found, and the one opened.
#[derive(Default)]
pub(super) struct Devices {
    pub(super) paths: Vec<PathBuf>,
    /// names of `paths` in the selector
    pub(super) labels: Vec<String>,
    /// a background check for plugged / unplugged devices is running
    pub(super) watching: bool,
    /// the opened device, on its thread (opened with the current
    /// `Topic::Device` ticket)
    pub(super) opened: Option<DeviceActor>,
    /// details of `opened`
    pub(super) info: Option<DevInfo>,
}

/// Cached details of the opened device.
pub(crate) struct DevInfo {
    pub(super) mount: PathBuf,
    pub(super) version: DeviceVersion,
    pub(super) snu: String,
    pub(super) info: String,
    /// "Lunii v3", "Flam"...
    pub(super) model: String,
    pub(super) firmware: String,
    pub(super) hw_text: String,
    pub(super) can_import: bool,
    /// archives this device can import
    pub(super) extensions: &'static [&'static str],
    /// export all possible (Flam, Lunii v1/v2, or v3 with device key)
    pub(super) export_all: bool,
    pub(super) config: Option<Vec<u16>>,
}

/// Details of an opened device (reads it: in the background).
pub(super) fn describe_device(dev: &dyn Device, path: &Path) -> DevInfo {
    let version = dev.version();
    let is_flam = version == DeviceVersion::FlamV1;
    let lunii = dev.as_lunii();
    let info = dev.info();
    let firmware = if is_flam {
        info.lines()
            .find_map(|l| l.strip_prefix("- Main firmware : ").map(str::to_string))
            .unwrap_or_default()
    } else {
        lunii.map(|l| l.firmware()).unwrap_or_default()
    };
    let model = tr(&version.to_string());
    let hw_text = format!("{model}, FW: {firmware}");
    let export_all = is_flam
        || version < DeviceVersion::LuniiV3
        || lunii.is_some_and(|l| l.device_key.is_some());
    DevInfo {
        mount: path.to_path_buf(),
        version,
        snu: dev.snu_str(),
        info,
        model,
        firmware,
        hw_text,
        can_import: dev.can_import(),
        extensions: dev.supported_extensions(),
        export_all,
        // only for a Lunii whose firmware handles the night mode
        config: lunii
            .filter(|l| l.night_mode_supported())
            .map(|l| l.config.clone()),
    }
}

/// Names of the devices in the selector: their model ("Lunii v3", "Flam"),
/// with their folder when several have the same.
pub(super) fn device_labels(devices: &[PathBuf]) -> Vec<String> {
    let models: Vec<String> = devices
        .iter()
        .map(|d| tr(&device::device_model(d).to_string()))
        .collect();
    models
        .iter()
        .zip(devices)
        .map(|(model, path)| {
            if models.iter().filter(|m| *m == model).count() > 1 {
                let folder = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                format!("{model} ({folder})")
            } else {
                model.clone()
            }
        })
        .collect()
}

/// The devices plugged in and their names (reads them: in the background).
fn detect_devices() -> (Vec<PathBuf>, Vec<String>) {
    // LUNII_DEVICES replaces the detection (tests, or devices mounted elsewhere)
    let devices: Vec<PathBuf> = match std::env::var_os("LUNII_DEVICES") {
        Some(list) => std::env::split_paths(&list)
            .filter(|p| device::is_lunii(p) || device::is_flam(p))
            .collect(),
        None => device::find_devices(None),
    };
    let labels = device_labels(&devices);
    (devices, labels)
}

pub(super) fn lunii_icon() -> slint::Image {
    decode_image(include_bytes!("../../ui/img/lunii.png")).unwrap_or_default()
}

pub(super) fn flam_icon() -> slint::Image {
    decode_image(include_bytes!("../../ui/img/flam.png")).unwrap_or_default()
}

impl App {
    pub(super) fn update_device_status(&self) {
        let ui = self.ui();
        let st = self.state.borrow();
        let Some(dev) = st.devices.info.as_ref() else {
            ui.set_snu("".into());
            return;
        };
        ui.set_snu(dev.snu.clone().into());
        ui.set_hw_version(dev.hw_text.clone().into());
        let free = lunii_core::format::megabytes(free_space(&dev.mount).unwrap_or(0));
        ui.set_free_space(format!("{free} MB").into());
        ui.set_low_space(free < 500);
        let count = st.list.visible.len();
        ui.set_story_count(if count > 0 {
            format!("{count} {}", tr("stories")).into()
        } else {
            "".into()
        });
    }

    /// Lines of the device information dialog, and the raw technical details.
    pub(super) fn device_info(&self) -> Option<(Vec<(String, String)>, String)> {
        let st = self.state.borrow();
        let dev = st.devices.info.as_ref()?;
        // updated with the stories (see `Event::Stories`)
        let details = dev.info.clone();
        let mb = |bytes: u64| lunii_core::format::megabytes(bytes).to_string();
        let space = match (free_space(&dev.mount), total_space(&dev.mount)) {
            (Ok(free), Ok(total)) => trf("{} MB free of {} MB", &[&mb(free), &mb(total)]),
            (Ok(free), Err(_)) => format!("{} MB", mb(free)),
            _ => "?".into(),
        };
        let hidden = st.list.stories.iter().filter(|s| s.hidden).count();
        let stories = if hidden > 0 {
            trf(
                "{} ({} hidden)",
                &[&st.list.stories.len().to_string(), &hidden.to_string()],
            )
        } else {
            st.list.stories.len().to_string()
        };
        let import = if dev.can_import {
            tr("Yes")
        } else if dev.version == DeviceVersion::LuniiV3 {
            tr("No: the story keys of this Lunii v3 are missing")
        } else {
            tr("No")
        };
        let rows = vec![
            (tr("Model"), dev.model.clone()),
            (tr("Firmware"), dev.firmware.clone()),
            ("SNU".into(), dev.snu.clone()),
            (tr("Location"), dev.mount.display().to_string()),
            (tr("Storage"), space),
            (tr("Stories"), stories),
            (tr("Story import"), import),
        ];
        Some((rows, details))
    }

    pub(super) fn show_device_info(&self) {
        let Some((rows, details)) = self.device_info() else {
            return;
        };
        let ui = self.ui();
        let rows: Vec<InfoRow> = rows
            .into_iter()
            .map(|(label, value)| InfoRow {
                label: label.into(),
                value: value.into(),
            })
            .collect();
        ui.set_device_info_rows(ModelRc::new(VecModel::from(rows)));
        ui.set_device_details(details.into());
        ui.set_dialog(DialogKind::DeviceInfo);
    }

    pub(super) fn copy_device_info(&self) {
        let Some((rows, details)) = self.device_info() else {
            return;
        };
        let mut info: String = rows
            .iter()
            .map(|(label, value)| format!("{label} : {value}\n"))
            .collect();
        info.push('\n');
        info.push_str(&details);
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(info);
        }
        self.ui()
            .set_status(tr("Device info sent to clipboard.").into());
    }

    /// Selects a device entry in the combobox (index and displayed text).
    pub(super) fn set_device_index(&self, index: i32) {
        let ui = self.ui();
        let text = self
            .state
            .borrow()
            .devices
            .labels
            .get(index.max(0) as usize)
            .filter(|_| index >= 0)
            .cloned();
        ui.set_device_index(index);
        ui.set_device_value(text.unwrap_or_default().into());
    }

    pub(super) fn set_devices(&self, devices: Vec<PathBuf>, labels: Vec<String>) {
        self.devices_model.set_vec(
            labels
                .iter()
                .map(|l| SharedString::from(l.as_str()))
                .collect::<Vec<_>>(),
        );
        let mut st = self.state.borrow_mut();
        st.devices.paths = devices;
        st.devices.labels = labels;
    }

    /// A new device search / opening: the previous ones are outdated, and
    /// so is everything about the device opened.
    pub(super) fn next_scan(&self) -> Ticket {
        self.state.borrow_mut().tickets.issue(Topic::Device)
    }

    /// The opened device: its thread, and the ticket of its opening.
    pub(super) fn device_handle(&self) -> Option<(DeviceHandle, Ticket)> {
        let st = self.state.borrow();
        let handle = st.devices.opened.as_ref()?.handle();
        Some((handle, st.tickets.current(Topic::Device)))
    }

    /// An answer about the device opened with `ticket`: is it still the one
    /// opened?
    pub(super) fn is_current(&self, ticket: Ticket) -> bool {
        self.state.borrow().tickets.is_current(ticket)
    }

    /// Searches the devices in the background (slow drives, network mounts:
    /// the window stays responsive), see `devices_found`.
    pub fn refresh_devices(&self) {
        let scan = self.next_scan();
        self.close_device();
        self.set_devices(Vec::new(), Vec::new());
        let ui = self.ui();
        self.set_device_index(-1);
        ui.set_device_icon(lunii_icon());
        ui.set_scanning(true);
        ui.set_device_placeholder(tr("Searching for devices...").into());
        self.status(&tr("🔍 Searching for Lunii and Flam devices..."));
        let sink = sink();
        std::thread::spawn(move || {
            let (devices, labels) = detect_devices();
            sink(Event::DevicesFound(scan, devices, labels));
        });
    }

    /// Checks in the background whether a device was plugged in or unplugged
    /// (every few seconds), see `devices_watched`. Not during a search, the
    /// opening of a device or a task.
    pub(super) fn watch_devices(&self) {
        let ui = self.ui();
        if ui.get_scanning() || ui.get_busy() {
            return;
        }
        let scan = {
            let mut st = self.state.borrow_mut();
            if std::mem::replace(&mut st.devices.watching, true) {
                return;
            }
            // not a new request: the device opened stays current
            st.tickets.current(Topic::Device)
        };
        let sink = sink();
        std::thread::spawn(move || {
            let (devices, labels) = detect_devices();
            sink(Event::DevicesWatched(scan, devices, labels));
        });
    }

    /// The devices plugged in now: the list follows them. The opened device
    /// unplugged is closed; a new one is opened when none is.
    pub(super) fn devices_watched(&self, scan: Ticket, devices: Vec<PathBuf>, labels: Vec<String>) {
        let ui = self.ui();
        let (known, opened) = {
            let mut st = self.state.borrow_mut();
            st.devices.watching = false;
            // a search, an opening or a task started meanwhile
            if !st.tickets.is_current(scan) || ui.get_scanning() || ui.get_busy() {
                return;
            }
            let opened = st.devices.info.as_ref().map(|d| d.mount.clone());
            (st.devices.paths.clone(), opened)
        };
        if devices == known {
            return;
        }
        let added = devices.iter().position(|d| !known.contains(d));
        let current = opened
            .as_ref()
            .and_then(|mount| devices.iter().position(|d| d == mount));
        let empty = devices.is_empty();
        let added_label = added.map(|i| labels[i].clone());
        self.set_devices(devices, labels);
        self.set_device_index(current.map_or(-1, |i| i as i32));
        ui.set_device_placeholder(if empty {
            tr("No Lunii detected 😥").into()
        } else {
            tr("Select your Lunii").into()
        });
        if opened.is_some() && current.is_none() {
            self.close_device();
            ui.set_device_icon(lunii_icon());
            self.status(&tr("🔌 Storyteller unplugged"));
        }
        match (current, added) {
            (None, Some(index)) => {
                self.set_device_index(index as i32);
                self.open_device(index as i32, true);
            }
            // another device stays opened: the new one is only announced
            (Some(_), Some(_)) => self.status(&trf(
                "🔌 {} detected: choose it in the Device menu to open it",
                &[&added_label.unwrap_or_default()],
            )),
            _ => {}
        }
    }

    pub(super) fn devices_found(&self, scan: Ticket, devices: Vec<PathBuf>, labels: Vec<String>) {
        if !self.is_current(scan) {
            return;
        }
        let ui = self.ui();
        ui.set_scanning(false);
        self.set_devices(devices.clone(), labels);
        self.status("");

        if devices.is_empty() {
            ui.set_device_placeholder(tr("No Lunii detected 😥").into());
            self.status(&tr("No Lunii detected 😥"));
        } else {
            ui.set_device_placeholder(tr("Select your Lunii").into());
            if devices.len() == 1 {
                self.set_device_index(0);
                self.select_device(0);
            }
        }
    }

    pub(super) fn close_device(&self) {
        {
            let mut st = self.state.borrow_mut();
            // its thread ends once its jobs are done (not waited for)
            st.devices.opened = None;
            st.devices.info = None;
            st.list.stories.clear();
            st.list.selected.clear();
            st.list.anchor = None;
            st.list.current = None;
            st.list.details_uuid = None;
        }
        let ui = self.ui();
        ui.set_device_ready(false);
        ui.set_list_placeholder("".into());
        ui.set_nm_available(false);
        ui.set_nm_active(false);
        self.rebuild_rows();
    }

    pub(crate) fn select_device(&self, index: i32) {
        self.open_device(index, false);
    }

    /// Opens the device `index` of the list (`detected`: just plugged in,
    /// opened by itself).
    fn open_device(&self, index: i32, detected: bool) {
        let Some(path) = self
            .state
            .borrow()
            .devices
            .paths
            .get(index as usize)
            .cloned()
        else {
            return;
        };
        self.close_device();
        let ui = self.ui();

        if !device::is_lunii(&path) && !device::is_flam(&path) {
            self.log(
                Level::Error,
                &format!("{} is not a recognized device.", path.display()),
            );
            let mut devices = self.state.borrow().devices.paths.clone();
            devices.remove(index as usize);
            let labels = device_labels(&devices);
            self.set_devices(devices, labels);
            self.set_device_index(-1);
            return;
        }

        // read in the background (all the stories): see `device_opened`
        let scan = self.next_scan();
        let label = self
            .state
            .borrow()
            .devices
            .labels
            .get(index as usize)
            .cloned()
            .unwrap_or_default();
        ui.set_scanning(true);
        let message = if detected {
            trf("🔌 {} detected, reading its stories...", &[&label])
        } else {
            trf("⏳ Reading {}...", &[&label])
        };
        // in the middle of the (empty) list too
        ui.set_list_placeholder(message.as_str().into());
        self.status(&message);
        let ctx = self.ctx();
        let sink = sink();
        std::thread::spawn(move || {
            // everything that reads the device (all the stories) is done here
            let opened = device::open_device(&ctx, &path)
                .map(|dev| {
                    Box::new(worker::OpenedDevice {
                        info: describe_device(dev.as_ref(), &path),
                        stories: worker::snapshot(dev.as_ref()),
                        dev,
                    })
                })
                .map_err(|e| format!("{e:#}"));
            sink(Event::DeviceOpened(scan, opened));
        });
    }

    pub(super) fn device_opened(
        &self,
        scan: Ticket,
        opened: Result<Box<worker::OpenedDevice>, String>,
    ) {
        if !self.is_current(scan) {
            return;
        }
        let ui = self.ui();
        ui.set_scanning(false);
        let worker::OpenedDevice { dev, info, stories } = match opened {
            Ok(d) => *d,
            Err(e) => {
                let error = tr("🛑 PermissionError : Unable to open this device");
                ui.set_list_placeholder("".into());
                self.log(Level::Error, &e);
                self.status(&error);
                return;
            }
        };
        let is_flam = info.version == DeviceVersion::FlamV1;
        // a Lunii v3 without story keys can't import
        let missing_keys = info.version == DeviceVersion::LuniiV3 && !info.can_import;
        let sizes_unknown = stories.iter().any(|s| s.size.is_none());
        let nm_supported = info.config.is_some();
        let nm_active = info
            .config
            .as_ref()
            .is_some_and(|c| c[LUNII_CFGPOS_NM_ENABLED] == 1);

        {
            let mut st = self.state.borrow_mut();
            st.devices.opened = Some(DeviceActor::spawn(dev));
            st.devices.info = Some(info);
            st.list.stories = stories;
        }
        self.forget_durations();

        ui.set_device_icon(if is_flam { flam_icon() } else { lunii_icon() });
        ui.set_nm_available(nm_supported);
        ui.set_nm_active(nm_active);
        ui.set_device_ready(true);
        ui.set_list_placeholder(
            tr("Drop your story archives here, or use Stories / Import").into(),
        );
        self.rebuild_rows();
        self.status("");
        self.follow_device_podcasts();

        if sizes_unknown {
            self.ensure_sizes();
        }
        // the log tells why
        if missing_keys {
            self.show_log();
        }
    }

    /// A quick change of the device (move, hide, night mode), on its thread:
    /// see `quick_changed`. Not while a task runs (it would only be done at
    /// its end).
    pub(super) fn device_op(
        &self,
        change: QuickChange,
        op: impl FnOnce(&mut dyn Device, &Context) -> anyhow::Result<()> + Send + 'static,
    ) {
        let Some((handle, ticket)) = self.device_handle() else {
            return;
        };
        if self.ui().get_busy() {
            self.status(&tr(
                "⏳ The storyteller is busy, try again once the current task is done",
            ));
            return;
        }
        let known: HashSet<Uuid> = self
            .state
            .borrow()
            .list
            .stories
            .iter()
            .map(|s| s.uuid)
            .collect();
        let ctx = self.ctx();
        let sink = sink();
        let sent = handle.run(move |dev| {
            let result = worker::quick_change(dev, &ctx, &known, op);
            sink(Event::QuickChanged(ticket, Box::new(result), change));
        });
        if !sent {
            self.status(&tr("🛑 The storyteller is no longer available"));
        }
    }

    /// A quick change done: the list follows the device.
    pub(super) fn quick_changed(
        &self,
        ticket: Ticket,
        done: worker::QuickResult,
        change: QuickChange,
    ) {
        if !self.is_current(ticket) {
            return;
        }
        let worker::QuickResult {
            result,
            order,
            stories,
            details,
        } = done;
        {
            let mut st = self.state.borrow_mut();
            let stories = stories.unwrap_or_else(|| {
                // order and flags from the device; the rest is unchanged
                let mut known: HashMap<Uuid, StoryInfo> =
                    st.list.stories.drain(..).map(|s| (s.uuid, s)).collect();
                order
                    .into_iter()
                    .filter_map(|(uuid, hidden, night_mode, dir)| {
                        known.remove(&uuid).map(|info| StoryInfo {
                            hidden,
                            night_mode,
                            dir,
                            ..info
                        })
                    })
                    .collect()
            });
            st.list.stories = stories;
            if let Some(info) = st.devices.info.as_mut() {
                info.info = details;
            }
        }
        // a measure may have failed on a story moved meanwhile (hide)
        if self.state.borrow().durations.measuring {
            self.forget_durations();
        }
        self.rebuild_rows();
        if let Err(e) = &result {
            self.log(Level::Error, e);
        }
        match change {
            QuickChange::Flags if result.is_ok() => self.status(&tr("✅ Stories updated...")),
            QuickChange::Flags => {
                self.status(&tr("🛑 Unable to update story ..."));
                self.show_log();
            }
            QuickChange::Move { ids, scroll } => {
                // keep the first moved story in view
                if let Some(row) = self.focus_moved(&ids)
                    && scroll
                {
                    self.ui().invoke_ensure_visible(row as i32);
                }
            }
        }
    }
}

/// What a quick change of the device is, for what follows it.
pub(crate) enum QuickChange {
    /// hidden or night mode flags: a message
    Flags,
    /// stories moved: the first one becomes the current row (`scroll`: and
    /// is brought into view)
    Move { ids: Vec<Uuid>, scroll: bool },
}
