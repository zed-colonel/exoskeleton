//! Direct LLM calls without AQ infrastructure.
//!
//! Used for bootstrap and other pre-vessel scenarios where the full
//! Cognitive AQ engine isn't running. Builds the appropriate HTTP backend
//! from config and makes a single synchronous call on a blocking thread.

use std::sync::Arc;
use std::time::Duration;

use actionqueue_executor_local::CancellationToken;
use exoskeleton_core::llm::{LlmBackend, LlmRequest, LlmResponse};
use exoskeleton_core::ExoError;

use crate::config::{FrontierProvider, LlmConfig, LocalApiFormat};

use super::http::{AnthropicBackend, LlmHttpBackend, OllamaNativeBackend, OpenAiCompatBackend};

/// Make a direct LLM call without AQ infrastructure.
///
/// Builds the appropriate HTTP backend from config, then executes the call
/// on a blocking thread (the backends use `Handle::current().block_on()`
/// internally for async HTTP).
///
/// This is the entry point for bootstrap conversations and connectivity
/// verification — anywhere you need an LLM response before the Vessel
/// is fully booted.
pub async fn direct_llm_call(
    llm_config: &LlmConfig,
    request: LlmRequest,
) -> Result<LlmResponse, ExoError> {
    let backend = build_default_backend(llm_config)?;
    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(llm_config.timeout_secs))
        .build()
        .map_err(|e| ExoError::Config(format!("HTTP client build failed: {e}")))?;
    let token = CancellationToken::new();

    tokio::task::spawn_blocking(move || backend.call(&http_client, &request, &token))
        .await
        .map_err(|e| ExoError::LlmInvocation(format!("blocking task join error: {e}")))?
}

/// Build the default backend based on `LlmConfig.default_backend`.
fn build_default_backend(config: &LlmConfig) -> Result<Arc<dyn LlmHttpBackend>, ExoError> {
    match config.default_backend {
        LlmBackend::Local => build_local_backend(config)?
            .ok_or_else(|| ExoError::Config("default_backend is Local but no local config".into())),
        LlmBackend::Frontier => build_frontier_backend(config)?.ok_or_else(|| {
            ExoError::Config("default_backend is Frontier but no frontier config".into())
        }),
    }
}

fn build_local_backend(config: &LlmConfig) -> Result<Option<Arc<dyn LlmHttpBackend>>, ExoError> {
    let local_config = match config.local.as_ref() {
        Some(c) => c,
        None => return Ok(None),
    };
    let backend: Arc<dyn LlmHttpBackend> = match local_config.api_format {
        LocalApiFormat::OpenAICompat => Arc::new(OpenAiCompatBackend::new(
            local_config.endpoint.clone(),
            local_config.model.clone(),
            None,
        )),
        LocalApiFormat::Ollama => Arc::new(OllamaNativeBackend::new(
            local_config.endpoint.clone(),
            local_config.model.clone(),
        )),
    };
    Ok(Some(backend))
}

fn build_frontier_backend(config: &LlmConfig) -> Result<Option<Arc<dyn LlmHttpBackend>>, ExoError> {
    let frontier_config = match config.frontier.as_ref() {
        Some(c) => c,
        None => return Ok(None),
    };

    let api_key = std::env::var(&frontier_config.api_key_env).map_err(|_| {
        ExoError::Config(format!(
            "API key env var '{}' not set (required for frontier backend)",
            frontier_config.api_key_env
        ))
    })?;

    let base_url =
        frontier_config
            .endpoint
            .clone()
            .unwrap_or_else(|| match frontier_config.provider {
                FrontierProvider::Anthropic => "https://api.anthropic.com".into(),
                FrontierProvider::OpenAI => "https://api.openai.com".into(),
            });

    let backend: Arc<dyn LlmHttpBackend> = match frontier_config.provider {
        FrontierProvider::Anthropic => Arc::new(AnthropicBackend::new(
            base_url,
            frontier_config.model.clone(),
            api_key,
        )),
        FrontierProvider::OpenAI => Arc::new(OpenAiCompatBackend::new(
            base_url,
            frontier_config.model.clone(),
            Some(api_key),
        )),
    };
    Ok(Some(backend))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_default_backend_rejects_missing_local() {
        let config = LlmConfig {
            local: None,
            frontier: None,
            default_backend: LlmBackend::Local,
            ..Default::default()
        };
        assert!(build_default_backend(&config).is_err());
    }

    #[test]
    fn build_default_backend_rejects_missing_frontier() {
        let config = LlmConfig {
            local: None,
            frontier: None,
            default_backend: LlmBackend::Frontier,
            ..Default::default()
        };
        assert!(build_default_backend(&config).is_err());
    }

    #[test]
    fn build_local_backend_openai_compat() {
        let config = LlmConfig {
            local: Some(crate::config::LocalModelConfig {
                endpoint: "http://localhost:11434".into(),
                model: "llama3.2:latest".into(),
                api_format: LocalApiFormat::OpenAICompat,
            }),
            ..Default::default()
        };
        assert!(build_local_backend(&config).unwrap().is_some());
    }

    #[test]
    fn build_local_backend_ollama() {
        let config = LlmConfig {
            local: Some(crate::config::LocalModelConfig {
                endpoint: "http://localhost:11434".into(),
                model: "llama3.2:latest".into(),
                api_format: LocalApiFormat::Ollama,
            }),
            ..Default::default()
        };
        assert!(build_local_backend(&config).unwrap().is_some());
    }

    #[test]
    fn build_local_backend_none_when_not_configured() {
        let config = LlmConfig::default();
        assert!(build_local_backend(&config).unwrap().is_none());
    }
}
