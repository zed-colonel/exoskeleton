//! `exo config` — view sanitized vessel configuration.

use crate::client::{CliError, DaemonClient};

pub async fn run_config(client: &DaemonClient, json: bool) -> Result<(), CliError> {
    let value = client.config().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&value).unwrap());
    } else {
        let vessel_id = value
            .get("vessel_id")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let mission = value.get("mission").and_then(|v| v.as_str()).unwrap_or("?");
        let data_dir = value
            .get("data_dir")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let backend = value
            .get("llm_default_backend")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let max_tokens = value
            .get("llm_max_output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let timeout = value
            .get("llm_timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let interval = value
            .get("master_loop_interval_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        println!("=== Vessel Configuration ===");
        println!("  Vessel ID:       {vessel_id}");
        println!("  Mission:         {mission}");
        println!("  Data Dir:        {data_dir}");
        println!("  Master Loop:     every {interval}s");
        println!("  LLM Backend:     {backend}");
        println!("  Max Tokens:      {max_tokens}");
        println!("  LLM Timeout:     {timeout}s");

        if let Some(local) = value.get("llm_local") {
            let endpoint = local
                .get("endpoint")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let model = local.get("model").and_then(|v| v.as_str()).unwrap_or("?");
            let format = local
                .get("api_format")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            println!("\n  Local LLM:");
            println!("    Endpoint:      {endpoint}");
            println!("    Model:         {model}");
            println!("    API Format:    {format}");
        }

        if let Some(frontier) = value.get("llm_frontier") {
            let provider = frontier
                .get("provider")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let model = frontier
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let key_env = frontier
                .get("api_key_env")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            println!("\n  Frontier LLM:");
            println!("    Provider:      {provider}");
            println!("    Model:         {model}");
            println!("    API Key Env:   ${key_env}");
        }

        if let Some(listen) = value.get("daemon_listen").and_then(|v| v.as_str()) {
            println!("\n  Daemon Listen:   {listen}");
        }
    }
    Ok(())
}
