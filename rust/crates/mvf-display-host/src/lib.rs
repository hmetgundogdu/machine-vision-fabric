//! Display host core (#30), independent of any window library so it is testable headless.
//!
//! - [`present`]: fit a frame into a window (letterbox, nearest-neighbour) and map window
//!   coordinates back to frame pixels.
//! - [`router`]: which input a node's grants admit.
//! - [`core`]: the host loop's state — discover surfaces, hold one reader per surface, poll the
//!   latest frames, heartbeat, route input, close.

pub mod core;
pub mod present;
pub mod router;

pub use crate::core::{HostCore, Routed};
