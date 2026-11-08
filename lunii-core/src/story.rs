//! Stories model, STUdio story conversion and archive type detection.

use std::path::Path;

use anyhow::{Context as _, Result, anyhow, bail};
use indexmap::IndexMap;
use serde_json::Value;
use uuid::Uuid;

use crate::archive::Archive;
use crate::constants::*;
use crate::crypto;
use crate::db;

pub const NODE_SIZE: usize = 0x2C;
pub const NI_HEADER_SIZE: usize = 0x200;

// ---------------------------------------------------------------------------
// Story
// ---------------------------------------------------------------------------

/// A story installed on a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Story {
    pub uuid: Uuid,
    pub hidden: bool,
    /// Night mode flag (`nm` file present)
    pub nm: bool,
    /// Size on device, `None` until computed
    pub size: Option<u64>,
}

impl Story {
    pub fn new(uuid: Uuid) -> Self {
        Self {
            uuid,
            hidden: false,
            nm: false,
            size: None,
        }
    }

    pub fn with_flags(uuid: Uuid, hidden: bool, nm: bool) -> Self {
        Self {
            uuid,
            hidden,
            nm,
            size: None,
        }
    }

    /// Upper case hyphenated UUID.
    pub fn str_uuid(&self) -> String {
        db::db_key(&self.uuid)
    }

    /// Last 8 hex chars of the UUID, upper case (Lunii story directory name).
    pub fn short_uuid(&self) -> String {
        short_uuid(&self.uuid)
    }

    pub fn name(&self) -> String {
        db::story_name(&self.uuid)
    }

    pub fn description(&self) -> String {
        db::story_description(&self.uuid)
    }

    pub fn author(&self) -> String {
        db::story_author(&self.uuid)
    }

    pub fn is_official(&self) -> bool {
        db::is_official(&self.uuid)
    }

    pub fn picture(&self, reload: bool) -> Option<Vec<u8>> {
        db::story_picture(&self.uuid, reload)
    }

    pub fn meta(&self) -> Option<String> {
        db::story_meta(&self.uuid)
    }
}

pub fn short_uuid(uuid: &Uuid) -> String {
    uuid.simple().to_string()[24..].to_uppercase()
}

/// `key_part` (dashes ignored) is contained in the UUID hex representation.
pub fn uuid_match(uuid: &Uuid, key_part: &str) -> bool {
    let hex = uuid.simple().to_string().to_uppercase();
    hex.contains(&key_part.replace('-', "").to_uppercase())
}

/// Ordered list of stories, searchable by partial UUID.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoryList(pub Vec<Story>);

impl StoryList {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn contains_key(&self, key_part: &str) -> bool {
        self.0.iter().any(|s| uuid_match(&s.uuid, key_part))
    }

    pub fn get(&self, key_part: &str) -> Option<&Story> {
        self.0.iter().find(|s| uuid_match(&s.uuid, key_part))
    }

    pub fn get_mut(&mut self, key_part: &str) -> Option<&mut Story> {
        self.0.iter_mut().find(|s| uuid_match(&s.uuid, key_part))
    }

    pub fn matching(&self, key_part: &str) -> Vec<&Story> {
        self.0
            .iter()
            .filter(|s| uuid_match(&s.uuid, key_part))
            .collect()
    }

    pub fn contains_uuid(&self, uuid: &Uuid) -> bool {
        self.0.iter().any(|s| s.uuid == *uuid)
    }

    pub fn remove_uuid(&mut self, uuid: &Uuid) {
        self.0.retain(|s| s.uuid != *uuid);
    }
}

impl std::ops::Deref for StoryList {
    type Target = Vec<Story>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for StoryList {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Moves the selected stories by one position (`offset` = ±1) or to the
/// top / bottom (`offset` = ±2 or more). Port of `MainWindow.ts_move`.
pub fn move_stories(list: &mut Vec<Story>, selected: &[Uuid], offset: i32) {
    if selected.is_empty() || offset == 0 {
        return;
    }
    // moving to the right is moving to the left on the reversed list
    if offset >= 1 {
        list.reverse();
    }

    let mut working: Vec<(Story, f64)> = list
        .drain(..)
        .enumerate()
        .map(|(i, s)| (s, (i + 1) as f64))
        .collect();

    let mut prev_index: Option<usize> = None;
    let mut first_index: Option<usize> = None;
    for i in 0..working.len() {
        if selected.contains(&working[i].0.uuid) {
            if let Some(prev) = prev_index {
                let reference = if offset.abs() > 1 {
                    working[first_index.unwrap()].1
                } else {
                    working[prev].1
                };
                working[i].1 = reference - reference / working[i].1;
            }
        } else {
            prev_index = Some(i);
            if first_index.is_none() {
                first_index = Some(i);
            }
        }
    }

    // stable sort
    working.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    list.extend(working.into_iter().map(|(s, _)| s));

    if offset >= 1 {
        list.reverse();
    }
}

// ---------------------------------------------------------------------------
// STUdio stories
// ---------------------------------------------------------------------------

/// `os.path.splitext(path)[0]` (posix flavour).
fn splitext_root(path: &str) -> &str {
    let base_start = path.rfind('/').map(|p| p + 1).unwrap_or(0);
    let base = &path[base_start..];
    let leading_dots = base.len() - base.trim_start_matches('.').len();
    match base[leading_dots..].rfind('.') {
        Some(dot) => &path[..base_start + leading_dots + dot],
        None => path,
    }
}

/// Last 8 chars, upper case.
/// Device file name of a STUdio asset: the last 8 characters of its name,
/// upper case, left padded with `0` when shorter. The devices need exactly 8
/// characters: `ri` / `si` are tables of fixed 12 bytes entries (`000\XXXXXXXX`).
pub(crate) fn normalized_name(file: &str) -> String {
    let root: Vec<char> = splitext_root(file).chars().collect();
    let start = root.len().saturating_sub(8);
    let name = root[start..].iter().collect::<String>().to_uppercase();
    format!("{name:0>8}")
}

fn as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(*b as i64),
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// A story in STUdio format (`story.json` + `assets/`).
#[derive(Debug, Clone)]
pub struct StudioStory {
    pub format_version: String,
    pub pack_version: i64,
    pub title: String,
    pub description: String,
    pub uuid: Uuid,
    /// Night mode available
    pub nm: bool,
    stage_nodes: Vec<Value>,
    /// (action node id, global index, options count)
    action_nodes: Vec<(String, i64, usize)>,
    /// image asset -> (normalized name, index)
    pub ri: IndexMap<String, (String, usize)>,
    /// audio asset -> (normalized name, index)
    pub si: IndexMap<String, (String, usize)>,
    pub li: Vec<i32>,
}

impl StudioStory {
    pub fn load(story_json: &Value) -> Result<Self> {
        let str_field = |k: &str| {
            story_json
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };

        let stage_nodes = story_json
            .get("stageNodes")
            .and_then(Value::as_array)
            .cloned()
            .context("missing 'stageNodes' in story.json")?;

        let mut story = StudioStory {
            format_version: str_field("format"),
            pack_version: story_json.get("version").and_then(as_i64).unwrap_or(0),
            title: str_field("title"),
            description: str_field("description"),
            uuid: Uuid::nil(),
            nm: story_json
                .get("nightModeAvailable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            stage_nodes: Vec::new(),
            action_nodes: Vec::new(),
            ri: IndexMap::new(),
            si: IndexMap::new(),
            li: Vec::new(),
        };

        let mut first_uuid = None;
        for snode in &stage_nodes {
            let n_uuid = snode
                .get("uuid")
                .and_then(Value::as_str)
                .and_then(|u| Uuid::parse_str(u).ok())
                .context("invalid stage node UUID in story.json")?;
            first_uuid.get_or_insert(n_uuid);

            if let Some(image) = snode
                .get("image")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                && !story.ri.contains_key(image)
            {
                let idx = story.ri.len();
                story
                    .ri
                    .insert(image.to_string(), (normalized_name(image), idx));
            }

            if let Some(audio) = snode
                .get("audio")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                && !story.si.contains_key(audio)
            {
                let idx = story.si.len();
                story
                    .si
                    .insert(audio.to_string(), (normalized_name(audio), idx));
            }
        }
        story.uuid = first_uuid.context("no stage node in story.json")?;

        let action_nodes = story_json
            .get("actionNodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut absolute_index = 0i64;
        for anode in &action_nodes {
            let id = anode
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let options = anode
                .get("options")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            story.action_nodes.push((id, absolute_index, options.len()));
            absolute_index += options.len() as i64;
            for option in &options {
                let option = option.as_str().unwrap_or_default();
                let index = stage_nodes
                    .iter()
                    .position(|s| s.get("uuid").and_then(Value::as_str) == Some(option))
                    .map(|i| i as i32)
                    .unwrap_or(-1);
                story.li.push(index);
            }
        }

        story.stage_nodes = stage_nodes;
        Ok(story)
    }

    pub fn short_uuid(&self) -> String {
        short_uuid(&self.uuid)
    }

    pub fn ri_data(&self) -> Vec<u8> {
        self.ri
            .values()
            .map(|(name, _)| format!("000\\{name}"))
            .collect::<String>()
            .into_bytes()
    }

    pub fn si_data(&self) -> Vec<u8> {
        self.si
            .values()
            .map(|(name, _)| format!("000\\{name}"))
            .collect::<String>()
            .into_bytes()
    }

    fn transition(&self, trans: Option<&Value>) -> Result<Vec<u8>> {
        let Some(trans) = trans.filter(|t| !t.is_null()) else {
            return Ok([0xFFu8; 12].to_vec());
        };
        let anode_id = trans
            .get("actionNode")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (_, global_index, options) = self
            .action_nodes
            .iter()
            .find(|(id, _, _)| id == anode_id)
            .ok_or_else(|| anyhow!("action node '{anode_id}' not found in story.json"))?;
        let option_index = trans.get("optionIndex").and_then(as_i64).unwrap_or(0);

        let mut out = Vec::with_capacity(12);
        out.extend_from_slice(&(*global_index as i32).to_le_bytes());
        out.extend_from_slice(&(*options as i32).to_le_bytes());
        out.extend_from_slice(&(option_index as i32).to_le_bytes());
        Ok(out)
    }

    pub fn ni_data(&self) -> Result<Vec<u8>> {
        let mut ni = Vec::new();
        let format: i64 = self
            .format_version
            .get(1..)
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);

        // header section
        ni.extend_from_slice(&(format as u16).to_le_bytes());
        ni.extend_from_slice(&(self.pack_version as u16).to_le_bytes());
        ni.extend_from_slice(&(NI_HEADER_SIZE as u32).to_le_bytes());
        ni.extend_from_slice(&(NODE_SIZE as u32).to_le_bytes());
        ni.extend_from_slice(&(self.stage_nodes.len() as u32).to_le_bytes());
        ni.extend_from_slice(&(self.ri.len() as u32).to_le_bytes());
        ni.extend_from_slice(&(self.si.len() as u32).to_le_bytes());
        ni.push(1);
        ni.resize(NI_HEADER_SIZE, 0);

        // node section
        for snode in &self.stage_nodes {
            let mut node = Vec::with_capacity(NODE_SIZE);

            let index_of = |table: &IndexMap<String, (String, usize)>, key: &str| -> i32 {
                snode
                    .get(key)
                    .and_then(Value::as_str)
                    .and_then(|f| table.get(f))
                    .map(|(_, i)| *i as i32)
                    .unwrap_or(-1)
            };
            node.extend_from_slice(&index_of(&self.ri, "image").to_le_bytes());
            node.extend_from_slice(&index_of(&self.si, "audio").to_le_bytes());

            node.extend(self.transition(snode.get("okTransition"))?);
            node.extend(self.transition(snode.get("homeTransition"))?);

            if let Some(controls) = snode.get("controlSettings").filter(|c| c.is_object()) {
                for key in ["wheel", "ok", "home", "pause", "autoplay"] {
                    let v = controls.get(key).and_then(as_i64).unwrap_or(0);
                    node.extend_from_slice(&(v as u16).to_le_bytes());
                }
                node.extend_from_slice(&[0, 0]);
            }

            node.resize(NODE_SIZE, 0xAA);
            ni.extend(node);
        }

        Ok(ni)
    }

    pub fn li_data(&self) -> Vec<u8> {
        let mut li: Vec<u8> = self.li.iter().flat_map(|i| i.to_le_bytes()).collect();
        if li.len() < 8 {
            li.resize(8, 0);
        }
        li
    }
}

// ---------------------------------------------------------------------------
// Archive contents detection
// ---------------------------------------------------------------------------

fn any_ends_with(contents: &[String], pattern: &str) -> bool {
    contents.iter().any(|f| f.to_lowercase().ends_with(pattern))
}

pub fn story_is_studio(contents: &[String]) -> bool {
    contents.iter().any(|f| {
        [FILE_STUDIO_JSON, FILE_STUDIO_THUMB, "assets/"]
            .iter()
            .any(|p| f.contains(p))
    })
}

pub fn story_is_plain(contents: &[String]) -> bool {
    any_ends_with(contents, FILE_UUID)
}

pub fn story_is_flam(contents: &[String]) -> bool {
    any_ends_with(contents, ".lsf")
}

pub fn story_is_flam_plain(contents: &[String]) -> bool {
    any_ends_with(contents, ".lua")
}

pub fn story_is_lunii(contents: &[String]) -> bool {
    ["ri", "si", "li", "ni"]
        .iter()
        .all(|p| any_ends_with(contents, p))
}

pub fn story_is_lunii_plain(contents: &[String]) -> bool {
    ["ri.plain", "si.plain", "li.plain"]
        .iter()
        .all(|p| any_ends_with(contents, p))
}

pub fn archive_check_plain(path: &Path) -> Result<ArchiveType> {
    let archive = Archive::open_zip(path)?;
    let contents = archive.names();
    if !story_is_plain(&contents) {
        return Ok(ArchiveType::Unknown);
    }
    Ok(if story_is_flam_plain(&contents) {
        ArchiveType::FlamPlain
    } else if story_is_lunii_plain(&contents) {
        ArchiveType::LuniiPlain
    } else {
        ArchiveType::Unknown
    })
}

pub fn archive_check_zipcontent(path: &Path) -> Result<ArchiveType> {
    let archive = Archive::open_zip(path)?;
    let contents = archive.names();

    if story_is_flam(&contents) {
        return Ok(ArchiveType::FlamZip);
    }
    if story_is_lunii(&contents) {
        if let Some(bt) = contents.iter().find(|e| e.ends_with("bt")) {
            return Ok(if archive.size(bt) == Some(0x20) {
                ArchiveType::LuniiV3Zip
            } else {
                ArchiveType::LuniiV2Zip
            });
        }
        if contents.iter().any(|e| e.ends_with("key")) {
            return Ok(ArchiveType::LuniiFlamZip);
        }
        // no bt file nor key file, guessing based on ri
        let ri = contents.iter().find(|e| e.ends_with("ri")).unwrap();
        let ri_ciphered = archive.read(ri)?;
        let is_v2 = crypto::xxtea_decipher(&ri_ciphered, &lunii_generic_key(), 512)
            .map(|plain| plain.starts_with(b"000\\"))
            .unwrap_or(false);
        return Ok(if is_v2 {
            ArchiveType::LuniiV2Zip
        } else {
            ArchiveType::Unknown
        });
    }
    if story_is_studio(&contents) {
        return Ok(ArchiveType::StudioZip);
    }
    Ok(ArchiveType::Unknown)
}

pub fn archive_check_7zcontent(path: &Path) -> Result<ArchiveType> {
    let archive = Archive::open_7z(path)?;
    let contents = archive.names();
    Ok(if story_is_flam(&contents) {
        ArchiveType::FlamSevenZ
    } else if story_is_lunii(&contents) {
        ArchiveType::LuniiV27z
    } else if story_is_studio(&contents) {
        ArchiveType::StudioSevenZ
    } else {
        ArchiveType::Unknown
    })
}

/// Identifies an archive based on its name and contents.
pub fn archive_type(path: &Path, lunii_target: bool) -> Result<ArchiveType> {
    let lower = path.to_string_lossy().to_lowercase();
    Ok(if lower.ends_with(EXT_PK_PLAIN) {
        archive_check_plain(path)?
    } else if lunii_target && (lower.ends_with(EXT_PK_V2) || lower.ends_with(EXT_PK_V1)) {
        ArchiveType::LuniiV2Zip
    } else if lower.ends_with(EXT_ZIP) {
        archive_check_zipcontent(path)?
    } else if lower.ends_with(EXT_7Z) {
        archive_check_7zcontent(path)?
    } else if lower.ends_with(EXT_PK_VX) {
        archive_check_zipcontent(path)?
    } else {
        ArchiveType::Unknown
    })
}

/// Directory name holding the UUID in archives (`<uuid>/ri`, `<uuid>/`...).
pub fn uuid_dir_from_entry(name: &str) -> String {
    let parts: Vec<&str> = name.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    match parts.len() {
        0 => String::new(),
        1 => parts[0].to_string(),
        n => parts[n - 2].to_string(),
    }
}

/// Parses the UUID found in the first entry of an archive.
/// Returns the raw directory string and the parsed UUID.
pub fn uuid_from_archive(first_entry: &str) -> Result<(String, Uuid)> {
    let uuid_str = uuid_dir_from_entry(first_entry);
    if uuid_str.len() < 16 {
        bail!("UUID directory is missing in archive !");
    }
    let parsed = if !uuid_str.contains('-') {
        crypto::unhexlify(&uuid_str)
            .ok()
            .and_then(|b| Uuid::from_slice(&b).ok())
    } else {
        Uuid::parse_str(&uuid_str).ok()
    };
    match parsed {
        Some(u) => Ok((uuid_str, u)),
        None => bail!("UUID parse error : '{uuid_str}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn splitext() {
        assert_eq!(splitext_root("a/b.c/file.name.png"), "a/b.c/file.name");
        assert_eq!(splitext_root(".hidden"), ".hidden");
        assert_eq!(splitext_root("noext"), "noext");
        assert_eq!(normalized_name("0123456789abcdef.png"), "89ABCDEF");
        assert_eq!(normalized_name("abc.mp3"), "00000ABC");
    }

    #[test]
    fn matching() {
        let u = Uuid::parse_str("0123abcd-0000-1111-2222-333344445555").unwrap();
        assert!(uuid_match(&u, "44445555"));
        assert!(uuid_match(&u, "0123ABCD-0000"));
        assert!(!uuid_match(&u, "ffff"));
    }

    #[test]
    fn archive_uuid() {
        assert_eq!(uuid_dir_from_entry("0123/ri"), "0123");
        assert_eq!(uuid_dir_from_entry("0123/"), "0123");
        assert_eq!(uuid_dir_from_entry("abc/rf/000/X"), "000");
        let (_, u) = uuid_from_archive("0123456789ABCDEF0123456789ABCDEF/ri").unwrap();
        assert_eq!(short_uuid(&u), "89ABCDEF");
        assert!(uuid_from_archive("000/ri").is_err());
    }

    fn sample_studio() -> Value {
        json!({
            "format": "v1", "version": 2, "title": "Test", "description": "Desc",
            "nightModeAvailable": true,
            "stageNodes": [
                {"uuid": "11111111-1111-1111-1111-111111111111", "image": "cover.png", "audio": "title.mp3",
                 "okTransition": {"actionNode": "a1", "optionIndex": 0}, "homeTransition": null,
                 "controlSettings": {"wheel": true, "ok": true, "home": false, "pause": false, "autoplay": false}},
                {"uuid": "22222222-2222-2222-2222-222222222222", "image": null, "audio": "story.mp3",
                 "okTransition": null, "homeTransition": null,
                 "controlSettings": {"wheel": false, "ok": false, "home": true, "pause": true, "autoplay": true}}
            ],
            "actionNodes": [{"id": "a1", "options": ["22222222-2222-2222-2222-222222222222"]}]
        })
    }

    #[test]
    fn studio_conversion() {
        let s = StudioStory::load(&sample_studio()).unwrap();
        assert!(s.nm);
        assert_eq!(s.short_uuid(), "11111111");
        // short asset names are padded to the 8 characters the devices expect
        assert_eq!(s.ri_data(), b"000\\000COVER");
        assert_eq!(s.si_data(), b"000\\000TITLE000\\000STORY");
        assert_eq!(s.li_data(), vec![1, 0, 0, 0, 0, 0, 0, 0]);
        let ni = s.ni_data().unwrap();
        assert_eq!(ni.len(), NI_HEADER_SIZE + 2 * NODE_SIZE);
        let n0 = &ni[NI_HEADER_SIZE..NI_HEADER_SIZE + NODE_SIZE];
        assert_eq!(&n0[0..8], &[0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&n0[8..20], &[0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&n0[20..32], &[0xFF; 12]);
        assert_eq!(&n0[32..44], &[1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn move_up_down() {
        let ids: Vec<Uuid> = (1..=4u128).map(Uuid::from_u128).collect();
        let mut list: Vec<Story> = ids.iter().map(|u| Story::new(*u)).collect();
        let order = |l: &Vec<Story>| l.iter().map(|s| s.uuid.as_u128()).collect::<Vec<_>>();

        move_stories(&mut list, &[ids[2]], -1);
        assert_eq!(order(&list), vec![1, 3, 2, 4]);
        move_stories(&mut list, &[ids[2]], -2);
        assert_eq!(order(&list), vec![3, 1, 2, 4]);
        move_stories(&mut list, &[ids[2]], 2);
        assert_eq!(order(&list), vec![1, 2, 4, 3]);
        move_stories(&mut list, &[ids[0], ids[1]], 1);
        assert_eq!(order(&list), vec![4, 1, 2, 3]);
    }
}
