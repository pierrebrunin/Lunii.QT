//! Application settings, in `~/.lunii/settings.json`.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::constants::{cfg_dir, ensure_cfg_dir};

fn load() -> Map<String, Value> {
    std::fs::read(cfg_dir().join("settings.json"))
        .ok()
        .and_then(|d| serde_json::from_slice(&d).ok())
        .unwrap_or_default()
}

/// A boolean setting (`false` if unset).
pub fn get_bool(name: &str) -> bool {
    load().get(name).and_then(Value::as_bool).unwrap_or(false)
}

pub fn set_bool(name: &str, value: bool) -> Result<()> {
    set(name, value.into())
}

/// A number setting (`None` if unset).
pub fn get_number(name: &str) -> Option<f64> {
    load().get(name).and_then(Value::as_f64)
}

pub fn set_number(name: &str, value: f64) -> Result<()> {
    set(name, value.into())
}

fn set(name: &str, value: Value) -> Result<()> {
    let mut settings = load();
    settings.insert(name.into(), value);
    let dir = ensure_cfg_dir()?;
    std::fs::write(
        dir.join("settings.json"),
        serde_json::to_vec_pretty(&settings)?,
    )?;
    Ok(())
}
