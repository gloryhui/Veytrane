//! Pure file configuration policy shared by settings and file execution.

pub mod path_batch;

pub mod bandwidth {
    use serde::{Deserialize, Serialize};

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
    }
}

pub mod receive_policy {
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

    #[cfg(test)]
    mod tests {
        use super::*;

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
    }
}
