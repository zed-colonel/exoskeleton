//! `exo send` — send a message to the vessel's inbox.

use crate::client::{CliError, DaemonClient};

/// Send a message to the vessel.
///
/// The message is deposited into the vessel's inbox and will be picked up
/// on the next Perceive step. The daemon returns the envelope ID for tracking.
pub async fn run_send(
    client: &DaemonClient,
    source: &str,
    content: &str,
    json: bool,
) -> Result<(), CliError> {
    let response = client.send_message(source, content).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&response).unwrap_or_default()
        );
    } else {
        let envelope_id = response
            .get("envelope_id")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)");
        println!("Message sent. Envelope ID: {envelope_id}");
        println!("The message will be picked up on the next tick.");
    }
    Ok(())
}
