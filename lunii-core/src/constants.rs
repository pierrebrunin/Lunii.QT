//! Constants shared by every module (port of `pkg/api/constants.py`).

use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// Hardcoded key found in the external flash of Lunii v1/v2 devices
/// (`91BD7A0A A75440A9 BBD49D6C E0DCC0E3`, stored little endian).
pub const RAW_KEY_GENERIC: [u32; 4] = [0x91BD7A0A, 0xA75440A9, 0xBBD49D6C, 0xE0DCC0E3];

/// [`RAW_KEY_GENERIC`] as bytes (each word little endian).
pub fn lunii_generic_key() -> [u8; 16] {
    vectkey_to_bytes(&RAW_KEY_GENERIC)
}

pub fn vectkey_to_bytes(key: &[u32; 4]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, k) in key.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&k.to_le_bytes());
    }
    out
}

pub const OFFICIAL_TOKEN_URL: &str = "https://server-auth-prod.lunii.com/guest/create";
pub const OFFICIAL_DB_URL: &str = "https://server-data-prod.lunii.com/v2/packs";
pub const OFFICIAL_IMAGE_BASE_URL: &str = "https://storage.googleapis.com/lunii-data-prod";

// ---------------------------------------------------------------------------
// Configuration directory (~/.lunii-qt, shared with the Python application)
// ---------------------------------------------------------------------------

static CFG_DIR_OVERRIDE: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Overrides the configuration directory (used by tests, or by a portable build).
pub fn set_cfg_dir(path: impl Into<PathBuf>) {
    *CFG_DIR_OVERRIDE.write().unwrap() = Some(path.into());
}

/// `~/.lunii-qt` (or the `LUNII_QT_CFG_DIR` environment variable, or the override).
pub fn cfg_dir() -> PathBuf {
    if let Some(p) = CFG_DIR_OVERRIDE.read().unwrap().as_ref() {
        return p.clone();
    }
    if let Some(p) = std::env::var_os("LUNII_QT_CFG_DIR") {
        return PathBuf::from(p);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".lunii-qt")
}

pub fn cache_dir() -> PathBuf {
    cfg_dir().join("cache")
}

pub fn file_official_db() -> PathBuf {
    cfg_dir().join("official.db")
}

pub fn file_third_party_db() -> PathBuf {
    cfg_dir().join("third-party.db")
}

/// Ensures the configuration directory exists.
pub fn ensure_cfg_dir() -> std::io::Result<PathBuf> {
    let dir = cfg_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

// ---------------------------------------------------------------------------
// FFMPEG
// ---------------------------------------------------------------------------

fn ffmpeg_binary_name() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Looks for ffmpeg, first in the configuration directory, then in `PATH`.
pub fn which_ffmpeg() -> Option<PathBuf> {
    let local = cfg_dir().join(ffmpeg_binary_name());
    if is_executable(&local) {
        return Some(local);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(ffmpeg_binary_name()))
        .find(|candidate| is_executable(candidate))
}

/// `True` when audio transcoding is possible on this host.
pub fn transcoding_supported() -> bool {
    which_ffmpeg().is_some()
}

// ---------------------------------------------------------------------------
// Hardware
// ---------------------------------------------------------------------------

/// Kind of storyteller. The numeric values match the Python constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceVersion {
    LuniiV1or2Unknown = 0,
    LuniiV1 = 1,
    LuniiV2 = 2,
    LuniiV3 = 3,
    FlamV1 = 10,
    Undefined = 255,
}

impl DeviceVersion {
    /// v1, v2 or unknown v1/v2 (XXTEA based devices).
    pub fn is_lunii_v1_v2(self) -> bool {
        self <= DeviceVersion::LuniiV2
    }

    pub fn is_flam(self) -> bool {
        self == DeviceVersion::FlamV1
    }
}

impl std::fmt::Display for DeviceVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            DeviceVersion::LuniiV1or2Unknown => "Lunii v1/v2",
            DeviceVersion::LuniiV1 => "Lunii v1",
            DeviceVersion::LuniiV2 => "Lunii v2",
            DeviceVersion::LuniiV3 => "Lunii v3",
            DeviceVersion::FlamV1 => "Flam",
            DeviceVersion::Undefined => "Unknown device",
        };
        f.write_str(s)
    }
}

pub const FAH_V1_USB_VID_PID: (u16, u16) = (0x0c45, 0x6820);
pub const FAH_V1_FW_2_USB_VID_PID: (u16, u16) = (0x0c45, 0x6840);
pub const FAH_V2_V3_USB_VID_PID: (u16, u16) = (0x0483, 0xa341);
pub const FLAM_USB_VID_PID: (u16, u16) = (0x303A, 0x819E);

// ---------------------------------------------------------------------------
// Archive types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveType {
    Unknown = 0x00,
    /// Lunii plain story
    LuniiPlain = 0x01,
    /// Lunii v3 (aes) zip
    LuniiV3Zip = 0x02,
    /// Lunii from Flam (aes) zip
    LuniiFlamZip = 0x03,
    /// Lunii v2 (xxtea) zip
    LuniiV2Zip = 0x10,
    /// Lunii v2 (xxtea) 7zip
    LuniiV27z = 0x11,
    /// STUdio format
    StudioZip = 0x20,
    StudioSevenZ = 0x21,
    /// Flam backup as zip
    FlamZip = 0x40,
    /// Flam backup as 7z
    FlamSevenZ = 0x41,
    /// Flam plain story
    FlamPlain = 0x42,
}

pub const EXT_PK_PLAIN: &str = ".plain.pk";
pub const EXT_PK_V2: &str = ".v2.pk";
pub const EXT_PK_V1: &str = ".v1.pk";
pub const EXT_PK_VX: &str = ".pk";
pub const EXT_ZIP: &str = ".zip";
pub const EXT_7Z: &str = ".7z";

pub const LUNII_SUPPORTED_EXT: &[&str] = &[
    EXT_ZIP,
    EXT_7Z,
    EXT_PK_V1,
    EXT_PK_V2,
    EXT_PK_PLAIN,
    EXT_PK_VX,
];
pub const FLAM_SUPPORTED_EXT: &[&str] = &[EXT_ZIP, EXT_7Z, EXT_PK_PLAIN, EXT_PK_VX];

// ---------------------------------------------------------------------------
// Lunii .cfg positions
// ---------------------------------------------------------------------------

pub const LUNII_CFGPOS_IDLE_TIME: usize = 0;
pub const LUNII_CFGPOS_LOWBAT_TIME: usize = 2;
pub const LUNII_CFGPOS_NM_ENABLED: usize = 3;
pub const LUNII_CFGPOS_NM_VOL_LIMIT: usize = 4;
pub const LUNII_CFGPOS_NM_STORYCOUNT: usize = 5;
pub const LUNII_CFGPOS_NM_AUTOPLAY: usize = 7;
pub const LUNII_CFGPOS_NM_TURNOFF_NM: usize = 8;

// ---------------------------------------------------------------------------
// Story archive file names
// ---------------------------------------------------------------------------

pub const FILE_THUMB: &str = "_thumbnail.png";
pub const FILE_META: &str = "_metadata.json";
pub const FILE_UUID: &str = "uuid.bin";
pub const FILE_STUDIO_JSON: &str = "story.json";
pub const FILE_STUDIO_THUMB: &str = "thumbnail.png";

pub const STORY_UNKNOWN: &str = "Unknown story (maybe a User created story)...";
pub const DESC_NOT_FOUND: &str = "No description found.";
pub const AUTHOR_NOT_FOUND: &str = "No authors.";
