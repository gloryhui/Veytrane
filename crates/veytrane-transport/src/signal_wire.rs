//! Shared signaling wire messages, candidates, and signature payloads.

use crate::error::Result;
use crate::identity::NodeId;
use crate::punch::PunchToken;
use crate::short_id::ShortId;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// 信令协议版本。
///
/// - v1：`Register { node_id, public_key, candidates }`，没有任何认证。
/// - v2：challenge-response 注册认证，单连接单身份。
pub const SIGNAL_PROTOCOL_VERSION: u32 = 2;
/// Opt-in extension; legacy v2 wire/signatures stay unchanged.
pub const SHORT_ID_PROTOCOL_VERSION: u32 = 3;
/// Desktop signaling with signed, peer-scoped recovery candidates.
pub const RECOVERY_PROTOCOL_VERSION: u32 = 4;
const RECOVERY_DOMAIN: &[u8] = b"p2p_file/signal-recovery-lookup/v1";

/// 注册 challenge 长度（256 bit）。
pub const CHALLENGE_LEN: usize = 32;

/// 注册签名的域标签，避免签名被挪用到别处。
pub const REGISTER_DOMAIN: &[u8] = b"p2p_file/signal-register/v2";

/// 单个节点允许公布的候选地址数量上限。
pub const MAX_CANDIDATES: usize = 32;

/// 候选地址的类型，决定了它的优先级。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum CandidateKind {
    /// 服务器反射地址（STUN 看到的公网映射）。
    ServerReflexive,
    /// Relay 兜底候选；实际使用须经过认证 UDP admission，不代表实现完整 TURN。
    Relay,
    /// 用端口预测猜出来的地址，只在对称型 NAT 下才用。
    Predicted,
    /// 直连地址（局域网或本机地址）。
    Host,
}

impl CandidateKind {
    /// 数字越小越优先尝试。
    pub fn priority(self) -> u32 {
        match self {
            Self::ServerReflexive => 0,
            Self::Host => 1,
            Self::Predicted => 2,
            Self::Relay => 3,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ServerReflexive => "公网映射",
            Self::Relay => "中继",
            Self::Predicted => "预测",
            Self::Host => "本机/局域网",
        }
    }
}

/// 一个候选地址。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub kind: CandidateKind,
    pub addr: SocketAddr,
}

impl Candidate {
    pub fn new(kind: CandidateKind, addr: SocketAddr) -> Self {
        Self { kind, addr }
    }

    /// 公网映射候选（STUN 或手动指定）。
    pub fn reflexive(addr: SocketAddr) -> Self {
        Self::new(CandidateKind::ServerReflexive, addr)
    }

    /// 本机/局域网候选。
    pub fn host(addr: SocketAddr) -> Self {
        Self::new(CandidateKind::Host, addr)
    }
}

impl std::fmt::Display for Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}（{}）", self.addr, self.kind.as_str())
    }
}

/// 把候选地址按尝试顺序排好。
pub fn sort_candidates(candidates: &mut [Candidate]) {
    candidates.sort_by_key(|candidate| {
        let family_bonus = if candidate.addr.is_ipv6() { 0 } else { 1 };
        (candidate.kind.priority(), family_bonus)
    });
}

/// 去重（同类型同地址只留一个）。
pub fn dedup_candidates(candidates: &mut Vec<Candidate>) {
    let mut seen = Vec::new();
    candidates.retain(|candidate| {
        if seen.contains(candidate) {
            false
        } else {
            seen.push(*candidate);
            true
        }
    });
}

/// 信令消息。
///
/// 变体顺序就是 postcard 的线格式，**不要重排**。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignalMessage {
    Register {
        node_id: NodeId,
        public_key: [u8; 32],
        candidates: Vec<Candidate>,
        signature: Vec<u8>,
    },
    Registered,
    Lookup {
        node_id: NodeId,
    },
    PeerCandidates {
        node_id: NodeId,
        candidates: Vec<Candidate>,
        token: PunchToken,
    },
    PeerPending {
        node_id: NodeId,
    },
    Ping,
    Pong,
    Error {
        reason: String,
    },
    RegisterHello {
        protocol_version: u32,
        node_id: NodeId,
        public_key: [u8; 32],
    },
    RegisterChallenge {
        protocol_version: u32,
        challenge: [u8; CHALLENGE_LEN],
    },
    RegisteredShort {
        short_id: ShortId,
    },
    LookupShort {
        short_id: ShortId,
    },
    ShortResolved {
        short_id: ShortId,
        node_id: Option<NodeId>,
    },
    RecoveryLookup {
        node_id: NodeId,
        candidates: Vec<Candidate>,
        sequence: u64,
        signature: Vec<u8>,
    },
}

impl SignalMessage {
    pub fn encode(&self) -> Result<Vec<u8>> {
        Ok(postcard::to_allocvec(self)?)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(postcard::from_bytes(bytes)?)
    }

    /// 便于日志阅读的简短名字。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::RegisterHello { .. } => "RegisterHello",
            Self::RegisterChallenge { .. } => "RegisterChallenge",
            Self::Register { .. } => "Register",
            Self::Registered => "Registered",
            Self::RegisteredShort { .. } => "RegisteredShort",
            Self::LookupShort { .. } => "LookupShort",
            Self::ShortResolved { .. } => "ShortResolved",
            Self::RecoveryLookup { .. } => "RecoveryLookup",
            Self::Lookup { .. } => "Lookup",
            Self::PeerCandidates { .. } => "PeerCandidates",
            Self::PeerPending { .. } => "PeerPending",
            Self::Ping => "Ping",
            Self::Pong => "Pong",
            Self::Error { .. } => "Error",
        }
    }
}

/// 注册签名载荷。
///
/// `REGISTER_DOMAIN + challenge + node_id + public_key + blake3(postcard(candidates))`
pub fn register_payload(
    challenge: &[u8; CHALLENGE_LEN],
    node_id: NodeId,
    public_key: &[u8; 32],
    candidates: &[Candidate],
) -> Result<Vec<u8>> {
    let mut payload = Vec::with_capacity(REGISTER_DOMAIN.len() + CHALLENGE_LEN + 16 + 32 + 32);
    payload.extend_from_slice(REGISTER_DOMAIN);
    payload.extend_from_slice(challenge);
    payload.extend_from_slice(node_id.as_bytes());
    payload.extend_from_slice(public_key);
    let encoded = postcard::to_allocvec(candidates)?;
    payload.extend_from_slice(blake3::hash(&encoded).as_bytes());
    Ok(payload)
}

#[doc(hidden)]
pub fn recovery_payload(
    challenge: &[u8; CHALLENGE_LEN],
    target: NodeId,
    candidates: &[Candidate],
    sequence: u64,
) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    payload.extend_from_slice(RECOVERY_DOMAIN);
    payload.extend_from_slice(challenge);
    payload.extend_from_slice(target.as_bytes());
    payload.extend_from_slice(&sequence.to_be_bytes());
    payload.extend_from_slice(blake3::hash(&postcard::to_allocvec(candidates)?).as_bytes());
    Ok(payload)
}
