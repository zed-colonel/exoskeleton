//! TypeScript type declaration validation tests.                                                                                                                                                                                                                                                                                                                                           
//!                                                                                                                                                                                                                                                                                                                                                                                         
//! Verify that ts-rs derivations on exoskeleton types produce correct output.
//! The actual codegen binary is src/bin/export_types.rs.

// ── exoskeleton-core types ──
use exoskeleton_core::thread::ThreadSchedule;
use exoskeleton_core::{
    ArtifactId, EnvelopeId, LedgerEntryId, PrincipalId, ThreadId, TickId, VesselId,
};
use ts_rs::TS;

// ── E3-T14: ID newtypes generate as string aliases ──

#[test]
fn ts_id_newtypes_are_string_aliases() {
    let cfg = ts_rs::Config::default();
    assert_eq!(VesselId::decl(&cfg), "type VesselId = string;");
    assert_eq!(TickId::decl(&cfg), "type TickId = string;");
    assert_eq!(ThreadId::decl(&cfg), "type ThreadId = string;");
    assert_eq!(PrincipalId::decl(&cfg), "type PrincipalId = string;");
    assert_eq!(EnvelopeId::decl(&cfg), "type EnvelopeId = string;");
    assert_eq!(LedgerEntryId::decl(&cfg), "type LedgerEntryId = string;");
    assert_eq!(ArtifactId::decl(&cfg), "type ArtifactId = string;");
}

// ── E3-T15: ThreadSchedule enum with mixed variants ──

#[test]
fn ts_thread_schedule_mixed_variants() {
    let cfg = ts_rs::Config::default();
    let decl = ThreadSchedule::decl(&cfg);
    assert!(decl.contains("every_tick"), "ThreadSchedule decl: {decl}");
    assert!(decl.contains("on_demand"), "ThreadSchedule decl: {decl}");
    assert!(
        decl.contains("every_n_ticks"),
        "ThreadSchedule decl: {decl}"
    );
}
