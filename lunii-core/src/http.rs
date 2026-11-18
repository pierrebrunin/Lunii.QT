//! Small helpers over `ureq` (v3).
//!
//! The body helpers read through `into_reader()`, which has no size limit:
//! `read_json()` / `read_to_vec()` stop at 10 MiB, too small for the official
//! database or firmwares.
//!
//! Network setup shared by every agent, for corporate networks:
//! - proxy from `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` (and `NO_PROXY`),
//!   or else, on Windows, the system proxy;
//! - certificates checked against the system trust store, not only the
//!   Mozilla roots built in: proxies that decrypt TLS (Netskope, Zscaler...)
//!   re-sign with their own root, installed in the system store.

use std::io::Read;
use std::sync::Once;
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use ureq::config::ConfigBuilder;
use ureq::tls::{RootCerts, TlsConfig};
use ureq::typestate::AgentScope;

pub type Response = ureq::http::Response<ureq::Body>;

/// Agent with a global timeout (connection + whole transfer).
pub fn agent(timeout: Duration) -> ureq::Agent {
    agent_with_ua(timeout, None)
}

/// Agent with a global timeout and an optional `User-Agent`.
pub fn agent_with_ua(timeout: Duration, user_agent: Option<&str>) -> ureq::Agent {
    let mut cfg = config().timeout_global(Some(timeout));
    if let Some(ua) = user_agent {
        cfg = cfg.user_agent(ua);
    }
    cfg.build().new_agent()
}

/// Common configuration: system certificates and proxy (see the module doc).
fn config() -> ConfigBuilder<AgentScope> {
    let cfg = ureq::Agent::config_builder().tls_config(
        TlsConfig::builder()
            .root_certs(RootCerts::PlatformVerifier)
            .build(),
    );
    // The proxy is read by ureq itself (default configuration).
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| match ureq::Proxy::try_from_env() {
        Some(p) => log::info!("HTTP proxy: {}:{}", p.host(), p.port()),
        None => log::debug!("no HTTP proxy"),
    });
    cfg
}

/// Agent for downloads: no limit on the whole transfer (big files on slow
/// connections), but an error if the server stops sending data.
pub fn download_agent() -> ureq::Agent {
    config()
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_per_read(Some(Duration::from_secs(30)))
        .build()
        .new_agent()
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
