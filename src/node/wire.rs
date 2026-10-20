//! Unified node-v1 negotiation and service control. Legacy codecs stay separate.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::{Error, Result};
use crate::protocol::frame::{read_raw_frame_limited, write_raw_frame_limited};

use super::protocol;

pub const MAGIC: &[u8; 5] = b"P2PN\x01";
pub const VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: u32 = 64 * 1024;
pub const MAX_SERVICES: usize = 128;
pub const CAP_NODE_BASE: u64 = 1 << 16;
pub const CAP_SERVICE_DIRECTORY: u64 = 1 << 17;
pub const CAP_RESOURCE_AUTHORIZATION: u64 = 1 << 18;
pub const REQUIRED: u64 = CAP_NODE_BASE | CAP_RESOURCE_AUTHORIZATION | protocol::CAP_REMOTE_AUTH;
pub const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerWireMode {
    #[default]
    UnifiedV1,
    LegacyDesktopV1,
    LegacyCliV3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum StreamKind {
    File = 1,
    Service = 2,
    Speed = 3,
}
impl TryFrom<u8> for StreamKind {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::File),
            2 => Ok(Self::Service),
            3 => Ok(Self::Speed),
            _ => Err(invalid("未知节点业务流类型")),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum NodeErrorCode {
    UnsupportedVersion,
    UnsupportedCapability,
    Unauthorized,
    UnknownService,
    Busy,
    InvalidRequest,
    TargetUnavailable,
    Internal,
}

impl NodeErrorCode {
    pub fn message(self) -> &'static str {
        match self {
            Self::UnsupportedVersion => "节点协议版本不兼容",
            Self::UnsupportedCapability => "节点未启用所需能力",
            Self::Unauthorized => "没有操作或资源权限",
            Self::UnknownService => "服务不存在或不可访问",
            Self::Busy => "节点资源已达上限",
            Self::InvalidRequest => "节点请求无效",
            Self::TargetUnavailable => "服务目标暂不可用",
            Self::Internal => "节点无法完成请求",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServiceDescriptor {
    pub id: String,
    pub name: String,
}

/// Append only. Postcard discriminants are frozen by NODE_WIRE.md.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum NodeMessage {
    Hello {
        capabilities: u64,
    },
    Ready,
    Services,
    ServiceList {
        #[serde(deserialize_with = "bounded_services")]
        services: Vec<ServiceDescriptor>,
    },
    OpenTcp {
        service_id: String,
    },
    TcpReady,
    Error {
        code: NodeErrorCode,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NodeFrame {
    pub version: u16,
    pub request_id: u64,
    pub message: NodeMessage,
}

impl NodeFrame {
    pub fn new(request_id: u64, message: NodeMessage) -> Self {
        Self {
            version: VERSION,
            request_id,
            message,
        }
    }
    fn validate(&self) -> Result<()> {
        if self.version != VERSION {
            return Err(invalid("节点协议版本不兼容"));
        }
        let negotiation = matches!(self.message, NodeMessage::Hello { .. } | NodeMessage::Ready);
        if negotiation != (self.request_id == 0) {
            return Err(invalid("节点请求编号无效"));
        }
        match &self.message {
            NodeMessage::OpenTcp { service_id } => validate_service_id(service_id)?,
            NodeMessage::ServiceList { services } => {
                if services.len() > MAX_SERVICES {
                    return Err(invalid("节点服务目录超限"));
                }
                let mut ids = std::collections::HashSet::new();
                for entry in services {
                    validate_service_id(&entry.id)?;
                    if !ids.insert(&entry.id)
                        || entry.name.trim().is_empty()
                        || entry.name.chars().count() > 128
                        || entry.name.chars().any(char::is_control)
                    {
                        return Err(invalid("节点服务目录包含重复或无效条目"));
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = MAGIC.to_vec();
        bytes.extend(postcard::to_allocvec(self)?);
        if bytes.len() > MAX_FRAME_BYTES as usize {
            return Err(invalid("节点控制帧超限"));
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_FRAME_BYTES as usize {
            return Err(invalid("节点控制帧超限"));
        }
        let payload = bytes
            .strip_prefix(MAGIC)
            .ok_or_else(|| invalid("不支持的节点控制格式"))?;
        let (frame, trailing): (Self, _) = postcard::take_from_bytes(payload)?;
        if !trailing.is_empty() {
            return Err(invalid("节点控制帧包含多余字节"));
        }
        frame.validate()?;
        Ok(frame)
    }
}

fn bounded_services<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<ServiceDescriptor>, D::Error> {
    struct Services;
    impl<'de> serde::de::Visitor<'de> for Services {
        type Value = Vec<ServiceDescriptor>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("最多 128 项的服务目录")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            if sequence.size_hint().is_some_and(|n| n > MAX_SERVICES) {
                return Err(serde::de::Error::custom("服务目录超限"));
            }
            let mut entries =
                Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(MAX_SERVICES));
            while let Some(entry) = sequence.next_element()? {
                if entries.len() == MAX_SERVICES {
                    return Err(serde::de::Error::custom("服务目录超限"));
                }
                entries.push(entry);
            }
            Ok(entries)
        }
    }
    deserializer.deserialize_seq(Services)
}

pub fn validate_service_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(invalid(
            "服务 ID 必须为 1..64 个 ASCII 字母、数字、-、_ 或 .",
        ));
    }
    Ok(())
}

pub async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> Result<NodeFrame> {
    let bytes = read_raw_frame_limited(reader, MAX_FRAME_BYTES)
        .await?
        .ok_or_else(|| invalid("节点控制流已关闭"))?;
    NodeFrame::decode(&bytes)
}
pub async fn write<W: AsyncWrite + Unpin>(writer: &mut W, frame: &NodeFrame) -> Result<()> {
    write_raw_frame_limited(writer, &frame.encode()?, MAX_FRAME_BYTES).await
}

pub fn capabilities(files: bool, speed: bool, tcp: bool) -> u64 {
    let mut result = REQUIRED | protocol::CAP_TRUSTED_DEVICE_AUTH;
    if files {
        result |= protocol::CAP_TASK_PROTOCOL
            | protocol::CAP_RELATIVE_ENTRIES
            | protocol::CAP_PAUSE_RESUME
            | protocol::CAP_FILE_TRANSFER
            | protocol::CAP_DIRECTORY_TRANSFER
            | protocol::CAP_FILE_FLOW
            | protocol::CAP_AUTO_RESUME
            | protocol::CAP_INCREMENTAL_DIRECTORY;
    }
    if speed {
        result |= protocol::CAP_SPEED_OWNERSHIP | protocol::CAP_SPEED_EXECUTION
    }
    if tcp {
        result |= protocol::CAP_TCP_TUNNEL | CAP_SERVICE_DIRECTORY
    }
    result
}

pub async fn negotiate(connection: &quinn::Connection, initiator: bool, local: u64) -> Result<u64> {
    let result = tokio::time::timeout(NEGOTIATION_TIMEOUT, async {
        if local & REQUIRED != REQUIRED {
            return Err(invalid("本机缺少必需节点安全能力"));
        }
        let (mut send, mut recv) = if initiator {
            connection.open_bi().await
        } else {
            connection.accept_bi().await
        }
        .map_err(|_| invalid("节点协商流不可用"))?;
        write(
            &mut send,
            &NodeFrame::new(
                0,
                NodeMessage::Hello {
                    capabilities: local,
                },
            ),
        )
        .await?;
        let remote = match read(&mut recv).await?.message {
            NodeMessage::Hello { capabilities } if capabilities & REQUIRED == REQUIRED => {
                capabilities
            }
            _ => return Err(invalid("对端缺少必需节点安全能力或协商顺序错误")),
        };
        write(&mut send, &NodeFrame::new(0, NodeMessage::Ready)).await?;
        if read(&mut recv).await?.message != NodeMessage::Ready {
            return Err(invalid("节点能力确认顺序错误"));
        }
        send.finish().map_err(|_| invalid("节点协商流关闭失败"))?;
        Ok(local & remote)
    })
    .await;
    match result {
        Ok(Ok(capabilities)) => Ok(capabilities),
        Ok(Err(error)) => {
            connection.close(3u32.into(), b"node protocol rejected");
            Err(error)
        }
        Err(_) => {
            connection.close(3u32.into(), b"node negotiation timeout");
            Err(invalid("节点协议协商超时"))
        }
    }
}

fn invalid(message: &str) -> Error {
    Error::Protocol(message.into())
}
