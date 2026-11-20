//! Optional verified TLS configuration for signaling.

use crate::error::{Error, Result};
#[cfg(feature = "signal-tls-server")]
use quinn::rustls::pki_types::PrivateKeyDer;
use quinn::rustls::{
    self,
    pki_types::{CertificateDer, ServerName, pem::PemObject},
};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};
#[cfg(feature = "signal-tls-server")]
use tokio_rustls::TlsAcceptor;
use tokio_rustls::TlsConnector;

const MAX_PEM_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignalTlsClient {
    /// None uses bundled Mozilla roots; Some uses only the specified PEM roots.
    pub ca_file: Option<PathBuf>,
    /// None verifies the host from HOST:PORT (including IP SANs).
    pub server_name: Option<String>,
}

#[derive(Clone, Debug)]
#[cfg(feature = "signal-tls-server")]
pub struct SignalTlsServer {
    pub certificate: PathBuf,
    pub private_key: PathBuf,
}

fn invalid(message: &'static str) -> Error {
    Error::Discovery(message.into())
}

fn read_pem(path: &Path) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    let metadata = std::fs::metadata(path).map_err(|_| invalid("读取信令 TLS PEM 文件失败"))?;
    if !metadata.is_file() || metadata.len() > MAX_PEM_BYTES {
        return Err(invalid("信令 TLS PEM 必须是最大 4 MiB 的普通文件"));
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    std::fs::File::open(path)
        .map_err(|_| invalid("打开信令 TLS PEM 文件失败"))?
        .take(MAX_PEM_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("读取信令 TLS PEM 文件失败"))?;
    if bytes.len() as u64 > MAX_PEM_BYTES {
        return Err(invalid("信令 TLS PEM 文件超过大小上限"));
    }
    Ok(bytes)
}

fn certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let bytes = read_pem(path)?;
    let certificates = CertificateDer::pem_slice_iter(&bytes)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| invalid("信令 TLS 证书 PEM 格式无效"))?;
    if certificates.is_empty() {
        return Err(invalid("信令 TLS PEM 未包含证书"));
    }
    Ok(certificates)
}

pub fn server_name(server: &str, override_name: Option<&str>) -> Result<ServerName<'static>> {
    let inferred;
    let name = if let Some(name) = override_name {
        name
    } else {
        let (host, port) = server
            .rsplit_once(':')
            .ok_or_else(|| invalid("TLS 信令地址须为 HOST:PORT"))?;
        if port.parse::<u16>().is_err() || host.is_empty() {
            return Err(invalid("TLS 信令地址或端口无效"));
        }
        inferred = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host)
            .to_owned();
        &inferred
    };
    if name.is_empty() || name.len() > 253 || name.chars().any(char::is_whitespace) {
        return Err(invalid("信令 TLS 校验主机名无效"));
    }
    ServerName::try_from(name.to_owned())
        .map_err(|_| invalid("信令 TLS 校验主机名须为 DNS 名称或 IP"))
}

impl SignalTlsClient {
    #[doc(hidden)]
    pub fn prepare(&self, server: &str) -> Result<(TlsConnector, ServerName<'static>)> {
        let name = server_name(server, self.server_name.as_deref())?;
        let mut roots = rustls::RootCertStore::empty();
        if let Some(path) = &self.ca_file {
            for certificate in certificates(path)? {
                roots
                    .add(certificate)
                    .map_err(|_| invalid("信令 TLS CA 证书无效"))?;
            }
        } else {
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        }
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| invalid("信令 TLS 协议配置无效"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok((TlsConnector::from(Arc::new(config)), name))
    }
}

#[cfg(feature = "signal-tls-server")]
impl SignalTlsServer {
    #[doc(hidden)]
    pub fn prepare(&self) -> Result<TlsAcceptor> {
        let chain = certificates(&self.certificate)?;
        let bytes = read_pem(&self.private_key)?;
        let key = PrivateKeyDer::from_pem_slice(&bytes)
            .map_err(|_| invalid("信令 TLS 私钥 PEM 格式无效"))?;
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| invalid("信令 TLS 协议配置无效"))?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|_| invalid("信令 TLS 证书与私钥不匹配或无效"))?;
        Ok(TlsAcceptor::from(Arc::new(config)))
    }
}
