//! Shared loopback HTTP authentication and token-file management.

use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use axum::{
    Json,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;
use tracing::warn;
use veytrane_transport::error::{Error, Result};

/// A compact HTTP authentication failure with a client-safe response body.
#[derive(Debug)]
pub struct TokenError {
    pub status: StatusCode,
    pub message: String,
}

impl TokenError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "需要有效的管理 token".into(),
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }
}

impl IntoResponse for TokenError {
    fn into_response(self) -> Response {
        let body = Json(json!({
            "error": self.message,
            "details": [],
        }));
        let mut response = (self.status, body).into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

pub fn authorize_token(headers: &HeaderMap, expected: &str) -> std::result::Result<(), TokenError> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| TokenError::bad_request("缺少 Host 请求头"))?;
    if !host_allowed(host) {
        return Err(TokenError::forbidden("Host 不受信任"));
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let origin = origin
            .to_str()
            .map_err(|_| TokenError::forbidden("Origin 非法"))?;
        if origin != format!("http://{host}") {
            return Err(TokenError::forbidden("Origin 与 Host 不匹配"));
        }
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(TokenError::unauthorized)?
        .trim();
    if !constant_time_eq(token.as_bytes(), expected.as_bytes()) {
        return Err(TokenError::unauthorized());
    }
    Ok(())
}

fn host_allowed(host: &str) -> bool {
    // Allow any local port so an SSH tunnel may map a different local port.
    // The hostname itself must stay loopback; DNS rebinding uses a foreign name.
    let hostname = if host.starts_with('[') {
        host.split_once(']')
            .map(|(name, _)| format!("{name}]"))
            .unwrap_or_else(|| host.to_string())
    } else {
        host.rsplit_once(':')
            .map(|(name, _)| name.to_string())
            .unwrap_or_else(|| host.to_string())
    };
    matches!(hostname.as_str(), "127.0.0.1" | "localhost" | "[::1]")
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (a, b) in left.iter().zip(right) {
        difference |= a ^ b;
    }
    difference == 0
}

/// Read a token file, creating a random token with `0600` permissions if absent.
pub fn load_or_create_token(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => {
            let token = text.trim().to_string();
            if token.len() < 32 {
                return Err(Error::Protocol(format!(
                    "管理 token 文件 {} 内容过短（至少 32 字符）",
                    path.display()
                )));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(metadata) = fs::metadata(path)
                    && metadata.permissions().mode() & 0o077 != 0
                {
                    warn!(
                        path = %path.display(),
                        "管理 token 文件权限过宽，建议 chmod 600"
                    );
                }
            }
            Ok(token)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                fs::create_dir_all(parent)?;
            }
            let token = hex::encode(rand::random::<[u8; 32]>());
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(path)?;
            file.write_all(token.as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            Ok(token)
        }
        Err(error) => Err(Error::Io(io::Error::new(
            error.kind(),
            format!("读取管理 token 文件 {} 失败: {error}", path.display()),
        ))),
    }
}
