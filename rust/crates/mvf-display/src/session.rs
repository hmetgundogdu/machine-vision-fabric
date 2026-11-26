//! Which session this process runs in: an interactive user session (native windows possible), a
//! service session (Windows Session 0, a launchd daemon), or a headless one (no display server).
//!
//! The graph scheduler never depends on the answer; it only decides whether `display.mode: native`
//! can be honoured. Managed surfaces work from any session because the display host is a separate
//! process in the user's session.

use std::ffi::OsString;

/// Kind of session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionKind {
    /// A user session with a display server or window station the process may use.
    Interactive,
    /// A service session: windows are impossible or invisible (Session 0, launchd daemon, ssh).
    Service,
    /// No display server is reachable.
    Headless,
}

/// The probed session.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Session {
    /// Kind.
    pub kind: SessionKind,
    /// Operator-facing detail, e.g. `interactive (x11 :0)` or `service (session 0)`.
    pub detail: String,
}

impl Session {
    /// Whether native windows are possible.
    pub fn interactive(&self) -> bool {
        self.kind == SessionKind::Interactive
    }

    fn new(kind: SessionKind, detail: impl Into<String>) -> Self {
        let label = match kind {
            SessionKind::Interactive => "interactive",
            SessionKind::Service => "service",
            SessionKind::Headless => "headless",
        };
        Self {
            kind,
            detail: format!("{label} ({})", detail.into()),
        }
    }
}

/// Environment variables that give a process access to the user's display server (Unix). A node
/// without a native display grant does not inherit them.
pub const DISPLAY_ENV: &[&str] = &["DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY", "WAYLAND_SOCKET"];

/// Probes the current process's session. `MVF_DISPLAY_SESSION=headless` forces a headless answer
/// (an operator's way to keep a GUI-capable machine headless); the override can only take
/// capability away, never grant it.
pub fn probe() -> Session {
    probe_with_override(std::env::var("MVF_DISPLAY_SESSION").ok().as_deref())
}

/// [`probe`] with an explicit override value (`Some("headless")` forces headless; anything else
/// probes the platform).
pub fn probe_with_override(value: Option<&str>) -> Session {
    if value == Some("headless") {
        return Session::new(SessionKind::Headless, "MVF_DISPLAY_SESSION=headless");
    }
    platform()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform() -> Session {
    use std::path::{Path, PathBuf};
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    if let Some(wayland) = var("WAYLAND_DISPLAY") {
        let socket = if Path::new(&wayland).is_absolute() {
            PathBuf::from(&wayland)
        } else {
            var("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(&wayland)
        };
        if socket.exists() {
            return Session::new(
                SessionKind::Interactive,
                format!("wayland {}", wayland.to_string_lossy()),
            );
        }
    }
    if let Some(display) = var("DISPLAY") {
        let d = display.to_string_lossy().to_string();
        // A local display `:N[.S]` has a socket; a remote `host:N` cannot be checked cheaply.
        let local = d
            .strip_prefix(':')
            .and_then(|n| n.split('.').next())
            .map(|n| Path::new("/tmp/.X11-unix").join(format!("X{n}")));
        if local.is_none_or(|s| s.exists()) {
            return Session::new(SessionKind::Interactive, format!("x11 {d}"));
        }
        return Session::new(
            SessionKind::Headless,
            format!("DISPLAY={d} but no X server socket"),
        );
    }
    Session::new(SessionKind::Headless, "no DISPLAY or WAYLAND_DISPLAY")
}

#[cfg(target_os = "macos")]
fn platform() -> Session {
    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SessionGetInfo(session: u32, session_id: *mut u32, attributes: *mut u32) -> i32;
    }
    const CALLER_SECURITY_SESSION: u32 = 0xFFFF_FFFF;
    const SESSION_HAS_GRAPHIC_ACCESS: u32 = 0x0010;
    let mut id = 0u32;
    let mut attrs = 0u32;
    // SAFETY: both out-pointers are valid for the duration of the call.
    let status = unsafe { SessionGetInfo(CALLER_SECURITY_SESSION, &mut id, &mut attrs) };
    if status != 0 {
        return Session::new(
            SessionKind::Headless,
            format!("SessionGetInfo failed ({status})"),
        );
    }
    if attrs & SESSION_HAS_GRAPHIC_ACCESS != 0 {
        Session::new(
            SessionKind::Interactive,
            format!("aqua, security session {id}"),
        )
    } else {
        Session::new(
            SessionKind::Service,
            format!("security session {id} has no window server access (daemon or ssh)"),
        )
    }
}

#[cfg(windows)]
fn platform() -> Session {
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows_sys::Win32::System::StationsAndDesktops::{
        GetProcessWindowStation, GetUserObjectInformationW, UOI_FLAGS, USEROBJECTFLAGS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    const WSF_VISIBLE: u32 = 0x0001;
    let mut session = 0u32;
    // SAFETY: valid out-pointer.
    let ok = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) } != 0;
    if ok && session == 0 {
        return Session::new(
            SessionKind::Service,
            "session 0: services cannot show windows",
        );
    }
    let mut flags = USEROBJECTFLAGS {
        fInherit: 0,
        fReserved: 0,
        dwFlags: 0,
    };
    let mut needed = 0u32;
    // SAFETY: the window station handle needs no close; the buffer is a USEROBJECTFLAGS.
    let visible = unsafe {
        let station = GetProcessWindowStation();
        !station.is_null()
            && GetUserObjectInformationW(
                station,
                UOI_FLAGS,
                (&mut flags as *mut USEROBJECTFLAGS).cast(),
                std::mem::size_of::<USEROBJECTFLAGS>() as u32,
                &mut needed,
            ) != 0
            && flags.dwFlags & WSF_VISIBLE != 0
    };
    if visible {
        Session::new(
            SessionKind::Interactive,
            format!("session {session}, WinSta0"),
        )
    } else {
        Session::new(
            SessionKind::Service,
            format!("session {session}: non-interactive window station"),
        )
    }
}

#[cfg(not(any(unix, windows)))]
fn platform() -> Session {
    Session::new(SessionKind::Headless, "unsupported platform")
}

/// The display environment this process has (Unix), for passing to a native node.
pub fn display_env() -> Vec<(String, OsString)> {
    DISPLAY_ENV
        .iter()
        .filter_map(|k| std::env::var_os(k).map(|v| (k.to_string(), v)))
        .collect()
}
