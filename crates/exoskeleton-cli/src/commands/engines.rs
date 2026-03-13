//! `exo engines` — show dual engine health and status.

use crate::client::{CliError, DaemonClient};
use crate::format;

/// Show engine status for both Cognitive AQ and Tool AQ.
pub async fn run_engines(client: &DaemonClient, json: bool) -> Result<(), CliError> {
    let value = client.engines().await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    } else {
        println!("{}", format::format_engines(&value));
    }
    Ok(())
}
