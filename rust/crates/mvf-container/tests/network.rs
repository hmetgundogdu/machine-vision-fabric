//! Restricted-network policy resolution and the generated nftables ruleset. Portable (no root).

use std::net::Ipv4Addr;

use mvf_container::network::{Policy, render_rules, subnet};
use mvf_contract::{NetworkBinding, NetworkPermission, PortBinding, Protocol};

fn permission(yaml: &str) -> NetworkPermission {
    serde_json::from_value(
        serde_json::to_value(
            mvf_contract::yaml::from_str::<serde_json::Value>(yaml, &Default::default())
                .expect("yaml"),
        )
        .expect("json"),
    )
    .expect("permission")
}

const FTP: &str = "mode: restricted\ninbound:\n  - { protocol: tcp, port: 2121, source: 192.168.77.10/32 }\n  - { protocol: tcp, portRange: 50000-50009, source: 192.168.77.10/32 }\noutbound: deny\n";

#[test]
fn listeners_are_published_on_their_own_ports_by_default_and_remapped_by_the_binding() {
    let p = Policy::resolve(&permission(FTP), None).expect("policy");
    assert_eq!(p.inbound[0].host, (2121, 2121));
    assert_eq!(p.inbound[1].host, (50000, 50009));
    assert!(p.outbound.is_empty(), "outbound: deny");
    assert_eq!(p.host_ports().len(), 11);
    let binding = NetworkBinding {
        host_ip: Some("192.168.77.1".into()),
        ports: vec![PortBinding {
            protocol: Protocol::Tcp,
            port: 2121,
            host_port: 21021,
        }],
        advertise: Some("192.168.77.1".into()),
    };
    let p = Policy::resolve(&permission(FTP), Some(&binding)).expect("policy");
    assert_eq!(
        (p.inbound[0].host, p.inbound[0].node),
        ((21021, 21021), (2121, 2121))
    );
    assert_eq!(p.inbound[0].host_ip, Some(Ipv4Addr::new(192, 168, 77, 1)));
}

#[test]
fn the_ruleset_is_default_deny_with_stateful_return_traffic() {
    let p = Policy::resolve(
        &permission("mode: restricted\ninbound: [ { protocol: tcp, port: 2121, source: 192.168.77.10/32 } ]\noutbound: [ { protocol: tcp, destination: 192.168.77.10/32, port: 5020 } ]\n"),
        None,
    )
    .expect("policy");
    let rules = render_rules("mvf_7", "mvfh7", Ipv4Addr::new(10, 89, 0, 30), &p, true);
    let standby = render_rules("mvf_7", "mvfh7", Ipv4Addr::new(10, 89, 0, 30), &p, false);
    assert!(
        !standby.contains("dnat"),
        "an unpromoted successor publishes nothing"
    );
    assert!(standby.contains("in-accept"), "but its filter is in place");
    let pos = |needle: &str| {
        rules
            .find(needle)
            .unwrap_or_else(|| panic!("missing `{needle}` in\n{rules}"))
    };
    // Published listener, restricted to its source.
    pos("ip saddr 192.168.77.10/32 tcp dport 2121 counter dnat ip to 10.89.0.30");
    // Return traffic comes before the drops; accepts before the matching drop.
    assert!(pos("ip daddr 10.89.0.30 ct state established,related") < pos("in-deny"));
    assert!(pos("in-accept") < pos("in-deny"));
    assert!(pos("ip saddr 10.89.0.30 ct state established,related") < pos("out-deny"));
    assert!(
        pos("ip daddr 192.168.77.10/32 tcp dport 5020 ct state new counter accept")
            < pos("out-deny")
    );
    // The node can never open connections to the host itself.
    pos("iifname \"mvfh7\" counter drop");
    pos("ip saddr 10.89.0.30 oifname != \"mvfh7\" masquerade");
}

#[test]
fn ipv6_rules_and_bad_ranges_fail_closed() {
    let v6 = permission(
        "mode: restricted\ninbound: [ { protocol: tcp, port: 80, source: fd00::/8 } ]\n",
    );
    assert!(Policy::resolve(&v6, None).unwrap_err().contains("IPv6"));
    let out6 =
        permission("mode: restricted\noutbound: [ { protocol: udp, destination: 'fd00::1' } ]\n");
    assert!(Policy::resolve(&out6, None).unwrap_err().contains("IPv6"));
}

#[test]
fn subnets_are_disjoint_slash_30s_in_the_pool() {
    assert_eq!(
        subnet(0),
        (Ipv4Addr::new(10, 89, 0, 1), Ipv4Addr::new(10, 89, 0, 2))
    );
    assert_eq!(
        subnet(1),
        (Ipv4Addr::new(10, 89, 0, 5), Ipv4Addr::new(10, 89, 0, 6))
    );
    assert_eq!(
        subnet(64),
        (Ipv4Addr::new(10, 89, 1, 1), Ipv4Addr::new(10, 89, 1, 2))
    );
}

#[test]
fn the_effective_policy_is_described_without_secrets() {
    let p = Policy::resolve(&permission(FTP), None).expect("policy");
    let line = p.describe(Ipv4Addr::new(10, 89, 0, 2));
    assert!(
        line.contains("tcp 0.0.0.0:2121 -> 2121 from 192.168.77.10/32"),
        "{line}"
    );
    assert!(line.contains("outbound=[deny]"), "{line}");
}
