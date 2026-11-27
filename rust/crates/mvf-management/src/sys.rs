//! Local endpoints authenticated by the operating system.
//!
//! - **Unix:** a socket in the private runtime directory, mode 0600. Every accepted connection's
//!   peer uid (`SO_PEERCRED` on Linux, `getpeereid` elsewhere) must equal the runtime's effective
//!   uid, or it is refused. Two locks, then: the filesystem keeps others from reaching the socket,
//!   and the peer check refuses whoever reaches it anyway (for example through a directory with the
//!   wrong mode).
//! - **Windows:** a named pipe whose DACL grants access to the current user and LocalSystem only
//!   (the default DACL would let Everyone read), with remote clients rejected and the first
//!   instance created exclusively so no other process can squat the name.

use std::io::{Read, Write};
use std::time::Duration;

/// A connection: a byte stream both ways.
pub trait Conn: Read + Write + Send {
    /// A second handle on the same connection (one reads, one writes).
    fn duplicate(&self) -> std::io::Result<Box<dyn Conn>>;
    /// Bounds blocking writes, where the platform can (Unix); a stuck client is then dropped.
    fn set_write_timeout(&self, timeout: Duration) -> std::io::Result<()>;
}

#[cfg(unix)]
pub(crate) use unix::Listener;
#[cfg(unix)]
pub use unix::connect;
#[cfg(windows)]
pub(crate) use windows::Listener;
#[cfg(windows)]
pub use windows::connect;

#[cfg(unix)]
mod unix {
    use std::os::fd::AsRawFd;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::Conn;

    impl Conn for UnixStream {
        fn duplicate(&self) -> std::io::Result<Box<dyn Conn>> {
            Ok(Box::new(self.try_clone()?))
        }

        fn set_write_timeout(&self, timeout: Duration) -> std::io::Result<()> {
            UnixStream::set_write_timeout(self, Some(timeout))
        }
    }

    pub(crate) struct Listener {
        inner: UnixListener,
        path: PathBuf,
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn peer_uid(s: &UnixStream) -> std::io::Result<u32> {
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: a valid socket and an out-buffer of the declared size.
        let r = unsafe {
            libc::getsockopt(
                s.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        };
        if r != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(cred.uid)
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn peer_uid(s: &UnixStream) -> std::io::Result<u32> {
        let (mut uid, mut gid) = (0, 0);
        // SAFETY: a valid socket and two out-parameters.
        if unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(uid)
    }

    impl Listener {
        pub(crate) fn bind(dir: &Path, name: &str) -> std::io::Result<(Self, String)> {
            use std::os::unix::fs::PermissionsExt;
            let mut path = dir.join(format!("{name}.sock"));
            // A socket path is limited to about 104 bytes (macOS; 108 on Linux). A long runtime
            // directory gets its socket in a short private one, checked the same way; the
            // registry file still says where it is.
            if path.as_os_str().len() > 100 {
                // SAFETY: geteuid has no preconditions.
                let short = PathBuf::from(format!("/tmp/mvf-{}", unsafe { libc::geteuid() }));
                crate::endpoint::ensure_private(&short)?;
                path = short.join(format!("{name}.sock"));
            }
            let _ = std::fs::remove_file(&path);
            let inner = UnixListener::bind(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            let endpoint = path.to_string_lossy().into_owned();
            Ok((Self { inner, path }, endpoint))
        }

        /// The next connection, with whether its peer is the runtime's own user.
        pub(crate) fn accept(&mut self) -> std::io::Result<(Box<dyn Conn>, bool)> {
            let (stream, _) = self.inner.accept()?;
            // SAFETY: geteuid has no preconditions.
            let me = unsafe { libc::geteuid() };
            let authorized = peer_uid(&stream).is_ok_and(|uid| uid == me);
            Ok((Box::new(stream), authorized))
        }

        pub(crate) fn cleanup(&self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// A raw connection to a local endpoint (no `hello`).
    pub fn connect(endpoint: &str) -> std::io::Result<Box<dyn Conn>> {
        Ok(Box::new(UnixStream::connect(endpoint)?))
    }
}

#[cfg(windows)]
mod windows {
    use std::fs::File;
    use std::os::windows::io::FromRawHandle;
    use std::path::Path;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    use super::Conn;

    impl Conn for File {
        fn duplicate(&self) -> std::io::Result<Box<dyn Conn>> {
            Ok(Box::new(self.try_clone()?))
        }

        fn set_write_timeout(&self, _: Duration) -> std::io::Result<()> {
            // Synchronous pipe writes cannot time out; a stuck client holds only its own thread.
            Ok(())
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The current user's SID as a string (`S-1-5-21-…`).
    fn current_user_sid() -> std::io::Result<String> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: our own process handle and an out-parameter.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut len = 0u32;
        // SAFETY: a size query.
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len) };
        // u64 elements keep the buffer aligned for TOKEN_USER.
        let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
        // SAFETY: a buffer of at least `len` bytes.
        let ok = unsafe {
            GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len)
        };
        // SAFETY: we opened the token.
        unsafe { CloseHandle(token) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: GetTokenInformation filled a TOKEN_USER at the start of the buffer.
        let sid = unsafe { (*(buf.as_ptr() as *const TOKEN_USER)).User.Sid };
        let mut text: *mut u16 = std::ptr::null_mut();
        // SAFETY: a valid SID; the string is freed with LocalFree below.
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut n = 0;
        // SAFETY: a NUL-terminated wide string from the system.
        while unsafe { *text.add(n) } != 0 {
            n += 1;
        }
        // SAFETY: `n` valid code units.
        let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, n) });
        // SAFETY: allocated by ConvertSidToStringSidW.
        unsafe { LocalFree(text.cast()) };
        Ok(s)
    }

    pub(crate) struct Listener {
        name: Vec<u16>,
        descriptor: PSECURITY_DESCRIPTOR,
        next: Option<HANDLE>,
    }

    // SAFETY: the descriptor and handles are owned by the listener and used from one thread at a
    // time.
    unsafe impl Send for Listener {}

    impl Listener {
        fn instance(&self, first: bool) -> std::io::Result<HANDLE> {
            let attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.descriptor,
                bInheritHandle: 0,
            };
            // SAFETY: a NUL-terminated name and a valid security descriptor.
            let h = unsafe {
                CreateNamedPipeW(
                    self.name.as_ptr(),
                    PIPE_ACCESS_DUPLEX
                        | if first {
                            FILE_FLAG_FIRST_PIPE_INSTANCE
                        } else {
                            0
                        },
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    PIPE_UNLIMITED_INSTANCES,
                    64 * 1024,
                    64 * 1024,
                    0,
                    &attributes,
                )
            };
            if h == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error());
            }
            Ok(h)
        }

        pub(crate) fn bind(_dir: &Path, name: &str) -> std::io::Result<(Self, String)> {
            let endpoint = format!(r"\\.\pipe\{name}");
            // Full access for the current user and LocalSystem; nobody else, not even to read.
            let sddl = wide(&format!("D:P(A;;GA;;;{})(A;;GA;;;SY)", current_user_sid()?));
            let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            // SAFETY: a NUL-terminated SDDL string; freed with LocalFree on drop.
            if unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            let mut listener = Self {
                name: wide(&endpoint),
                descriptor,
                next: None,
            };
            // The first instance exists before the endpoint is advertised.
            listener.next = Some(listener.instance(true)?);
            Ok((listener, endpoint))
        }

        /// The next connection. The DACL already admitted only the current user.
        pub(crate) fn accept(&mut self) -> std::io::Result<(Box<dyn Conn>, bool)> {
            let h = match self.next.take() {
                Some(h) => h,
                None => self.instance(false)?,
            };
            // SAFETY: a valid pipe instance; synchronous connect.
            let ok = unsafe { ConnectNamedPipe(h, std::ptr::null_mut()) };
            let error = std::io::Error::last_os_error();
            if ok == 0 && error.raw_os_error() != Some(ERROR_PIPE_CONNECTED as i32) {
                // SAFETY: our own handle.
                unsafe { CloseHandle(h) };
                return Err(error);
            }
            // SAFETY: a connected pipe handle we own from here on.
            let file = unsafe { File::from_raw_handle(h) };
            Ok((Box::new(file), true))
        }

        pub(crate) fn cleanup(&self) {}
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            if let Some(h) = self.next.take() {
                // SAFETY: our own handle.
                unsafe { CloseHandle(h) };
            }
            // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
            unsafe { LocalFree(self.descriptor) };
        }
    }

    /// A raw connection to a local endpoint (no `hello`).
    pub fn connect(endpoint: &str) -> std::io::Result<Box<dyn Conn>> {
        // All instances can be busy for a moment, between one accept and the next instance.
        let mut last = None;
        for _ in 0..100 {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(endpoint)
            {
                Ok(f) => return Ok(Box::new(f)),
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    last = Some(e);
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(e),
            }
        }
        Err(last.unwrap_or_else(|| std::io::Error::other("pipe busy")))
    }
}
