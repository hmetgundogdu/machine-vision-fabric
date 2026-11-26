//! Orphan prevention: worker processes must not outlive the host, even when the host is killed
//! (`SIGKILL`, task manager, crash) and never gets to reap them.
//!
//! - **Linux**: each child gets `PR_SET_PDEATHSIG(SIGKILL)`. The kernel delivers the signal when
//!   the *thread* that forked the child exits, not the process. The engine starts workers from many
//!   threads: candidate preparation, pipelined stages and restarts. So every spawn goes through one
//!   dedicated spawner thread that lives as long as the process. A child whose parent died between
//!   `fork` and `prctl` exits at once (the classic race).
//! - **Windows**: every child is assigned to one process-wide Job Object with
//!   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. The host never closes the handle, so the OS closes it, and
//!   kills every member, when the host process ends for any reason. Members' own children inherit the
//!   job. A child that dies between spawn and assignment is harmless; one that the host's death
//!   interrupts in that window can escape (there is no suspended spawn in `std`).
//! - **macOS and other Unix**: no kernel mechanism. Children rely on the SDKs' cooperative exit on
//!   stdin EOF; a child that never reads stdin can be orphaned. macOS is a development platform here,
//!   not an edge target.

use std::process::{Child, Command};

/// Spawns `command` so that the child dies with the host process where the OS allows it.
pub fn spawn(command: Command) -> std::io::Result<Child> {
    imp::spawn(command)
}

/// Whether this platform kills workers when the host process dies (non-cooperatively).
pub const KILLS_CHILDREN_WITH_HOST: bool = cfg!(any(target_os = "linux", windows));

#[cfg(target_os = "linux")]
mod imp {
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};
    use std::sync::mpsc::{self, Sender};
    use std::sync::{Mutex, OnceLock};

    type Request = (Command, Sender<std::io::Result<Child>>);

    fn spawner() -> &'static Mutex<Sender<Request>> {
        static SPAWNER: OnceLock<Mutex<Sender<Request>>> = OnceLock::new();
        SPAWNER.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Request>();
            // Never joined: it must outlive every child it forks (PDEATHSIG is per thread).
            let _ = std::thread::Builder::new()
                .name("mvf-spawner".into())
                .spawn(move || {
                    for (mut command, reply) in rx {
                        let _ = reply.send(spawn_here(&mut command));
                    }
                });
            Mutex::new(tx)
        })
    }

    fn spawn_here(command: &mut Command) -> std::io::Result<Child> {
        let host = std::process::id() as libc::pid_t;
        // SAFETY: the closure runs in the forked child before `exec` and only calls the
        // async-signal-safe `prctl`, `getppid` and `_exit`; it allocates nothing and takes no lock.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(
                    libc::PR_SET_PDEATHSIG,
                    libc::SIGKILL as libc::c_ulong,
                    0,
                    0,
                    0,
                ) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != host {
                    libc::_exit(1); // the host died before the death signal was armed
                }
                Ok(())
            });
        }
        command.spawn()
    }

    pub(super) fn spawn(command: Command) -> std::io::Result<Child> {
        let (reply, result) = mpsc::channel();
        let sent = spawner()
            .lock()
            .map_err(|_| std::io::Error::other("spawner poisoned"))?
            .send((command, reply));
        match sent {
            Ok(()) => result
                .recv()
                .unwrap_or_else(|_| Err(std::io::Error::other("spawner thread ended"))),
            Err(mpsc::SendError((mut command, _))) => spawn_here(&mut command),
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::io::AsRawHandle;
    use std::process::{Child, Command};
    use std::sync::OnceLock;

    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    /// The process-wide job (a raw handle kept for the life of the process), or `None` when the OS
    /// refused to create or configure it.
    fn job() -> Option<usize> {
        static JOB: OnceLock<Option<usize>> = OnceLock::new();
        *JOB.get_or_init(|| {
            // SAFETY: plain Win32 calls with valid arguments; the info struct is a C POD for which
            // all-zero is a valid value, and the handle is checked before use.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                (ok != 0).then_some(job as usize)
            }
        })
    }

    pub(super) fn spawn(mut command: Command) -> std::io::Result<Child> {
        let child = command.spawn()?;
        if let Some(job) = job() {
            // SAFETY: `job` is a live job handle owned by this process; the child handle is valid
            // while `child` is alive. Failure (e.g. a job policy of the parent) leaves the child
            // unassigned, which is the pre-existing cooperative behaviour.
            unsafe {
                AssignProcessToJobObject(job as _, child.as_raw_handle() as _);
            }
        }
        Ok(child)
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use std::process::{Child, Command};

    pub(super) fn spawn(mut command: Command) -> std::io::Result<Child> {
        command.spawn()
    }
}
