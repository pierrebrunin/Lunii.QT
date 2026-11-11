//! Flam storytellers.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use uuid::Uuid;

use super::Device;
use super::common::{self, StoryZip, join_path, remove_suffix, split_path};
use crate::album;
use crate::archive::Archive;
use crate::constants::*;
use crate::convert::image;
use crate::crypto::{self, hex_spaced, hexlify, reverse_bytes};
use crate::report::{Context, check_abort};
use crate::story::{
    self, Story, StoryList, archive_type, story_is_flam, story_is_lunii, story_is_studio,
    uuid_from_entries,
};

pub const STORIES_BASEDIR: &str = "str/";
pub const HIDDEN_STORIES_BASEDIR: &str = "str.hidden/";
const LIB_BASEDIR: &str = "etc/library/";
const LIB_CACHE: &str = "usr/0/library.cache";

pub struct FlamDevice {
    mount_point: PathBuf,
    pub device_version: DeviceVersion,
    pub snu: Vec<u8>,
    pub fw_main: String,
    pub fw_comm: String,
    story_key: Vec<u8>,
    story_iv: Vec<u8>,
    keyfile: Vec<u8>,
    stories: StoryList,
    /// Debug: write plain files on the device
    pub debug_plain: bool,
}

impl FlamDevice {
    /// Opens the Flam mounted at `mount_point` (reads `.mdf` and the library lists).
    pub fn open(ctx: &Context, mount_point: &Path) -> Result<Self> {
        let mdf_path = mount_point.join(".mdf");
        let mdf = std::fs::read(&mdf_path)
            .with_context(|| format!("unable to read {}", mdf_path.display()))?;
        if mdf.len() < 0x6E {
            bail!("Flam metadata file is too short ({} bytes)", mdf.len());
        }

        // firmware versions
        let raw = String::from_utf8_lossy(&mdf[2..50]);
        let raw = raw
            .trim_matches('\0')
            .replace("main: ", "")
            .replace("comm: ", "");
        let versions: Vec<&str> = raw.lines().collect();
        let fw_main = versions
            .first()
            .map(|v| v.split('-').next().unwrap_or("").to_string())
            .unwrap_or_else(|| "?.?.?".into());
        let fw_comm = versions
            .get(1)
            .map(|v| v.split('-').next().unwrap_or("").to_string())
            .unwrap_or_else(|| "?.?.?".into());

        // SNU
        let snu_str = String::from_utf8_lossy(&mdf[50..74])
            .trim_end_matches('\0')
            .to_string();
        let snu = crypto::unhexlify(&snu_str)?;

        let vid = u16::from_le_bytes([mdf[74], mdf[75]]);
        let pid = u16::from_le_bytes([mdf[76], mdf[77]]);

        let hex = hexlify(&snu);
        let (keyfile, story_key, story_iv) = if fw_main.starts_with("1.") {
            // major v1 uses the decipher trick
            let mut key = hex.clone();
            key.extend_from_slice(&[0, 0]);
            let mut iv = vec![0u8; 8];
            iv.extend_from_slice(&hex[..hex.len().min(8)]);
            (mdf[0x4E..0x6E].to_vec(), key, iv)
        } else {
            let mut keyfile = hex.clone();
            keyfile.extend_from_slice(&[0u8; 10]);
            keyfile.extend_from_slice(&hex[..hex.len().min(8)]);
            (keyfile, mdf[0x4E..0x5E].to_vec(), mdf[0x5E..0x6E].to_vec())
        };

        let mut dev = FlamDevice {
            mount_point: mount_point.to_path_buf(),
            device_version: if (vid, pid) == FLAM_USB_VID_PID {
                DeviceVersion::FlamV1
            } else {
                DeviceVersion::Undefined
            },
            snu,
            fw_main,
            fw_comm,
            story_key,
            story_iv,
            keyfile,
            stories: StoryList::new(),
            debug_plain: false,
        };

        // metadata file backup
        let backup = cfg_dir().join(format!("{}.v{}.mdf", dev.snu_str(), dev.fw_main));
        if !backup.is_file() {
            ctx.info(format!(
                "No backup of v{} metadata file found, creating one...",
                dev.fw_main
            ));
            ensure_cfg_dir()?;
            std::fs::write(&backup, &mdf)?;
        }

        ctx.debug(format!(
            "\nSNU : {}\nHW  : v1\nFW (main) : {}\nFW (comm) : {}\nVID/PID : 0x{vid:04X} / 0x{pid:04X}",
            dev.snu_str(),
            dev.fw_main,
            dev.fw_comm
        ));

        dev.stories = feed_stories(ctx, mount_point);
        dev.update_pack_index()?;
        Ok(dev)
    }

    pub fn snu_str(&self) -> String {
        crypto::hex_string(&self.snu)
            .to_uppercase()
            .trim_start_matches('0')
            .to_string()
    }

    fn cipher(&self, data: &[u8], key: &[u8], iv: &[u8], len: usize) -> Result<Vec<u8>> {
        if self.debug_plain {
            return Ok(data.to_vec());
        }
        crypto::aes_cipher(data, key, iv, len)
    }

    fn ciphered_data(
        &self,
        file: &str,
        data: &[u8],
        flam_story: bool,
        force: bool,
    ) -> Result<Vec<u8>> {
        let (key, iv, len) = if !flam_story {
            // Lunii story on Flam
            if file.ends_with("ni") || file.ends_with("nm") {
                return Ok(data.to_vec());
            }
            (
                reverse_bytes(&self.story_key),
                reverse_bytes(&self.story_iv),
                0x200,
            )
        } else {
            if !(file.ends_with(".lua") || file.ends_with(".plain") || force) {
                return Ok(data.to_vec());
            }
            (
                Some(self.story_key.clone()),
                Some(self.story_iv.clone()),
                data.len(),
            )
        };
        match (key, iv) {
            (Some(key), Some(iv)) => self.cipher(data, &key, &iv, len),
            _ => Ok(data.to_vec()),
        }
    }

    fn flam_ciphered_name(file: &str) -> String {
        let file = remove_suffix(file, ".plain");
        if file.ends_with(".lua") {
            file.replace(".lua", ".lsf")
        } else {
            file.to_string()
        }
    }

    pub fn lunii_ciphered_name(file: &str, studio_ri: bool, studio_si: bool) -> String {
        let mut file = remove_suffix(file, ".plain").to_string();
        if studio_ri {
            file = format!("rf/000/{file}");
        }
        if studio_si {
            file = format!("sf/000/{file}");
        }
        let lower = file.to_lowercase();
        let lower = remove_suffix(&lower, ".mp3").to_lowercase();
        let file = remove_suffix(&lower, ".bmp").to_string();

        let (dir, base) = split_path(&file);
        if base.chars().count() >= 8 {
            return join_path(dir, &base.to_uppercase());
        }
        file
    }

    fn write_keyfile(&self, ctx: &Context, story_dir: &Path) -> Result<()> {
        ctx.info("Authorization file creation...");
        common::write_file(&story_dir.join("key"), &self.keyfile)
    }

    fn write_info(&self, story: &Story, story_dir: &Path) -> Result<()> {
        let name = deunicode::deunicode(&story.name());
        let author = deunicode::deunicode(&story.author());
        common::write_file(
            &story_dir.join("info"),
            format!("{name}\n{name}\n0\n{name}\n{author}").as_bytes(),
        )
    }

    fn write_thumbnail(&self, ctx: &Context, story: &Story, story_dir: &Path) -> Result<()> {
        let Some(picture) = story.picture(false) else {
            ctx.warn("No picture available for this story, thumbnail skipped");
            return Ok(());
        };
        let liff = image::image_to_liff(&picture)?;
        common::write_file(&story_dir.join("img").join("thumbnail.lif"), &liff)
    }

    fn finish_import(&mut self, story: Story) -> Result<()> {
        self.stories.push(story);
        self.update_pack_index()
    }

    fn import_lunii_plain(&mut self, ctx: &Context, path: &Path) -> Result<()> {
        let archive = Archive::open_zip(path)?;
        let contents = archive.names();
        if !archive.contains(FILE_UUID) {
            bail!("No UUID file found in archive. Unable to add this story.");
        }
        let new_uuid = Uuid::from_slice(&archive.read(FILE_UUID)?)?;
        self.check_not_loaded(&new_uuid)?;
        common::import_plain_metadata(&archive, &new_uuid)?;

        let short = story::short_uuid(&new_uuid);
        let output = self.content_dir().join(new_uuid.to_string());
        std::fs::create_dir_all(&output)?;

        for (index, file) in contents.iter().enumerate() {
            ctx.story_progress(&short, index, contents.len());
            if ctx.is_aborted() {
                return Err(self.abort_cleanup(ctx, &new_uuid));
            }
            if [FILE_UUID, FILE_META, FILE_THUMB].contains(&file.as_str())
                || file.ends_with("bt")
                || archive.is_dir(file)
            {
                continue;
            }
            let data = self.ciphered_data(file, &archive.read(file)?, false, false)?;
            let new_name = Self::lunii_ciphered_name(file, false, false);
            ctx.debug(format!(
                "File {}/{} > {new_name}",
                index + 1,
                contents.len()
            ));
            common::write_with_progress(ctx, &output.join(&new_name), &data)?;
        }

        self.write_keyfile(ctx, &output)?;
        let story = Story::new(new_uuid);
        self.write_info(&story, &output)?;
        self.write_thumbnail(ctx, &story, &output)?;
        self.finish_import(story)
    }

    fn import_flam_plain(&mut self, ctx: &Context, path: &Path) -> Result<()> {
        let archive = Archive::open_zip(path)?;
        let contents = archive.names();
        if !archive.contains(FILE_UUID) {
            bail!("No UUID file found in archive. Unable to add this story.");
        }
        let new_uuid = Uuid::from_slice(&archive.read(FILE_UUID)?)?;
        self.check_not_loaded(&new_uuid)?;

        let short = story::short_uuid(&new_uuid);
        let output = self.content_dir().join(new_uuid.to_string());
        std::fs::create_dir_all(&output)?;

        for (index, file) in contents.iter().enumerate() {
            ctx.story_progress(&short, index, contents.len());
            if ctx.is_aborted() {
                return Err(self.abort_cleanup(ctx, &new_uuid));
            }
            if [FILE_UUID, FILE_META, FILE_THUMB].contains(&file.as_str()) || archive.is_dir(file) {
                continue;
            }
            let data = self.ciphered_data(file, &archive.read(file)?, true, false)?;
            let new_name = Self::flam_ciphered_name(file);
            ctx.debug(format!(
                "File {}/{} > {new_name}",
                index + 1,
                contents.len()
            ));
            common::write_with_progress(ctx, &output.join(&new_name), &data)?;
        }

        self.write_keyfile(ctx, &output)?;
        self.finish_import(Story::new(new_uuid))
    }

    fn import_flam_backup(&mut self, ctx: &Context, path: &Path) -> Result<()> {
        let archive = Archive::open(path)?;
        let contents = archive.names();
        if story_is_lunii(&contents) || story_is_studio(&contents) {
            bail!("Archive seems to be made of Lunii story (Flam story expected)");
        }

        // story keys for transciphering ?
        let story_keys = match contents.iter().find(|e| e.ends_with("bt")) {
            Some(bt) => {
                ctx.info("Transciphering Flam story");
                let data = archive.read(bt)?;
                Some((
                    data[..data.len().min(16)].to_vec(),
                    data.get(16..).unwrap_or_default().to_vec(),
                ))
            }
            None => {
                if !contents.iter().any(|e| e.ends_with("key")) {
                    bail!("Flam story backup is incomplete, missing key file.");
                }
                ctx.info("Restoring Flam story backup");
                None
            }
        };

        let (_, new_uuid) = uuid_from_entries(&contents)?;
        if let Some(s) = self.stories.get(&new_uuid.to_string()) {
            bail!("'{}' is already loaded, aborting !", s.name());
        }

        let short = story::short_uuid(&new_uuid);
        let output = self.content_dir();
        std::fs::create_dir_all(&output)?;
        if crate::archive::is_7z(path) {
            ctx.info("Reading 7zip archive... (takes time)");
        }

        let files = archive.file_names();
        for (index, file) in files.iter().enumerate() {
            if ctx.is_aborted() {
                return Err(self.abort_cleanup(ctx, &new_uuid));
            }
            ctx.story_progress(&short, index, files.len());
            ctx.debug(format!("File {}/{} > {file}", index + 1, files.len()));
            let mut data = archive.read(file)?;

            if let Some((key, iv)) = &story_keys
                && (file.ends_with(".lsf") || file.ends_with("info"))
            {
                ctx.debug(format!("Transciphering file {file}"));
                let plain = crypto::aes_decipher(&data, key, iv, data.len())?;
                data = self.ciphered_data(file, &plain, true, true)?;
            }
            common::write_with_progress(ctx, &output.join(file), &data)?;
        }

        if story_keys.is_some() {
            self.write_keyfile(ctx, &output.join(new_uuid.to_string()))?;
        }
        self.finish_import(Story::new(new_uuid))
    }

    fn import_lunii_flam_zip(&mut self, ctx: &Context, path: &Path) -> Result<()> {
        let filename = path
            .file_name()
            .map(|f| f.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !filename.starts_with(&self.snu_str().to_lowercase()) {
            bail!(
                "Lunii story from Flam personnal backup can't be imported on this device (SNU mismatch)."
            );
        }
        ctx.info("Restoring Lunii story from Flam personnal backup...");

        let archive = Archive::open_zip(path)?;
        let names = archive.names();
        let (_, new_uuid) = uuid_from_entries(&names)?;
        self.check_not_loaded(&new_uuid)?;

        let output = self.content_dir();
        std::fs::create_dir_all(&output)?;
        let short = story::short_uuid(&new_uuid);

        let files = archive.file_names();
        let mut night_mode = false;
        for (index, file) in files.iter().enumerate() {
            ctx.story_progress(&short, index, files.len());
            if ctx.is_aborted() {
                return Err(self.abort_cleanup(ctx, &new_uuid));
            }
            if file.ends_with("nm") {
                night_mode = true;
            }
            ctx.debug(format!("File {}/{} > {file}", index + 1, files.len()));
            common::write_with_progress(ctx, &output.join(file), &archive.read(file)?)?;
        }
        self.finish_import(Story::with_flags(new_uuid, false, night_mode))
    }

    fn import_lunii_v2(&mut self, ctx: &Context, path: &Path) -> Result<()> {
        let archive = Archive::open(path)?;
        let names = archive.names();
        if archive.contains(FILE_STUDIO_JSON) {
            bail!("Corrupted file format. Unable to add this story.");
        }
        let (uuid_str, new_uuid) = uuid_from_entries(&names)?;
        self.check_not_loaded(&new_uuid)?;

        let long_uuid = new_uuid.to_string();
        let short = story::short_uuid(&new_uuid);
        let output = self.content_dir();
        std::fs::create_dir_all(&output)?;

        let files = archive.file_names();
        let mut night_mode = false;
        for (index, file) in files.iter().enumerate() {
            ctx.story_progress(&short, index, files.len());
            if ctx.is_aborted() {
                return Err(self.abort_cleanup(ctx, &new_uuid));
            }
            if file == FILE_UUID || file.ends_with("bt") {
                continue;
            }
            if file.ends_with("nm") {
                night_mode = true;
            }

            let data_v2 = archive.read(file)?;
            let plain = if file.ends_with("ni") || file.ends_with("nm") {
                data_v2
            } else {
                crypto::xxtea_decipher(&data_v2, &lunii_generic_key(), 512)?
            };
            let data = self.ciphered_data(file, &plain, false, false)?;
            let new_name =
                Self::lunii_ciphered_name(&file.replace(&uuid_str, &long_uuid), false, false);
            ctx.debug(format!("File {}/{} > {new_name}", index + 1, files.len()));
            common::write_with_progress(ctx, &output.join(&new_name), &data)?;
        }

        let story_dir = output.join(&long_uuid);
        self.write_keyfile(ctx, &story_dir)?;
        let story = Story::with_flags(new_uuid, false, night_mode);
        self.write_info(&story, &story_dir)?;
        self.write_thumbnail(ctx, &story, &story_dir)?;
        self.finish_import(story)
    }

    fn export_backup_story(&self, ctx: &Context, story: &Story, out_dir: &Path) -> Result<PathBuf> {
        let story_path = self.story_path(story);
        let short = story.short_uuid();
        ctx.info(format!("🚧 Exporting {short} - {}", story.name()));

        let zip_path = out_dir.join(format!(
            "{}.{}.{short}.zip",
            self.snu_str(),
            common::secure_filename(&story.name())
        ));
        let files = common::walk_files(&story_path);
        let mut zip = StoryZip::create(&zip_path)?;
        ctx.debug("> Zipping story ...");
        for (index, (abs, rel)) in files.iter().enumerate() {
            check_abort(ctx)?;
            ctx.story_progress(&short, index, files.len());
            let arcname = format!("{}/{rel}", story.uuid);
            ctx.debug(&arcname);
            zip.add(&arcname, &std::fs::read(abs)?)?;
        }
        zip.finish()
    }

    fn export_flam_plain(
        &self,
        ctx: &Context,
        story: &Story,
        out_dir: &Path,
        key: &[u8],
        iv: &[u8],
    ) -> Result<PathBuf> {
        let story_path = self.story_path(story);
        let short = story.short_uuid();
        ctx.info(format!("🚧 Exporting {} - {}", story.uuid, story.name()));

        let zip_path = out_dir.join(format!(
            "{}.{short}.plain.pk",
            common::secure_filename(&story.name())
        ));
        let files: Vec<_> = common::walk_files(&story_path)
            .into_iter()
            .filter(|(abs, _)| {
                !matches!(
                    abs.file_name().and_then(|f| f.to_str()),
                    Some("key") | Some("bt") | Some(album::meta::FILE_ALBUM)
                )
            })
            .collect();

        let mut zip = StoryZip::create(&zip_path)?;
        ctx.debug("> Zipping story ...");
        for (index, (abs, rel)) in files.iter().enumerate() {
            ctx.story_progress(&short, index, files.len());
            check_abort(ctx)?;

            let mut target = rel.clone();
            if target.ends_with(".lsf") {
                target = target.replace(".lsf", ".lua");
            }
            if target.ends_with("info") {
                target += ".plain";
            }

            let mut data = std::fs::read(abs)?;
            if rel.ends_with(".lsf") || rel.ends_with("info") {
                data = crypto::aes_decipher(&data, key, iv, data.len())?;
            }
            zip.add(&target, &data)?;
        }
        ctx.debug("> Adding UUID ...");
        zip.add(FILE_UUID, story.uuid.as_bytes())?;
        zip.finish()
    }

    fn export_lunii_plain(
        &self,
        ctx: &Context,
        story: &Story,
        out_dir: &Path,
        key: &[u8],
        iv: &[u8],
    ) -> Result<PathBuf> {
        let story_path = self.story_path(story);
        let short = story.short_uuid();
        ctx.info(format!("🚧 Exporting {} - {}", story.uuid, story.name()));

        let zip_path = out_dir.join(format!(
            "{}.{short}.plain.pk",
            common::secure_filename(&story.name())
        ));
        let files: Vec<_> = common::walk_files(&story_path)
            .into_iter()
            .filter(|(abs, rel)| {
                let (dir, _) = split_path(rel);
                !dir.contains("img")
                    && !matches!(
                        abs.file_name().and_then(|f| f.to_str()),
                        Some("key") | Some("bt") | Some("info") | Some(album::meta::FILE_ALBUM)
                    )
            })
            .collect();

        let mut zip = StoryZip::create(&zip_path)?;
        ctx.debug("> Zipping story ...");
        for (index, (abs, rel)) in files.iter().enumerate() {
            ctx.story_progress(&short, index, files.len());
            check_abort(ctx)?;

            let mut target = rel.clone();
            if target.contains("rf/") {
                target += ".bmp";
            }
            if target.contains("sf/") {
                target += ".mp3";
            }
            if target.ends_with("li") || target.ends_with("ri") || target.ends_with("si") {
                target += ".plain";
            }

            let mut data = std::fs::read(abs)?;
            if target.ends_with(".mp3") || target.ends_with(".bmp") || target.ends_with(".plain") {
                data = crypto::aes_decipher(&data, key, iv, 0x200)?;
            }
            zip.add(&target, &data)?;
        }

        ctx.debug("> Adding UUID ...");
        zip.add(FILE_UUID, story.uuid.as_bytes())?;
        if !story.is_official() {
            ctx.debug("> Adding thumbnail ...");
            if let Some(pict) = story.picture(false) {
                zip.add(FILE_THUMB, &pict)?;
            }
            ctx.debug("> Adding metadata ...");
            if let Some(meta) = story.meta() {
                zip.add(FILE_META, meta.as_bytes())?;
            }
        }
        zip.finish()
    }
}

impl Device for FlamDevice {
    fn mount_point(&self) -> &Path {
        &self.mount_point
    }

    fn version(&self) -> DeviceVersion {
        self.device_version
    }

    fn snu_str(&self) -> String {
        FlamDevice::snu_str(self)
    }

    fn info(&self) -> String {
        format!(
            "Flam device on \"{}\"\n- Main firmware : v{}\n- Comm firmware : v{}\n- SNU      : {}\n- stories  : {}x",
            self.mount_point.display(),
            self.fw_main,
            self.fw_comm,
            hex_spaced(&self.snu),
            self.stories.len()
        )
    }

    fn stories(&self) -> &StoryList {
        &self.stories
    }

    fn stories_mut(&mut self) -> &mut StoryList {
        &mut self.stories
    }

    fn stories_basedir(&self) -> &'static str {
        STORIES_BASEDIR
    }

    fn hidden_stories_basedir(&self) -> &'static str {
        HIDDEN_STORIES_BASEDIR
    }

    fn story_dir_name(&self, uuid: &Uuid) -> String {
        uuid.to_string()
    }

    fn update_pack_index(&self) -> Result<()> {
        let lib_path = self.mount_point.join(LIB_BASEDIR);
        let list_path = lib_path.join("list");
        let list_hidden_path = lib_path.join("list.hidden");
        let _ = std::fs::remove_file(&list_path);
        let _ = std::fs::remove_file(&list_hidden_path);
        let _ = std::fs::remove_file(self.mount_point.join(LIB_CACHE));
        std::fs::create_dir_all(&lib_path)?;

        let mut list = String::new();
        let mut hidden = String::new();
        for story in self.stories.iter() {
            let target = if story.hidden { &mut hidden } else { &mut list };
            target.push_str(&story.uuid.to_string());
            target.push('\n');
        }
        std::fs::write(list_path, list)?;
        std::fs::write(list_hidden_path, hidden)?;
        Ok(())
    }

    fn import_story(&mut self, ctx: &Context, path: &Path) -> Result<()> {
        ctx.info(format!("🚧 Loading {}...", path.display()));

        let archive_size = std::fs::metadata(path)?.len();
        let free = common::free_space(&self.mount_point)?;
        if archive_size >= free {
            bail!(
                "Not enough space left on Flam (only {}MB)",
                free / 1024 / 1024
            );
        }

        let kind = archive_type(path, false)?;
        use ArchiveType::*;
        if self.fw_main.starts_with("1.")
            && matches!(
                kind,
                LuniiPlain | LuniiV2Zip | LuniiV27z | StudioZip | StudioSevenZ
            )
        {
            bail!("Please update your Flam with v2.x.x to support Lunii Stories");
        }

        ctx.debug(format!("Archive => {kind:?}"));
        match kind {
            LuniiPlain => self.import_lunii_plain(ctx, path),
            FlamPlain => self.import_flam_plain(ctx, path),
            FlamZip | FlamSevenZ => self.import_flam_backup(ctx, path),
            LuniiV2Zip | LuniiV27z => self.import_lunii_v2(ctx, path),
            LuniiFlamZip => self.import_lunii_flam_zip(ctx, path),
            StudioZip | StudioSevenZ => common::import_studio(self, ctx, path),
            other => bail!("Archive => Unsupported type 0x{:02X}", other as u8),
        }
    }

    fn export_story(&mut self, ctx: &Context, uuid_part: &str, out_dir: &Path) -> Result<PathBuf> {
        let story = self.find_story(uuid_part)?;
        let story_path = self.story_path(&story);
        if !story_path.is_dir() {
            bail!("Story directory not found ({})", story_path.display());
        }

        // official stories are never deciphered (piracy): backup only,
        // to put them back on a device
        if story.is_official() {
            return self.export_backup_story(ctx, &story, out_dir);
        }

        let story_files: Vec<String> = common::walk_files(&story_path)
            .into_iter()
            .map(|(_, rel)| rel)
            .collect();

        // known keys ?
        let mut keys = None;
        if let Ok(key_data) = std::fs::read(story_path.join("key"))
            && key_data == self.keyfile
        {
            keys = Some((self.story_key.clone(), self.story_iv.clone()));
        }
        if keys.is_none()
            && let Ok(bt) = std::fs::read(story_path.join("bt"))
        {
            keys = Some((
                bt[..bt.len().min(16)].to_vec(),
                bt.get(16..32).unwrap_or_default().to_vec(),
            ));
        }

        let Some((key, iv)) = keys else {
            return self.export_backup_story(ctx, &story, out_dir);
        };
        if story_is_flam(&story_files) {
            self.export_flam_plain(ctx, &story, out_dir, &key, &iv)
        } else if story_is_lunii(&story_files) {
            let key = reverse_bytes(&key).context("invalid story key")?;
            let iv = reverse_bytes(&iv).context("invalid story IV")?;
            self.export_lunii_plain(ctx, &story, out_dir, &key, &iv)
        } else {
            bail!("This story format is not supported for export.")
        }
    }

    fn supported_extensions(&self) -> &'static [&'static str] {
        FLAM_SUPPORTED_EXT
    }

    fn resolve_dir_uuid(&self, dir_name: &str) -> Option<String> {
        super::uuid_from_db(dir_name).or_else(|| Some(dir_name.to_string()))
    }

    fn edit_album(
        &mut self,
        ctx: &Context,
        uuid: &Uuid,
        entries: &[album::AlbumEntry],
    ) -> Result<()> {
        common::edit_album(self, ctx, uuid, entries)
    }

    fn validate_story_dir(&mut self, _ctx: &Context, _story_dir: &Path) -> bool {
        true
    }
}

fn feed_list(ctx: &Context, path: &Path, hidden: bool, list: &mut StoryList) -> usize {
    let Ok(content) = std::fs::read_to_string(path) else {
        return 0;
    };
    let mut count = 0;
    for line in content.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Ok(uuid) = Uuid::parse_str(line) else {
            ctx.warn(format!("Invalid UUID in library list: {line}"));
            continue;
        };
        ctx.debug(format!("> {uuid}"));
        if list.contains_uuid(&uuid) {
            ctx.warn("Found duplicate story, cleaning...");
            continue;
        }
        list.push(Story::with_flags(uuid, hidden, false));
        count += 1;
    }
    count
}

/// Reads the library lists (installed stories).
pub fn feed_stories(ctx: &Context, root: &Path) -> StoryList {
    ctx.info("Reading Flam loaded stories...");
    let lib = root.join(LIB_BASEDIR);
    let mut list = StoryList::new();
    let count = feed_list(ctx, &lib.join("list"), false, &mut list);
    ctx.info(format!("Read {count} stories"));
    let hidden = feed_list(ctx, &lib.join("list.hidden"), true, &mut list);
    ctx.info(format!("Read {hidden} hidden stories"));
    list
}

impl common::StoryWriter for FlamDevice {
    fn cipher_story_file(&self, file: &str, data: &[u8]) -> Result<Vec<u8>> {
        self.ciphered_data(file, data, false, false)
    }

    fn seal_story(
        &mut self,
        ctx: &Context,
        story_dir: &Path,
        story: &Story,
        _ri: &[u8],
        new_story: bool,
    ) -> Result<()> {
        if new_story {
            self.write_keyfile(ctx, story_dir)?;
            self.write_info(story, story_dir)?;
            self.write_thumbnail(ctx, story, story_dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(FlamDevice::flam_ciphered_name("main.lua.plain"), "main.lsf");
        assert_eq!(FlamDevice::flam_ciphered_name("info.plain"), "info");
        assert_eq!(
            FlamDevice::lunii_ciphered_name("rf/000/0a1b2c3d.bmp", false, false),
            "rf/000/0A1B2C3D"
        );
        assert_eq!(
            FlamDevice::lunii_ciphered_name(
                "0123abcd-0000-0000-0000-000000000000/ri",
                false,
                false
            ),
            "0123abcd-0000-0000-0000-000000000000/ri"
        );
    }
}
