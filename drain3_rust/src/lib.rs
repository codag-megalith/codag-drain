pub mod cluster;
pub mod drain;
pub mod masking;
pub mod node;
pub mod parse_event;
pub mod pipeline;
pub mod similarity;
pub mod storage;

#[cfg(test)]
mod bench;
#[cfg(test)]
mod tests;

// Convenient re-exports so callers can write `drain3_rust::Drain` etc.
pub use cluster::{LogCluster, UpdateType};
pub use drain::Drain;
pub use masking::{LogMasker, MaskingInstruction};
pub use parse_event::DrainResult;
pub use pipeline::ConcurrentDrain;
