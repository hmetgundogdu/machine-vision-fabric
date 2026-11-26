//! OCI import/export/unpack: digest verification, malformed and hostile archives, platform
//! selection, cache reuse and cleanup, round trips. Hermetic: images are built in the test.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use mvf_oci::*;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mvf-oci-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("tmp");
    d
}

fn linux(arch: &str) -> Platform {
    Platform {
        os: "linux".into(),
        architecture: arch.into(),
        variant: None,
    }
}

fn exec() -> ExecConfig {
    ExecConfig {
        entrypoint: Some(vec!["/bin/app".into()]),
        cmd: Some(vec!["--serve".into()]),
        env: Some(vec!["PATH=/usr/bin:/bin".into(), "MODE=test".into()]),
        working_dir: Some("/work".into()),
        user: Some("1000:1000".into()),
        labels: Some(BTreeMap::from([(
            NODE_CONTRACT_LABEL.to_string(),
            "/mvf/node.yaml".to_string(),
        )])),
    }
}

/// A two-layer image: base files, then an upper layer with a whiteout, a symlink and a hard link.
fn sample(store: &ContentStore, name: &str, arch: &str) -> ImageRecord {
    let base = LayerBuilder::new()
        .dir("bin")
        .file("bin/app", b"#!/bin/sh\necho app\n", 0o755)
        .dir("etc")
        .file("etc/old.conf", b"old", 0o644)
        .dir("data")
        .file("data/a", b"A", 0o644)
        .file("data/b", b"B", 0o644)
        .gzip()
        .expect("base layer");
    let upper = LayerBuilder::new()
        .file("etc/.wh.old.conf", b"", 0o644)
        .dir("mvf")
        .file("mvf/node.yaml", b"apiVersion: mvf.dev/v1alpha1\n", 0o644)
        .symlink("current", "mvf")
        .entry(FileEntry::HardLink {
            path: "bin/app2".into(),
            target: "bin/app".into(),
        })
        .file("data/.wh..wh..opq", b"", 0o644)
        .file("data/c", b"C", 0o644)
        .uncompressed()
        .expect("upper layer");
    mvf_oci::create_image(store, name, &linux(arch), &exec(), &[base, upper]).expect("image")
}

fn blobs(store: &ContentStore) -> usize {
    std::fs::read_dir(store.root().join("blobs/sha256"))
        .expect("blobs")
        .count()
}

#[test]
fn oci_layout_round_trip_preserves_every_digest() {
    let dir = tmp("roundtrip");
    let a = ContentStore::open(dir.join("a")).expect("store");
    let original = sample(&a, "mvf/sample:1", "arm64");
    let tar = dir.join("image.tar");
    export(&a, &original, &tar, ArchiveFormat::OciLayout).expect("export");

    let b = ContentStore::open(dir.join("b")).expect("store");
    let imported = import(&b, &tar, &linux("arm64"), None).expect("import");
    assert_eq!(
        imported.name, "docker.io/mvf/sample:1",
        "names are stored fully qualified, as Docker does"
    );
    assert_eq!(imported.manifest.digest, original.manifest.digest);
    assert_eq!(imported.config.digest, original.config.digest);
    assert_eq!(imported.layers, original.layers);
    assert_eq!(imported.source, "oci-layout");
    let config = read_config(&b, &imported).expect("config");
    assert_eq!(
        config.config,
        Some(exec()),
        "entrypoint/env/user/workdir/labels round-trip"
    );
}

#[test]
fn docker_archive_round_trip_and_format_detection() {
    let dir = tmp("docker");
    let a = ContentStore::open(dir.join("a")).expect("store");
    let original = sample(&a, "mvf/sample:docker", "arm64");
    let tar = dir.join("docker.tar");
    export(&a, &original, &tar, ArchiveFormat::Docker).expect("export");
    let b = ContentStore::open(dir.join("b")).expect("store");
    let imported = import(&b, &tar, &linux("arm64"), None).expect("import");
    assert_eq!(imported.source, "docker-archive");
    assert_eq!(imported.manifest.digest, original.manifest.digest);
}

fn legacy_archive(path: &Path, tamper: bool, alias: bool) -> String {
    let layer = LayerBuilder::new()
        .file("hello.txt", b"hello from a legacy docker save\n", 0o644)
        .tar_bytes()
        .expect("layer");
    let diff = Digest::of(&layer);
    let config = serde_json::json!({
        "architecture": "arm64", "os": "linux",
        "config": {"Cmd": ["cat", "/hello.txt"]},
        "rootfs": {"type": "layers", "diff_ids": [diff, diff]},
    })
    .to_string();
    let config_digest = Digest::of(config.as_bytes());
    let file = std::fs::File::create(path).expect("create");
    let mut b = tar::Builder::new(file);
    let mut add = |name: &str, bytes: &[u8]| {
        let mut h = tar::Header::new_gnu();
        h.set_size(bytes.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, name, bytes).expect("append");
    };
    add(
        "manifest.json",
        serde_json::json!([{
            "Config": format!("{}.json", config_digest.hex()),
            "RepoTags": ["legacy/hello:1"],
            "Layers": ["l1/layer.tar", "l2/layer.tar"],
        }])
        .to_string()
        .as_bytes(),
    );
    add(&format!("{}.json", config_digest.hex()), config.as_bytes());
    let mut first = layer.clone();
    if tamper {
        first[600] ^= 0xff;
    }
    add("l1/layer.tar", &first);
    if alias {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        h.set_mode(0o777);
        b.append_link(&mut h, "l2/layer.tar", "../l1/layer.tar")
            .expect("link");
    } else {
        add("l2/layer.tar", &layer);
    }
    b.into_inner().expect("finish").flush().expect("flush");
    config_digest.to_string()
}

#[test]
fn legacy_docker_archive_is_converted_and_verified_against_diff_ids() {
    let dir = tmp("legacy");
    let store = ContentStore::open(dir.join("store")).expect("store");
    let path = dir.join("legacy.tar");
    let config_digest = legacy_archive(&path, false, true);
    let record = import(&store, &path, &linux("arm64"), None).expect("legacy import");
    assert_eq!(record.name, "legacy/hello:1");
    assert_eq!(record.source, "docker-archive-legacy");
    assert_eq!(record.config.digest.to_string(), config_digest);
    assert_eq!(record.layers.len(), 2, "symlinked duplicate layer resolved");

    let tampered = dir.join("tampered.tar");
    legacy_archive(&tampered, true, false);
    let err = import(&store, &tampered, &linux("arm64"), None).expect_err("tampered");
    assert!(matches!(err, OciError::DigestMismatch { .. }), "{err}");
}

/// Writes an OCI layout tar from raw parts so tests can corrupt any piece.
fn raw_layout(
    path: &Path,
    index: &serde_json::Value,
    blobs: &[(String, Vec<u8>)],
    extra: &[(&str, &[u8])],
) {
    let file = std::fs::File::create(path).expect("create");
    let mut b = tar::Builder::new(file);
    let mut add = |name: &str, bytes: &[u8]| {
        let mut h = tar::Header::new_gnu();
        h.set_size(bytes.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, name, bytes).expect("append");
    };
    add("oci-layout", br#"{"imageLayoutVersion":"1.0.0"}"#);
    add("index.json", index.to_string().as_bytes());
    for (hex, bytes) in blobs {
        add(&format!("blobs/sha256/{hex}"), bytes);
    }
    for (name, bytes) in extra {
        // Raw header name: the tar crate itself refuses to write hostile paths.
        let mut h = tar::Header::new_gnu();
        h.set_size(bytes.len() as u64);
        h.set_mode(0o644);
        let field = &mut h.as_old_mut().name;
        field[..name.len()].copy_from_slice(name.as_bytes());
        h.set_cksum();
        b.append(&h, *bytes).expect("append raw");
    }
    b.into_inner().expect("finish").flush().expect("flush");
}

/// Manifest + config + one layer as (index, blobs).
fn parts(arch: &str, layer_media: &str) -> (serde_json::Value, Vec<(String, Vec<u8>)>) {
    let layer = LayerBuilder::new()
        .file("x", b"x", 0o644)
        .tar_bytes()
        .expect("layer");
    let ld = Digest::of(&layer);
    let config = serde_json::json!({"architecture": arch, "os": "linux", "rootfs": {"type": "layers", "diff_ids": [ld]}}).to_string();
    let cd = Digest::of(config.as_bytes());
    let manifest = serde_json::json!({
        "schemaVersion": 2, "mediaType": media::OCI_MANIFEST,
        "config": {"mediaType": media::OCI_CONFIG, "digest": cd, "size": config.len()},
        "layers": [{"mediaType": layer_media, "digest": ld, "size": layer.len()}],
    })
    .to_string();
    let md = Digest::of(manifest.as_bytes());
    let index = serde_json::json!({"schemaVersion": 2, "manifests": [{
        "mediaType": media::OCI_MANIFEST, "digest": md, "size": manifest.len(),
        "platform": {"os": "linux", "architecture": arch},
    }]});
    (
        index,
        vec![
            (md.hex().to_string(), manifest.into_bytes()),
            (cd.hex().to_string(), config.into_bytes()),
            (ld.hex().to_string(), layer),
        ],
    )
}

#[test]
fn corrupted_blob_is_rejected_by_digest() {
    let dir = tmp("corrupt");
    let store = ContentStore::open(dir.join("s")).expect("store");
    let (index, mut blobs) = parts("arm64", media::OCI_LAYER);
    blobs[2].1[0] ^= 1;
    raw_layout(&dir.join("bad.tar"), &index, &blobs, &[]);
    let err = import(&store, &dir.join("bad.tar"), &linux("arm64"), None).expect_err("corrupt");
    assert!(matches!(err, OciError::DigestMismatch { .. }), "{err}");
    assert_eq!(blobs_in_tmp(&store), 0, "no partial blob left behind");
}

fn blobs_in_tmp(store: &ContentStore) -> usize {
    std::fs::read_dir(store.root().join("tmp"))
        .expect("tmp")
        .count()
}

#[test]
fn descriptor_size_mismatch_is_rejected() {
    let dir = tmp("size");
    let store = ContentStore::open(dir.join("s")).expect("store");
    let (mut index, blobs) = parts("arm64", media::OCI_LAYER);
    index["manifests"][0]["size"] = serde_json::json!(1);
    raw_layout(&dir.join("bad.tar"), &index, &blobs, &[]);
    let err = import(&store, &dir.join("bad.tar"), &linux("arm64"), None).expect_err("size");
    assert!(matches!(err, OciError::SizeMismatch { .. }), "{err}");
}

#[test]
fn malformed_and_traversing_archives_fail_safely() {
    let dir = tmp("malformed");
    let store = ContentStore::open(dir.join("s")).expect("store");
    std::fs::write(dir.join("junk.tar"), b"this is not a tar archive at all").expect("write");
    assert!(import(&store, &dir.join("junk.tar"), &linux("arm64"), None).is_err());

    let (index, blobs) = parts("arm64", media::OCI_LAYER);
    raw_layout(
        &dir.join("evil.tar"),
        &index,
        &blobs,
        &[("blobs/../../../evil", b"x")],
    );
    let err = import(&store, &dir.join("evil.tar"), &linux("arm64"), None).expect_err("traversal");
    assert!(matches!(err, OciError::UnsafeEntry { .. }), "{err}");
    assert!(!dir.join("evil").exists());

    let empty = dir.join("empty.tar");
    tar::Builder::new(std::fs::File::create(&empty).expect("f"))
        .finish()
        .expect("finish");
    assert!(matches!(
        import(&store, &empty, &linux("arm64"), None).expect_err("empty"),
        OciError::Malformed(_)
    ));
}

#[test]
fn platform_is_selected_from_a_multi_platform_index() {
    let dir = tmp("platform");
    let store = ContentStore::open(dir.join("s")).expect("store");
    let (i1, mut b1) = parts("amd64", media::OCI_LAYER);
    let (i2, b2) = parts("arm64", media::OCI_LAYER);
    let index = serde_json::json!({"schemaVersion": 2, "mediaType": media::OCI_INDEX,
        "manifests": [i1["manifests"][0].clone(), i2["manifests"][0].clone()]});
    b1.extend(b2);
    raw_layout(&dir.join("multi.tar"), &index, &b1, &[]);
    let arm = import(
        &store,
        &dir.join("multi.tar"),
        &linux("arm64"),
        Some("multi:arm"),
    )
    .expect("arm64");
    assert_eq!(arm.platform.architecture, "arm64");
    let amd = import(
        &store,
        &dir.join("multi.tar"),
        &linux("amd64"),
        Some("multi:amd"),
    )
    .expect("amd64");
    assert_eq!(amd.platform.architecture, "amd64");
    assert_ne!(arm.manifest.digest, amd.manifest.digest);
    match import(&store, &dir.join("multi.tar"), &linux("riscv64"), None) {
        Err(OciError::Platform(msg)) => assert!(
            msg.contains("linux/amd64") && msg.contains("linux/arm64"),
            "{msg}"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn zstd_layers_fail_closed() {
    let dir = tmp("zstd");
    let store = ContentStore::open(dir.join("s")).expect("store");
    let (index, blobs) = parts("arm64", media::OCI_LAYER_ZSTD);
    raw_layout(&dir.join("z.tar"), &index, &blobs, &[]);
    assert!(matches!(
        import(&store, &dir.join("z.tar"), &linux("arm64"), None).expect_err("zstd"),
        OciError::Unsupported(_)
    ));
}

#[test]
fn import_is_deduplicated_and_gc_cleans_up() {
    let dir = tmp("gc");
    let a = ContentStore::open(dir.join("a")).expect("store");
    let original = sample(&a, "mvf/sample:gc", "arm64");
    export(&a, &original, &dir.join("i.tar"), ArchiveFormat::OciLayout).expect("export");
    let b = ContentStore::open(dir.join("b")).expect("store");
    import(&b, &dir.join("i.tar"), &linux("arm64"), Some("one")).expect("1");
    let after_first = blobs(&b);
    import(&b, &dir.join("i.tar"), &linux("arm64"), Some("two")).expect("2");
    assert_eq!(blobs(&b), after_first, "identical content is stored once");
    assert_eq!(after_first, 4, "manifest + config + 2 layers");

    b.remove("one").expect("rm");
    assert_eq!(
        b.gc().expect("gc").removed,
        0,
        "`two` still references every blob"
    );
    b.remove("two").expect("rm");
    let report = b.gc().expect("gc");
    assert_eq!((report.removed, report.kept), (4, 0));
    assert_eq!(blobs(&b), 0);
}

#[test]
fn tampering_on_disk_is_detected() {
    let dir = tmp("tamper");
    let store = ContentStore::open(dir.join("s")).expect("store");
    let r = sample(&store, "t:1", "arm64");
    store.verify_blob(&r.layers[0].digest).expect("intact");
    let path = store.blob_path(&r.layers[0].digest);
    let mut bytes = std::fs::read(&path).expect("read");
    bytes[10] ^= 0xff;
    std::fs::write(&path, bytes).expect("write");
    assert!(matches!(
        store.verify_blob(&r.layers[0].digest),
        Err(OciError::DigestMismatch { .. })
    ));
    assert!(export(&store, &r, &dir.join("x.tar"), ArchiveFormat::OciLayout).is_err());
}

#[cfg(unix)]
mod unpacking {
    use super::*;

    #[test]
    fn layers_whiteouts_symlinks_and_hardlinks_apply() {
        let dir = tmp("unpack");
        let store = ContentStore::open(dir.join("s")).expect("store");
        let r = sample(&store, "u:1", "arm64");
        let root = dir.join("rootfs");
        let report = unpack(&store, &r, &root).expect("unpack");
        assert_eq!(report.layers, 2);
        assert!(root.join("bin/app").is_file());
        assert!(
            !root.join("etc/old.conf").exists(),
            "whiteout removed lower file"
        );
        assert!(
            !root.join("data/a").exists() && !root.join("data/b").exists(),
            "opaque dir hid lower entries"
        );
        assert_eq!(std::fs::read(root.join("data/c")).expect("c"), b"C");
        assert_eq!(
            std::fs::read_link(root.join("current")).expect("link"),
            Path::new("mvf")
        );
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(root.join("bin/app2")).expect("hl").ino(),
            std::fs::metadata(root.join("bin/app")).expect("f").ino()
        );
        assert_eq!(
            std::fs::metadata(root.join("bin/app")).expect("m").mode() & 0o777,
            0o755
        );
        assert_eq!(report.whiteouts, 2);
    }

    fn hostile(
        entries: LayerBuilder,
        name: &str,
    ) -> (PathBuf, std::result::Result<UnpackReport, OciError>) {
        let dir = tmp(name);
        let store = ContentStore::open(dir.join("s")).expect("store");
        let layer = entries.uncompressed().expect("layer");
        let r = create_image(
            &store,
            name,
            &linux("arm64"),
            &ExecConfig::default(),
            &[layer],
        )
        .expect("image");
        let root = dir.join("rootfs");
        let result = unpack(&store, &r, &root);
        (dir, result)
    }

    #[test]
    fn traversal_and_absolute_entries_are_rejected_and_cleaned_up() {
        for (name, layer) in [
            (
                "dotdot",
                LayerBuilder::new().file("../../escape", b"x", 0o644),
            ),
            (
                "nested-dotdot",
                LayerBuilder::new()
                    .dir("a")
                    .file("a/../../escape", b"x", 0o644),
            ),
            (
                "absolute",
                LayerBuilder::new().file("/tmp/mvf-escape", b"x", 0o644),
            ),
            (
                "hardlink-out",
                LayerBuilder::new().entry(FileEntry::HardLink {
                    path: "h".into(),
                    target: "../../etc/passwd".into(),
                }),
            ),
        ] {
            let (dir, result) = hostile(layer, name);
            assert!(
                matches!(result, Err(OciError::UnsafeEntry { .. })),
                "{name}: {result:?}"
            );
            assert!(
                !dir.join("rootfs").exists(),
                "{name}: partial rootfs removed"
            );
            assert!(!dir.join("escape").exists());
        }
        assert!(!Path::new("/tmp/mvf-escape").exists());
    }

    #[test]
    fn symlinks_never_lead_writes_outside_the_rootfs() {
        let outside = tmp("outside-target");
        let sentinel = outside.join("victim");
        std::fs::write(&sentinel, b"untouched").expect("sentinel");
        let layer = LayerBuilder::new()
            // A directory symlink pointing outside (absolute and relative), then writes through it.
            .symlink("abs", outside.to_str().expect("utf8"))
            .file("abs/pwned", b"x", 0o644)
            .symlink("rel", "../../../../../../../../../../tmp")
            .file("rel/mvf-pwned", b"x", 0o644)
            // A file symlink to the sentinel, then a regular file at the same path.
            .symlink("victim-link", sentinel.to_str().expect("utf8"))
            .file("victim-link", b"replaced inside", 0o644)
            // A hard link through a symlinked parent to something outside.
            .entry(FileEntry::HardLink {
                path: "hl".into(),
                target: "abs/victim".into(),
            });
        let (dir, result) = hostile(layer, "symlinks");
        // The hard link target resolves inside the rootfs where no such file exists → refused.
        assert!(
            matches!(result, Err(OciError::UnsafeEntry { .. })),
            "{result:?}"
        );
        assert_eq!(std::fs::read(&sentinel).expect("sentinel"), b"untouched");
        assert!(!outside.join("pwned").exists());
        assert!(!Path::new("/tmp/mvf-pwned").exists());
        let _ = dir;

        // Without the hard link the same layer unpacks, with every write kept inside.
        let layer = LayerBuilder::new()
            .symlink("abs", outside.to_str().expect("utf8"))
            .file("abs/pwned", b"x", 0o644)
            .symlink("victim-link", sentinel.to_str().expect("utf8"))
            .file("victim-link", b"replaced inside", 0o644);
        let (dir, result) = hostile(layer, "symlinks-ok");
        result.expect("unpacks safely");
        let root = dir.join("rootfs");
        assert_eq!(std::fs::read(&sentinel).expect("sentinel"), b"untouched");
        assert!(!outside.join("pwned").exists());
        let inside = root
            .join(outside.strip_prefix("/").expect("abs"))
            .join("pwned");
        assert!(
            inside.is_file(),
            "absolute link target resolved inside the rootfs: {}",
            inside.display()
        );
        assert_eq!(
            std::fs::read(root.join("victim-link")).expect("f"),
            b"replaced inside"
        );
        assert!(
            !std::fs::symlink_metadata(root.join("victim-link"))
                .expect("m")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn diff_id_mismatch_and_size_limits_abort_and_clean_up() {
        let dir = tmp("diffid");
        let store = ContentStore::open(dir.join("s")).expect("store");
        let mut layer = LayerBuilder::new()
            .file("f", b"content", 0o644)
            .uncompressed()
            .expect("l");
        layer.diff_id = Digest::of(b"something else");
        let r = create_image(
            &store,
            "d:1",
            &linux("arm64"),
            &ExecConfig::default(),
            &[layer],
        )
        .expect("img");
        let root = dir.join("rootfs");
        assert!(matches!(
            unpack(&store, &r, &root),
            Err(OciError::DigestMismatch { .. })
        ));
        assert!(!root.exists());

        let mut limited = ContentStore::open(dir.join("s2")).expect("store");
        limited.limits.max_unpacked_bytes = 4;
        let layer = LayerBuilder::new()
            .file("big", b"0123456789", 0o644)
            .uncompressed()
            .expect("l");
        let r = create_image(
            &limited,
            "big:1",
            &linux("arm64"),
            &ExecConfig::default(),
            &[layer],
        )
        .expect("img");
        assert!(matches!(
            unpack(&limited, &r, &dir.join("r2")),
            Err(OciError::Limit(_))
        ));
    }

    #[test]
    fn appended_layer_builds_a_derived_image() {
        let dir = tmp("append");
        let store = ContentStore::open(dir.join("s")).expect("store");
        let base = sample(&store, "base:1", "arm64");
        let layer = LayerBuilder::new()
            .dir("app")
            .file("app/main.py", b"print('hi')\n", 0o644)
            .gzip()
            .expect("l");
        let mut overrides = ExecConfig {
            entrypoint: Some(vec!["python3".into(), "/app/main.py".into()]),
            ..ExecConfig::default()
        };
        overrides.labels = Some(BTreeMap::from([("dev.mvf.extra".into(), "yes".into())]));
        let derived =
            append_layer(&store, &base, "derived:1", &layer, Some(&overrides)).expect("append");
        assert_eq!(derived.layers.len(), 3);
        assert_eq!(&derived.layers[..2], &base.layers[..]);
        let config = read_config(&store, &derived).expect("config");
        let exec = config.config.expect("exec");
        assert_eq!(
            exec.entrypoint,
            Some(vec!["python3".into(), "/app/main.py".into()])
        );
        assert_eq!(
            exec.working_dir.as_deref(),
            Some("/work"),
            "base settings kept"
        );
        let labels = exec.labels.expect("labels");
        assert_eq!(
            labels.get(NODE_CONTRACT_LABEL).map(String::as_str),
            Some("/mvf/node.yaml")
        );
        assert_eq!(labels.get("dev.mvf.extra").map(String::as_str), Some("yes"));
        let root = dir.join("rootfs");
        unpack(&store, &derived, &root).expect("unpack");
        assert!(root.join("app/main.py").is_file() && root.join("bin/app").is_file());
    }
}

#[test]
fn references_are_normalized_like_docker() {
    use mvf_oci::export::normalize_reference;
    assert_eq!(
        normalize_reference("busybox:1.37"),
        "docker.io/library/busybox:1.37"
    );
    assert_eq!(
        normalize_reference("mvf/py-invert:1"),
        "docker.io/mvf/py-invert:1"
    );
    assert_eq!(
        normalize_reference("docker.io/library/python:3"),
        "docker.io/library/python:3"
    );
    assert_eq!(
        normalize_reference("registry.local:5000/a/b:c"),
        "registry.local:5000/a/b:c"
    );
    assert_eq!(normalize_reference("localhost/x:1"), "localhost/x:1");
}
