//! The device: search, opening, information, quick changes (move, hide).

use super::*;

/// The devices found, and the one opened.
#[derive(Default)]
pub(super) struct Devices {
    pub(super) paths: Vec<PathBuf>,
    /// names of `paths` in the selector
    pub(super) labels: Vec<String>,
    /// id of the last device search / opening: older results are ignored
    pub(super) scan: u32,
    pub(super) opened: Option<SharedDevice>,
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
        // fresh details (story count...), the cached ones while a task runs
        let details = st
            .devices
            .opened
            .as_ref()
            .and_then(|d| Self::lock_device(d).ok().map(|d| d.info()))
            .unwrap_or_else(|| dev.info.clone());
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

    pub(super) fn set_devices(&self, devices: Vec<PathBuf>) {
        let labels = device_labels(&devices);
        self.set_devices_labelled(devices, labels);
    }

    pub(super) fn set_devices_labelled(&self, devices: Vec<PathBuf>, labels: Vec<String>) {
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

    /// A new device search / opening: its id, the previous ones are outdated.
    pub(super) fn next_scan(&self) -> u32 {
        let mut st = self.state.borrow_mut();
        st.devices.scan = st.devices.scan.wrapping_add(1);
        st.devices.scan
    }

    /// Searches the devices in the background (slow drives, network mounts:
    /// the window stays responsive), see `devices_found`.
    pub fn refresh_devices(&self) {
        let scan = self.next_scan();
        self.close_device();
        self.set_devices_labelled(Vec::new(), Vec::new());
        let ui = self.ui();
        self.set_device_index(-1);
        ui.set_device_icon(lunii_icon());
        ui.set_scanning(true);
        ui.set_device_placeholder(tr("Searching for devices...").into());
        self.status(&tr("🔍 Searching for Lunii and Flam devices..."));
        let sink = sink();
        std::thread::spawn(move || {
            // LUNII_DEVICES replaces the detection (tests, or devices mounted elsewhere)
            let devices: Vec<PathBuf> = match std::env::var_os("LUNII_DEVICES") {
                Some(list) => std::env::split_paths(&list)
                    .filter(|p| device::is_lunii(p) || device::is_flam(p))
                    .collect(),
                None => device::find_devices(None),
            };
            let labels = device_labels(&devices);
            sink(Event::DevicesFound(scan, devices, labels));
        });
    }

    pub(super) fn devices_found(&self, scan: u32, devices: Vec<PathBuf>, labels: Vec<String>) {
        if scan != self.state.borrow().devices.scan {
            return;
        }
        let ui = self.ui();
        ui.set_scanning(false);
        self.set_devices_labelled(devices.clone(), labels);
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

    pub(super) fn close_device(&self) {
        {
            let mut st = self.state.borrow_mut();
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
        ui.set_nm_available(false);
        ui.set_nm_active(false);
        self.rebuild_rows();
    }

    pub(crate) fn select_device(&self, index: i32) {
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
            self.set_devices(devices);
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
        self.status(&trf("⏳ Reading {}...", &[&label]));
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
        scan: u32,
        opened: Result<Box<worker::OpenedDevice>, String>,
    ) {
        if scan != self.state.borrow().devices.scan {
            return;
        }
        let ui = self.ui();
        ui.set_scanning(false);
        let worker::OpenedDevice { dev, info, stories } = match opened {
            Ok(d) => *d,
            Err(e) => {
                let error = tr("🛑 PermissionError : Unable to open this device");
                self.log(Level::Error, &e);
                self.status(&error);
                return;
            }
        };
        let version = info.version;
        let is_flam = version == DeviceVersion::FlamV1;
        let sizes_unknown = stories.iter().any(|s| s.size.is_none());
        let nm_supported = info.config.is_some();
        let nm_active = info
            .config
            .as_ref()
            .is_some_and(|c| c[LUNII_CFGPOS_NM_ENABLED] == 1);

        {
            let mut st = self.state.borrow_mut();
            st.devices.opened = Some(Arc::new(Mutex::new(dev)));
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
        // a Lunii v3 without story keys can't import: show why
        if version == DeviceVersion::LuniiV3
            && !self
                .state
                .borrow()
                .devices
                .info
                .as_ref()
                .unwrap()
                .can_import
        {
            self.show_log();
        }
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
    pub(super) fn device_op(
        &self,
        op: impl FnOnce(&mut dyn Device, &Context) -> anyhow::Result<()>,
    ) -> bool {
        let Some(dev) = self.state.borrow().devices.opened.clone() else {
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
            // order and hidden state from the device; the rest is unchanged
            // (no disk read on the UI thread: albums, names...)
            let mut known: HashMap<Uuid, StoryInfo> = self
                .state
                .borrow_mut()
                .list
                .stories
                .drain(..)
                .map(|s| (s.uuid, s))
                .collect();
            let reordered: Option<Vec<StoryInfo>> = dev
                .stories()
                .iter()
                .map(|s| {
                    known.remove(&s.uuid).map(|info| StoryInfo {
                        hidden: s.hidden,
                        ..info
                    })
                })
                .collect();
            // a story not listed yet: read again
            let stories = reordered.unwrap_or_else(|| worker::snapshot(dev.as_ref()));
            (result, stories)
        };
        self.state.borrow_mut().list.stories = stories;
        // a measure may have failed on a story moved meanwhile (hide)
        if self.state.borrow().durations.measuring {
            self.forget_durations();
        }
        self.rebuild_rows();
        match result {
            Ok(()) => true,
            Err(e) => {
                self.log(Level::Error, &format!("{e:#}"));
                false
            }
        }
    }
}
