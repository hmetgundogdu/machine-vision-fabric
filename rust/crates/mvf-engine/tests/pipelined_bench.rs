//! Cost of output sampling in the pipelined executor (#33 previews):
//! `MVF_PIPE_BENCH=1 cargo test --release -p mvf-engine --test pipelined_bench -- --nocapture`.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use mvf_engine::*;
use mvf_graph::build_graph;

/// Wants the outputs of `node` at most every `every` asks (`u64::MAX` = never).
struct Every {
    node: &'static str,
    every: u64,
    asked: u64,
    got: u64,
}

impl Observer for Every {
    fn wants_outputs(&mut self, node: &str) -> bool {
        if node != self.node {
            return false;
        }
        self.asked += 1;
        self.every != u64::MAX && self.asked.is_multiple_of(self.every)
    }
    fn outputs(&mut self, _: u64, _: &str, _: &BTreeMap<String, Value>) {
        self.got += 1;
    }
}

#[test]
fn bench_pipelined_sampling() {
    if std::env::var_os("MVF_PIPE_BENCH").is_none_or(|v| v != "1") {
        return;
    }
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: bench }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { width: 64, height: 48, channels: 3, frames: 50000 } }\n    - { id: inv, builtin: invert }\n    - { id: pass, builtin: passthrough }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: inv.frame }\n      - { from: inv.frame, to: pass.frame }\n      - { from: pass.frame, to: sink.frame }\n";
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let options = RunOptions {
        pipelined: true,
        ..RunOptions::default()
    };
    for (label, every) in [
        ("null observer", 0),
        ("never wanted", u64::MAX),
        ("every 100th", 100),
        ("every cycle", 1),
    ]
    .into_iter()
    .cycle()
    .take(8)
    {
        let graph = build_graph(text, &HostResolver::new(&fixtures)).expect("graph");
        let mut engine =
            Engine::build(graph, &default_backends(), &HostServices::default()).expect("build");
        let mut observer = Every {
            node: "pass",
            every,
            asked: 0,
            got: 0,
        };
        let t = Instant::now();
        let report = if every == 0 {
            engine.run(&options, &mut NullObserver)
        } else {
            engine.run(&options, &mut observer)
        };
        let us = t.elapsed().as_secs_f64() * 1e6 / report.cycles_completed as f64;
        eprintln!(
            "pipelined, {label:<13}: {us:.3} us/cycle over {} cycles, {} samples",
            report.cycles_completed, observer.got
        );
    }
}
