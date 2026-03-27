#![forbid(unsafe_code)]
//! Relationship Ledger + Snapshot + Align step logic for Exoskeleton.
//!
//! This crate provides the relationship substrate for I8 (relationship awareness
//! is durable and explicit):
//!
//! - **Ledger** (`ledger`): `RelationshipLedger` trait + `InMemoryRelationshipLedger`
//! - **Snapshot compiler** (`snapshot`): `compile_relationship_snapshot()` — trust computation
//! - **Signal processing** (`signals`): `process_relational_signals()` — envelope → ledger
//! - **Alignment checker** (`align`): `check_alignment()`, `AlignConfig` — action filtering

pub mod align;
pub mod ledger;
pub mod signals;
pub mod snapshot;

pub use align::{check_alignment, AlignAction, AlignActionResult, AlignConfig};
pub use ledger::{InMemoryRelationshipLedger, RelationshipLedger};
pub use signals::process_relational_signals;
pub use snapshot::compile_relationship_snapshot;
