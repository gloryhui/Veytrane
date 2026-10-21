//! Node-v2 negotiation and service control; one protocol for all node callers.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::{Error, Result};
use crate::protocol::frame::{read_raw_frame_limited, write_raw_frame_limited};

use crate::node::core::protocol;

pub const MAGIC: &[u8; 5] = b"P2PN\x01";
pub const VERSION: u16 = 2;
pub const MAX_FRAME_BYTES: u32 = 64 * 1024;
pub const MAX_SERVICES: usize = 128;
pub const CAP_NODE_BASE: u64 = 1 << 16;
pub const CAP_SERVICE_DIRECTORY: u64 = 1 << 17;
pub const CAP_RESOURCE_AUTHORIZATION: u64 = 1 << 18;
pub const REQUIRED: u64 = CAP_NODE_BASE
    | CAP_RESOURCE_AUTHORIZATION
    | protocol::CAP_REMOTE_AUTH
    | protocol::CAP_TRUSTED_DEVICE_AUTH;
pub const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(5);

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
    let mut result = REQUIRED;
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

async fn exchange<S: AsyncWrite + Unpin, R: AsyncRead + Unpin>(
    send: &mut S,
    recv: &mut R,
    local: u64,
) -> Result<u64> {
    if local & REQUIRED != REQUIRED {
        return Err(invalid("本机缺少必需节点安全能力"));
    }
    write(
        send,
        &NodeFrame::new(
            0,
            NodeMessage::Hello {
                capabilities: local,
            },
        ),
    )
    .await?;
    let remote = match read(recv).await?.message {
        NodeMessage::Hello { capabilities } if capabilities & REQUIRED == REQUIRED => capabilities,
        _ => return Err(invalid("对端缺少必需节点安全能力或协商顺序错误")),
    };
    write(send, &NodeFrame::new(0, NodeMessage::Ready)).await?;
    if read(recv).await?.message != NodeMessage::Ready {
        return Err(invalid("节点能力确认顺序错误"));
    }
    Ok(local & remote)
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
        let negotiated = exchange(&mut send, &mut recv, local).await?;
        send.finish().map_err(|_| invalid("节点协商流关闭失败"))?;
        Ok(negotiated)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_v2_bytes_and_request_ids_are_independent_of_current_encoder() {
        let vectors: &[(&[u8], u64, NodeMessage)] = &[
            (
                b"P2PN\x01\x02\x00\x00\x80\x82\x14",
                0,
                NodeMessage::Hello {
                    capabilities: 327936,
                },
            ),
            (b"P2PN\x01\x02\x00\x01", 0, NodeMessage::Ready),
            (b"P2PN\x01\x02\x07\x02", 7, NodeMessage::Services),
            (
                b"P2PN\x01\x02\x07\x03\x01\x04echo\x04Echo",
                7,
                NodeMessage::ServiceList {
                    services: vec![ServiceDescriptor {
                        id: "echo".into(),
                        name: "Echo".into(),
                    }],
                },
            ),
            (
                b"P2PN\x01\x02\x07\x04\x04echo",
                7,
                NodeMessage::OpenTcp {
                    service_id: "echo".into(),
                },
            ),
            (b"P2PN\x01\x02\x07\x05", 7, NodeMessage::TcpReady),
            (
                b"P2PN\x01\x02\x07\x06\x02",
                7,
                NodeMessage::Error {
                    code: NodeErrorCode::Unauthorized,
                },
            ),
        ];
        for (bytes, request, message) in vectors {
            let frame = NodeFrame::new(*request, message.clone());
            assert_eq!(frame.encode().unwrap(), *bytes);
            assert_eq!(NodeFrame::decode(bytes).unwrap(), frame);
            let mut trailing = bytes.to_vec();
            trailing.push(0);
            assert!(NodeFrame::decode(&trailing).is_err());
        }
    }

    #[test]
    fn rejects_legacy_unknown_versions_oversized_directory_and_invalid_services() {
        for bytes in [
            b"P2PD\x01\x01\x00\x01".as_slice(),
            b"P2PN\x01\x01\x00\x01",
            b"P2PN\x01\x02\x00\x02",
            b"P2PN\x01\x02\x07\x03\x81\x01",
        ] {
            assert!(NodeFrame::decode(bytes).is_err());
        }
        for id in ["", "../secret", "127.0.0.1:8080", "服务", "with space"] {
            assert!(
                NodeFrame::new(
                    1,
                    NodeMessage::OpenTcp {
                        service_id: id.into()
                    }
                )
                .encode()
                .is_err()
            );
        }
        let entry = ServiceDescriptor {
            id: "same".into(),
            name: "Name".into(),
        };
        assert!(
            NodeFrame::new(
                1,
                NodeMessage::ServiceList {
                    services: vec![entry.clone(), entry]
                }
            )
            .encode()
            .is_err()
        );
        let tcp_only = capabilities(false, false, true);
        assert_eq!(tcp_only & REQUIRED, REQUIRED);
        assert_eq!(tcp_only & protocol::CAP_FILE_TRANSFER, 0);
        assert_eq!(tcp_only & protocol::CAP_SPEED_EXECUTION, 0);
        assert_ne!(tcp_only & protocol::CAP_TCP_TUNNEL, 0);
    }

    #[tokio::test]
    async fn rejects_length_before_waiting_for_body_and_keeps_little_endian_outer_frame() {
        let (mut writer, mut reader) = tokio::io::duplex(128);
        use tokio::io::AsyncWriteExt;
        writer
            .write_all(&(MAX_FRAME_BYTES + 1).to_le_bytes())
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), read(&mut reader))
                .await
                .unwrap()
                .is_err()
        );
        let (mut writer, mut reader) = tokio::io::duplex(128);
        write(&mut writer, &NodeFrame::new(7, NodeMessage::Services))
            .await
            .unwrap();
        use tokio::io::AsyncReadExt;
        let mut bytes = [0u8; 12];
        reader.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"\x08\x00\x00\x00P2PN\x01\x02\x07\x02");
    }
}

#[cfg(test)]
mod negotiation_tests {
    use super::*;

    #[tokio::test]
    async fn capability_exchange_requires_both_hello_and_ready() {
        let (a, b) = tokio::io::duplex(4096);
        let (mut ar, mut aw) = tokio::io::split(a);
        let (mut br, mut bw) = tokio::io::split(b);
        let local = capabilities(true, true, true);
        let (left, right) = tokio::join!(
            exchange(&mut aw, &mut ar, local),
            exchange(&mut bw, &mut br, local)
        );
        assert_eq!(left.unwrap(), local);
        assert_eq!(right.unwrap(), local);
    }

    #[tokio::test]
    async fn unsupported_version_missing_security_capability_and_old_frames_are_rejected() {
        let all = capabilities(true, true, true);
        let mut responses = vec![
            b"P2PD\x01\x00\x00\x01\x7f".to_vec(),
            b"P2PN\x01\x01\x00\x01".to_vec(),
            NodeFrame::new(0, NodeMessage::Ready).encode().unwrap(),
        ];
        for bit in [
            CAP_NODE_BASE,
            CAP_RESOURCE_AUTHORIZATION,
            protocol::CAP_REMOTE_AUTH,
            protocol::CAP_TRUSTED_DEVICE_AUTH,
        ] {
            responses.push(
                NodeFrame::new(
                    0,
                    NodeMessage::Hello {
                        capabilities: all & !bit,
                    },
                )
                .encode()
                .unwrap(),
            );
        }
        for response in responses {
            let (a, mut b) = tokio::io::duplex(4096);
            let (mut recv, mut send) = tokio::io::split(a);
            let peer = async {
                read(&mut b).await.unwrap();
                write_raw_frame_limited(&mut b, &response, MAX_FRAME_BYTES)
                    .await
                    .unwrap();
            };
            let (result, ()) = tokio::join!(exchange(&mut send, &mut recv, all), peer);
            assert!(result.is_err());
        }
    }

    #[tokio::test]
    async fn schema_only_speed_capability_does_not_enable_execution() {
        let (a, b) = tokio::io::duplex(4096);
        let (mut ar, mut aw) = tokio::io::split(a);
        let (mut br, mut bw) = tokio::io::split(b);
        let restricted = REQUIRED | protocol::CAP_SPEED_OWNERSHIP;
        let (left, right) = tokio::join!(
            exchange(&mut aw, &mut ar, capabilities(true, true, true)),
            exchange(&mut bw, &mut br, restricted)
        );
        assert_eq!(left.unwrap(), restricted);
        assert_eq!(right.unwrap() & protocol::CAP_SPEED_EXECUTION, 0);
    }

    #[tokio::test]
    async fn repeated_hello_cannot_replace_ready_confirmation() {
        let all = capabilities(true, true, true);
        let (a, mut b) = tokio::io::duplex(4096);
        let (mut recv, mut send) = tokio::io::split(a);
        let peer = async {
            read(&mut b).await.unwrap();
            let hello = NodeFrame::new(0, NodeMessage::Hello { capabilities: all });
            write(&mut b, &hello).await.unwrap();
            assert_eq!(read(&mut b).await.unwrap().message, NodeMessage::Ready);
            write(&mut b, &hello).await.unwrap();
        };
        let (result, ()) = tokio::join!(exchange(&mut send, &mut recv, all), peer);
        assert!(result.unwrap_err().to_string().contains("顺序"));
    }
}
