//! Safe file writes, for the files of the application (`~/.lunii`) and the
//! small files of the devices (indexes, settings).
//!
//! - [`write_atomic`]: a file is written next to its target, synced, then
//!   renamed over it. Interrupted (crash), the target keeps its previous
//!   content instead of being truncated. Not on a device: see
//!   [`write_in_place`].
//! - [`write_in_place`]: a small file of a device is rewritten where it is on
//!   the disk.
//! - [`read_json`]: an unreadable file is set aside (`<name>.corrupted`) with
//!   a warning, instead of being treated as empty and overwritten.
//! - [`lock`]: serializes the read-modify-write cycles of a file, between the
//!   threads of the application and between processes (interface and CLI).

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

/// `<path><suffix>`, next to `path`.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Writes `data` to `path` without ever leaving it truncated: the previous
/// content stays until the new one is complete.
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = sibling(path, ".tmp");
    let result = (|| {
        let mut file = File::create(&tmp)?;
        file.write_all(data)?;
        // on the disk before the rename: otherwise a crash may leave the
        // renamed file empty
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        // the rename itself (best effort: not possible on every system)
        #[cfg(unix)]
        if let Some(dir) = path.parent()
            && let Ok(dir) = File::open(if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                dir
            })
        {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Rewrites `path` (a small file of a device: index, settings) where it is
/// on the disk, then syncs it.
///
/// A rename would put the new content in blocks the storyteller may still
/// believe free: a Flam rebuilding its `library.cache` overwrote the index
/// written this way. Rewritten in place, a file of up to one cluster (4 KB:
/// about 110 stories in an index) keeps its block, nothing is allocated.
pub fn write_in_place(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.write_all(data)?;
    file.set_len(data.len() as u64)?;
    file.sync_all()
}

/// Sets an unreadable file aside, as `<name>.corrupted` (replacing an older
/// one), so that it can be looked at or restored instead of being lost.
pub fn set_aside(path: &Path) {
    let aside = sibling(path, ".corrupted");
    match std::fs::rename(path, &aside) {
        Ok(()) => log::warn!(
            target: "lunii",
            "Unreadable file {}, kept as {}",
            path.display(),
            aside.display()
        ),
        Err(e) => log::warn!(
            target: "lunii",
            "Unreadable file {}, unable to keep it: {e}",
            path.display()
        ),
    }
}

/// Reads the JSON file `path`: `None` if it is missing, or if it is
/// unreadable (then set aside, see [`set_aside`]).
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => {
            log::warn!(target: "lunii", "Unable to read {}: {e}", path.display());
            return None;
        }
    };
    match serde_json::from_slice(&data) {
        Ok(value) => Some(value),
        Err(_) => {
            set_aside(path);
            None
        }
    }
}

/// Exclusive access to a file for a read-modify-write cycle, released on
/// drop. Held on `<path>.lock`: the file itself is replaced by
/// [`write_atomic`].
#[must_use]
pub struct FileLock(#[allow(dead_code)] File);

/// Waits for the exclusive access to `path` (other threads, other processes).
pub fn lock(path: &Path) -> io::Result<FileLock> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(sibling(path, ".lock"))?;
    file.lock()?;
    Ok(FileLock(file))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn atomic_write_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        // no temporary file left
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("a.json")]);
    }

    /// Same file (same inode), new content, shorter or longer.
    #[test]
    fn in_place_write_keeps_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("list");
        write_in_place(&path, b"first content, quite long").unwrap();
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            || std::fs::metadata(&path).unwrap().ino()
        };
        #[cfg(unix)]
        let before = inode();
        write_in_place(&path, b"short").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"short");
        write_in_place(&path, b"longer than before").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"longer than before");
        #[cfg(unix)]
        assert_eq!(inode(), before, "rewritten, not replaced");
    }

    #[test]
    fn corrupted_json_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("podcasts.json");
        assert_eq!(read_json::<serde_json::Value>(&path), None);
        std::fs::write(&path, b"{\"trunc").unwrap();
        assert_eq!(read_json::<serde_json::Value>(&path), None);
        assert!(!path.exists());
        assert_eq!(
            std::fs::read(dir.path().join("podcasts.json.corrupted")).unwrap(),
            b"{\"trunc"
        );
        std::fs::write(&path, b"{\"a\":1}").unwrap();
        assert_eq!(
            read_json::<serde_json::Value>(&path),
            Some(serde_json::json!({"a": 1}))
        );
    }

    /// Two threads incrementing a counter in a file: no update lost.
    #[test]
    fn lock_serializes_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(dir.path().join("counter"));
        write_atomic(&path, b"0").unwrap();
        let barrier = Arc::new(Barrier::new(4));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (path, barrier) = (path.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..25 {
                        let _guard = lock(&path).unwrap();
                        let n: u32 = std::fs::read_to_string(&*path).unwrap().parse().unwrap();
                        write_atomic(&path, (n + 1).to_string().as_bytes()).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(std::fs::read_to_string(&*path).unwrap(), "100");
    }
}
