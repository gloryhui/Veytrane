use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, thiserror::Error)]
pub enum ReceiveDirectoryError {
    #[error("接收目录不可用：{0}")]
    Unavailable(String),
    #[error("接收目录没有写权限：{0}")]
    Permission(String),
    #[error("文件系统操作失败：{0}")]
    Io(#[from] io::Error),
}

pub fn validate(path: &Path) -> Result<(), ReceiveDirectoryError> {
    let metadata = fs::metadata(path).map_err(|error| directory_error(path, error))?;
    if !metadata.is_dir() {
        return Err(ReceiveDirectoryError::Unavailable(format!(
            "{} 不是目录",
            path.display()
        )));
    }
    probe_write(path).map_err(|error| directory_error(path, error))
}

fn directory_error(path: &Path, error: io::Error) -> ReceiveDirectoryError {
    match error.kind() {
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => {
            ReceiveDirectoryError::Permission(format!("{}：{error}", path.display()))
        }
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => {
            ReceiveDirectoryError::Unavailable(format!("{}：{error}", path.display()))
        }
        _ => ReceiveDirectoryError::Io(error),
    }
}

fn probe_write(directory: &Path) -> io::Result<()> {
    for _ in 0..32 {
        let probe_path = directory.join(format!(
            ".p2p-file-write-probe-{:032x}.tmp",
            rand::random::<u128>()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        // Construct the cleanup guard before opening the file so unwinding
        // drops the file handle first (important on Windows), then removes it.
        let mut cleanup = ProbeFileCleanup {
            path: probe_path.clone(),
            armed: false,
        };
        let mut file = match options.open(&probe_path) {
            Ok(file) => {
                cleanup.armed = true;
                file
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };

        let write_result = file.write_all(b"p2p-file receive-directory probe\n");
        drop(file);
        let cleanup_result = cleanup.remove();
        return write_result.and(cleanup_result);
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "无法创建唯一的接收目录写能力探针",
    ))
}

struct ProbeFileCleanup {
    path: PathBuf,
    armed: bool,
}

impl ProbeFileCleanup {
    fn remove(&mut self) -> io::Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => {
                self.armed = false;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.armed = false;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

impl Drop for ProbeFileCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.remove();
        }
    }
}
