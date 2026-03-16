//! Smoke test: boot a Vessel, wait for 1 tick, shut down.
//!
//! This verifies the acceptance test crate can compile and run against
//! the full Exoskeleton stack.

mod support;

use std::time::Duration;

#[tokio::test]
async fn vessel_boots_ticks_and_shuts_down() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;

    // Wait for at least 1 tick to complete
    let ticks = support::wait_for_ticks(vessel.storage(), 1, Duration::from_secs(10)).await;
    assert!(!ticks.is_empty(), "at least 1 tick should have completed");
    assert_eq!(ticks[0].tick_number, 1);

    support::shutdown_and_verify(vessel).await;
}
