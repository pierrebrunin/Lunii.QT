//! Misc tasks of the Python worker: STUdio DB import, ffmpeg install,
//! downloads and application update check.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use base64::Engine;
use serde_json::{Map, Value};
use ureq::ResponseExt as _;
use uuid::Uuid;

use crate::constants::*;
use crate::db;
use crate::http;
use crate::report::{Context, check_abort};

pub const RELEASES_URL: &str = "https://github.com/o-daneel/Lunii.QT/releases/latest";

/// Imports a STUdio library database (`unofficial.json` like file).
/// Returns (imported, total entries).
pub fn import_studio_db(ctx: &Context, path: &Path) -> Result<(usize, usize)> {
    let stories: Map<String, Value> =
        serde_json::from_slice(&std::fs::read(path)?).context("invalid JSON file")?;
    if stories.contains_key("response") {
        bail!("🛑 Failed to import DB (wrong file ?)");
    }

    let mut count = 0;
    for (index, entry) in stories.values().enumerate() {
        check_abort(ctx)?;
        ctx.story_progress("", index, stories.len());

        let uuid = entry.get("uuid").and_then(Value::as_str);
        let title = entry.get("title").and_then(Value::as_str);
        let Some(uuid) = uuid.and_then(|u| Uuid::parse_str(u).ok()) else {
            continue;
        };
        db::thirdparty_add_story(
            &uuid,
            title,
            entry.get("description").and_then(Value::as_str),
        )?;

        if let Some(image) = entry.get("image").and_then(Value::as_str)
            && let Some((_, encoded)) = image.split_once(',')
            && let Ok(data) = base64::engine::general_purpose::STANDARD.decode(encoded)
        {
            db::thirdparty_add_thumb(&uuid, &data)?;
        }
        count += 1;
    }
    Ok((count, stories.len()))
}

/// Downloads `url` to `target`, reporting progress.
pub fn download(ctx: &Context, url: &str, target: &Path, label: &str) -> Result<u64> {
    let resp = http::download_agent().get(url).call()?;
    let total = http::content_length(&resp).unwrap_or(0);
    let mut reader = resp.into_body().into_reader();
    let mut out = std::fs::File::create(target)?;
    let mut buf = vec![0u8; 100 * 1024];
    let mut downloaded = 0u64;
    loop {
        check_abort(ctx)?;
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        downloaded += n as u64;
        ctx.file_progress(label, downloaded, total);
    }
    Ok(downloaded)
}

/// Downloads ffmpeg into the configuration directory (Windows / macOS only).
pub fn install_ffmpeg(ctx: &Context) -> Result<PathBuf> {
    let url = if cfg!(windows) {
        "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-lgpl.zip"
    } else if cfg!(target_os = "macos") {
        "https://evermeet.cx/ffmpeg/getrelease/zip"
    } else if cfg!(target_os = "linux") {
        bail!(
            "You must rely on your Linux package manager to install FFMPEG (e.g., on Ubuntu/Debian: sudo apt install ffmpeg)"
        );
    } else {
        bail!("Unsupported OS.");
    };

    let tmp = std::env::temp_dir().join(format!("ffmpeg_download_{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let archive_path = tmp.join("ffmpeg.zip");

    ctx.info(format!(
        "Downloading FFmpeg for {}...",
        std::env::consts::OS
    ));
    let result = (|| -> Result<PathBuf> {
        download(ctx, url, &archive_path, "FFMPEG")?;
        ctx.info("FFmpeg archive downloaded, extracting...");

        let mut zip = zip::ZipArchive::new(std::fs::File::open(&archive_path)?)?;
        let cfg = ensure_cfg_dir()?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            let base = entry
                .name()?
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();
            if base == "ffmpeg" || base == "ffmpeg.exe" {
                let target = cfg.join(&base);
                let mut out = std::fs::File::create(&target)?;
                std::io::copy(&mut entry, &mut out)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
                }
                return Ok(target);
            }
        }
        bail!("🛑 Failed to extract ffmpeg executable")
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// Latest released version of Lunii (tag name), following GitHub redirection.
pub fn latest_version() -> Option<String> {
    let resp = http::agent(Duration::from_secs(5))
        .get(RELEASES_URL)
        .header("Referer", "Lunii")
        .call()
        .ok()?;
    resp.get_uri().path().rsplit('/').next().map(str::to_string)
}
