//! Primitive typing and validation: synthesized contracts, refined value types, loop edges and the
//! value/select/binding rules (the .NET `pipeline.node.*` value codes).

use std::collections::HashMap;

use mvf_contract::{Channel, ContractError, NodeContract, PortType, parse_contract};
use mvf_graph::{ContractResolver, GraphError, LoopMode, build_graph};

struct Fixtures(HashMap<&'static str, NodeContract>);

fn contract(id: &str, inputs: &str, outputs: &str) -> NodeContract {
    let text = format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: {{ id: {id}, version: 1.0.0 }}\nspec:\n  ports:\n    inputs: [{inputs}]\n    outputs: [{outputs}]\n  artifacts: [ {{ backend: in-process, builtin: {id} }} ]\n"
    );
    parse_contract(&text).unwrap_or_else(|e| panic!("{id}: {e}"))
}

impl ContractResolver for Fixtures {
    fn builtin(&self, name: &str) -> Option<NodeContract> {
        self.0.get(name).cloned()
    }
    fn file(&self, path: &str) -> Result<(String, NodeContract), ContractError> {
        Err(ContractError::Invalid(vec![mvf_contract::ContractIssue {
            code: "test.no-files",
            path: path.into(),
            message: "no files".into(),
        }]))
    }
}

fn fixtures() -> Fixtures {
    let frame = "{ name: frame, channel: data, type: frame }";
    Fixtures(HashMap::from([
        ("source", contract("source", "", frame)),
        ("sink", contract("sink", frame, "")),
        (
            "classifier",
            contract(
                "classifier",
                frame,
                "{ name: class, channel: data, type: classification }",
            ),
        ),
        (
            "jsink",
            contract("jsink", "{ name: json, channel: data, type: json }", ""),
        ),
        (
            "gate",
            contract(
                "gate",
                frame,
                "{ name: ok, channel: control, type: signal }",
            ),
        ),
    ]))
}

fn pipeline(nodes: &str, data: &str, control: &str, loop_edges: &str) -> String {
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: t }}\nspec:\n  nodes: [{nodes}]\n  edges:\n    data: [{data}]\n    control: [{control}]\n    loop: [{loop_edges}]\n"
    )
}

fn codes(text: &str) -> Vec<&'static str> {
    match build_graph(text, &fixtures()) {
        Ok(_) => Vec::new(),
        Err(GraphError::Invalid(issues)) => issues.iter().map(|i| i.code).collect(),
        Err(e) => panic!("unexpected: {e}"),
    }
}

#[test]
fn inspection_shaped_graph_types_every_primitive_port() {
    let text = pipeline(
        "{ id: cycle, primitive: loop, config: { mode: forever } },
         { id: cam, builtin: source },
         { id: fork, primitive: fork },
         { id: cls, builtin: classifier },
         { id: route, primitive: switch },
         { id: ok, builtin: sink },
         { id: other, builtin: sink },
         { id: gate, builtin: gate },
         { id: when, primitive: if },
         { id: kept, builtin: sink },
         { id: thr, primitive: value, config: { type: int, literal: 40 } }",
        "{ from: cam.frame, to: fork.frame },
         { from: fork.toCls, to: cls.frame }, { from: fork.toRoute, to: route.frame },
         { from: fork.toGate, to: gate.frame }, { from: fork.toIf, to: when.frame },
         { from: cls.class, to: route.class },
         { from: route.ok, to: ok.frame }, { from: route.default, to: other.frame },
         { from: when.acceptedFrame, to: kept.frame }",
        "{ from: gate.ok, to: when.productPresent }",
        "{ from: ok, to: cycle }, { from: other, to: cycle }",
    );
    let g = build_graph(&text, &fixtures()).expect("valid");
    let node = |id: &str| &g.nodes[g.index_of(id).expect("node")];
    let fork_outs: Vec<_> = node("fork")
        .contract
        .spec
        .ports
        .outputs
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(
        fork_outs,
        ["toCls", "toRoute", "toGate", "toIf"],
        "outputs are the ones edges name"
    );
    let route_outs: Vec<_> = node("route")
        .contract
        .spec
        .ports
        .outputs
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(route_outs, ["ok", "default"]);
    let pp = node("when").input("productPresent").expect("port");
    assert_eq!(
        (pp.channel, pp.port_type),
        (Channel::Control, PortType::Signal)
    );
    assert_eq!(
        node("thr").output("value").map(|p| p.port_type),
        Some(PortType::Value)
    );
    assert!(
        !node("thr").is_source(),
        "a value is a constant, not a stream"
    );
    assert!(node("cam").is_source());
    let spec = g.loop_spec.expect("loop");
    assert_eq!((spec.mode, spec.node), (LoopMode::Forever, 0));
    assert_eq!(g.loop_edges.len(), 2);
}

#[test]
fn primitive_configs_are_strict() {
    let one = |node: &str| {
        codes(&pipeline(
            &format!("{{ id: cam, builtin: source }}, {node}"),
            "",
            "",
            "",
        ))
    };
    assert_eq!(
        one("{ id: f, primitive: fork, config: { x: 1 } }"),
        ["graph.primitive.invalid-config"]
    );
    assert_eq!(
        one("{ id: v, primitive: value, config: { type: decimal, literal: 1 } }"),
        ["graph.primitive.invalid-config"]
    );
    assert_eq!(
        one("{ id: v, primitive: value, config: { type: int, shape: matrix, literal: 1 } }"),
        ["graph.primitive.invalid-config"]
    );
    assert_eq!(
        one("{ id: s, primitive: select, config: { mode: all } }"),
        ["graph.primitive.invalid-config"]
    );
    assert_eq!(
        one("{ id: l, primitive: loop, config: { mode: sometimes } }"),
        ["graph.primitive.invalid-config"]
    );
    assert_eq!(
        one("{ id: x, primitive: fork, builtin: sink }"),
        ["graph.node.ambiguous-implementation"]
    );
}

#[test]
fn value_rules_match_the_dotnet_codes() {
    let one = |cfg: &str| {
        codes(&pipeline(
            &format!(
                "{{ id: cam, builtin: source }}, {{ id: v, primitive: value, config: {cfg} }}"
            ),
            "",
            "",
            "",
        ))
    };
    assert_eq!(
        one("{ type: int, literal: forty }"),
        ["graph.value.literal-type-mismatch"]
    );
    assert_eq!(
        one("{ type: int, literal: 300, schema: { maximum: 255 } }"),
        ["graph.value.literal-type-mismatch"]
    );
    assert_eq!(
        one("{ type: int, binding: t, default: x }"),
        ["graph.value.default-type-mismatch"]
    );
    assert_eq!(
        one("{ type: json, literal: 1, schema: { type: decimal } }"),
        ["graph.value.invalid-schema"]
    );
    assert_eq!(one("{ type: int }"), ["graph.value.unresolvable"]);
    assert_eq!(
        one(
            "{ type: json, shape: list, literal: [ { a: 1 }, { b: 2 } ], schema: { required: [a] } }"
        ),
        ["graph.value.literal-type-mismatch"]
    );
    assert!(
        one("{ type: int, binding: t }").is_empty(),
        "a binding alone is valid; resolving it is per machine"
    );
}

#[test]
fn refined_value_types_are_checked_between_primitives() {
    let text = pipeline(
        "{ id: cam, builtin: source },
         { id: nums, primitive: value, config: { type: int, shape: list, literal: [1] } },
         { id: pick, primitive: select, config: { mode: many, where: 1 } },
         { id: out, builtin: jsink }",
        "{ from: nums.value, to: pick.items }, { from: pick.selected, to: out.json }",
        "",
        "",
    );
    assert_eq!(
        codes(&text),
        ["graph.edge.value-type-mismatch"],
        "list:int into list:json"
    );
    let text = pipeline(
        "{ id: cam, builtin: source },
         { id: nums, primitive: value, config: { type: int, shape: list, literal: [1] } },
         { id: want, primitive: value, config: { type: int, literal: 1 } },
         { id: pick, primitive: select, config: { mode: many, type: string } }",
        "{ from: nums.value, to: pick.items }, { from: want.value, to: pick.criterion }",
        "",
        "",
    );
    let c = codes(&text);
    assert!(c.contains(&"graph.edge.value-type-mismatch"), "{c:?}");
}

#[test]
fn loop_rules() {
    let base = "{ id: cam, builtin: source }, { id: s, builtin: sink }";
    let data = "{ from: cam.frame, to: s.frame }";
    let with =
        |extra: &str, loops: &str| codes(&pipeline(&format!("{base}, {extra}"), data, "", loops));
    assert!(with("{ id: c, primitive: loop }", "{ from: s, to: c }").is_empty());
    assert!(
        with("{ id: c, primitive: loop }", "").is_empty(),
        "a bare loop still owns iteration"
    );
    assert_eq!(
        with("{ id: c, primitive: loop }, { id: d, primitive: loop }", ""),
        ["graph.loop.multiple"]
    );
    assert_eq!(
        with("{ id: c, primitive: loop, config: { mode: count } }", ""),
        ["graph.loop.invalid-count"]
    );
    assert_eq!(
        with(
            "{ id: c, primitive: loop, config: { mode: forever, count: 3 } }",
            ""
        ),
        ["graph.loop.invalid-count"]
    );
    assert_eq!(
        with("{ id: c, primitive: loop }", "{ from: s, to: cam }"),
        ["graph.edge.invalid-loop-target"]
    );
    assert_eq!(
        with("{ id: c, primitive: loop }", "{ from: nope, to: c }"),
        ["graph.edge.missing-source-node"]
    );
    assert_eq!(
        with("{ id: c, primitive: loop }", "{ from: c, to: c }"),
        ["graph.edge.invalid-loop-source"]
    );
}

#[test]
fn a_graph_of_only_values_has_no_source() {
    let text = pipeline(
        "{ id: v, primitive: value, config: { type: int, literal: 1 } }",
        "",
        "",
        "",
    );
    assert_eq!(codes(&text), ["graph.node.no-source"]);
}

#[test]
fn module_binding_rules() {
    let with = |node: &str| codes(&pipeline(node, "", "", ""));
    assert!(with("{ id: cam, builtin: source, config: { rate: 5 }, bindings: { rate: { type: int, binding: cam.rate } } }").is_empty());
    assert_eq!(
        with(
            "{ id: cam, builtin: source, config: { rate: fast }, bindings: { rate: { type: int, binding: cam.rate } } }"
        ),
        ["graph.binding.config-type-mismatch"]
    );
    assert_eq!(
        with(
            "{ id: cam, builtin: source, bindings: { rate: { type: int, binding: cam.rate, schema: { type: nope } } } }"
        ),
        ["graph.binding.invalid-schema"]
    );
    assert_eq!(
        with("{ id: cam, builtin: source, bindings: { rate: { type: int, binding: '' } } }"),
        ["graph.binding.invalid-key"]
    );
    assert_eq!(
        codes(&pipeline(
            "{ id: cam, builtin: source }, { id: f, primitive: fork, bindings: { x: { type: int, binding: k } } }",
            "{ from: cam.frame, to: f.frame }",
            "",
            ""
        )),
        ["graph.binding.on-primitive"]
    );
}

#[test]
fn switch_and_fork_output_names_must_be_port_names() {
    let text = pipeline(
        "{ id: cam, builtin: source }, { id: f, primitive: fork }, { id: s, builtin: sink }",
        "{ from: cam.frame, to: f.frame }, { from: f.9bad, to: s.frame }",
        "",
        "",
    );
    assert!(codes(&text).contains(&"graph.primitive.invalid-port"));
}
