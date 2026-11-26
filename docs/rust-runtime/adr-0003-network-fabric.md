# ADR-0003 — Network Fabric: per-node network policy as a runtime service

- **Status:** Accepted for the research branch (#23). Not a migration decision.
- **Date:** 2026-11-26
- **Issues:** #23, part of #10. Related: #21 (event-driven execution), #27 (Windows, rootless OCI).
- **Evidence:** [stage 16](stages/16.md), [stage 17](stages/17.md)

## Context

Industrial nodes act as both clients and servers:

- a camera uploads images to an FTP server inside a node;
- a node talks to a PLC, an OPC UA server, an RTSP or GigE camera, REST or MQTT.

Before #23 the OCI backend offered only `network: none`, and trusted (in-process and worker) nodes
silently shared the host network, so the contract's network field promised nothing for them.

## Decisions

1. **The graph stays local.** Payload and control between graph nodes use shared memory and local
   IPC, unchanged. Networking is only for external devices and services. There is no
   distributed graph and no mandatory broker or proxy.
2. **Contract, binding and enforcement are separate.**
   - The portable contract declares what a node needs: listeners (`inbound`: protocol, port or
     range, source CIDR) and destinations (`outbound`, or `deny`).
   - The deployment binding (pipeline node `network`) chooses where to publish (`hostIp`,
     `hostPort`) and what to advertise (`MVF_ADVERTISE_IP`).
   - The backend enforces it, or refuses.
3. **Honest labels, fail closed.**

   | Backend | `none` | `host` | `restricted` |
   |---|---|---|---|
   | OCI, Linux, root with iproute2 and nftables | enforced (empty namespace) | allowed; reported as not isolated (`placement.host-network`) | **enforced** |
   | OCI without the tools | enforced | allowed, reported | rejected |
   | worker / in-process | **not enforced**; logged as "shares the host network" | host network | **rejected** (`negotiation.network-not-enforceable`) |
   | Windows | n/a (no OCI) | host network | rejected (**BLOCKED**: HNS/WFP not implemented) |
   | macOS | n/a (no Linux OCI) | host network | rejected |

4. **Linux enforcement uses the kernel, not a proxy.** Each node gets:
   - its own network namespace, owned by the host's user namespace, so the node holds no
     capability over its interface and cannot change its address, routes or rules;
   - a routed veth `/30` from `10.89.0.0/16`;
   - one nftables table: DNAT of published listeners, source-restricted inbound,
     destination-restricted outbound with masquerade, host deny, anti-spoofing, stateful return
     traffic and per-rule counters.

   `ip` and `nft` are invoked as tools, and no network daemon is required.
5. **Ports are owned by pipeline nodes, not generations.**
   - A successor generation of the same node shares the host-port lease and starts unpublished.
   - At cut-over, `NodeHandle::promote` hands the listeners over in one atomic nftables batch,
     and a rollback promotes the restored generation back.
   - A port held by another node or any other process fails the start with an actionable error.
   - Leases of dead hosts are swept.
6. **Source workers are a protocol v1 extension.** Capability `source`: `execute` carries only an
   output slot, and the child answers with a frame (`cameraId`, `sequence`) or `exhausted`. This
   is what lets a network server (FTP) be a graph source. Network bytes are received by the node,
   which is not zero copy. From the output slot on, nothing is copied.

## Consequences

- An FTP camera, a PLC client and any TCP/UDP listener can be packaged as OCI nodes whose network
  access is exactly what the contract says, and verified, which the e2e tests show.
- A restricted node has an empty `/sys`: sysfs cannot be mounted in a namespace the node does not
  own.
- Live replacement of a listening node is seamless for **new** connections. Connections
  established with the previous generation keep working until it stops (at commit, or after a
  rollback). The handover adds the `nft` invocation to the cut-over pause, 2–20 ms measured. A
  netlink implementation would remove the process spawn.
- Not covered:
  - IPv6;
  - broadcast and multicast across the routed veth (camera discovery needs `host` mode);
  - host-local clients via `127.0.0.1`;
  - ports below 1024 inside the node (publish a high node port on host port 21 instead);
  - rootless networking (#27).

## Implementation phases

1. Done ([stage 16](stages/16.md)): contract, binding, negotiation, Linux enforcement, cleanup,
   sweep.
2. Done ([stage 17](stages/17.md)): source workers, the FTP camera PoC, PLC allow/deny, listener
   handover with rollback, measurements.
3. Next, design-gated:
   - IPv6 and multicast (macvlan/ipvlan, or a discovery relay);
   - netlink instead of `ip`/`nft`;
   - rootless (slirp4netns/pasta);
   - Windows HNS/WFP (#27);
   - TLS and credential handling guidance per protocol (the node owns its application protocol).

## Threat model

- **Protects against:** a node reaching networks, hosts or ports it did not declare; strangers
  reaching its listeners; the node reconfiguring or spoofing its own interface; leaked rules or
  namespaces after crashes.
- **Does not protect against:**
  - weaknesses of the application protocol itself (plain FTP sends credentials in clear text; use
    it only on an isolated, authorized network);
  - a compromised host or engine, which runs as root;
  - denial of service within the declared allowances;
  - traffic of a `host`-mode or trusted node, which is unisolated by declaration.
- **Credentials** are never part of the effective-policy line or the counters.
