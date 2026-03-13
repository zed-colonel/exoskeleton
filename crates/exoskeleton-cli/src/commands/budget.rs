//! `exo budget` — show cognitive and tool budget status.

use crate::client::{CliError, DaemonClient};
use crate::format;

/// Show the current budget status.
pub async fn run_budget(client: &DaemonClient, json: bool) -> Result<(), CliError> {
    let value = client.budget().await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    } else {
        println!("{}", format::format_budget(&value));
    }
    Ok(())
}
