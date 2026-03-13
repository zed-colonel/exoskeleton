//! `exo thread` — list and inspect cognitive threads.

use crate::client::{CliError, DaemonClient};
use crate::format;

/// List all registered threads with status.
pub async fn run_thread_list(client: &DaemonClient, json: bool) -> Result<(), CliError> {
    let threads = client.threads().await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&threads).unwrap_or_default()
        );
    } else {
        println!("{}", format::format_threads(&threads));
    }
    Ok(())
}

/// Show detail for a specific thread by ID.
///
/// Fetches the full thread list and filters to the requested ID. This avoids
/// requiring a separate endpoint -- the thread list is small.
pub async fn run_thread_inspect(
    client: &DaemonClient,
    thread_id: &str,
    json: bool,
) -> Result<(), CliError> {
    let threads = client.threads().await?;

    let matching: Vec<_> = threads
        .iter()
        .filter(|t| {
            t.get("thread_id")
                .and_then(|v| v.as_str())
                .map(|id| id == thread_id || id.starts_with(thread_id))
                .unwrap_or(false)
        })
        .collect();

    if matching.is_empty() {
        println!("Thread {thread_id} not found.");
        return Ok(());
    }

    for thread in &matching {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(thread).unwrap_or_default()
            );
        } else {
            let id = thread
                .get("thread_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let name = thread.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let charter = thread
                .get("charter")
                .and_then(|v| v.as_str())
                .unwrap_or("(none)");
            let priority = thread
                .get("priority")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let schedule = thread.get("schedule");
            let schedule_str = match schedule {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Object(obj)) => {
                    if let Some((key, val)) = obj.iter().next() {
                        format!("{key}({val})")
                    } else {
                        "?".into()
                    }
                }
                _ => "?".into(),
            };
            let status = thread.get("status").and_then(|v| v.as_str()).unwrap_or("?");
            let token_budget = thread
                .get("token_budget")
                .and_then(|v| v.as_u64())
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".into());
            let latest_output = thread
                .get("latest_output_summary")
                .and_then(|v| v.as_str())
                .unwrap_or("(no output yet)");

            println!(
                "\
=== Thread Detail ===
  Thread ID:     {id}
  Name:          {name}
  Charter:       {charter}
  Priority:      {priority}
  Schedule:      {schedule_str}
  Status:        {status}
  Token Budget:  {token_budget}
  Latest Output: {latest_output}"
            );
        }
    }

    Ok(())
}
