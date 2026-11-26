//! Graph validation: every rejection carries a stable code; valid graphs order deterministically.

use std::collections::HashMap;

use mvf_contract::{ContractError, NodeContract, parse_contract};
use mvf_graph::{ContractResolver, GraphError, build_graph, topological_order};

/// Test resolver with a few inline contracts.
struct Fixtures(HashMap<&'static str, NodeContract>);

fn contract(id: &str, inputs: &str, outputs: &str) -> NodeContract {
    let text = format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: {{ id: {id}, version: 1.0.0 }}\nspec:\n  ports:\n    inputs: [{inputs}]\n    outputs: [{outputs}]\n  artifacts: [ {{ backend: in-process, builtin: {id} }} ]\n"
    );
    parse_contract(&text).unwrap_or_else(|e| panic!("{id}: {e}"))
}

fn fixtures() -> Fixtures {
    let frame_in = "{ name: frame, channel: data, type: frame }";
    let frame_out = frame_in;
    Fixtures(HashMap::from([
        ("source", contract("source", "", frame_out)),
        ("proc", contract("proc", frame_in, frame_out)),
        ("sink", contract("sink", frame_in, "")),
        (
            "classifier",
            contract(
                "classifier",
                frame_in,
                "{ name: class, channel: data, type: classification }, { name: ok, channel: control, type: signal }",
            ),
        ),
        (
            "gated",
            contract(
                "gated",
                "{ name: frame, channel: data, type: frame }, { name: enable, channel: control, type: signal, required: false }",
                "",
            ),
        ),
        (
            "joiner",
            contract(
                "joiner",
                "{ name: a, channel: data, type: frame }, { name: b, channel: data, type: frame, required: false }",
                frame_out,
            ),
        ),
    ]))
}

impl ContractResolver for Fixtures {
    fn builtin(&self, name: &str) -> Option<NodeContract> {
        self.0.get(name).cloned()
    }
    fn file(&self, path: &str) -> Result<(String, NodeContract), ContractError> {
        Err(ContractError::Invalid(vec![mvf_contract::ContractIssue {
            code: "test.no-files",
            path: path.into(),
            message: "no files in this test".into(),
        }]))
    }
}

fn pipeline(nodes: &str, data: &str, control: &str) -> String {
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: t }}\nspec:\n  nodes:\n{nodes}\n  edges:\n    data: [{data}]\n    control: [{control}]\n"
    )
}

fn node(id: &str, builtin: &str) -> String {
    format!("    - {{ id: {id}, builtin: {builtin} }}")
}

fn codes(text: &str) -> Vec<&'static str> {
    match build_graph(text, &fixtures()) {
        Ok(_) => vec![],
        Err(e) => e.codes(),
    }
}

fn nodes(list: &[(&str, &str)]) -> String {
    list.iter()
        .map(|(i, b)| node(i, b))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn valid_linear_graph_orders_deterministically() {
    let text = pipeline(
        &nodes(&[("sink", "sink"), ("cam", "source"), ("p", "proc")]),
        "{ from: cam.frame, to: p.frame }, { from: p.frame, to: sink.frame }",
        "",
    );
    let graph = build_graph(&text, &fixtures()).expect("valid");
    let ids: Vec<_> = graph
        .order
        .iter()
        .map(|i| graph.nodes[*i].id.as_str())
        .collect();
    assert_eq!(ids, ["cam", "p", "sink"]);
    assert_eq!(graph.data_edges.len(), 2);
    assert!(graph.control_edges.is_empty());
}

#[test]
fn rejects_missing_nodes_and_ports() {
    let text = pipeline(
        &nodes(&[("cam", "source"), ("sink", "sink")]),
        "{ from: ghost.frame, to: sink.frame }, { from: cam.nope, to: sink.frame }, { from: cam.frame, to: nobody.frame }, { from: cam.frame, to: sink.nope }",
        "",
    );
    let c = codes(&text);
    for expected in [
        "graph.edge.missing-source-node",
        "graph.edge.missing-source-port",
        "graph.edge.missing-target-node",
        "graph.edge.missing-target-port",
    ] {
        assert!(c.contains(&expected), "missing {expected}: {c:?}");
    }
}

#[test]
fn rejects_type_mismatch() {
    let text = pipeline(
        &nodes(&[("cam", "source"), ("cls", "classifier"), ("sink", "sink")]),
        "{ from: cam.frame, to: cls.frame }, { from: cls.class, to: sink.frame }",
        "",
    );
    assert!(codes(&text).contains(&"graph.edge.type-mismatch"));
}

#[test]
fn rejects_control_port_on_a_data_edge_and_vice_versa() {
    let data_on_control = pipeline(
        &nodes(&[("cam", "source"), ("cls", "classifier"), ("g", "gated")]),
        "{ from: cam.frame, to: cls.frame }, { from: cam.frame, to: g.frame }, { from: cls.ok, to: g.enable }",
        "",
    );
    assert!(codes(&data_on_control).contains(&"graph.edge.channel-mismatch"));

    let frame_on_control = pipeline(
        &nodes(&[("cam", "source"), ("sink", "sink")]),
        "",
        "{ from: cam.frame, to: sink.frame }",
    );
    let c = codes(&frame_on_control);
    assert!(c.contains(&"graph.edge.channel-mismatch"), "{c:?}");
}

#[test]
fn accepts_control_edge_between_control_ports() {
    let text = pipeline(
        &nodes(&[("cam", "source"), ("cls", "classifier"), ("g", "gated")]),
        "{ from: cam.frame, to: cls.frame }, { from: cam.frame, to: g.frame }",
        "{ from: cls.ok, to: g.enable }",
    );
    let graph = build_graph(&text, &fixtures()).expect("valid");
    assert_eq!(graph.control_edges.len(), 1);
    let ids: Vec<_> = graph
        .order
        .iter()
        .map(|i| graph.nodes[*i].id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["cam", "cls", "g"],
        "control edges order the controller first"
    );
}

#[test]
fn rejects_illegal_cycles_with_the_cycle_path() {
    let text = pipeline(
        &nodes(&[
            ("cam", "source"),
            ("j", "joiner"),
            ("p", "proc"),
            ("sink", "sink"),
        ]),
        "{ from: cam.frame, to: j.a }, { from: j.frame, to: p.frame }, { from: p.frame, to: j.b }, { from: p.frame, to: sink.frame }",
        "",
    );
    match build_graph(&text, &fixtures()) {
        Err(GraphError::Invalid(issues)) => {
            let cycle = issues
                .iter()
                .find(|i| i.code == "graph.cycle")
                .expect("cycle");
            assert!(cycle.message.contains("j -> p -> j"), "{}", cycle.message);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn rejects_implicit_joins_and_duplicates() {
    let text = pipeline(
        &nodes(&[("a", "source"), ("b", "source"), ("sink", "sink")]),
        "{ from: a.frame, to: sink.frame }, { from: b.frame, to: sink.frame }, { from: a.frame, to: sink.frame }",
        "",
    );
    let c = codes(&text);
    assert!(c.contains(&"graph.input.multiple-edges"), "{c:?}");
    assert!(c.contains(&"graph.edge.duplicate"), "{c:?}");
}

#[test]
fn rejects_unconnected_required_input_and_missing_source() {
    let text = pipeline(&nodes(&[("p", "proc")]), "", "");
    let c = codes(&text);
    assert!(c.contains(&"graph.input.unconnected-required"), "{c:?}");
    assert!(c.contains(&"graph.node.no-source"), "{c:?}");
}

#[test]
fn rejects_bad_nodes() {
    let text = pipeline(
        "    - { id: cam, builtin: source }\n    - { id: cam, builtin: source }\n    - { id: Bad, builtin: source }\n    - { id: x, builtin: nope }\n    - { id: y }\n    - { id: z, builtin: source, contract: z.yaml }\n    - { id: w, contract: w.yaml }",
        "",
        "",
    );
    let c = codes(&text);
    for expected in [
        "graph.node.duplicate-id",
        "graph.node.invalid-id",
        "graph.node.unknown-builtin",
        "graph.node.ambiguous-implementation",
        "graph.node.invalid-contract",
    ] {
        assert!(c.contains(&expected), "missing {expected}: {c:?}");
    }
}

#[test]
fn rejects_malformed_endpoints_and_unknown_fields() {
    let text = pipeline(
        &nodes(&[("cam", "source"), ("sink", "sink")]),
        "{ from: cam, to: sink.frame }",
        "",
    );
    assert!(codes(&text).contains(&"graph.edge.invalid-endpoint"));

    let unknown = pipeline(
        &nodes(&[("cam", "source")]),
        "{ from: cam.frame, to: x.frame, kind: data }",
        "",
    );
    assert_eq!(codes(&unknown), vec!["yaml.schema"]);

    let collapsed = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: t }\nspec:\n  nodes: []\n  edges: [ { from: a.b, to: c.d } ]\n";
    assert_eq!(
        codes(collapsed),
        vec!["yaml.schema"],
        "a single untyped edge list is not accepted"
    );
}

#[test]
fn rejects_zero_cycle_bound_and_bad_versions() {
    let text = "apiVersion: mvf.dev/v2\nkind: Pipe\nmetadata: { name: t }\nspec:\n  nodes: [ { id: cam, builtin: source } ]\n  run: { maxCycles: 0 }\n";
    let c = codes(text);
    for expected in [
        "graph.api-version.unsupported",
        "graph.kind.unsupported",
        "graph.run.zero-cycles",
    ] {
        assert!(c.contains(&expected), "missing {expected}: {c:?}");
    }
}

#[test]
fn topological_order_breaks_ties_by_declaration_index() {
    // 3 -> 0, 2 -> 0, 1 independent: ready set {1,2,3} drains in index order.
    assert_eq!(
        topological_order(4, &[(3, 0), (2, 0)]),
        Ok(vec![1, 2, 3, 0])
    );
    assert_eq!(topological_order(2, &[(0, 1), (1, 0)]), Err(vec![0, 1, 0]));
}
