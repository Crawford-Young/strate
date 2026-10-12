//! Core model for strate: discovery of Claude Code projects, sessions,
//! subagents and teams from a config dir, an incremental tail of their
//! transcripts, the live registry poll and optional hook receiver, the
//! SQLite store, and cost and time accounting.

pub mod cost;
pub mod discovery;
pub mod github;
pub mod hooks;
pub mod names;
mod proc;
pub mod registry;
pub mod store;
pub mod tail;
pub mod time;
