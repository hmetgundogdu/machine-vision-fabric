//! `mvf volume`: list, inspect and explicitly delete retained volumes (#31). Volume data is
//! never removed by a node stopping, a replacement or the engine exiting; only `delete --yes` does.

use std::path::Path;
use std::process::ExitCode;

use clap::Subcommand;

/// Volume commands.
#[derive(Debug, Subcommand)]
pub enum VolumeCommand {
    /// Retained persistent and shared volumes, their size and file count.
    List {
        /// JSON output.
        #[arg(long)]
        json: bool,
    },
    /// One volume: path, type, writers, usage.
    Inspect {
        /// `<pipeline>/<volume>`.
        volume: String,
    },
    /// Delete a volume and all its data. Irreversible.
    Delete {
        /// `<pipeline>/<volume>`.
        volume: String,
        /// Confirm the deletion.
        #[arg(long)]
        yes: bool,
    },
    /// Pinned artifacts in the store (digest, size).
    Artifacts,
}

fn split(v: &str) -> Option<(&str, &str)> {
    v.split_once('/')
        .filter(|(s, n)| !s.is_empty() && !n.is_empty())
}

/// Runs a volume command against the storage root.
pub fn run(root: &Path, command: VolumeCommand) -> ExitCode {
    match command {
        VolumeCommand::List { json } => {
            let volumes = mvf_storage::list_volumes(root);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&volumes).unwrap_or_default()
                );
            } else if volumes.is_empty() {
                println!("no retained volumes under {}", root.display());
            } else {
                println!(
                    "{:<32} {:<10} {:>10} {:>7}  path",
                    "volume", "type", "bytes", "files"
                );
                for v in volumes {
                    println!(
                        "{:<32} {:<10} {:>10} {:>7}  {}",
                        format!("{}/{}", v.scope, v.name),
                        v.kind,
                        v.bytes,
                        v.files,
                        v.path.display()
                    );
                }
            }
            ExitCode::SUCCESS
        }
        VolumeCommand::Inspect { volume } => {
            let Some((scope, name)) = split(&volume) else {
                eprintln!("error: expected `<pipeline>/<volume>`, got `{volume}`");
                return ExitCode::from(2);
            };
            match mvf_storage::list_volumes(root)
                .into_iter()
                .find(|v| v.scope == scope && v.name == name)
            {
                Some(v) => {
                    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
                    ExitCode::SUCCESS
                }
                None => {
                    eprintln!("error: no volume `{volume}` under {}", root.display());
                    ExitCode::from(1)
                }
            }
        }
        VolumeCommand::Delete { volume, yes } => {
            let Some((scope, name)) = split(&volume) else {
                eprintln!("error: expected `<pipeline>/<volume>`, got `{volume}`");
                return ExitCode::from(2);
            };
            if !yes {
                eprintln!(
                    "error: deleting `{volume}` removes all its data and cannot be undone; pass --yes"
                );
                return ExitCode::from(2);
            }
            match mvf_storage::delete_volume(root, scope, name) {
                Ok(bytes) => {
                    println!("deleted `{volume}` ({bytes} bytes)");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(1)
                }
            }
        }
        VolumeCommand::Artifacts => {
            for (digest, bytes) in mvf_storage::list_artifacts(root) {
                println!("{digest} {bytes}");
            }
            ExitCode::SUCCESS
        }
    }
}
