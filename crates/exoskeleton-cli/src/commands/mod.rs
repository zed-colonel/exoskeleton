//! CLI subcommand implementations.
//!
//! Each submodule corresponds to a top-level `exo` subcommand. The `start`
//! command boots a Vessel + daemon in-process; all others are HTTP clients
//! that query a running daemon.

pub mod artifact;
pub mod budget;
pub mod config;
pub mod engines;
pub mod events;
pub mod fork;
pub mod inbox_history;
pub mod inspect;
pub mod memory;
pub mod relationship;
pub mod reload_charters;
pub mod send;
pub mod snapshot;
pub mod start;
pub mod thread;
