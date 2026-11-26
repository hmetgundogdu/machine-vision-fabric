//! `mvf-rs` — native CLI for the MVF Rust runtime.
//!
//! Exit codes: 0 = run ended normally (cycle limit, source exhausted, cancelled) or validation passed;
//! 1 = a node failed or could not start; 2 = invalid input (YAML, contract, graph, negotiation).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use mvf_engine::{
    CancellationToken, EndReason, Engine, HostResolver, HostServices, NullObserver, Record,
    Recorder, RunOptions, RunReport, default_backends,
};

#[derive(Debug, Parser)]
#[command(
    name = "mvf-rs",
    version,
    about = "MVF Rust runtime (research build, issue #10)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate a node contract and show how this host would place it.
    Contract {
        /// Contract YAML file.
        file: PathBuf,
    },
    /// Validate a pipeline: YAML, contracts, typed edges and backend negotiation.
    Validate {
        /// Pipeline YAML file.
        pipeline: PathBuf,
    },
    /// Run a pipeline until a limit, source exhaustion, failure or Ctrl+C.
    Run {
        /// Pipeline YAML file.
        pipeline: PathBuf,
        /// Override `spec.run.maxCycles`.
        #[arg(long)]
        max_cycles: Option<u64>,
        /// Emit JSON lines (`{"record":…}` per sink record, then `{"report":…}`).
        #[arg(long)]
        json: bool,
    },
    /// List the builtins and backends this host provides.
    Host,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Contract { file } => contract(&file),
        Command::Validate { pipeline } => validate(&pipeline),
        Command::Run {
            pipeline,
            max_cycles,
            json,
        } => run(&pipeline, max_cycles, json),
        Command::Host => host(),
    }
}

fn fail(code: u8, message: impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::from(code)
}

fn contract(file: &Path) -> ExitCode {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => return fail(2, format!("{}: {e}", file.display())),
    };
    let contract = match mvf_contract::parse_contract(&text) {
        Ok(c) => c,
        Err(e) => return fail(2, e),
    };
    let placement = default_backends().negotiate(&contract);
    let out = serde_json::json!({
        "id": contract.metadata.id,
        "version": contract.metadata.version,
        "placement": match &placement {
            Ok(p) => serde_json::to_value(p).unwrap_or_default(),
            Err(e) => serde_json::to_value(e).unwrap_or_default(),
        },
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    if placement.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}

fn load_engine(pipeline: &Path, services: &HostServices) -> Result<Engine, ExitCode> {
    let text = std::fs::read_to_string(pipeline)
        .map_err(|e| fail(2, format!("{}: {e}", pipeline.display())))?;
    let base = pipeline.parent().unwrap_or(Path::new("."));
    let graph = mvf_graph::build_graph(&text, &HostResolver::new(base)).map_err(|e| fail(2, e))?;
    Engine::build(graph, &default_backends(), services).map_err(|e| fail(2, e))
}

fn validate(pipeline: &Path) -> ExitCode {
    let engine = match load_engine(pipeline, &HostServices::default()) {
        Ok(e) => e,
        Err(code) => return code,
    };
    let graph = engine.graph();
    println!(
        "pipeline `{}`: {} nodes, {} data edges, {} control edges — valid",
        graph.name,
        graph.nodes.len(),
        graph.data_edges.len(),
        graph.control_edges.len()
    );
    for (node, stats) in graph.nodes.iter().zip(engine.stats()) {
        println!(
            "  {:<16} {:<28} backend={}",
            node.id,
            format!(
                "{}@{}",
                node.contract.metadata.id, node.contract.metadata.version
            ),
            stats.backend
        );
    }
    ExitCode::SUCCESS
}

fn print_record(record: &Record, json: bool) {
    if json {
        println!("{}", serde_json::json!({ "record": record }));
    } else {
        println!(
            "cycle={} node={} gen={} port={} {}",
            record.cycle,
            record.node,
            record.generation,
            record.port,
            serde_json::to_string(&record.value).unwrap_or_default()
        );
    }
}

fn print_report(report: &RunReport, json: bool) {
    if json {
        println!("{}", serde_json::json!({ "report": report }));
    } else {
        eprintln!(
            "run `{}` ended: {} after {} cycles in {:.1} ms",
            report.pipeline,
            serde_json::to_string(&report.end).unwrap_or_default(),
            report.cycles_completed,
            report.wall_time_ms
        );
        for n in &report.nodes {
            eprintln!(
                "  {:<16} backend={:<10} gen={} executed={} skipped={} total={}us max={}us",
                n.id, n.backend, n.generation, n.executed, n.skipped, n.total_us, n.max_us
            );
        }
    }
}

fn run(pipeline: &Path, max_cycles: Option<u64>, json: bool) -> ExitCode {
    let services = HostServices {
        recorder: Recorder::streaming(move |r| print_record(r, json)),
    };
    let mut engine = match load_engine(pipeline, &services) {
        Ok(e) => e,
        Err(code) => return code,
    };
    let cancel = CancellationToken::new();
    let handler_token = cancel.clone();
    // Ctrl+C requests a clean stop at the next cycle boundary.
    let _ = ctrlc::set_handler(move || handler_token.cancel());
    let report = engine.run(&RunOptions { max_cycles, cancel }, &mut NullObserver);
    print_report(&report, json);
    match report.end {
        EndReason::NodeFailed { .. } | EndReason::StartFailed { .. } => ExitCode::from(1),
        _ => ExitCode::SUCCESS,
    }
}

fn host() -> ExitCode {
    let out = serde_json::json!({
        "builtins": mvf_engine::builtins::builtin_names(),
        "backends": default_backends().capabilities(),
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    ExitCode::SUCCESS
}
