//! Small helpers over `ureq` (v3).
//!
//! The body helpers read through `into_reader()`, which has no size limit:
//! `read_json()` / `read_to_vec()` stop at 10 MiB, too small for the official
//! database or firmwares.

use std::io::Read;
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

pub type Response = ureq::http::Response<ureq::Body>;

/// Agent with a global timeout (connection + whole transfer).
pub fn agent(timeout: Duration) -> ureq::Agent {
    agent_with_ua(timeout, None)
}

/// Agent with a global timeout and an optional `User-Agent`.
pub fn agent_with_ua(timeout: Duration, user_agent: Option<&str>) -> ureq::Agent {
    let mut cfg = ureq::Agent::config_builder().timeout_global(Some(timeout));
    if let Some(ua) = user_agent {
        cfg = cfg.user_agent(ua);
    }
    cfg.build().new_agent()
}

/// Parses the whole body as JSON.
pub fn json(resp: Response) -> Result<Value> {
    Ok(serde_json::from_reader(resp.into_body().into_reader())?)
}

/// Reads the whole body.
pub fn bytes(resp: Response) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    resp.into_body().into_reader().read_to_end(&mut data)?;
    Ok(data)
}

/// `Content-Length` header, if any.
pub fn content_length(resp: &Response) -> Option<u64> {
    resp.headers()
        .get("Content-Length")?
        .to_str()
        .ok()?
        .parse()
        .ok()
}
