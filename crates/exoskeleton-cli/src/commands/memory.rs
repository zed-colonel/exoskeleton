//! `exo memory` — view memory (episodic summaries and long-term notes).

use crate::client::{CliError, DaemonClient};

pub async fn run_memory(
    client: &DaemonClient,
    memory_type: Option<&str>,
    limit: usize,
    json: bool,
) -> Result<(), CliError> {
    let value = client.memory(memory_type, limit).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&value).unwrap());
    } else {
        if let Some(episodic) = value.get("episodic").and_then(|v| v.as_array()) {
            println!("=== Episodic Summaries ({}) ===", episodic.len());
            for item in episodic {
                let start = item.get("start_tick").and_then(|v| v.as_u64()).unwrap_or(0);
                let end = item.get("end_tick").and_then(|v| v.as_u64()).unwrap_or(0);
                let summary = item.get("summary").and_then(|v| v.as_str()).unwrap_or("?");
                println!("  Ticks {start}-{end}: {summary}");
            }
        }
        if let Some(long_term) = value.get("long_term").and_then(|v| v.as_array()) {
            println!("\n=== Long-Term Notes ({}) ===", long_term.len());
            for note in long_term {
                let topic = note.get("topic").and_then(|v| v.as_str()).unwrap_or("?");
                let content = note.get("content").and_then(|v| v.as_str()).unwrap_or("?");
                println!("  [{topic}] {content}");
            }
        }
    }
    Ok(())
}
