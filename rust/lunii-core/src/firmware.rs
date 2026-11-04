//! Firmware download from Lunii servers (port of `pkg/api/firmware.py`).

use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde_json::json;

use crate::constants::*;
use crate::http::{self, Response};

/// Anonymous fake Lunii created to get v1 / v2 firmwares.
const V1V2_FAHID: &str = "-NnUun90mQ56GosDyA3R";
const USER_AGENT: &str = "unirest-java/3.1.00";

/// Authentication headers obtained from the Luniistore.
#[derive(Debug, Clone)]
pub struct AuthToken {
    token: String,
}

fn agent() -> ureq::Agent {
    http::agent_with_ua(Duration::from_secs(10), Some(USER_AGENT))
}

fn get(auth: &AuthToken, url: &str) -> Result<Response> {
    Ok(agent()
        .get(url)
        .header("x-auth-token", &auth.token)
        .header("authorization", &format!("Bearer {}", auth.token))
        .call()?)
}

/// Logs into the Luniistore. `Ok(None)` if the credentials are refused.
pub fn luniistore_get_authtoken(login: &str, password: &str) -> Result<Option<AuthToken>> {
    let resp = http::agent(Duration::from_secs(30))
        .post("https://server-auth-prod.lunii.com/auth/signin")
        .send_json(
            json!({"application": "luniistore_mobile", "email": login, "password": password}),
        );
    let resp = match resp {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(_)) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let body = http::json(resp)?;
    let token = body["response"]["tokens"]["access_tokens"]["user"]["server"]
        .as_str()
        .context("no token in Luniistore response")?;
    Ok(Some(AuthToken {
        token: token.to_string(),
    }))
}

fn vid_pid(version: DeviceVersion) -> (u16, u16) {
    if version == DeviceVersion::LuniiV1 {
        FAH_V1_FW_2_USB_VID_PID
    } else {
        FAH_V2_V3_USB_VID_PID
    }
}

/// Current firmware version of v1 / v2 devices (`fa` or `fu` file), like `"2_22"`.
pub fn luniiv1_fw_version(
    version: DeviceVersion,
    auth: &AuthToken,
    fu_upgrade: bool,
) -> Result<Option<String>> {
    if !version.is_lunii_v1_v2() {
        return Ok(None);
    }
    let (vid, pid) = vid_pid(version);
    let url = format!(
        "https://server-user-prod.lunii.com/v2/fah/{V1V2_FAHID}/update/current?vendor_id={vid:04x}&product_id={pid:04x}"
    );
    let body = http::json(get(auth, &url)?)?;
    let versions = &body["response"]["currentUpdate"];
    let key = if fu_upgrade {
        "fu_version"
    } else {
        "fa_version"
    };
    Ok(versions
        .get(key)
        .filter(|v| !v.is_null())
        .map(|v| format!("{}_{}", v["major"], v["minor"])))
}

/// Firmware files available for a device (default file names).
pub fn device_fw_getlist(
    version: DeviceVersion,
    snu_str: &str,
    auth: &AuthToken,
) -> Result<Vec<String>> {
    let name = |v: Option<String>| v.unwrap_or_else(|| "None".into());
    Ok(match version {
        DeviceVersion::LuniiV1 => vec![
            format!(
                "fa.v{}.bin",
                name(luniiv1_fw_version(version, auth, false)?)
            ),
            format!("fu.v{}.bin", name(luniiv1_fw_version(version, auth, true)?)),
        ],
        DeviceVersion::LuniiV2 | DeviceVersion::LuniiV1or2Unknown => {
            vec![format!(
                "fa.v{}.bin",
                name(luniiv1_fw_version(version, auth, false)?)
            )]
        }
        DeviceVersion::LuniiV3 => vec![format!("fa.{snu_str}.v3xx.bin")],
        DeviceVersion::FlamV1 => vec!["update-main.enc".into(), "update-comm.enc".into()],
        DeviceVersion::Undefined => vec![],
    })
}

fn save(resp: Response, target: &Path) -> Result<u64> {
    let data = http::bytes(resp)?;
    std::fs::write(target, &data)?;
    Ok(data.len() as u64)
}

/// Downloads a firmware to `target`. `second_fw` selects `fu` (v1) or `comm` (Flam).
/// Returns the downloaded size.
pub fn device_fw_download(
    version: DeviceVersion,
    snu_str: &str,
    auth: &AuthToken,
    target: &Path,
    second_fw: bool,
) -> Result<u64> {
    match version {
        v if v.is_lunii_v1_v2() => {
            let (vid, pid) = vid_pid(version);
            let url = format!(
                "https://server-user-prod.lunii.com/v2/fah/{V1V2_FAHID}/update?vendor_id={vid:04x}&product_id={pid:04x}"
            );
            let body = http::json(get(auth, &url)?)?;
            let key = if second_fw { "fu_file" } else { "fa_file" };
            let Some(url) = body["response"]["update"][key]["url"].as_str() else {
                bail!("no {key} available for this device");
            };
            save(agent().get(url).call()?, target)
        }
        DeviceVersion::LuniiV3 => {
            let url = format!(
                "https://server-backend-prod.lunii.com/devices/{snu_str}/firmware?installed=3.1.2"
            );
            save(get(auth, &url)?, target)
        }
        DeviceVersion::FlamV1 => {
            let chipset = if second_fw { "comm" } else { "main" };
            let url = format!(
                "https://server-backend-prod.lunii.com/devices/{snu_str}/firmware?installed=1.1.1-01&chipset={chipset}"
            );
            save(get(auth, &url)?, target)
        }
        _ => bail!("unsupported device"),
    }
}
