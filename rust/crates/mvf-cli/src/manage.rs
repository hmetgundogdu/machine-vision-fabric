//! `mvf-rs manage …` (#33): observe a running runtime over its local management endpoint
//! (`mvf-rs run --manage`), and run commands as its owner. Every command goes through the engine's
//! own validation, is idempotent by `--id`, and is written to the run's audit log.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::Subcommand;
use mvf_management::{Client, list, runtime_dir};
use serde_json::Value;

// Parsed once from the command line; boxing the larger variant would buy nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
pub enum ManageCommand {
    /// Runtimes registered for this user.
    List {
        /// JSON output.
        #[arg(long)]
        json: bool,
    },
    /// The runtime's current state: topology, node counters and latency, status.
    Status {
        /// Endpoint (socket path or pipe name); default: the only registered runtime.
        #[arg(long)]
        endpoint: Option<String>,
        /// The raw snapshot as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Follow the runtime live: node deltas, events, logs, status. Reconnects and continues from
    /// the last sequence it saw; after a gap it resynchronises from a snapshot.
    Watch {
        /// Endpoint; default: the only registered runtime.
        #[arg(long)]
        endpoint: Option<String>,
        /// Start at this sequence (default: a fresh snapshot, then everything after it).
        #[arg(long)]
        from: Option<u64>,
        /// Print the protocol messages as JSON lines.
        #[arg(long)]
        json: bool,
        /// Stop once the run has ended.
        #[arg(long)]
        until_stopped: bool,
        /// Give up reconnecting after this many seconds without a connection (default 30).
        #[arg(long, default_value_t = 30)]
        reconnect_secs: u64,
    },
    /// Save previews of an opted-in node (`run --manage --preview NODE`) as PNG files.
    Preview {
        /// Endpoint; default: the only registered runtime.
        #[arg(long)]
        endpoint: Option<String>,
        /// The node.
        #[arg(long)]
        node: String,
        /// How many previews to save (default 1).
        #[arg(long, default_value_t = 1)]
        count: usize,
        /// Previews per second to ask for (capped by the runtime).
        #[arg(long)]
        fps: Option<f64>,
        /// Directory for `<node>-<seq>.png` (default: the current directory).
        #[arg(long, default_value = ".")]
        out: std::path::PathBuf,
    },
    /// Upload a prepared node package (a directory with `node.yaml`) and stage it; activate it
    /// with `command replace --node … --contract <the printed contract>`. Resumes an
    /// interrupted upload.
    Upload {
        /// Endpoint; default: the only registered runtime.
        #[arg(long)]
        endpoint: Option<String>,
        /// Package name (immutable once staged: use a new name per version).
        #[arg(long)]
        package: String,
        /// A signature from `mvf-remote sign package` (required by a runtime that trusts
        /// signers).
        #[arg(long)]
        signature: Option<std::path::PathBuf>,
        /// The package directory.
        dir: std::path::PathBuf,
    },
    /// Run a command: stop, pause, resume, set-value, configure, replace, rollback, commit, stage.
    Command {
        /// The command.
        name: String,
        /// Endpoint; default: the only registered runtime.
        #[arg(long)]
        endpoint: Option<String>,
        /// Idempotency key: repeating a command with the same id never runs it twice (default:
        /// generated).
        #[arg(long)]
        id: Option<String>,
        /// Target node.
        #[arg(long)]
        node: Option<String>,
        /// `set-value` / `configure`: the new value, as JSON.
        #[arg(long)]
        value: Option<String>,
        /// `configure`: the bound config field.
        #[arg(long)]
        field: Option<String>,
        /// `replace`: candidate module id.
        #[arg(long)]
        module: Option<String>,
        /// `replace`: candidate builtin.
        #[arg(long)]
        builtin: Option<String>,
        /// `replace`: candidate contract, inside the pipeline's package.
        #[arg(long)]
        contract: Option<String>,
        /// `replace`: candidate config, as a JSON object.
        #[arg(long)]
        config: Option<String>,
        /// `pause`: resume by itself after this many milliseconds.
        #[arg(long)]
        for_ms: Option<u64>,
        /// Wait at most this long for the outcome (default 5000, at most 60000).
        #[arg(long)]
        deadline_ms: Option<u64>,
    },
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("mvf-rs manage: {message}");
    ExitCode::from(1)
}

fn endpoint(given: Option<String>) -> Result<String, ExitCode> {
    if let Some(e) = given {
        return Ok(e);
    }
    let dir = runtime_dir().map_err(fail)?;
    let found = list(&dir);
    match found.as_slice() {
        [one] => Ok(one.endpoint.clone()),
        [] => Err(fail(format!(
            "no runtime registered in {} (start one with `mvf-rs run --manage`)",
            dir.display()
        ))),
        many => Err(fail(format!(
            "{} runtimes registered; pass --endpoint ({})",
            many.len(),
            many.iter()
                .map(|i| format!("{} pid {}", i.pipeline, i.pid))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

pub fn run(command: ManageCommand) -> ExitCode {
    match command {
        ManageCommand::List { json } => {
            let dir = match runtime_dir() {
                Ok(d) => d,
                Err(e) => return fail(e),
            };
            let found = list(&dir);
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&found).unwrap_or_else(|_| "[]".into())
                );
            } else {
                for i in &found {
                    println!("{:<8} {:<24} {}", i.pid, i.pipeline, i.endpoint);
                }
            }
            ExitCode::SUCCESS
        }
        ManageCommand::Status { endpoint: e, json } => {
            let e = match endpoint(e) {
                Ok(e) => e,
                Err(code) => return code,
            };
            let snapshot = Client::connect(&e, "mvf-rs").and_then(|mut c| c.snapshot());
            match snapshot {
                Ok((_, s)) if json => {
                    println!("{s}");
                    ExitCode::SUCCESS
                }
                Ok((_, s)) => {
                    print_status(&s);
                    ExitCode::SUCCESS
                }
                Err(err) => fail(format!("{e}: {err}")),
            }
        }
        ManageCommand::Watch {
            endpoint: e,
            from,
            json,
            until_stopped,
            reconnect_secs,
        } => {
            let e = match endpoint(e) {
                Ok(e) => e,
                Err(code) => return code,
            };
            watch(
                &e,
                from,
                json,
                until_stopped,
                Duration::from_secs(reconnect_secs),
            )
        }
        ManageCommand::Preview {
            endpoint: e,
            node,
            count,
            fps,
            out,
        } => {
            let e = match endpoint(e) {
                Ok(e) => e,
                Err(code) => return code,
            };
            let mut c = match Client::connect(&e, "mvf-rs") {
                Ok(c) => c,
                Err(err) => return fail(format!("{e}: {err}")),
            };
            let mut req = serde_json::json!({"op": "preview", "node": node});
            if let Some(f) = fps {
                req["fps"] = serde_json::json!(f);
            }
            let mut m = match c.request(&req) {
                Ok(m) => m,
                Err(err) => return fail(err),
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
                        if let Err(err) = std::fs::write(&path, png) {
                            return fail(format!("{}: {err}", path.display()));
                        }
                        println!(
                            "{} ({}x{} from {}x{}, cycle {})",
                            path.display(),
                            m["width"],
                            m["height"],
                            m["sourceWidth"],
                            m["sourceHeight"],
                            m["cycle"]
                        );
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
                    Err(err) => return fail(err),
                };
            }
        }
        ManageCommand::Upload {
            endpoint: e,
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
                Err(err) => return fail(format!("--signature: {err}")),
            };
            let e = match endpoint(e) {
                Ok(e) => e,
                Err(code) => return code,
            };
            let mut c = match Client::connect(&e, "mvf-rs") {
                Ok(c) => c,
                Err(err) => return fail(format!("{e}: {err}")),
            };
            let mut request = |v: &Value| c.request(v);
            match mvf_management::upload_dir_signed(
                &mut request,
                &package,
                &dir,
                512 * 1024,
                signature.as_ref(),
            ) {
                Ok(r) if r["status"] == "applied" => {
                    println!("{r}");
                    ExitCode::SUCCESS
                }
                Ok(r) => {
                    println!("{r}");
                    ExitCode::from(1)
                }
                Err(err) => fail(err),
            }
        }
        ManageCommand::Command {
            name,
            endpoint: e,
            id,
            node,
            value,
            field,
            module,
            builtin,
            contract,
            config,
            for_ms,
            deadline_ms,
        } => {
            let e = match endpoint(e) {
                Ok(e) => e,
                Err(code) => return code,
            };
            let parse = |what: &str, text: Option<String>| -> Result<Option<Value>, ExitCode> {
                text.map(|t| {
                    serde_json::from_str(&t)
                        .map_err(|err| fail(format!("--{what} is not JSON: {err}")))
                })
                .transpose()
            };
            let (value, config) = match (parse("value", value), parse("config", config)) {
                (Ok(v), Ok(c)) => (v, c),
                (Err(code), _) | (_, Err(code)) => return code,
            };
            let args = serde_json::json!({
                "node": node, "value": value, "field": field, "module": module,
                "builtin": builtin, "contract": contract, "config": config, "forMs": for_ms,
            });
            let id = id.unwrap_or_else(|| {
                format!(
                    "cli-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                )
            });
            let reply = Client::connect(&e, "mvf-rs")
                .and_then(|mut c| c.command(&id, &name, args, deadline_ms));
            match reply {
                Ok(r) => {
                    println!("{r}");
                    match r["status"].as_str() {
                        Some("applied") => ExitCode::SUCCESS,
                        Some("pending") => ExitCode::from(3),
                        _ => ExitCode::from(1),
                    }
                }
                Err(err) => fail(format!("{e}: {err}")),
            }
        }
    }
}

fn print_status(s: &Value) {
    println!(
        "{} — {} at cycle {} (pid {}, API {})",
        s["runtime"]["pipeline"].as_str().unwrap_or("?"),
        s["status"].as_str().unwrap_or("?"),
        s["cycle"],
        s["runtime"]["pid"],
        s["api"]
    );
    if !s["end"].is_null() {
        println!("  ended: {}", s["end"]);
    }
    println!(
        "  {:<16} {:<12} {:>4} {:>10} {:>8} {:>10} {:>10} {:>10}",
        "node", "backend", "gen", "executed", "skipped", "p50 us", "p99 us", "max us"
    );
    for n in s["topology"]["nodes"].as_array().into_iter().flatten() {
        let id = n["id"].as_str().unwrap_or("?");
        let v = &s["nodes"][id];
        println!(
            "  {:<16} {:<12} {:>4} {:>10} {:>8} {:>10.1} {:>10.1} {:>10}",
            id,
            n["backend"].as_str().unwrap_or("-"),
            v["generation"].as_u64().unwrap_or(0),
            v["executed"].as_u64().unwrap_or(0),
            v["skipped"].as_u64().unwrap_or(0),
            v["latencyUs"]["p50"].as_f64().unwrap_or(0.0),
            v["latencyUs"]["p99"].as_f64().unwrap_or(0.0),
            v["latencyUs"]["max"].as_u64().unwrap_or(0),
        );
    }
}

fn print_message(m: &Value) {
    match m["op"].as_str() {
        Some("entry") => {
            let e = &m["entry"];
            let seq = &m["seq"];
            match e["kind"].as_str() {
                Some("delta") => {
                    let rates: Vec<String> = e["nodes"]
                        .as_object()
                        .into_iter()
                        .flatten()
                        .map(|(id, v)| {
                            format!(
                                "{id} {:.0}/s p99 {:.0}us",
                                v["window"]["perSecond"].as_f64().unwrap_or(0.0),
                                v["latencyUs"]["p99"].as_f64().unwrap_or(0.0)
                            )
                        })
                        .collect();
                    println!("[{seq}] cycle {}: {}", e["cycle"], rates.join(", "));
                }
                Some("log") => println!(
                    "[{seq}] {} {}: {}",
                    e["node"].as_str().unwrap_or("?"),
                    e["level"].as_str().unwrap_or("?"),
                    e["message"].as_str().unwrap_or("")
                ),
                Some("event") => println!(
                    "[{seq}] {} {}",
                    e["event"].as_str().unwrap_or("?"),
                    e["detail"]
                ),
                Some("status") => println!(
                    "[{seq}] status {} {}",
                    e["status"].as_str().unwrap_or("?"),
                    e.get("end").unwrap_or(&Value::Null)
                ),
                Some("dropped") => println!(
                    "[{seq}] {} log line(s) dropped by the telemetry budget",
                    e["logs"]
                ),
                _ => println!("[{seq}] {e}"),
            }
        }
        Some("gap") => println!(
            "-- missed entries {}..{}; resynchronised from a snapshot",
            m["from"], m["to"]
        ),
        Some("snapshot") => println!(
            "-- snapshot at seq {}: {}",
            m["seq"], m["snapshot"]["status"]
        ),
        _ => {}
    }
}

fn watch(
    endpoint: &str,
    mut from: Option<u64>,
    json: bool,
    until_stopped: bool,
    reconnect: Duration,
) -> ExitCode {
    let mut last_ok = Instant::now();
    loop {
        let sub = Client::connect(endpoint, "mvf-rs").and_then(|c| c.subscribe(from));
        let mut sub = match sub {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return fail(e),
            Err(e) => {
                if last_ok.elapsed() > reconnect {
                    return fail(format!("{endpoint}: {e}"));
                }
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        for m in sub.by_ref() {
            let Ok(m) = m else { break };
            last_ok = Instant::now();
            if json {
                println!("{m}");
            } else if m["op"] != "heartbeat" {
                print_message(&m);
            }
            if until_stopped && m["op"] == "entry" && m["entry"]["status"] == "stopped" {
                return ExitCode::SUCCESS;
            }
        }
        // Disconnected: continue after the last sequence seen.
        from = sub.last_seq().map(|s| s + 1).or(from);
        if !json {
            eprintln!(
                "-- disconnected; reconnecting from seq {}",
                from.map_or("start".into(), |f| f.to_string())
            );
        }
    }
}
