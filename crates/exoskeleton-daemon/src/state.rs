//! Shared application state for the HTTP daemon.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::http::HeaderValue;
use dashmap::{DashMap, DashSet};
use exoskeleton_core::inbox::Inbox;
use exoskeleton_core::{
    LedgerEntryId, LiveEvent, ProposalStatus, VesselId, VesselMode, WatchStore,
};
use exoskeleton_host::inspect::VesselInspector;
use exoskeleton_host::kernel::WiHostSlot;
use exoskeleton_host::metrics::ExoMetrics;
use exoskeleton_host::CognitiveEngineSlot;
use exoskeleton_relationship::AlignConfig;
use exoskeleton_threads::ThreadRegistry;
use worldinterface_connector::SignalRegistry;

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
    /// CORS allowed origins (U1). Empty means no CORS (same-origin only).
    pub cors_origins: Vec<HeaderValue>,
    /// Event IDs that have been acknowledged by an operator.
    /// In-memory only — resets on vessel restart.
    pub acknowledged_events: Arc<DashSet<LedgerEntryId>>,
    /// HMAC-SHA256 secrets for webhook signature verification.
    /// Key: source name (from X-Webhook-Source header).
    pub webhook_secrets: HashMap<String, Vec<u8>>,
    /// Operator-override statuses for charter proposals.
    /// In-memory only — resets on vessel restart.
    pub charter_proposal_statuses: Arc<DashMap<LedgerEntryId, ProposalStatus>>,
    /// Watch store for listing active watches.
    pub watch_store: Arc<dyn WatchStore>,
    /// Thread registry for applying approved charter proposals.
    pub thread_registry: Arc<ThreadRegistry>,
    /// WI host slot for connector hot-loading (shared with KernelContext).
    pub wi_host_slot: WiHostSlot,
    /// Connectors directory path (for resolving load requests by name).
    pub connectors_dir: Option<PathBuf>,
    /// AlignConfig for dynamic destructive tool classification.
    pub align_config: Option<Arc<AlignConfig>>,
    /// Cognitive AQ engine slot for early-wake triggers.
    pub cognitive_engine: CognitiveEngineSlot,
    /// Shared vessel mode state.
    pub vessel_mode: Arc<std::sync::Mutex<VesselMode>>,
    /// Signal registry for signal.await/signal.emit coordination.
    pub signal_registry: Arc<SignalRegistry>,
}
