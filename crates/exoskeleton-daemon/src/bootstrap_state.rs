//! In-memory state for a single bootstrap session.
//!
//! Only one bootstrap can be in progress per vessel daemon instance.
//! The state tracks LLM configuration, conversation transcript, and
//! extracted identity through the bootstrap flow.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Mutex;

use exoskeleton_core::llm::LlmMessage;
use exoskeleton_core::prompt::PromptRegistry;
use exoskeleton_core::ExoError;
use exoskeleton_host::config::LlmConfig;
use exoskeleton_host::prompt_loader;
use tokio::sync::oneshot;

use crate::DaemonConfig;

/// Bootstrap session state. Thread-safe via interior mutability.
pub struct BootstrapState {
    pub data_dir: PathBuf,
    pub listen_addr: SocketAddr,
    registration_token: Option<String>,
    inner: Mutex<BootstrapInner>,
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
}

struct BootstrapInner {
    llm_config: Option<LlmConfig>,
    transcript: Vec<LlmMessage>,
    prompt_registry: PromptRegistry,
    identity: Option<VesselIdentity>,
    vessel_config: Option<DaemonConfig>,
    finalized: bool,
}

/// Extracted vessel identity from the first-contact conversation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VesselIdentity {
    pub vessel_name: String,
    pub mission: String,
    pub user_name: Option<String>,
    pub user_summary: Option<String>,
}

#[derive(Debug)]
pub enum BootstrapError {
    NotConfigured,
    NoTranscript,
    NotFinalized,
    AlreadyFinalized,
    Unauthorized,
    Llm(String),
    Config(String),
    Io(String),
}

impl fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConfigured => {
                write!(f, "LLM not configured — call /bootstrap/configure first")
            }
            Self::NoTranscript => write!(
                f,
                "no conversation transcript — complete the first-contact conversation first"
            ),
            Self::NotFinalized => {
                write!(
                    f,
                    "bootstrap not finalized — call /bootstrap/finalize first"
                )
            }
            Self::AlreadyFinalized => write!(f, "bootstrap already finalized"),
            Self::Unauthorized => {
                write!(f, "unauthorized — invalid or missing registration token")
            }
            Self::Llm(msg) => write!(f, "LLM error: {msg}"),
            Self::Config(msg) => write!(f, "config error: {msg}"),
            Self::Io(msg) => write!(f, "I/O error: {msg}"),
        }
    }
}

impl std::error::Error for BootstrapError {}

impl BootstrapState {
    pub fn new(
        data_dir: PathBuf,
        listen_addr: SocketAddr,
        shutdown_tx: oneshot::Sender<()>,
        registration_token: Option<String>,
    ) -> Self {
        // Load prompt registry — tier 2 (project-level) + tier 3 (compiled-in).
        // Tier 1 (per-vessel) is unavailable since the vessel doesn't exist yet.
        let mut prompt_registry = PromptRegistry::with_defaults();
        if data_dir.exists() {
            prompt_loader::load_prompt_overrides(&mut prompt_registry, &data_dir);
        } else {
            prompt_loader::load_project_prompt_overrides(&mut prompt_registry);
        }

        Self {
            data_dir,
            listen_addr,
            registration_token,
            inner: Mutex::new(BootstrapInner {
                llm_config: None,
                transcript: Vec::new(),
                prompt_registry,
                identity: None,
                vessel_config: None,
                finalized: false,
            }),
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
        }
    }

    /// Validate a registration token against the configured token.
    ///
    /// If no token is configured, all requests are accepted.
    /// If a token is configured, the request must provide a matching token.
    pub fn validate_registration_token(
        &self,
        request_token: Option<&str>,
    ) -> Result<(), BootstrapError> {
        match &self.registration_token {
            None => Ok(()), // No token configured — accept all requests
            Some(expected) => match request_token {
                Some(token) if token == expected => Ok(()),
                _ => Err(BootstrapError::Unauthorized),
            },
        }
    }

    /// Store LLM configuration from the configure endpoint.
    pub fn set_llm_config(&self, config: LlmConfig) -> Result<(), BootstrapError> {
        let mut inner = self.inner.lock().unwrap();
        inner.llm_config = Some(config);
        Ok(())
    }

    /// Get the current LLM config. Returns error if not yet configured.
    pub fn llm_config(&self) -> Result<LlmConfig, BootstrapError> {
        let inner = self.inner.lock().unwrap();
        inner
            .llm_config
            .clone()
            .ok_or(BootstrapError::NotConfigured)
    }

    /// Append a message to the transcript.
    pub fn push_message(&self, message: LlmMessage) {
        let mut inner = self.inner.lock().unwrap();
        inner.transcript.push(message);
    }

    /// Get the full transcript.
    pub fn transcript(&self) -> Vec<LlmMessage> {
        let inner = self.inner.lock().unwrap();
        inner.transcript.clone()
    }

    /// Get a prompt template by name.
    pub fn get_prompt(&self, name: &str) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        inner.prompt_registry.get(name).map(|s| s.to_string())
    }

    /// Resolve a prompt template by name with variable substitutions.
    pub fn resolve_prompt(&self, name: &str, vars: &[(&str, &str)]) -> Result<String, ExoError> {
        let inner = self.inner.lock().unwrap();
        inner.prompt_registry.resolve(name, vars)
    }

    /// Store the extracted identity after finalization.
    pub fn set_identity(&self, identity: VesselIdentity) {
        let mut inner = self.inner.lock().unwrap();
        inner.identity = Some(identity);
    }

    /// Mark bootstrap as finalized.
    pub fn set_finalized(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.finalized = true;
    }

    /// Check if bootstrap has been finalized.
    pub fn is_finalized(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.finalized
    }

    /// Store the DaemonConfig for transition and trigger server shutdown.
    pub fn trigger_start_vessel(&self, config: DaemonConfig) {
        let mut inner = self.inner.lock().unwrap();
        inner.vessel_config = Some(config);
        drop(inner);

        // Trigger shutdown of the bootstrap server
        if let Some(tx) = self.shutdown_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
    }

    /// Take the stored DaemonConfig (called after server shutdown).
    pub fn take_vessel_config(&self) -> Option<DaemonConfig> {
        let mut inner = self.inner.lock().unwrap();
        inner.vessel_config.take()
    }
}

#[cfg(test)]
mod tests {
    use exoskeleton_core::llm::LlmRole;

    use super::*;

    fn test_state() -> (tempfile::TempDir, BootstrapState) {
        let dir = tempfile::tempdir().unwrap();
        let addr: SocketAddr = "127.0.0.1:7600".parse().unwrap();
        let (tx, _rx) = oneshot::channel();
        let state = BootstrapState::new(dir.path().to_path_buf(), addr, tx, None);
        (dir, state)
    }

    // EO3-T4: BootstrapState: set_llm_config + llm_config round-trips
    #[test]
    fn set_llm_config_roundtrip() {
        let (_dir, state) = test_state();
        let config = LlmConfig {
            frontier: Some(exoskeleton_host::config::FrontierModelConfig {
                provider: exoskeleton_host::config::FrontierProvider::Anthropic,
                model: "claude-sonnet-4-20250514".into(),
                api_key_env: "ANTHROPIC_API_KEY".into(),
                endpoint: None,
            }),
            default_backend: exoskeleton_core::llm::LlmBackend::Frontier,
            ..Default::default()
        };
        state.set_llm_config(config.clone()).unwrap();
        let retrieved = state.llm_config().unwrap();
        assert_eq!(retrieved.default_backend, config.default_backend);
        assert!(retrieved.frontier.is_some());
    }

    // EO3-T5: BootstrapState: push_message accumulates transcript
    #[test]
    fn push_message_accumulates() {
        let (_dir, state) = test_state();
        state.push_message(LlmMessage::text(LlmRole::User, "Hello"));
        state.push_message(LlmMessage::text(LlmRole::Assistant, "Hi there"));
        let transcript = state.transcript();
        assert_eq!(transcript.len(), 2);
        assert_eq!(
            transcript[0].content[0],
            exoskeleton_core::llm::ContentBlock::Text {
                text: "Hello".into()
            }
        );
        assert_eq!(
            transcript[1].content[0],
            exoskeleton_core::llm::ContentBlock::Text {
                text: "Hi there".into()
            }
        );
    }

    // EO3-T6: BootstrapState: llm_config returns error when not configured
    #[test]
    fn llm_config_not_configured() {
        let (_dir, state) = test_state();
        let result = state.llm_config();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), BootstrapError::NotConfigured));
    }
}
