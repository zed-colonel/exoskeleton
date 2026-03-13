//! `exo relationship` — show relationship snapshot and principal history.

use crate::client::{CliError, DaemonClient};
use crate::format;

/// Show the current relationship snapshot.
pub async fn run_relationship_show(client: &DaemonClient, json: bool) -> Result<(), CliError> {
    let value = client.relationships().await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    } else {
        println!("{}", format::format_relationships(&value));
    }
    Ok(())
}

/// Show relationship history for a specific principal.
pub async fn run_relationship_history(
    client: &DaemonClient,
    principal_id: &str,
    limit: usize,
    json: bool,
) -> Result<(), CliError> {
    let records = client.relationship_history(principal_id, limit).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&records).unwrap_or_default()
        );
    } else if records.is_empty() {
        println!("No relationship history for principal {principal_id}.");
    } else {
        println!(
            "=== Relationship History for {principal_id} ({} records) ===\n",
            records.len()
        );
        for record in &records {
            let signal_type = record
                .get("signal_type")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let timestamp = record
                .get("timestamp")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let summary = record
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("(no summary)");
            println!("  [{timestamp}] {signal_type}: {summary}");
        }
    }
    Ok(())
}
