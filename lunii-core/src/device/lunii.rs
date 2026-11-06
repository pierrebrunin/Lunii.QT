//! Lunii storytellers v1 / v2 / v3.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use uuid::Uuid;

use super::Device;
use super::common::{self, StoryZip, join_path, remove_suffix, skip_chars, split_path};
use crate::archive::Archive;
use crate::constants::*;
use crate::crypto::{self, hex_spaced, hexlify, reverse_bytes};
use crate::report::{Context, check_abort};
use crate::story::{self, Story, StoryList, archive_type, uuid_from_archive};

pub const STORIES_BASEDIR: &str = ".content/";
pub const HIDDEN_STORIES_BASEDIR: &str = ".content.hidden/";

const DEFAULT_CONFIG: [u16; 10] = [
    300, // Idle time before sleep (in s)
    60,  // TBD
    5,   // Time to display Low-Battery message (in s)
    0,   // Night mode (bool)
    0,   // Night mode - volume level
    3,   // Night mode - stories to play
    0,   // Night mode - TBD (volume related?)
    1,   // TBD
    1,   // Night mode - recreate nm files
    1,   // TBD
];

pub struct LuniiDevice {
    mount_point: PathBuf,
    pub device_version: DeviceVersion,
    pub snu: Vec<u8>,
    pub fw_major: i32,
    pub fw_minor: i32,
    pub fw_subminor: i32,
    pub device_key: Option<Vec<u8>>,
    pub device_iv: Option<Vec<u8>>,
    pub story_key: Option<Vec<u8>>,
    pub story_iv: Option<Vec<u8>>,
    bt: Vec<u8>,
    /// Device settings (`.cfg`), see `LUNII_CFGPOS_*`
    pub config: Vec<u16>,
    stories: StoryList,
    /// Debug: write plain files on the device
    pub debug_plain: bool,
}

fn u16_le(data: &[u8], pos: usize) -> u16 {
    data.get(pos..pos + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .unwrap_or(0)
}

fn opt_hex(v: &Option<Vec<u8>>) -> String {
    v.as_ref()
        .map(|k| hex_spaced(k))
        .unwrap_or_else(|| "N/A".into())
}

/// Reads the 14 chars hexadecimal SNU of v3 metadata files.
fn md_v3_snu(md: &[u8]) -> Result<Vec<u8>> {
    let raw = md.get(0x1A..0x1A + 14).context("metadata file too short")?;
    crypto::unhexlify(std::str::from_utf8(raw)?)
}

/// Model of a Lunii from its `.md` file contents.
pub(crate) fn md_model(md: &[u8]) -> DeviceVersion {
    let md_version = u16_le(md, 0);
    if (1..=5).contains(&md_version) && md.len() == 512 {
        match (u16_le(md, 18), u16_le(md, 20)) {
            p if p == FAH_V1_USB_VID_PID || p == FAH_V1_FW_2_USB_VID_PID => DeviceVersion::LuniiV1,
            p if p == FAH_V2_V3_USB_VID_PID => DeviceVersion::LuniiV2,
            _ => DeviceVersion::LuniiV1or2Unknown,
        }
    } else if (6..=7).contains(&md_version) && [112, 128].contains(&md.len()) {
        DeviceVersion::LuniiV3
    } else {
        DeviceVersion::Undefined
    }
}

impl LuniiDevice {
    /// Opens the Lunii mounted at `mount_point` (reads `.md`, `.cfg` and `.pi`).
    pub fn open(ctx: &Context, mount_point: &Path) -> Result<Self> {
        let md_path = mount_point.join(".md");
        let md = std::fs::read(&md_path)
            .with_context(|| format!("unable to read {}", md_path.display()))?;

        let mut dev = LuniiDevice {
            mount_point: mount_point.to_path_buf(),
            device_version: DeviceVersion::LuniiV1or2Unknown,
            snu: Vec::new(),
            fw_major: 0,
            fw_minor: 0,
            fw_subminor: 0,
            device_key: None,
            device_iv: None,
            story_key: None,
            story_iv: None,
            bt: Vec::new(),
            config: DEFAULT_CONFIG.to_vec(),
            stories: StoryList::new(),
            debug_plain: false,
        };

        let md_version = u16_le(&md, 0);
        if (1..=5).contains(&md_version) && md.len() == 512 {
            dev.md1to5_parse(ctx, &md)?;
        } else if (6..=7).contains(&md_version) && [112, 128].contains(&md.len()) {
            dev.md6to7_parse(ctx, &md)?;
        } else {
            dev.unsupported_md_parse(ctx, &md)?;
        }

        dev.config = feed_config(ctx, mount_point);
        dev.stories = feed_stories(ctx, mount_point);
        dev.update_pack_index()?;
        Ok(dev)
    }

    pub fn snu_str(&self) -> String {
        if self.snu == [0u8; 16] {
            return "empty".into();
        }
        crypto::hex_string(&self.snu)
            .to_uppercase()
            .trim_start_matches('0')
            .to_string()
    }

    pub fn firmware(&self) -> String {
        if self.device_version.is_lunii_v1_v2() {
            format!("{}.{}", self.fw_major, self.fw_minor)
        } else {
            format!("{}.{}.{}", self.fw_major, self.fw_minor, self.fw_subminor)
        }
    }

    fn md1to5_parse(&mut self, ctx: &Context, md: &[u8]) -> Result<()> {
        self.fw_major = u16_le(md, 6) as i32;
        self.fw_minor = u16_le(md, 8) as i32;
        self.snu = md[10..18].to_vec();
        let vid = u16_le(md, 18);
        let pid = u16_le(md, 20);
        self.device_version = md_model(md);

        let dec = crypto::xxtea_decrypt(&md[0x100..0x200], &lunii_generic_key())?;
        // reordering key components
        let mut key = dec[8..16].to_vec();
        key.extend_from_slice(&dec[0..8]);
        self.device_key = Some(key);

        ctx.debug(format!(
            "\nSNU : {}\nHW  : v{}\nFW  : v{}.{}\nVID/PID : 0x{vid:04X} / 0x{pid:04X}\nDev Key : {}",
            self.snu_str(),
            self.device_version as u8,
            self.fw_major,
            self.fw_minor,
            opt_hex(&self.device_key)
        ));
        Ok(())
    }

    fn forged_bt(&self) -> Vec<u8> {
        let hex = hexlify(&self.snu);
        let mut bt = hex.clone();
        bt.extend_from_slice(&[0u8; 10]);
        bt.extend_from_slice(&hex[..hex.len().min(8)]);
        bt
    }

    fn md6to7_parse(&mut self, ctx: &Context, md: &[u8]) -> Result<()> {
        self.device_version = DeviceVersion::LuniiV3;
        let md_vers = md[0];
        self.fw_major = md[2] as i32 - 0x30;
        self.fw_minor = md[4] as i32 - 0x30;
        self.fw_subminor = md[6] as i32 - 0x30;
        self.snu = md_v3_snu(md)?;

        // creating a backup of the metadata file
        let backup = cfg_dir().join(format!("{}.v{md_vers}.md", self.snu_str()));
        if !backup.is_file() {
            ctx.info(format!(
                "No backup of v{md_vers} metadata file found, creating one..."
            ));
            ensure_cfg_dir()?;
            std::fs::write(&backup, md)?;
        }

        if md_vers == 6 {
            ctx.debug("Forging story keys for v6 metadata file");
            self.bt = md[0x40..0x60].to_vec();
            self.load_md_fakestory_keys();
        } else {
            ctx.debug("Forging story keys for v7+ metadata file");
            self.story_key = reverse_bytes(&md[0x40..0x50]);
            self.story_iv = reverse_bytes(&md[0x50..0x60]);
            self.bt = self.forged_bt();
        }

        self.load_device_keys(ctx);
        self.log_v3_details(ctx);
        Ok(())
    }

    /// Real v3 device keys, if provided by the user in `~/.lunii/<SNU>.keys`.
    fn load_device_keys(&mut self, ctx: &Context) {
        let keyfile = cfg_dir().join(format!("{}.keys", self.snu_str()));
        if let Ok(keys) = std::fs::read(&keyfile)
            && keys.len() >= 0x20
        {
            self.device_key = reverse_bytes(&keys[0..0x10]);
            self.device_iv = reverse_bytes(&keys[0x10..0x20]);
            ctx.info(format!("v3 key file read from {}", keyfile.display()));
        }
    }

    fn log_v3_details(&self, ctx: &Context) {
        let (vid, pid) = FAH_V2_V3_USB_VID_PID;
        ctx.debug(format!(
            "\nSNU : {}\nHW  : v3\nFW  : v{}\nVID/PID : 0x{vid:04X} / 0x{pid:04X}\nDev Key : {}\nDev IV  : {}\nStory Key : {}\nStory IV  : {}",
            self.snu_str(),
            self.firmware(),
            opt_hex(&self.device_key),
            opt_hex(&self.device_iv),
            opt_hex(&self.story_key),
            opt_hex(&self.story_iv)
        ));
    }

    fn load_mdbackup(&mut self, ctx: &Context, version: u16) {
        let path = cfg_dir().join(format!("{}.v{version}.md", self.snu_str()));
        let Ok(md) = std::fs::read(&path) else {
            ctx.warn(format!("no .md v{version} file found ({})", path.display()));
            return;
        };
        ctx.info(format!(".md v{version} file found ({})", path.display()));

        if u16_le(&md, 0) != version {
            ctx.warn(format!(".md file is not v{version} ({})", path.display()));
            return;
        }
        match md_v3_snu(&md) {
            Ok(snu) if snu == self.snu => {}
            Ok(snu) => {
                ctx.warn(format!(
                    ".md file SNU mismatch ({}) vs. ({})",
                    crypto::hex_string(&snu),
                    crypto::hex_string(&self.snu)
                ));
                return;
            }
            Err(e) => {
                ctx.warn(format!(".md file unreadable ({e})"));
                return;
            }
        }
        if md.len() < 0x60 {
            return;
        }
        if version == 6 {
            self.bt = md[0x40..0x60].to_vec();
            self.load_md_fakestory_keys();
        } else if version == 7 {
            self.story_key = reverse_bytes(&md[0x40..0x50]);
            self.story_iv = reverse_bytes(&md[0x50..0x60]);
            self.bt = self.forged_bt();
        }
    }

    fn unsupported_md_parse(&mut self, ctx: &Context, md: &[u8]) -> Result<()> {
        self.device_version = DeviceVersion::LuniiV3;
        let md_vers = md.first().copied().unwrap_or(0);
        self.fw_major = (md.get(2).copied().unwrap_or(0) & !0x30) as i32;
        self.fw_minor = (md.get(4).copied().unwrap_or(0) & !0x30) as i32;
        self.fw_subminor = (md.get(6).copied().unwrap_or(0) & !0x30) as i32;
        self.snu = match md_v3_snu(md) {
            Ok(snu) => snu,
            Err(_) => {
                ctx.error("🛑 corrupted metadata file ? (maybe SD corruption)");
                vec![0u8; 16]
            }
        };

        ctx.warn(format!(
            "⚠️ Unsupported or corrupted metadata file v{md_vers}, checking for backups..."
        ));

        if self.story_key.is_none() {
            self.load_mdbackup(ctx, 6);
        }
        if self.story_key.is_none() {
            self.load_mdbackup(ctx, 7);
        }

        self.load_device_keys(ctx);
        if let (Some(dk), Some(div)) = (self.device_key.clone(), self.device_iv.clone()) {
            self.load_md_fakestory_keys();
            // preparing bt file by ciphering fake keys with real device keys
            if let (Some(sk), Some(siv)) = (&self.story_key, &self.story_iv) {
                let mut buffer = reverse_bytes(sk).unwrap_or_default();
                buffer.extend(reverse_bytes(siv).unwrap_or_default());
                self.bt = crypto::aes_cbc_encrypt(&buffer, &dk, &div)?;
            }
        }

        if self.story_key.is_none() {
            ctx.warn("🛑 no keys at all, unable to import stories. See README on Github for help.");
        } else {
            ctx.info("✅ story keys found, import supported.");
        }
        self.log_v3_details(ctx);
        Ok(())
    }

    /// Story keys forged from the SNU (v3 "trick" keys).
    fn load_md_fakestory_keys(&mut self) {
        let hex = hexlify(&self.snu);
        let mut key = hex.clone();
        key.extend_from_slice(&[0, 0]);
        let mut iv = vec![0u8; 8];
        iv.extend_from_slice(&hex[..hex.len().min(8)]);
        self.story_key = reverse_bytes(&key);
        self.story_iv = reverse_bytes(&iv);
    }

    /// Loads the story keys from its `bt` file (needs real device keys).
    fn load_story_keys(&mut self, bt_path: &Path) -> Result<()> {
        if let (Some(dk), Some(div)) = (self.device_key.clone(), self.device_iv.clone())
            && let Ok(bt) = std::fs::read(bt_path)
        {
            let ciphered = &bt[..bt.len().min(0x20)];
            let plain = self.decipher(ciphered, &dk, Some(&div))?;
            if plain.len() >= 0x20 {
                self.story_key = reverse_bytes(&plain[..0x10]);
                self.story_iv = reverse_bytes(&plain[0x10..0x20]);
                return Ok(());
            }
        }
        self.load_md_fakestory_keys();
        Ok(())
    }

    fn decipher(&self, buffer: &[u8], key: &[u8], iv: Option<&[u8]>) -> Result<Vec<u8>> {
        if self.device_version == DeviceVersion::LuniiV3 {
            crypto::aes_decipher(buffer, key, iv.context("missing IV")?, 512)
        } else {
            crypto::xxtea_decipher(buffer, key, 512)
        }
    }

    fn cipher(&self, buffer: &[u8], key: &[u8], iv: Option<&[u8]>) -> Result<Vec<u8>> {
        if self.debug_plain {
            return Ok(buffer.to_vec());
        }
        if self.device_version == DeviceVersion::LuniiV3 {
            crypto::aes_cipher(buffer, key, iv.context("missing IV")?, 512)
        } else {
            crypto::xxtea_cipher(buffer, key, 512)
        }
    }

    /// Authorization file of v1/v2 devices: ciphered `ri` header with the device key.
    fn v2_bt(&self, ri_ciphered: &[u8]) -> Result<Vec<u8>> {
        let key = self.device_key.clone().context("no device key")?;
        self.cipher(&ri_ciphered[..ri_ciphered.len().min(0x40)], &key, None)
    }

    /// Key / IV to decipher a file read from the device.
    fn plain_keys(&self, file: &str) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
        let v12 = self.device_version.is_lunii_v1_v2();
        let (mut key, mut iv) = if v12 {
            (Some(lunii_generic_key().to_vec()), None)
        } else {
            (self.story_key.clone(), self.story_iv.clone())
        };
        if file.ends_with("bt") {
            key = self.device_key.clone();
            iv = if v12 { None } else { self.device_iv.clone() };
        }
        if file.ends_with("ni") || file.ends_with("nm") {
            key = None;
        }
        (key, iv)
    }

    fn plain_data(&self, file: &Path) -> Result<Vec<u8>> {
        let Ok(data) = std::fs::read(file) else {
            return Ok(Vec::new());
        };
        let (key, iv) = self.plain_keys(&file.to_string_lossy());
        match key {
            Some(key) => self.decipher(&data, &key, iv.as_deref()),
            None => Ok(data),
        }
    }

    fn ciphered_data(&self, file: &str, data: &[u8]) -> Result<Vec<u8>> {
        let (mut key, iv) = if self.device_version.is_lunii_v1_v2() {
            (Some(lunii_generic_key().to_vec()), None)
        } else {
            (self.story_key.clone(), self.story_iv.clone())
        };
        if file.ends_with("bt") {
            key = self.device_key.clone();
        }
        if file.ends_with("ni") || file.ends_with("nm") {
            key = None;
        }
        match key {
            Some(key) => self.cipher(data, &key, iv.as_deref()),
            None => Ok(data.to_vec()),
        }
    }

    /// Story file name on the device (from a plain archive name).
    pub fn ciphered_name(file: &str, studio_ri: bool, studio_si: bool) -> String {
        let mut file = remove_suffix(file, ".plain").to_string();
        if studio_ri {
            file = format!("rf/000/{file}");
        }
        if studio_si {
            file = format!("sf/000/{file}");
        }
        let lower = file.to_lowercase();
        let lower = remove_suffix(&lower, ".mp3").to_lowercase();
        let mut file = remove_suffix(&lower, ".bmp").to_string();

        // upcasing filename
        let (dir, base) = split_path(&file);
        if base.chars().count() >= 8 {
            file = join_path(dir, &base.to_uppercase());
        }

        // upcasing uuid dir if present
        let (dir, _) = split_path(&file);
        if dir.chars().count() >= 8 {
            let head: String = file.chars().take(8).collect();
            if !head.contains('/') && !head.contains('\\') {
                file = head.to_uppercase() + &skip_chars(&file, 8);
            }
        }
        file.replace('\\', "/")
    }

    /// Plain archive name of a device file (relative to its story directory).
    fn plain_name(rel: &str) -> String {
        if rel.contains("rf/") || rel.contains("rf\\") {
            return format!("{rel}.bmp");
        }
        if rel.contains("sf/") || rel.contains("sf\\") {
            return format!("{rel}.mp3");
        }
        if rel.ends_with("li") || rel.ends_with("ri") || rel.ends_with("si") {
            return format!("{rel}.plain");
        }
        rel.to_string()
    }

    fn check_v3_key(&self, story_path: &Path) -> bool {
        let Ok(ri) = std::fs::read(story_path.join("ri")) else {
            return false;
        };
        let (Some(key), Some(iv)) = (&self.story_key, &self.story_iv) else {
            return false;
        };
        self.decipher(&ri, key, Some(iv))
            .map(|p| p.starts_with(b"000"))
            .unwrap_or(false)
    }

    fn check_v2_bt(&self, story_path: &Path) -> bool {
        let (Ok(bt), Ok(ri)) = (
            std::fs::read(story_path.join("bt")),
            std::fs::read(story_path.join("ri")),
        ) else {
            return false;
        };
        let Some(key) = &self.device_key else {
            return false;
        };
        self.decipher(&bt, key, None)
            .map(|plain| ri[..ri.len().min(0x40)] == plain[..])
            .unwrap_or(false)
    }

    fn valid_story(&mut self, ctx: &Context, story_path: &Path) -> bool {
        let files: Vec<String> = common::walk_files(story_path)
            .into_iter()
            .map(|(_, rel)| rel.to_lowercase())
            .collect();
        let dirs: Vec<String> = walkdir::WalkDir::new(story_path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_lowercase())
            .collect();

        for pattern in ["li", "ni", "ri", "si"] {
            if !files.iter().any(|f| f.ends_with(pattern)) {
                ctx.warn(format!("Missing {pattern} in {}", story_path.display()));
                return false;
            }
        }

        if self.device_version == DeviceVersion::LuniiV3 {
            let _ = self.load_story_keys(&story_path.join("bt"));
            if !self.check_v3_key(story_path) {
                // not the trick keys or dev keys unknown... can't get further
                return true;
            }
        }

        if self.device_version.is_lunii_v1_v2() && !self.check_v2_bt(story_path) {
            ctx.warn(format!(
                "Bad authorization file bt in {}",
                story_path.display()
            ));
            if let Ok(ri) = std::fs::read(story_path.join("ri"))
                && let Ok(bt) = self.v2_bt(&ri)
            {
                ctx.info("Authorization file creation...");
                self.bt = bt;
                if let Err(e) = std::fs::write(story_path.join("bt"), &self.bt) {
                    ctx.error(e.to_string());
                }
            }
        }

        for pattern in ["rf", "sf"] {
            if !dirs.iter().any(|d| d.ends_with(pattern)) {
                ctx.warn(format!("Missing {pattern} in {}", story_path.display()));
                return false;
            }
        }

        for (index_file, res_dir) in [("ri", "rf"), ("si", "sf")] {
            let plain = match self
                .plain_data(&story_path.join(index_file))
                .map(String::from_utf8)
            {
                Ok(Ok(p)) => p,
                _ => {
                    ctx.warn("Failed to decode ri or si file");
                    return false;
                }
            };
            let plain: Vec<char> = plain.trim_end_matches('\0').chars().collect();
            for chunk in plain.chunks(12) {
                let res: String = chunk.iter().collect::<String>().replace('\\', "/");
                if !story_path.join(res_dir).join(&res).is_file() {
                    ctx.warn(format!(
                        "Missing {res_dir}/{res} in {}",
                        story_path.display()
                    ));
                    return false;
                }
            }
        }
        true
    }

    fn finish_import(
        &mut self,
        ctx: &Context,
        story_dir: &Path,
        story: Story,
        write_bt: bool,
    ) -> Result<()> {
        if write_bt {
            ctx.info("Authorization file creation...");
            std::fs::write(story_dir.join("bt"), &self.bt)?;
        }
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
        let output = self.content_dir().join(&short);
        std::fs::create_dir_all(&output)?;

        let mut night_mode = false;
        for (index, file) in contents.iter().enumerate() {
            ctx.story_progress(&short, index, contents.len());
            if ctx.is_aborted() {
                return Err(self.abort_cleanup(ctx, &new_uuid));
            }
            if [FILE_UUID, FILE_META, FILE_THUMB].contains(&file.as_str()) {
                continue;
            }
            if file.ends_with("nm") {
                night_mode = true;
            }
            if archive.is_dir(file) {
                continue;
            }

            let data_plain = archive.read(file)?;
            let data = self.ciphered_data(file, &data_plain)?;
            let new_name = Self::ciphered_name(file, false, false);
            ctx.debug(format!(
                "File {}/{} > {new_name}",
                index + 1,
                contents.len()
            ));
            common::write_with_progress(ctx, &output.join(&new_name), &data)?;

            if self.device_version.is_lunii_v1_v2() && file.ends_with("ri.plain") {
                self.bt = self.v2_bt(&data)?;
            }
        }

        self.finish_import(
            ctx,
            &output,
            Story::with_flags(new_uuid, false, night_mode),
            true,
        )
    }

    fn import_lunii_v2(&mut self, ctx: &Context, path: &Path) -> Result<()> {
        let archive = Archive::open(path)?;
        let names = archive.names();
        let first = names.first().context("empty archive")?;
        let (_, new_uuid) = uuid_from_archive(first)?;
        self.check_not_loaded(&new_uuid)?;

        let output = self.content_dir();
        std::fs::create_dir_all(&output)?;
        let short = story::short_uuid(&new_uuid);

        let files = archive.file_names();
        let mut night_mode = false;
        for (index, entry) in files.iter().enumerate() {
            ctx.story_progress(&short, index, files.len());
            if ctx.is_aborted() {
                return Err(self.abort_cleanup(ctx, &new_uuid));
            }
            if entry.ends_with("bt") {
                continue;
            }
            if entry.ends_with("nm") {
                night_mode = true;
            }

            let data_v2 = archive.read(entry)?;
            // stripping extra uuid chars
            let file = if entry.contains('-') {
                skip_chars(entry, 28)
            } else {
                skip_chars(entry, 24)
            };

            let data = if self.device_version.is_lunii_v1_v2() {
                data_v2
            } else {
                let plain = if file.ends_with("ni") || file.ends_with("nm") {
                    data_v2
                } else {
                    crypto::xxtea_decipher(&data_v2, &lunii_generic_key(), 512)?
                };
                self.ciphered_data(&file, &plain)?
            };

            let new_name = Self::ciphered_name(&file, false, false);
            ctx.debug(format!("File {}/{} > {new_name}", index + 1, files.len()));
            common::write_with_progress(ctx, &output.join(&new_name), &data)?;

            if self.device_version.is_lunii_v1_v2() && file.ends_with("ri") {
                self.bt = self.v2_bt(&data)?;
            }
        }

        let story_dir = output.join(&short);
        self.finish_import(
            ctx,
            &story_dir,
            Story::with_flags(new_uuid, false, night_mode),
            true,
        )
    }

    fn import_lunii_v3(&mut self, ctx: &Context, path: &Path) -> Result<()> {
        let filename = path
            .file_name()
            .map(|f| f.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !filename.starts_with(&self.snu_str().to_lowercase()) {
            bail!("Lunii v3 personnal backup can't be imported on this device (SNU mismatch).");
        }
        ctx.info("Restoring Lunii v3 personnal backup...");

        let archive = Archive::open_zip(path)?;
        let names = archive.names();
        let (_, new_uuid) = uuid_from_archive(names.first().context("empty archive")?)?;
        self.check_not_loaded(&new_uuid)?;

        let short = story::short_uuid(&new_uuid);
        let story_dir = self.content_dir().join(&short);
        std::fs::create_dir_all(&story_dir)?;

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
            let data = archive.read(file)?;
            // backup entries are "<uuid>/<path in story dir>", the story dir is the short UUID
            let rel = file.split_once('/').map(|(_, r)| r).unwrap_or(file);
            ctx.debug(format!("File {}/{} > {file}", index + 1, files.len()));
            common::write_with_progress(ctx, &story_dir.join(rel), &data)?;
        }

        self.finish_import(
            ctx,
            &story_dir,
            Story::with_flags(new_uuid, false, night_mode),
            false,
        )
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

    fn export_plain_story(
        &mut self,
        ctx: &Context,
        story: &Story,
        out_dir: &Path,
    ) -> Result<PathBuf> {
        let short = story.short_uuid();
        let story_path = self.story_path(story);
        ctx.info(format!("🚧 Exporting {short} - {}", story.name()));

        if self.device_version == DeviceVersion::LuniiV3 {
            self.load_story_keys(&story_path.join("bt"))?;
            if !self.check_v3_key(&story_path) {
                bail!("Lunii v3 requires Device Key for genuine story export.");
            }
        }

        let zip_path = out_dir.join(format!(
            "{}.{short}.plain.pk",
            common::secure_filename(&story.name())
        ));
        let files: Vec<_> = common::walk_files(&story_path)
            .into_iter()
            .filter(|(abs, _)| {
                !matches!(
                    abs.file_name().and_then(|f| f.to_str()),
                    Some("bt") | Some("md") | Some(crate::album::meta::FILE_ALBUM)
                )
            })
            .collect();

        let mut zip = StoryZip::create(&zip_path)?;
        ctx.debug("> Zipping story ...");
        for (index, (abs, rel)) in files.iter().enumerate() {
            ctx.story_progress(&short, index, files.len());
            check_abort(ctx)?;
            let data_plain = self.plain_data(abs)?;
            let new_name = Self::plain_name(rel);
            ctx.debug(&new_name);
            zip.add(&new_name, &data_plain)?;
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

    /// Writes the device settings (`.cfg`).
    pub fn update_config(&self) -> Result<()> {
        let mut cfg = vec![0x00, 0x01]; // version
        for (index, value) in self.config.iter().enumerate() {
            cfg.extend_from_slice(&(index as u16).to_le_bytes());
            cfg.extend_from_slice(&value.to_le_bytes());
        }
        std::fs::write(self.mount_point.join(".cfg"), cfg)?;
        Ok(())
    }

    pub fn night_mode_enabled(&self) -> bool {
        self.config
            .get(LUNII_CFGPOS_NM_ENABLED)
            .copied()
            .unwrap_or(0)
            == 1
    }

    /// Toggles the night mode flag (`nm` file) of stories.
    pub fn toggle_night_mode(&mut self, uuids: &[Uuid]) -> Result<()> {
        for uuid in uuids {
            let story = self
                .stories
                .iter()
                .find(|s| s.uuid == *uuid)
                .cloned()
                .context("This story is not present on your storyteller")?;
            let nm = self.story_path(&story).join("nm");
            if nm.is_file() {
                std::fs::remove_file(&nm)?;
            } else {
                std::fs::write(&nm, b"")?;
            }
            if let Some(s) = self.stories.iter_mut().find(|s| s.uuid == *uuid) {
                s.nm = !s.nm;
            }
        }
        Ok(())
    }
}

impl Device for LuniiDevice {
    fn mount_point(&self) -> &Path {
        &self.mount_point
    }

    fn version(&self) -> DeviceVersion {
        self.device_version
    }

    fn snu_str(&self) -> String {
        LuniiDevice::snu_str(self)
    }

    fn info(&self) -> String {
        let mut s = format!("Lunii device on \"{}\"\n", self.mount_point.display());
        s += &format!("- firmware : v{}\n", self.firmware());
        s += &format!("- SNU      : {}\n", hex_spaced(&self.snu));
        s += &format!("- dev key  : {}\n", opt_hex(&self.device_key));
        if self.device_version == DeviceVersion::LuniiV3 {
            s += &format!("- dev iv   : {}\n", opt_hex(&self.device_iv));
            if self.story_key.is_some() {
                s += &format!("- story key: {}\n", opt_hex(&self.story_key));
            }
            s += &format!("- story iv : {}\n", opt_hex(&self.story_iv));
        }
        s += &format!("- stories  : {}x", self.stories.len());
        s
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
        story::short_uuid(uuid)
    }

    fn update_pack_index(&self) -> Result<()> {
        let mut pi = Vec::new();
        let mut pi_hidden = Vec::new();
        for story in self.stories.iter() {
            if story.hidden {
                &mut pi_hidden
            } else {
                &mut pi
            }
            .extend_from_slice(story.uuid.as_bytes());
        }
        let pi_path = self.mount_point.join(".pi");
        let pi_hidden_path = self.mount_point.join(".pi.hidden");
        let _ = std::fs::remove_file(&pi_path);
        let _ = std::fs::remove_file(&pi_hidden_path);
        std::fs::write(pi_path, pi)?;
        std::fs::write(pi_hidden_path, pi_hidden)?;
        Ok(())
    }

    fn import_story(&mut self, ctx: &Context, path: &Path) -> Result<()> {
        ctx.info(format!("🚧 Loading {}...", path.display()));

        let archive_size = std::fs::metadata(path)?.len();
        let free = common::free_space(&self.mount_point)?;
        if archive_size >= free {
            bail!(
                "Not enough space left on Lunii (only {}MB)",
                free / 1024 / 1024
            );
        }

        let kind = archive_type(path, true)?;
        ctx.debug(format!("Archive => {kind:?}"));
        match kind {
            ArchiveType::LuniiPlain => self.import_lunii_plain(ctx, path),
            ArchiveType::LuniiV2Zip | ArchiveType::LuniiV27z => self.import_lunii_v2(ctx, path),
            ArchiveType::LuniiV3Zip => self.import_lunii_v3(ctx, path),
            ArchiveType::StudioZip | ArchiveType::StudioSevenZ => {
                common::import_studio(self, ctx, path)
            }
            other => bail!("Archive => Unsupported type 0x{:02X}", other as u8),
        }
    }

    fn export_story(&mut self, ctx: &Context, uuid_part: &str, out_dir: &Path) -> Result<PathBuf> {
        let story = self.find_story(uuid_part)?;
        let story_path = self.story_path(&story);
        if !story_path.is_dir() {
            bail!("Story directory not found ({})", story_path.display());
        }

        // official stories can't be exported in plain format (piracy)
        if story.is_official() {
            if self.device_version < DeviceVersion::LuniiV3 {
                bail!("🛑 Forbidden to export official story : '{}'", story.name());
            }
            return self.export_backup_story(ctx, &story, out_dir);
        }

        if self.device_version < DeviceVersion::LuniiV3 || self.device_key.is_some() {
            self.export_plain_story(ctx, &story, out_dir)
        } else {
            self.export_backup_story(ctx, &story, out_dir)
        }
    }

    fn can_import(&self) -> bool {
        !(self.device_version == DeviceVersion::LuniiV3 && self.story_key.is_none())
    }

    fn can_export(&self) -> bool {
        true
    }

    fn story_exportable(&self, story: &Story) -> bool {
        self.device_version >= DeviceVersion::LuniiV3 || !story.is_official()
    }

    fn supported_extensions(&self) -> &'static [&'static str] {
        LUNII_SUPPORTED_EXT
    }

    fn resolve_dir_uuid(&self, dir_name: &str) -> Option<String> {
        super::uuid_from_db(dir_name).or_else(|| {
            (dir_name.len() == 8 && dir_name.chars().all(|c| c.is_ascii_hexdigit()))
                .then(|| format!("{}{dir_name}", "00".repeat(12)))
        })
    }

    fn edit_album(
        &mut self,
        ctx: &Context,
        uuid: &Uuid,
        entries: &[crate::album::AlbumEntry],
    ) -> Result<()> {
        common::edit_album(self, ctx, uuid, entries)
    }

    fn validate_story_dir(&mut self, ctx: &Context, story_dir: &Path) -> bool {
        self.valid_story(ctx, story_dir)
    }

    fn as_lunii(&self) -> Option<&LuniiDevice> {
        Some(self)
    }

    fn as_lunii_mut(&mut self) -> Option<&mut LuniiDevice> {
        Some(self)
    }
}

fn feed_stories_file(
    ctx: &Context,
    root: &Path,
    pi_path: &Path,
    hidden: bool,
    list: &mut StoryList,
) {
    let Ok(data) = std::fs::read(pi_path) else {
        ctx.info(format!(
            "Read 0 {}stories",
            if hidden { "(hidden) " } else { "" }
        ));
        return;
    };
    let mut count = 0;
    for chunk in data.as_chunks::<16>().0 {
        let uuid = Uuid::from_bytes(*chunk);
        ctx.debug(format!("> {uuid}"));
        if list.contains_uuid(&uuid) {
            ctx.warn("Found duplicate story, cleaning...");
            continue;
        }
        let mut story = Story::with_flags(uuid, hidden, false);
        story.nm = root
            .join(if hidden {
                HIDDEN_STORIES_BASEDIR
            } else {
                STORIES_BASEDIR
            })
            .join(story.short_uuid())
            .join("nm")
            .is_file();
        list.push(story);
        count += 1;
    }
    ctx.info(format!(
        "Read {count} {}stories",
        if hidden { "(hidden) " } else { "" }
    ));
}

/// Reads `.pi` and `.pi.hidden` (installed stories).
pub fn feed_stories(ctx: &Context, root: &Path) -> StoryList {
    ctx.info("Reading Lunii loaded stories...");
    let mut list = StoryList::new();
    feed_stories_file(ctx, root, &root.join(".pi"), false, &mut list);
    feed_stories_file(ctx, root, &root.join(".pi.hidden"), true, &mut list);
    list
}

/// Reads the settings file (`.cfg`).
pub fn feed_config(ctx: &Context, root: &Path) -> Vec<u16> {
    let mut config = DEFAULT_CONFIG.to_vec();
    let Ok(cfg) = std::fs::read(root.join(".cfg")) else {
        ctx.debug("Config file not found, using default values");
        return config;
    };
    let version = u16_le(&cfg, 0);
    if version != 0x100 {
        ctx.error(format!(
            "🛑 Unsupported config version {version}, using default values"
        ));
        return config;
    }
    ctx.debug("Reading Lunii config...");
    for item in cfg[2..].as_chunks::<4>().0 {
        let index = u16::from_le_bytes([item[0], item[1]]) as usize;
        let value = u16::from_le_bytes([item[2], item[3]]);
        if index < config.len() {
            config[index] = value;
        }
    }
    config
}

impl common::StoryWriter for LuniiDevice {
    fn cipher_story_file(&self, file: &str, data: &[u8]) -> Result<Vec<u8>> {
        self.ciphered_data(file, data)
    }

    /// The authorization file `bt`: on v1/v2 derived from `ri` (rewritten
    /// when tracks are appended), the device one on v3.
    fn seal_story(
        &mut self,
        ctx: &Context,
        story_dir: &Path,
        _story: &Story,
        ri: &[u8],
        new_story: bool,
    ) -> Result<()> {
        if self.device_version.is_lunii_v1_v2() {
            let bt = self.v2_bt(ri)?;
            if new_story {
                self.bt = bt.clone();
            }
            ctx.info("Authorization file creation...");
            return common::write_file(&story_dir.join("bt"), &bt);
        }
        if new_story {
            ctx.info("Authorization file creation...");
            std::fs::write(story_dir.join("bt"), &self.bt)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(
            LuniiDevice::ciphered_name("rf/000/0a1b2c3d.bmp", false, false),
            "rf/000/0A1B2C3D"
        );
        assert_eq!(LuniiDevice::ciphered_name("ri.plain", false, false), "ri");
        assert_eq!(
            LuniiDevice::ciphered_name("89abcdef/sf/000/12345678.mp3", false, false),
            "89ABCDEF/sf/000/12345678"
        );
        assert_eq!(
            LuniiDevice::ciphered_name("COVER", true, false),
            "rf/000/cover"
        );
        assert_eq!(
            LuniiDevice::ciphered_name("ABCDEFGH", false, true),
            "sf/000/ABCDEFGH"
        );
        assert_eq!(
            LuniiDevice::plain_name("rf/000/0A1B2C3D"),
            "rf/000/0A1B2C3D.bmp"
        );
        assert_eq!(LuniiDevice::plain_name("ri"), "ri.plain");
        assert_eq!(LuniiDevice::plain_name("ni"), "ni");
    }
}
