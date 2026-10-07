//! Project Life — a standalone archive of observed project states.
//!
//! All logic lives here; `main.rs` only parses arguments. The storage format is described in
//! `SPEC.md` (section 7) and `STORAGE_FORMAT.md`. The invariant everything rests on:
//! **blob -> journal event -> state**, so an event never references
//! a blob that is not on disk.

pub mod archive;
pub mod brand;
pub mod cache;
pub mod cli;
pub mod daemon;
pub mod detect;
pub mod doctor;
pub mod events;
pub mod filters;
pub mod glob;
pub mod health;
pub mod lifecycle;
pub mod mcp;
pub mod ops;
pub mod profiles;
pub mod quick;
pub mod restore;
pub mod retention;
pub mod scan;
pub mod space;
pub mod store;
pub mod util;
pub mod watch;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const SCHEMA_VERSION: u64 = 1;
