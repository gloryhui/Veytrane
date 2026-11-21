use std::path::{Component, PathBuf};

pub const MAX_PATHS: usize = 128;
pub const MAX_PATH_BYTES: usize = 32_768;
const MAX_BATCH_BYTES: usize = 1024 * 1024;

pub fn validate(paths: &[PathBuf]) -> Result<(), &'static str> {
    if paths.is_empty() || paths.len() > MAX_PATHS {
        return Err("每次拖入 1..128 个文件或目录");
    }
    let mut bytes = 0usize;
    for path in paths {
        let length = path.as_os_str().as_encoded_bytes().len();
        bytes = bytes.saturating_add(length);
        if length > MAX_PATH_BYTES || bytes > MAX_BATCH_BYTES {
            return Err("拖入路径过长或本批次路径总量超过 1 MiB");
        }
        if !path.is_absolute()
            || path.to_str().is_none()
            || path.file_name().is_none()
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err("拖入项必须是绝对 UTF-8 文件或目录路径，不支持父目录跳转");
        }
    }
    Ok(())
}
