//! Top-level error type for the Exoskeleton system.

/// Top-level error type for the Exoskeleton system.
///
/// Individual crates define more specific error types. `ExoError` is the
/// unifying type used at crate boundaries and in the host/daemon/CLI.
#[derive(Debug, thiserror::Error)]
pub enum ExoError {
    /// Configuration error (invalid settings, missing required fields).
    #[error("configuration error: {0}")]
    Config(String),

    /// Storage error (database, file system, WAL).
    #[error("storage error: {0}")]
    Storage(String),

    /// ActionQueue engine error (either Cognitive AQ or Tool AQ).
    #[error("engine error: {0}")]
    Engine(String),

    /// LLM invocation error (timeout, rate limit, model error).
    #[error("LLM invocation error: {0}")]
    LlmInvocation(String),

    /// Context compilation error (budget exceeded, source unavailable).
    #[error("context compilation error: {0}")]
    ContextCompilation(String),

    /// Cognitive thread execution error.
    #[error("thread execution error: {0}")]
    ThreadExecution(String),

    /// Relationship invariant violation (atomic write failure, ledger corruption).
    #[error("relationship violation: {0}")]
    RelationshipViolation(String),

    /// Budget exhausted (no remaining capacity in at least one dimension).
    #[error("budget exhausted: {0}")]
    BudgetExhausted(String),

    /// A sacred invariant was violated. This is a system-level error that
    /// should trigger immediate investigation.
    #[error("INVARIANT VIOLATION: {0}")]
    InvariantViolation(String),

    /// Shutdown requested or in progress.
    #[error("shutdown: {0}")]
    Shutdown(String),

    /// Serialization/deserialization error.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T-9: Error Types ──

    #[test]
    fn exo_error_display() {
        let errors: Vec<ExoError> = vec![
            ExoError::Config("bad config".into()),
            ExoError::Storage("disk full".into()),
            ExoError::Engine("timeout".into()),
            ExoError::LlmInvocation("rate limited".into()),
            ExoError::ContextCompilation("budget exceeded".into()),
            ExoError::ThreadExecution("thread panicked".into()),
            ExoError::RelationshipViolation("ledger corrupted".into()),
            ExoError::BudgetExhausted("tokens depleted".into()),
            ExoError::InvariantViolation("I9 violated".into()),
            ExoError::Shutdown("graceful".into()),
        ];
        for e in &errors {
            let display = e.to_string();
            assert!(!display.is_empty(), "Empty display for {:?}", e);
        }
    }

    #[test]
    fn exo_error_from_serde() {
        let serde_err = serde_json::from_str::<String>("not json").unwrap_err();
        let exo_err: ExoError = serde_err.into();
        assert!(matches!(exo_err, ExoError::Serde(_)));
    }

    #[test]
    fn exo_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let exo_err: ExoError = io_err.into();
        assert!(matches!(exo_err, ExoError::Io(_)));
    }

    #[test]
    fn invariant_violation_message() {
        let err = ExoError::InvariantViolation("dual-engine merge attempted".into());
        let display = err.to_string();
        assert!(
            display.starts_with("INVARIANT VIOLATION"),
            "Expected INVARIANT VIOLATION prefix, got: {display}"
        );
    }
}
