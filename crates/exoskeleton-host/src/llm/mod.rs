//! LLM inference infrastructure for the Cognitive AQ.
//!
//! All LLM calls are cognitive work dispatched through the Cognitive AQ (I1, I9).
//! This module provides:
//! - HTTP backends for different LLM APIs (OpenAI-compatible, Anthropic, Ollama)
//! - Error classification (retryable vs. terminal)
//! - Mock backend for testing
//! - LlmClient convenience wrapper for submitting LLM tasks

pub mod client;
pub mod http;
pub mod mock;
