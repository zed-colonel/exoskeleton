//! Vessel runtime configuration.
//!
//! [`VesselConfig`] is the runtime representation with typed fields (Duration,
//! NonZeroUsize). [`VesselConfigFile`] is the TOML-friendly intermediate that
//! deserializes from files and converts to `VesselConfig` via `TryFrom`.
//!
//! Each engine has independent configuration settings (I9: cognitive and tool
//! execution isolated).

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::Duration;

use exoskeleton_core::llm::LlmBackend;
use exoskeleton_core::{ExoError, VesselId};
use serde::{Deserialize, Serialize};

use crate::kernel::policy::ToolPolicyConfig;

/// Wire format of the LLM API for local models.
///
/// Most local inference servers support the OpenAI Chat Completions format.
/// Ollama also has a native format at `/api/chat`. For maximum compatibility,
/// default to `OpenAICompat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalApiFormat {
    /// OpenAI-compatible Chat Completions API (`/v1/chat/completions`).
    /// Works with: Ollama (v0.1.24+), vLLM, LM Studio, llama.cpp server.
    #[serde(rename = "openai_compat")]
    OpenAICompat,
    /// Ollama native API (`/api/chat`).
    #[serde(rename = "ollama")]
    Ollama,
}

/// Frontier model provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontierProvider {
    /// Anthropic Messages API.
    Anthropic,
    /// OpenAI Chat Completions API.
    #[serde(rename = "openai")]
    OpenAI,
    /// Google Gemini (OpenAI-compatible endpoint).
    Gemini,
    /// xAI Grok (OpenAI-compatible endpoint).
    Grok,
    /// OpenRouter (OpenAI-compatible multi-model proxy).
    #[serde(rename = "openrouter")]
    OpenRouter,
    /// DeepSeek (OpenAI-compatible endpoint).
    #[serde(rename = "deepseek")]
    DeepSeek,
}

/// Configuration for a local LLM backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalModelConfig {
    /// Base URL of the local inference server.
    /// Examples: "http://localhost:11434" (Ollama), "http://localhost:8080" (vLLM).
    pub endpoint: String,
    /// Model name/tag. Examples: "llama3.2:latest", "qwen2.5-coder:7b".
    pub model: String,
    /// Which API format the server speaks.
    #[serde(default = "default_openai_compat")]
    pub api_format: LocalApiFormat,
}

fn default_openai_compat() -> LocalApiFormat {
    LocalApiFormat::OpenAICompat
}

/// Configuration for a frontier LLM backend.
///
/// API keys are read from environment variables at call time (I4: least
/// privilege). The `api_key_env` field holds the NAME of the env var,
/// not the key itself. Keys MUST NEVER be stored in config files,
/// task payloads, artifacts, or logs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontierModelConfig {
    /// Which provider's API to use.
    pub provider: FrontierProvider,
    /// Model identifier. Examples: "claude-sonnet-4-20250514", "gpt-4o".
    pub model: String,
    /// Name of the environment variable holding the API key.
    /// Examples: "ANTHROPIC_API_KEY", "OPENAI_API_KEY".
    /// The key is read from this env var at call time — never stored.
    pub api_key_env: String,
    /// Optional custom endpoint URL override.
    /// If `None`, uses the provider's default endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

fn default_local_backend() -> LlmBackend {
    LlmBackend::Local
}
fn default_max_output_tokens() -> u64 {
    4096
}
fn default_llm_timeout_secs() -> u64 {
    120
}

/// LLM backend configuration.
///
/// At least one of `local` or `frontier` must be configured. The
/// `default_backend` determines which is used when a request doesn't
/// specify a backend explicitly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmConfig {
    /// Local model configuration. `None` if no local model available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<LocalModelConfig>,
    /// Frontier model configuration. `None` if no frontier model available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontier: Option<FrontierModelConfig>,
    /// Which backend to use when requests don't specify one.
    #[serde(default = "default_local_backend")]
    pub default_backend: LlmBackend,
    /// Default max output tokens for requests that don't specify one.
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u64,
    /// Request timeout in seconds. Applied per HTTP request.
    #[serde(default = "default_llm_timeout_secs")]
    pub timeout_secs: u64,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            local: None,
            frontier: None,
            default_backend: LlmBackend::Local,
            max_output_tokens: 4096,
            timeout_secs: 120,
        }
    }
}

/// Inner loop configuration for interactive coding sessions (E8-S1).
///
/// Controls the bounded inner interaction loop within PODAARA ticks.
/// When `enabled = false` (default), the master loop behaves exactly as
/// before (one Decide+Act per tick). When enabled, the agent can request
/// iterative DecideLite→Act→Observe cycles within a single tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InnerLoopConfig {
    /// Enable the bounded inner interaction loop within PODAARA ticks.
    /// When false, the master loop behaves exactly as before.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum inner-loop steps (Decide→Act iterations) per tick.
    #[serde(default = "default_inner_loop_max_steps")]
    pub max_steps_per_tick: u32,
    /// Maximum total tokens (input + output) consumed by inner-loop
    /// LLM calls within one tick. Independent of the window budget.
    #[serde(default = "default_inner_loop_max_tokens")]
    pub max_tokens_per_session: u64,
    /// Maximum wall-clock time (seconds) for the inner loop within one tick.
    #[serde(default = "default_inner_loop_timeout")]
    pub timeout_secs: u64,
    /// Number of recent tool results to include in full when building
    /// DecideLite context. Older results are summarized.
    #[serde(default = "default_inner_loop_context_window_size")]
    pub context_window_size: u32,
    /// Number of identical consecutive tool calls (same tool + same args)
    /// before doom-loop detection triggers and aborts the inner loop.
    #[serde(default = "default_inner_loop_doom_threshold")]
    pub doom_loop_threshold: u32,
    /// Workspace root path for repo analysis and git context.
    #[serde(default)]
    pub workspace_root: Option<String>,
}

fn default_inner_loop_max_steps() -> u32 {
    25
}
fn default_inner_loop_max_tokens() -> u64 {
    500_000
}
fn default_inner_loop_timeout() -> u64 {
    300
}
fn default_inner_loop_context_window_size() -> u32 {
    3
}
fn default_inner_loop_doom_threshold() -> u32 {
    3
}

impl Default for InnerLoopConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_steps_per_tick: 25,
            max_tokens_per_session: 500_000,
            timeout_secs: 300,
            context_window_size: 3,
            doom_loop_threshold: 3,
            workspace_root: None,
        }
    }
}

/// Builder for coding-optimized vessel configuration defaults (E10-S2, W-101).
pub struct CodingDefaults;

impl CodingDefaults {
    /// Build an InnerLoopConfig with coding-optimized settings.
    pub fn inner_loop_config(workspace_root: Option<String>) -> InnerLoopConfig {
        InnerLoopConfig {
            enabled: true,
            max_steps_per_tick: 25,
            max_tokens_per_session: 500_000,
            timeout_secs: 300,
            context_window_size: 3,
            doom_loop_threshold: 3,
            workspace_root,
        }
    }

    /// Build a ToolPolicyConfig with coding defaults.
    pub fn tool_policy_config() -> crate::kernel::policy::ToolPolicyConfig {
        use crate::kernel::policy::{PolicyRule, ToolPolicyConfig};

        let mut rules = std::collections::HashMap::new();
        rules.insert("shell.exec".into(), PolicyRule::Ask);
        rules.insert("fs.write".into(), PolicyRule::Ask);

        ToolPolicyConfig {
            default: PolicyRule::Allow,
            rules,
        }
    }
}

/// Top-level configuration for an Exoskeleton Vessel.
///
/// Each engine has independent configuration (I9: cognitive and tool execution
/// isolated). The Vessel maps these into `RuntimeConfig` (for the Cognitive AQ)
/// and `HostConfig` (for the WI Host / Tool AQ).
#[derive(Debug, Clone)]
pub struct VesselConfig {
    /// Unique identity of this vessel instance.
    pub vessel_id: VesselId,
    /// Root data directory. All engine-specific subdirectories are created under this.
    pub data_dir: PathBuf,
    /// The vessel's primary objective.
    pub mission: String,

    // ── Cognitive AQ settings (I9: independent) ──
    /// Cognitive AQ dispatch tick interval.
    /// How often the Cognitive AQ scheduler checks for promotable/dispatchable tasks.
    /// Default: 100ms.
    pub cognitive_tick_interval: Duration,
    /// Cognitive AQ worker count (max concurrent executing runs).
    /// Default: 4.
    pub cognitive_dispatch_concurrency: NonZeroUsize,
    /// Cognitive AQ lease timeout in seconds.
    /// Must account for worst-case Act step duration (IBP §4.6) — during Act,
    /// the cognitive task is in Running state while waiting for Tool AQ completion.
    /// Default: 600 (10 minutes).
    pub cognitive_lease_timeout_secs: u64,

    // ── Tool AQ settings (I9: independent, passed to WI Host) ──
    /// Tool AQ (WI Host) dispatch tick interval.
    /// Default: 50ms.
    pub tool_tick_interval: Duration,
    /// Tool AQ (WI Host) worker count.
    /// Default: 4.
    pub tool_dispatch_concurrency: NonZeroUsize,

    // ── Lifecycle settings ──
    /// How long to wait for graceful shutdown of each engine.
    /// Default: 30 seconds.
    pub shutdown_timeout: Duration,

    // ── LLM settings (Sprint 4) ──
    /// LLM backend configuration: local/frontier models, timeouts, defaults.
    pub llm_config: LlmConfig,

    // ── Master loop settings (Sprint 5) ──
    /// How often the master loop runs a PODAARA tick (seconds).
    /// Default: 60 seconds.
    pub master_loop_interval_secs: u64,

    /// Directory for the file-based inbox.
    /// Default: {data_dir}/inbox/
    pub inbox_dir: Option<PathBuf>,

    // ── Budget settings (Sprint 9) ──
    /// Cognitive AQ budget configuration. `None` disables budget enforcement.
    pub cognitive_budget: Option<exoskeleton_core::CognitiveBudgetConfig>,
    /// Tool AQ budget configuration. `None` disables tool rate limiting.
    pub tool_budget: Option<exoskeleton_core::ToolBudgetConfig>,

    // ── Daemon settings (Sprint 10) ──
    /// Daemon HTTP listen address. `None` uses default 127.0.0.1:7600.
    /// Set to 0.0.0.0:7600 for Docker/container deployment.
    pub daemon_listen: Option<std::net::SocketAddr>,

    // ── CORS settings (Phase U1) ──
    /// Allowed origins for CORS requests. Empty disables CORS (same-origin only).
    /// Set to `["http://localhost:5173"]` for Observatory development mode.
    pub cors_allowed_origins: Vec<String>,

    // ── Relationship settings (E1-S3) ──
    /// Trust decay configuration. `None` disables time-based decay (pre-E1-S3 behavior).
    pub trust_decay: Option<exoskeleton_core::TrustDecayConfig>,

    // ── Memory settings (E1-S3) ──
    /// Episodic memory capacity. When episodic count exceeds this, oldest entries
    /// are evicted. `None` disables eviction. Default: Some(200).
    pub episodic_memory_capacity: Option<u64>,

    // ── Bootstrap settings (Decoherence Fix) ──
    /// Number of ticks during which built-in threads receive bootstrap-phase
    /// context (reduced sensitivity for Threat Monitor and Self-Critique).
    /// 0 disables the grace period. Default: 30.
    pub bootstrap_grace_period_ticks: u64,

    // ── Thread settings (Decoherence Fix) ──
    /// Operator overrides for built-in thread schedule and budget.
    /// `None` uses compiled-in defaults.
    pub threads: Option<ThreadsSection>,

    // ── Source access settings (E2-S2) ──
    /// Source repositories to mount in the vessel container. Empty means no source access.
    pub source_repos: Vec<SourceRepoConfig>,

    // ── Sandbox settings (E2-S2) ──
    /// Sandbox configuration. Default: enabled, 256MB tmpfs.
    pub sandbox: SandboxConfig,

    // ── Observatory connection (E4-S1) ──
    /// Observatory base URL for fleet discovery (peer.resolve).
    /// `None` disables vessel-to-vessel communication.
    /// Example: "http://observatory:3000"
    pub observatory_url: Option<String>,

    /// Environment variable name holding the Observatory API token.
    /// The variable is read at vessel startup, not stored in config.
    /// Example: "EXO_OBSERVATORY_TOKEN"
    pub observatory_token_env: Option<String>,

    // ── Multi-turn Decide settings (E5-S1) ──
    /// Maximum number of LLM turns in the Decide step. Each turn can be
    /// an introspection query or the final decision. Minimum: 1 (single-turn,
    /// backward compatible). Default: 5.
    pub max_decide_turns: u32,
    /// Maximum number of active watches per vessel. Default: 20.
    pub max_watches: u32,
    /// Additional tools classified as destructive beyond the AlignConfig defaults.
    /// Loaded from [connectors.destructive] in vessel.toml.
    pub extra_destructive_tools: Vec<String>,

    // ── WASM connector settings ──
    /// Directory containing pre-compiled WASM connectors (.wasm + .connector.toml).
    /// `None` disables WASM connector loading.
    /// Default: None (env override: EXO_CONNECTORS_DIR).
    pub connectors_dir: Option<PathBuf>,

    // ── Inner loop settings (E8-S1) ──
    /// Inner loop configuration for interactive coding sessions.
    pub inner_loop: InnerLoopConfig,
    /// Tool policy configuration for allow/deny/ask rules.
    pub tool_policy: ToolPolicyConfig,
}

impl Default for VesselConfig {
    fn default() -> Self {
        Self {
            vessel_id: VesselId::new(),
            data_dir: PathBuf::from("data"),
            mission: String::new(), // Must be overridden — validate() rejects empty
            cognitive_tick_interval: Duration::from_millis(100),
            cognitive_dispatch_concurrency: NonZeroUsize::new(4).unwrap(),
            cognitive_lease_timeout_secs: 600,
            tool_tick_interval: Duration::from_millis(50),
            tool_dispatch_concurrency: NonZeroUsize::new(4).unwrap(),
            shutdown_timeout: Duration::from_secs(30),
            llm_config: LlmConfig::default(),
            master_loop_interval_secs: 60,
            inbox_dir: None,
            cognitive_budget: None,
            tool_budget: None,
            daemon_listen: None,
            cors_allowed_origins: Vec::new(),
            trust_decay: None,
            episodic_memory_capacity: Some(200),
            bootstrap_grace_period_ticks: 30,
            threads: None,
            source_repos: Vec::new(),
            sandbox: SandboxConfig::default(),
            observatory_url: None,
            observatory_token_env: None,
            max_decide_turns: 5,
            max_watches: exoskeleton_core::watch::DEFAULT_MAX_WATCHES,
            extra_destructive_tools: vec![],
            connectors_dir: None,
            inner_loop: InnerLoopConfig::default(),
            tool_policy: ToolPolicyConfig::default(),
        }
    }
}

impl VesselConfig {
    /// Validate the configuration. Returns `ExoError::Config` on failure.
    pub fn validate(&self) -> Result<(), ExoError> {
        if self.mission.is_empty() {
            return Err(ExoError::Config("mission must not be empty".into()));
        }
        if self.cognitive_tick_interval.is_zero() {
            return Err(ExoError::Config(
                "cognitive_tick_interval must be > 0".into(),
            ));
        }
        if self.cognitive_tick_interval > Duration::from_secs(60) {
            return Err(ExoError::Config(
                "cognitive_tick_interval must be <= 60s".into(),
            ));
        }
        if self.cognitive_lease_timeout_secs < 3 {
            return Err(ExoError::Config(
                "cognitive_lease_timeout_secs must be >= 3".into(),
            ));
        }
        if self.tool_tick_interval.is_zero() {
            return Err(ExoError::Config("tool_tick_interval must be > 0".into()));
        }
        if self.tool_tick_interval > Duration::from_secs(60) {
            return Err(ExoError::Config("tool_tick_interval must be <= 60s".into()));
        }
        if self.shutdown_timeout.is_zero() {
            return Err(ExoError::Config("shutdown_timeout must be > 0".into()));
        }

        // LLM config validation
        if self.llm_config.timeout_secs < 1 {
            return Err(ExoError::Config("llm timeout_secs must be >= 1".into()));
        }
        if self.llm_config.timeout_secs >= self.cognitive_lease_timeout_secs {
            return Err(ExoError::Config(
                "llm timeout_secs must be < cognitive_lease_timeout_secs".into(),
            ));
        }

        if self.llm_config.default_backend == LlmBackend::Local && self.llm_config.local.is_none() {
            tracing::warn!("default_backend is Local but no local model configured");
        }
        if self.llm_config.default_backend == LlmBackend::Frontier
            && self.llm_config.frontier.is_none()
        {
            tracing::warn!("default_backend is Frontier but no frontier model configured");
        }

        // Budget config validation (Sprint 9)
        if let Some(ref cb) = self.cognitive_budget {
            cb.validate()?;
        }
        if let Some(ref tb) = self.tool_budget {
            tb.validate()?;
        }

        // Source repo validation (E2-S2)
        for repo in &self.source_repos {
            repo.validate()?;
        }

        // Master loop config validation
        if self.master_loop_interval_secs < 1 {
            return Err(ExoError::Config(
                "master_loop_interval_secs must be >= 1".into(),
            ));
        }
        if self.master_loop_interval_secs >= self.cognitive_lease_timeout_secs {
            return Err(ExoError::Config(
                "master_loop_interval_secs must be < cognitive_lease_timeout_secs".into(),
            ));
        }

        Ok(())
    }

    /// Generate a vessel.toml file for a forked vessel.
    ///
    /// Creates a `VesselConfigFile` from this config, overriding the vessel_id,
    /// data_dir, and optionally the mission. Writes the TOML to
    /// `{data_dir}/vessel.toml` and returns the path.
    pub fn generate_fork_config(
        &self,
        new_vessel_id: VesselId,
        new_data_dir: &Path,
        mission_override: Option<&str>,
    ) -> Result<PathBuf, ExoError> {
        let config_file = VesselConfigFile {
            vessel: VesselSection {
                vessel_id: Some(new_vessel_id.to_string()),
                mission: mission_override.unwrap_or(&self.mission).to_string(),
                data_dir: new_data_dir.to_path_buf(),
                master_loop_interval_secs: self.master_loop_interval_secs,
                inbox_dir: None,
                trust_decay_rate: self.trust_decay.as_ref().map(|c| c.decay_rate),
                trust_decay_min_inactivity_days: self
                    .trust_decay
                    .as_ref()
                    .map(|c| c.min_inactivity_days)
                    .unwrap_or(7),
                episodic_memory_capacity: self.episodic_memory_capacity.unwrap_or(200),
                bootstrap_grace_period_ticks: self.bootstrap_grace_period_ticks,
                observatory_url: self.observatory_url.clone(),
                observatory_token_env: self.observatory_token_env.clone(),
                max_decide_turns: self.max_decide_turns,
                max_watches: self.max_watches,
            },
            cognitive: CognitiveSection {
                tick_interval_ms: self.cognitive_tick_interval.as_millis() as u64,
                dispatch_concurrency: self.cognitive_dispatch_concurrency.get(),
                lease_timeout_secs: self.cognitive_lease_timeout_secs,
                budget: self.cognitive_budget.clone(),
            },
            tool: ToolSection {
                tick_interval_ms: self.tool_tick_interval.as_millis() as u64,
                dispatch_concurrency: self.tool_dispatch_concurrency.get(),
                budget: self.tool_budget.clone(),
            },
            llm: self.llm_config.clone(),
            daemon: None,
            threads: None,
            source: if self.source_repos.is_empty() {
                None
            } else {
                Some(SourceSection {
                    repos: self.source_repos.clone(),
                })
            },
            sandbox: Some(self.sandbox.clone()),
            connectors: if self.extra_destructive_tools.is_empty() && self.connectors_dir.is_none()
            {
                None
            } else {
                Some(ConnectorsSection {
                    dir: self.connectors_dir.clone(),
                    destructive: self.extra_destructive_tools.clone(),
                })
            },
            inner_loop: if self.inner_loop.enabled {
                Some(self.inner_loop.clone())
            } else {
                None
            },
            tool_policy: if self.tool_policy == ToolPolicyConfig::default() {
                None
            } else {
                Some(self.tool_policy.clone())
            },
        };

        let toml_str = toml::to_string_pretty(&config_file)
            .map_err(|e| ExoError::Config(format!("failed to serialize fork config: {e}")))?;

        let config_path = new_data_dir.join("vessel.toml");
        std::fs::write(&config_path, &toml_str).map_err(|e| {
            ExoError::Storage(format!(
                "failed to write fork config to {}: {e}",
                config_path.display()
            ))
        })?;

        Ok(config_path)
    }

    /// Load configuration from a TOML file, applying environment variable overrides.
    ///
    /// Env vars (all optional, override TOML values):
    /// - `EXO_DATA_DIR` -> `data_dir`
    /// - `EXO_MISSION` -> `mission`
    /// - `EXO_VESSEL_ID` -> `vessel_id` (UUID string)
    /// - `EXO_COGNITIVE_TICK_INTERVAL_MS` -> `cognitive_tick_interval`
    /// - `EXO_COGNITIVE_DISPATCH_CONCURRENCY` -> `cognitive_dispatch_concurrency`
    /// - `EXO_TOOL_TICK_INTERVAL_MS` -> `tool_tick_interval`
    /// - `EXO_TOOL_DISPATCH_CONCURRENCY` -> `tool_dispatch_concurrency`
    /// - `EXO_DAEMON_LISTEN` -> `daemon_listen` (host:port)
    /// - `EXO_CORS_ORIGINS` -> `cors_allowed_origins` (comma-separated origins)
    /// - `EXO_OBSERVATORY_URL` -> `observatory_url`
    /// - `EXO_CONNECTORS_DIR` -> `connectors_dir` (path to WASM connectors)
    pub fn from_file(path: &Path) -> Result<Self, ExoError> {
        let toml_str = std::fs::read_to_string(path)
            .map_err(|e| ExoError::Config(format!("failed to read config file: {e}")))?;
        let file: VesselConfigFile = toml::from_str(&toml_str)
            .map_err(|e| ExoError::Config(format!("invalid TOML: {e}")))?;
        let mut config = Self::try_from(file)?;
        config.apply_env_overrides()?;
        config.validate()?;
        Ok(config)
    }

    /// Apply environment variable overrides to an already-constructed config.
    fn apply_env_overrides(&mut self) -> Result<(), ExoError> {
        if let Ok(val) = std::env::var("EXO_DATA_DIR") {
            self.data_dir = PathBuf::from(val);
        }
        if let Ok(val) = std::env::var("EXO_MISSION") {
            self.mission = val;
        }
        if let Ok(val) = std::env::var("EXO_VESSEL_ID") {
            self.vessel_id = val
                .parse()
                .map_err(|e| ExoError::Config(format!("invalid EXO_VESSEL_ID: {e}")))?;
        }
        if let Ok(val) = std::env::var("EXO_COGNITIVE_TICK_INTERVAL_MS") {
            let ms: u64 = val.parse().map_err(|e| {
                ExoError::Config(format!("invalid EXO_COGNITIVE_TICK_INTERVAL_MS: {e}"))
            })?;
            self.cognitive_tick_interval = Duration::from_millis(ms);
        }
        if let Ok(val) = std::env::var("EXO_COGNITIVE_DISPATCH_CONCURRENCY") {
            let n: usize = val.parse().map_err(|e| {
                ExoError::Config(format!("invalid EXO_COGNITIVE_DISPATCH_CONCURRENCY: {e}"))
            })?;
            self.cognitive_dispatch_concurrency = NonZeroUsize::new(n).ok_or_else(|| {
                ExoError::Config("EXO_COGNITIVE_DISPATCH_CONCURRENCY must be > 0".into())
            })?;
        }
        if let Ok(val) = std::env::var("EXO_TOOL_TICK_INTERVAL_MS") {
            let ms: u64 = val
                .parse()
                .map_err(|e| ExoError::Config(format!("invalid EXO_TOOL_TICK_INTERVAL_MS: {e}")))?;
            self.tool_tick_interval = Duration::from_millis(ms);
        }
        if let Ok(val) = std::env::var("EXO_TOOL_DISPATCH_CONCURRENCY") {
            let n: usize = val.parse().map_err(|e| {
                ExoError::Config(format!("invalid EXO_TOOL_DISPATCH_CONCURRENCY: {e}"))
            })?;
            self.tool_dispatch_concurrency = NonZeroUsize::new(n).ok_or_else(|| {
                ExoError::Config("EXO_TOOL_DISPATCH_CONCURRENCY must be > 0".into())
            })?;
        }
        if let Ok(val) = std::env::var("EXO_DAEMON_LISTEN") {
            self.daemon_listen = Some(
                val.parse()
                    .map_err(|e| ExoError::Config(format!("invalid EXO_DAEMON_LISTEN: {e}")))?,
            );
        }
        if let Ok(val) = std::env::var("EXO_CORS_ORIGINS") {
            self.cors_allowed_origins = val
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Ok(val) = std::env::var("EXO_BOOTSTRAP_GRACE_PERIOD") {
            if let Ok(ticks) = val.parse::<u64>() {
                self.bootstrap_grace_period_ticks = ticks;
            }
        }
        if let Ok(val) = std::env::var("EXO_SANDBOX_ENABLED") {
            match val.to_lowercase().as_str() {
                "false" | "0" | "no" => self.sandbox.enabled = false,
                _ => {}
            }
        }
        if let Ok(val) = std::env::var("EXO_OBSERVATORY_URL") {
            self.observatory_url = Some(val);
        }
        if let Ok(val) = std::env::var("EXO_CONNECTORS_DIR") {
            self.connectors_dir = Some(PathBuf::from(val));
        }
        if let Ok(val) = std::env::var("EXO_INNER_LOOP_ENABLED") {
            match val.to_lowercase().as_str() {
                "true" | "1" | "yes" => self.inner_loop.enabled = true,
                "false" | "0" | "no" => self.inner_loop.enabled = false,
                _ => {}
            }
        }
        Ok(())
    }

    /// Build `ThreadConfigOverrides` from the `[threads]` TOML section.
    ///
    /// Returns `None` if no threads section is configured.
    pub fn build_thread_overrides(&self) -> Option<exoskeleton_threads::ThreadConfigOverrides> {
        let ts = self.threads.as_ref()?;
        Some(exoskeleton_threads::ThreadConfigOverrides {
            threat_monitor_schedule: if matches!(ts.threat_monitor_enabled, Some(false)) {
                Some(exoskeleton_core::ThreadSchedule::OnDemand)
            } else {
                ts.threat_monitor_schedule
                    .as_deref()
                    .and_then(parse_thread_schedule)
            },
            threat_monitor_token_budget: ts.threat_monitor_token_budget,
            self_critique_schedule: if matches!(ts.self_critique_enabled, Some(false)) {
                Some(exoskeleton_core::ThreadSchedule::OnDemand)
            } else {
                ts.self_critique_schedule
                    .as_deref()
                    .and_then(parse_thread_schedule)
            },
            self_critique_token_budget: ts.self_critique_token_budget,
            memory_consolidation_schedule: if matches!(ts.memory_consolidation_enabled, Some(false))
            {
                Some(exoskeleton_core::ThreadSchedule::OnDemand)
            } else {
                ts.memory_consolidation_schedule
                    .as_deref()
                    .and_then(parse_thread_schedule)
            },
            memory_consolidation_token_budget: ts.memory_consolidation_token_budget,
            meta_cognition_schedule: if matches!(ts.meta_cognition_enabled, Some(false)) {
                Some(exoskeleton_core::ThreadSchedule::OnDemand)
            } else {
                ts.meta_cognition_schedule
                    .as_deref()
                    .and_then(parse_thread_schedule)
            },
            meta_cognition_token_budget: ts.meta_cognition_token_budget,
            creative_synthesis_schedule: if matches!(ts.creative_synthesis_enabled, Some(false)) {
                Some(exoskeleton_core::ThreadSchedule::OnDemand)
            } else {
                ts.creative_synthesis_schedule
                    .as_deref()
                    .and_then(parse_thread_schedule)
            },
            creative_synthesis_token_budget: ts.creative_synthesis_token_budget,
            initiative_schedule: if matches!(ts.initiative_enabled, Some(false)) {
                Some(exoskeleton_core::ThreadSchedule::OnDemand)
            } else {
                ts.initiative_schedule
                    .as_deref()
                    .and_then(parse_thread_schedule)
            },
            initiative_token_budget: ts.initiative_token_budget,
        })
    }
}

// ── TOML deserialization types ──

fn default_60() -> u64 {
    60
}
fn default_100() -> u64 {
    100
}
fn default_50() -> u64 {
    50
}
fn default_4() -> usize {
    4
}
fn default_7() -> u64 {
    7
}
fn default_200() -> u64 {
    200
}
fn default_30() -> u64 {
    30
}
fn default_600() -> u64 {
    600
}
fn default_5() -> u32 {
    5
}

/// TOML-friendly configuration file format.
///
/// All Duration values are represented as integer milliseconds.
/// All NonZeroUsize values are represented as plain usize (validated on conversion).
/// Optional fields use serde defaults.
#[derive(Debug, Serialize, Deserialize)]
pub struct VesselConfigFile {
    /// `[vessel]` section.
    pub vessel: VesselSection,
    /// `[cognitive]` section. Uses defaults if omitted.
    #[serde(default)]
    pub cognitive: CognitiveSection,
    /// `[tool]` section. Uses defaults if omitted.
    #[serde(default)]
    pub tool: ToolSection,
    /// `[llm]` section. Uses defaults if omitted.
    #[serde(default)]
    pub llm: LlmConfig,
    /// `[daemon]` section. Uses defaults if omitted.
    #[serde(default)]
    pub daemon: Option<DaemonSection>,
    /// `[threads]` section. Uses defaults if omitted.
    #[serde(default)]
    pub threads: Option<ThreadsSection>,
    /// `[source]` section — source code repository mounts.
    #[serde(default)]
    pub source: Option<SourceSection>,
    /// `[sandbox]` section — sandbox execution config.
    #[serde(default)]
    pub sandbox: Option<SandboxConfig>,
    /// `[connectors]` section — runtime connector configuration.
    #[serde(default)]
    pub connectors: Option<ConnectorsSection>,
    /// `[inner_loop]` section — inner loop for interactive coding sessions.
    #[serde(default)]
    pub inner_loop: Option<InnerLoopConfig>,
    /// `[tool_policy]` section — per-tool allow/deny/ask rules.
    #[serde(default)]
    pub tool_policy: Option<ToolPolicyConfig>,
}

/// The `[daemon]` section of the TOML config file.
#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonSection {
    /// Listen address as "host:port" string. Default: 127.0.0.1:7600.
    pub listen: Option<String>,
    /// CORS allowed origins. Empty or omitted disables CORS.
    #[serde(default)]
    pub cors_allowed_origins: Vec<String>,
}

/// The `[vessel]` section of the TOML config file.
#[derive(Debug, Serialize, Deserialize)]
pub struct VesselSection {
    /// UUID string. Generated if omitted.
    pub vessel_id: Option<String>,
    /// The vessel's primary objective.
    pub mission: String,
    /// Root data directory.
    pub data_dir: PathBuf,
    /// Master loop interval in seconds. Default: 60.
    #[serde(default = "default_60")]
    pub master_loop_interval_secs: u64,
    /// Directory for the file-based inbox. Default: {data_dir}/inbox/
    #[serde(default)]
    pub inbox_dir: Option<PathBuf>,
    /// Trust decay rate per day of inactivity. `None` or omitted disables decay.
    #[serde(default)]
    pub trust_decay_rate: Option<f64>,
    /// Minimum inactive days before trust decay begins. Default: 7.
    #[serde(default = "default_7")]
    pub trust_decay_min_inactivity_days: u64,
    /// Episodic memory capacity (max summaries). Default: 200. Set to 0 to disable.
    #[serde(default = "default_200")]
    pub episodic_memory_capacity: u64,
    /// Bootstrap grace period in ticks. During this window, Threat Monitor
    /// and Self-Critique threads receive additional context indicating that
    /// early self-referential patterns are expected. Default: 30.
    #[serde(default = "default_30")]
    pub bootstrap_grace_period_ticks: u64,
    /// Observatory base URL for fleet discovery. Omit to disable peer communication.
    #[serde(default)]
    pub observatory_url: Option<String>,
    /// Environment variable name holding the Observatory API bearer token.
    #[serde(default)]
    pub observatory_token_env: Option<String>,
    /// Maximum number of LLM turns in the Decide step. Default: 5.
    #[serde(default = "default_5")]
    pub max_decide_turns: u32,
    /// Maximum number of active watches per vessel. Default: 20.
    #[serde(default = "default_20")]
    pub max_watches: u32,
}

fn default_20() -> u32 {
    20
}

/// The `[cognitive]` section of the TOML config file.
#[derive(Debug, Serialize, Deserialize)]
pub struct CognitiveSection {
    /// Tick interval in milliseconds. Default: 100.
    #[serde(default = "default_100")]
    pub tick_interval_ms: u64,
    /// Worker count. Default: 4.
    #[serde(default = "default_4")]
    pub dispatch_concurrency: usize,
    /// Lease timeout in seconds. Default: 600.
    #[serde(default = "default_600")]
    pub lease_timeout_secs: u64,
    /// `[cognitive.budget]` section. `None` disables budget enforcement.
    #[serde(default)]
    pub budget: Option<exoskeleton_core::CognitiveBudgetConfig>,
}

impl Default for CognitiveSection {
    fn default() -> Self {
        Self {
            tick_interval_ms: 100,
            dispatch_concurrency: 4,
            lease_timeout_secs: 600,
            budget: None,
        }
    }
}

/// The `[tool]` section of the TOML config file.
#[derive(Debug, Serialize, Deserialize)]
pub struct ToolSection {
    /// Tick interval in milliseconds. Default: 50.
    #[serde(default = "default_50")]
    pub tick_interval_ms: u64,
    /// Worker count. Default: 4.
    #[serde(default = "default_4")]
    pub dispatch_concurrency: usize,
    /// `[tool.budget]` section. `None` disables tool rate limiting.
    #[serde(default)]
    pub budget: Option<exoskeleton_core::ToolBudgetConfig>,
}

impl Default for ToolSection {
    fn default() -> Self {
        Self {
            tick_interval_ms: 50,
            dispatch_concurrency: 4,
            budget: None,
        }
    }
}

/// The `[threads]` section of the TOML config file.
///
/// All fields are optional — omitted values use the compiled-in defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThreadsSection {
    /// Threat Monitor enabled flag. `false` maps to on_demand schedule.
    #[serde(default)]
    pub threat_monitor_enabled: Option<bool>,
    /// Self-Critique enabled flag. `false` maps to on_demand schedule.
    #[serde(default)]
    pub self_critique_enabled: Option<bool>,
    /// Memory Consolidation enabled flag. `false` maps to on_demand schedule.
    #[serde(default)]
    pub memory_consolidation_enabled: Option<bool>,
    /// Meta-Cognition enabled flag. `false` maps to on_demand schedule.
    #[serde(default)]
    pub meta_cognition_enabled: Option<bool>,
    /// Creative Synthesis enabled flag. `false` maps to on_demand schedule.
    #[serde(default)]
    pub creative_synthesis_enabled: Option<bool>,
    /// Initiative enabled flag. `false` maps to on_demand schedule.
    #[serde(default)]
    pub initiative_enabled: Option<bool>,
    /// Threat Monitor schedule override. Options: "every_tick", "every_N" (e.g., "every_5"),
    /// "on_demand". Default: "every_tick".
    #[serde(default)]
    pub threat_monitor_schedule: Option<String>,
    /// Self-Critique schedule override. Default: "every_tick".
    #[serde(default)]
    pub self_critique_schedule: Option<String>,
    /// Memory Consolidation schedule override. Default: "every_5".
    #[serde(default)]
    pub memory_consolidation_schedule: Option<String>,
    /// Threat Monitor token budget override. Default: 4096.
    #[serde(default)]
    pub threat_monitor_token_budget: Option<u64>,
    /// Self-Critique token budget override. Default: 4096.
    #[serde(default)]
    pub self_critique_token_budget: Option<u64>,
    /// Memory Consolidation token budget override. Default: 6144.
    #[serde(default)]
    pub memory_consolidation_token_budget: Option<u64>,
    /// Meta-Cognition schedule override. Default: "every_10".
    #[serde(default)]
    pub meta_cognition_schedule: Option<String>,
    /// Meta-Cognition token budget override. Default: 8192.
    #[serde(default)]
    pub meta_cognition_token_budget: Option<u64>,
    /// Creative Synthesis schedule override. Default: "every_15".
    #[serde(default)]
    pub creative_synthesis_schedule: Option<String>,
    /// Creative Synthesis token budget override. Default: 8192.
    #[serde(default)]
    pub creative_synthesis_token_budget: Option<u64>,
    /// Initiative schedule override. Default: "every_10".
    #[serde(default)]
    pub initiative_schedule: Option<String>,
    /// Initiative token budget override. Default: 8192.
    #[serde(default)]
    pub initiative_token_budget: Option<u64>,
}

/// Configuration for a source code repository to mount in the vessel container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRepoConfig {
    /// Display name for the repo (used as the mount point suffix: /workspace/{name}).
    pub name: String,
    /// Absolute path to the repository on the host filesystem.
    pub host_path: PathBuf,
    /// Mount point inside the vessel container.
    /// Default: /workspace/{name}
    #[serde(default)]
    pub mount_point: Option<String>,
}

impl SourceRepoConfig {
    /// Returns the effective mount point inside the container.
    pub fn effective_mount_point(&self) -> String {
        self.mount_point
            .clone()
            .unwrap_or_else(|| format!("/workspace/{}", self.name))
    }

    /// Validate the config. Returns error if host_path is not absolute.
    pub fn validate(&self) -> Result<(), ExoError> {
        if self.name.is_empty() {
            return Err(ExoError::Config(
                "source repo name must not be empty".into(),
            ));
        }
        if !self.host_path.is_absolute() {
            return Err(ExoError::Config(format!(
                "source repo '{}' host_path must be absolute, got '{}'",
                self.name,
                self.host_path.display()
            )));
        }
        // Mount point must start with /workspace/ if specified
        if let Some(ref mp) = self.mount_point {
            if !mp.starts_with("/workspace/") {
                return Err(ExoError::Config(format!(
                    "source repo '{}' mount_point must start with /workspace/, got '{}'",
                    self.name, mp
                )));
            }
        }
        Ok(())
    }
}

/// The `[source]` section of the TOML config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSection {
    /// Source repositories to mount read-only in the vessel container.
    #[serde(default)]
    pub repos: Vec<SourceRepoConfig>,
}

/// Configuration for the sandbox execution environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Whether sandbox.exec is available. Default: true.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Tmpfs size for /sandbox mount (in bytes). Default: 256MB.
    /// Only used by Observatory when creating the vessel container.
    #[serde(default = "default_sandbox_tmpfs_size")]
    pub tmpfs_size_bytes: u64,
}

fn default_true() -> bool {
    true
}

fn default_sandbox_tmpfs_size() -> u64 {
    268_435_456 // 256MB
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            tmpfs_size_bytes: default_sandbox_tmpfs_size(),
        }
    }
}

/// The `[connectors]` section of the TOML config file.
#[derive(Debug, Serialize, Deserialize)]
pub struct ConnectorsSection {
    /// Directory containing pre-compiled WASM connectors (.wasm + .connector.toml).
    /// Scanned at boot; supports hot-reload via daemon API.
    #[serde(default)]
    pub dir: Option<PathBuf>,
    /// Additional tool names classified as destructive beyond AlignConfig defaults.
    #[serde(default)]
    pub destructive: Vec<String>,
}

/// Parse a thread schedule string into a `ThreadSchedule`.
///
/// Accepts: "every_tick", "on_demand", "every_N" (e.g., "every_5").
pub fn parse_thread_schedule(s: &str) -> Option<exoskeleton_core::ThreadSchedule> {
    match s.trim().to_lowercase().as_str() {
        "every_tick" => Some(exoskeleton_core::ThreadSchedule::EveryTick),
        "on_demand" => Some(exoskeleton_core::ThreadSchedule::OnDemand),
        s if s.starts_with("every_") => s
            .strip_prefix("every_")
            .and_then(|n| n.parse::<u32>().ok())
            .map(exoskeleton_core::ThreadSchedule::EveryNTicks),
        _ => None,
    }
}

impl TryFrom<VesselConfigFile> for VesselConfig {
    type Error = ExoError;

    fn try_from(file: VesselConfigFile) -> Result<Self, Self::Error> {
        let vessel_id = match file.vessel.vessel_id {
            Some(s) => s
                .parse::<VesselId>()
                .map_err(|e| ExoError::Config(format!("invalid vessel_id: {e}")))?,
            None => VesselId::new(),
        };

        let cognitive_dispatch_concurrency = NonZeroUsize::new(file.cognitive.dispatch_concurrency)
            .ok_or_else(|| ExoError::Config("cognitive.dispatch_concurrency must be > 0".into()))?;

        let tool_dispatch_concurrency = NonZeroUsize::new(file.tool.dispatch_concurrency)
            .ok_or_else(|| ExoError::Config("tool.dispatch_concurrency must be > 0".into()))?;

        Ok(VesselConfig {
            vessel_id,
            data_dir: file.vessel.data_dir,
            mission: file.vessel.mission,
            cognitive_tick_interval: Duration::from_millis(file.cognitive.tick_interval_ms),
            cognitive_dispatch_concurrency,
            cognitive_lease_timeout_secs: file.cognitive.lease_timeout_secs,
            tool_tick_interval: Duration::from_millis(file.tool.tick_interval_ms),
            tool_dispatch_concurrency,
            shutdown_timeout: Duration::from_secs(30),
            llm_config: file.llm,
            master_loop_interval_secs: file.vessel.master_loop_interval_secs,
            inbox_dir: file.vessel.inbox_dir,
            cognitive_budget: file.cognitive.budget,
            tool_budget: file.tool.budget,
            daemon_listen: file
                .daemon
                .as_ref()
                .and_then(|d| d.listen.as_ref())
                .map(|s| {
                    s.parse::<std::net::SocketAddr>().map_err(|e| {
                        ExoError::Config(format!("invalid daemon listen address: {e}"))
                    })
                })
                .transpose()?,
            cors_allowed_origins: file
                .daemon
                .map(|d| d.cors_allowed_origins)
                .unwrap_or_default(),
            trust_decay: file.vessel.trust_decay_rate.map(|rate| {
                exoskeleton_core::TrustDecayConfig {
                    decay_rate: rate,
                    baseline: 0.5,
                    min_inactivity_days: file.vessel.trust_decay_min_inactivity_days,
                }
            }),
            episodic_memory_capacity: if file.vessel.episodic_memory_capacity == 0 {
                None
            } else {
                Some(file.vessel.episodic_memory_capacity)
            },
            bootstrap_grace_period_ticks: file.vessel.bootstrap_grace_period_ticks,
            threads: file.threads,
            source_repos: file.source.map(|s| s.repos).unwrap_or_default(),
            sandbox: file.sandbox.unwrap_or_default(),
            observatory_url: file.vessel.observatory_url,
            observatory_token_env: file.vessel.observatory_token_env,
            max_decide_turns: file.vessel.max_decide_turns.max(1),
            max_watches: file.vessel.max_watches.min(100),
            extra_destructive_tools: file
                .connectors
                .as_ref()
                .map(|c| c.destructive.clone())
                .unwrap_or_default(),
            connectors_dir: file.connectors.as_ref().and_then(|c| c.dir.clone()),
            inner_loop: file.inner_loop.unwrap_or_default(),
            tool_policy: file.tool_policy.unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T-1: VesselConfig Validation ──

    #[test]
    fn config_default_has_expected_values() {
        let config = VesselConfig::default();
        assert_eq!(config.cognitive_tick_interval, Duration::from_millis(100));
        assert_eq!(config.cognitive_dispatch_concurrency.get(), 4);
        assert_eq!(config.cognitive_lease_timeout_secs, 600);
        assert_eq!(config.tool_tick_interval, Duration::from_millis(50));
        assert_eq!(config.tool_dispatch_concurrency.get(), 4);
        assert_eq!(config.shutdown_timeout, Duration::from_secs(30));
    }

    #[test]
    fn config_validate_rejects_empty_mission() {
        let config = VesselConfig::default();
        assert!(matches!(config.validate(), Err(ExoError::Config(_))));
    }

    #[test]
    fn config_validate_rejects_zero_cognitive_tick_interval() {
        let config = VesselConfig {
            mission: "test".into(),
            cognitive_tick_interval: Duration::ZERO,
            ..Default::default()
        };
        assert!(matches!(config.validate(), Err(ExoError::Config(_))));
    }

    #[test]
    fn config_validate_rejects_excessive_cognitive_tick_interval() {
        let config = VesselConfig {
            mission: "test".into(),
            cognitive_tick_interval: Duration::from_secs(61),
            ..Default::default()
        };
        assert!(matches!(config.validate(), Err(ExoError::Config(_))));
    }

    #[test]
    fn config_validate_rejects_zero_tool_tick_interval() {
        let config = VesselConfig {
            mission: "test".into(),
            tool_tick_interval: Duration::ZERO,
            ..Default::default()
        };
        assert!(matches!(config.validate(), Err(ExoError::Config(_))));
    }

    #[test]
    fn config_validate_rejects_low_lease_timeout() {
        let config = VesselConfig {
            mission: "test".into(),
            cognitive_lease_timeout_secs: 2,
            ..Default::default()
        };
        assert!(matches!(config.validate(), Err(ExoError::Config(_))));
    }

    #[test]
    fn config_validate_rejects_zero_shutdown_timeout() {
        let config = VesselConfig {
            mission: "test".into(),
            shutdown_timeout: Duration::ZERO,
            ..Default::default()
        };
        assert!(matches!(config.validate(), Err(ExoError::Config(_))));
    }

    #[test]
    fn config_validate_accepts_valid() {
        let config = VesselConfig {
            mission: "valid mission".into(),
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    // ── T-2: TOML Config Loading ──

    #[test]
    fn toml_full_config_parses() {
        let toml_str = r#"
[vessel]
vessel_id = "550e8400-e29b-41d4-a716-446655440000"
mission = "test mission"
data_dir = "/tmp/exo"

[cognitive]
tick_interval_ms = 200
dispatch_concurrency = 8
lease_timeout_secs = 300

[tool]
tick_interval_ms = 25
dispatch_concurrency = 2

[llm]
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert_eq!(config.mission, "test mission");
        assert_eq!(config.data_dir, PathBuf::from("/tmp/exo"));
        assert_eq!(config.cognitive_tick_interval, Duration::from_millis(200));
        assert_eq!(config.cognitive_dispatch_concurrency.get(), 8);
        assert_eq!(config.cognitive_lease_timeout_secs, 300);
        assert_eq!(config.tool_tick_interval, Duration::from_millis(25));
        assert_eq!(config.tool_dispatch_concurrency.get(), 2);
    }

    #[test]
    fn toml_minimal_config_uses_defaults() {
        let toml_str = r#"
[vessel]
mission = "minimal"
data_dir = "/tmp/exo"
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert_eq!(config.mission, "minimal");
        assert_eq!(config.cognitive_tick_interval, Duration::from_millis(100));
        assert_eq!(config.cognitive_dispatch_concurrency.get(), 4);
        assert_eq!(config.cognitive_lease_timeout_secs, 600);
        assert_eq!(config.tool_tick_interval, Duration::from_millis(50));
        assert_eq!(config.tool_dispatch_concurrency.get(), 4);
    }

    #[test]
    fn toml_invalid_toml_returns_error() {
        let result = toml::from_str::<VesselConfigFile>("not valid toml {");
        assert!(result.is_err());
    }

    #[test]
    fn toml_missing_mission_returns_error() {
        let toml_str = r#"
[vessel]
data_dir = "/tmp/exo"
"#;
        let result = toml::from_str::<VesselConfigFile>(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn toml_zero_concurrency_returns_error() {
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"

[cognitive]
dispatch_concurrency = 0
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let result = VesselConfig::try_from(file);
        assert!(matches!(result, Err(ExoError::Config(_))));
    }

    #[test]
    fn toml_vessel_id_parsed_when_present() {
        let toml_str = r#"
[vessel]
vessel_id = "550e8400-e29b-41d4-a716-446655440000"
mission = "test"
data_dir = "/tmp/exo"
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert_eq!(
            config.vessel_id.to_string(),
            "550e8400-e29b-41d4-a716-446655440000"
        );
    }

    #[test]
    fn toml_vessel_id_generated_when_absent() {
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        // Generated ID should be a non-nil UUID
        assert_ne!(
            config.vessel_id.to_string(),
            "00000000-0000-0000-0000-000000000000"
        );
    }

    #[test]
    fn toml_invalid_vessel_id_returns_error() {
        let toml_str = r#"
[vessel]
vessel_id = "not-a-uuid"
mission = "test"
data_dir = "/tmp/exo"
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let result = VesselConfig::try_from(file);
        assert!(matches!(result, Err(ExoError::Config(_))));
    }

    // ── T-2 (Sprint 4): LLM Config Types ──

    #[test]
    fn llm_config_default() {
        let config = LlmConfig::default();
        assert!(config.local.is_none());
        assert!(config.frontier.is_none());
        assert_eq!(config.default_backend, LlmBackend::Local);
        assert_eq!(config.max_output_tokens, 4096);
        assert_eq!(config.timeout_secs, 120);
    }

    #[test]
    fn llm_config_roundtrip() {
        let config = LlmConfig {
            local: Some(LocalModelConfig {
                endpoint: "http://localhost:11434".into(),
                model: "llama3.2:latest".into(),
                api_format: LocalApiFormat::OpenAICompat,
            }),
            frontier: Some(FrontierModelConfig {
                provider: FrontierProvider::Anthropic,
                model: "claude-sonnet-4-20250514".into(),
                api_key_env: "ANTHROPIC_API_KEY".into(),
                endpoint: None,
            }),
            default_backend: LlmBackend::Local,
            max_output_tokens: 8192,
            timeout_secs: 60,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: LlmConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn local_model_config_roundtrip() {
        let config = LocalModelConfig {
            endpoint: "http://localhost:8080".into(),
            model: "qwen2.5-coder:7b".into(),
            api_format: LocalApiFormat::Ollama,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: LocalModelConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn local_api_format_roundtrip() {
        for fmt in [LocalApiFormat::OpenAICompat, LocalApiFormat::Ollama] {
            let json = serde_json::to_string(&fmt).unwrap();
            let parsed: LocalApiFormat = serde_json::from_str(&json).unwrap();
            assert_eq!(fmt, parsed);
        }
    }

    #[test]
    fn frontier_model_config_roundtrip() {
        let config = FrontierModelConfig {
            provider: FrontierProvider::OpenAI,
            model: "gpt-4o".into(),
            api_key_env: "OPENAI_API_KEY".into(),
            endpoint: Some("https://custom.api.com".into()),
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: FrontierModelConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn frontier_provider_roundtrip() {
        for provider in [FrontierProvider::Anthropic, FrontierProvider::OpenAI] {
            let json = serde_json::to_string(&provider).unwrap();
            let parsed: FrontierProvider = serde_json::from_str(&json).unwrap();
            assert_eq!(provider, parsed);
        }
    }

    #[test]
    fn toml_full_llm_config() {
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"

[llm]
default_backend = "local"
max_output_tokens = 8192
timeout_secs = 60

[llm.local]
endpoint = "http://localhost:11434"
model = "llama3.2:latest"
api_format = "openai_compat"

[llm.frontier]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
api_key_env = "ANTHROPIC_API_KEY"
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert_eq!(config.llm_config.default_backend, LlmBackend::Local);
        assert_eq!(config.llm_config.max_output_tokens, 8192);
        assert_eq!(config.llm_config.timeout_secs, 60);
        let local = config.llm_config.local.unwrap();
        assert_eq!(local.endpoint, "http://localhost:11434");
        assert_eq!(local.model, "llama3.2:latest");
        assert_eq!(local.api_format, LocalApiFormat::OpenAICompat);
        let frontier = config.llm_config.frontier.unwrap();
        assert_eq!(frontier.provider, FrontierProvider::Anthropic);
        assert_eq!(frontier.model, "claude-sonnet-4-20250514");
        assert_eq!(frontier.api_key_env, "ANTHROPIC_API_KEY");
    }

    #[test]
    fn toml_minimal_llm_config() {
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"

[llm]
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert_eq!(config.llm_config.default_backend, LlmBackend::Local);
        assert_eq!(config.llm_config.max_output_tokens, 4096);
        assert_eq!(config.llm_config.timeout_secs, 120);
    }

    #[test]
    fn toml_local_only() {
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"

[llm]
default_backend = "local"

[llm.local]
endpoint = "http://localhost:11434"
model = "llama3.2:latest"
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert!(config.llm_config.local.is_some());
        assert!(config.llm_config.frontier.is_none());
    }

    #[test]
    fn toml_frontier_only() {
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"

[llm]
default_backend = "frontier"

[llm.frontier]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
api_key_env = "ANTHROPIC_API_KEY"
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert!(config.llm_config.local.is_none());
        assert!(config.llm_config.frontier.is_some());
    }

    #[test]
    fn toml_no_llm_section() {
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert_eq!(config.llm_config.default_backend, LlmBackend::Local);
        assert_eq!(config.llm_config.max_output_tokens, 4096);
    }

    #[test]
    fn validate_timeout_bounds() {
        // timeout_secs must be >= 1
        let config = VesselConfig {
            mission: "test".into(),
            llm_config: LlmConfig {
                timeout_secs: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(config.validate(), Err(ExoError::Config(_))));

        // timeout_secs must be < cognitive_lease_timeout_secs
        let config = VesselConfig {
            mission: "test".into(),
            cognitive_lease_timeout_secs: 60,
            llm_config: LlmConfig {
                timeout_secs: 60,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(config.validate(), Err(ExoError::Config(_))));

        // Valid: timeout < lease timeout
        let config = VesselConfig {
            mission: "test".into(),
            cognitive_lease_timeout_secs: 600,
            llm_config: LlmConfig {
                timeout_secs: 120,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    // ── T-11: Master Loop Config ──

    #[test]
    fn config_master_loop_interval_default() {
        let config = VesselConfig::default();
        assert_eq!(config.master_loop_interval_secs, 60);
    }

    #[test]
    fn config_master_loop_interval_validation() {
        // < 1 rejected
        let config = VesselConfig {
            mission: "test".into(),
            master_loop_interval_secs: 0,
            ..Default::default()
        };
        assert!(
            matches!(config.validate(), Err(ExoError::Config(msg)) if msg.contains("master_loop_interval_secs must be >= 1"))
        );

        // >= lease_timeout rejected
        let config = VesselConfig {
            mission: "test".into(),
            master_loop_interval_secs: 600, // == cognitive_lease_timeout_secs (default 600)
            ..Default::default()
        };
        assert!(
            matches!(config.validate(), Err(ExoError::Config(msg)) if msg.contains("master_loop_interval_secs must be < cognitive_lease_timeout_secs"))
        );

        // > lease_timeout also rejected
        let config = VesselConfig {
            mission: "test".into(),
            master_loop_interval_secs: 601,
            ..Default::default()
        };
        assert!(
            matches!(config.validate(), Err(ExoError::Config(msg)) if msg.contains("master_loop_interval_secs must be < cognitive_lease_timeout_secs"))
        );

        // Valid: well under lease timeout
        let config = VesselConfig {
            mission: "test".into(),
            master_loop_interval_secs: 60,
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn config_inbox_dir_default() {
        let config = VesselConfig::default();
        assert!(config.inbox_dir.is_none());
    }

    #[test]
    fn config_roundtrip_with_new_fields() {
        let toml_str = r#"
[vessel]
mission = "test roundtrip"
data_dir = "/tmp/exo"
master_loop_interval_secs = 30
inbox_dir = "/tmp/exo/my-inbox"
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert_eq!(config.master_loop_interval_secs, 30);
        assert_eq!(config.inbox_dir, Some(PathBuf::from("/tmp/exo/my-inbox")));
    }

    #[test]
    fn config_roundtrip_new_fields_defaults() {
        // When master_loop fields are omitted, defaults are used
        let toml_str = r#"
[vessel]
mission = "test defaults"
data_dir = "/tmp/exo"
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert_eq!(config.master_loop_interval_secs, 60);
        assert!(config.inbox_dir.is_none());
    }

    // ── T-4 (Sprint 10): Daemon Config ──

    #[test]
    fn toml_daemon_section_parses() {
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"

[daemon]
listen = "0.0.0.0:7600"
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        let addr: std::net::SocketAddr = "0.0.0.0:7600".parse().unwrap();
        assert_eq!(config.daemon_listen, Some(addr));
    }

    #[test]
    fn toml_no_daemon_section_defaults_to_none() {
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert!(config.daemon_listen.is_none());
    }

    #[test]
    fn toml_invalid_daemon_listen_rejected() {
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"

[daemon]
listen = "not-a-socket-addr"
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let result = VesselConfig::try_from(file);
        assert!(matches!(result, Err(ExoError::Config(msg)) if msg.contains("daemon listen")));
    }

    #[test]
    fn daemon_listen_default_value() {
        let config = VesselConfig::default();
        assert!(config.daemon_listen.is_none());
    }

    // ── T-1..T-4 (D3): CORS Env Var ──
    //
    // All CORS env var tests in a single function to avoid parallel env var races.
    // std::env::set_var/remove_var are process-global and not thread-safe.

    #[test]
    fn cors_env_var_behavior() {
        // T-1: Parsed as comma-separated origins
        std::env::set_var(
            "EXO_CORS_ORIGINS",
            "http://localhost:3000,http://localhost:8080",
        );
        let mut config = VesselConfig {
            mission: "test".into(),
            ..Default::default()
        };
        config.apply_env_overrides().unwrap();
        assert_eq!(
            config.cors_allowed_origins,
            vec!["http://localhost:3000", "http://localhost:8080"],
            "T-1: comma-separated origins"
        );

        // T-1 (continued): Whitespace trimmed
        std::env::set_var("EXO_CORS_ORIGINS", " http://a:3000 , http://b:8080 ");
        let mut config = VesselConfig {
            mission: "test".into(),
            ..Default::default()
        };
        config.apply_env_overrides().unwrap();
        assert_eq!(
            config.cors_allowed_origins,
            vec!["http://a:3000", "http://b:8080"],
            "T-1: whitespace trimmed"
        );

        // T-2: Overrides TOML value
        std::env::set_var("EXO_CORS_ORIGINS", "http://new-origin:3000");
        let mut config = VesselConfig {
            mission: "test".into(),
            cors_allowed_origins: vec!["http://old-origin:9999".into()],
            ..Default::default()
        };
        config.apply_env_overrides().unwrap();
        assert_eq!(
            config.cors_allowed_origins,
            vec!["http://new-origin:3000"],
            "T-2: env var overrides TOML"
        );

        // T-4: Empty string clears origins
        std::env::set_var("EXO_CORS_ORIGINS", "");
        let mut config = VesselConfig {
            mission: "test".into(),
            cors_allowed_origins: vec!["http://will-be-cleared:5000".into()],
            ..Default::default()
        };
        config.apply_env_overrides().unwrap();
        assert!(
            config.cors_allowed_origins.is_empty(),
            "T-4: empty env var clears origins"
        );

        // T-3: Absent env var preserves TOML value (must be last — removes the var)
        std::env::remove_var("EXO_CORS_ORIGINS");
        let mut config = VesselConfig {
            mission: "test".into(),
            cors_allowed_origins: vec!["http://preserved:5000".into()],
            ..Default::default()
        };
        config.apply_env_overrides().unwrap();
        assert_eq!(
            config.cors_allowed_origins,
            vec!["http://preserved:5000"],
            "T-3: absent env var preserves TOML"
        );
    }

    // ── E3-S3: Fork Config Generation ──

    #[test]
    fn generate_fork_config_creates_valid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let source_config = VesselConfig {
            mission: "original mission".into(),
            llm_config: LlmConfig {
                local: Some(LocalModelConfig {
                    endpoint: "http://localhost:11434".into(),
                    model: "llama3.2:latest".into(),
                    api_format: LocalApiFormat::OpenAICompat,
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let new_id = VesselId::new();
        let path = source_config
            .generate_fork_config(new_id, dir.path(), None)
            .unwrap();

        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        let file: VesselConfigFile = toml::from_str(&content).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert_eq!(config.vessel_id, new_id);
        assert_eq!(config.mission, "original mission");
        assert!(config.llm_config.local.is_some());
        config.validate().unwrap();
    }

    #[test]
    fn generate_fork_config_applies_mission_override() {
        let dir = tempfile::tempdir().unwrap();
        let source_config = VesselConfig {
            mission: "original mission".into(),
            ..Default::default()
        };

        let new_id = VesselId::new();
        let path = source_config
            .generate_fork_config(new_id, dir.path(), Some("overridden mission"))
            .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let file: VesselConfigFile = toml::from_str(&content).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert_eq!(config.mission, "overridden mission");
        config.validate().unwrap();
    }

    #[test]
    fn generate_fork_config_roundtrip_preserves_settings() {
        let dir = tempfile::tempdir().unwrap();
        let source_config = VesselConfig {
            mission: "test".into(),
            cognitive_tick_interval: Duration::from_millis(200),
            cognitive_dispatch_concurrency: NonZeroUsize::new(8).unwrap(),
            cognitive_lease_timeout_secs: 300,
            tool_tick_interval: Duration::from_millis(25),
            tool_dispatch_concurrency: NonZeroUsize::new(2).unwrap(),
            master_loop_interval_secs: 30,
            ..Default::default()
        };

        let path = source_config
            .generate_fork_config(VesselId::new(), dir.path(), None)
            .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let file: VesselConfigFile = toml::from_str(&content).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert_eq!(config.cognitive_tick_interval, Duration::from_millis(200));
        assert_eq!(config.cognitive_dispatch_concurrency.get(), 8);
        assert_eq!(config.cognitive_lease_timeout_secs, 300);
        assert_eq!(config.tool_tick_interval, Duration::from_millis(25));
        assert_eq!(config.tool_dispatch_concurrency.get(), 2);
        assert_eq!(config.master_loop_interval_secs, 30);
    }

    // ── DC-T1..DC-T10: Decoherence Fix — Config Tests ──

    #[test]
    fn dc_t1_bootstrap_grace_period_default() {
        let config = VesselConfig::default();
        assert_eq!(config.bootstrap_grace_period_ticks, 30);
    }

    #[test]
    fn dc_t2_bootstrap_grace_period_from_toml() {
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"
bootstrap_grace_period_ticks = 50
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert_eq!(config.bootstrap_grace_period_ticks, 50);
    }

    #[test]
    fn dc_t3_bootstrap_grace_period_env_override() {
        // Use a single test to avoid env var race conditions
        std::env::set_var("EXO_BOOTSTRAP_GRACE_PERIOD", "10");
        let mut config = VesselConfig {
            mission: "test".into(),
            bootstrap_grace_period_ticks: 30,
            ..Default::default()
        };
        config.apply_env_overrides().unwrap();
        assert_eq!(config.bootstrap_grace_period_ticks, 10);
        std::env::remove_var("EXO_BOOTSTRAP_GRACE_PERIOD");
    }

    #[test]
    fn dc_t4_bootstrap_grace_period_zero_disables() {
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"
bootstrap_grace_period_ticks = 0
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert_eq!(config.bootstrap_grace_period_ticks, 0);
    }

    #[test]
    fn dc_t5_parse_thread_schedule_every_tick() {
        assert_eq!(
            parse_thread_schedule("every_tick"),
            Some(exoskeleton_core::ThreadSchedule::EveryTick)
        );
    }

    #[test]
    fn dc_t6_parse_thread_schedule_every_n() {
        assert_eq!(
            parse_thread_schedule("every_5"),
            Some(exoskeleton_core::ThreadSchedule::EveryNTicks(5))
        );
    }

    #[test]
    fn dc_t7_parse_thread_schedule_on_demand() {
        assert_eq!(
            parse_thread_schedule("on_demand"),
            Some(exoskeleton_core::ThreadSchedule::OnDemand)
        );
    }

    #[test]
    fn dc_t8_parse_thread_schedule_invalid() {
        assert_eq!(parse_thread_schedule("garbage"), None);
    }

    #[test]
    fn dc_t9_threads_section_from_toml() {
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"

[threads]
threat_monitor_schedule = "every_3"
self_critique_schedule = "every_tick"
memory_consolidation_schedule = "every_10"
threat_monitor_token_budget = 2048
self_critique_token_budget = 3000
memory_consolidation_token_budget = 8000
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        let ts = config.threads.unwrap();
        assert_eq!(ts.threat_monitor_schedule.as_deref(), Some("every_3"));
        assert_eq!(ts.self_critique_schedule.as_deref(), Some("every_tick"));
        assert_eq!(
            ts.memory_consolidation_schedule.as_deref(),
            Some("every_10")
        );
        assert_eq!(ts.threat_monitor_token_budget, Some(2048));
        assert_eq!(ts.self_critique_token_budget, Some(3000));
        assert_eq!(ts.memory_consolidation_token_budget, Some(8000));
    }

    #[test]
    fn dc_t10_threads_section_defaults_when_omitted() {
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert!(config.threads.is_none());
    }

    // ── E2S2-T21: Source repo config parses from TOML ──

    #[test]
    fn source_repo_config_parses_from_toml() {
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"

[source]
repos = [
    { name = "exoskeleton", host_path = "/home/keiths/src/exoskeleton" },
    { name = "worldinterface", host_path = "/home/keiths/src/worldinterface" },
]
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let source = file.source.unwrap();
        assert_eq!(source.repos.len(), 2);
        assert_eq!(source.repos[0].name, "exoskeleton");
        assert_eq!(
            source.repos[0].host_path,
            PathBuf::from("/home/keiths/src/exoskeleton")
        );
        assert_eq!(source.repos[1].name, "worldinterface");
    }

    // ── E2S2-T22: Source repo validates absolute path ──

    #[test]
    fn source_repo_config_validates_absolute_path() {
        let repo = SourceRepoConfig {
            name: "myrepo".into(),
            host_path: PathBuf::from("relative/path"),
            mount_point: None,
        };
        assert!(repo.validate().is_err());
    }

    // ── E2S2-T23: Source repo validates mount prefix ──

    #[test]
    fn source_repo_config_validates_mount_prefix() {
        let repo = SourceRepoConfig {
            name: "myrepo".into(),
            host_path: PathBuf::from("/home/user/src/myrepo"),
            mount_point: Some("/data/myrepo".into()),
        };
        assert!(repo.validate().is_err());
    }

    // ── E2S2-T24: Source repo default mount point ──

    #[test]
    fn source_repo_config_default_mount_point() {
        let repo = SourceRepoConfig {
            name: "myrepo".into(),
            host_path: PathBuf::from("/home/user/src/myrepo"),
            mount_point: None,
        };
        assert_eq!(repo.effective_mount_point(), "/workspace/myrepo");
    }

    // ── E2S2-T25: Source repo empty name rejected ──

    #[test]
    fn source_repo_config_empty_name_rejected() {
        let repo = SourceRepoConfig {
            name: String::new(),
            host_path: PathBuf::from("/home/user/src/repo"),
            mount_point: None,
        };
        assert!(repo.validate().is_err());
    }

    // ── E2S2-T26: Sandbox config parses from TOML ──

    #[test]
    fn sandbox_config_parses_from_toml() {
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"

[sandbox]
enabled = false
tmpfs_size_bytes = 134217728
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let sandbox = file.sandbox.unwrap();
        assert!(!sandbox.enabled);
        assert_eq!(sandbox.tmpfs_size_bytes, 134_217_728);
    }

    // ── E2S2-T27: Sandbox config defaults ──

    #[test]
    fn sandbox_config_defaults() {
        let config = SandboxConfig::default();
        assert!(config.enabled);
        assert_eq!(config.tmpfs_size_bytes, 268_435_456);
    }

    // ── E2S2-T28: Full vessel config with source and sandbox ──

    #[test]
    fn vessel_config_with_source_and_sandbox() {
        let toml_str = r#"
[vessel]
mission = "Monitor and improve the codebase"
data_dir = "/data"

[source]
repos = [
    { name = "exoskeleton", host_path = "/home/keiths/src/exoskeleton" },
]

[sandbox]
enabled = true
tmpfs_size_bytes = 268435456
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert_eq!(config.source_repos.len(), 1);
        assert_eq!(config.source_repos[0].name, "exoskeleton");
        assert!(config.sandbox.enabled);
        assert_eq!(config.sandbox.tmpfs_size_bytes, 268_435_456);
    }

    // ── E2S2-T29: Missing [source] section defaults to empty ──

    #[test]
    fn vessel_config_without_source_section() {
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert!(config.source_repos.is_empty());
        // sandbox defaults
        assert!(config.sandbox.enabled);
        assert_eq!(config.sandbox.tmpfs_size_bytes, 268_435_456);
    }

    // ── E2S2-T30: Source repo validation runs on vessel validate ──

    #[test]
    fn source_repo_validation_runs_on_vessel_validate() {
        let config = VesselConfig {
            mission: "test".into(),
            source_repos: vec![SourceRepoConfig {
                name: "bad".into(),
                host_path: PathBuf::from("relative/path"),
                mount_point: None,
            }],
            ..Default::default()
        };
        let result = config.validate();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("absolute"));
    }

    // ── E4S1-T10: VesselConfig observatory_url default is None ──

    #[test]
    fn vessel_config_observatory_url_default_none() {
        let config = VesselConfig::default();
        assert!(config.observatory_url.is_none());
        assert!(config.observatory_token_env.is_none());
    }

    // Mutex to serialize tests that read/write EXO_OBSERVATORY_URL env var.
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // ── E4S1-T11: VesselConfig parses observatory_url from TOML ──

    #[test]
    fn vessel_config_observatory_url_from_toml() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("EXO_OBSERVATORY_URL");
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"
observatory_url = "http://observatory:3000"
observatory_token_env = "EXO_OBSERVATORY_TOKEN"
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert_eq!(
            config.observatory_url.as_deref(),
            Some("http://observatory:3000")
        );
        assert_eq!(
            config.observatory_token_env.as_deref(),
            Some("EXO_OBSERVATORY_TOKEN")
        );
    }

    // ── E4S1-T12: EXO_OBSERVATORY_URL env var overrides TOML value ──

    #[test]
    fn vessel_config_observatory_url_env_override() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("EXO_OBSERVATORY_URL", "http://env-override:9000");
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"
observatory_url = "http://toml-value:3000"
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let mut config = VesselConfig::try_from(file).unwrap();
        config.apply_env_overrides().unwrap();
        assert_eq!(
            config.observatory_url.as_deref(),
            Some("http://env-override:9000")
        );
        std::env::remove_var("EXO_OBSERVATORY_URL");
    }

    // ── E4S1-T13: VesselConfig parses observatory_token_env from TOML ──

    #[test]
    fn vessel_config_observatory_token_env_from_toml() {
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"
observatory_token_env = "MY_CUSTOM_TOKEN"
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert_eq!(
            config.observatory_token_env.as_deref(),
            Some("MY_CUSTOM_TOKEN")
        );
    }

    // ── E5S1-T22: max_decide_turns default ──

    #[test]
    fn vessel_config_max_decide_turns_default() {
        let config = VesselConfig::default();
        assert_eq!(config.max_decide_turns, 5);
    }

    // ── E5S1-T23: max_decide_turns from TOML with floor enforcement ──

    #[test]
    fn vessel_config_max_decide_turns_from_toml() {
        // Explicit value parses correctly
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"
max_decide_turns = 3
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert_eq!(config.max_decide_turns, 3);

        // Zero is floored to 1 (max(1) enforcement)
        let toml_str = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"
max_decide_turns = 0
"#;
        let file: VesselConfigFile = toml::from_str(toml_str).unwrap();
        let config = VesselConfig::try_from(file).unwrap();
        assert_eq!(config.max_decide_turns, 1);
    }

    // ── E8S1-T16: inner_loop_config_defaults ──

    #[test]
    fn inner_loop_config_defaults() {
        let config = InnerLoopConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.max_steps_per_tick, 25);
        assert_eq!(config.max_tokens_per_session, 500_000);
        assert_eq!(config.timeout_secs, 300);
        assert_eq!(config.context_window_size, 3);
        assert_eq!(config.doom_loop_threshold, 3);
        assert_eq!(config.workspace_root, None);
    }

    // ── E8S1-T17: inner_loop_config_toml_roundtrip ──

    #[test]
    fn inner_loop_config_toml_roundtrip() {
        let config = InnerLoopConfig {
            enabled: true,
            max_steps_per_tick: 50,
            max_tokens_per_session: 1_000_000,
            timeout_secs: 600,
            context_window_size: 4,
            doom_loop_threshold: 5,
            workspace_root: None,
        };
        let toml_str = toml::to_string(&config).unwrap();
        let parsed: InnerLoopConfig = toml::from_str(&toml_str).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.max_steps_per_tick, 50);
        assert_eq!(parsed.max_tokens_per_session, 1_000_000);
        assert_eq!(parsed.timeout_secs, 600);
        assert_eq!(parsed.context_window_size, 4);
        assert_eq!(parsed.doom_loop_threshold, 5);
        assert_eq!(parsed.workspace_root, None);

        // Also verify full vessel config with inner_loop section
        let vessel_toml = r#"
[vessel]
mission = "test"
data_dir = "/tmp/exo"

[inner_loop]
enabled = true
max_steps_per_tick = 30
max_tokens_per_session = 750000
timeout_secs = 180
context_window_size = 6
doom_loop_threshold = 4
"#;
        let file: VesselConfigFile = toml::from_str(vessel_toml).unwrap();
        let vessel_config = VesselConfig::try_from(file).unwrap();
        assert!(vessel_config.inner_loop.enabled);
        assert_eq!(vessel_config.inner_loop.max_steps_per_tick, 30);
        assert_eq!(vessel_config.inner_loop.max_tokens_per_session, 750_000);
        assert_eq!(vessel_config.inner_loop.timeout_secs, 180);
        assert_eq!(vessel_config.inner_loop.context_window_size, 6);
        assert_eq!(vessel_config.inner_loop.doom_loop_threshold, 4);
        assert_eq!(vessel_config.inner_loop.workspace_root, None);
    }

    #[test]
    fn coding_inner_loop_enabled() {
        let config = CodingDefaults::inner_loop_config(None);
        assert!(config.enabled);
        assert_eq!(config.max_steps_per_tick, 25);
        assert_eq!(config.max_tokens_per_session, 500_000);
    }

    #[test]
    fn coding_inner_loop_with_workspace() {
        let config = CodingDefaults::inner_loop_config(Some("/home/user/project".into()));
        assert_eq!(config.workspace_root.as_deref(), Some("/home/user/project"));
    }

    #[test]
    fn coding_tool_policy_allows_code_tools() {
        let policy = CodingDefaults::tool_policy_config();
        assert_eq!(
            policy.rule_for("code.read"),
            crate::kernel::policy::PolicyRule::Allow
        );
        assert_eq!(
            policy.rule_for("code.edit"),
            crate::kernel::policy::PolicyRule::Allow
        );
    }

    #[test]
    fn coding_tool_policy_asks_for_shell() {
        let policy = CodingDefaults::tool_policy_config();
        assert_eq!(
            policy.rule_for("shell.exec"),
            crate::kernel::policy::PolicyRule::Ask
        );
    }

    // ── E8S1-T18: inner_loop_config_env_override ──

    #[test]
    fn inner_loop_config_env_override() {
        // Test EXO_INNER_LOOP_ENABLED=true enables the inner loop
        std::env::set_var("EXO_INNER_LOOP_ENABLED", "true");
        let mut config = VesselConfig {
            mission: "test".into(),
            ..Default::default()
        };
        assert!(!config.inner_loop.enabled);
        config.apply_env_overrides().unwrap();
        assert!(config.inner_loop.enabled);

        // Test EXO_INNER_LOOP_ENABLED=false disables it
        std::env::set_var("EXO_INNER_LOOP_ENABLED", "false");
        config.apply_env_overrides().unwrap();
        assert!(!config.inner_loop.enabled);

        // Test EXO_INNER_LOOP_ENABLED=1 also enables
        std::env::set_var("EXO_INNER_LOOP_ENABLED", "1");
        config.apply_env_overrides().unwrap();
        assert!(config.inner_loop.enabled);

        // Clean up
        std::env::remove_var("EXO_INNER_LOOP_ENABLED");
    }
}
