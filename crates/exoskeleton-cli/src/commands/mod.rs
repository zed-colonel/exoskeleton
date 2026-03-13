//! CLI subcommand implementations.
//!
//! Each submodule corresponds to a top-level `exo` subcommand. The `start`
//! command boots a Vessel + daemon in-process; all others are HTTP clients
//! that query a running daemon.

pub mod budget;
pub mod engines;
pub mod events;
pub mod inspect;
pub mod relationship;
pub mod send;
pub mod start;
pub mod thread;
