//! File settings and their pure validation rules.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const MAX_RECEIVE_ROOT_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileLimits {
    pub upload_kib: u32,
    pub download_kib: u32,
}

impl FileLimits {
    pub fn validate(self) -> Result<(), &'static str> {
        if self.upload_kib > 1_048_576 || self.download_kib > 1_048_576 {
            Err("文件限速须为 0～1048576 KiB/s；0 表示不限速")
        } else {
            Ok(())
        }
    }

    pub fn parse(upload: &str, download: &str) -> Result<Self, &'static str> {
        let limits = Self {
            upload_kib: upload
                .trim()
                .parse()
                .map_err(|_| "上传限速须为非负整数 KiB/s")?,
            download_kib: download
                .trim()
                .parse()
                .map_err(|_| "下载限速须为非负整数 KiB/s")?,
        };
        limits.validate()?;
        Ok(limits)
    }
}

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

pub fn validate_send_concurrency(value: u8) -> Result<(), &'static str> {
    if (1..=3).contains(&value) {
        Ok(())
    } else {
        Err("发送并发数仅支持 1、2 或 3")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileSettings {
    pub receive_root: PathBuf,
    pub auto_resume: bool,
    pub send_concurrency: u8,
    pub receive_safety_mib: u16,
    pub file_limits: FileLimits,
}

impl FileSettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.receive_root.is_absolute()
            || self.receive_root.as_os_str().len() > MAX_RECEIVE_ROOT_BYTES
        {
            return Err("接收根必须是绝对路径");
        }
        validate_send_concurrency(self.send_concurrency)?;
        validate_safety_mib(self.receive_safety_mib)?;
        self.file_limits.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_validation_has_explicit_units_and_zero_is_unlimited() {
        assert_eq!(FileLimits::parse("0", " 1024 ").unwrap().download_kib, 1024);
        for bad in ["-1", "", "1.5", "1048577", "4294967296"] {
            assert!(FileLimits::parse(bad, "0").is_err());
        }
    }

    #[test]
    fn default_and_inclusive_upper_bound_are_stable() {
        assert_eq!(DEFAULT_SAFETY_MIB, 64);
        assert_eq!(default_safety_mib(), DEFAULT_SAFETY_MIB);
        assert!(validate_safety_mib(0).is_ok());
        assert!(validate_safety_mib(MAX_SAFETY_MIB).is_ok());
        assert_eq!(
            validate_safety_mib(MAX_SAFETY_MIB + 1),
            Err("接收安全余量必须为 0..4096 MiB")
        );
    }

    #[test]
    fn send_concurrency_accepts_only_existing_one_to_three_range() {
        for value in 1..=3 {
            assert!(validate_send_concurrency(value).is_ok());
        }
        for value in [0, 4] {
            assert_eq!(
                validate_send_concurrency(value),
                Err("发送并发数仅支持 1、2 或 3")
            );
        }
    }

    #[test]
    fn file_settings_reuse_bounded_file_rules() {
        let root = std::env::temp_dir();
        let mut settings = FileSettings {
            receive_root: root.clone(),
            auto_resume: true,
            send_concurrency: 1,
            receive_safety_mib: DEFAULT_SAFETY_MIB,
            file_limits: FileLimits::default(),
        };
        assert!(settings.validate().is_ok());

        settings.receive_root = PathBuf::from("relative");
        assert_eq!(settings.validate(), Err("接收根必须是绝对路径"));

        settings.receive_root = root.join("x".repeat(MAX_RECEIVE_ROOT_BYTES + 1));
        assert_eq!(settings.validate(), Err("接收根必须是绝对路径"));

        settings.receive_root = root;
        settings.send_concurrency = 4;
        assert_eq!(settings.validate(), Err("发送并发数仅支持 1、2 或 3"));
    }
}
