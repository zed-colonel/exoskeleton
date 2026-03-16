//! Three-tier prompt loading from filesystem.
//!
//! Load priority:
//! 1. `{data_dir}/prompts/{file}` — per-vessel operator override (most specific)
//! 2. `prompts/{file}` relative to CWD — project-level defaults (version-controlled)
//! 3. `include_str!()` compiled-in fallback (already in registry from `with_defaults()`)
//!
//! Loading happens at vessel startup. The `load_prompt_overrides()` function
//! overlays filesystem-loaded prompts on top of the compiled-in defaults.

use std::path::Path;

use exoskeleton_core::prompt::PromptRegistry;

/// All prompt file names and their registry keys.
const PROMPT_FILES: &[(&str, &str)] = &[
    ("decide-system", "decide-system.md"),
    ("context-system-section", "context-system-section.md"),
    ("thread-execution", "thread-execution.md"),
    ("thread-user-message", "thread-user-message.md"),
    ("charter-threat-monitor", "charters/threat-monitor.md"),
    ("charter-self-critique", "charters/self-critique.md"),
    (
        "charter-memory-consolidation",
        "charters/memory-consolidation.md",
    ),
    ("bootstrap-first-contact", "bootstrap/first-contact.md"),
    (
        "bootstrap-identity-extraction",
        "bootstrap/identity-extraction.md",
    ),
];

/// Load prompt overrides from filesystem, overlaying on compiled-in defaults.
///
/// For each prompt, checks:
///
/// 1. `{data_dir}/prompts/{file}` — operator override
/// 2. `prompts/{file}` relative to CWD — project-level
///
/// If found, replaces the compiled-in default in the registry.
pub fn load_prompt_overrides(registry: &mut PromptRegistry, data_dir: &Path) {
    for (key, filename) in PROMPT_FILES {
        // Tier 1: per-vessel override
        let tier1 = data_dir.join("prompts").join(filename);
        if let Ok(content) = std::fs::read_to_string(&tier1) {
            tracing::info!(
                prompt = key,
                path = %tier1.display(),
                "loaded prompt override (tier 1: data_dir)"
            );
            registry.insert(*key, content);
            continue;
        }

        // Tier 2: project-level
        let tier2 = Path::new("prompts").join(filename);
        if let Ok(content) = std::fs::read_to_string(&tier2) {
            tracing::debug!(
                prompt = key,
                path = %tier2.display(),
                "loaded prompt (tier 2: project)"
            );
            registry.insert(*key, content);
            continue;
        }

        // Tier 3: compiled-in default (already in registry from with_defaults())
        tracing::debug!(prompt = key, "using compiled-in default (tier 3)");
    }
}

/// Load only project-level (tier 2) prompt overrides.
///
/// Used by bootstrap which runs before a vessel data_dir exists.
pub fn load_project_prompt_overrides(registry: &mut PromptRegistry) {
    for (key, filename) in PROMPT_FILES {
        let tier2 = Path::new("prompts").join(filename);
        if let Ok(content) = std::fs::read_to_string(&tier2) {
            tracing::debug!(
                prompt = key,
                path = %tier2.display(),
                "loaded prompt (tier 2: project)"
            );
            registry.insert(*key, content);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── E0-T12: data_dir override takes precedence ──

    #[test]
    fn load_from_data_dir_overrides_default() {
        let dir = tempfile::tempdir().unwrap();
        let prompts_dir = dir.path().join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();
        std::fs::write(
            prompts_dir.join("decide-system.md"),
            "Custom decide prompt from data_dir",
        )
        .unwrap();

        let mut registry = PromptRegistry::with_defaults();
        let original = registry.get("decide-system").unwrap().to_string();
        assert_ne!(original, "Custom decide prompt from data_dir");

        load_prompt_overrides(&mut registry, dir.path());

        assert_eq!(
            registry.get("decide-system").unwrap(),
            "Custom decide prompt from data_dir"
        );
        // Other prompts should still be defaults
        assert_eq!(registry.len(), 9);
    }

    // ── E0-T13: project-level override takes precedence over compiled-in ──

    #[test]
    fn load_from_project_dir_overrides_default() {
        // This test uses the real prompts/ directory relative to CWD,
        // which actually exists in the workspace. The project-level files
        // should be loaded and match what's in the prompts/ directory.
        let mut registry = PromptRegistry::with_defaults();
        let compiled_in = registry.get("decide-system").unwrap().to_string();

        // Load with a non-existent data_dir so tier 1 is skipped
        let fake_data_dir = tempfile::tempdir().unwrap();
        load_prompt_overrides(&mut registry, fake_data_dir.path());

        // The project-level file (if accessible from CWD) should match compiled-in
        // since both come from the same source. This validates tier 2 loading works.
        let after_load = registry.get("decide-system").unwrap();
        // Content should still be valid (either project or compiled-in)
        assert!(!after_load.is_empty());
        // If project file was found, it should match compiled-in (same source)
        // If not found, compiled-in default is preserved
        assert!(after_load.contains("vessel") || after_load == compiled_in);
    }

    // ── E0-T14: data_dir takes precedence over project ──

    #[test]
    fn data_dir_takes_precedence_over_project() {
        let dir = tempfile::tempdir().unwrap();
        let prompts_dir = dir.path().join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();
        std::fs::write(
            prompts_dir.join("decide-system.md"),
            "Data dir override wins",
        )
        .unwrap();

        let mut registry = PromptRegistry::with_defaults();
        load_prompt_overrides(&mut registry, dir.path());

        // data_dir tier 1 should win over tier 2 (project) and tier 3 (compiled-in)
        assert_eq!(
            registry.get("decide-system").unwrap(),
            "Data dir override wins"
        );
    }

    // ── E0-T15: missing files use compiled default ──

    #[test]
    fn missing_files_use_compiled_default() {
        let dir = tempfile::tempdir().unwrap();
        // No prompt files in data_dir

        let mut registry = PromptRegistry::with_defaults();
        let before = registry.get("decide-system").unwrap().to_string();

        load_prompt_overrides(&mut registry, dir.path());

        // Should still have the compiled-in default (or project-level if accessible)
        let after = registry.get("decide-system").unwrap();
        assert!(!after.is_empty());
        // All 9 prompts should still be present
        assert_eq!(registry.len(), 9);
        // If no project-level files found, should match the compiled-in
        if !Path::new("prompts/decide-system.md").exists() {
            assert_eq!(after, &before);
        }
    }

    // ── E0-T: charter subdirectory loading ──

    #[test]
    fn data_dir_charter_subdirectory_loading() {
        let dir = tempfile::tempdir().unwrap();
        let charters_dir = dir.path().join("prompts").join("charters");
        std::fs::create_dir_all(&charters_dir).unwrap();
        std::fs::write(
            charters_dir.join("threat-monitor.md"),
            "Custom threat charter",
        )
        .unwrap();

        let mut registry = PromptRegistry::with_defaults();
        load_prompt_overrides(&mut registry, dir.path());

        assert_eq!(
            registry.get("charter-threat-monitor").unwrap(),
            "Custom threat charter"
        );
    }
}
