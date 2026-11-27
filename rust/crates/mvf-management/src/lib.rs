//! MVF runtime management, part 1 (#33): live observation of a running engine on an
//! authenticated local endpoint.
//!
//! - [`Feed`] is one ordered, sequenced stream of node deltas, engine events, logs and status, in
//!   a bounded ring, with the current state for snapshots. [`Tap`] fills it from the engine thread
//!   and never blocks it.
//! - [`Server`] serves the feed on a local endpoint that only the runtime's own OS user can reach
//!   (see [`sys`] for how each platform enforces that). [`Client`] is the other end.
//! - The wire protocol is newline-delimited JSON, API version 1: `protocol/management-v1.md`.
//!
//! Part 2 adds [`command`]: authorized, idempotent, deadline-bounded and audited commands that go
//! through the engine's own validation. Remote access is a separate relay process (`mvf-remote`),
//! never a network listener in the runtime (ADR-0007).

pub mod client;
pub mod command;
pub mod endpoint;
pub mod feed;
pub mod model;
pub mod server;
pub mod sys;

pub use client::{Client, Subscription};
pub use command::{Audit, Commands, Control, Role, Session};
pub use endpoint::{Instance, list, runtime_dir};
pub use feed::{Feed, FeedConfig, Read, RuntimeInfo, Tap, Tee};
pub use model::{API, Histogram, Topology};
pub use server::{Server, ServerOptions};
pub use sys::{Conn, connect as connect_raw};
