//! Fork a new vessel from a historical snapshot (E3-S3, W-23).

use crate::client::{CliError, DaemonClient};

pub async fn run_fork(
    client: DaemonClient,
    tick: u64,
    data_dir: &str,
    mission: Option<&str>,
    json: bool,
) -> Result<(), CliError> {
    let result = client.fork_snapshot(tick, data_dir, mission).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result).unwrap());
    } else {
        println!("Fork created successfully:");
        println!(
            "  Vessel ID:  {}",
            result["vessel_id"].as_str().unwrap_or("?")
        );
        println!(
            "  Config:     {}",
            result["config_path"].as_str().unwrap_or("?")
        );
        println!(
            "  Data Dir:   {}",
            result["data_dir"].as_str().unwrap_or("?")
        );
        println!("  Forked from: tick {}", result["forked_from_tick"]);
        println!();
        println!("Start the forked vessel with:");
        println!(
            "  exo start --config {}",
            result["config_path"].as_str().unwrap_or("<config_path>")
        );
    }

    Ok(())
}
