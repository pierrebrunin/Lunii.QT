//! Misc tasks: STUdio DB import, downloads and application update check.

use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use base64::Engine;
use serde_json::{Map, Value};
use ureq::ResponseExt as _;
use uuid::Uuid;

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

/// Latest released version of Lunii (tag name), following GitHub redirection.
pub fn latest_version() -> Option<String> {
    let resp = http::agent(Duration::from_secs(5))
        .get(RELEASES_URL)
        .header("Referer", "Lunii")
        .call()
        .ok()?;
    resp.get_uri().path().rsplit('/').next().map(str::to_string)
}
