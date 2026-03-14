//! Shared application state for the HTTP daemon.

use std::sync::Arc;

use exoskeleton_core::inbox::Inbox;
use exoskeleton_core::{LiveEvent, VesselId};
use exoskeleton_host::inspect::VesselInspector;
use exoskeleton_host::metrics::ExoMetrics;

/// Shared state available to all HTTP handlers via axum's `State` extractor.
pub struct AppState {
    /// Read-only inspection surface over all vessel state.
    pub inspector: VesselInspector,
    /// Prometheus metrics for the vessel.
    pub metrics: Arc<ExoMetrics>,
    /// Inbox for submitting messages (write path).
    pub inbox: Arc<dyn Inbox>,
    /// The vessel's unique identity.
    pub vessel_id: VesselId,
    /// Broadcast sender for WebSocket event distribution (D2).
    pub event_tx: tokio::sync::broadcast::Sender<LiveEvent>,
}
