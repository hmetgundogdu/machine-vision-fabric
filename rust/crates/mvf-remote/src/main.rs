//! `mvf-remote`: remote management of an MVF runtime over mutually authenticated TLS (#33).
//!
//! On the edge box:
//!   mvf-remote pki init --dir pki --host panel-07 --host 10.0.0.7
//!   mvf-remote pki issue --dir pki --name alice --role operator
//!   mvf-remote serve --pki pki --listen 10.0.0.7:7443        # next to `mvf-rs run --manage`
//! On the operator's machine (with ca.pem and alice's certificate and key):
//!   mvf-remote status --connect panel-07:7443 --ca ca.pem --cert alice.pem --key alice.key
//!   mvf-remote watch …   mvf-remote command pause …

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};
use mvf_remote::{AccessList, Relay, RelayConfig, RemoteClient, pki};
use serde_json::{Value, json};

#[derive(Debug, Parser)]
#[command(
    name = "mvf-remote",
    version,
    about = "Remote management of an MVF runtime over mutual TLS (#33)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Args)]
struct Identity {
    /// The relay, `host:port`; `host` must be one of the names in its certificate.
    #[arg(long)]
    connect: String,
    /// The CA certificate (`ca.pem`).
    #[arg(long)]
    ca: PathBuf,
    /// This client's certificate.
    #[arg(long)]
    cert: PathBuf,
    /// This client's key.
    #[arg(long)]
    key: PathBuf,
}

#[derive(Debug, Subcommand)]
enum PkiCommand {
    /// Create the CA, the relay's certificate for the given names, and an empty access list.
    Init {
        /// PKI directory.
        #[arg(long)]
        dir: PathBuf,
        /// A DNS name or IP address clients will use to reach the relay (repeatable).
        #[arg(long = "host", required = true)]
        hosts: Vec<String>,
    },
    /// Issue a client certificate; with --role, also admit it in the access list.
    Issue {
        /// PKI directory.
        #[arg(long)]
        dir: PathBuf,
        /// Client name (the principal the runtime audits).
        #[arg(long)]
        name: String,
        /// `observer` or `operator`.
        #[arg(long)]
        role: Option<String>,
        /// Validity in days (default 90).
        #[arg(long, default_value_t = 90)]
        days: i64,
    },
    /// Revoke a client by name (all its certificates) or by fingerprint.
    Revoke {
        /// PKI directory.
        #[arg(long)]
        dir: PathBuf,
        /// Name or `sha256:` fingerprint.
        who: String,
    },
}

#[derive(Debug, Subcommand)]
enum SignCommand {
    /// Create a signing key: `signer.pk8` (private, 0600) and `signer.pub` (what runtimes trust).
    Keygen {
        /// Directory for the key.
        #[arg(long)]
        dir: PathBuf,
    },
    /// Sign a package directory under a package name.
    Package {
        /// The private key (`signer.pk8`).
        #[arg(long)]
        key: PathBuf,
        /// The package name it will be staged as.
        #[arg(long)]
        package: String,
        /// Where to write the signature.
        #[arg(long)]
        out: PathBuf,
        /// The package directory.
        dir: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Package signing keys and signatures (for runtimes started with `--trust-signer`).
    Sign {
        #[command(subcommand)]
        command: SignCommand,
    },
    /// Certificates and the access list.
    Pki {
        #[command(subcommand)]
        command: PkiCommand,
    },
    /// Serve the local runtime's management API over mutual TLS.
    Serve {
        /// PKI directory (`ca.pem`, `server.pem`, `server.key`, `access.json`).
        #[arg(long)]
        pki: PathBuf,
        /// Address to listen on, e.g. `10.0.0.7:7443`. Required: nothing listens by default.
        #[arg(long)]
        listen: SocketAddr,
        /// Local endpoint (default: the only runtime registered for this user, found per
        /// connection).
        #[arg(long)]
        endpoint: Option<String>,
    },
    /// The runtime's current state.
    Status {
        #[command(flatten)]
        id: Identity,
        /// Raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Follow the runtime live; reconnects and resumes from the last sequence.
    Watch {
        #[command(flatten)]
        id: Identity,
        /// Start at this sequence.
        #[arg(long)]
        from: Option<u64>,
        /// Stop once the run has ended.
        #[arg(long)]
        until_stopped: bool,
        /// Give up after this many seconds without a connection (default 30).
        #[arg(long, default_value_t = 30)]
        reconnect_secs: u64,
    },
    /// Save previews of an opted-in node as PNG files (previewer role).
    Preview {
        #[command(flatten)]
        id: Identity,
        /// The node.
        #[arg(long)]
        node: String,
        /// How many to save (default 1).
        #[arg(long, default_value_t = 1)]
        count: usize,
        /// Directory for `<node>-<seq>.png`.
        #[arg(long, default_value = ".")]
        out: PathBuf,
    },
    /// Upload and stage a node package (operator role); resumes an interrupted upload.
    Upload {
        #[command(flatten)]
        id: Identity,
        /// Package name (immutable once staged).
        #[arg(long)]
        package: String,
        /// A signature from `mvf-remote sign package` (required by runtimes that trust signers).
        #[arg(long)]
        signature: Option<PathBuf>,
        /// The package directory.
        dir: PathBuf,
    },
    /// Run a command (operator role).
    #[command(name = "command")]
    Run {
        #[command(flatten)]
        id: Identity,
        /// stop, pause, resume, set-value, configure, replace, rollback, commit.
        name: String,
        /// Idempotency key (default: generated). Retrying with the same id never runs it twice.
        #[arg(long = "id")]
        command_id: Option<String>,
        /// Arguments as a JSON object, e.g. `{"node":"thr","value":41}`.
        #[arg(long, default_value = "{}")]
        args: String,
        /// Wait at most this long for the outcome.
        #[arg(long)]
        deadline_ms: Option<u64>,
    },
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("mvf-remote: {message}");
    ExitCode::from(1)
}

fn connect(id: &Identity) -> std::io::Result<RemoteClient> {
    let tls = mvf_remote::tls::client(&id.ca, &id.cert, &id.key)?;
    RemoteClient::connect(&id.connect, tls, "mvf-remote")
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Pki { command } => match command {
            PkiCommand::Init { dir, hosts } => match pki::init(&dir, &hosts) {
                Ok(()) => {
                    println!(
                        "created {}: ca.pem, server.pem, access.json (keys are mode 0600)",
                        dir.display()
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            },
            PkiCommand::Issue {
                dir,
                name,
                role,
                days,
            } => {
                let role = match role
                    .as_deref()
                    .map(|r| mvf_management::Role::parse(r).ok_or(r))
                {
                    Some(Err(r)) => {
                        return fail(format!("unknown role {r:?}: observer or operator"));
                    }
                    Some(Ok(r)) => Some(r),
                    None => None,
                };
                let (from, to) = pki::validity(days);
                match pki::issue(&dir, &name, role, from, to) {
                    Ok(i) => {
                        println!(
                            "{}",
                            json!({"cert": i.cert, "key": i.key, "fingerprint": i.fingerprint, "admitted": role.map(|r| r.as_str())})
                        );
                        ExitCode::SUCCESS
                    }
                    Err(e) => fail(e),
                }
            }
            PkiCommand::Revoke { dir, who } => {
                let path = dir.join("access.json");
                let mut list = match AccessList::load(&path) {
                    Ok(l) => l,
                    Err(e) => return fail(format!("{}: {e}", path.display())),
                };
                let n = list.revoke(&who);
                if n == 0 {
                    return fail(format!("no client {who:?} in the access list"));
                }
                if let Err(e) = list.save(&path) {
                    return fail(e);
                }
                println!("revoked {n} certificate(s) of {who}; effective on the next connection");
                ExitCode::SUCCESS
            }
        },
        Command::Serve {
            pki: dir,
            listen,
            endpoint,
        } => {
            let mut config = RelayConfig::from_pki(&dir, listen);
            config.endpoint = endpoint;
            match Relay::start(config) {
                Ok(relay) => {
                    eprintln!(
                        "mvf-remote: serving {} (mutual TLS 1.3, access list {})",
                        relay.local_addr(),
                        dir.join("access.json").display()
                    );
                    loop {
                        std::thread::park();
                    }
                }
                Err(e) => fail(e),
            }
        }
        Command::Status { id, json: raw } => match connect(&id).and_then(|mut c| c.snapshot()) {
            Ok((_, s)) if raw => {
                println!("{s}");
                ExitCode::SUCCESS
            }
            Ok((_, s)) => {
                println!(
                    "{} — {} at cycle {}",
                    s["runtime"]["pipeline"].as_str().unwrap_or("?"),
                    s["status"].as_str().unwrap_or("?"),
                    s["cycle"]
                );
                for n in s["topology"]["nodes"].as_array().into_iter().flatten() {
                    let nid = n["id"].as_str().unwrap_or("?");
                    let v = &s["nodes"][nid];
                    println!(
                        "  {:<16} gen {:<3} executed {:<10} p99 {:.1} us",
                        nid,
                        v["generation"].as_u64().unwrap_or(0),
                        v["executed"].as_u64().unwrap_or(0),
                        v["latencyUs"]["p99"].as_f64().unwrap_or(0.0)
                    );
                }
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        },
        Command::Watch {
            id,
            mut from,
            until_stopped,
            reconnect_secs,
        } => {
            let mut last_ok = Instant::now();
            loop {
                let mut sub = match connect(&id).and_then(|c| c.subscribe(from)) {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return fail(e),
                    Err(e) => {
                        if last_ok.elapsed() > Duration::from_secs(reconnect_secs) {
                            return fail(e);
                        }
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                };
                for m in sub.by_ref() {
                    let Ok(m) = m else { break };
                    last_ok = Instant::now();
                    if m["op"] != "heartbeat" {
                        println!("{m}");
                    }
                    if until_stopped && m["op"] == "entry" && m["entry"]["status"] == "stopped" {
                        return ExitCode::SUCCESS;
                    }
                }
                from = sub.last_seq().map(|s| s + 1).or(from);
            }
        }
        Command::Preview {
            id,
            node,
            count,
            out,
        } => {
            let mut c = match connect(&id) {
                Ok(c) => c,
                Err(e) => return fail(e),
            };
            let mut m = match c.request(&json!({"op": "preview", "node": node})) {
                Ok(m) => m,
                Err(e) => return fail(e),
            };
            let mut saved = 0;
            loop {
                match m["op"].as_str() {
                    Some("preview") => {
                        let Some(png) = m["data"]
                            .as_str()
                            .and_then(mvf_management::preview::unbase64)
                        else {
                            return fail(format!("not an image: {}", m["reason"]));
                        };
                        let path = out.join(format!("{node}-{}.png", m["seq"]));
                        if let Err(e) = std::fs::write(&path, png) {
                            return fail(e);
                        }
                        println!("{}", path.display());
                        saved += 1;
                        if saved >= count {
                            return ExitCode::SUCCESS;
                        }
                    }
                    Some("error") => return fail(format!("{}: {}", m["code"], m["message"])),
                    _ => {}
                }
                m = match c.read_next() {
                    Ok(m) => m,
                    Err(e) => return fail(e),
                };
            }
        }
        Command::Sign { command } => match command {
            SignCommand::Keygen { dir } => match mvf_remote::sign::keygen(&dir) {
                Ok(id) => {
                    println!("{id}");
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            },
            SignCommand::Package {
                key,
                package,
                out,
                dir,
            } => match mvf_remote::sign::sign(&key, &package, &dir)
                .and_then(|s| std::fs::write(&out, s.to_string()).map(|_| s))
            {
                Ok(s) => {
                    println!(
                        "signed {package} ({} files) by {}",
                        s["files"].as_object().map_or(0, |f| f.len()),
                        s["key"].as_str().unwrap_or("?")
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            },
        },
        Command::Upload {
            id,
            package,
            signature,
            dir,
        } => {
            let signature = match signature
                .map(|p| {
                    std::fs::read(&p).and_then(|b| {
                        serde_json::from_slice::<Value>(&b).map_err(std::io::Error::other)
                    })
                })
                .transpose()
            {
                Ok(s) => s,
                Err(e) => return fail(format!("--signature: {e}")),
            };
            let mut c = match connect(&id) {
                Ok(c) => c,
                Err(e) => return fail(e),
            };
            let mut request = |v: &Value| c.request(v);
            match mvf_management::upload_dir_signed(
                &mut request,
                &package,
                &dir,
                512 * 1024,
                signature.as_ref(),
            ) {
                Ok(r) => {
                    println!("{r}");
                    if r["status"] == "applied" {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::from(1)
                    }
                }
                Err(e) => fail(e),
            }
        }
        Command::Run {
            id,
            name,
            command_id,
            args,
            deadline_ms,
        } => {
            let args: Value = match serde_json::from_str(&args) {
                Ok(a) => a,
                Err(e) => return fail(format!("--args is not JSON: {e}")),
            };
            let cid = command_id.unwrap_or_else(|| {
                format!(
                    "remote-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                )
            });
            match connect(&id).and_then(|mut c| c.command(&cid, &name, args, deadline_ms)) {
                Ok(r) => {
                    println!("{r}");
                    match r["status"].as_str() {
                        Some("applied") => ExitCode::SUCCESS,
                        Some("pending") => ExitCode::from(3),
                        _ => ExitCode::from(1),
                    }
                }
                Err(e) => fail(e),
            }
        }
    }
}
