//! Helpers shared by Lunii and Flam devices.

use std::fs::File;
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context as _, Result};
use unicode_normalization::UnicodeNormalization;
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;

use crate::report::{Context, check_abort};

/// Makes a story title usable as a file name (ASCII only, reserved chars replaced).
pub fn secure_filename(filename: &str) -> String {
    const INVALID_FILE_CHARS: &str = "/\\?%*:|\"<>";
    filename
        .nfkd()
        .map(|c| match c {
            '\u{0141}' => 'L',
            '\u{0142}' => 'l',
            c if INVALID_FILE_CHARS.contains(c) => '_',
            c => c,
        })
        .filter(char::is_ascii)
        .collect()
}

/// Thousands separator like Python's `{:,}`.
pub fn group_thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Writes `data` in 10KB chunks, reporting speed and progress every 250ms.
pub fn write_with_progress(ctx: &Context, target: &Path, data: &[u8]) -> Result<()> {
    const TIME_SPAN_S: f64 = 0.250;
    const BLOCK_SIZE: usize = 10 * 1024;

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let fname = target
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    let total = data.len() as u64;

    let mut f =
        File::create(target).with_context(|| format!("unable to create {}", target.display()))?;
    let mut written = 0u64;
    let mut last_written = 0u64;
    let mut last_emit = Instant::now();

    for chunk in data.chunks(BLOCK_SIZE) {
        f.write_all(chunk)?;
        written += chunk.len() as u64;
        ctx.add_written(chunk.len() as u64);
        let elapsed = last_emit.elapsed().as_secs_f64();
        if elapsed >= TIME_SPAN_S || written == total {
            let speed = if elapsed > 0.0 {
                ((written - last_written) as f64 / elapsed) as u64 / 1024
            } else {
                0
            };
            ctx.file_progress(&format!("{} KB/s", group_thousands(speed)), written, total);
            ctx.debug(format!(
                "Progress on {fname} - {} / {} Bytes ( {} KB/s )",
                group_thousands(written),
                group_thousands(total),
                group_thousands(speed)
            ));
            last_emit = Instant::now();
            last_written = written;
        }
    }
    f.flush()?;
    Ok(())
}

/// Writes a whole file, creating its parent directory.
pub fn write_file(target: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(target, data).with_context(|| format!("unable to write {}", target.display()))
}

/// Names of the sub-directories of `dir` (`None` if `dir` does not exist).
pub fn list_subdirs(dir: &Path) -> Option<Vec<String>> {
    let rd = std::fs::read_dir(dir).ok()?;
    Some(
        rd.filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect(),
    )
}

/// Total size of the files inside `dir`.
pub fn dir_size(dir: &Path) -> u64 {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// All the files under `dir`, as (absolute path, relative path with `/` separators).
pub fn walk_files(dir: &Path) -> Vec<(PathBuf, String)> {
    WalkDir::new(dir)
        .sort_by_file_name()
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            let rel = e
                .path()
                .strip_prefix(dir)
                .unwrap_or(e.path())
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join("/");
            (e.into_path(), rel)
        })
        .collect()
}

/// Free space available on the file system holding `path`.
pub fn free_space(path: &Path) -> Result<u64> {
    Ok(fs4::available_space(path)?)
}

/// Removes the given directories (if they exist).
pub fn remove_dirs(dirs: &[PathBuf]) -> Result<()> {
    for d in dirs {
        if d.is_dir() {
            std::fs::remove_dir_all(d)
                .with_context(|| format!("unable to remove {}", d.display()))?;
        }
    }
    Ok(())
}

/// Moves `src` directory inside `dst_parent` (like `shutil.move(src, dst_parent)`).
pub fn move_dir_into(src: &Path, dst_parent: &Path) -> Result<()> {
    std::fs::create_dir_all(dst_parent)?;
    let name = src.file_name().context("invalid source directory")?;
    let dst = dst_parent.join(name);
    if dst.exists() {
        anyhow::bail!("Destination path '{}' already exists", dst.display());
    }
    if std::fs::rename(src, &dst).is_err() {
        // cross device move: copy then delete
        copy_dir(src, &dst)?;
        std::fs::remove_dir_all(src)?;
    }
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Zip writer (stored, like Python's `zipfile.ZipFile(path, 'w')`), removing the
/// partial file if not finished.
pub struct StoryZip {
    path: PathBuf,
    writer: Option<zip::ZipWriter<File>>,
}

impl StoryZip {
    pub fn create(path: &Path) -> Result<Self> {
        let file = File::create(path)
            .with_context(|| format!("failed to create ZIP - {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            writer: Some(zip::ZipWriter::new(file)),
        })
    }

    pub fn add(&mut self, name: &str, data: &[u8]) -> Result<()> {
        let w = self.writer.as_mut().unwrap();
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(data.len() as u64 >= 0xFFFF_FFFF);
        w.start_file(name, options)?;
        w.write_all(data)?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<PathBuf> {
        let mut f = self.writer.take().unwrap().finish()?;
        f.flush()?;
        let _ = f.stream_position();
        Ok(self.path.clone())
    }
}

impl Drop for StoryZip {
    fn drop(&mut self) {
        if self.writer.take().is_some() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Iterates over a list, reporting story progress and checking abort requests.
pub fn progress_iter<'a, T>(
    ctx: &'a Context,
    short_uuid: &'a str,
    items: &'a [T],
) -> impl Iterator<Item = Result<(usize, &'a T)>> + 'a {
    items.iter().enumerate().map(move |(i, item)| {
        ctx.story_progress(short_uuid, i, items.len());
        check_abort(ctx)?;
        Ok((i, item))
    })
}

/// Python's `str.removesuffix`.
pub fn remove_suffix<'a>(s: &'a str, suffix: &str) -> &'a str {
    s.strip_suffix(suffix).unwrap_or(s)
}

/// Splits a `/` path into (dirname, basename) like `os.path` (posix).
pub fn split_path(file: &str) -> (&str, &str) {
    match file.rfind('/') {
        Some(p) => {
            let dir = file[..p].trim_end_matches('/');
            (
                if dir.is_empty() && p > 0 {
                    &file[..1]
                } else {
                    dir
                },
                &file[p + 1..],
            )
        }
        None => ("", file),
    }
}

pub fn join_path(dir: &str, base: &str) -> String {
    if dir.is_empty() {
        base.to_string()
    } else if dir.ends_with('/') {
        format!("{dir}{base}")
    } else {
        format!("{dir}/{base}")
    }
}

/// Python slicing `s[n:]` on an ASCII-ish string.
pub fn skip_chars(s: &str, n: usize) -> String {
    s.chars().skip(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secure() {
        assert_eq!(
            secure_filename("Le Château: été / hiver?"),
            "Le Chateau_ ete _ hiver_"
        );
        assert_eq!(secure_filename("Łódź"), "Lodz");
    }

    #[test]
    fn thousands() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(1234567), "1,234,567");
        assert_eq!(group_thousands(123), "123");
    }

    #[test]
    fn paths() {
        assert_eq!(split_path("rf/000/abc"), ("rf/000", "abc"));
        assert_eq!(split_path("abc"), ("", "abc"));
        assert_eq!(join_path("", "abc"), "abc");
    }
}
