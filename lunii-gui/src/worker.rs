//! Background tasks.
//!
//! A task runs on the thread of the opened device (see `device_actor`), or
//! in a thread of its own when it does not need one, and talks to the UI with
//! [`Event`]s, delivered in order on the Slint event loop.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use lunii_core::constants::DeviceVersion;
use lunii_core::convert::audio::AudioOptions;
use lunii_core::firmware::{self, AuthToken};
use lunii_core::podcast::{self, Episode, Podcast, SearchResult};
use lunii_core::{Aborted, Context, Device, Level, Reporter, album, db, report, tools};
use uuid::Uuid;

use crate::device_actor::DeviceHandle;
use crate::i18n::{tr, trf};
use crate::tickets::Ticket;

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

/// Removes a temporary directory on drop (abort included).
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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
    /// seconds, from the database (the others are measured on the device)
    pub duration: Option<u32>,
    /// MP3 album created by Lunii, tracks can be appended
    pub album: bool,
    /// album of a podcast: its episodes are added and removed from the
    /// Podcasts view
    pub podcast: bool,
    /// track titles of an album (albums of sub-folders included), read with
    /// the stories: the UI does not read the device
    pub album_info: Option<album::meta::AlbumInfo>,
    /// folder of the story on the device (durations measured there)
    pub dir: PathBuf,
}

/// A device opened in the background, with what the UI needs (read there too).
pub struct OpenedDevice {
    pub dev: Box<dyn Device>,
    pub info: crate::app::DevInfo,
    pub stories: Vec<StoryInfo>,
}

/// Why a device could not be opened.
pub enum OpenError {
    /// neither a Lunii nor a Flam (no `.md` / `.mdf`): removed from the list
    NotRecognized(PathBuf),
    Failed(String),
}

/// The stories of the device, with all the UI displays of them (reads the
/// device: on its thread).
pub fn snapshot(dev: &dyn Device) -> Vec<StoryInfo> {
    dev.stories()
        .iter()
        .map(|s| {
            let album_info = dev.album_info(s);
            StoryInfo {
                uuid: s.uuid,
                str_uuid: s.str_uuid(),
                name: s.name(),
                night_mode: s.nm,
                official: s.is_official(),
                hidden: s.hidden,
                exportable: dev.story_exportable(s),
                size: s.size,
                duration: db::story_duration(&s.uuid),
                album: dev.album_track_count(s).is_some(),
                podcast: album_info.as_ref().is_some_and(|i| i.podcast.is_some()),
                album_info,
                dir: dev.story_path(s),
            }
        })
        .collect()
}

/// A quick change of the device (on its thread), and the stories after it:
/// their order and flags from the device, the rest from `known` (read again
/// only when a story is not in it: no album read for a move).
pub fn quick_change(
    dev: &mut dyn Device,
    ctx: &Context,
    known: &HashSet<Uuid>,
    op: impl FnOnce(&mut dyn Device, &Context) -> anyhow::Result<()>,
) -> QuickResult {
    let result = op(dev, ctx).map_err(|e| format!("{e:#}"));
    let order: Vec<(Uuid, bool, bool, PathBuf)> = dev
        .stories()
        .iter()
        .map(|s| (s.uuid, s.hidden, s.nm, dev.story_path(s)))
        .collect();
    let stories = order
        .iter()
        .any(|(u, ..)| !known.contains(u))
        .then(|| snapshot(dev));
    QuickResult {
        result,
        order,
        stories,
        details: dev.info(),
    }
}

/// Result of [`quick_change`].
pub struct QuickResult {
    pub result: Result<(), String>,
    /// the stories in their order: UUID, hidden, night mode, folder
    pub order: Vec<(Uuid, bool, bool, PathBuf)>,
    /// all the stories read again (a story was not known)
    pub stories: Option<Vec<StoryInfo>>,
    /// `Device::info()` after the change
    pub details: String,
}

pub enum Event {
    /// Log message (English, from lunii-core)
    Log(Level, String),
    /// Status bar message (already translated), also logged
    Status(String),
    ShowLog,
    Total(usize, usize),
    /// Episode of a podcast being imported (index, number of episodes)
    Episode(usize, usize),
    /// Bytes an import is expected to write to the device (again: the total
    /// known so far, updated)
    ImportPlan(u64),
    /// Podcast feed loaded (requested address, podcast or error)
    Feed(String, Result<Box<Podcast>, String>),
    /// "Add a podcast" dialog: podcasts found
    PodcastSearch(Ticket, Result<Vec<SearchResult>, String>),
    /// "Add a podcast" dialog: feed checked
    PodcastChecked(Ticket, Result<Box<Podcast>, String>),
    /// "Add a podcast" dialog: cover of the result at this index (ticket of
    /// the search, or of the check of an address)
    PodcastArtwork(Ticket, usize, Vec<u8>),
    Story(String, usize, usize),
    File(String, u64, u64),
    /// The stories of the device opened with this ticket, read again, and
    /// its `Device::info()`
    Stories(Ticket, Vec<StoryInfo>, String),
    /// A quick change of the device opened with this ticket is done
    QuickChanged(Ticket, Box<QuickResult>, crate::app::QuickChange),
    /// Night mode settings written (the new ones, or the error)
    NightModeSaved(Ticket, Result<Vec<u16>, String>),
    DbRefreshed(bool),
    /// Databases loaded at startup
    DbLoaded(bool),
    FirmwareFiles(AuthToken, Vec<String>),
    UpdateCheck(Option<String>),
    Details(Uuid, String, Option<Vec<u8>>),
    /// Background device search done (devices and their names)
    DevicesFound(Ticket, Vec<PathBuf>, Vec<String>),
    /// Devices plugged in now, checked in the background (ticket current at
    /// the time, devices and their names)
    DevicesWatched(Ticket, Vec<PathBuf>, Vec<String>),
    /// Background device opening done (device or error)
    DeviceOpened(Ticket, Result<Box<OpenedDevice>, OpenError>),
    Finished,
    /// Duration measured on the device (story, seconds; `None`: not
    /// measurable)
    Duration(Ticket, Uuid, Option<u32>),
    /// The durations asked are all measured
    DurationsDone,
    /// Space of the device opened with this ticket (`None`: not measurable)
    Space(Ticket, Option<crate::app::Space>),
}

impl Event {
    /// Its name, for the log (see `watchdog`).
    pub fn name(&self) -> &'static str {
        match self {
            Event::Log(..) => "Log",
            Event::Status(..) => "Status",
            Event::ShowLog => "ShowLog",
            Event::Total(..) => "Total",
            Event::Episode(..) => "Episode",
            Event::ImportPlan(..) => "ImportPlan",
            Event::Feed(..) => "Feed",
            Event::PodcastSearch(..) => "PodcastSearch",
            Event::PodcastChecked(..) => "PodcastChecked",
            Event::PodcastArtwork(..) => "PodcastArtwork",
            Event::Story(..) => "Story",
            Event::File(..) => "File",
            Event::Stories(..) => "Stories",
            Event::QuickChanged(..) => "QuickChanged",
            Event::NightModeSaved(..) => "NightModeSaved",
            Event::DbRefreshed(..) => "DbRefreshed",
            Event::DbLoaded(..) => "DbLoaded",
            Event::FirmwareFiles(..) => "FirmwareFiles",
            Event::UpdateCheck(..) => "UpdateCheck",
            Event::Details(..) => "Details",
            Event::DevicesFound(..) => "DevicesFound",
            Event::DevicesWatched(..) => "DevicesWatched",
            Event::DeviceOpened(..) => "DeviceOpened",
            Event::Finished => "Finished",
            Event::Duration(..) => "Duration",
            Event::DurationsDone => "DurationsDone",
            Event::Space(..) => "Space",
        }
    }
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
    /// Episodes of a podcast removed from the device, then the others
    /// imported as an album (or appended to it)
    ImportPodcast {
        podcast: Box<Podcast>,
        episodes: Vec<Episode>,
        /// episodes on the device to remove first
        remove: Vec<Episode>,
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

struct Runner<'d> {
    ctx: Context,
    sink: Sink,
    /// the device, when the task runs on its thread
    device: RefCell<Option<&'d mut dyn Device>>,
    /// the ticket of the device opened (its stories sent with it)
    ticket: Option<Ticket>,
    audio: AudioOptions,
}

impl Runner<'_> {
    fn emit(&self, e: Event) {
        (self.sink)(e);
    }

    fn message(&self, msg: String) {
        self.emit(Event::Status(msg));
    }

    fn refresh(&self) {
        let snapshot = self.with_device(|d| (snapshot(d), d.info()));
        if let (Some((stories, details)), Some(ticket)) = (snapshot, self.ticket) {
            self.emit(Event::Stories(ticket, stories, details));
        }
    }

    fn with_device<T>(&self, f: impl FnOnce(&mut dyn Device) -> T) -> Option<T> {
        let mut dev = self.device.borrow_mut();
        Some(f(dev.as_deref_mut()?))
    }

    /// Like `with_device`, for tasks that can't run without a device.
    fn on_device<T>(
        &self,
        f: impl FnOnce(&mut dyn Device) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        self.with_device(f)
            .unwrap_or_else(|| Err(anyhow::anyhow!("No device selected")))
    }

    /// Computes the missing story sizes, then lists the stories again.
    fn update_sizes(&self) -> anyhow::Result<()> {
        self.on_device(|d| d.compute_sizes(&self.ctx))?;
        self.refresh();
        Ok(())
    }

    /// Name of a story of the device (`uuid` itself when it is not found).
    fn story_name(&self, uuid: &str) -> String {
        self.with_device(|d| d.find_story(uuid).map(|s| s.name()).ok())
            .flatten()
            .unwrap_or_else(|| uuid.to_string())
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
        let prepared: Vec<(&PathBuf, anyhow::Result<Option<album::PreparedAlbum>>)> = files
            .iter()
            .map(|file| {
                let album = album::is_album_dir(file)
                    .then(|| self.prepare_album(file))
                    .transpose();
                (file, album)
            })
            .collect();
        let archive = |file: &PathBuf, album: &Option<album::PreparedAlbum>| {
            album
                .as_ref()
                .map_or(file.clone(), |a| a.path().to_path_buf())
        };
        self.emit(Event::ImportPlan(
            prepared
                .iter()
                .filter_map(|(file, r)| r.as_ref().ok().map(|a| expected_size(&archive(file, a))))
                .sum(),
        ));

        let success = report::batch(
            &self.ctx,
            &prepared,
            |index, (file, album)| {
                self.emit(Event::Total(index, files.len()));
                let start = Instant::now();
                let result = match album {
                    Ok(album) => {
                        let archive = archive(file, album);
                        self.on_device(|d| d.import_story(&self.ctx, &archive))
                    }
                    Err(e) => Err(anyhow::anyhow!("{e:#}")),
                };
                if result.is_ok() {
                    let time = lunii_core::format::secs(start.elapsed().as_secs());
                    self.message(trf("Time to import : {}", &[&time]));
                    self.message(trf(
                        "👍 New story imported : '{}'",
                        &[&file.display().to_string()],
                    ));
                }
                self.refresh();
                result
            },
            |(file, _)| {
                self.emit(Event::ShowLog);
                self.message(trf(
                    "🛑 Failed to import : '{}'",
                    &[&file.display().to_string()],
                ));
            },
        )?;
        if update_size {
            self.update_sizes()?;
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
        let work = TempDir(std::env::temp_dir().join(format!("lunii-append-{uuid}")));
        let tracks = album::prepare_tracks(&self.ctx, &tracks, self.audio, &work.0)?;
        let result = self.on_device(|d| d.add_album_tracks(&self.ctx, uuid, &tracks));
        self.refresh();
        let added = result?;
        if update_size {
            self.update_sizes()?;
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
            self.update_sizes()?;
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

    /// Episodes of a podcast removed from the device: their stories, or their
    /// tracks of the podcast album (the whole album if none is left).
    fn remove_episodes(
        &self,
        podcast: &Podcast,
        episodes: &[Episode],
        as_stories: bool,
    ) -> anyhow::Result<()> {
        if as_stories {
            let uuids: Vec<String> = episodes
                .iter()
                .map(|e| podcast.episode_uuid(e).to_string())
                .collect();
            return self.remove(&uuids);
        }
        let uuid = podcast.album_uuid();
        let guids: HashSet<&str> = episodes.iter().map(|e| e.guid.as_str()).collect();
        // tracks without their episode (albums imported before it was
        // recorded): found by title
        let titles: HashSet<&str> = episodes.iter().map(|e| e.title.as_str()).collect();
        // tracks kept, as positions on the device
        let (count, kept) = self
            .with_device(|d| {
                let story = d.stories().get(&uuid.to_string())?.clone();
                let count = d.album_track_count(&story)?;
                let info = d.album_info(&story)?;
                Some((
                    count,
                    (0..count)
                        .filter(|&i| {
                            let guid = info.episodes.get(i).map_or("", String::as_str);
                            let removed = if guid.is_empty() {
                                info.tracks
                                    .get(i)
                                    .is_some_and(|t| titles.contains(t.as_str()))
                            } else {
                                guids.contains(guid)
                            };
                            !removed
                        })
                        .collect::<Vec<usize>>(),
                ))
            })
            .flatten()
            .ok_or_else(|| anyhow::anyhow!("This story is not an MP3 album created by Lunii"))?;
        if kept.len() == count {
            anyhow::bail!("Episodes to remove not found in the album");
        }
        if kept.is_empty() {
            return self.remove(&[uuid.to_string()]);
        }
        let entries = kept.into_iter().map(album::AlbumEntry::Existing).collect();
        self.edit_album(&uuid, entries, false)
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
        // one episode after the other: downloaded, converted, written. An
        // interrupted import leaves the episodes already written, recorded.
        let uuid = podcast.album_uuid();
        let mut in_album = !as_stories && self.album_on_device(&uuid);
        let cover = if as_stories || !in_album {
            podcast::download_cover(podcast)
        } else {
            None
        };
        // bytes to write: the sizes announced by the feed, then the real ones
        let mut expected: Vec<u64> = episodes.iter().map(|e| e.size.unwrap_or(0)).collect();
        let total = episodes.len();
        let success = report::batch(
            &self.ctx,
            episodes,
            |i, ep| {
                self.emit(Event::Episode(i, total));
                self.ctx
                    .info(format!("⬇ {} ({}/{})", ep.title, i + 1, total));
                let track = podcast::download_episode(
                    &self.ctx,
                    ep,
                    &format!("{}/{total}", i + 1),
                    &dir.0,
                    &format!("episode{i:04}"),
                    self.audio,
                )?;
                let zip = dir.0.join(podcast::story_zip_name(i, &ep.title));
                let result = if as_stories {
                    podcast::write_episode_story(podcast, ep, &track, cover.as_deref(), &zip)?;
                    expected[i] = expected_size(&zip);
                    self.emit(Event::ImportPlan(expected.iter().sum()));
                    self.on_device(|d| d.import_story(&self.ctx, &zip))
                } else if in_album {
                    expected[i] = append_expected(std::iter::once(track.path.as_path()));
                    self.emit(Event::ImportPlan(expected.iter().sum()));
                    self.on_device(|d| {
                        d.add_album_tracks(&self.ctx, &uuid, std::slice::from_ref(&track))
                            .map(|_| ())
                    })
                } else {
                    // the album, made of its first episode
                    podcast::write_new_album(podcast, vec![track.clone()], cover.clone(), &zip)?;
                    expected[i] = expected_size(&zip);
                    self.emit(Event::ImportPlan(expected.iter().sum()));
                    self.on_device(|d| d.import_story(&self.ctx, &zip))
                };
                let _ = std::fs::remove_file(&zip);
                let _ = std::fs::remove_file(&track.path);
                self.refresh();
                result?;
                if !as_stories {
                    self.on_device(|d| {
                        podcast::record_imported(
                            d.mount_point(),
                            podcast,
                            std::slice::from_ref(&ep.guid),
                            !in_album,
                        )
                    })?;
                    in_album = true;
                    // and on the device: seen from any computer
                    if let Some(Err(e)) = self.with_device(|d| {
                        podcast::record_on_device(d, podcast, std::slice::from_ref(ep))
                    }) {
                        self.ctx.warn(format!("{e:#}"));
                    }
                }
                Ok(())
            },
            |ep| {
                self.emit(Event::ShowLog);
                self.message(trf("🛑 Failed to import : '{}'", &[&ep.title]));
            },
        )?;
        if update_size {
            self.update_sizes()?;
        }
        self.message(trf(
            "✅ {} episode(s) of '{}' imported in {}",
            &[
                &success.to_string(),
                &podcast.title,
                &lunii_core::format::secs(start.elapsed().as_secs()),
            ],
        ));
        Ok(())
    }

    /// A folder of MP3 files, turned into a temporary STUdio archive.
    fn prepare_album(&self, dir: &Path) -> anyhow::Result<album::PreparedAlbum> {
        let album = album::prepare_album(&self.ctx, dir, self.audio)?;
        self.message(trf(
            "🎵 MP3 album '{}' : {} tracks",
            &[&album.title, &album.tracks.to_string()],
        ));
        Ok(album)
    }

    fn export(&self, uuids: &[String], out: &std::path::Path) -> anyhow::Result<()> {
        let success = report::batch(
            &self.ctx,
            uuids,
            |index, uuid| {
                self.emit(Event::Total(index, uuids.len()));
                let result = self.on_device(|d| d.export_story(&self.ctx, uuid, out));
                if let Ok(path) = &result {
                    self.message(trf(
                        "👍 Story exported to '{}'",
                        &[&path.display().to_string()],
                    ));
                }
                self.refresh();
                result.map(|_| ())
            },
            |uuid| self.failed(uuid, |name| trf("🛑 Failed to export : '{}'", &[name])),
        )?;
        self.message(trf(
            "✅ Export done : {}/{}",
            &[&success.to_string(), &uuids.len().to_string()],
        ));
        Ok(())
    }

    /// After the failure on a story: the log, and the message made with its
    /// name.
    fn failed(&self, uuid: &str, message: impl FnOnce(&str) -> String) {
        self.emit(Event::ShowLog);
        self.message(message(&self.story_name(uuid)));
    }

    fn remove(&self, uuids: &[String]) -> anyhow::Result<()> {
        let success = report::batch(
            &self.ctx,
            uuids,
            |index, uuid| {
                self.emit(Event::Total(index, uuids.len()));
                let name = self.story_name(uuid);
                let result = self.on_device(|d| d.remove_story(&self.ctx, uuid));
                if result.is_ok() {
                    self.message(trf("👍 Story removed: '{}'", &[&name]));
                }
                self.refresh();
                result
            },
            |uuid| self.failed(uuid, |name| trf("🛑 Failed to remove : '{}'", &[name])),
        )?;
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
                    self.ctx.error(format!("{e:#}"));
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
                remove,
                as_stories,
                update_size,
            } => {
                if !remove.is_empty() {
                    self.remove_episodes(&podcast, &remove, as_stories)?;
                }
                if !episodes.is_empty() {
                    self.import_podcast(&podcast, &episodes, as_stories, update_size)?;
                } else if update_size {
                    self.update_sizes()?;
                }
            }
            Task::Size => {
                self.update_sizes()?;
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
                    self.ctx.error(format!("{e:#}"));
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
                            self.ctx.error(format!("{e:#}"));
                            self.message(tr("🛑 Network error..."));
                        }
                    }
                }
                Ok(None) => self.message(tr("⚠️ Login failed, please check your credentials")),
                Err(e) => {
                    self.ctx.error(format!("{e:#}"));
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

/// Runs a task on the thread of `device` (opened with this ticket), or
/// without one in a new thread (its handle returned). `Event::Finished` is
/// always sent at the end.
pub fn spawn(
    task: Task,
    device: Option<(DeviceHandle, Ticket)>,
    audio: AudioOptions,
    ctx: Context,
    sink: Sink,
) -> Option<std::thread::JoinHandle<()>> {
    let run = move |device: Option<&mut dyn Device>, ticket: Option<Ticket>, sink: Sink| {
        let runner = Runner {
            ctx,
            sink,
            device: RefCell::new(device),
            ticket,
            audio,
        };
        if let Err(e) = runner.run(task) {
            if e.is::<Aborted>() {
                runner.refresh();
                runner.message(tr("🛑 Aborted"));
            } else {
                runner.emit(Event::ShowLog);
                runner.message(trf("🛑 Critical error : {}", &[&tr_error(e)]));
            }
        }
        runner.emit(Event::Finished);
    };
    match device {
        Some((handle, ticket)) => {
            let fallback = sink.clone();
            if !handle.run(move |dev| run(Some(dev), Some(ticket), sink)) {
                // the device thread is gone: the task ends at once
                fallback(Event::Status(tr(
                    "🛑 The storyteller is no longer available",
                )));
                fallback(Event::Finished);
            }
            None
        }
        None => Some(std::thread::spawn(move || run(None, None, sink))),
    }
}

/// An error of lunii-core, translated for the user.
fn tr_error(e: anyhow::Error) -> String {
    crate::i18n::tr_message(&format!("{e:#}"))
}

/// Measures the duration of stories from their audio files (not a task: only
/// reads a little of each file).
pub fn measure_durations(ticket: Ticket, stories: Vec<(Uuid, PathBuf)>, sink: Sink) {
    std::thread::spawn(move || {
        for (uuid, dir) in stories {
            let duration = lunii_core::mp3::story_duration(&dir);
            sink(Event::Duration(ticket, uuid, duration));
        }
        sink(Event::DurationsDone);
    });
}

/// Fetches the description and picture of a story (picture may need the network).
pub fn fetch_details(uuid: Uuid, sink: Sink) {
    std::thread::spawn(move || {
        // official descriptions are HTML
        let desc = db::plain_text(&db::story_description(&uuid));
        let picture = db::story_picture(&uuid, false);
        sink(Event::Details(uuid, desc, picture));
    });
}

/// Loads the feed of a followed podcast in the background. Not a task: the
/// device is not involved.
pub fn fetch_feed(url: String, sink: Sink) {
    std::thread::spawn(move || {
        let result = podcast::parse_feed_url(&url)
            .map(Box::new)
            .map_err(tr_error);
        sink(Event::Feed(url, result));
    });
}

/// Searches podcasts by name (Apple Podcasts; `kids_only`: Kids & Family
/// genre), then downloads their covers.
pub fn search_podcasts(ticket: Ticket, term: String, country: String, kids_only: bool, sink: Sink) {
    std::thread::spawn(move || {
        let results = podcast::search(&term, &country, kids_only).map_err(tr_error);
        let artworks: Vec<(usize, String)> = results
            .iter()
            .flatten()
            .enumerate()
            .filter_map(|(i, r)| r.artwork_url.clone().map(|u| (i, u)))
            .collect();
        sink(Event::PodcastSearch(ticket, results));
        // a few at a time
        for chunk in artworks.chunks(8) {
            std::thread::scope(|scope| {
                for (index, url) in chunk {
                    let sink = &sink;
                    scope.spawn(move || {
                        if let Some(data) = podcast::download_artwork(url) {
                            sink(Event::PodcastArtwork(ticket, *index, data));
                        }
                    });
                }
            });
        }
    });
}

/// Loads a feed to follow (`address`: typed by the user, may be a page or an
/// Apple Podcasts link; its cover is then downloaded as result 0). `root`:
/// the device whose followed podcasts it may already be.
pub fn check_feed(ticket: Ticket, url: String, address: bool, root: Option<PathBuf>, sink: Sink) {
    std::thread::spawn(move || {
        let mut result = if address {
            podcast::fetch(&url)
        } else {
            podcast::parse_feed_url(&url)
        };
        // already followed under another address: that one
        if let (Ok(p), Some(root)) = (result.as_mut(), root.as_deref()) {
            podcast::match_followed(root, p);
        }
        let image = result
            .as_ref()
            .ok()
            .filter(|_| address)
            .and_then(|p| p.image_url.clone());
        let result = result.map(Box::new).map_err(tr_error);
        sink(Event::PodcastChecked(ticket, result));
        if let Some(data) = image.and_then(|u| podcast::download_artwork(&u)) {
            sink(Event::PodcastArtwork(ticket, 0, data));
        }
    });
}

/// Checks the latest release on GitHub.
pub fn check_version(sink: Sink) {
    std::thread::spawn(move || sink(Event::UpdateCheck(tools::latest_version())));
}
