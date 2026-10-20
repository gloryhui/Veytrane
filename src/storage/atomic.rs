//! File replacement with an explicit distinction between pre/post commit errors.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

use serde::Serialize;

#[derive(Default)]
pub(crate) struct WriteOutcome {
    /// Replacement succeeded. A directory sync failure must not cause rollback.
    pub durability_warning: Option<String>,
}

pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<WriteOutcome> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    let mut bytes = bytes;
    bytes.push(b'\n');
    write_bytes(path, &bytes)
}

pub(crate) fn write_bytes(path: &Path, bytes: &[u8]) -> io::Result<WriteOutcome> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata)
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || is_reparse(&metadata) =>
        {
            return Err(io::Error::other("配置目标必须是普通文件，不能是链接"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = parent.join(format!(".p2p-config-{}.tmp", rand::random::<u128>()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace(&temporary, path)
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(WriteOutcome {
        durability_warning: sync_directory(parent)
            .err()
            .map(|error| format!("配置已提交，但目录持久化确认失败：{error}")),
    })
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

pub(crate) fn regular_file(metadata: &std::fs::Metadata) -> bool {
    metadata.is_file() && !metadata.file_type().is_symlink() && !is_reparse(metadata)
}

#[cfg(not(windows))]
fn is_reparse(_metadata: &std::fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(windows)]
fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    let encode = |path: &Path| -> io::Result<Vec<u16>> {
        let mut wide: Vec<_> = path.as_os_str().encode_wide().collect();
        if wide.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "文件路径包含 NUL",
            ));
        }
        wide.push(0);
        Ok(wide)
    };
    let source = encode(source)?;
    let destination = encode(destination)?;
    // Both buffers are NUL terminated and remain alive through the call.
    let replaced = unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 0x1 | 0x8) };
    if replaced == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
