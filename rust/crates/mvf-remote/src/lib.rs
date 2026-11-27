//! MVF remote management (#33 part 2): a separate, optional relay process that serves a runtime's
//! local management API to remote operators over mutually authenticated TLS 1.3.
//!
//! The runtime itself never listens on the network, and its process contains no TLS stack
//! (ADR-0007). The relay is an ordinary client of the local endpoint: it speaks `onBehalfOf` the
//! principal its access list admitted, so the runtime enforces the role again and audits the real
//! name. A relay that crashes, hangs or is compromised reaches the runtime only through the local
//! API's own checks.
//!
//! - [`pki`]: a small private CA (CA, relay certificate, per-operator client certificates).
//! - [`access`]: who may connect, with which role, and revocation, by certificate fingerprint.
//! - [`relay`]: the server. [`client`]: the remote client.

pub mod access;
pub mod client;
pub mod pki;
pub mod relay;
pub mod sign;
pub mod tls;

pub use access::{AccessList, ClientEntry, Refusal};
pub use client::{RemoteClient, RemoteSubscription};
pub use relay::{Relay, RelayConfig};
