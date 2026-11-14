//! Optional navigation UI and HTTP management adapter, hosted by the Agent.
//! It owns presentation state only; all networking mutations use the Agent API.
pub mod manager;
pub mod platform;
mod web;

use serde_json::Value;
use std::{future::Future, net::SocketAddr, path::Path, pin::Pin};

pub use web::serve;

#[derive(Clone, Debug)]
pub struct Problem {
    pub message: String,
    pub code: String,
    pub uncertain: bool,
    pub details: Option<Value>,
}
pub type Result<T> = std::result::Result<T, Problem>;
pub type Call<'a> = Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

impl Problem {
    pub fn new(message: impl Into<String>) -> Self {
        Self::code(message, "invalid_request")
    }
    pub fn code(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: code.into(),
            uncertain: false,
            details: None,
        }
    }
    pub fn unknown(message: impl Into<String>) -> Self {
        Self {
            uncertain: true,
            ..Self::code(message, "outcome_unknown")
        }
    }
}
impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Problem {}
impl From<std::io::Error> for Problem {
    fn from(error: std::io::Error) -> Self {
        Self::new(error.to_string())
    }
}
impl From<serde_json::Error> for Problem {
    fn from(error: serde_json::Error) -> Self {
        Self::new(error.to_string())
    }
}

/// Production uses the existing IPC endpoint; tests supply an isolated backend.
pub trait Backend: Send + Sync {
    fn call<'a>(&'a self, command: Value, remote: bool, request_id: Option<&'a str>) -> Call<'a>;
    fn target_probe<'a>(&'a self, service: &'a str) -> Call<'a>;
    /// Persist the receipt and reply before the host performs a supervised restart.
    fn restart<'a>(&'a self) -> Call<'a>;
}

pub fn id() -> String {
    format!("{:032x}", rand::random::<u128>())
}

pub fn stamp() -> String {
    // Gregorian UTC, without invoking platform commands or adding a clock runtime.
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (seconds / 86400) as i64 + 719468;
    let era = days / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds % 86400 / 3600,
        seconds % 3600 / 60,
        seconds % 60
    )
}

pub fn address(value: &str, loopback: bool) -> Result<SocketAddr> {
    let parsed: SocketAddr = value
        .parse()
        .map_err(|_| Problem::new("请填写有效的 IP:端口，IPv6 请使用 [IP]:端口。"))?;
    if parsed.port() == 0
        || parsed.ip().is_unspecified()
        || parsed.ip().is_multicast()
        || (loopback && !parsed.ip().is_loopback())
    {
        return Err(Problem::new("监听地址必须是有效的本机回环 IP:端口。"));
    }
    Ok(parsed)
}

pub fn write_private(path: &Path, value: &Value) -> Result<()> {
    write_private_bytes(path, &serde_json::to_vec_pretty(value)?)
}
pub fn write_private_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Problem::new("导航文件缺少父目录。"))?;
    crate::storage::permissions::ensure_directory(parent)?;
    crate::storage::atomic::write_bytes(path, bytes)?;
    Ok(())
}
