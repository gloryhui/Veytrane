//! TLS session channel binding for the identity handshake.

use std::fmt;

use crate::error::{Error, Result};

/// 导出会话绑定值时使用的 TLS exporter 标签（RFC 5705）。
pub const CHANNEL_BINDING_LABEL: &[u8] = b"p2p_file/channel-binding/v1";
/// 会话绑定值长度。32 字节足够，且不会触发 exporter 的长度限制。
pub const CHANNEL_BINDING_LEN: usize = 32;

/// 当前 QUIC/TLS 会话的通道绑定值。
///
/// 绑定值由 [`quinn::Connection::export_keying_material`] 从当前会话派生，
/// 不同 TLS 会话导出各自独立的伪随机值。中间人无法把一条连接上的握手消息
/// 原样搬到另一条连接：两侧签名覆盖的绑定值不同，验签必然失败。
///
/// 字段私有，正常代码只能经 [`ChannelBinding::from_connection`] 构造。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ChannelBinding([u8; CHANNEL_BINDING_LEN]);

impl ChannelBinding {
    /// 从当前 QUIC/TLS 会话导出绑定值。正常路径唯一公开构造入口。
    pub fn from_connection(connection: &quinn::Connection) -> Result<Self> {
        let mut binding = [0u8; CHANNEL_BINDING_LEN];
        connection
            .export_keying_material(&mut binding, CHANNEL_BINDING_LABEL, b"")
            .map_err(|_| {
                Error::Transport("导出 TLS 会话绑定值失败：TLS 握手尚未完成或连接已失效".into())
            })?;
        Ok(Self(binding))
    }

    pub fn as_bytes(&self) -> &[u8; CHANNEL_BINDING_LEN] {
        &self.0
    }
}

impl fmt::Debug for ChannelBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ChannelBinding(<32 bytes>)")
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ChannelBinding {
    pub fn from_bytes_for_test(bytes: [u8; CHANNEL_BINDING_LEN]) -> Self {
        Self(bytes)
    }
}
