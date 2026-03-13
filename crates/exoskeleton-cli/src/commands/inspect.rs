//! `exo inspect` — show current state snapshot or tick history.

use crate::client::{CliError, DaemonClient};
use crate::format;

/// Show the current state snapshot.
pub async fn run_inspect(client: &DaemonClient, json: bool) -> Result<(), CliError> {
    match client.status().await? {
        Some(value) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).unwrap_or_default()
                );
            } else {
                println!("{}", format::format_snapshot(&value));
            }
        }
        None => {
            println!("No snapshot available (vessel has not completed a tick yet).");
        }
    }
    Ok(())
}

/// Show detail for a specific tick.
pub async fn run_inspect_tick(
    client: &DaemonClient,
    tick_id: &str,
    json: bool,
) -> Result<(), CliError> {
    match client.tick(tick_id).await? {
        Some(value) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).unwrap_or_default()
                );
            } else {
                println!("{}", format::format_tick(&value));
            }
        }
        None => {
            println!("Tick {tick_id} not found.");
        }
    }
    Ok(())
}

/// Show recent tick history.
pub async fn run_inspect_ticks(
    client: &DaemonClient,
    limit: usize,
    json: bool,
) -> Result<(), CliError> {
    let ticks = client.ticks(limit).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ticks).unwrap_or_default()
        );
    } else {
        println!("{}", format::format_ticks(&ticks));
    }
    Ok(())
}
