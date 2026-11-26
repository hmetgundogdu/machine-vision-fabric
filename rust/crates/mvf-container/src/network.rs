//! Enforced per-node networking for sandboxed (OCI) nodes on Linux (#23).
//!
//! `restricted` gives a node its own network namespace, reached through a routed veth pair:
//! `10.89.<n>.2/30` inside, `.1` on the host. One nftables table per node (`inet mvf_<n>`)
//! enforces the contract:
//!
//! - **inbound**: each declared listener is published on the host. The deployment binding sets
//!   the host address and port; by default it is all addresses and the same port. Traffic is
//!   DNAT-ed to the node and accepted only from the rule's `source`. Everything else addressed to
//!   the node is dropped;
//! - **outbound**: new connections only to the declared destinations, masqueraded behind the host.
//!   Everything else is dropped, and so is any connection to the host itself;
//! - **return traffic** of a permitted session (`ct state established,related`) always flows,
//!   even when new outbound connections are denied.
//!
//! Normal network traffic never enters the graph's data plane, and there is no user-space proxy:
//! the kernel routes and filters. Prerequisites: root, `ip` (iproute2) and `nft` (nftables), and
//! IPv4 forwarding, which is enabled if off. IPv4 only, and IPv6 rules are refused. Broadcast and
//! multicast do not cross the routed veth (camera discovery needs `host` mode). Subnets and host
//! ports are leased under `<state>/net/`; a lease whose process is gone is swept, with its
//! namespace, interface and table, before the next allocation.

use std::fmt::Write as _;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Command;

use mvf_contract::{Cidr, NetworkBinding, NetworkPermission, Protocol, parse_port_range};

/// Base of the node subnet pool (`10.89.0.0/16`, 16384 nodes of /30).
const POOL: [u8; 2] = [10, 89];
const POOL_SIZE: u32 = 16384;

/// Host tools for enforced networking.
#[derive(Debug, Clone)]
pub struct NetworkSupport {
    ip: PathBuf,
    nft: PathBuf,
}

impl NetworkSupport {
    /// Root plus `ip` and `nft` on `PATH` (Linux).
    pub fn detect() -> Option<Self> {
        if !cfg!(target_os = "linux") || !crate::running_as_root() {
            return None;
        }
        Some(Self {
            ip: crate::first_on_path(&["ip"])?,
            nft: crate::first_on_path(&["nft"])?,
        })
    }

    fn run(&self, program: &Path, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
        use std::io::Write;
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(if stdin.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("{}: {e}", program.display()))?;
        if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(input.as_bytes())
                .map_err(|e| format!("{}: {e}", program.display()))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("{}: {e}", program.display()))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(format!(
                "`{} {}` failed: {}",
                program.display(),
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    fn ip(&self, args: &[&str]) -> Result<String, String> {
        self.run(&self.ip, args, None)
    }

    fn nft(&self, args: &[&str], script: Option<&str>) -> Result<String, String> {
        self.run(&self.nft, args, script)
    }
}

/// A listener as published on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    /// Protocol.
    pub protocol: Protocol,
    /// Host address (`None` = all).
    pub host_ip: Option<Ipv4Addr>,
    /// Host port range.
    pub host: (u16, u16),
    /// Node port range (same length).
    pub node: (u16, u16),
    /// Allowed sources.
    pub source: Option<Cidr>,
}

/// One allowed outbound destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    /// Protocol.
    pub protocol: Protocol,
    /// Destination network.
    pub destination: Cidr,
    /// Port range (`None` = any).
    pub ports: Option<(u16, u16)>,
}

/// The effective, enforceable policy of one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// Published listeners.
    pub inbound: Vec<Published>,
    /// Allowed outbound destinations.
    pub outbound: Vec<Destination>,
}

fn v4(cidr: &Cidr, what: &str) -> Result<(), String> {
    if cidr.is_ipv4() {
        Ok(())
    } else {
        Err(format!(
            "{what} {cidr}: IPv6 rules are not supported by this backend (IPv4 only)"
        ))
    }
}

impl Policy {
    /// Resolves a contract's rules and a deployment binding into what will be enforced.
    pub fn resolve(
        permission: &NetworkPermission,
        binding: Option<&NetworkBinding>,
    ) -> Result<Self, String> {
        let host_ip = match binding.and_then(|b| b.host_ip.as_deref()) {
            None | Some("0.0.0.0") => None,
            Some(ip) => Some(
                ip.parse::<Ipv4Addr>()
                    .map_err(|_| format!("hostIp `{ip}`: IPv4 only"))?,
            ),
        };
        let mut inbound = Vec::new();
        for rule in permission.inbound() {
            let node = match (rule.port, rule.port_range.as_deref()) {
                (Some(p), None) => (p, p),
                (None, Some(r)) => parse_port_range(r)?,
                _ => return Err("an inbound rule needs `port` or `portRange`".into()),
            };
            let source = rule.source.as_deref().map(Cidr::parse).transpose()?;
            if let Some(s) = &source {
                v4(s, "source")?;
            }
            // A single port may be remapped by the binding; ranges are published as-is.
            let host = match binding.and_then(|b| {
                b.ports
                    .iter()
                    .find(|p| p.protocol == rule.protocol && p.port == node.0 && node.0 == node.1)
            }) {
                Some(p) => (p.host_port, p.host_port),
                None => node,
            };
            inbound.push(Published {
                protocol: rule.protocol,
                host_ip,
                host,
                node,
                source,
            });
        }
        let mut outbound = Vec::new();
        for rule in permission.outbound() {
            let destination = Cidr::parse(&rule.destination)?;
            v4(&destination, "destination")?;
            let ports = match (rule.port, rule.port_range.as_deref()) {
                (Some(p), None) => Some((p, p)),
                (None, Some(r)) => Some(parse_port_range(r)?),
                _ => None,
            };
            outbound.push(Destination {
                protocol: rule.protocol,
                destination,
                ports,
            });
        }
        Ok(Self { inbound, outbound })
    }

    /// Every host port this policy publishes.
    pub fn host_ports(&self) -> Vec<(Protocol, u16)> {
        self.inbound
            .iter()
            .flat_map(|p| (p.host.0..=p.host.1).map(move |port| (p.protocol, port)))
            .collect()
    }

    /// One-line description of the effective policy (logs, reports).
    pub fn describe(&self, node_ip: Ipv4Addr) -> String {
        let inbound: Vec<String> = self
            .inbound
            .iter()
            .map(|p| {
                format!(
                    "{} {}:{} -> {} from {}",
                    p.protocol.as_str(),
                    p.host_ip.map_or("0.0.0.0".into(), |i| i.to_string()),
                    span(p.host),
                    span(p.node),
                    p.source.map_or("any".into(), |s| s.to_string())
                )
            })
            .collect();
        let outbound: Vec<String> = self
            .outbound
            .iter()
            .map(|d| {
                format!(
                    "{} {}{}",
                    d.protocol.as_str(),
                    d.destination,
                    d.ports.map(|p| format!(":{}", span(p))).unwrap_or_default()
                )
            })
            .collect();
        format!(
            "network: restricted ip={node_ip} inbound=[{}] outbound=[{}] (default deny; return traffic allowed)",
            inbound.join(", "),
            if outbound.is_empty() {
                "deny".into()
            } else {
                outbound.join(", ")
            }
        )
    }
}

fn span((a, b): (u16, u16)) -> String {
    if a == b {
        a.to_string()
    } else {
        format!("{a}-{b}")
    }
}

/// The DNAT rules that publish a node's listeners (rule bodies, without a chain).
pub fn publish_rules(host_if: &str, node_ip: Ipv4Addr, policy: &Policy) -> Vec<String> {
    policy
        .inbound
        .iter()
        .map(|p| {
            let daddr = p
                .host_ip
                .map(|i| format!("ip daddr {i} "))
                .unwrap_or_default();
            let saddr = p
                .source
                .map(|c| format!("ip saddr {c} "))
                .unwrap_or_default();
            let to = if p.host == p.node {
                format!("{node_ip}")
            } else {
                format!("{node_ip}:{}", span(p.node))
            };
            format!(
                "iifname != \"{host_if}\" {daddr}{saddr}{} dport {} counter dnat ip to {to} comment \"publish\"",
                p.protocol.as_str(),
                span(p.host)
            )
        })
        .collect()
}

/// The nftables ruleset of one node (pure; unit-tested on every host). With `publish: false` (a
/// successor generation during a live replacement) the listeners are not published yet.
pub fn render_rules(
    table: &str,
    host_if: &str,
    node_ip: Ipv4Addr,
    policy: &Policy,
    publish: bool,
) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "table inet {table} {{");
    // Published listeners: DNAT from the host address to the node.
    let _ = writeln!(
        s,
        "  chain prerouting {{ type nat hook prerouting priority dstnat; policy accept;"
    );
    if publish {
        for rule in publish_rules(host_if, node_ip, policy) {
            let _ = writeln!(s, "    {rule}");
        }
    }
    let _ = writeln!(s, "  }}");
    // Permitted outbound leaves behind the host's address.
    let _ = writeln!(
        s,
        "  chain postrouting {{ type nat hook postrouting priority srcnat; policy accept;"
    );
    let _ = writeln!(
        s,
        "    ip saddr {node_ip} oifname != \"{host_if}\" masquerade"
    );
    let _ = writeln!(s, "  }}");
    // Forwarded traffic to/from the node: stateful, default deny.
    let _ = writeln!(
        s,
        "  chain forward {{ type filter hook forward priority filter; policy accept;"
    );
    // Anti-spoofing: from the node's interface only the node's own address.
    let _ = writeln!(
        s,
        "    iifname \"{host_if}\" ip saddr != {node_ip} counter drop comment \"spoof\""
    );
    let _ = writeln!(
        s,
        "    ip daddr {node_ip} ct state established,related counter accept comment \"in-return\""
    );
    let _ = writeln!(
        s,
        "    ip saddr {node_ip} ct state established,related counter accept comment \"out-return\""
    );
    for p in &policy.inbound {
        let saddr = p
            .source
            .map(|c| format!("ip saddr {c} "))
            .unwrap_or_default();
        let _ = writeln!(
            s,
            "    ip daddr {node_ip} {saddr}{} dport {} ct state new counter accept comment \"in-accept\"",
            p.protocol.as_str(),
            span(p.node)
        );
    }
    let _ = writeln!(s, "    ip daddr {node_ip} counter drop comment \"in-deny\"");
    for d in &policy.outbound {
        let ports = d
            .ports
            .map(|p| format!("{} dport {} ", d.protocol.as_str(), span(p)))
            .unwrap_or_else(|| format!("meta l4proto {} ", d.protocol.as_str()));
        let _ = writeln!(
            s,
            "    ip saddr {node_ip} ip daddr {} {ports}ct state new counter accept comment \"out-accept\"",
            d.destination
        );
    }
    let _ = writeln!(
        s,
        "    ip saddr {node_ip} counter drop comment \"out-deny\""
    );
    let _ = writeln!(s, "  }}");
    // The node never reaches services of the host itself (only return traffic).
    let _ = writeln!(
        s,
        "  chain input {{ type filter hook input priority filter; policy accept;"
    );
    let _ = writeln!(
        s,
        "    iifname \"{host_if}\" ct state established,related accept"
    );
    let _ = writeln!(
        s,
        "    iifname \"{host_if}\" counter drop comment \"host-deny\""
    );
    let _ = writeln!(s, "  }}");
    let _ = writeln!(s, "}}");
    s
}

/// The /30 of pool slot `n`: (host side, node side).
pub fn subnet(n: u32) -> (Ipv4Addr, Ipv4Addr) {
    let base = n * 4;
    let a = Ipv4Addr::new(POOL[0], POOL[1], (base >> 8) as u8, (base & 0xff) as u8);
    let o = u32::from(a);
    (Ipv4Addr::from(o + 1), Ipv4Addr::from(o + 2))
}

fn alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

/// A live, enforced network of one node. Dropping it tears everything down.
#[derive(Debug)]
pub struct NodeNetwork {
    support: NetworkSupport,
    slot: u32,
    netns: String,
    host_if: String,
    table: String,
    /// The node's address.
    pub node_ip: Ipv4Addr,
    /// The effective policy.
    pub policy: Policy,
    leases: Vec<PathBuf>,
    /// Host-port leases (shared by the generations of one node; only the publisher releases them).
    ports: Vec<PathBuf>,
    /// `<pid> <node>`: generations of the same node may share host ports.
    owner: String,
    torn_down: bool,
}

/// A host-port lease: `<pid> <node> <publishing slot>`.
fn read_lease(path: &Path) -> Option<(String, u32)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut parts = text.split_whitespace();
    let pid = parts.next()?;
    let node = parts.next()?;
    let slot = parts.next()?.parse().ok()?;
    Some((format!("{pid} {node}"), slot))
}

impl NodeNetwork {
    /// The namespace path handed to the runtime.
    pub fn netns_path(&self) -> PathBuf {
        PathBuf::from("/run/netns").join(&self.netns)
    }

    /// Effective policy, one line.
    pub fn describe(&self) -> String {
        self.policy.describe(self.node_ip)
    }

    /// Creates namespace, veth, addresses, routes and rules for one generation of pipeline node
    /// `node`. A host port already published by *another* generation of the same node is shared:
    /// this generation starts unpublished until [`NodeNetwork::promote`]. Fails closed: on any error
    /// everything created so far is removed.
    pub fn setup(
        support: &NetworkSupport,
        state_dir: &Path,
        node: &str,
        policy: Policy,
    ) -> Result<Self, String> {
        let node = node.split_whitespace().next().unwrap_or("node");
        let dir = state_dir.join("net");
        std::fs::create_dir_all(dir.join("leases"))
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        std::fs::create_dir_all(dir.join("ports"))
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        sweep(support, &dir);
        let mut net = Self {
            support: support.clone(),
            slot: 0,
            netns: String::new(),
            host_if: String::new(),
            table: String::new(),
            node_ip: Ipv4Addr::UNSPECIFIED,
            policy,
            leases: Vec::new(),
            ports: Vec::new(),
            owner: format!("{} {node}", std::process::id()),
            torn_down: false,
        };
        let stamp = net.owner.clone();
        let mut publish = true;
        // Host ports first: a collision is the most likely failure and costs nothing to detect.
        for (protocol, port) in net.policy.host_ports() {
            let lease = dir
                .join("ports")
                .join(format!("{}-{port}", protocol.as_str()));
            if std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lease)
                .is_err()
            {
                match read_lease(&lease) {
                    // Another generation of this node publishes it: share; publish on promotion.
                    Some((owner, _)) if owner == net.owner => {
                        publish = false;
                        net.ports.push(lease);
                        continue;
                    }
                    _ => {
                        let owner = std::fs::read_to_string(&lease).unwrap_or_default();
                        return Err(format!(
                            "host {} port {port} is already published by another MVF node ({})",
                            protocol.as_str(),
                            owner.trim()
                        ));
                    }
                }
            }
            let _ = std::fs::write(&lease, format!("{stamp} {}", u32::MAX));
            net.ports.push(lease);
            let host = net
                .policy
                .inbound
                .iter()
                .find(|p| p.protocol == protocol && (p.host.0..=p.host.1).contains(&port))
                .and_then(|p| p.host_ip)
                .unwrap_or(Ipv4Addr::UNSPECIFIED);
            let free = match protocol {
                Protocol::Tcp => std::net::TcpListener::bind((host, port)).is_ok(),
                Protocol::Udp => std::net::UdpSocket::bind((host, port)).is_ok(),
            };
            if !free {
                return Err(format!(
                    "host {} port {port} is in use by another process",
                    protocol.as_str()
                ));
            }
        }
        // A subnet slot.
        let slot = (0..POOL_SIZE)
            .find(|n| {
                let lease = dir.join("leases").join(n.to_string());
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&lease)
                    .and_then(|_| std::fs::write(&lease, &stamp))
                    .is_ok()
            })
            .ok_or("the node subnet pool (10.89.0.0/16) is exhausted")?;
        net.leases.push(dir.join("leases").join(slot.to_string()));
        net.slot = slot;
        if publish {
            for lease in &net.ports {
                let _ = std::fs::write(lease, format!("{stamp} {slot}"));
            }
        }
        net.netns = format!("mvf-{slot}");
        net.host_if = format!("mvfh{slot:x}");
        net.table = format!("mvf_{slot}");
        let (host_side, node_side) = subnet(slot);
        net.node_ip = node_side;
        let peer = format!("mvfc{slot:x}");
        let host_cidr = format!("{host_side}/30");
        let node_cidr = format!("{node_side}/30");
        let gateway = host_side.to_string();
        let ns = net.netns.clone();
        let host_if = net.host_if.clone();
        let steps: Vec<Vec<&str>> = vec![
            vec!["netns", "add", &ns],
            vec![
                "link", "add", &host_if, "type", "veth", "peer", "name", &peer,
            ],
            vec!["link", "set", &peer, "netns", &ns],
            vec!["-n", &ns, "link", "set", &peer, "name", "eth0"],
            vec!["-n", &ns, "addr", "add", &node_cidr, "dev", "eth0"],
            vec!["-n", &ns, "link", "set", "lo", "up"],
            vec!["-n", &ns, "link", "set", "eth0", "up"],
            vec!["-n", &ns, "route", "add", "default", "via", &gateway],
            vec!["addr", "add", &host_cidr, "dev", &host_if],
            vec!["link", "set", &host_if, "up"],
        ];
        for step in steps {
            support.ip(&step)?;
        }
        if std::fs::read_to_string("/proc/sys/net/ipv4/ip_forward").is_ok_and(|v| v.trim() != "1") {
            std::fs::write("/proc/sys/net/ipv4/ip_forward", "1")
                .map_err(|e| format!("enable IPv4 forwarding: {e}"))?;
        }
        let rules = render_rules(&net.table, &net.host_if, net.node_ip, &net.policy, publish);
        support.nft(&["-f", "-"], Some(&rules))?;
        Ok(net)
    }

    /// Whether this generation currently publishes its node's listeners.
    pub fn publishes(&self) -> bool {
        self.ports
            .iter()
            .all(|lease| read_lease(lease).is_some_and(|(_, slot)| slot == self.slot))
    }

    /// Makes this generation the publisher of its node's listeners: in one atomic nftables batch the
    /// previous publisher's DNAT rules are flushed and this generation's are added. New connections
    /// reach this generation from then on; connections already established with the previous one
    /// keep their translation until that generation stops. Idempotent.
    pub fn promote(&mut self) -> Result<(), String> {
        if self.ports.is_empty() || self.publishes() {
            return Ok(());
        }
        let mut previous: Vec<u32> = self
            .ports
            .iter()
            .filter_map(|lease| read_lease(lease).map(|(_, slot)| slot))
            .filter(|slot| *slot != self.slot && *slot != u32::MAX)
            .collect();
        previous.sort_unstable();
        previous.dedup();
        let mut batch = String::new();
        for slot in previous {
            let _ = writeln!(batch, "flush chain inet mvf_{slot} prerouting");
        }
        for rule in publish_rules(&self.host_if, self.node_ip, &self.policy) {
            let _ = writeln!(batch, "add rule inet {} prerouting {rule}", self.table);
        }
        self.support.nft(&["-f", "-"], Some(&batch))?;
        for lease in &self.ports {
            let _ = std::fs::write(lease, format!("{} {}", self.owner, self.slot));
        }
        Ok(())
    }

    /// Packet counters per rule kind (`in-accept`, `in-deny`, `out-accept`, `out-deny`, …).
    pub fn counters(&self) -> Vec<(String, u64, u64)> {
        let Ok(json) = self
            .support
            .nft(&["-j", "list", "table", "inet", &self.table], None)
        else {
            return Vec::new();
        };
        let Ok(doc) = serde_json::from_str::<serde_json::Value>(&json) else {
            return Vec::new();
        };
        let mut totals: Vec<(String, u64, u64)> = Vec::new();
        for item in doc["nftables"].as_array().into_iter().flatten() {
            let rule = &item["rule"];
            let (Some(comment), Some(exprs)) = (rule["comment"].as_str(), rule["expr"].as_array())
            else {
                continue;
            };
            for e in exprs {
                if let Some(c) = e.get("counter") {
                    let packets = c["packets"].as_u64().unwrap_or(0);
                    let bytes = c["bytes"].as_u64().unwrap_or(0);
                    match totals.iter_mut().find(|(k, _, _)| k == comment) {
                        Some(t) => {
                            t.1 += packets;
                            t.2 += bytes;
                        }
                        None => totals.push((comment.to_string(), packets, bytes)),
                    }
                }
            }
        }
        totals
    }

    /// Removes rules, interfaces, the namespace and the leases. Idempotent.
    pub fn teardown(&mut self) {
        if self.torn_down {
            return;
        }
        self.torn_down = true;
        if !self.table.is_empty() {
            teardown_slot(&self.support, self.slot);
        }
        for lease in &self.leases {
            let _ = std::fs::remove_file(lease);
        }
        // A host port is released by the generation that publishes it; a superseded generation
        // leaves it to its successor.
        for lease in &self.ports {
            if read_lease(lease).is_some_and(|(owner, slot)| {
                owner == self.owner && (slot == self.slot || slot == u32::MAX)
            }) {
                let _ = std::fs::remove_file(lease);
            }
        }
    }
}

impl Drop for NodeNetwork {
    fn drop(&mut self) {
        self.teardown();
    }
}

fn teardown_slot(support: &NetworkSupport, slot: u32) {
    let _ = support.nft(&["delete", "table", "inet", &format!("mvf_{slot}")], None);
    let _ = support.ip(&["link", "del", &format!("mvfh{slot:x}")]);
    let _ = support.ip(&["netns", "del", &format!("mvf-{slot}")]);
}

/// Cleans up after nodes whose process is gone (a killed host never tore its networks down).
fn sweep(support: &NetworkSupport, dir: &Path) {
    let stale = |path: &Path| {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| {
                t.split_whitespace()
                    .next()
                    .and_then(|p| p.parse::<u32>().ok())
            })
            .is_some_and(|pid| !alive(pid))
    };
    for entry in std::fs::read_dir(dir.join("leases"))
        .into_iter()
        .flatten()
        .flatten()
    {
        if stale(&entry.path())
            && let Some(slot) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
        {
            teardown_slot(support, slot);
            let _ = std::fs::remove_file(entry.path());
        }
    }
    for entry in std::fs::read_dir(dir.join("ports"))
        .into_iter()
        .flatten()
        .flatten()
    {
        if stale(&entry.path()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
