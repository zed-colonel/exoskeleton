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

// ── EO1-T38: Export completeness check ──

/// Verify that every type with `#[derive(TS)]` in the workspace has a
/// corresponding reference in the export-types binary.
#[test]
fn export_types_binary_is_complete() {
    use std::process::Command;

    let crates_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/");

    // Find all types with #[derive(...TS...)] in crates/
    let grep = Command::new("grep")
        .args(["-r", "-A3", r"#\[derive(.*\bTS\b", crates_dir])
        .output()
        .expect("grep failed");
    let grep_output = String::from_utf8_lossy(&grep.stdout);

    // Extract type names from "pub struct Foo" / "pub enum Bar" lines.
    // Lines include grep filename prefixes with ':' or '-' separators, so
    // we search for the keywords anywhere in the line rather than parsing prefixes.
    let mut derived_types: Vec<String> = Vec::new();
    for line in grep_output.lines() {
        let extract = |keyword: &str| -> Option<String> {
            let pos = line.find(keyword)?;
            let rest = &line[pos + keyword.len()..];
            let name = rest.split_whitespace().next()?;
            let name = name.split(|c| c == '{' || c == '<' || c == '(').next()?;
            if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            }
        };
        if let Some(name) = extract("pub struct ").or_else(|| extract("pub enum ")) {
            derived_types.push(name);
        }
    }

    derived_types.sort();
    derived_types.dedup();
    assert!(
        !derived_types.is_empty(),
        "No #[derive(TS)] types found — grep broken?"
    );

    // Read the export-types binary source
    let export_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/bin/export_types.rs"
    ))
    .expect("could not read export_types.rs");

    // Check each derived type appears in the binary source
    let mut missing: Vec<&str> = Vec::new();
    for ty in &derived_types {
        if !export_src.contains(ty.as_str()) {
            missing.push(ty);
        }
    }

    assert!(
        missing.is_empty(),
        "Types with #[derive(TS)] missing from export-types binary:\n  {}\n\
         Add emit!() calls for these types in src/bin/export_types.rs",
        missing.join("\n  ")
    );
}
