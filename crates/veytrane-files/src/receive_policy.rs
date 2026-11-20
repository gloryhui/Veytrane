//! Persisted receive-space policy, independent from file-system probing.
pub const DEFAULT_SAFETY_MIB: u16 = 64;
const MAX_SAFETY_MIB: u16 = 4096;

pub fn default_safety_mib() -> u16 {
    DEFAULT_SAFETY_MIB
}

pub fn validate_safety_mib(value: u16) -> Result<(), &'static str> {
    if value > MAX_SAFETY_MIB {
        Err("接收安全余量必须为 0..4096 MiB")
    } else {
        Ok(())
    }
}
