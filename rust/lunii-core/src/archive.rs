//! Uniform read access to zip and 7z story archives.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};

struct Entry {
    name: String,
    is_dir: bool,
    size: u64,
}

enum Inner {
    Zip(RefCell<zip::ZipArchive<File>>),
    SevenZ {
        path: PathBuf,
        contents: RefCell<Option<HashMap<String, Vec<u8>>>>,
    },
}

/// A story archive (zip, .pk or 7z).
pub struct Archive {
    entries: Vec<Entry>,
    inner: Inner,
}

impl Archive {
    /// Opens a zip archive (`.zip`, `.pk`, `.plain.pk`...).
    pub fn open_zip(path: &Path) -> Result<Self> {
        let file =
            File::open(path).with_context(|| format!("unable to open {}", path.display()))?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| anyhow!("Bad zip file: {e}"))?;
        let mut entries = Vec::with_capacity(zip.len());
        for i in 0..zip.len() {
            let f = zip.by_index_raw(i)?;
            entries.push(Entry {
                name: f.name()?.into_owned(),
                is_dir: f.is_dir(),
                size: f.size(),
            });
        }
        Ok(Self {
            entries,
            inner: Inner::Zip(RefCell::new(zip)),
        })
    }

    /// Opens a 7z archive. Contents are only decompressed on first read.
    pub fn open_7z(path: &Path) -> Result<Self> {
        let reader = sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())
            .map_err(|e| anyhow!("Bad 7z file: {e}"))?;
        let entries = reader
            .archive()
            .files
            .iter()
            .map(|f| Entry {
                name: f.name().to_string(),
                is_dir: f.is_directory(),
                size: f.size(),
            })
            .collect();
        Ok(Self {
            entries,
            inner: Inner::SevenZ {
                path: path.to_path_buf(),
                contents: RefCell::new(None),
            },
        })
    }

    /// Opens the archive according to its extension.
    pub fn open(path: &Path) -> Result<Self> {
        if is_7z(path) {
            Self::open_7z(path)
        } else {
            Self::open_zip(path)
        }
    }

    /// All entry names (directories included), in archive order.
    pub fn names(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.name.clone()).collect()
    }

    /// File entry names only, in archive order.
    pub fn file_names(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|e| !e.is_dir)
            .map(|e| e.name.clone())
            .collect()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.entries.iter().any(|e| e.name == name)
    }

    pub fn is_dir(&self, name: &str) -> bool {
        self.entries.iter().any(|e| e.name == name && e.is_dir)
    }

    /// Uncompressed size of an entry.
    pub fn size(&self, name: &str) -> Option<u64> {
        self.entries.iter().find(|e| e.name == name).map(|e| e.size)
    }

    /// Reads (and decompresses) an entry.
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        match &self.inner {
            Inner::Zip(zip) => {
                let mut zip = zip.borrow_mut();
                let mut f = zip
                    .by_name(name)
                    .with_context(|| format!("no '{name}' in archive"))?;
                let mut data = Vec::with_capacity(f.size() as usize);
                f.read_to_end(&mut data)?;
                Ok(data)
            }
            Inner::SevenZ { path, contents } => {
                if contents.borrow().is_none() {
                    *contents.borrow_mut() = Some(read_all_7z(path)?);
                }
                contents
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .get(name)
                    .cloned()
                    .with_context(|| format!("no '{name}' in archive"))
            }
        }
    }
}

fn read_all_7z(path: &Path) -> Result<HashMap<String, Vec<u8>>> {
    let mut reader = sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())
        .map_err(|e| anyhow!("Bad 7z file: {e}"))?;
    let mut out = HashMap::new();
    reader
        .for_each_entries(|entry, r| {
            if !entry.is_directory() {
                let mut data = Vec::with_capacity(entry.size() as usize);
                r.read_to_end(&mut data)?;
                out.insert(entry.name().to_string(), data);
            }
            Ok(true)
        })
        .map_err(|e| anyhow!("7z read error: {e}"))?;
    Ok(out)
}

pub fn is_7z(path: &Path) -> bool {
    path.to_string_lossy()
        .to_lowercase()
        .ends_with(crate::constants::EXT_7Z)
}
