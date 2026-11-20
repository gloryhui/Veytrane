//! Local authenticated byte transport. The protocol never listens on TCP.
use crate::paths::AgentPaths;
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
    let descriptor = veytrane_transport::permissions::windows::Descriptor::new(false)?;
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

#[cfg(all(test, windows))]
mod windows_tests {
    use super::{Listener, connect};
    use crate::paths::AgentPaths;
    use std::os::windows::io::AsRawHandle;
    use std::path::PathBuf;
    use std::ptr;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use windows_sys::Win32::{
        Foundation::{ERROR_SUCCESS, HANDLE, LocalFree},
        Security::{
            ACCESS_ALLOWED_ACE, ACL,
            Authorization::{ConvertStringSidToSidW, GetSecurityInfo, SE_FILE_OBJECT},
            DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetSecurityDescriptorControl,
            PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
        },
    };

    struct OwnedSid(PSID);

    impl OwnedSid {
        fn parse(text: &str) -> Self {
            let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
            let mut sid = ptr::null_mut();
            assert_ne!(
                unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut sid) },
                0,
                "ConvertStringSidToSidW({text:?})"
            );
            Self(sid)
        }
    }

    impl Drop for OwnedSid {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }

    struct OwnedSecurityDescriptor(PSECURITY_DESCRIPTOR);

    impl Drop for OwnedSecurityDescriptor {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }

    fn assert_pipe_dacl(pipe: &tokio::net::windows::named_pipe::NamedPipeServer) {
        let current_text = veytrane_transport::permissions::windows::user_sid().unwrap();
        let current = OwnedSid::parse(&current_text);
        let system = OwnedSid::parse("S-1-5-18");
        let mut descriptor = ptr::null_mut();
        let mut dacl: *mut ACL = ptr::null_mut();
        let status = unsafe {
            GetSecurityInfo(
                pipe.as_raw_handle() as HANDLE,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(
            status,
            ERROR_SUCCESS,
            "GetSecurityInfo: {}",
            std::io::Error::last_os_error()
        );
        let descriptor = OwnedSecurityDescriptor(descriptor);
        let mut control = 0;
        let mut revision = 0;
        assert_ne!(
            unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) },
            0,
            "GetSecurityDescriptorControl: {}",
            std::io::Error::last_os_error()
        );
        assert_ne!(control & SE_DACL_PROTECTED, 0, "DACL must be protected");
        let dacl = unsafe { dacl.as_ref() }.expect("pipe DACL is present");
        let mut saw_current = false;
        let mut saw_system = false;
        for index in 0..u32::from(dacl.AceCount) {
            let mut ace = ptr::null_mut();
            assert_ne!(
                unsafe { GetAce(dacl, index, &mut ace) },
                0,
                "GetAce({index}): {}",
                std::io::Error::last_os_error()
            );
            let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
            assert_eq!(ace.Header.AceType, 0, "unexpected non-access-allowed ACE");
            let sid = ptr::addr_of!(ace.SidStart)
                .cast_mut()
                .cast::<std::ffi::c_void>();
            let is_current = unsafe { EqualSid(sid, current.0) } != 0;
            let is_system = unsafe { EqualSid(sid, system.0) } != 0;
            assert!(
                is_current || is_system,
                "pipe DACL contains a SID other than SYSTEM/current user"
            );
            saw_current |= is_current;
            saw_system |= is_system;
        }
        assert!(saw_current, "current user SID is absent");
        assert!(saw_system, "SYSTEM SID is absent");
        assert_eq!(dacl.AceCount, 2, "pipe DACL must contain exactly two ACEs");
    }

    #[test]
    fn windows_pipe_dacl_is_private_and_same_user_roundtrip_releases() {
        let endpoint = format!(
            r"\\.\pipe\veytrane-management-dacl-{}-{:032x}",
            std::process::id(),
            rand::random::<u128>()
        );
        let paths = AgentPaths {
            root: PathBuf::new(),
            endpoint,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut listener = Listener::bind(&paths).unwrap();
            assert_pipe_dacl(&listener.inner);
            let (mut accepted, mut client) = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::try_join!(listener.accept(), connect(&paths))
            })
            .await
            .expect("accept/connect timed out")
            .expect("accept/connect failed");
            assert_pipe_dacl(&listener.inner);
            client.write_all(b"same-user").await.unwrap();
            client.flush().await.unwrap();
            let mut received = [0; 9];
            accepted.read_exact(&mut received).await.unwrap();
            assert_eq!(&received, b"same-user");

            drop(client);
            drop(accepted);
            drop(listener);
            tokio::task::yield_now().await;
            drop(Listener::bind(&paths).expect("pipe namespace released after drop"));
        });
    }
}
