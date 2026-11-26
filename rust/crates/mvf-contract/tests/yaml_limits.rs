//! The strict YAML loader: bounded inputs and core-schema scalar resolution.

use mvf_contract::yaml::{YamlLimits, parse_value};
use serde_json::json;

#[test]
fn rejects_input_over_the_size_limit() {
    let limits = YamlLimits {
        max_bytes: 64,
        ..YamlLimits::default()
    };
    let text = format!("key: {}", "x".repeat(100));
    assert_eq!(
        parse_value(&text, &limits).unwrap_err().code,
        "yaml.too-large"
    );
}

#[test]
fn rejects_excessive_nesting() {
    let mut text = String::new();
    for i in 0..40 {
        text.push_str(&" ".repeat(i * 2));
        text.push_str("a:\n");
    }
    assert_eq!(
        parse_value(&text, &YamlLimits::default()).unwrap_err().code,
        "yaml.too-deep"
    );
}

#[test]
fn rejects_flow_nesting_bomb() {
    let text = format!("{}{}", "[".repeat(200), "]".repeat(200));
    assert_eq!(
        parse_value(&text, &YamlLimits::default()).unwrap_err().code,
        "yaml.too-deep"
    );
}

#[test]
fn rejects_too_many_nodes() {
    let limits = YamlLimits {
        max_events: 50,
        ..YamlLimits::default()
    };
    let text = format!("[{}]", vec!["1"; 100].join(","));
    assert_eq!(
        parse_value(&text, &limits).unwrap_err().code,
        "yaml.too-many-nodes"
    );
}

#[test]
fn rejects_long_scalars() {
    let limits = YamlLimits {
        max_scalar_bytes: 16,
        ..YamlLimits::default()
    };
    assert_eq!(
        parse_value(&format!("k: {}", "y".repeat(32)), &limits)
            .unwrap_err()
            .code,
        "yaml.scalar-too-long"
    );
}

#[test]
fn rejects_complex_keys() {
    assert_eq!(
        parse_value("? [a, b]\n: c\n", &YamlLimits::default())
            .unwrap_err()
            .code,
        "yaml.complex-key"
    );
}

#[test]
fn billion_laughs_is_rejected_at_the_first_anchor() {
    let text = "a: &a [x, x]\nb: &b [*a, *a]\nc: [*b, *b]\n";
    assert_eq!(
        parse_value(text, &YamlLimits::default()).unwrap_err().code,
        "yaml.anchor-forbidden"
    );
}

#[test]
fn resolves_core_schema_scalars_conservatively() {
    let text = r#"
n1: null
n2: ~
n3:
b1: true
b2: False
i: -42
big: 18446744073709551615
f: 1.5e3
q: "123"
yes11: yes
octal: 0o17
hex: 0x1f
inf: .inf
nan: .nan
ver: 1.0.0
"#;
    let v = parse_value(text, &YamlLimits::default()).expect("parse");
    assert_eq!(
        v,
        json!({
            "n1": null, "n2": null, "n3": null,
            "b1": true, "b2": false,
            "i": -42, "big": 18446744073709551615u64, "f": 1500.0,
            "q": "123", "yes11": "yes", "octal": "0o17", "hex": "0x1f",
            "inf": ".inf", "nan": ".nan", "ver": "1.0.0"
        })
    );
}

#[test]
fn error_locations_are_one_based() {
    let err = parse_value("a: 1\na: 2\n", &YamlLimits::default()).unwrap_err();
    assert_eq!(err.code, "yaml.duplicate-key");
    let (line, col) = err.location.expect("location");
    assert_eq!(line, 2);
    assert!(col >= 1);
}
