//! Graph primitives, the loop, values/selects, bindings and on-demand activation — parity tests
//! against the .NET reference behaviour (`*PrimitiveNodeRunnerTests`, `LoopAndRunningStateGraphTests`,
//! `ValueAndSelectGraphTests`, `BindingPrePassTests`, `ModuleBindingGraphTests`,
//! `OnDemandActivationTests`). Test names follow the .NET ones where a case maps one to one.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mvf_engine::*;
use mvf_graph::build_graph;
use serde_json::{Value as Json, json};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// A pipeline from node and edge YAML fragments (one item per line).
fn doc(nodes: &[&str], data: &[&str], control: &[&str], loop_edges: &[&str], run: &str) -> String {
    let list = |items: &[&str]| {
        items
            .iter()
            .map(|i| format!("      - {i}\n"))
            .collect::<String>()
    };
    let nodes: String = nodes.iter().map(|n| format!("    - {n}\n")).collect();
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: p }}\nspec:\n  nodes:\n{nodes}  edges:\n    data:\n{}    control:\n{}    loop:\n{}  run: {{ {run} }}\n",
        list(data),
        list(control),
        list(loop_edges)
    )
    .replace("    data:\n    control", "    data: []\n    control")
    .replace("    control:\n    loop", "    control: []\n    loop")
    .replace("    loop:\n  run", "    loop: []\n  run")
}

fn engine(text: &str, services: &HostServices) -> Result<Engine, BuildError> {
    let graph = build_graph(text, &HostResolver::new(fixtures())).expect("graph");
    Engine::build(graph, &default_backends(), services)
}

fn run_observed(text: &str, services: &HostServices, observer: &mut dyn Observer) -> RunReport {
    engine(text, services)
        .expect("build")
        .run(&RunOptions::default(), observer)
}

fn run(text: &str) -> (RunReport, HostServices) {
    let s = HostServices::default();
    (run_observed(text, &s, &mut NullObserver), s)
}

/// (cycle, sequence) of the frames recorded by `node`.
fn frames(s: &HostServices, node: &str) -> Vec<(u64, u64)> {
    s.recorder
        .records()
        .into_iter()
        .filter(|r| r.node == node)
        .filter_map(|r| match r.value {
            RecordedValue::Frame { sequence, .. } => Some((r.cycle, sequence)),
            _ => None,
        })
        .collect()
}

fn checksums(s: &HostServices, node: &str) -> Vec<String> {
    s.recorder
        .records()
        .into_iter()
        .filter(|r| r.node == node)
        .filter_map(|r| match r.value {
            RecordedValue::Frame { checksum, .. } => Some(checksum),
            _ => None,
        })
        .collect()
}

/// (cycle, json) recorded by `node`.
fn jsons(s: &HostServices, node: &str) -> Vec<(u64, Json)> {
    s.recorder
        .records()
        .into_iter()
        .filter(|r| r.node == node)
        .filter_map(|r| match r.value {
            RecordedValue::Json { value } => Some((r.cycle, value)),
            _ => None,
        })
        .collect()
}

fn stats<'a>(r: &'a RunReport, id: &str) -> &'a NodeStats {
    r.nodes.iter().find(|n| n.id == id).expect("node")
}

/// One pixel per frame: pixel = 7 × sequence, so with threshold 20 frames 1–2 are dark, 3+ bright.
const CAM: &str =
    "{ id: cam, builtin: synthetic-source, config: { width: 1, height: 1, frames: 6 } }";
const GATE: &str = "{ id: gate, builtin: brightness-gate, config: { threshold: 20 } }";

/// A command sent at the boundary before a cycle.
type Step = (u64, Box<dyn Fn(&ControlHandle)>);

/// Sends commands from `cycle_completed`, i.e. at the boundary before the next cycle.
struct Script {
    control: ControlHandle,
    at: Vec<Step>,
}

impl Observer for Script {
    fn cycle_completed(&mut self, cycle: u64) {
        for (at, f) in &self.at {
            if *at == cycle + 1 {
                f(&self.control);
            }
        }
    }
}

fn run_scripted(text: &str, services: &HostServices, at: Vec<Step>) -> RunReport {
    let mut e = engine(text, services).expect("build");
    let mut script = Script {
        control: e.control(),
        at,
    };
    e.run(&RunOptions::default(), &mut script)
}

// ------------------------------------------------------------------------------------------ fork

#[test]
fn fork_with_frame_emits_on_all_output_ports() {
    let (r, s) = run(&doc(
        &[
            CAM,
            "{ id: fork, primitive: fork }",
            "{ id: a, builtin: record }",
            "{ id: b, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: fork.frame }",
            "{ from: fork.toA, to: a.frame }",
            "{ from: fork.toB, to: b.frame }",
        ],
        &[],
        &[],
        "",
    ));
    assert_eq!(r.end, EndReason::SourceExhausted { node: "cam".into() });
    let all: Vec<_> = (1..=6).map(|c| (c, c)).collect();
    assert_eq!(frames(&s, "a"), all);
    assert_eq!(frames(&s, "b"), all);
    assert_eq!(
        checksums(&s, "a"),
        checksums(&s, "b"),
        "the same frame, unchanged"
    );
    assert_eq!(
        r.data_plane.heap_deliveries,
        6 * 3,
        "fan-out shares the payload"
    );
}

#[test]
fn fork_without_frame_is_skipped_and_absence_propagates() {
    let (r, s) = run(&doc(
        &[
            CAM,
            GATE,
            "{ id: when, primitive: if }",
            "{ id: fork, primitive: fork }",
            "{ id: a, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: gate.frame }",
            "{ from: cam.frame, to: when.frame }",
            "{ from: when.acceptedFrame, to: fork.frame }",
            "{ from: fork.out, to: a.frame }",
        ],
        &["{ from: gate.bright, to: when.productPresent }"],
        &[],
        "",
    ));
    assert_eq!(frames(&s, "a"), [(3, 3), (4, 4), (5, 5), (6, 6)]);
    assert_eq!(
        (stats(&r, "fork").executed, stats(&r, "fork").skipped),
        (4, 2)
    );
}

#[test]
fn fork_with_no_output_ports_is_a_leaf_that_emits_nothing() {
    let (r, _) = run(&doc(
        &[CAM, "{ id: fork, primitive: fork }"],
        &["{ from: cam.frame, to: fork.frame }"],
        &[],
        &[],
        "",
    ));
    assert_eq!(stats(&r, "fork").executed, 6);
}

// ------------------------------------------------------------------------------------------ if

#[test]
fn if_passes_the_frame_only_while_the_gate_is_open() {
    let (r, s) = run(&doc(
        &[
            CAM,
            GATE,
            "{ id: when, primitive: if }",
            "{ id: sink, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: gate.frame }",
            "{ from: cam.frame, to: when.frame }",
            "{ from: when.acceptedFrame, to: sink.frame }",
        ],
        &["{ from: gate.bright, to: when.productPresent }"],
        &[],
        "",
    ));
    assert_eq!(frames(&s, "sink"), [(3, 3), (4, 4), (5, 5), (6, 6)]);
    let when = stats(&r, "when");
    assert_eq!(
        (when.executed, when.skipped),
        (4, 2),
        "closed gate: skipped, no output"
    );
}

#[test]
fn if_without_frame_or_control_emits_nothing() {
    // `first` only passes bright frames; the second gate's frame and control both come from it, so
    // on dark cycles `second` has neither and stays silent.
    let (r, s) = run(&doc(
        &[
            CAM,
            GATE,
            "{ id: first, primitive: if }",
            "{ id: gate2, builtin: brightness-gate, config: { threshold: 0 } }",
            "{ id: second, primitive: if }",
            "{ id: sink, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: gate.frame }",
            "{ from: cam.frame, to: first.frame }",
            "{ from: first.acceptedFrame, to: gate2.frame }",
            "{ from: first.acceptedFrame, to: second.frame }",
            "{ from: second.acceptedFrame, to: sink.frame }",
        ],
        &[
            "{ from: gate.bright, to: first.productPresent }",
            "{ from: gate2.bright, to: second.productPresent }",
        ],
        &[],
        "",
    ));
    assert_eq!(frames(&s, "sink").len(), 4);
    assert_eq!(stats(&r, "second").skipped, 2);
}

// ------------------------------------------------------------------------------------------ switch

fn switch_doc(outputs: &[&str]) -> String {
    let mut data = vec![
        "{ from: cam.frame, to: gate.frame }".to_string(),
        "{ from: cam.frame, to: route.frame }".to_string(),
        "{ from: gate.class, to: route.class }".to_string(),
    ];
    let mut nodes = vec![
        CAM.to_string(),
        GATE.to_string(),
        "{ id: route, primitive: switch }".to_string(),
    ];
    for port in outputs {
        nodes.push(format!("{{ id: to-{port}, builtin: record }}"));
        data.push(format!("{{ from: route.{port}, to: to-{port}.frame }}"));
    }
    let nodes: Vec<&str> = nodes.iter().map(String::as_str).collect();
    let data: Vec<&str> = data.iter().map(String::as_str).collect();
    doc(&nodes, &data, &[], &[], "")
}

#[test]
fn switch_class_matches_port_routes_to_correct_port() {
    let (_, s) = run(&switch_doc(&["bright", "dark"]));
    assert_eq!(frames(&s, "to-dark"), [(1, 1), (2, 2)]);
    assert_eq!(frames(&s, "to-bright"), [(3, 3), (4, 4), (5, 5), (6, 6)]);
}

#[test]
fn switch_no_match_falls_to_default() {
    let (_, s) = run(&switch_doc(&["bright", "default"]));
    assert_eq!(frames(&s, "to-default"), [(1, 1), (2, 2)]);
    assert_eq!(frames(&s, "to-bright").len(), 4);
}

#[test]
fn switch_no_match_no_default_returns_no_output() {
    let (r, s) = run(&switch_doc(&["bright"]));
    assert_eq!(frames(&s, "to-bright").len(), 4);
    assert_eq!(stats(&r, "route").executed, 6, "it ran and emitted nothing");
    assert_eq!(
        stats(&r, "to-bright").executed,
        6,
        "record runs; it just has nothing"
    );
}

// ------------------------------------------------------------------------------------------ value + select

const CAMERAS: &str = r#"{ id: cameras, primitive: value, config: { type: json, shape: list, schema: { type: object, required: [serial, protocol] }, default: [ { serial: ABC123, protocol: gige }, { serial: DEF456, protocol: gige }, { serial: GHI789, protocol: usb } ] } }"#;

fn select_doc(select: &str, extra_nodes: &[&str], extra_edges: &[&str]) -> String {
    let mut nodes = vec![CAM, CAMERAS, select, "{ id: out, builtin: record }"];
    nodes.extend_from_slice(extra_nodes);
    let mut data = vec![
        "{ from: cameras.value, to: pick.items }",
        "{ from: pick.selected, to: out.json }",
    ];
    data.extend_from_slice(extra_edges);
    doc(&nodes, &data, &[], &[], "")
}

#[test]
fn execute_mode_one_selects_by_property() {
    let (_, s) = run(&select_doc(
        "{ id: pick, primitive: select, config: { mode: one, by: serial, where: DEF456 } }",
        &[],
        &[],
    ));
    let got = jsons(&s, "out");
    assert_eq!(got.len(), 6, "every cycle");
    assert_eq!(got[0].1, json!({"serial": "DEF456", "protocol": "gige"}));
}

#[test]
fn execute_mode_many_narrows_the_collection() {
    let (_, s) = run(&select_doc(
        "{ id: pick, primitive: select, config: { mode: many, by: protocol, where: gige } }",
        &[],
        &[],
    ));
    assert_eq!(
        jsons(&s, "out")[0].1,
        json!([{"serial": "ABC123", "protocol": "gige"}, {"serial": "DEF456", "protocol": "gige"}])
    );
}

#[test]
fn execute_mode_many_emits_an_empty_collection_when_nothing_matches() {
    let (_, s) = run(&select_doc(
        "{ id: pick, primitive: select, config: { mode: many, by: protocol, where: coaxpress } }",
        &[],
        &[],
    ));
    assert_eq!(jsons(&s, "out")[0].1, json!([]));
}

#[test]
fn execute_mode_one_emits_nothing_when_nothing_matches() {
    let (_, s) = run(&select_doc(
        "{ id: pick, primitive: select, config: { mode: one, by: serial, where: NOPE } }",
        &[],
        &[],
    ));
    assert!(jsons(&s, "out").is_empty());
}

#[test]
fn execute_object_criterion_matches_field_by_field() {
    let (_, s) = run(&select_doc(
        "{ id: pick, primitive: select, config: { mode: many, where: { protocol: gige, serial: ABC123 } } }",
        &[],
        &[],
    ));
    assert_eq!(
        jsons(&s, "out")[0].1,
        json!([{"serial": "ABC123", "protocol": "gige"}])
    );
}

#[test]
fn execute_criterion_on_the_edge_wins_over_the_static_one() {
    let (_, s) = run(&select_doc(
        "{ id: pick, primitive: select, config: { mode: one, by: serial, where: ABC123 } }",
        &["{ id: want, primitive: value, config: { type: json, literal: GHI789 } }"],
        &["{ from: want.value, to: pick.criterion }"],
    ));
    assert_eq!(
        jsons(&s, "out")[0].1,
        json!({"serial": "GHI789", "protocol": "usb"})
    );
}

#[test]
fn execute_scalar_elements_compare_directly() {
    let (_, s) = run(&doc(
        &[
            CAM,
            "{ id: nums, primitive: value, config: { type: int, shape: list, literal: [1, 2, 3, 2] } }",
            "{ id: pick, primitive: select, config: { mode: many, type: int, where: 2 } }",
            "{ id: out, builtin: record }",
        ],
        &[
            "{ from: nums.value, to: pick.items }",
            "{ from: pick.selected, to: out.json }",
        ],
        &[],
        &[],
        "",
    ));
    assert_eq!(jsons(&s, "out")[0].1, json!([2, 2]));
}

#[test]
fn select_without_any_criterion_fails_before_the_run() {
    let s = HostServices::default();
    let err = engine(
        &select_doc(
            "{ id: pick, primitive: select, config: { mode: one, by: serial } }",
            &[],
            &[],
        ),
        &s,
    )
    .expect_err("unattended host: nothing can answer");
    assert!(err.to_string().contains("no `where`, no binding"), "{err}");
}

#[test]
fn execute_emits_the_same_constant_every_cycle_and_carries_json_whole() {
    let (r, s) = run(&doc(
        &[
            CAM,
            "{ id: v, primitive: value, config: { type: json, literal: { line: 4, zones: [a, b] } } }",
            "{ id: out, builtin: record }",
        ],
        &["{ from: v.value, to: out.json }"],
        &[],
        &[],
        "",
    ));
    let got = jsons(&s, "out");
    assert_eq!(got.len(), 6);
    assert!(
        got.iter()
            .all(|(_, v)| *v == json!({"line": 4, "zones": ["a", "b"]}))
    );
    assert_eq!(stats(&r, "v").executed, 6, "every node runs every cycle");
}

fn tuned_doc() -> String {
    select_doc(
        "{ id: pick, primitive: select, config: { mode: many, by: protocol } }",
        &[
            "{ id: want, primitive: value, config: { type: json, literal: gige, binding: wanted.protocol } }",
        ],
        &["{ from: want.value, to: pick.criterion }"],
    )
}

#[test]
fn a_tuned_value_reaches_the_graph_on_the_next_cycle_without_stopping_the_run() {
    let s = HostServices::default();
    let r = run_scripted(
        &tuned_doc(),
        &s,
        vec![(
            4,
            Box::new(|c: &ControlHandle| {
                c.set_value("want", json!("usb"));
            }),
        )],
    );
    assert_eq!(r.end, EndReason::SourceExhausted { node: "cam".into() });
    let got = jsons(&s, "out");
    let gige =
        json!([{"serial": "ABC123", "protocol": "gige"}, {"serial": "DEF456", "protocol": "gige"}]);
    let usb = json!([{"serial": "GHI789", "protocol": "usb"}]);
    assert_eq!(
        got.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
        (1..=6).collect::<Vec<_>>()
    );
    assert!(
        got[..3].iter().all(|(_, v)| *v == gige),
        "cycles 1-3 keep the old value"
    );
    assert!(
        got[3..].iter().all(|(_, v)| *v == usb),
        "cycle 4 sees the change"
    );
    assert_eq!(r.live[0].kind, LiveEventKind::ValueSet);
    assert_eq!(r.live[0].cycle, 4);
    assert_eq!(stats(&r, "want").generation, 1, "no restart");
}

#[test]
fn a_tuned_value_is_type_and_schema_checked_before_it_reaches_the_graph() {
    let s = HostServices::default();
    let text = doc(
        &[
            CAM,
            "{ id: thr, primitive: value, config: { type: int, literal: 40, schema: { type: integer, minimum: 0, maximum: 255 } } }",
        ],
        &[],
        &[],
        &[],
        "",
    );
    let r = run_scripted(
        &text,
        &s,
        vec![
            (
                2,
                Box::new(|c: &ControlHandle| {
                    c.set_value("thr", json!("forty"));
                }),
            ),
            (
                3,
                Box::new(|c: &ControlHandle| {
                    c.set_value("thr", json!(999));
                }),
            ),
            (
                4,
                Box::new(|c: &ControlHandle| {
                    c.set_value("nope", json!(1));
                }),
            ),
            (
                5,
                Box::new(|c: &ControlHandle| {
                    c.set_value("cam", json!(1));
                }),
            ),
        ],
    );
    let kinds: Vec<_> = r.live.iter().map(|e| e.kind).collect();
    assert_eq!(kinds, [LiveEventKind::Rejected; 4]);
    assert!(
        r.live[0]
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("expected int")
    );
    assert_eq!(r.live[1].message.as_deref(), Some("$: must be <= 255"));
    assert!(
        r.live[2]
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("no node `nope`")
    );
    assert!(
        r.live[3]
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("only `value` and `select`")
    );
}

#[test]
fn tuning_a_list_applies_the_schema_to_each_element_and_rejects_heterogeneous_lists() {
    let s = HostServices::default();
    let r = run_scripted(
        &select_doc(
            "{ id: pick, primitive: select, config: { mode: many, by: protocol, where: gige } }",
            &[],
            &[],
        ),
        &s,
        vec![
            (
                2,
                Box::new(|c: &ControlHandle| {
                    c.set_value("cameras", json!([{"serial": "X"}]));
                }),
            ),
            (
                3,
                Box::new(|c: &ControlHandle| {
                    c.set_value("cameras", json!([{"serial": "X", "protocol": "gige"}]));
                }),
            ),
        ],
    );
    assert_eq!(r.live[0].kind, LiveEventKind::Rejected);
    assert_eq!(
        r.live[0].message.as_deref(),
        Some("$[0]: missing required property 'protocol'")
    );
    assert_eq!(r.live[1].kind, LiveEventKind::ValueSet);
    let got = jsons(&s, "out");
    assert_eq!(
        got[2].1,
        json!([{"serial": "X", "protocol": "gige"}]),
        "the candidate list itself changed"
    );
}

#[test]
fn a_select_fed_by_an_edge_is_not_offered_as_a_tunable() {
    let s = HostServices::default();
    let r = run_scripted(
        &tuned_doc(),
        &s,
        vec![(
            2,
            Box::new(|c: &ControlHandle| {
                c.set_value("pick", json!("usb"));
            }),
        )],
    );
    assert_eq!(r.live[0].kind, LiveEventKind::Rejected);
    assert!(
        r.live[0]
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("edge")
    );
}

#[test]
fn choosing_another_candidate_mid_run_re_narrows_on_the_next_cycle_and_is_persisted() {
    let store =
        Arc::new(Bindings::memory([("camera.serial".to_string(), json!("ABC123"))]).without_env());
    let s = HostServices {
        bindings: store.clone(),
        ..HostServices::default()
    };
    let r = run_scripted(
        &select_doc(
            "{ id: pick, primitive: select, config: { mode: one, by: serial, binding: camera.serial } }",
            &[],
            &[],
        ),
        &s,
        vec![(
            3,
            Box::new(|c: &ControlHandle| {
                c.set_value("pick", json!("GHI789"));
            }),
        )],
    );
    let serials: Vec<_> = jsons(&s, "out")
        .into_iter()
        .map(|(_, v)| v["serial"].clone())
        .collect();
    assert_eq!(
        serials,
        [
            json!("ABC123"),
            json!("ABC123"),
            json!("GHI789"),
            json!("GHI789"),
            json!("GHI789"),
            json!("GHI789")
        ]
    );
    assert_eq!(r.live[0].kind, LiveEventKind::ValueSet);
    assert_eq!(store.stored("camera.serial"), Some(json!("GHI789")));
}

// ------------------------------------------------------------------------------------------ bindings pre-pass

fn bound_value_doc(config: &str) -> String {
    doc(
        &[
            CAM,
            &format!("{{ id: v, primitive: value, config: {config} }}"),
            "{ id: out, builtin: record }",
        ],
        &["{ from: v.value, to: out.json }"],
        &[],
        &[],
        "",
    )
}

fn first_json(text: &str, bindings: Bindings) -> Result<Json, String> {
    let s = HostServices {
        bindings: Arc::new(bindings.without_env()),
        ..HostServices::default()
    };
    let mut e = engine(text, &s).map_err(|e| e.to_string())?;
    e.run(
        &RunOptions {
            max_cycles: Some(1),
            ..RunOptions::default()
        },
        &mut NullObserver,
    );
    Ok(jsons(&s, "out")[0].1.clone())
}

#[test]
fn run_literal_needs_no_binding_and_asks_nobody() {
    let v = first_json(
        &bound_value_doc("{ type: json, literal: { a: 1 } }"),
        Bindings::default(),
    );
    assert_eq!(v, Ok(json!({"a": 1})));
}

#[test]
fn run_reads_a_stored_binding_and_falls_back_to_the_default() {
    let text = bound_value_doc("{ type: json, binding: line.name, default: fallback }");
    let stored = Bindings::memory([("line.name".to_string(), json!("line-4"))]);
    assert_eq!(first_json(&text, stored), Ok(json!("line-4")));
    assert_eq!(
        first_json(&text, Bindings::default()),
        Ok(json!("fallback"))
    );
}

#[test]
fn run_fails_with_the_binding_name_and_where_to_set_it() {
    let err = first_json(
        &bound_value_doc("{ type: json, binding: camera.address }"),
        Bindings::default(),
    )
    .expect_err("unresolved");
    assert!(
        err.contains("camera.address") && err.contains("MVF_BINDING_CAMERA_ADDRESS"),
        "{err}"
    );
}

#[test]
fn run_rejects_a_stored_binding_of_the_wrong_type_or_schema() {
    let text = bound_value_doc(
        "{ type: json, binding: cam, schema: { type: object, required: [serial] } }",
    );
    let err = first_json(
        &text,
        Bindings::memory([("cam".to_string(), json!({"address": "x"}))]),
    )
    .expect_err("schema");
    assert!(err.contains("missing required property 'serial'"), "{err}");
}

#[test]
fn two_machines_bind_the_same_pipeline_file_to_different_cameras() {
    let text = select_doc(
        "{ id: pick, primitive: select, config: { mode: one, by: serial, binding: camera.serial } }",
        &[],
        &[],
    );
    let one = first_json(
        &text,
        Bindings::memory([("camera.serial".to_string(), json!("ABC123"))]),
    );
    let two = first_json(
        &text,
        Bindings::memory([("camera.serial".to_string(), json!("GHI789"))]),
    );
    assert_eq!(one.map(|v| v["serial"].clone()), Ok(json!("ABC123")));
    assert_eq!(two.map(|v| v["serial"].clone()), Ok(json!("GHI789")));
}

#[test]
fn tuned_values_persist_to_the_binding_file_and_come_back_next_run() {
    let dir = std::env::temp_dir().join(format!("mvf-live-{}", std::process::id()));
    let path = dir.join(".mvf/bindings.json");
    let text = bound_value_doc("{ type: json, binding: brightness.threshold, default: 40 }");
    let s = HostServices {
        bindings: Arc::new(Bindings::file(&path).without_env()),
        ..HostServices::default()
    };
    let r = run_scripted(
        &text,
        &s,
        vec![(
            2,
            Box::new(|c: &ControlHandle| {
                c.set_value("v", json!(60));
            }),
        )],
    );
    assert!(r.live[0].persisted);
    assert_eq!(
        first_json(&text, Bindings::file(&path)),
        Ok(json!(60)),
        "tomorrow's run starts where the operator left off"
    );
    let _ = std::fs::remove_dir_all(dir);
}

// ------------------------------------------------------------------------------------------ loop

fn loop_doc(mode: &str, frames: u64, run: &str) -> String {
    doc(
        &[
            "{ id: cycle, primitive: loop, config: { mode: MODE } }"
                .replace("MODE", mode)
                .as_str(),
            &format!(
                "{{ id: cam, builtin: synthetic-source, config: {{ width: 1, height: 1, frames: {frames} }} }}"
            ),
            "{ id: thr, primitive: value, config: { type: int, literal: 40 } }",
            "{ id: sink, builtin: record }",
        ],
        &["{ from: cam.frame, to: sink.frame }"],
        &[],
        &["{ from: sink, to: cycle }"],
        run,
    )
}

#[test]
fn loop_graph_is_valid_and_every_node_runs_every_cycle() {
    let (r, s) = run(&loop_doc("until-exhausted", 4, ""));
    assert_eq!(r.end, EndReason::SourceExhausted { node: "cam".into() });
    assert_eq!(frames(&s, "sink").len(), 4);
    for id in ["cam", "thr", "sink"] {
        assert!(
            stats(&r, id).executed >= 4,
            "{id}: no setup region, nothing runs once"
        );
    }
    assert_eq!(
        stats(&r, "cycle").executed,
        0,
        "the loop owns iteration; it is not a stage"
    );
    assert_eq!(
        r.iteration.as_ref().map(|i| i.mode),
        Some(mvf_graph::LoopMode::UntilExhausted)
    );
}

#[test]
fn forever_mode_rewinds_the_source_and_keeps_going() {
    let (r, s) = run(&loop_doc("forever", 3, "maxCycles: 8"));
    assert_eq!(r.end, EndReason::CycleLimit { limit: 8 });
    let got: Vec<u64> = frames(&s, "sink").into_iter().map(|(_, q)| q).collect();
    assert_eq!(
        got,
        [1, 2, 3, 1, 2, 3, 1, 2],
        "replayed from frame 1; exhaustion passes not counted"
    );
    let cycles: Vec<u64> = frames(&s, "sink").into_iter().map(|(c, _)| c).collect();
    assert_eq!(cycles, (1..=8).collect::<Vec<_>>());
    assert_eq!(r.iteration.as_ref().map(|i| i.rewinds), Some(2));
}

#[test]
fn forever_does_not_spin_on_a_source_that_produces_nothing() {
    let (r, _) = run(&loop_doc("forever", 0, ""));
    assert_eq!(r.end, EndReason::SourceExhausted { node: "cam".into() });
    assert_eq!(r.iteration.as_ref().map(|i| i.rewinds), Some(0));
}

#[test]
fn count_mode_stops_after_exactly_n_cycles_as_a_completed_run() {
    let dir = std::env::temp_dir().join(format!("mvf-count-{}", std::process::id()));
    let text = doc(
        &[
            "{ id: cycle, primitive: loop, config: { mode: count, count: 5 } }",
            "{ id: cam, builtin: synthetic-source, config: { width: 1, height: 1 } }",
            "{ id: sink, builtin: record }",
        ],
        &["{ from: cam.frame, to: sink.frame }"],
        &[],
        &[],
        &format!(
            "checkpoint: {{ everyCycles: 2, directory: {} }}",
            dir.display()
        ),
    );
    let (r, s) = run(&text);
    assert_eq!(r.end, EndReason::LoopCompleted { count: 5 });
    assert_eq!(frames(&s, "sink").len(), 5);
    assert!(
        !dir.exists(),
        "a completed count drops its checkpoint like an exhausted source"
    );
}

#[test]
fn pausing_freezes_the_loop_and_resuming_continues_where_it_left_off() {
    let s = HostServices::default();
    let r = run_scripted(
        &loop_doc("until-exhausted", 6, ""),
        &s,
        vec![(
            3,
            Box::new(|c: &ControlHandle| {
                c.pause(None);
                let c = c.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    c.resume();
                });
            }),
        )],
    );
    assert_eq!(
        frames(&s, "sink"),
        (1..=6).map(|c| (c, c)).collect::<Vec<_>>(),
        "state kept, nothing lost"
    );
    let it = r.iteration.as_ref().expect("loop");
    assert_eq!(it.pauses, 1);
    assert!(it.paused_ms >= 140.0, "{}", it.paused_ms);
    let kinds: Vec<_> = r.live.iter().map(|e| e.kind).collect();
    assert_eq!(kinds, [LiveEventKind::Paused, LiveEventKind::Resumed]);
}

#[test]
fn pause_resumes_by_itself_and_needs_a_loop() {
    let s = HostServices::default();
    let r = run_scripted(
        &loop_doc("until-exhausted", 4, ""),
        &s,
        vec![(
            2,
            Box::new(|c: &ControlHandle| {
                c.pause(Some(60));
            }),
        )],
    );
    assert!(r.iteration.as_ref().is_some_and(|i| i.paused_ms >= 50.0));
    let s = HostServices::default();
    let r = run_scripted(
        &doc(
            &[CAM, "{ id: sink, builtin: record }"],
            &["{ from: cam.frame, to: sink.frame }"],
            &[],
            &[],
            "",
        ),
        &s,
        vec![(
            2,
            Box::new(|c: &ControlHandle| {
                c.pause(None);
            }),
        )],
    );
    assert_eq!(
        r.live[0].kind,
        LiveEventKind::Rejected,
        "pause is the loop's running state"
    );
    assert_eq!(frames(&s, "sink").len(), 6);
}

// ------------------------------------------------------------------------------------------ module bindings

fn seeded_doc() -> String {
    doc(
        &[
            "{ id: cam, builtin: synthetic-source, config: { width: 1, height: 1, frames: 6, seed: 0 }, bindings: { seed: { type: int, binding: cam.seed, schema: { type: integer, minimum: 0 } } } }",
            "{ id: sink, builtin: record }",
        ],
        &["{ from: cam.frame, to: sink.frame }"],
        &[],
        &[],
        "",
    )
}

#[test]
fn pre_pass_overlays_a_stored_edit_onto_config_and_rejects_the_wrong_type() {
    let plain = run(&seeded_doc()).1;
    let s = HostServices {
        bindings: Arc::new(Bindings::memory([("cam.seed".to_string(), json!(5))]).without_env()),
        ..HostServices::default()
    };
    run_observed(&seeded_doc(), &s, &mut NullObserver);
    assert_ne!(
        checksums(&plain, "sink"),
        checksums(&s, "sink"),
        "the stored seed came back"
    );
    let bad = HostServices {
        bindings: Arc::new(
            Bindings::memory([("cam.seed".to_string(), json!("five"))]).without_env(),
        ),
        ..HostServices::default()
    };
    let err = engine(&seeded_doc(), &bad).expect_err("wrong type");
    assert!(err.to_string().contains("not a valid value:int"), "{err}");
}

#[test]
fn editing_a_module_binding_reopens_the_node_with_the_new_value() {
    let store = Arc::new(Bindings::memory([]).without_env());
    let s = HostServices {
        bindings: store.clone(),
        ..HostServices::default()
    };
    let r = run_scripted(
        &seeded_doc(),
        &s,
        vec![
            (
                3,
                Box::new(|c: &ControlHandle| {
                    c.configure("cam", "seed", json!(9));
                }),
            ),
            (
                4,
                Box::new(|c: &ControlHandle| {
                    c.configure("cam", "seed", json!(-1));
                }),
            ),
            (
                4,
                Box::new(|c: &ControlHandle| {
                    c.configure("cam", "frames", json!(2));
                }),
            ),
        ],
    );
    let seqs: Vec<u64> = frames(&s, "sink").into_iter().map(|(_, q)| q).collect();
    assert_eq!(
        seqs,
        [1, 2, 1, 2, 3, 4, 5, 6],
        "reopened: a source restarts at its first frame"
    );
    let kinds: Vec<_> = r.live.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        [
            LiveEventKind::Reopened,
            LiveEventKind::Rejected,
            LiveEventKind::Rejected
        ]
    );
    assert_eq!(r.live[1].message.as_deref(), Some("$: must be >= 0"));
    assert!(
        r.live[2]
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("not a declared binding")
    );
    assert_eq!(
        stats(&r, "cam").generation,
        2,
        "fenced like any new generation"
    );
    assert_eq!(store.stored("cam.seed"), Some(json!(9)));
}

// ------------------------------------------------------------------------------------------ on-demand

fn on_demand_doc(threshold: u64) -> String {
    doc(
        &[
            CAM,
            &format!(
                "{{ id: gate, builtin: brightness-gate, config: {{ threshold: {threshold} }} }}"
            ),
            "{ id: when, primitive: if }",
            "{ id: helper, contract: on-demand-passthrough.yaml }",
            "{ id: sink, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: gate.frame }",
            "{ from: cam.frame, to: when.frame }",
            "{ from: when.acceptedFrame, to: helper.frame }",
            "{ from: helper.frame, to: sink.frame }",
        ],
        &["{ from: gate.bright, to: when.productPresent }"],
        &[],
        "",
    )
}

#[test]
fn on_demand_node_that_never_receives_a_frame_is_never_activated() {
    let (r, s) = run(&on_demand_doc(255));
    assert!(frames(&s, "sink").is_empty());
    let helper = stats(&r, "helper");
    assert_eq!((helper.executed, helper.activated_at_cycle), (0, None));
    assert!(r.live.is_empty());
}

#[test]
fn on_demand_node_that_receives_a_frame_activates_lazily_and_runs() {
    let (r, s) = run(&on_demand_doc(20));
    assert_eq!(frames(&s, "sink"), [(3, 3), (4, 4), (5, 5), (6, 6)]);
    assert_eq!(stats(&r, "helper").activated_at_cycle, Some(3));
    assert_eq!(r.live[0].kind, LiveEventKind::Activated);
    assert_eq!(r.live[0].node.as_deref(), Some("helper"));
}
