//! `exo events` — show recent events from the event ledger.

use crate::client::{CliError, DaemonClient};
use crate::format;

/// Show recent events.
pub async fn run_events(client: &DaemonClient, limit: usize, json: bool) -> Result<(), CliError> {
    let events = client.events(limit).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&events).unwrap_or_default()
        );
    } else {
        println!("{}", format::format_events(&events));
    }
    Ok(())
}
