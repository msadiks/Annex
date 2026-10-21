//! Platform-independent Annex logic: the cable rules (resolution, identity),
//! Sharp/Fast mode assignment, adaptive bitrate, clock sync and stats helpers,
//! config files, and CPU reference implementations of the colour packing.

pub mod abr;
pub mod config;
pub mod identity;
pub mod modes;
pub mod pack;
pub mod resolution;
pub mod stats;

pub use modes::{EncoderBudget, MonitorDemand, assign_modes};
pub use resolution::{sharp_eligible, stream_size};
