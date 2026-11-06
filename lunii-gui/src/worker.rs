//! Background tasks.
//!
//! A task runs in its own thread and talks to the UI with [`Event`]s, delivered
//! in order on the Slint event loop.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use lunii_core::constants::DeviceVersion;
use lunii_core::convert::audio::AudioOptions;
use lunii_core::firmware::{self, AuthToken};
use lunii_core::podcast::{self, Episode, ImportMode, ImportPlan, Podcast};
use lunii_core::{Aborted, Context, Device, Level, Reporter, album, db, tools};
use uuid::Uuid;

use crate::i18n::{tr, trf};

/// Progress expected for the import of `path`: the uncompressed size of the
/// archive (or of the folder). Converted files are counted with their source
/// size (`Context::count_as_source`), so this is exact up to the few metadata
/// files that are not written as is (`story.json`, thumbnail).
fn expected_size(path: &Path) -> u64 {
    fn dir_size(dir: &Path) -> u64 {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        entries
            .filter_map(Result::ok)
            .map(|e| match e.file_type() {
                Ok(t) if t.is_dir() => dir_size(&e.path()),
                Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
                _ => 0,
            })
            .sum()
    }

    if path.is_dir() {
        return dir_size(path);
    }
    lunii_core::archive::Archive::open(path)
        .map(|a| a.uncompressed_size())
        .or_else(|_| std::fs::metadata(path).map(|m| m.len()))
        .unwrap_or(0)
}

/// Progress expected when appending these MP3 files to an album: for each
/// track the audio and the screen.
fn append_expected<'a>(files: impl Iterator<Item = &'a Path>) -> u64 {
    files
        .filter_map(|f| std::fs::metadata(f).ok())
        .map(|m| m.len() + 10_000)
        .sum()
}

fn format_secs(d: u64) -> String {
    if d > 120 {
        format!("{} min {} s", d / 60, d % 60)
    } else {
        format!("{d} s")
    }
}

/// Removes a temporary directory on drop (abort included).
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Files to import: (requested file, archive to import + `true` if temporary).
/// The temporary archives are removed on drop, abort included.
struct PreparedFiles<'a>(Vec<(&'a PathBuf, anyhow::Result<(PathBuf, bool)>)>);

impl Drop for PreparedFiles<'_> {
    fn drop(&mut self) {
        for (_, r) in &self.0 {
            if let Ok((archive, true)) = r {
                let _ = std::fs::remove_file(archive);
            }
        }
    }
}

pub type SharedDevice = Arc<Mutex<Box<dyn Device>>>;

/// What the UI needs to display a story.
#[derive(Debug, Clone)]
pub struct StoryInfo {
    pub uuid: Uuid,
    pub str_uuid: String,
    pub name: String,
    pub night_mode: bool,
    pub official: bool,
    pub hidden: bool,
    pub exportable: bool,
    pub size: Option<u64>,
    /// MP3 album created by Lunii, tracks can be appended
    pub album: bool,
}

/// A device opened in the background, with what the UI needs (read there too).
pub struct OpenedDevice {
    pub dev: Box<dyn Device>,
    pub info: crate::app::DevInfo,
    pub stories: Vec<StoryInfo>,
}

pub fn snapshot(dev: &dyn Device) -> Vec<StoryInfo> {
    dev.stories()
        .iter()
        .map(|s| StoryInfo {
            uuid: s.uuid,
            str_uuid: s.str_uuid(),
            name: s.name(),
            night_mode: s.nm,
            official: s.is_official(),
            hidden: s.hidden,
            exportable: dev.story_exportable(s),
            size: s.size,
            album: dev.album_track_count(s).is_some(),
        })
        .collect()
}

pub enum Event {
    /// Log message (English, from lunii-core)
    Log(Level, String),
    /// Status bar message (already translated), also logged
    Status(String),
    ShowLog,
    Total(usize, usize),
    /// An import starts: expected number of bytes written to the device
    ImportPlan(u64),
    /// Podcast feed loaded (requested address, podcast or error)
    Feed(String, Result<Box<Podcast>, String>),
    Story(String, usize, usize),
    File(String, u64, u64),
    Stories(Vec<StoryInfo>),
    DbRefreshed(bool),
    /// Databases loaded at startup
    DbLoaded(bool),
    FirmwareFiles(AuthToken, Vec<String>),
    UpdateCheck(Option<String>),
    Details(Uuid, String, Option<Vec<u8>>),
    /// Background device search done (search id, devices and their names)
    DevicesFound(u32, Vec<PathBuf>, Vec<String>),
    /// Background device opening done (search id, device or error)
    DeviceOpened(u32, Result<Box<OpenedDevice>, String>),
    Finished,
}

pub type Sink = Arc<dyn Fn(Event) + Send + Sync>;

/// Forwards the lunii-core feedback to the UI.
pub struct GuiReporter(pub Sink);

impl Reporter for GuiReporter {
    fn log(&self, level: Level, message: &str) {
        (self.0)(Event::Log(level, message.to_string()));
    }

    fn story_progress(&self, short_uuid: &str, current: usize, total: usize) {
        (self.0)(Event::Story(short_uuid.to_string(), current, total));
    }

    fn file_progress(&self, label: &str, current: u64, total: u64) {
        (self.0)(Event::File(label.to_string(), current, total));
    }
}

pub enum Task {
    Import {
        files: Vec<PathBuf>,
        update_size: bool,
    },
    Export {
        uuids: Vec<String>,
        out: PathBuf,
    },
    Remove {
        uuids: Vec<String>,
    },
    /// Episodes of a podcast imported as an album (or appended to it)
    ImportPodcast {
        podcast: Box<Podcast>,
        episodes: Vec<Episode>,
        /// one story per episode instead of an album
        as_stories: bool,
        update_size: bool,
    },
    /// MP3 files appended to an album story
    AddToAlbum {
        uuid: Uuid,
        files: Vec<PathBuf>,
        update_size: bool,
    },
    /// New track list of an album (tracks moved, removed, added)
    EditAlbum {
        uuid: Uuid,
        entries: Vec<album::AlbumEntry>,
        update_size: bool,
    },
    Size,
    Recover {
        dry_run: bool,
    },
    Cleanup,
    DbImport(PathBuf),
    RefreshDb,
    Ffmpeg,
    FirmwareLogin {
        email: String,
        password: String,
        version: DeviceVersion,
        snu: String,
    },
    FirmwareDownload {
        auth: AuthToken,
        version: DeviceVersion,
        snu: String,
        targets: Vec<(PathBuf, bool)>,
    },
}

struct Runner {
    ctx: Context,
    sink: Sink,
    device: Option<SharedDevice>,
    audio: AudioOptions,
}

impl Runner {
    fn emit(&self, e: Event) {
        (self.sink)(e);
    }

    fn message(&self, msg: String) {
        self.emit(Event::Status(msg));
    }

    fn refresh(&self) {
        if let Some(dev) = &self.device {
            let dev = dev.lock().unwrap();
            self.emit(Event::Stories(snapshot(dev.as_ref())));
        }
    }

    fn with_device<T>(&self, f: impl FnOnce(&mut dyn Device) -> T) -> Option<T> {
        let dev = self.device.as_ref()?;
        let mut dev = dev.lock().unwrap();
        Some(f(dev.as_mut()))
    }

    /// Like `with_device`, for tasks that can't run without a device.
    fn on_device<T>(
        &self,
        f: impl FnOnce(&mut dyn Device) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        self.with_device(f)
            .unwrap_or_else(|| Err(anyhow::anyhow!("No device selected")))
    }

    fn import(&self, files: &[PathBuf], update_size: bool) -> anyhow::Result<()> {
        let is_flam = self
            .with_device(|d| d.version() == DeviceVersion::FlamV1)
            .unwrap_or(false);
        if !files.is_empty() && is_flam {
            self.emit(Event::ShowLog);
            self.message(tr(
                "😮‍💨 This process is veeeeeeeeery long due to Flam firmware. 😴 Be patient ...",
            ));
            self.message(tr("Importing stories..."));
        }

        // MP3 albums are turned into STUdio archives first: the remaining time
        // estimation needs their real size (screens...)
        let prepared = PreparedFiles(
            files
                .iter()
                .map(|file| {
                    if album::is_album_dir(file) {
                        (file, self.prepare_album(file).map(|zip| (zip, true)))
                    } else {
                        (file, Ok((file.clone(), false)))
                    }
                })
                .collect(),
        );
        self.emit(Event::ImportPlan(
            prepared
                .0
                .iter()
                .filter_map(|(_, r)| r.as_ref().ok())
                .map(|(archive, _)| expected_size(archive))
                .sum(),
        ));

        let mut success = 0;
        for (index, (file, archive)) in prepared.0.iter().enumerate() {
            lunii_core::report::check_abort(&self.ctx)?;
            self.emit(Event::Total(index, files.len()));
            let start = Instant::now();
            let result = match archive {
                Ok((archive, _)) => self.on_device(|d| d.import_story(&self.ctx, archive)),
                Err(e) => Err(anyhow::anyhow!("{e:#}")),
            };
            let name = file.display().to_string();
            match result {
                Ok(()) => {
                    let d = start.elapsed().as_secs();
                    let time = if d > 120 {
                        format!("{} min {} s", d / 60, d % 60)
                    } else {
                        format!("{d} s")
                    };
                    self.message(trf("Time to import : {}", &[&time]));
                    self.message(trf("👍 New story imported : '{}'", &[&name]));
                    success += 1;
                }
                Err(e) if e.is::<Aborted>() => return Err(e),
                Err(e) => {
                    self.emit(Event::Log(Level::Error, format!("{e:#}")));
                    self.emit(Event::ShowLog);
                    self.message(trf("🛑 Failed to import : '{}'", &[&name]));
                }
            }
            self.refresh();
        }
        if update_size {
            self.on_device(|d| d.compute_sizes(&self.ctx))?;
            self.refresh();
        }
        self.message(trf(
            "✅ Import done : {}/{}",
            &[&success.to_string(), &files.len().to_string()],
        ));
        Ok(())
    }

    fn add_to_album(
        &self,
        uuid: &Uuid,
        files: &[PathBuf],
        update_size: bool,
    ) -> anyhow::Result<()> {
        self.message(tr("Adding MP3 files to the album..."));
        self.emit(Event::ImportPlan(append_expected(
            files.iter().map(PathBuf::as_path),
        )));
        self.emit(Event::Total(0, 1));
        let start = Instant::now();
        let tracks = album::scan_files(files)?.tracks;
        album::check_ffmpeg(&tracks)?;
        let work = TempDir(std::env::temp_dir().join(format!("lunii-append-{uuid}")));
        let tracks = album::prepare_tracks(&self.ctx, &tracks, self.audio, &work.0)?;
        let result = self.on_device(|d| d.add_album_tracks(&self.ctx, uuid, &tracks));
        self.refresh();
        let added = result?;
        if update_size {
            self.on_device(|d| d.compute_sizes(&self.ctx))?;
            self.refresh();
        }
        self.message(trf(
            "✅ {} track(s) added to the album in {} s",
            &[&added.to_string(), &start.elapsed().as_secs().to_string()],
        ));
        Ok(())
    }

    fn edit_album(
        &self,
        uuid: &Uuid,
        mut entries: Vec<album::AlbumEntry>,
        update_size: bool,
    ) -> anyhow::Result<()> {
        use album::AlbumEntry;
        self.message(tr("Updating the album..."));
        let new: Vec<album::Track> = entries
            .iter()
            .filter_map(|e| match e {
                AlbumEntry::New(t) => Some(t.clone()),
                AlbumEntry::Existing(_) => None,
            })
            .collect();
        self.emit(Event::ImportPlan(append_expected(
            new.iter().map(|t| t.path.as_path()),
        )));
        self.emit(Event::Total(0, 1));
        let start = Instant::now();
        album::check_ffmpeg(&new)?;
        let work = TempDir(std::env::temp_dir().join(format!("lunii-edit-{uuid}")));
        // the added tracks, normalized if required
        let mut prepared = album::prepare_tracks(&self.ctx, &new, self.audio, &work.0)?.into_iter();
        for entry in &mut entries {
            if let AlbumEntry::New(track) = entry
                && let Some(p) = prepared.next()
            {
                *track = p;
            }
        }
        let result = self.on_device(|d| d.edit_album(&self.ctx, uuid, &entries));
        self.refresh();
        result?;
        if update_size {
            self.on_device(|d| d.compute_sizes(&self.ctx))?;
            self.refresh();
        }
        self.message(trf(
            "✅ Album updated in {} s",
            &[&start.elapsed().as_secs().to_string()],
        ));
        Ok(())
    }

    /// Is the album `uuid` on the device (tracks can be appended)?
    fn album_on_device(&self, uuid: &Uuid) -> bool {
        self.with_device(|d| {
            d.stories()
                .get(&uuid.to_string())
                .cloned()
                .is_some_and(|s| d.album_track_count(&s).is_some())
        })
        .unwrap_or(false)
    }

    fn import_podcast(
        &self,
        podcast: &Podcast,
        episodes: &[Episode],
        as_stories: bool,
        update_size: bool,
    ) -> anyhow::Result<()> {
        let start = Instant::now();
        let dir =
            TempDir(std::env::temp_dir().join(format!("lunii-podcast-{}", podcast.album_uuid())));
        self.message(trf(
            "⬇ Downloading {} episode(s) of '{}'...",
            &[&episodes.len().to_string(), &podcast.title],
        ));
        let mode = ImportMode {
            as_stories,
            album_on_device: self.album_on_device(&podcast.album_uuid()),
            audio: self.audio,
        };
        let plan = podcast::prepare_import(&self.ctx, podcast, episodes, mode, &dir.0, |i| {
            self.emit(Event::Total(i, episodes.len()))
        })?;

        match &plan {
            // regular imports of STUdio archives
            ImportPlan::Stories(archives) => return self.import(archives, update_size),
            ImportPlan::Append(uuid, tracks) => {
                self.emit(Event::Total(0, 1));
                self.emit(Event::ImportPlan(append_expected(
                    tracks.iter().map(|t| t.path.as_path()),
                )));
                let result = self.on_device(|d| d.add_album_tracks(&self.ctx, uuid, tracks));
                self.refresh();
                result?;
            }
            ImportPlan::NewAlbum(zip) => {
                self.emit(Event::Total(0, 1));
                self.emit(Event::ImportPlan(expected_size(zip)));
                let result = self.on_device(|d| d.import_story(&self.ctx, zip));
                self.refresh();
                result?;
            }
        }
        podcast::record_plan(podcast, episodes, &plan)?;
        if update_size {
            self.on_device(|d| d.compute_sizes(&self.ctx))?;
            self.refresh();
        }
        self.message(trf(
            "✅ {} episode(s) of '{}' imported in {}",
            &[
                &episodes.len().to_string(),
                &podcast.title,
                &format_secs(start.elapsed().as_secs()),
            ],
        ));
        Ok(())
    }

    /// A folder of MP3 files, turned into a temporary STUdio archive.
    fn prepare_album(&self, dir: &Path) -> anyhow::Result<PathBuf> {
        let mut album = album::scan(dir)?;
        self.message(trf(
            "🎵 MP3 album '{}' : {} tracks",
            &[&album.title, &album.tracks.len().to_string()],
        ));
        album::check_ffmpeg(&album.tracks)?;
        let work = TempDir(std::env::temp_dir().join(format!("lunii-album-{}", album.uuid())));
        album.tracks = album::prepare_tracks(&self.ctx, &album.tracks, self.audio, &work.0)?;
        let zip = std::env::temp_dir().join(format!("lunii-album-{}.zip", album.uuid()));
        if let Err(e) = album.write_studio_zip(&zip) {
            let _ = std::fs::remove_file(&zip);
            return Err(e);
        }
        Ok(zip)
    }

    fn export(&self, uuids: &[String], out: &std::path::Path) -> anyhow::Result<()> {
        let mut success = 0;
        for (index, uuid) in uuids.iter().enumerate() {
            lunii_core::report::check_abort(&self.ctx)?;
            self.emit(Event::Total(index, uuids.len()));
            let (name, result) = self
                .with_device(|d| {
                    let name = d
                        .find_story(uuid)
                        .map(|s| s.name())
                        .unwrap_or_else(|_| uuid.clone());
                    (name, d.export_story(&self.ctx, uuid, out))
                })
                .unwrap_or_else(|| (uuid.clone(), Err(anyhow::anyhow!("No device selected"))));
            match result {
                Ok(path) => {
                    self.message(trf(
                        "👍 Story exported to '{}'",
                        &[&path.display().to_string()],
                    ));
                    success += 1;
                }
                Err(e) if e.is::<Aborted>() => return Err(e),
                Err(e) => {
                    self.emit(Event::Log(Level::Error, format!("{e:#}")));
                    self.emit(Event::ShowLog);
                    self.message(trf("🛑 Failed to export : '{}'", &[&name]));
                }
            }
            self.refresh();
        }
        self.message(trf(
            "✅ Export done : {}/{}",
            &[&success.to_string(), &uuids.len().to_string()],
        ));
        Ok(())
    }

    fn remove(&self, uuids: &[String]) -> anyhow::Result<()> {
        let mut success = 0;
        for (index, uuid) in uuids.iter().enumerate() {
            lunii_core::report::check_abort(&self.ctx)?;
            self.emit(Event::Total(index, uuids.len()));
            let (name, result) = self
                .with_device(|d| {
                    let name = d
                        .find_story(uuid)
                        .map(|s| s.name())
                        .unwrap_or_else(|_| uuid.clone());
                    (name, d.remove_story(&self.ctx, uuid))
                })
                .unwrap_or_else(|| (uuid.clone(), Err(anyhow::anyhow!("No device selected"))));
            match result {
                Ok(()) => {
                    self.message(trf("👍 Story removed: '{}'", &[&name]));
                    success += 1;
                }
                Err(e) => {
                    self.emit(Event::Log(Level::Error, format!("{e:#}")));
                    self.emit(Event::ShowLog);
                    self.message(trf("🛑 Failed to remove : '{}'", &[&name]));
                }
            }
            self.refresh();
        }
        self.message(trf("✅ {} stories removed", &[&success.to_string()]));
        Ok(())
    }

    fn firmware_download(
        &self,
        auth: &AuthToken,
        version: DeviceVersion,
        snu: &str,
        targets: &[(PathBuf, bool)],
    ) {
        for (index, (target, second)) in targets.iter().enumerate() {
            self.emit(Event::Total(index, targets.len()));
            match firmware::device_fw_download(version, snu, auth, target, *second) {
                Ok(size) if size > 0 => {
                    let name = target
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    self.message(format!("{} {name}", tr("✅ Firmware downloaded to")));
                }
                Ok(_) => self.message(tr("🛑 Fail to download update")),
                Err(e) => {
                    self.emit(Event::Log(Level::Error, format!("{e:#}")));
                    self.message(tr("🛑 Fail to download update"));
                }
            }
        }
    }

    fn run(&self, task: Task) -> anyhow::Result<()> {
        match task {
            Task::Import { files, update_size } => self.import(&files, update_size)?,
            Task::Export { uuids, out } => self.export(&uuids, &out)?,
            Task::Remove { uuids } => self.remove(&uuids)?,
            Task::AddToAlbum {
                uuid,
                files,
                update_size,
            } => self.add_to_album(&uuid, &files, update_size)?,
            Task::EditAlbum {
                uuid,
                entries,
                update_size,
            } => self.edit_album(&uuid, entries, update_size)?,
            Task::ImportPodcast {
                podcast,
                episodes,
                as_stories,
                update_size,
            } => self.import_podcast(&podcast, &episodes, as_stories, update_size)?,
            Task::Size => {
                self.on_device(|d| d.compute_sizes(&self.ctx))?;
                self.refresh();
                self.message(tr("✅ Stories parsed, sizes updated"));
            }
            Task::Recover { dry_run } => {
                self.message(tr("🚧 Analyzing storage ..."));
                let count = self.on_device(|d| d.recover_stories(&self.ctx, dry_run))?;
                self.refresh();
                let what = if dry_run {
                    tr("found (try to recover them)")
                } else {
                    tr("recovered")
                };
                self.message(trf(
                    "✅ Storage parsed, {} lost stories {}",
                    &[&count.to_string(), &what],
                ));
            }
            Task::Cleanup => {
                self.message(tr("🚧 Cleaning storage ..."));
                let (count, size) = self.on_device(|d| d.cleanup_stories(&self.ctx))?;
                self.refresh();
                self.message(trf(
                    "✅ Storage parsed, {} lost stories removed, {} MB recovered",
                    &[&count.to_string(), &size.to_string()],
                ));
            }
            Task::DbImport(path) => match tools::import_studio_db(&self.ctx, &path) {
                Ok((count, total)) => {
                    self.refresh();
                    self.message(trf(
                        "✅ STUdio DB imported ({}/{}).",
                        &[&count.to_string(), &total.to_string()],
                    ));
                }
                Err(e) if e.is::<Aborted>() => return Err(e),
                Err(e) => {
                    self.emit(Event::Log(Level::Error, format!("{e:#}")));
                    self.message(tr("🛑 Failed to import DB (wrong file ?)"));
                }
            },
            Task::RefreshDb => {
                self.emit(Event::Total(0, 2));
                let ok = db::load_db(true);
                self.emit(Event::Total(1, 2));
                self.refresh();
                self.emit(Event::DbRefreshed(ok));
            }
            Task::Ffmpeg => match tools::install_ffmpeg(&self.ctx) {
                Ok(_) => self.message(tr("✅ FFMPEG installed.")),
                Err(e) if e.is::<Aborted>() => return Err(e),
                Err(e) => {
                    self.emit(Event::ShowLog);
                    self.message(crate::i18n::tr_message(&format!("{e:#}")));
                }
            },
            Task::FirmwareLogin {
                email,
                password,
                version,
                snu,
            } => match firmware::luniistore_get_authtoken(&email, &password) {
                Ok(Some(auth)) => {
                    self.message(tr("Login success..."));
                    match firmware::device_fw_getlist(version, &snu, &auth) {
                        Ok(files) => self.emit(Event::FirmwareFiles(auth, files)),
                        Err(e) => {
                            self.emit(Event::Log(Level::Error, format!("{e:#}")));
                            self.message(tr("🛑 Network error..."));
                        }
                    }
                }
                Ok(None) => self.message(tr("⚠️ Login failed, please check your credentials")),
                Err(e) => {
                    self.emit(Event::Log(Level::Error, format!("{e:#}")));
                    self.message(tr("🛑 Network error..."));
                }
            },
            Task::FirmwareDownload {
                auth,
                version,
                snu,
                targets,
            } => self.firmware_download(&auth, version, &snu, &targets),
        }
        Ok(())
    }
}

/// Runs a task in a new thread. `Event::Finished` is always sent at the end.
pub fn spawn(
    task: Task,
    device: Option<SharedDevice>,
    audio: AudioOptions,
    ctx: Context,
    sink: Sink,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let runner = Runner {
            ctx,
            sink,
            device,
            audio,
        };
        if let Err(e) = runner.run(task) {
            if e.is::<Aborted>() {
                runner.refresh();
                runner.message(tr("🛑 Aborted"));
            } else {
                runner.emit(Event::ShowLog);
                runner.message(trf(
                    "🛑 Critical error : {}",
                    &[&crate::i18n::tr_message(&format!("{e:#}"))],
                ));
            }
        }
        runner.emit(Event::Finished);
    })
}

/// Fetches the description and picture of a story (picture may need the network).
pub fn fetch_details(uuid: Uuid, sink: Sink) {
    std::thread::spawn(move || {
        let desc = db::story_description(&uuid);
        let picture = db::story_picture(&uuid, false);
        sink(Event::Details(uuid, desc, picture));
    });
}

/// Loads a podcast feed in the background (`discover`: `url` may be the page of
/// the podcast). Not a task: the device is not involved.
pub fn fetch_feed(url: String, discover: bool, sink: Sink) {
    std::thread::spawn(move || {
        let result = if discover {
            podcast::fetch(&url)
        } else {
            podcast::parse_feed_url(&url)
        };
        let result = result
            .map(Box::new)
            .map_err(|e| crate::i18n::tr_message(&format!("{e:#}")));
        sink(Event::Feed(url, result));
    });
}

/// Checks the latest release on GitHub.
pub fn check_version(sink: Sink) {
    std::thread::spawn(move || sink(Event::UpdateCheck(tools::latest_version())));
}
