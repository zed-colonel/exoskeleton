# Configuration Reference

Exoskeleton is configured via a TOML file (`vessel.toml`) with environment variable overrides. The file is loaded by `VesselConfig::from_file()` and validated before the Vessel starts.

---

## Full Example

```toml
# vessel.toml -- complete example with all sections

[vessel]
# UUID string. Generated automatically if omitted.
# vessel_id = "550e8400-e29b-41d4-a716-446655440000"

# Required. The vessel's primary objective.
mission = "Monitor infrastructure and respond to alerts"

# Required. Root data directory for all engine-specific subdirectories.
data_dir = "/var/lib/exoskeleton"

# How often the master loop runs a PODAARA tick (seconds). Default: 60.
master_loop_interval_secs = 60

# Directory for the file-based inbox. Default: {data_dir}/inbox/
# inbox_dir = "/var/lib/exoskeleton/inbox"


[cognitive]
# Cognitive AQ dispatch tick interval in milliseconds. Default: 100.
tick_interval_ms = 100

# Max concurrent executing runs on the Cognitive AQ. Default: 4.
dispatch_concurrency = 4

# Cognitive AQ lease timeout in seconds. Must account for worst-case
# Act step duration (LLM call + tool execution). Default: 600.
lease_timeout_secs = 600


[cognitive.budget]
# Total local model tokens per window (input + output). Default: 1,000,000.
local_token_budget = 1000000

# Total frontier model tokens per window. Default: 100,000.
frontier_token_budget = 100000

# Maximum frontier spend per window in hundredths of a cent. Default: 500.
frontier_cost_budget_cents = 500

# Budget window duration in seconds (e.g., 3600 = hourly). Default: 3600.
time_window_secs = 3600

# Maximum tokens consumed in a single tick. Default: 50,000.
per_tick_token_cap = 50000

# Maximum tokens consumed by a single thread per tick. Default: 10,000.
per_thread_token_cap = 10000

# Model escalation policy.
[cognitive.budget.escalation_policy]
# Self-Critique uncertainty threshold for frontier escalation (0.0-1.0). Default: 0.7.
escalate_on_uncertainty = 0.7

# Action types that always use frontier model. Default: [].
escalate_on_stakes = ["fs.write", "http.request"]

# Consecutive failures before trying frontier. Default: 3.
escalate_on_consecutive_failures = 3

# Maximum frontier calls per window. Default: 50.
max_frontier_calls_per_window = 50


[tool]
# Tool AQ (WI Host) dispatch tick interval in milliseconds. Default: 50.
tick_interval_ms = 50

# Tool AQ worker count. Default: 4.
dispatch_concurrency = 4


[tool.budget]
# Maximum tool invocations per time window. Default: 1000.
max_invocations_per_window = 1000

# Time window duration in seconds. Default: 3600.
time_window_secs = 3600


[llm]
# Which backend to use when requests don't specify one.
# Values: "local", "frontier". Default: "local".
default_backend = "local"

# Default max output tokens. Default: 4096.
max_output_tokens = 4096

# Per-request timeout in seconds. Must be < cognitive lease_timeout_secs. Default: 120.
timeout_secs = 120


[llm.local]
# Base URL of the local inference server.
endpoint = "http://localhost:11434"

# Model name/tag.
model = "llama3.2:latest"

# API format: "openai_compat" or "ollama". Default: "openai_compat".
# openai_compat works with: Ollama (v0.1.24+), vLLM, LM Studio, llama.cpp server.
api_format = "openai_compat"


[llm.frontier]
# Provider: "anthropic" or "openai".
provider = "anthropic"

# Model identifier.
model = "claude-sonnet-4-20250514"

# Name of the environment variable holding the API key.
# The key is read at call time -- never stored in config, artifacts, or logs (I4).
api_key_env = "ANTHROPIC_API_KEY"

# Optional custom endpoint URL override.
# endpoint = "https://custom.api.com"


[daemon]
# HTTP listen address. Default: 127.0.0.1:7600.
# Set to "0.0.0.0:7600" for Docker/container deployment.
listen = "127.0.0.1:7600"
```

---

## Section Reference

### `[vessel]`

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `vessel_id` | UUID string | No | Generated | Unique identity of this vessel instance |
| `mission` | String | **Yes** | -- | The vessel's primary objective (must not be empty) |
| `data_dir` | Path | **Yes** | -- | Root data directory for all engine subdirectories |
| `master_loop_interval_secs` | u64 | No | `60` | PODAARA tick interval in seconds (must be >= 1 and < `lease_timeout_secs`) |
| `inbox_dir` | Path | No | `{data_dir}/inbox/` | Directory for the file-based inbox |

### `[cognitive]`

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `tick_interval_ms` | u64 | No | `100` | Cognitive AQ dispatch tick interval (must be > 0 and <= 60000) |
| `dispatch_concurrency` | usize | No | `4` | Max concurrent executing runs (must be > 0) |
| `lease_timeout_secs` | u64 | No | `600` | Lease timeout for cognitive tasks (must be >= 3) |

### `[cognitive.budget]`

Optional. Omit the entire section to disable cognitive budget enforcement.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `local_token_budget` | u64 | No | `1,000,000` | Local model tokens per window |
| `frontier_token_budget` | u64 | No | `100,000` | Frontier model tokens per window |
| `frontier_cost_budget_cents` | u64 | No | `500` | Max frontier spend per window (hundredths of a cent) |
| `time_window_secs` | u64 | No | `3600` | Budget window duration (must be > 0) |
| `per_tick_token_cap` | u64 | No | `50,000` | Max tokens per single tick (must be > 0) |
| `per_thread_token_cap` | u64 | No | `10,000` | Max tokens per thread per tick (must be > 0) |

### `[cognitive.budget.escalation_policy]`

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `escalate_on_uncertainty` | f64 | No | `0.7` | Uncertainty threshold for frontier escalation (0.0-1.0) |
| `escalate_on_stakes` | String[] | No | `[]` | Action types that always use frontier |
| `escalate_on_consecutive_failures` | u32 | No | `3` | Failure count before trying frontier |
| `max_frontier_calls_per_window` | u64 | No | `50` | Max frontier calls per window |

### `[tool]`

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `tick_interval_ms` | u64 | No | `50` | Tool AQ dispatch tick interval (must be > 0 and <= 60000) |
| `dispatch_concurrency` | usize | No | `4` | Tool AQ worker count (must be > 0) |

### `[tool.budget]`

Optional. Omit the entire section to disable tool rate limiting.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `max_invocations_per_window` | u64 | No | `1000` | Max tool invocations per window (must be > 0) |
| `time_window_secs` | u64 | No | `3600` | Window duration in seconds (must be > 0) |

### `[llm]`

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `default_backend` | `"local"` or `"frontier"` | No | `"local"` | Default backend for unspecified requests |
| `max_output_tokens` | u64 | No | `4096` | Default max output tokens |
| `timeout_secs` | u64 | No | `120` | Per-request timeout (must be >= 1 and < `lease_timeout_secs`) |

### `[llm.local]`

Optional. Required if `default_backend = "local"`.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `endpoint` | URL string | **Yes** | -- | Base URL of the local inference server |
| `model` | String | **Yes** | -- | Model name/tag |
| `api_format` | `"openai_compat"` or `"ollama"` | No | `"openai_compat"` | Which API format the server speaks |

### `[llm.frontier]`

Optional. Required if `default_backend = "frontier"`.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `provider` | `"anthropic"` or `"openai"` | **Yes** | -- | Frontier provider API |
| `model` | String | **Yes** | -- | Model identifier |
| `api_key_env` | String | **Yes** | -- | Name of the env var holding the API key |
| `endpoint` | URL string | No | Provider default | Custom endpoint URL override |

### `[daemon]`

Optional. Omit to use the default listen address.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `listen` | `"host:port"` string | No | `127.0.0.1:7600` | HTTP daemon listen address |

---

## Environment Variable Overrides

All environment variables are optional and override their TOML counterparts:

| Variable | Overrides | Example |
|----------|-----------|---------|
| `EXO_DATA_DIR` | `vessel.data_dir` | `/var/lib/exoskeleton` |
| `EXO_MISSION` | `vessel.mission` | `"Monitor alerts"` |
| `EXO_VESSEL_ID` | `vessel.vessel_id` | `550e8400-e29b-41d4-a716-446655440000` |
| `EXO_COGNITIVE_TICK_INTERVAL_MS` | `cognitive.tick_interval_ms` | `200` |
| `EXO_COGNITIVE_DISPATCH_CONCURRENCY` | `cognitive.dispatch_concurrency` | `8` |
| `EXO_TOOL_TICK_INTERVAL_MS` | `tool.tick_interval_ms` | `25` |
| `EXO_TOOL_DISPATCH_CONCURRENCY` | `tool.dispatch_concurrency` | `2` |
| `EXO_DAEMON_LISTEN` | `daemon.listen` | `0.0.0.0:7600` |

API keys are always read from environment variables (never stored in TOML):

| Variable | Used by |
|----------|---------|
| `ANTHROPIC_API_KEY` | Anthropic frontier provider |
| `OPENAI_API_KEY` | OpenAI frontier provider |

The `api_key_env` field in `[llm.frontier]` specifies which env var to read -- it holds the variable name, not the key itself.

---

## Validation Rules

The configuration is validated at load time. The following rules are enforced:

1. `mission` must not be empty
2. `cognitive.tick_interval_ms` must be > 0 and <= 60,000
3. `tool.tick_interval_ms` must be > 0 and <= 60,000
4. `cognitive.lease_timeout_secs` must be >= 3
5. `cognitive.dispatch_concurrency` must be > 0
6. `tool.dispatch_concurrency` must be > 0
7. `llm.timeout_secs` must be >= 1 and < `cognitive.lease_timeout_secs`
8. `master_loop_interval_secs` must be >= 1 and < `cognitive.lease_timeout_secs`
9. If `cognitive.budget` is present, `time_window_secs` must be > 0 and at least one token budget must be > 0
10. If `tool.budget` is present, both `max_invocations_per_window` and `time_window_secs` must be > 0
11. `escalation_policy.escalate_on_uncertainty` must be in [0.0, 1.0]

---

## Minimal Configuration

The smallest valid `vessel.toml`:

```toml
[vessel]
mission = "My first vessel"
data_dir = "./data"
```

This uses all defaults: local LLM backend (no model configured -- will warn), 60-second master loop, no budget enforcement, no daemon override.
