//! OS-user management boundary, separate from remote peer authorization.
use std::{io, path::Path};

pub fn ensure_directory(path: &Path) -> io::Result<()> {
    super::config::ensure_private_app_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if std::fs::metadata(path)?.uid() != rustix::process::getuid().as_raw() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "目录不属于当前用户",
            ));
        }
    }
    #[cfg(windows)]
    windows::protect_directory(path)?;
    Ok(())
}

pub fn user_key() -> io::Result<String> {
    #[cfg(windows)]
    {
        windows::user_sid()
    }
    #[cfg(unix)]
    {
        Ok(rustix::process::getuid().as_raw().to_string())
    }
}

#[cfg(windows)]
pub(crate) mod windows {
    use std::{ffi::c_void, io, os::windows::ffi::OsStrExt, path::Path, ptr};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, LocalFree},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            },
            DACL_SECURITY_INFORMATION, GetTokenInformation, PROTECTED_DACL_SECURITY_INFORMATION,
            SetFileSecurityW, TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    pub fn user_sid() -> io::Result<String> {
        // Token handles and LocalAlloc strings are released on every path below.
        unsafe {
            let mut token = ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut size = 0;
            GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut size);
            let mut buffer = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
            let result = GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                size,
                &mut size,
            );
            let error = io::Error::last_os_error();
            CloseHandle(token);
            if result == 0 {
                return Err(error);
            }
            let user = &*(buffer.as_ptr().cast::<TOKEN_USER>());
            let mut text = ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut length = 0;
            while *text.add(length) != 0 {
                length += 1;
            }
            let result = String::from_utf16(std::slice::from_raw_parts(text, length))
                .map_err(io::Error::other);
            LocalFree(text.cast());
            result
        }
    }

    pub struct Descriptor(*mut c_void);
    impl Descriptor {
        pub fn new(inherit: bool) -> io::Result<Self> {
            let flags = if inherit { "OICI" } else { "" };
            let sddl = format!("D:P(A;{flags};GA;;;SY)(A;{flags};GA;;;{})", user_sid()?);
            let text: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
            let mut descriptor = ptr::null_mut();
            // SDDL is generated from an OS-provided SID, never user text.
            if unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    text.as_ptr(),
                    1,
                    &mut descriptor,
                    ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(descriptor))
        }
        pub fn pointer(&self) -> *mut c_void {
            self.0
        }
    }
    impl Drop for Descriptor {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
    pub fn protect_directory(path: &Path) -> io::Result<()> {
        let descriptor = Descriptor::new(true)?;
        let text: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        if text[..text.len() - 1].contains(&0) {
            return Err(io::Error::other("目录含NUL"));
        }
        // Protected DACL prevents an inherited Everyone ACE granting access.
        if unsafe {
            SetFileSecurityW(
                text.as_ptr(),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                descriptor.pointer(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
