//! Provisioning (#31): retention, artifact pinning and verification, host-path safety, scratch
//! lifecycle and crash sweep. Portable (macOS, Linux, Windows).

use std::path::{Path, PathBuf};
use std::process::Command;

use mvf_contract::{BoundMount, MountAccess, MountKind, StorageMount, VolumeDecl, VolumeKind};
use mvf_storage::*;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mvf-storage-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp");
    // Canonical, so comparisons survive /var -> /private/var on macOS.
    std::fs::canonicalize(&dir).expect("canonical")
}

fn volume(name: &str, kind: VolumeKind) -> VolumeDecl {
    VolumeDecl {
        name: name.into(),
        kind,
        writers: vec![],
        host_path: None,
        access: None,
        file: None,
        digest: None,
    }
}

fn mount(name: &str, access: MountAccess, volume: Option<VolumeDecl>) -> BoundMount {
    BoundMount {
        mount: StorageMount {
            name: name.into(),
            target: format!("/{name}"),
            access,
            kind: if volume.is_some() {
                MountKind::Volume
            } else {
                MountKind::Scratch
            },
            max_size: volume.is_none().then(|| "1MiB".into()),
            required: true,
        },
        volume,
        access,
        scope: "plant".into(),
    }
}

fn sha256(bytes: &[u8]) -> String {
    mvf_oci::Digest::of(bytes).to_string()
}

#[test]
fn persistent_data_is_retained_until_explicitly_deleted() {
    let root = temp("persistent");
    let results = mount(
        "results",
        MountAccess::ReadWrite,
        Some(volume("results", VolumeKind::Persistent)),
    );
    {
        let s = StorageService::new(&root);
        let g = s
            .provision(
                "insp",
                1,
                std::slice::from_ref(&results),
                None,
                Scratch::HostDir,
                None,
            )
            .expect("provision");
        let dir = g.mounts[0].host_path.clone().expect("path");
        assert_eq!(g.mounts[0].env, "MVF_VOLUME_RESULTS");
        std::fs::write(dir.join("seq-1.json"), b"{}").expect("write");
    }
    // A new engine (a restart, a replacement, the next run) sees the same data.
    let s = StorageService::new(&root);
    let g = s
        .provision(
            "insp",
            7,
            std::slice::from_ref(&results),
            None,
            Scratch::HostDir,
            None,
        )
        .expect("again");
    let dir = g.mounts[0].host_path.clone().expect("path");
    assert!(
        dir.join("seq-1.json").is_file(),
        "retained across engines and generations"
    );
    let listed = list_volumes(&root);
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (
            listed[0].scope.as_str(),
            listed[0].name.as_str(),
            listed[0].files
        ),
        ("plant", "results", 1)
    );
    // Changing its type is refused rather than silently reinterpreting the data.
    let as_shared = mount(
        "results",
        MountAccess::ReadWrite,
        Some(volume("results", VolumeKind::Shared)),
    );
    let err = s
        .provision("insp", 8, &[as_shared], None, Scratch::HostDir, None)
        .expect_err("type change");
    assert!(err.to_string().contains("exists as `persistent`"), "{err}");
    drop(g);
    assert_eq!(delete_volume(&root, "plant", "results").expect("delete"), 2);
    assert!(list_volumes(&root).is_empty());
}

#[test]
fn artifacts_are_verified_deduplicated_and_pinned_per_generation() {
    let root = temp("artifact");
    let package = temp("artifact-pkg");
    std::fs::create_dir_all(package.join("models")).expect("dir");
    std::fs::write(package.join("models/v1.onnx"), b"model-one").expect("v1");
    std::fs::write(package.join("models/v2.onnx"), b"model-two").expect("v2");
    let artifact = |name: &str, file: &str, digest: String| {
        mount(
            "model",
            MountAccess::ReadOnly,
            Some(VolumeDecl {
                file: Some(file.into()),
                digest: Some(digest),
                ..volume(name, VolumeKind::Artifact)
            }),
        )
    };
    let s = StorageService::new(&root);
    let v1 = artifact("model-v1", "models/v1.onnx", sha256(b"model-one"));
    let v2 = artifact("model-v2", "models/v2.onnx", sha256(b"model-two"));
    let g1 = s
        .provision(
            "insp",
            1,
            std::slice::from_ref(&v1),
            Some(&package),
            Scratch::HostDir,
            None,
        )
        .expect("v1");
    let g2 = s
        .provision("insp", 2, &[v2], Some(&package), Scratch::HostDir, None)
        .expect("v2");
    let view = |g: &Grant| g.mounts[0].host_path.clone().expect("view");
    // Both generations are pinned to their own content at once (replacement + rollback window).
    assert_eq!(
        std::fs::read(view(&g1).join("v1.onnx")).expect("read"),
        b"model-one"
    );
    assert_eq!(
        std::fs::read(view(&g2).join("v2.onnx")).expect("read"),
        b"model-two"
    );
    assert!(g1.mounts[0].read_only);
    // Read-only by permission (root bypasses file permissions; a sandbox adds a kernel read-only
    // bind on top, see the OCI tests).
    #[cfg(unix)]
    let root_user = unsafe { libc::geteuid() } == 0;
    #[cfg(not(unix))]
    let root_user = false;
    if !root_user {
        assert!(
            std::fs::write(view(&g1).join("v1.onnx"), b"tamper").is_err(),
            "the artifact file is read-only"
        );
    }
    // A second generation of v1 reuses the stored blob.
    let again = s
        .provision("insp", 3, &[v1], Some(&package), Scratch::HostDir, None)
        .expect("again");
    assert_eq!(list_artifacts(&root).len(), 2, "deduplicated");
    let (old_view, newer) = (view(&g1), view(&again));
    drop(g1);
    assert!(!old_view.exists(), "a generation's view goes with it");
    assert!(newer.join("v1.onnx").is_file(), "the blob itself stays");

    // Content that does not match the pinned digest is never mounted.
    std::fs::write(package.join("models/v3.onnx"), b"model-three").expect("v3");
    let wrong = artifact("model-v3", "models/v3.onnx", sha256(b"something else"));
    let err = s
        .provision("insp", 4, &[wrong], Some(&package), Scratch::HostDir, None)
        .expect_err("mismatch");
    assert!(err.to_string().contains("digest"), "{err}");
    let escape = artifact("x", "../outside.onnx", sha256(b"x"));
    assert!(
        s.provision("insp", 5, &[escape], Some(&package), Scratch::HostDir, None)
            .is_err()
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc/hosts", package.join("models/link.onnx"))
            .expect("symlink");
        let linked = artifact("link", "models/link.onnx", sha256(b"x"));
        let err = s
            .provision("insp", 6, &[linked], Some(&package), Scratch::HostDir, None)
            .expect_err("symlink");
        assert!(err.to_string().contains("not a regular file"), "{err}");
    }
}

#[test]
fn host_binds_refuse_system_paths_home_and_symlinks() {
    let root = temp("bind");
    let s = StorageService::new(&root);
    let bind = |path: &Path| {
        mount(
            "camera",
            MountAccess::ReadOnly,
            Some(VolumeDecl {
                host_path: Some(path.to_string_lossy().to_string()),
                ..volume("camera", VolumeKind::Bind)
            }),
        )
    };
    let camera = temp("bind-camera");
    let g = s
        .provision("cam", 1, &[bind(&camera)], None, Scratch::HostDir, None)
        .expect("approved dir");
    assert_eq!(g.mounts[0].host_path.as_deref(), Some(camera.as_path()));
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        let home = std::fs::canonicalize(home).expect("home");
        let err = s
            .provision("cam", 2, &[bind(&home)], None, Scratch::HostDir, None)
            .expect_err("home");
        assert!(
            err.to_string().contains("home") || err.to_string().contains("system"),
            "{err}"
        );
    }
    let missing = camera.join("does-not-exist");
    assert!(
        s.provision("cam", 3, &[bind(&missing)], None, Scratch::HostDir, None)
            .is_err()
    );
    #[cfg(unix)]
    {
        let link = temp("bind-link").join("link");
        std::os::unix::fs::symlink(&camera, &link).expect("symlink");
        let err = s
            .provision("cam", 4, &[bind(&link)], None, Scratch::HostDir, None)
            .expect_err("symlink");
        assert!(err.to_string().contains("symlink"), "{err}");
        let err = s
            .provision(
                "cam",
                5,
                &[bind(Path::new("/etc"))],
                None,
                Scratch::HostDir,
                None,
            )
            .expect_err("system");
        assert!(err.to_string().contains("system directory"), "{err}");
    }
}

#[test]
fn scratch_is_fresh_per_generation_and_gone_after_it() {
    let root = temp("scratch");
    let s = StorageService::new(&root);
    let tmp = mount("tmp", MountAccess::ReadWrite, None);
    let mut g = s
        .provision(
            "insp",
            1,
            std::slice::from_ref(&tmp),
            None,
            Scratch::HostDir,
            None,
        )
        .expect("scratch");
    let dir = g.mounts[0].host_path.clone().expect("dir");
    std::fs::write(dir.join("big.bin"), vec![0u8; 2 << 20]).expect("write");
    let notes = g.release();
    assert!(!dir.exists(), "removed when the generation goes");
    assert!(notes[0].contains("advisory 1 MiB"), "{notes:?}");
    // A sandbox gets a tmpfs instead: nothing on the host.
    let sandbox = s
        .provision("insp", 2, &[tmp], None, Scratch::Tmpfs, None)
        .expect("tmpfs");
    assert_eq!(sandbox.mounts[0].host_path, None);
    assert_eq!(
        sandbox.mounts[0].source,
        Source::Scratch { max_bytes: 1 << 20 }
    );
}

#[test]
fn what_a_dead_engine_left_is_swept_but_volume_data_is_not() {
    let root = temp("sweep");
    let s = StorageService::new(&root);
    let results = mount(
        "results",
        MountAccess::ReadWrite,
        Some(volume("results", VolumeKind::Persistent)),
    );
    let g = s
        .provision("insp", 1, &[results], None, Scratch::HostDir, None)
        .expect("volume");
    std::fs::write(g.mounts[0].host_path.clone().expect("p").join("keep"), b"1").expect("write");
    let mut child = if cfg!(windows) {
        Command::new("cmd").args(["/C", "exit"]).spawn()
    } else {
        Command::new("true").spawn()
    }
    .expect("spawn");
    let dead = child.id();
    child.wait().expect("wait");
    let stale = root.join(format!("runs/run-{dead}-000000001"));
    std::fs::create_dir_all(stale.join("scratch/insp-g1-1-tmp")).expect("stale");
    std::fs::write(stale.join("owner"), dead.to_string()).expect("owner");
    assert_eq!(sweep(&root), 1);
    assert!(!stale.exists());
    assert_eq!(list_volumes(&root)[0].files, 1, "volume data untouched");
}

/// `MVF_STORAGE_BENCH=1` (`--release`): provisioning cost of a 64 MiB model artifact — the first
/// start imports and verifies it, later starts only link the stored blob into a view.
#[test]
fn bench_artifact_provisioning() {
    if std::env::var_os("MVF_STORAGE_BENCH").is_none_or(|v| v != "1") {
        return;
    }
    let root = temp("bench-root");
    let package = temp("bench-pkg");
    let data: Vec<u8> = (0..64u32 << 20)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    std::fs::write(package.join("big.onnx"), &data).expect("model");
    let m = mount(
        "model",
        MountAccess::ReadOnly,
        Some(VolumeDecl {
            file: Some("big.onnx".into()),
            digest: Some(sha256(&data)),
            ..volume("big", VolumeKind::Artifact)
        }),
    );
    let s = StorageService::new(&root);
    let t = std::time::Instant::now();
    let first = s
        .provision(
            "n",
            1,
            std::slice::from_ref(&m),
            Some(&package),
            Scratch::HostDir,
            None,
        )
        .expect("first");
    let import = t.elapsed();
    let mut cached = Vec::new();
    for g in 2..12 {
        let t = std::time::Instant::now();
        let grant = s
            .provision(
                "n",
                g,
                std::slice::from_ref(&m),
                Some(&package),
                Scratch::HostDir,
                None,
            )
            .expect("again");
        cached.push(t.elapsed());
        drop(grant);
    }
    cached.sort();
    eprintln!(
        "64 MiB artifact: first start (import + SHA-256 verify) {:.1} ms; later starts (link into a view) p50 {:.3} ms; store holds it once ({} blob)",
        import.as_secs_f64() * 1e3,
        cached[cached.len() / 2].as_secs_f64() * 1e3,
        list_artifacts(&root).len()
    );
    drop(first);
}
