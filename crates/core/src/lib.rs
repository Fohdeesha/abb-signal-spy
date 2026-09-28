//! The protocol core of ABB Signal Spy: a read-only RobAPI InfoStream client for ABB
//! IRC5 controllers, the typed sample decoder, and the pieces that sit behind the
//! window (channel store, recorder, catalogue).
//!
//! Nothing in this crate knows about a particular cell: no address, no signal set
//! and no client label is built in.

pub mod catalogue;
pub mod discovery;
pub mod log;
pub mod reading;
pub mod recording;
pub mod reply;
pub mod request;
pub mod review;
pub mod sample;
pub mod session;
pub mod store;
pub mod timeline;
pub mod util;
pub mod wire;

#[cfg(feature = "fake")]
pub mod fake;
