//! Application settings, in `~/.lunii/settings.json`.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Map, Value};

use crate::constants::{cfg_dir, ensure_cfg_dir};
use crate::store;

fn settings_file() -> PathBuf {
    cfg_dir().join("settings.json")
}

fn load() -> Map<String, Value> {
    store::read_json(&settings_file()).unwrap_or_default()
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
    ensure_cfg_dir()?;
    let path = settings_file();
    let _lock = store::lock(&path)?;
    let mut settings = load();
    settings.insert(name.into(), value);
    store::write_atomic(&path, &serde_json::to_vec_pretty(&settings)?)?;
    Ok(())
}
