//! `exo inbox-history` — view inbox message history.

use crate::client::{CliError, DaemonClient};

pub async fn run_inbox_history(
    client: &DaemonClient,
    limit: usize,
    json: bool,
) -> Result<(), CliError> {
    let entries = client.inbox_history(limit).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&entries).unwrap());
    } else if entries.is_empty() {
        println!("No inbox history available.");
    } else {
        for entry in &entries {
            let timestamp = entry
                .get("timestamp")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let source = entry
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let content = entry.get("content").and_then(|v| v.as_str()).unwrap_or("?");
            // Truncate timestamp for display
            let ts_short = if timestamp.len() > 19 {
                &timestamp[..19]
            } else {
                timestamp
            };
            println!("  [{ts_short}] {source}: {content}");
        }
    }
    Ok(())
}
