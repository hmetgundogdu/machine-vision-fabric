//! MVF worker protocol v2 (#32): the runtime-to-node control channel.
//!
//! Transport, framing and semantics are separate:
//!
//! - **Transport**: the inherited stdio pipes of the child (no listening endpoint, so no peer to
//!   authenticate beyond the one MVF launched; works unchanged through `runc` and on Windows). A v2
//!   SDK moves its own `stdout` onto `stderr` at start, so a stray `print()` or a native library
//!   can no longer corrupt the control channel; logs travel on `stderr` only.
//! - **Framing** ([`frame`]): a 24-byte little-endian header (`MV`, version 2, kind, flags,
//!   correlation id, generation, body length) and a bounded body. A frame larger than the
//!   negotiated maximum, a bad magic or a truncated read is a protocol violation, not a guess.
//! - **Messages** ([`msg`]): fixed binary bodies for the hot path (`execute`, `result`,
//!   `checkpoint`, `restore`, `ping`), JSON bodies for the cold path (`hello`, `configure`, errors,
//!   classification and metadata values). Payload bytes never travel here: only arena offsets.
//!
//! Negotiation: the engine offers v2 in the environment (`MVF_PROTOCOL=2`, `MVF_SESSION=<token>`).
//! A v2 child answers with a binary `hello` frame carrying the token; a v1 child, which ignores the
//! offer, answers with its JSON line. The engine tells them apart by the first byte.

pub mod frame;
pub mod msg;

pub use frame::{FrameError, Header, Kind, MAGIC, MAX_FRAME, VERSION, read_frame, write_frame};
pub use msg::{Execute, ResultFlags, ResultMsg};
