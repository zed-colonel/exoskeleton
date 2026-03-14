//! `exo snapshots` — view snapshot history or a specific snapshot.

use crate::client::{CliError, DaemonClient};
use crate::format;

pub async fn run_snapshots(
    client: &DaemonClient,
    limit: usize,
    json: bool,
) -> Result<(), CliError> {
    let snapshots = client.snapshots(limit).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&snapshots).unwrap());
    } else if snapshots.is_empty() {
        println!("No snapshots recorded yet.");
    } else {
        for snap in &snapshots {
            let tick = snap
                .get("tick_number")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let status = snap.get("status").and_then(|v| v.as_str()).unwrap_or("?");
            let updated = snap
                .get("updated_at")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            println!("  Tick {tick:>5}  {status:<12}  {updated}");
        }
    }
    Ok(())
}

pub async fn run_snapshot_at(client: &DaemonClient, tick: u64, json: bool) -> Result<(), CliError> {
    match client.snapshot_at(tick).await? {
        Some(value) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&value).unwrap());
            } else {
                println!("{}", format::format_snapshot(&value));
            }
            Ok(())
        }
        None => {
            eprintln!("no snapshot found for tick {tick}");
            Ok(())
        }
    }
}
