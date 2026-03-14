//! `exo artifact` — fetch and display an artifact by ID.

use crate::client::{CliError, DaemonClient};

pub async fn run_artifact(client: &DaemonClient, id: &str, json: bool) -> Result<(), CliError> {
    match client.artifact(id).await? {
        Some(value) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&value).unwrap());
            } else {
                let kind = value.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                let content_type = value
                    .get("content_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let created = value
                    .get("created_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let content = value.get("content").and_then(|v| v.as_str()).unwrap_or("");
                println!("Kind:         {kind}");
                println!("Content-Type: {content_type}");
                println!("Created:      {created}");
                println!();
                println!("{content}");
            }
            Ok(())
        }
        None => {
            eprintln!("artifact not found: {id}");
            Ok(())
        }
    }
}
