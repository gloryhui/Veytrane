//! Stable public device addresses. These numbers never authenticate a device.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

pub const FIRST_SHORT_ID: u32 = 100_000_000;
pub const LAST_SHORT_ID: u32 = 999_999_999;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize)]
pub struct ShortId(u32);

impl<'de> Deserialize<'de> for ShortId {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = u32::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl ShortId {
    pub fn new(value: u32) -> Result<Self> {
        if !(FIRST_SHORT_ID..=LAST_SHORT_ID).contains(&value) {
            return Err(Error::Discovery(
                "设备 ID 必须是 100000000..999999999 的 9 位数字".into(),
            ));
        }
        Ok(Self(value))
    }

    pub fn normalize(input: &str) -> Result<Self> {
        // Only ASCII spaces/hyphens are presentation separators; no Unicode digits.
        if input.len() > 32
            || input
                .bytes()
                .any(|b| !b.is_ascii_digit() && b != b' ' && b != b'-')
        {
            return Err(Error::Discovery("设备 ID 格式无效".into()));
        }
        let digits: String = input
            .chars()
            .filter(|ch| *ch != ' ' && *ch != '-')
            .collect();
        if digits.len() != 9 {
            return Err(Error::Discovery("设备 ID 必须是 9 位数字".into()));
        }
        Self::new(
            digits
                .parse()
                .map_err(|_| Error::Discovery("设备 ID 格式无效".into()))?,
        )
    }

    pub fn value(self) -> u32 {
        self.0
    }

    pub fn display(self) -> String {
        let s = self.0.to_string();
        format!("{} {} {}", &s[..3], &s[3..6], &s[6..])
    }
}

impl std::fmt::Display for ShortId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
