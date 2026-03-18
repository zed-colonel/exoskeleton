//! Prompt template registry with `{{variable}}` substitution.
//!
//! The `PromptRegistry` is a pure in-memory registry of named prompt templates.
//! Templates use `{{variable_name}}` placeholders (Mustache-style). The registry
//! is populated at boot time and immutable during the run. Thread-safe via
//! `Arc<PromptRegistry>` on `KernelContext`.
//!
//! No I/O, no filesystem access — this module keeps `exoskeleton-core` as a
//! leaf dependency. Filesystem loading is in `exoskeleton-host::prompt_loader`.

use std::collections::HashMap;

use crate::ExoError;

/// In-memory registry of named prompt templates.
///
/// Templates use `{{variable_name}}` placeholders (Mustache-style).
/// The registry is populated at boot time and immutable during the run.
/// Thread-safe via `Arc<PromptRegistry>` on `KernelContext`.
pub struct PromptRegistry {
    templates: HashMap<String, String>,
}

impl PromptRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            templates: HashMap::new(),
        }
    }

    /// Create a registry pre-populated with all compiled-in defaults.
    ///
    /// Uses `include_str!()` to embed the prompt files at compile time.
    /// This is the tier-3 fallback — always works, even without filesystem.
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.insert(
            "decide-system",
            include_str!("../../../prompts/decide-system.md"),
        );
        registry.insert(
            "context-system-section",
            include_str!("../../../prompts/context-system-section.md"),
        );
        registry.insert(
            "thread-execution",
            include_str!("../../../prompts/thread-execution.md"),
        );
        registry.insert(
            "thread-user-message",
            include_str!("../../../prompts/thread-user-message.md"),
        );
        registry.insert(
            "charter-threat-monitor",
            include_str!("../../../prompts/charters/threat-monitor.md"),
        );
        registry.insert(
            "charter-self-critique",
            include_str!("../../../prompts/charters/self-critique.md"),
        );
        registry.insert(
            "charter-memory-consolidation",
            include_str!("../../../prompts/charters/memory-consolidation.md"),
        );
        registry.insert(
            "reflect-system",
            include_str!("../../../prompts/reflect-system.md"),
        );
        registry.insert(
            "bootstrap-first-contact",
            include_str!("../../../prompts/bootstrap/first-contact.md"),
        );
        registry.insert(
            "bootstrap-identity-extraction",
            include_str!("../../../prompts/bootstrap/identity-extraction.md"),
        );
        registry
    }

    /// Insert or replace a template.
    pub fn insert(&mut self, name: impl Into<String>, template: impl Into<String>) {
        self.templates.insert(name.into(), template.into());
    }

    /// Get raw template text by name.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.templates.get(name).map(|s| s.as_str())
    }

    /// Resolve a template: look up by name, substitute all `{{var}}` placeholders.
    ///
    /// Returns `ExoError::Config` if:
    /// - Template name not found
    /// - Any `{{variable}}` remains unresolved after substitution
    pub fn resolve(&self, name: &str, vars: &[(&str, &str)]) -> Result<String, ExoError> {
        let template = self
            .templates
            .get(name)
            .ok_or_else(|| ExoError::Config(format!("prompt template not found: {name}")))?;

        let mut result = template.clone();
        for (key, value) in vars {
            let placeholder = format!("{{{{{key}}}}}");
            result = result.replace(&placeholder, value);
        }

        // Check for unresolved variables
        if let Some(start) = result.find("{{") {
            if let Some(end) = result[start + 2..].find("}}") {
                let var_name = &result[start + 2..start + 2 + end];
                // Only flag as unresolved if it looks like a variable name
                // (alphanumeric + underscore, no spaces/punctuation)
                if var_name.chars().all(|c| c.is_alphanumeric() || c == '_') && !var_name.is_empty()
                {
                    return Err(ExoError::Config(format!(
                        "unresolved variable in template '{name}': {{{{{var_name}}}}}"
                    )));
                }
            }
        }

        Ok(result)
    }

    /// List all registered template names.
    pub fn names(&self) -> Vec<&str> {
        self.templates.keys().map(|s| s.as_str()).collect()
    }

    /// Number of templates in the registry.
    pub fn len(&self) -> usize {
        self.templates.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }
}

impl Default for PromptRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── E0-T6: resolve substitutes all variables ──

    #[test]
    fn resolve_substitutes_all_variables() {
        let mut registry = PromptRegistry::new();
        registry.insert("test", "Hello {{name}}, you are {{role}} at {{company}}.");

        let result = registry
            .resolve(
                "test",
                &[("name", "Alice"), ("role", "engineer"), ("company", "Acme")],
            )
            .unwrap();

        assert_eq!(result, "Hello Alice, you are engineer at Acme.");
    }

    // ── E0-T7: resolve errors on missing variable ──

    #[test]
    fn resolve_errors_on_missing_variable() {
        let mut registry = PromptRegistry::new();
        registry.insert("test", "Hello {{name}}, you have {{unknown}} items.");

        let result = registry.resolve("test", &[("name", "Alice")]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown"),
            "error should mention the unresolved variable: {err}"
        );
    }

    // ── E0-T8: resolve errors on unknown template ──

    #[test]
    fn resolve_errors_on_unknown_template() {
        let registry = PromptRegistry::new();
        let result = registry.resolve("nonexistent", &[]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("nonexistent"),
            "error should mention the missing template: {err}"
        );
    }

    // ── E0-T9: with_defaults has all nine prompts ──

    #[test]
    fn with_defaults_has_all_ten_prompts() {
        let registry = PromptRegistry::with_defaults();
        assert_eq!(registry.len(), 10, "should have 10 compiled-in prompts");

        let expected_keys = [
            "decide-system",
            "context-system-section",
            "thread-execution",
            "thread-user-message",
            "charter-threat-monitor",
            "charter-self-critique",
            "charter-memory-consolidation",
            "reflect-system",
            "bootstrap-first-contact",
            "bootstrap-identity-extraction",
        ];
        for key in &expected_keys {
            assert!(
                registry.get(key).is_some(),
                "missing compiled-in prompt: {key}"
            );
            assert!(
                !registry.get(key).unwrap().is_empty(),
                "compiled-in prompt should not be empty: {key}"
            );
        }
    }

    // ── E0-T10: resolve preserves literal braces ──

    #[test]
    fn resolve_preserves_literal_braces() {
        let mut registry = PromptRegistry::new();
        registry.insert(
            "json-example",
            "Respond with JSON:\n{\n  \"name\": \"{{name}}\"\n}",
        );

        let result = registry
            .resolve("json-example", &[("name", "test")])
            .unwrap();
        assert_eq!(result, "Respond with JSON:\n{\n  \"name\": \"test\"\n}");
    }

    // ── E0-T11: insert overwrites existing ──

    #[test]
    fn insert_overwrites_existing() {
        let mut registry = PromptRegistry::new();
        registry.insert("key", "first value");
        registry.insert("key", "second value");

        assert_eq!(registry.get("key").unwrap(), "second value");
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn resolve_with_no_variables() {
        let mut registry = PromptRegistry::new();
        registry.insert("plain", "No variables here.");

        let result = registry.resolve("plain", &[]).unwrap();
        assert_eq!(result, "No variables here.");
    }

    #[test]
    fn resolve_decide_system_template() {
        let registry = PromptRegistry::with_defaults();
        let result = registry
            .resolve(
                "decide-system",
                &[
                    ("vessel_id", "test-vessel-123"),
                    ("mission", "explore the cosmos"),
                    ("tools", "tool_a: does thing A\ntool_b: does thing B"),
                ],
            )
            .unwrap();

        assert!(result.contains("vessel test-vessel-123"));
        assert!(result.contains("explore the cosmos"));
        assert!(result.contains("tool_a: does thing A"));
        // JSON braces should be preserved as literal
        assert!(result.contains("\"reasoning\""));
    }

    #[test]
    fn resolve_context_system_section_template() {
        let registry = PromptRegistry::with_defaults();
        let result = registry
            .resolve(
                "context-system-section",
                &[("vessel_id", "v-001"), ("mission", "assist humans")],
            )
            .unwrap();

        assert!(result.contains("v-001"));
        assert!(result.contains("assist humans"));
        assert!(result.contains("PODAARA"));
    }

    #[test]
    fn resolve_thread_execution_template() {
        let registry = PromptRegistry::with_defaults();
        let result = registry
            .resolve(
                "thread-execution",
                &[
                    ("name", "Threat Monitor"),
                    ("id", "abc-123"),
                    ("charter", "scan for threats"),
                    ("priority", "Critical"),
                    ("tick", "42"),
                ],
            )
            .unwrap();

        assert!(result.contains("THREAD: Threat Monitor"));
        assert!(result.contains("abc-123"));
        assert!(result.contains("scan for threats"));
        assert!(result.contains("Critical"));
        assert!(result.contains("42"));
    }

    #[test]
    fn resolve_identity_extraction_template() {
        let registry = PromptRegistry::with_defaults();
        let result = registry
            .resolve(
                "bootstrap-identity-extraction",
                &[("transcript", "User: Hello\nAssistant: Hi there!")],
            )
            .unwrap();

        assert!(result.contains("User: Hello"));
        assert!(result.contains("vessel_name"));
    }
}
