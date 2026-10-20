use super::task_model::TaskId;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{io, path::PathBuf};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const API_VERSION: u16 = 1;
pub const FRAME_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub api_version: u16,
    pub request_id: String,
    pub timeout_ms: u32,
    pub command: Command,
}
impl Request {
    pub fn new(command: Command) -> Self {
        Self {
            api_version: API_VERSION,
            request_id: format!("{:032x}", rand::random::<u128>()),
            timeout_ms: 30000,
            command,
        }
    }
    pub fn validate(&self) -> io::Result<()> {
        if self.api_version != API_VERSION {
            return Err(io::Error::other("api_version_unsupported"));
        }
        if self.request_id.is_empty()
            || self.request_id.len() > 64
            || !self
                .request_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err(io::Error::other("invalid_request_id"));
        }
        if self.timeout_ms == 0 || self.timeout_ms > 30000 {
            return Err(io::Error::other("invalid_timeout"));
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Status,
    Config,
    Tasks,
    Events {
        epoch: String,
        after: u64,
    },
    Services {
        peer: String,
    },
    Connect {
        peer: String,
        password: Option<String>,
    },
    Disconnect {
        peer: String,
    },
    Send {
        peer: String,
        paths: Vec<PathBuf>,
    },
    Pause {
        task: TaskId,
    },
    Resume {
        peer: String,
        task: TaskId,
    },
    Cancel {
        task: TaskId,
    },
    StartForward {
        rule: String,
    },
    StopForward {
        rule: String,
    },
    ApplyConfig {
        expected_revision: u64,
        settings: serde_json::Value,
    },
    SetPassword {
        expected_revision: u64,
        password: String,
    },
    Stop,
}
impl Command {
    pub fn is_query(&self) -> bool {
        matches!(
            self,
            Self::Status | Self::Config | Self::Tasks | Self::Events { .. } | Self::Services { .. }
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub api_version: u16,
    pub request_id: String,
    pub result: Option<serde_json::Value>,
    pub error: Option<ApiError>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}
impl Response {
    pub fn ok(id: &str, result: serde_json::Value) -> Self {
        Self {
            api_version: API_VERSION,
            request_id: id.into(),
            result: Some(result),
            error: None,
        }
    }
    pub fn error(id: &str, code: &str, message: impl ToString) -> Self {
        Self {
            api_version: API_VERSION,
            request_id: id.into(),
            result: None,
            error: Some(ApiError {
                code: code.into(),
                message: message.to_string(),
            }),
        }
    }
}
pub async fn read<T: DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin + ?Sized),
) -> io::Result<T> {
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > FRAME_LIMIT {
        return Err(io::Error::other("IPC frame too large or empty"));
    }
    let mut bytes = vec![0u8; length];
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}
pub async fn write<T: Serialize>(
    stream: &mut (impl AsyncWrite + Unpin + ?Sized),
    value: &T,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > FRAME_LIMIT {
        return Err(io::Error::other("IPC response too large; narrow the query"));
    }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await
}
