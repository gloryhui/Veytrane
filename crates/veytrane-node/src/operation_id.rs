//! Canonical operation identifiers shared by files, speed tests, and management.

use serde::{Deserialize, Serialize};

/// An operation ID failed its canonical hexadecimal validation.
#[derive(Debug, thiserror::Error)]
#[error("task ID must be 32 lowercase hexadecimal characters")]
pub struct InvalidTaskId;

/// Stable, random 128-bit operation identifier serialized as lowercase hex.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TaskId(String);

impl TaskId {
    /// Generates a new operation identifier.
    pub fn generate() -> Self {
        Self(hex::encode(rand::random::<[u8; 16]>()))
    }

    /// Parses the canonical representation.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidTaskId`] when `value` is not 32 lowercase hex characters.
    pub fn parse(value: &str) -> Result<Self, InvalidTaskId> {
        Self::try_from(value.to_owned())
    }

    /// Borrows the canonical representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for TaskId {
    type Error = InvalidTaskId;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 32
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(InvalidTaskId);
        }
        Ok(Self(value))
    }
}

impl From<TaskId> for String {
    fn from(value: TaskId) -> Self {
        value.0
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
