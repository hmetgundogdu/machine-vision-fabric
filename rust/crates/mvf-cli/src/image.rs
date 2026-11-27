//! `mvf image …` — the offline OCI content store (no Docker daemon, no network).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Subcommand;
use mvf_oci::{ArchiveFormat, ContentStore, Platform};

#[derive(Debug, Subcommand)]
pub enum ImageCommand {
    /// Import an OCI layout (directory or tar) or a Docker archive (`docker save`).
    Import {
        /// Archive or layout directory.
        path: PathBuf,
        /// Name to store it under (default: the name recorded in the archive).
        #[arg(long)]
        name: Option<String>,
        /// Platform to select, `os/arch[/variant]` (default: linux/<host arch>).
        #[arg(long)]
        platform: Option<String>,
    },
    /// List stored images.
    Ls,
    /// Show an image record and its config as JSON.
    Inspect {
        /// Image name.
        name: String,
    },
    /// Re-hash every blob of an image.
    Verify {
        /// Image name.
        name: String,
    },
    /// Export an image as an OCI layout tar or a Docker archive.
    Export {
        /// Image name.
        name: String,
        /// Output file.
        #[arg(short, long)]
        output: PathBuf,
        /// `oci` or `docker`.
        #[arg(long, default_value = "oci")]
        format: String,
    },
    /// Unpack an image's root filesystem into an empty directory.
    Unpack {
        /// Image name.
        name: String,
        /// Destination directory.
        dest: PathBuf,
    },
    /// Remove an image record (blobs are freed by `gc`).
    Rm {
        /// Image name.
        name: String,
    },
    /// Delete blobs no image references.
    Gc,
    /// Package a Python module as an OCI-profile node image on top of a stored base image, and
    /// write its node contract pinned to the new image digest.
    Package {
        /// Stored base image (must contain python3).
        #[arg(long)]
        base: String,
        /// Name of the new image.
        #[arg(long)]
        name: String,
        /// Module directory (with `node.yaml` and the entry script).
        #[arg(long)]
        module: PathBuf,
        /// Entry script inside the module directory.
        #[arg(long)]
        entry: String,
        /// The `mvf_sdk` package directory.
        #[arg(long)]
        sdk: PathBuf,
        /// Where to write the OCI node contract.
        #[arg(long)]
        contract_out: PathBuf,
    },
}

pub fn default_store() -> PathBuf {
    if let Some(p) = std::env::var_os("MVF_STORE") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".mvf").join("store")
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::from(2)
}

fn print(value: serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(&value).unwrap_or_default()
    );
}

pub fn run(store_root: &Path, command: ImageCommand) -> ExitCode {
    let store = match ContentStore::open(store_root) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let result: Result<(), mvf_oci::OciError> = (|| {
        match command {
            ImageCommand::Import {
                path,
                name,
                platform,
            } => {
                let platform = match platform {
                    Some(p) => Platform::parse(&p)?,
                    None => Platform::host_linux(),
                };
                let r = mvf_oci::import(&store, &path, &platform, name.as_deref())?;
                print(serde_json::to_value(&r).unwrap_or_default());
            }
            ImageCommand::Ls => {
                for r in store.records()? {
                    println!(
                        "{:<40} {} {:<12} {} layers  {}",
                        r.name,
                        &r.manifest.digest.as_str()[..19],
                        r.platform.to_string(),
                        r.layers.len(),
                        r.source
                    );
                }
            }
            ImageCommand::Inspect { name } => {
                let r = store.record(&name)?;
                let config: serde_json::Value =
                    serde_json::from_slice(&store.read_json_blob(&r.config.digest)?)
                        .unwrap_or_default();
                print(serde_json::json!({ "record": r, "config": config }));
            }
            ImageCommand::Verify { name } => {
                let r = store.record(&name)?;
                for d in std::iter::once(&r.manifest)
                    .chain(std::iter::once(&r.config))
                    .chain(r.layers.iter())
                {
                    store.verify_blob(&d.digest)?;
                }
                println!("{name}: {} blobs verified", r.layers.len() + 2);
            }
            ImageCommand::Export {
                name,
                output,
                format,
            } => {
                let r = store.record(&name)?;
                let format = match format.as_str() {
                    "oci" => ArchiveFormat::OciLayout,
                    "docker" => ArchiveFormat::Docker,
                    other => {
                        return Err(mvf_oci::OciError::Unsupported(format!(
                            "export format `{other}`"
                        )));
                    }
                };
                mvf_oci::export(&store, &r, &output, format)?;
                println!("{} -> {} ({})", name, output.display(), r.manifest.digest);
            }
            ImageCommand::Unpack { name, dest } => {
                let r = store.record(&name)?;
                let report = mvf_oci::unpack(&store, &r, &dest)?;
                println!("{report:?}");
            }
            ImageCommand::Rm { name } => store.remove(&name)?,
            ImageCommand::Gc => println!("{:?}", store.gc()?),
            ImageCommand::Package {
                base,
                name,
                module,
                entry,
                sdk,
                contract_out,
            } => {
                let base = store.record(&base)?;
                let text = std::fs::read_to_string(module.join("node.yaml"))
                    .map_err(|e| mvf_oci::OciError::Malformed(format!("node.yaml: {e}")))?;
                let module_contract = mvf_contract::parse_contract(&text)
                    .map_err(|e| mvf_oci::OciError::Malformed(e.to_string()))?;
                let record = mvf_container::package::python_node_image(
                    &store,
                    &base,
                    &name,
                    &sdk,
                    &module,
                    &entry,
                    &mvf_container::package::contract_text(&module_contract),
                )?;
                let contract = mvf_container::package::oci_contract(
                    &module_contract,
                    &name,
                    record.manifest.digest.as_str(),
                );
                std::fs::write(
                    &contract_out,
                    mvf_container::package::contract_text(&contract),
                )
                .map_err(|e| mvf_oci::OciError::Malformed(e.to_string()))?;
                println!(
                    "{name} {} -> {}",
                    record.manifest.digest,
                    contract_out.display()
                );
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(e),
    }
}
