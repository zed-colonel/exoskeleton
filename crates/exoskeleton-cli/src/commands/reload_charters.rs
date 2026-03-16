//! `exo reload-charters` — reload thread charters from prompt files on disk.

use crate::client::{CliError, DaemonClient};

pub async fn run_reload_charters(client: &DaemonClient, json: bool) -> Result<(), CliError> {
    let value = client.reload_charters().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&value).unwrap());
    } else {
        let updated = value.get("updated").and_then(|v| v.as_u64()).unwrap_or(0);
        let message = value
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("charters reloaded");
        if updated > 0 {
            println!("{message}");
        } else {
            println!("No charter changes detected.");
        }
    }
    Ok(())
}
