//! Core model for strate: discovery of Claude Code projects, sessions,
//! subagents and teams from a config dir, an incremental tail of their
//! transcripts, the live registry poll and optional hook receiver, the
//! SQLite store, cost and time accounting, and the ingest [`engine`] that
//! runs them all for a host.

pub mod charge;
pub mod cost;
pub mod discovery;
pub mod engine;
pub mod github;
pub mod hooks;
mod proc;
pub mod registry;
pub mod store;
pub mod tail;
pub mod time;
