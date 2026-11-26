//! # mvf-container — daemonless OCI execution backend
//!
//! Runs an OCI image from the MVF content store (#17) through a **low-level OCI runtime** (`runc`
//! or `crun`, found on `PATH` or via `MVF_OCI_RUNTIME`) — no Docker Engine, no containerd, no
//! daemon. The container's stdio *is* the MVF worker protocol, and the engine arena is bind-mounted,
//! so a containerised node is supervised, typed and zero-copy exactly like a local worker: this
//! crate only supplies a [`mvf_worker::Launcher`].
//!
//! Linux only (OCI images need a Linux kernel). On other hosts [`OciRuntime::detect`] returns
//! `None`, the backend is not registered, and contracts that need it are rejected by negotiation.
//! The sandbox policy is documented in [`spec`].

pub mod network;
pub mod package;
pub mod seccomp;
pub mod spec;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use mvf_contract::{
    Artifact, BackendCapabilities, BackendKind, Cancellation, GrantSupport, IsolationLevel,
    MemoryDomain, NetworkPolicy, NodeContract, Replacement, Target,
};
use mvf_engine::{ExecutionBackend, InstantiateRequest, NodeError, NodeErrorKind, NodeHandle};
use mvf_oci::{ContentStore, ImageRecord};
use mvf_worker::process::LaunchSpec;
use mvf_worker::{Launcher, worker_node};

use crate::spec::{DeviceNode, IdMapping, Lsm, SpecInput, runtime_spec};

/// A detected low-level OCI runtime and what this host lets it enforce.
#[derive(Debug, Clone)]
pub struct OciRuntime {
    /// Runtime binary.
    pub path: PathBuf,
    /// `--version` output (first line).
    pub version: String,
    /// Running as root (rootful containers, cgroup limits, user-namespace remapping).
    pub root: bool,
    /// cgroup v2 controllers available for limits.
    pub controllers: Vec<String>,
    /// The runtime was built with seccomp support.
    pub seccomp: bool,
}

pub(crate) fn running_as_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(2).map(|e| e == "0"))
        })
        .unwrap_or(false)
}

pub(crate) fn first_on_path(names: &[&str]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
}

impl OciRuntime {
    /// Detects a runtime on this host. `None` on non-Linux hosts or when no runtime is installed.
    pub fn detect() -> Option<Self> {
        if !cfg!(target_os = "linux") {
            return None;
        }
        let path = std::env::var_os("MVF_OCI_RUNTIME")
            .map(PathBuf::from)
            .or_else(|| first_on_path(&["runc", "crun"]))?;
        let out = Command::new(&path).arg("--version").output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let root = running_as_root();
        let controllers = std::fs::read_to_string("/sys/fs/cgroup/cgroup.controllers")
            .map(|s| s.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default();
        Some(Self {
            version: text.lines().next().unwrap_or_default().to_string(),
            seccomp: text.to_lowercase().contains("seccomp"),
            path,
            root,
            controllers,
        })
    }

    fn enforces(&self, controller: &str) -> bool {
        self.root && self.controllers.iter().any(|c| c == controller)
    }
}

/// Host device paths that can be granted (character/block devices under `/dev` and `/dev/dri`).
fn grantable_devices() -> Vec<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let mut out = Vec::new();
        for dir in ["/dev", "/dev/dri"] {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for e in entries.flatten() {
                    if let Ok(t) = e.file_type()
                        && (t.is_char_device() || t.is_block_device())
                    {
                        out.push(e.path().display().to_string());
                    }
                }
            }
        }
        out.sort();
        out
    }
    #[cfg(not(unix))]
    Vec::new()
}

/// The host's security-module profile: `MVF_OCI_APPARMOR_PROFILE` when AppArmor is enabled in the
/// kernel, else `MVF_OCI_SELINUX_LABEL` when SELinux is. Nothing is loaded by MVF: the profile must
/// already exist, as with Docker's `--security-opt`.
fn detect_lsm() -> Option<Lsm> {
    let apparmor = std::fs::read_to_string("/sys/module/apparmor/parameters/enabled")
        .is_ok_and(|v| v.trim_start().starts_with('Y'));
    if apparmor && let Ok(profile) = std::env::var("MVF_OCI_APPARMOR_PROFILE") {
        return Some(Lsm::AppArmor(profile));
    }
    let selinux = Path::new("/sys/fs/selinux/enforce").exists();
    if selinux && let Ok(label) = std::env::var("MVF_OCI_SELINUX_LABEL") {
        return Some(Lsm::SeLinux(label));
    }
    None
}

/// The OCI backend.
#[derive(Debug, Clone)]
pub struct OciBackend {
    runtime: OciRuntime,
    store: ContentStore,
    state_dir: PathBuf,
    /// What composing the most recent rootfs did (layer reuse, links), for reports and tests.
    last_rootfs: std::sync::Arc<std::sync::Mutex<Option<mvf_oci::CacheReport>>>,
    /// Force the seccomp allowlist for every node (`MVF_OCI_SECCOMP=allowlist`).
    allowlist: bool,
    /// The security-module profile this host applies, when its kernel has the LSM enabled.
    lsm: Option<Lsm>,
    /// Enforced per-node networking (`network: restricted`), when this host can provide it.
    network: Option<network::NetworkSupport>,
}

impl OciBackend {
    /// A backend using `runtime`, images from `store` and scratch state (rootfs cache, bundles,
    /// runtime state) under `state_dir`.
    pub fn new(runtime: OciRuntime, store: ContentStore, state_dir: impl Into<PathBuf>) -> Self {
        Self {
            runtime,
            store,
            state_dir: state_dir.into(),
            last_rootfs: Default::default(),
            allowlist: std::env::var("MVF_OCI_SECCOMP").is_ok_and(|v| v == "allowlist"),
            lsm: detect_lsm(),
            network: network::NetworkSupport::detect(),
        }
    }

    /// Applies the seccomp allowlist to every node, whatever its contract says.
    pub fn with_syscall_allowlist(mut self, on: bool) -> Self {
        self.allowlist = on;
        self
    }

    /// Overrides the security-module profile (`None` disables it). The profile must exist on the
    /// host; the runtime refuses to start a container otherwise.
    pub fn with_lsm(mut self, lsm: Option<Lsm>) -> Self {
        self.lsm = lsm;
        self
    }

    /// What composing the most recent rootfs did (`None` until one was composed; a rootfs already
    /// in the cache is not composed again).
    pub fn last_rootfs(&self) -> Option<mvf_oci::CacheReport> {
        self.last_rootfs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Detects a runtime and returns a backend, or `None` when this host cannot run OCI images.
    pub fn detect(store: ContentStore, state_dir: impl Into<PathBuf>) -> Option<Self> {
        OciRuntime::detect().map(|r| Self::new(r, store, state_dir))
    }

    /// The runtime in use.
    pub fn runtime(&self) -> &OciRuntime {
        &self.runtime
    }

    fn find(&self, digest: &str) -> Result<ImageRecord, NodeError> {
        self.store
            .records()
            .map_err(|e| NodeError::new(NodeErrorKind::StartFailed, e.to_string()))?
            .into_iter()
            .find(|r| r.manifest.digest.as_str() == digest)
            .ok_or_else(|| {
                NodeError::new(
                    NodeErrorKind::StartFailed,
                    format!(
                        "image {digest} is not in the store (import it with `mvf-rs image import`)"
                    ),
                )
            })
    }
}

impl ExecutionBackend for OciBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            kind: BackendKind::Oci,
            // Namespaces + dropped capabilities + no-new-privs + seccomp make a security sandbox;
            // without seccomp support the runtime only gives a process-level boundary.
            isolation: if self.runtime.seccomp {
                IsolationLevel::Sandbox
            } else {
                IsolationLevel::Process
            },
            targets: vec![Target::Cpu],
            zero_copy_domains: vec![MemoryDomain::Host],
            cancellation: Cancellation::Enforceable,
            enforceable_termination: true,
            enforces_memory_limit: self.runtime.enforces("memory"),
            enforces_cpu_limit: self.runtime.enforces("cpu"),
            enforces_pids_limit: self.runtime.enforces("pids"),
            replacement: vec![Replacement::Stateless],
            grants: GrantSupport {
                network: {
                    let mut modes = vec![NetworkPolicy::None, NetworkPolicy::Host];
                    if self.network.is_some() {
                        modes.push(NetworkPolicy::Restricted);
                    }
                    modes
                },
                devices: grantable_devices(),
                // CDI injection is not implemented: no CDI device is ever offered.
                cdi_devices: vec![],
                lsm: self.lsm.iter().map(Lsm::grant).collect(),
            },
            private_arenas: true,
            display: Default::default(),
        }
    }

    fn instantiate(
        &self,
        request: &InstantiateRequest<'_>,
        artifact: &Artifact,
    ) -> Result<Box<dyn NodeHandle>, NodeError> {
        let Artifact::Oci { digest, .. } = artifact else {
            return Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "OCI backend received a non-OCI artifact",
            ));
        };
        let record = self.find(digest)?;
        let launcher = OciLauncher {
            backend: self.clone(),
            record,
            contract: request.contract.clone(),
            name: format!("{}-g{}", request.node_id, request.generation),
            args: None,
            container: None,
            binding: request.network.cloned(),
            network: None,
            logs: Some((
                request.services.clone(),
                request.node_id.to_string(),
                request.generation,
            )),
        };
        worker_node(request, Box::new(launcher), false)
    }
}

static IDS: AtomicU64 = AtomicU64::new(0);

/// Composes (once per manifest digest and id mapping) and returns the read-only rootfs. Layers come
/// from the layer-level cache (`<state>/layers[-u<id>]/<diff_id>`): each is unpacked once, shared
/// by every image built on it, and hard-linked into the rootfs — so a packaged node image on an
/// already cached base unpacks only its own layers. Mount points the sandbox needs are created
/// here, while the tree is still writable, and with a user namespace the tree is owned by the
/// mapped container root (as Docker's userns-remap does).
fn ensure_rootfs(
    backend: &OciBackend,
    record: &ImageRecord,
    userns: Option<IdMapping>,
) -> Result<PathBuf, NodeError> {
    let fail = |m: String| NodeError::new(NodeErrorKind::StartFailed, m);
    let dir = backend.state_dir.join("rootfs");
    std::fs::create_dir_all(&dir).map_err(|e| fail(format!("{}: {e}", dir.display())))?;
    let key = match userns {
        Some(m) => format!("{}-u{}", record.manifest.digest.hex(), m.host_id),
        None => record.manifest.digest.hex().to_string(),
    };
    let rootfs = dir.join(&key);
    #[cfg(unix)]
    if userns.is_some() {
        // The remapped container root must be able to traverse every ancestor of the rootfs.
        use std::os::unix::fs::PermissionsExt;
        let mut p = Some(dir.as_path());
        while let Some(d) = p {
            if let Ok(m) = std::fs::metadata(d)
                && m.permissions().mode() & 0o001 == 0
            {
                return Err(fail(format!(
                    "{} is not traversable by the user-namespace root (mode {:o}); put the OCI state directory elsewhere (MVF_OCI_STATE) or set MVF_OCI_USERNS=0",
                    d.display(),
                    m.permissions().mode() & 0o777
                )));
            }
            p = d.parent();
        }
    }
    if rootfs.is_dir() {
        return Ok(rootfs);
    }
    let tmp = dir.join(format!(
        "{key}.tmp-{}-{}",
        std::process::id(),
        IDS.fetch_add(1, Ordering::Relaxed)
    ));
    let layers = mvf_oci::LayerCache::new(backend.state_dir.join(match userns {
        Some(m) => format!("layers-u{}", m.host_id),
        None => "layers".into(),
    }));
    let owner = userns.map(|m| m.host_id);
    let chown_layer = |root: &Path| -> std::io::Result<()> {
        match userns {
            Some(m) => chown_tree(root, m.host_id),
            None => Ok(()),
        }
    };
    let report = layers
        .materialize(&backend.store, record, &tmp, &chown_layer, owner)
        .map_err(|e| fail(format!("compose rootfs: {e}")))?;
    *backend
        .last_rootfs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(report);
    let prepared = prepare_rootfs(&tmp, userns).map_err(|e| fail(format!("prepare rootfs: {e}")));
    if let Err(e) = prepared {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    if std::fs::rename(&tmp, &rootfs).is_err() {
        // Another preparation won the race; theirs is identical.
        let _ = std::fs::remove_dir_all(&tmp);
    }
    Ok(rootfs)
}

/// Creates the sandbox's mount points in a composed rootfs. Layer content already has its owner
/// (set once per cached layer), so only what is created here is chowned.
fn prepare_rootfs(root: &Path, userns: Option<IdMapping>) -> std::io::Result<()> {
    let mut created = Vec::new();
    for d in ["proc", "sys", "dev", "tmp", "mvf"] {
        let p = root.join(d);
        if std::fs::symlink_metadata(&p).is_err() {
            std::fs::create_dir_all(&p)?;
            created.push(p);
        }
    }
    let arena = root.join("mvf/arena");
    if std::fs::symlink_metadata(&arena).is_err() {
        std::fs::write(&arena, [])?;
        created.push(arena);
    }
    #[cfg(unix)]
    if let Some(m) = userns {
        std::os::unix::fs::lchown(root, Some(m.host_id), Some(m.host_id))?;
        for p in created {
            std::os::unix::fs::lchown(&p, Some(m.host_id), Some(m.host_id))?;
        }
    }
    #[cfg(not(unix))]
    let _ = (userns, created);
    Ok(())
}

/// Gives a whole tree to the mapped container root (one cached layer, once).
fn chown_tree(p: &Path, id: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::lchown(p, Some(id), Some(id))?;
        let meta = std::fs::symlink_metadata(p)?;
        if meta.is_dir() {
            for e in std::fs::read_dir(p)? {
                chown_tree(&e?.path(), id)?;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = (p, id);
    Ok(())
}

#[cfg(unix)]
fn device_node(grant: &mvf_contract::DeviceGrant) -> Result<DeviceNode, String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let m = std::fs::metadata(&grant.path).map_err(|e| format!("{}: {e}", grant.path))?;
    let kind = if m.file_type().is_char_device() {
        'c'
    } else if m.file_type().is_block_device() {
        'b'
    } else {
        return Err(format!("{} is not a device node", grant.path));
    };
    let rdev = m.rdev();
    // Linux dev_t encoding (glibc `major`/`minor`).
    let major = ((rdev >> 8) & 0xfff) | ((rdev >> 32) & !0xfff);
    let minor = (rdev & 0xff) | ((rdev >> 12) & !0xff);
    Ok(DeviceNode {
        path: grant.path.clone(),
        kind,
        major: major as i64,
        minor: minor as i64,
        access: grant.access,
    })
}

#[cfg(not(unix))]
fn device_node(grant: &mvf_contract::DeviceGrant) -> Result<DeviceNode, String> {
    Err(format!("{}: device grants need Linux", grant.path))
}

/// User namespace used when the host runs as root: container 0..65536 → host 100000..165536.
pub const USERNS: IdMapping = IdMapping {
    host_id: 100_000,
    size: 65_536,
};

struct OciLauncher {
    backend: OciBackend,
    record: ImageRecord,
    contract: NodeContract,
    name: String,
    args: Option<Vec<String>>,
    container: Option<(String, PathBuf)>,
    /// Deployment-time network binding.
    binding: Option<mvf_contract::NetworkBinding>,
    /// The enforced network, while the container exists.
    network: Option<network::NodeNetwork>,
    /// Where the effective policy and counters are reported.
    logs: Option<(mvf_engine::HostServices, String, u64)>,
}

impl OciLauncher {
    fn log(&self, message: String) {
        if let Some((services, node, generation)) = &self.logs {
            services.log(node, *generation, "supervisor", message);
        }
    }

    /// Prepares the enforced network of `network: restricted` (fails closed when unavailable).
    fn prepare_network(&mut self, id: &str) -> Result<Option<PathBuf>, NodeError> {
        let fail = |m: String| NodeError::new(NodeErrorKind::StartFailed, m);
        match self.contract.spec.permissions.network.mode() {
            NetworkPolicy::Restricted => {
                let support = self.backend.network.clone().ok_or_else(|| {
                    fail("`network: restricted` needs root, `ip` and `nft` on this host".into())
                })?;
                let policy = network::Policy::resolve(
                    &self.contract.spec.permissions.network,
                    self.binding.as_ref(),
                )
                .map_err(fail)?;
                // Generations of one pipeline node share its host ports (handover on promotion);
                // a generic workload owns them alone.
                let owner = self
                    .logs
                    .as_ref()
                    .map_or_else(|| id.to_string(), |(_, node, _)| node.clone());
                let net =
                    network::NodeNetwork::setup(&support, &self.backend.state_dir, &owner, policy)
                        .map_err(|e| fail(format!("network: {e}")))?;
                self.log(net.describe());
                if !net.publishes() {
                    let ports: Vec<String> = net
                        .policy
                        .host_ports()
                        .iter()
                        .map(|(p, n)| format!("{}/{n}", p.as_str()))
                        .collect();
                    self.log(format!(
                        "network: standby until promoted (the previous generation publishes {})",
                        ports.join(", ")
                    ));
                }
                let path = net.netns_path();
                self.network = Some(net);
                Ok(Some(path))
            }
            NetworkPolicy::Host => {
                self.log("network: host (the host's network, not isolated)".into());
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn release_network(&mut self) {
        if let Some(mut net) = self.network.take() {
            let counters = net.counters();
            if !counters.is_empty() {
                let text: Vec<String> = counters
                    .iter()
                    .map(|(k, p, b)| format!("{k}={p}p/{b}B"))
                    .collect();
                self.log(format!("network counters: {}", text.join(" ")));
            }
            net.teardown();
        }
    }
    fn bundle(&mut self, arena: &Path) -> Result<LaunchSpec, NodeError> {
        let fail = |m: String| NodeError::new(NodeErrorKind::StartFailed, m);
        let userns = (self.backend.runtime.root
            && std::env::var_os("MVF_OCI_USERNS").is_none_or(|v| v != "0"))
        .then_some(USERNS);
        let rootfs = ensure_rootfs(&self.backend, &self.record, userns)?;
        let config = mvf_oci::read_config(&self.backend.store, &self.record)
            .map_err(|e| fail(e.to_string()))?
            .config
            .unwrap_or_default();
        let devices = self
            .contract
            .spec
            .permissions
            .devices
            .iter()
            .map(device_node)
            .collect::<Result<Vec<_>, _>>()
            .map_err(fail)?;
        #[cfg(unix)]
        if let Some(m) = userns {
            // Container root (host id `m.host_id`) must be able to write its output slots.
            std::os::unix::fs::chown(arena, Some(m.host_id), Some(m.host_id))
                .map_err(|e| fail(format!("chown arena: {e}")))?;
        }
        let id = format!(
            "mvf-{}-{}-{}",
            self.name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                })
                .collect::<String>(),
            std::process::id(),
            IDS.fetch_add(1, Ordering::Relaxed)
        );
        let netns = self.prepare_network(&id)?;
        let mut extra_env = vec![
            "PYTHONUNBUFFERED=1".to_string(),
            "PYTHONDONTWRITEBYTECODE=1".to_string(),
        ];
        if let Some(ip) = self.binding.as_ref().and_then(|b| b.advertise.clone()) {
            extra_env.push(format!("MVF_ADVERTISE_IP={ip}"));
        }
        let spec = runtime_spec(&SpecInput {
            rootfs: &rootfs,
            arena,
            image: &config,
            contract: &self.contract,
            args: self.args.clone(),
            extra_env,
            devices,
            userns,
            hostname: "mvf-node".into(),
            syscalls: if self.backend.allowlist {
                mvf_contract::SyscallPolicy::Allowlist
            } else {
                self.contract.spec.isolation.syscalls
            },
            lsm: self.backend.lsm.clone(),
            netns,
        })
        .map_err(|e| {
            self.release_network();
            fail(e)
        })?;
        let bundle = self.backend.state_dir.join("bundles").join(&id);
        std::fs::create_dir_all(&bundle).map_err(|e| fail(format!("{}: {e}", bundle.display())))?;
        std::fs::write(bundle.join("config.json"), spec.to_string())
            .map_err(|e| fail(format!("write config.json: {e}")))?;
        let root = self.backend.state_dir.join("runtime");
        self.container = Some((id.clone(), bundle.clone()));
        Ok(LaunchSpec {
            program: self.backend.runtime.path.clone().into_os_string(),
            args: vec![
                OsString::from("--root"),
                root.into_os_string(),
                "run".into(),
                "--bundle".into(),
                bundle.clone().into_os_string(),
                id.into(),
            ],
            cwd: bundle,
            // runc starts the container with the bundle's explicit environment only.
            env: vec![],
            remove_env: vec![],
        })
    }
}

impl Launcher for OciLauncher {
    fn prepare(&mut self, arena: &Path) -> Result<LaunchSpec, NodeError> {
        self.bundle(arena)
    }

    fn cleanup(&mut self) {
        if let Some((id, bundle)) = self.container.take() {
            delete_container(&self.backend, &id);
            let _ = std::fs::remove_dir_all(bundle);
        }
        self.release_network();
    }

    fn promote(&mut self) -> Result<(), NodeError> {
        let Some(net) = self.network.as_mut() else {
            return Ok(());
        };
        if net.publishes() {
            return Ok(());
        }
        let t0 = Instant::now();
        net.promote().map_err(|e| {
            NodeError::new(NodeErrorKind::StartFailed, format!("network handover: {e}"))
        })?;
        let message = format!(
            "network: promoted, new connections reach {} (handover {} us)",
            net.node_ip,
            t0.elapsed().as_micros()
        );
        self.log(message);
        Ok(())
    }
}

fn delete_container(backend: &OciBackend, id: &str) {
    let _ = Command::new(&backend.runtime.path)
        .arg("--root")
        .arg(backend.state_dir.join("runtime"))
        .args(["delete", "--force", id])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Lists containers the runtime still knows about (for leak checks).
pub fn list_containers(backend: &OciBackend) -> Vec<String> {
    Command::new(&backend.runtime.path)
        .arg("--root")
        .arg(backend.state_dir.join("runtime"))
        .args(["list", "-q"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::to_string)
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Result of a generic (non-MVF) workload run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkloadOutput {
    /// Exit code of the container process (128+signal when killed).
    pub exit_code: Option<i32>,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
    /// The run hit the timeout and was killed.
    pub timed_out: bool,
}

/// Runs an image's entrypoint (or `args`) to completion under the same sandbox as nodes. This is
/// the *generic workload* compatibility level: the image does not need to speak the MVF protocol.
pub fn run_workload(
    backend: &OciBackend,
    record: &ImageRecord,
    contract: &NodeContract,
    args: Option<Vec<String>>,
    timeout: Duration,
) -> Result<WorkloadOutput, NodeError> {
    run_workload_with(backend, record, contract, args, timeout, None)
}

/// [`run_workload`] with a deployment-time network binding (published ports, advertised address).
pub fn run_workload_with(
    backend: &OciBackend,
    record: &ImageRecord,
    contract: &NodeContract,
    args: Option<Vec<String>>,
    timeout: Duration,
    binding: Option<&mvf_contract::NetworkBinding>,
) -> Result<WorkloadOutput, NodeError> {
    let scratch = backend.state_dir.join("workload-arenas");
    std::fs::create_dir_all(&scratch).map_err(|e| NodeError::failed(e.to_string()))?;
    let arena = scratch.join(format!(
        "{}-{}",
        std::process::id(),
        IDS.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&arena, []).map_err(|e| NodeError::failed(e.to_string()))?;
    let mut launcher = OciLauncher {
        backend: backend.clone(),
        record: record.clone(),
        contract: contract.clone(),
        name: "workload".into(),
        args,
        container: None,
        binding: binding.cloned(),
        network: None,
        logs: None,
    };
    let spec = launcher.bundle(&arena)?;
    let mut child = Command::new(&spec.program)
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| NodeError::new(NodeErrorKind::StartFailed, e.to_string()))?;
    let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
    let t_out = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(o) = out.as_mut() {
            let _ = std::io::Read::read_to_string(o, &mut s);
        }
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = err.as_mut() {
            let _ = std::io::Read::read_to_string(e, &mut s);
        }
        s
    });
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Ok(Some(s)) = child.try_wait() {
            break Some(s);
        }
        if started.elapsed() > timeout {
            timed_out = true;
            launcher.cleanup();
            let _ = child.kill();
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    launcher.cleanup();
    let _ = std::fs::remove_file(&arena);
    Ok(WorkloadOutput {
        exit_code: status.and_then(|s| s.code()),
        stdout: t_out.join().unwrap_or_default(),
        stderr: t_err.join().unwrap_or_default(),
        timed_out,
    })
}
