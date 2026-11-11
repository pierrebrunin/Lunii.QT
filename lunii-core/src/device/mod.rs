//! Storyteller devices (Lunii v1/v2/v3 and Flam).

pub mod common;
pub mod flam;
pub mod lunii;

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use uuid::Uuid;

use crate::constants::DeviceVersion;
use crate::db;
use crate::report::{Context, check_abort};
use crate::story::{Story, StoryList, move_stories, move_stories_before};

pub use flam::FlamDevice;
pub use lunii::LuniiDevice;

/// Behaviour shared by every storyteller.
pub trait Device: Send {
    fn mount_point(&self) -> &Path;
    fn version(&self) -> DeviceVersion;
    fn snu_str(&self) -> String;
    /// Multi-line human readable description.
    fn info(&self) -> String;

    fn stories(&self) -> &StoryList;
    fn stories_mut(&mut self) -> &mut StoryList;

    fn stories_basedir(&self) -> &'static str;
    fn hidden_stories_basedir(&self) -> &'static str;
    /// Directory name of a story (short UUID on Lunii, full UUID on Flam).
    fn story_dir_name(&self, uuid: &Uuid) -> String;

    /// Rewrites the stories index files on the device.
    fn update_pack_index(&self) -> Result<()>;

    fn import_story(&mut self, ctx: &Context, path: &Path) -> Result<()>;
    /// Exports the story matching `uuid_part`, returns the created archive.
    fn export_story(&mut self, ctx: &Context, uuid_part: &str, out_dir: &Path) -> Result<PathBuf>;

    /// Is story import possible (v3 needs story keys).
    fn can_import(&self) -> bool {
        true
    }
    /// Can stories be exported (as plain or backup).
    fn can_export(&self) -> bool {
        true
    }
    /// Can a story be exported (official stories are forbidden on Lunii v1/v2).
    fn story_exportable(&self, _story: &Story) -> bool {
        true
    }

    fn supported_extensions(&self) -> &'static [&'static str];

    /// Full UUID string for a directory found on the device (recovery).
    fn resolve_dir_uuid(&self, dir_name: &str) -> Option<String>;
    /// Checks a lost story directory before recovery.
    fn validate_story_dir(&mut self, ctx: &Context, story_dir: &Path) -> bool;

    fn as_lunii(&self) -> Option<&LuniiDevice> {
        None
    }

    /// Number of tracks if `story` is an MP3 album created by Lunii that
    /// can be edited: tracks added, removed, moved (see [`crate::album`]).
    fn album_track_count(&self, story: &Story) -> Option<usize> {
        crate::album::device_track_count(&self.story_path(story))
    }

    /// Track titles if `story` is an MP3 album created by Lunii (also the
    /// albums made of sub-folders, which can't be edited).
    fn album_info(&self, story: &Story) -> Option<crate::album::meta::AlbumInfo> {
        crate::album::meta::on_device(&self.story_path(story))
    }

    /// Rewrites the tracks of an album story: `entries` is the new track list
    /// (tracks on the device, MP3 files to add), the missing tracks are
    /// removed.
    fn edit_album(
        &mut self,
        _ctx: &Context,
        _uuid: &Uuid,
        _entries: &[crate::album::AlbumEntry],
    ) -> Result<()> {
        bail!("Editing an album is not available on this storyteller")
    }

    /// Appends tracks to an album story, returns the number of added tracks.
    fn add_album_tracks(
        &mut self,
        ctx: &Context,
        uuid: &Uuid,
        tracks: &[crate::album::Track],
    ) -> Result<usize> {
        use crate::album::AlbumEntry;
        let story = self.find_story(&uuid.to_string())?;
        let count = self
            .album_track_count(&story)
            .context("This story is not an MP3 album created by Lunii")?;
        if tracks.is_empty() {
            bail!("No MP3 file to add");
        }
        let entries: Vec<AlbumEntry> = (0..count)
            .map(AlbumEntry::Existing)
            .chain(tracks.iter().cloned().map(AlbumEntry::New))
            .collect();
        self.edit_album(ctx, uuid, &entries)?;
        Ok(tracks.len())
    }
    fn as_lunii_mut(&mut self) -> Option<&mut LuniiDevice> {
        None
    }

    // -----------------------------------------------------------------------
    // Provided methods
    // -----------------------------------------------------------------------

    fn content_dir(&self) -> PathBuf {
        self.mount_point().join(self.stories_basedir())
    }

    fn hidden_content_dir(&self) -> PathBuf {
        self.mount_point().join(self.hidden_stories_basedir())
    }

    /// Location of a story on the device (taking care of hidden state).
    fn story_path(&self, story: &Story) -> PathBuf {
        let base = if story.hidden {
            self.hidden_content_dir()
        } else {
            self.content_dir()
        };
        base.join(self.story_dir_name(&story.uuid))
    }

    /// Single story matching `uuid_part`, error if none or ambiguous.
    /// Error if the story `uuid` is already on the device.
    fn check_not_loaded(&self, uuid: &Uuid) -> Result<()> {
        if let Some(s) = self.stories().get(&uuid.to_string()) {
            bail!("'{}' is already loaded !", s.name());
        }
        Ok(())
    }

    /// Removes the story being imported after an abort, returns the error.
    fn abort_cleanup(&self, ctx: &Context, uuid: &Uuid) -> anyhow::Error {
        ctx.warn("Import aborted, performing cleanup on current story...");
        if let Err(e) = self.clean_up_story_dir(uuid) {
            ctx.error(e.to_string());
        }
        crate::report::Aborted.into()
    }

    fn find_story(&self, uuid_part: &str) -> Result<Story> {
        let list = self.stories().matching(uuid_part);
        match list.len() {
            0 => bail!("This story is not present on your storyteller"),
            1 => Ok(list[0].clone()),
            n => {
                let names: Vec<String> = list
                    .iter()
                    .map(|s| format!("[{} - {}]", s.str_uuid(), s.name()))
                    .collect();
                bail!(
                    "at least {n} match your pattern. Try a longer UUID.\n{}",
                    names.join("\n")
                )
            }
        }
    }

    fn remove_story(&mut self, ctx: &Context, uuid_part: &str) -> Result<()> {
        let story = self.find_story(uuid_part)?;
        let short = story.short_uuid();
        ctx.info(format!("🚧 Removing {short} - {}...", story.name()));
        ctx.story_progress(&short, 0, 3);

        let dir_name = self.story_dir_name(&story.uuid);
        common::remove_dirs(&[
            self.content_dir().join(&dir_name),
            self.hidden_content_dir().join(&dir_name),
        ])?;
        ctx.story_progress(&short, 1, 3);

        self.stories_mut().remove_uuid(&story.uuid);
        self.update_pack_index()?;
        ctx.story_progress(&short, 2, 3);
        Ok(())
    }

    /// Removes the directories of a partially imported story.
    fn clean_up_story_dir(&self, uuid: &Uuid) -> Result<()> {
        let dir_name = self.story_dir_name(uuid);
        common::remove_dirs(&[
            self.content_dir().join(&dir_name),
            self.hidden_content_dir().join(&dir_name),
        ])
    }

    /// Story directories found on the device, as (name, hidden), sorted by name.
    fn story_dirs_on_device(&self) -> Option<Vec<(String, bool)>> {
        let active = common::list_subdirs(&self.content_dir())?;
        let hidden = common::list_subdirs(&self.hidden_content_dir()).unwrap_or_default();
        let mut all: Vec<(String, bool)> = active
            .into_iter()
            .map(|d| (d, false))
            .chain(hidden.into_iter().map(|d| (d, true)))
            .collect();
        all.sort();
        Some(all)
    }

    /// Looks for story directories missing from the index. Returns the number found.
    fn recover_stories(&mut self, ctx: &Context, dry_run: bool) -> Result<usize> {
        let mut recovered = 0;
        let Some(dirs) = self.story_dirs_on_device() else {
            return Ok(0);
        };

        for (index, (dir_name, hidden)) in dirs.iter().enumerate() {
            ctx.story_progress(dir_name, index, dirs.len());
            check_abort(ctx)?;

            let str_uuid = self.resolve_dir_uuid(dir_name);
            let Some(full_uuid) = str_uuid.as_deref().and_then(|s| Uuid::parse_str(s).ok()) else {
                ctx.debug(format!(
                    "Not a valid UUID - {}",
                    str_uuid.unwrap_or_else(|| dir_name.clone())
                ));
                continue;
            };

            let base = if *hidden {
                self.hidden_content_dir()
            } else {
                self.content_dir()
            };
            let story_dir = base.join(dir_name);
            let nm = self.version() != DeviceVersion::FlamV1 && story_dir.join("nm").is_file();
            let story = Story::with_flags(full_uuid, *hidden, nm);
            let label = format!("{} - {}", story.str_uuid(), story.name());

            if !self.stories().contains_uuid(&full_uuid) {
                if self.validate_story_dir(ctx, &story_dir) {
                    if !dry_run {
                        ctx.info(format!("Recovered - {label}"));
                        self.stories_mut().push(story);
                    } else {
                        ctx.info(format!("Found - {label}"));
                    }
                    recovered += 1;
                } else {
                    ctx.info(format!(
                        "Skipping lost story (seems broken/incomplete) - {label}"
                    ));
                }
            } else if !self.validate_story_dir(ctx, &story_dir) {
                ctx.warn(format!("Already in list but invalid - {label}"));
            } else {
                ctx.debug(format!("Already in list - {label}"));
            }
        }

        if !dry_run {
            self.update_pack_index()?;
        }
        Ok(recovered)
    }

    /// Removes the story directories missing from the index.
    /// Returns (removed stories, recovered MB).
    fn cleanup_stories(&mut self, ctx: &Context) -> Result<(usize, u64)> {
        let mut removed = 0;
        let mut recovered_size = 0;
        let dirs = self.story_dirs_on_device().unwrap_or_default();

        for (index, (dir_name, hidden)) in dirs.iter().enumerate() {
            ctx.story_progress(dir_name, index, dirs.len());
            check_abort(ctx)?;

            if self.stories().contains_key(dir_name) {
                continue;
            }
            let base = if *hidden {
                self.hidden_content_dir()
            } else {
                self.content_dir()
            };
            let lost = base.join(dir_name);
            let size = common::dir_size(&lost);
            ctx.info(format!("Deleting - {}", lost.display()));
            match std::fs::remove_dir_all(&lost) {
                Ok(()) => {
                    recovered_size += size;
                    removed += 1;
                }
                Err(e) => {
                    ctx.warn(format!("Failed to delete - {}", lost.display()));
                    ctx.error(e.to_string());
                }
            }
        }
        Ok((removed, crate::format::megabytes(recovered_size)))
    }

    /// Computes the size of the stories not computed yet.
    fn compute_sizes(&mut self, ctx: &Context) -> Result<()> {
        let total = self.stories().len();
        for index in 0..total {
            ctx.story_progress("", index, total);
            check_abort(ctx)?;
            let story = self.stories()[index].clone();
            if story.size.is_some() {
                continue;
            }
            let size = common::dir_size(&self.story_path(&story));
            self.stories_mut()[index].size = Some(size);
        }
        Ok(())
    }

    /// Toggles the hidden state of stories (moving their directory).
    fn toggle_hidden(&mut self, ctx: &Context, uuids: &[Uuid]) -> Result<()> {
        for uuid in uuids {
            let Some(story) = self.stories().iter().find(|s| s.uuid == *uuid).cloned() else {
                bail!("This story is not present on your storyteller");
            };
            let src = self.story_path(&story);
            let dst_parent = if story.hidden {
                self.content_dir()
            } else {
                self.hidden_content_dir()
            };
            if !src.is_dir() {
                ctx.info("💡 Try to use menu 'Device/Lost stories' (Recover or Remove) to fix it");
                bail!(
                    "Story dir {} does not exist or already {}",
                    src.display(),
                    if story.hidden { "visible" } else { "hidden" }
                );
            }
            if let Err(e) = common::move_dir_into(&src, &dst_parent) {
                ctx.info("💡 Try to use menu 'Device/Lost stories' (Recover or Remove) to fix it");
                bail!("Error occurred while moving story directories: {e}");
            }
            if let Some(s) = self.stories_mut().iter_mut().find(|s| s.uuid == *uuid) {
                s.hidden = !s.hidden;
            }
        }
        self.update_pack_index()
    }

    /// Moves stories in the list (`offset` ±1: one step, ±2: top/bottom) and saves the index.
    fn move_stories(&mut self, uuids: &[Uuid], offset: i32) -> Result<()> {
        move_stories(self.stories_mut(), uuids, offset);
        self.update_pack_index()
    }

    /// Moves stories just before `before` (at the end: `None`) and saves the index.
    fn move_stories_before(&mut self, uuids: &[Uuid], before: Option<Uuid>) -> Result<()> {
        move_stories_before(self.stories_mut(), uuids, before);
        self.update_pack_index()
    }
}

/// Is there a Lunii at this path (`.md` file present)?
pub fn is_lunii(root: &Path) -> bool {
    root.join(".md").is_file()
}

/// Is there a Flam at this path (`.mdf` file present)?
pub fn is_flam(root: &Path) -> bool {
    root.join(".mdf").is_file()
}

/// Model of the storyteller at `root`, from its `.md` / `.mdf` file only
/// (quick, without opening it).
pub fn device_model(root: &Path) -> DeviceVersion {
    if is_flam(root) {
        DeviceVersion::FlamV1
    } else if let Ok(md) = std::fs::read(root.join(".md")) {
        lunii::md_model(&md)
    } else {
        DeviceVersion::Undefined
    }
}

/// Opens the storyteller mounted at `path`.
pub fn open_device(ctx: &Context, path: &Path) -> Result<Box<dyn Device>> {
    if is_lunii(path) {
        Ok(Box::new(LuniiDevice::open(ctx, path)?))
    } else if is_flam(path) {
        Ok(Box::new(FlamDevice::open(ctx, path)?))
    } else {
        bail!("No Lunii nor Flam found at {}", path.display())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn is_fat(fstype: &str) -> bool {
    fstype.starts_with("msdos") || fstype == "vfat"
}

#[cfg(target_os = "linux")]
fn candidate_mount_points() -> Vec<PathBuf> {
    // /proc/mounts escapes spaces and special chars as octal (\040)
    fn unescape(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\'
                && i + 4 <= bytes.len()
                && bytes[i + 1..i + 4]
                    .iter()
                    .all(|b| (b'0'..=b'7').contains(b))
            {
                let v = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 4]).unwrap(), 8)
                    .unwrap_or(b'?');
                out.push(v);
                i += 4;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8_lossy(&out).to_string()
    }

    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else {
        return Vec::new();
    };
    mounts
        .lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let (device, mount, fstype) = (it.next()?, it.next()?, it.next()?);
            (device.starts_with("/dev/sd") && is_fat(fstype))
                .then(|| PathBuf::from(unescape(mount)))
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn candidate_mount_points() -> Vec<PathBuf> {
    let Ok(disks) = sysinfo::Disks::new_with_refreshed_list() else {
        return Vec::new();
    };
    disks
        .iter()
        .filter(|d| {
            let mount = d.mount_point().to_string_lossy().to_lowercase();
            let fs = d.file_system().to_string_lossy().to_lowercase();
            ["/mnt", "/media", "/volume"]
                .iter()
                .any(|p| mount.starts_with(p))
                && is_fat(&fs)
        })
        .map(|d| d.mount_point().to_path_buf())
        .collect()
}

#[cfg(windows)]
fn candidate_mount_points() -> Vec<PathBuf> {
    (b'A'..=b'Z')
        .map(|l| PathBuf::from(format!("{}:/", l as char)))
        .collect()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn candidate_mount_points() -> Vec<PathBuf> {
    Vec::new()
}

/// Lists the mount points holding a Lunii or a Flam.
pub fn find_devices(extra_path: Option<&Path>) -> Vec<PathBuf> {
    let mut devices: Vec<PathBuf> = candidate_mount_points()
        .into_iter()
        .filter(|p| is_lunii(p) || is_flam(p))
        .collect();
    if let Some(extra) = extra_path
        && (is_lunii(extra) || is_flam(extra))
        && !devices.iter().any(|d| d == extra)
    {
        devices.push(extra.to_path_buf());
    }
    devices
}

/// Full UUID resolution from the story databases.
pub(crate) fn uuid_from_db(dir_name: &str) -> Option<String> {
    db::find_full_uuid(dir_name)
}
