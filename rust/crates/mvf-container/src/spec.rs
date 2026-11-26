//! OCI runtime-spec (`config.json`) generation for an MVF sandboxed node. Pure: no I/O besides
//! reading the rootfs `/etc/passwd`/`/etc/group` for named users, so it is unit-tested everywhere.
//!
//! The sandbox is **default deny**:
//! - new pid, mount, ipc, uts, cgroup and **network** namespaces (no network unless granted);
//!   a user namespace when the host runs as root (container root maps to an unprivileged range);
//! - every capability dropped, `noNewPrivileges`, read-only root filesystem, `/tmp` and `/dev/shm`
//!   as size-limited tmpfs, `/proc` and `/sys` paths masked or read-only;
//! - only the standard runtime devices (null, zero, full, random, urandom, tty) plus explicit
//!   grants; every other device access is denied by the device cgroup;
//! - a seccomp **denylist** (module loading, kexec, mount, namespace and keyring syscalls, bpf,
//!   ptrace, …), or Docker's **allowlist** minus that denylist (`isolation.syscalls: allowlist`,
//!   see [`crate::seccomp`]);
//! - an AppArmor profile or SELinux label when the host configures one;
//! - memory/cpu/pids cgroup limits from the contract;
//! - the engine arena file is the only host path mounted (bind, `noexec,nosuid,nodev`).

use std::path::Path;

use mvf_contract::{DeviceAccess, NetworkPolicy, NodeContract};
use mvf_oci::ExecConfig;
use serde_json::{Value, json};

/// Path of the arena inside the container (`MVF_ARENA_PATH`).
pub const CONTAINER_ARENA: &str = "/mvf/arena";
/// Directory of a node's display surfaces inside the container (`MVF_DISPLAY_SURFACES`).
pub const CONTAINER_DISPLAY: &str = "/mvf/display";

/// Syscalls denied by the default profile.
pub const DENIED_SYSCALLS: &[&str] = &[
    "acct",
    "add_key",
    "bpf",
    "clock_adjtime",
    "clock_settime",
    "create_module",
    "delete_module",
    "finit_module",
    "fsconfig",
    "fsmount",
    "fsopen",
    "fspick",
    "get_kernel_syms",
    "init_module",
    "ioperm",
    "iopl",
    "kcmp",
    "kexec_file_load",
    "kexec_load",
    "keyctl",
    "lookup_dcookie",
    "mount",
    "mount_setattr",
    "move_mount",
    "name_to_handle_at",
    "nfsservctl",
    "open_by_handle_at",
    "open_tree",
    "perf_event_open",
    "pivot_root",
    "process_vm_readv",
    "process_vm_writev",
    "ptrace",
    "query_module",
    "quotactl",
    "reboot",
    "request_key",
    "setns",
    "settimeofday",
    "swapoff",
    "swapon",
    "syslog",
    "umount2",
    "unshare",
    "uselib",
    "userfaultfd",
    "vhangup",
];

/// A device to expose: host path plus its node numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceNode {
    /// Path (same inside and outside).
    pub path: String,
    /// `c` or `b`.
    pub kind: char,
    /// Major number.
    pub major: i64,
    /// Minor number.
    pub minor: i64,
    /// Granted access.
    pub access: DeviceAccess,
}

/// User-namespace mapping: container uid/gid 0.. maps to `host_id`.. for `size` ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdMapping {
    /// First host id.
    pub host_id: u32,
    /// Number of ids.
    pub size: u32,
}

/// Inputs of the spec generator.
#[derive(Debug, Clone)]
pub struct SpecInput<'a> {
    /// Absolute rootfs path.
    pub rootfs: &'a Path,
    /// Host arena path.
    pub arena: &'a Path,
    /// Image execution defaults.
    pub image: &'a ExecConfig,
    /// The node contract (limits, permissions).
    pub contract: &'a NodeContract,
    /// Argument override (default: image entrypoint + cmd).
    pub args: Option<Vec<String>>,
    /// Extra environment (`KEY=value`).
    pub extra_env: Vec<String>,
    /// Devices to expose (already resolved on the host).
    pub devices: Vec<DeviceNode>,
    /// User namespace mapping, when used.
    pub userns: Option<IdMapping>,
    /// Hostname inside the sandbox.
    pub hostname: String,
    /// Seccomp policy.
    pub syscalls: mvf_contract::SyscallPolicy,
    /// Security-module profile to apply.
    pub lsm: Option<Lsm>,
    /// For `network: restricted`: the prepared namespace the container joins.
    pub netns: Option<std::path::PathBuf>,
    /// Display Fabric surfaces: `(path inside the container, host file)`, each surface with its
    /// writer lock. Bind-mounted into a private tmpfs at [`CONTAINER_DISPLAY`]; nothing else of
    /// the display — no server socket, no host lock, no other node's surface — is visible.
    pub display: Vec<(String, std::path::PathBuf)>,
}

/// A Linux security module profile the host provides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lsm {
    /// An AppArmor profile already loaded on the host.
    AppArmor(String),
    /// An SELinux process label.
    SeLinux(String),
}

impl Lsm {
    /// The negotiation grant name (`apparmor:<profile>`, `selinux:<label>`).
    pub fn grant(&self) -> String {
        match self {
            Lsm::AppArmor(p) => format!("apparmor:{p}"),
            Lsm::SeLinux(l) => format!("selinux:{l}"),
        }
    }
}

/// Resolves the image `User` (`uid[:gid]` or names from the rootfs) to numeric ids.
pub fn resolve_user(user: Option<&str>, rootfs: &Path) -> Result<(u32, u32), String> {
    let Some(user) = user.filter(|u| !u.is_empty()) else {
        return Ok((0, 0));
    };
    let (u, g) = match user.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (user, None),
    };
    // Fields: passwd `name:x:uid:gid:…`, group `name:x:gid:members`.
    let fields = |file: &str, name: &str| -> Option<Vec<String>> {
        let text = std::fs::read_to_string(rootfs.join(file)).ok()?;
        text.lines()
            .map(|l| l.split(':').map(str::to_string).collect::<Vec<_>>())
            .find(|f| f.first().map(String::as_str) == Some(name) && f.len() >= 3)
    };
    let lookup = |file: &str, name: &str| -> Option<(u32, u32)> {
        let f = fields(file, name)?;
        let first: u32 = f[2].parse().ok()?;
        let second = f.get(3).and_then(|g| g.parse().ok()).unwrap_or(first);
        Some((first, second))
    };
    let (uid, primary_gid) = match u.parse::<u32>() {
        Ok(uid) => (uid, uid),
        Err(_) => {
            lookup("etc/passwd", u).ok_or_else(|| format!("user `{u}` not found in the image"))?
        }
    };
    let gid = match g {
        None => primary_gid,
        Some(g) => match g.parse::<u32>() {
            Ok(gid) => gid,
            Err(_) => lookup("etc/group", g)
                .map(|(gid, _)| gid)
                .ok_or_else(|| format!("group `{g}` not found in the image"))?,
        },
    };
    Ok((uid, gid))
}

fn access(a: DeviceAccess) -> &'static str {
    match a {
        DeviceAccess::R => "r",
        DeviceAccess::W => "w",
        DeviceAccess::Rw => "rw",
    }
}

/// Builds `config.json`.
pub fn runtime_spec(input: &SpecInput<'_>) -> Result<Value, String> {
    let image = input.image;
    let args = match &input.args {
        Some(a) => a.clone(),
        None => {
            let mut a = image.entrypoint.clone().unwrap_or_default();
            a.extend(image.cmd.clone().unwrap_or_default());
            a
        }
    };
    if args.is_empty() {
        return Err("the image defines no entrypoint or command".into());
    }
    let (uid, gid) = resolve_user(image.user.as_deref(), input.rootfs)?;
    let mut env = image.env.clone().unwrap_or_default();
    if !env.iter().any(|e| e.starts_with("PATH=")) {
        env.push("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into());
    }
    env.push(format!("MVF_ARENA_PATH={CONTAINER_ARENA}"));
    env.extend(input.extra_env.iter().cloned());

    let mut namespaces = vec![
        json!({"type": "pid"}),
        json!({"type": "ipc"}),
        json!({"type": "uts"}),
        json!({"type": "mount"}),
        json!({"type": "cgroup"}),
    ];
    // `none`: a fresh, empty namespace. `restricted`: the namespace MVF prepared and enforces.
    // `host`: no network namespace at all (the host's network, unisolated).
    match (input.contract.spec.permissions.network.mode(), &input.netns) {
        (NetworkPolicy::None, _) => namespaces.push(json!({"type": "network"})),
        (NetworkPolicy::Restricted, Some(path)) => {
            namespaces.push(json!({"type": "network", "path": path}))
        }
        (NetworkPolicy::Restricted, None) => {
            return Err("`network: restricted` needs a prepared network namespace".into());
        }
        (NetworkPolicy::Host, _) => {}
        (NetworkPolicy::Loopback, _) => {
            return Err("`network: loopback` is not supported by this backend".into());
        }
    }
    let mut linux = json!({
        "namespaces": namespaces,
        "maskedPaths": [
            "/proc/acpi", "/proc/asound", "/proc/kcore", "/proc/keys", "/proc/latency_stats",
            "/proc/timer_list", "/proc/timer_stats", "/proc/sched_debug", "/proc/scsi", "/sys/firmware"
        ],
        "readonlyPaths": ["/proc/bus", "/proc/fs", "/proc/irq", "/proc/sys", "/proc/sysrq-trigger"],
        "seccomp": match input.syscalls {
            mvf_contract::SyscallPolicy::Denylist => crate::seccomp::denylist(),
            mvf_contract::SyscallPolicy::Allowlist => crate::seccomp::allowlist(crate::seccomp::moby_arch())?,
        },
    });
    if let Some(m) = input.userns {
        if let Some(n) = linux["namespaces"].as_array_mut() {
            n.push(json!({"type": "user"}));
        }
        linux["uidMappings"] = json!([{ "containerID": 0, "hostID": m.host_id, "size": m.size }]);
        linux["gidMappings"] = json!([{ "containerID": 0, "hostID": m.host_id, "size": m.size }]);
    }

    let r = &input.contract.spec.resources;
    let mut device_rules = vec![json!({"allow": false, "access": "rwm"})];
    let mut devices = Vec::new();
    for d in &input.devices {
        device_rules.push(json!({
            "allow": true, "type": d.kind.to_string(), "major": d.major, "minor": d.minor,
            "access": access(d.access),
        }));
        devices.push(json!({
            "path": d.path, "type": d.kind.to_string(), "major": d.major, "minor": d.minor,
            "fileMode": 0o660, "uid": uid, "gid": gid,
        }));
    }
    let mut resources = json!({ "devices": device_rules });
    if let Some(mib) = r.memory_mib {
        let bytes = mib.saturating_mul(1024 * 1024);
        resources["memory"] = json!({ "limit": bytes, "swap": bytes });
    }
    if let Some(millis) = r.cpu_millis {
        resources["cpu"] = json!({ "quota": millis.saturating_mul(100), "period": 100_000 });
    }
    if let Some(pids) = r.pids {
        resources["pids"] = json!({ "limit": pids });
    }
    linux["resources"] = resources;
    if !devices.is_empty() {
        linux["devices"] = json!(devices);
    }

    let cwd = image
        .working_dir
        .clone()
        .filter(|w| w.starts_with('/'))
        .unwrap_or_else(|| "/".into());
    // A joined network namespace belongs to the host's user namespace (so the node holds no
    // capability over its own interface), and sysfs can only be mounted by the namespace owner: such
    // a node gets an empty /sys instead of the host's sysfs.
    let sys_mount = if input.netns.is_some() {
        json!({"destination": "/sys", "type": "tmpfs", "source": "tmpfs", "options": ["nosuid", "noexec", "nodev", "mode=555", "size=1024k"]})
    } else {
        json!({"destination": "/sys", "type": "sysfs", "source": "sysfs", "options": ["nosuid", "noexec", "nodev", "ro"]})
    };
    let ro_bind = |dest: &str, src: &Path| {
        json!({"destination": dest, "type": "bind", "source": src,
               "options": ["rbind", "rw", "nosuid", "nodev", "noexec"]})
    };
    let mut spec = json!({
        "ociVersion": "1.0.2",
        "process": {
            "terminal": false,
            "user": { "uid": uid, "gid": gid },
            "args": args,
            "env": env,
            "cwd": cwd,
            "capabilities": { "bounding": [], "effective": [], "inheritable": [], "permitted": [], "ambient": [] },
            "rlimits": [{ "type": "RLIMIT_NOFILE", "hard": 4096, "soft": 4096 }],
            "noNewPrivileges": true,
        },
        "root": { "path": input.rootfs, "readonly": true },
        "hostname": input.hostname,
        "mounts": [
            {"destination": "/proc", "type": "proc", "source": "proc"},
            {"destination": "/dev", "type": "tmpfs", "source": "tmpfs", "options": ["nosuid", "strictatime", "mode=755", "size=65536k"]},
            {"destination": "/dev/pts", "type": "devpts", "source": "devpts", "options": ["nosuid", "noexec", "newinstance", "ptmxmode=0666", "mode=0620"]},
            {"destination": "/dev/shm", "type": "tmpfs", "source": "shm", "options": ["nosuid", "noexec", "nodev", "mode=1777", "size=65536k"]},
            {"destination": "/dev/mqueue", "type": "mqueue", "source": "mqueue", "options": ["nosuid", "noexec", "nodev"]},
            sys_mount,
            {"destination": "/sys/fs/cgroup", "type": "cgroup", "source": "cgroup", "options": ["nosuid", "noexec", "nodev", "relatime", "ro"]},
            {"destination": "/tmp", "type": "tmpfs", "source": "tmpfs", "options": ["nosuid", "nodev", "mode=1777", "size=65536k"]},
            ro_bind(CONTAINER_ARENA, input.arena),
        ],
        "linux": linux,
    });
    if !input.display.is_empty()
        && let Some(mounts) = spec["mounts"].as_array_mut()
    {
        mounts.push(
            json!({"destination": CONTAINER_DISPLAY, "type": "tmpfs", "source": "tmpfs",
                           "options": ["nosuid", "nodev", "noexec", "mode=755", "size=64k"]}),
        );
        for (dest, src) in &input.display {
            mounts.push(ro_bind(dest, src));
        }
    }
    match &input.lsm {
        Some(Lsm::AppArmor(profile)) => spec["process"]["apparmorProfile"] = json!(profile),
        Some(Lsm::SeLinux(label)) => spec["process"]["selinuxLabel"] = json!(label),
        None => {}
    }
    Ok(spec)
}
