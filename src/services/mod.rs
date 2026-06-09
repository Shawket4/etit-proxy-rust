//! Orchestration layer.
//!
//! Each service module is the *only* place in the codebase that knows the
//! full read path for one resource: try memory cache → try DB → fetch from
//! upstream → write back to both. Handlers must always call services, never
//! upstream or DB directly.

pub mod broadcaster;
pub mod history;
pub mod live;
pub mod vehicles;

pub use broadcaster::{Broadcaster, LiveEvent};
