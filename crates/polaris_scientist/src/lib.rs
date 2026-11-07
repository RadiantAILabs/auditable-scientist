//! Research systems and phase graphs composed from reusable flow blocks.
pub mod campaign;
pub mod claims;
pub mod config;
pub mod durable;
pub mod eval;
pub mod latex;
pub mod phases;
pub mod prompts;
pub mod records;
pub mod roles;
pub use phases::{BuildError, PhaseCtx, PhaseRegistry, RegisterError};
