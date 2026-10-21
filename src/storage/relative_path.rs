//! Portable relative-name validation shared by storage and wire protocols.
use crate::error::{Error, Result};
fn invalid(message: &str) -> Error {
    Error::Protocol(message.into())
}

/// Only portable relative names selected by the sender cross this boundary.
pub fn validate_relative_path(path: &str) -> Result<()> {
    if path.is_empty() || path.len() > 4096 || path.split('/').count() > 64 {
        return Err(invalid("桌面相对路径长度或深度非法"));
    }
    for component in path.split('/') {
        if component.eq_ignore_ascii_case(".p2p-desktop")
            || component.is_empty()
            || component == "."
            || component == ".."
            || component.len() > 255
            || component.ends_with(['.', ' '])
            || component
                .chars()
                .any(|ch| ch.is_control() || "\\:<>\"|?*".contains(ch))
        {
            return Err(invalid("桌面相对路径包含非法组件"));
        }
        let stem = component
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ["COM", "LPT"].iter().any(|prefix| {
                stem.strip_prefix(prefix).is_some_and(|suffix| {
                    matches!(
                        suffix,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                })
            });
        if device {
            return Err(invalid("桌面相对路径包含保留设备名"));
        }
    }
    Ok(())
}
