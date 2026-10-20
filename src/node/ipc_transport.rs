//! Local authenticated byte transport. The protocol never listens on TCP.
use super::agent_paths::AgentPaths;
use std::io;

pub trait LocalStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> LocalStream for T {}
pub type Stream = Box<dyn LocalStream>;

pub struct Listener {
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
    #[cfg(windows)]
    inner: tokio::net::windows::named_pipe::NamedPipeServer,
    endpoint: String,
}
impl Listener {
    pub fn bind(paths: &AgentPaths) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{FileTypeExt, PermissionsExt};
            // Caller must hold agent.lock before stale socket cleanup.
            if let Ok(metadata) = std::fs::symlink_metadata(&paths.endpoint) {
                if !metadata.file_type().is_socket() {
                    return Err(io::Error::other("IPC路径被非socket文件占用"));
                }
                std::fs::remove_file(&paths.endpoint)?;
            }
            let inner = tokio::net::UnixListener::bind(&paths.endpoint)?;
            std::fs::set_permissions(&paths.endpoint, std::fs::Permissions::from_mode(0o600))?;
            Ok(Self {
                inner,
                endpoint: paths.endpoint.clone(),
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                inner: create_pipe(&paths.endpoint, true)?,
                endpoint: paths.endpoint.clone(),
            })
        }
    }
    pub async fn accept(&mut self) -> io::Result<Stream> {
        #[cfg(unix)]
        {
            loop {
                let (stream, _) = self.inner.accept().await?;
                if stream
                    .peer_cred()
                    .is_ok_and(|peer| peer.uid() == rustix::process::getuid().as_raw())
                {
                    return Ok(Box::new(stream));
                }
                // Rejection belongs to this client, not to the listener. An
                // unauthorized connection must not shut down the whole Agent.
                drop(stream);
                tokio::task::yield_now().await;
            }
        }
        #[cfg(windows)]
        {
            self.inner.connect().await?;
            // Create a replacement before handing off the connected instance,
            // retaining the namespace throughout the server lifetime.
            let replacement = create_pipe(&self.endpoint, false)?;
            Ok(Box::new(std::mem::replace(&mut self.inner, replacement)))
        }
    }
}
impl Drop for Listener {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(&self.endpoint);
        }
    }
}

pub async fn connect(paths: &AgentPaths) -> io::Result<Stream> {
    #[cfg(unix)]
    {
        Ok(Box::new(
            tokio::net::UnixStream::connect(&paths.endpoint).await?,
        ))
    }
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ClientOptions;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match ClientOptions::new().open(&paths.endpoint) {
                Ok(stream) => return Ok(Box::new(stream)),
                Err(e)
                    if e.raw_os_error() == Some(231) && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await
                }
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(windows)]
fn create_pipe(
    endpoint: &str,
    first: bool,
) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    use tokio::net::windows::named_pipe::ServerOptions;
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    let descriptor = super::private_access::windows::Descriptor::new(false)?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.pointer(),
        bInheritHandle: 0,
    };
    // CreateNamedPipe copies the descriptor during this call; both buffers live through it.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .max_instances(33)
            .create_with_security_attributes_raw(
                endpoint,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            )
    }
}
