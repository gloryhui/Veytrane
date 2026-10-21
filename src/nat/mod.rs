//! NAT 穿透：STUN 查询、类型判定与打洞。主动网关端口映射尚未实现。

pub mod classify;
pub mod punch;
pub mod stun;

pub use classify::{
    FilteringBehavior, MappingBehavior, MappingEvidence, MappingProbe, StunObservation,
    classify_mapping, observe, probe_rfc5780,
};
pub use punch::{PunchConfig, PunchToken, simultaneous_open, simultaneous_open_any};
pub use stun::{StunResult, query_binding, query_binding_with_change, resolve_server};
