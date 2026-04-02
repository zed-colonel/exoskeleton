//! Vessel runtime: dual-engine lifecycle management.
//!
//! The [`Vessel`] owns two independent subsystems:
//! - **Cognitive AQ** (direct ownership): master loop ticks, thread executions,
//!   LLM inference. Own WAL, own scheduler, own dispatch loop.
//! - **WI Host** (delegation): adapter invocations, workflow orchestration,
//!   external I/O. Internally owns the Tool AQ with its own WAL, scheduler,
//!   dispatch loop.
//!
//! The Act step (Sprint 5) is the sole boundary crossing from cognitive
//! decisions to tool execution (I9, IBP §3.3).

use std::sync::Arc;
use std::time::Duration;

use actionqueue_core::ids::TaskId;
use actionqueue_core::task::constraints::TaskConstraints;
use actionqueue_core::task::metadata::TaskMetadata;
use actionqueue_core::task::run_policy::RunPolicy;
use actionqueue_core::task::task_spec::{TaskPayload, TaskSpec};
use exoskeleton_core::inbox::Inbox;
use exoskeleton_core::{ArtifactStore, ExoError, LiveEvent, VesselId};
use exoskeleton_memory::{ApproximateTokenCounter, ContextCompiler};
use exoskeleton_threads::ThreadRegistry;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use worldinterface_connector::connectors::default_registry;
use worldinterface_connector::connectors::PeerResolveConnector;
use worldinterface_connector::registry::ConnectorRegistry;
use worldinterface_core::descriptor::Descriptor;
use worldinterface_host::config::HostConfig;
use worldinterface_host::host::EmbeddedHost;

use crate::budget::{CognitiveBudgetTracker, ToolBudgetGate};
use crate::cognitive_engine::{
    bootstrap_cognitive_engine, bootstrap_cognitive_engine_with_backends, CognitiveEngine,
    CognitivePayload, CognitiveTaskType,
};
use crate::config::VesselConfig;
use crate::inspect::VesselInspector;
use crate::kernel::{KernelContext, MasterLoopPayload, WiHostSlot};
use crate::llm::client::LlmClient;
use crate::storage::StorageManager;

/// Type alias for the engine slot (shared between tick loop and Vessel API).
pub(crate) type CognitiveEngineSlot = Arc<Mutex<Option<CognitiveEngine>>>;

/// The top-level Exoskeleton runtime.
///
/// Owns two independent subsystems:
/// - **Cognitive AQ** (direct ownership): master loop ticks, thread executions,
///   LLM inference. Own WAL, own scheduler, own dispatch loop.
/// - **WI Host** (delegation): adapter invocations, workflow orchestration,
///   external I/O. Internally owns the Tool AQ with its own WAL, scheduler,
///   dispatch loop.
///
/// The Act step (Sprint 5) is the sole boundary crossing from cognitive
/// decisions to tool execution (I9, IBP §3.3).
pub struct Vessel {
    config: VesselConfig,
    cognitive_engine: CognitiveEngineSlot,
    cognitive_tick_handle: JoinHandle<()>,
    cognitive_shutdown_tx: tokio::sync::watch::Sender<bool>,
    wi_host_slot: WiHostSlot,
    storage: StorageManager,
    llm_client: LlmClient,
    master_loop_task_id: TaskId,
    /// Budget window timer handle (Sprint 9). `None` if no budgets configured.
    budget_window_handle: Option<JoinHandle<()>>,
    /// Shared inbox reference (Sprint 10).
    inbox: Arc<dyn Inbox>,
    /// Thread registry (Sprint 10).
    thread_registry: Arc<ThreadRegistry>,
    /// Relationship ledger (Sprint 10).
    relationship_ledger: Arc<dyn exoskeleton_relationship::RelationshipLedger>,
    /// Cognitive budget tracker (Sprint 10).
    budget_tracker: Option<Arc<std::sync::Mutex<crate::budget::CognitiveBudgetTracker>>>,
    /// Tool budget gate (Sprint 10).
    tool_budget_gate: Option<Arc<std::sync::Mutex<crate::budget::ToolBudgetGate>>>,
    /// Broadcast sender for real-time events (D2).
    event_tx: tokio::sync::broadcast::Sender<LiveEvent>,
}

impl Vessel {
    /// Start the Vessel with the default connector registry.
    ///
    /// Includes all built-in connectors from WorldInterface's default registry
    /// (delay, http.request, fs.read, fs.write, shell.exec, sandbox.exec).
    /// If `observatory_url` is configured, also registers `peer.resolve`.
    ///
    /// # Errors
    /// - `ExoError::Config` — invalid configuration
    /// - `ExoError::Engine` — bootstrap failure for either engine
    /// - `ExoError::Storage` — data directory creation failure
    pub async fn start(config: VesselConfig) -> Result<Self, ExoError> {
        Self::start_with_registry(config, default_registry()).await
    }

    /// Start the Vessel with a custom connector registry.
    pub async fn start_with_registry(
        config: VesselConfig,
        registry: ConnectorRegistry,
    ) -> Result<Self, ExoError> {
        Self::start_with_registry_and_backends(config, registry, None, None).await
    }

    /// Start the Vessel with custom connector registry and LLM backends.
    ///
    /// Allows injection of mock LLM backends for acceptance/integration testing
    /// without requiring a real LLM endpoint.
    pub async fn start_with_registry_and_backends(
        config: VesselConfig,
        registry: ConnectorRegistry,
        local_backend: Option<Arc<dyn crate::llm::http::LlmHttpBackend>>,
        frontier_backend: Option<Arc<dyn crate::llm::http::LlmHttpBackend>>,
    ) -> Result<Self, ExoError> {
        // 1. Validate config
        config.validate()?;

        // 2. Create data directories (including inbox_dir)
        ensure_data_dirs(&config)?;

        // 3. Open storage layer (Sprint 2)
        let storage = StorageManager::open(&config.data_dir)?;

        // 4. Create ContextCompiler
        let counter = Arc::new(ApproximateTokenCounter);
        let compiler =
            ContextCompiler::with_defaults(counter, config.llm_config.max_output_tokens * 4);
        // Budget is 4x output tokens as a reasonable context budget

        // 5. Create Inbox
        let inbox_dir = config
            .inbox_dir
            .clone()
            .unwrap_or_else(|| config.data_dir.join("inbox"));
        let inbox: Arc<dyn Inbox> = Arc::new(crate::inbox::FileInbox::new(inbox_dir)?);

        // 6. Create WiHostSlot (empty)
        let wi_host_slot: WiHostSlot = Arc::new(tokio::sync::Mutex::new(None));

        // 7. Build KernelContext
        let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();
        let thread_registry = Arc::new(ThreadRegistry::new(storage.thread_store().clone()));
        let relationship_ledger: Arc<dyn exoskeleton_relationship::RelationshipLedger> =
            storage.relationship_store().clone();
        // 7.5 Create budget trackers (Sprint 9)
        let budget_tracker = if let Some(ref cb) = config.cognitive_budget {
            let budget_store: Arc<dyn exoskeleton_core::BudgetStore> =
                storage.budget_store().clone();
            let mut tracker = crate::budget::CognitiveBudgetTracker::new(cb.clone(), budget_store);
            if let Err(e) = tracker.load_or_reset() {
                tracing::warn!(error = %e, "failed to load budget state; starting fresh");
            }
            Some(Arc::new(std::sync::Mutex::new(tracker)))
        } else {
            None
        };
        let tool_budget_gate = config.tool_budget.as_ref().map(|tb| {
            Arc::new(std::sync::Mutex::new(crate::budget::ToolBudgetGate::new(
                tb.clone(),
            )))
        });

        // 7.6 Create broadcast channel for real-time events (D2)
        let (event_tx, _) = tokio::sync::broadcast::channel::<LiveEvent>(256);

        // 7.7 Create PromptRegistry (Epoch 0)
        let mut prompt_registry = exoskeleton_core::prompt::PromptRegistry::with_defaults();
        crate::prompt_loader::load_prompt_overrides(&mut prompt_registry, &config.data_dir);
        let prompt_registry = Arc::new(prompt_registry);

        let kernel_context = Arc::new(KernelContext {
            snapshot_store: storage.snapshot_store().clone(),
            event_ledger: storage.event_ledger().clone(),
            tick_store: storage.tick_store().clone(),
            memory_store: storage.memory_store().clone(),
            artifact_store: artifact_store.clone(),
            context_compiler: Arc::new(compiler),
            wi_host_slot: Arc::clone(&wi_host_slot),
            inbox: inbox.clone(),
            vessel_id: config.vessel_id,
            mission: config.mission.clone(),
            max_output_tokens: config.llm_config.max_output_tokens,
            master_loop_interval_secs: config.master_loop_interval_secs,
            thread_registry: thread_registry.clone(),
            relationship_ledger: relationship_ledger.clone(),
            conversation_store: storage.conversation_store().clone(),
            budget_tracker: budget_tracker.clone(),
            tool_budget_gate: tool_budget_gate.clone(),
            metrics: None,
            event_tx: event_tx.clone(),
            prompt_registry: prompt_registry.clone(),
            trust_decay_config: config.trust_decay.clone(),
            episodic_memory_capacity: config.episodic_memory_capacity,
            bootstrap_grace_period_ticks: config.bootstrap_grace_period_ticks,
            max_decide_turns: config.max_decide_turns,
            watch_store: storage.watch_store().clone() as Arc<dyn exoskeleton_core::WatchStore>,
            max_watches: config.max_watches,
            read_paths_this_tick: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            inner_loop_config: config.inner_loop.clone(),
        });

        // 7.8 Seed episodic memory for first boot (Decoherence Fix)
        // Prevents empty-context starvation that triggers LLM fabrication.
        if kernel_context.snapshot_store.latest()?.is_none() {
            let seed_summary = exoskeleton_core::EpisodicSummary::new(
                0,
                0,
                format!(
                    "Vessel initialized. Mission: \"{mission}\". \
                     Bootstrap complete. Cognitive engines starting. \
                     All three built-in threads registered: Threat Monitor (Critical, EveryTick), \
                     Self-Critique (High, EveryTick), Memory Consolidation (Normal, every 5 ticks). \
                     Bootstrap grace period active for {grace} ticks — \
                     early self-referential activity is expected during initialization.",
                    mission = config.mission,
                    grace = config.bootstrap_grace_period_ticks,
                ),
                vec![
                    "vessel_initialized".into(),
                    "bootstrap_complete".into(),
                    "cognitive_engines_starting".into(),
                ],
            );
            kernel_context.memory_store.write_episodic(&seed_summary)?;
            tracing::info!("seeded initial episodic memory for first boot");
        }

        // 8. Register built-in cognitive threads (Sprint 7)
        // Build thread config overrides from [threads] TOML section
        let thread_overrides = config.build_thread_overrides();
        exoskeleton_threads::register_builtin_threads(
            &kernel_context.thread_registry,
            &prompt_registry,
            thread_overrides.as_ref(),
        )?;

        // 9. Bootstrap Cognitive AQ engine with KernelContext
        let cognitive_engine = if local_backend.is_some() || frontier_backend.is_some() {
            bootstrap_cognitive_engine_with_backends(
                &config,
                local_backend,
                frontier_backend,
                artifact_store.clone(),
                Some(kernel_context),
            )?
        } else {
            bootstrap_cognitive_engine(&config, artifact_store.clone(), Some(kernel_context))?
        };
        let engine_slot: CognitiveEngineSlot = Arc::new(Mutex::new(Some(cognitive_engine)));

        // 10. Start cognitive tick loop
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let tick_handle = start_cognitive_tick_loop(
            Arc::clone(&engine_slot),
            config.cognitive_tick_interval,
            shutdown_rx,
        );

        // 10b. Conditionally register peer.resolve connector
        if let Some(ref observatory_url) = config.observatory_url {
            let token = config
                .observatory_token_env
                .as_ref()
                .and_then(|env_name| std::env::var(env_name).ok());
            registry.register(Arc::new(PeerResolveConnector::new(
                observatory_url.clone(),
                token,
            )));
        }

        // 10c. Create streaming message handler for WI → inbox bridge
        let stream_handler: Option<Arc<dyn worldinterface_core::streaming::StreamMessageHandler>> =
            Some(Arc::new(crate::inbox::InboxStreamHandler::new(
                Arc::clone(&inbox),
                artifact_store.clone(),
            )));

        // 11. Bootstrap WI Host (which bootstraps Tool AQ internally)
        let host_config = Self::build_host_config(&config);
        let wi_host = match EmbeddedHost::start(host_config, registry, stream_handler).await {
            Ok(host) => host,
            Err(e) => {
                // Clean up: stop tick loop and shutdown cognitive engine
                let _ = shutdown_tx.send(true);
                let _ = tick_handle.await;
                if let Some(engine) = engine_slot.lock().await.take() {
                    let _ = engine.shutdown();
                }
                return Err(ExoError::Engine(format!("WI host bootstrap failed: {e}")));
            }
        };

        // 12. Populate WiHostSlot
        wi_host_slot.lock().await.replace(wi_host);

        // 13. Create LlmClient (Sprint 4)
        let llm_client = LlmClient::new(Arc::clone(&engine_slot), &config.llm_config);

        // 14. Submit master loop task
        let master_loop_task_id = schedule_master_loop(&engine_slot, &config).await?;

        // 15. Allocate AQ budgets on master loop task (Sprint 9)
        if let Some(ref cb) = config.cognitive_budget {
            let total_token_budget = cb
                .local_token_budget
                .saturating_add(cb.frontier_token_budget);
            let mut guard = engine_slot.lock().await;
            if let Some(engine) = guard.as_mut() {
                engine
                    .allocate_budget(
                        master_loop_task_id,
                        actionqueue_core::budget::BudgetDimension::Token,
                        total_token_budget,
                    )
                    .map_err(|e| {
                        ExoError::Engine(format!("cognitive budget allocation (Token): {e}"))
                    })?;
                engine
                    .allocate_budget(
                        master_loop_task_id,
                        actionqueue_core::budget::BudgetDimension::CostCents,
                        cb.frontier_cost_budget_cents,
                    )
                    .map_err(|e| {
                        ExoError::Engine(format!("cognitive budget allocation (CostCents): {e}"))
                    })?;
                tracing::info!(
                    total_token_budget,
                    frontier_cost_cents = cb.frontier_cost_budget_cents,
                    "AQ budgets allocated on master loop task"
                );
            }
        }

        // 16. Spawn budget window timer (Sprint 9)
        let budget_window_handle =
            if config.cognitive_budget.is_some() || config.tool_budget.is_some() {
                let window_secs = config
                    .cognitive_budget
                    .as_ref()
                    .map(|cb| cb.time_window_secs)
                    .unwrap_or(3600);
                let shutdown_rx2 = shutdown_tx.subscribe();
                Some(spawn_budget_window_timer(BudgetWindowTimerConfig {
                    engine_slot: Arc::clone(&engine_slot),
                    master_loop_task_id,
                    cognitive_budget: config.cognitive_budget.clone(),
                    budget_tracker: budget_tracker.clone(),
                    tool_budget_gate: tool_budget_gate.clone(),
                    shutdown_rx: shutdown_rx2,
                    window_secs,
                }))
            } else {
                None
            };

        tracing::info!(
            vessel_id = %config.vessel_id,
            cognitive_aq_dir = %config.data_dir.join("cognitive-aq").display(),
            tool_aq_dir = %config.data_dir.join("wi/aq").display(),
            master_loop_task_id = %master_loop_task_id,
            "vessel started — both engines running, master loop scheduled"
        );

        // D2: Broadcast VesselStarted LiveEvent (fires once at boot)
        let initial_snapshot =
            exoskeleton_core::StateSnapshot::initial(config.vessel_id, config.mission.clone());
        let _ = event_tx.send(LiveEvent {
            event_type: exoskeleton_core::EventType::VesselStarted,
            tick_number: None,
            summary: format!("Vessel {} started", config.vessel_id),
            timestamp: chrono::Utc::now(),
            snapshot: Some(initial_snapshot),
            inner_loop_detail: None,
        });

        Ok(Self {
            config,
            cognitive_engine: engine_slot,
            cognitive_tick_handle: tick_handle,
            cognitive_shutdown_tx: shutdown_tx,
            wi_host_slot,
            storage,
            llm_client,
            master_loop_task_id,
            budget_window_handle,
            inbox,
            thread_registry,
            relationship_ledger,
            budget_tracker,
            tool_budget_gate,
            event_tx,
        })
    }

    /// Shut down the Vessel gracefully.
    ///
    /// Shutdown order:
    /// 0. Signal shutdown to all listeners (budget timer + tick loop)
    /// 1. Stop budget window timer (now receives signal and exits)
    /// 2. Stop cognitive tick loop (also received signal)
    /// 3. Drain and shut down the Cognitive AQ engine (WAL flush)
    /// 4. Shut down the WI Host (which shuts down Tool AQ internally)
    ///
    /// Both engines are shut down independently (I9, IBP §3.6).
    pub async fn shutdown(self) -> Result<(), ExoError> {
        tracing::info!(vessel_id = %self.config.vessel_id, "vessel shutting down");

        // 0. Signal shutdown to all listeners FIRST.
        //    Both the budget window timer and cognitive tick loop subscribe to this
        //    channel. Signaling before awaiting handles prevents a deadlock where
        //    we await a timer that is waiting for the signal we haven't sent yet.
        let _ = self.cognitive_shutdown_tx.send(true);

        // 1. Stop budget window timer (Sprint 9 — now receives the signal)
        if let Some(handle) = self.budget_window_handle {
            let _ = handle.await;
        }

        // 2. Stop cognitive tick loop (also received the signal)
        let _ = self.cognitive_tick_handle.await;

        // 3. Drain and shutdown Cognitive AQ
        let cognitive_engine = self
            .cognitive_engine
            .lock()
            .await
            .take()
            .ok_or_else(|| ExoError::Engine("cognitive engine already shut down".into()))?;

        cognitive_engine
            .drain_and_shutdown(self.config.shutdown_timeout)
            .await
            .map_err(|e| ExoError::Engine(format!("cognitive AQ shutdown: {e}")))?;

        // 4. Shutdown WI Host (take from slot)
        let wi_host = self
            .wi_host_slot
            .lock()
            .await
            .take()
            .ok_or_else(|| ExoError::Engine("WI host already shut down".into()))?;
        wi_host
            .shutdown()
            .await
            .map_err(|e| ExoError::Engine(format!("WI host shutdown: {e}")))?;

        tracing::info!("vessel shutdown complete");
        Ok(())
    }

    /// The vessel's configuration.
    pub fn config(&self) -> &VesselConfig {
        &self.config
    }

    /// The vessel's unique identity.
    pub fn vessel_id(&self) -> VesselId {
        self.config.vessel_id
    }

    /// Access the Cognitive AQ engine slot (for testing and direct task submission).
    pub fn cognitive_engine_slot(&self) -> &CognitiveEngineSlot {
        &self.cognitive_engine
    }

    /// Access the storage manager (for testing and direct store access).
    pub fn storage(&self) -> &StorageManager {
        &self.storage
    }

    /// Access the LLM client for inference via the Cognitive AQ.
    pub fn llm_client(&self) -> &LlmClient {
        &self.llm_client
    }

    /// List all tool capabilities available through the WI Host.
    ///
    /// Returns descriptors for all registered connectors (e.g., `delay`,
    /// `http.request`, `fs.read`, `fs.write`). These are Tool AQ capabilities —
    /// cognitive work (LLM calls, threads) is not represented here.
    pub fn list_capabilities(&self) -> Vec<Descriptor> {
        match self.wi_host_slot.try_lock() {
            Ok(guard) => match guard.as_ref() {
                Some(host) => host.list_capabilities(),
                None => vec![],
            },
            Err(_) => vec![],
        }
    }

    /// Describe a specific tool capability by name.
    ///
    /// Returns `None` if no connector with the given name is registered.
    pub fn describe(&self, name: &str) -> Option<Descriptor> {
        match self.wi_host_slot.try_lock() {
            Ok(guard) => guard.as_ref().and_then(|host| host.describe(name)),
            Err(_) => None,
        }
    }

    /// Invoke a single tool operation through the WI Host (Tool AQ path).
    ///
    /// This is the primitive that the Act step (Sprint 5) will use. It creates
    /// an ephemeral one-node FlowSpec and executes it through the full Tool AQ
    /// pipeline: validation -> compilation -> AQ submission -> connector execution ->
    /// result.
    ///
    /// # Arguments
    /// - `name` — connector name (e.g., `"delay"`, `"fs.write"`)
    /// - `params` — connector-specific parameters as JSON
    ///
    /// # Errors
    /// - Tool not found
    /// - Connector execution failure
    /// - Tool AQ engine error
    pub async fn invoke_tool(
        &self,
        name: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ExoError> {
        let guard = self.wi_host_slot.lock().await;
        let host = guard
            .as_ref()
            .ok_or_else(|| ExoError::Engine("WI host not available".into()))?;
        host.invoke_single(name, params)
            .await
            .map_err(|e| ExoError::Engine(format!("tool invocation failed: {e}")))
    }

    /// The master loop task ID submitted during boot.
    pub fn master_loop_task_id(&self) -> TaskId {
        self.master_loop_task_id
    }

    // ── Sprint 10: Inspection accessors ──

    /// Create a VesselInspector for read-only queries.
    pub fn inspector(&self) -> VesselInspector {
        VesselInspector::new(
            self.storage.clone(),
            self.thread_registry.clone(),
            self.relationship_ledger.clone(),
            self.budget_tracker.clone(),
            self.tool_budget_gate.clone(),
            self.wi_host_slot.clone(),
            self.config.clone(),
        )
    }

    /// Access the WI Host slot (for inspector engine status queries).
    pub fn wi_host_slot(&self) -> &WiHostSlot {
        &self.wi_host_slot
    }

    /// Access the thread registry.
    pub fn thread_registry(&self) -> &Arc<ThreadRegistry> {
        &self.thread_registry
    }

    /// Access the watch store (E5-S2).
    pub fn watch_store(&self) -> Arc<dyn exoskeleton_core::WatchStore> {
        self.storage.watch_store().clone() as Arc<dyn exoskeleton_core::WatchStore>
    }

    /// Access the budget tracker.
    pub fn budget_tracker(&self) -> &Option<Arc<std::sync::Mutex<CognitiveBudgetTracker>>> {
        &self.budget_tracker
    }

    /// Access the tool budget gate.
    pub fn tool_budget_gate(&self) -> &Option<Arc<std::sync::Mutex<ToolBudgetGate>>> {
        &self.tool_budget_gate
    }

    /// Access the inbox (for daemon write path).
    pub fn inbox(&self) -> &Arc<dyn Inbox> {
        &self.inbox
    }

    /// Access the broadcast sender for real-time events (D2).
    pub fn event_sender(&self) -> &tokio::sync::broadcast::Sender<LiveEvent> {
        &self.event_tx
    }

    /// Build a WI HostConfig from VesselConfig tool settings.
    fn build_host_config(config: &VesselConfig) -> HostConfig {
        HostConfig {
            aq_data_dir: config.data_dir.join("wi").join("aq"),
            context_store_path: config.data_dir.join("wi").join("context.db"),
            tick_interval: config.tool_tick_interval,
            dispatch_concurrency: config.tool_dispatch_concurrency,
            connectors_dir: config.connectors_dir.clone(),
            ..Default::default()
        }
    }
}

/// Create the data directory structure required by both engines.
///
/// Creates all required subdirectories under `config.data_dir`. The AQ engines
/// create their own WAL and snapshot files during bootstrap — we only need to
/// ensure the parent directories exist.
///
/// Note: `cognitive-aq/` and `wi/aq/` are separate directories. This is the
/// physical manifestation of I9 (dual-engine isolation).
pub fn ensure_data_dirs(config: &VesselConfig) -> Result<(), ExoError> {
    let inbox_dir = config
        .inbox_dir
        .clone()
        .unwrap_or_else(|| config.data_dir.join("inbox"));
    let dirs = [
        config.data_dir.join("cognitive-aq"),
        config.data_dir.join("wi").join("aq"),
        config.data_dir.join("wi"),
        config.data_dir.join("exo"),
        config.data_dir.join("workspace"),
        inbox_dir,
    ];
    for dir in &dirs {
        std::fs::create_dir_all(dir).map_err(|e| {
            ExoError::Storage(format!("failed to create directory {}: {e}", dir.display()))
        })?;
    }

    // sandbox.exec expects /sandbox to exist (tmpfs in production containers,
    // regular dir for local/dev). Best-effort — may fail outside containers.
    if let Err(e) = std::fs::create_dir_all("/sandbox") {
        tracing::debug!("could not create /sandbox (non-fatal): {e}");
    }

    Ok(())
}

/// Start the background tick loop for the Cognitive AQ engine.
///
/// Ticks the engine at the configured interval. Stops when the shutdown
/// signal is received.
fn start_cognitive_tick_loop(
    engine_slot: CognitiveEngineSlot,
    tick_interval: Duration,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tick_interval);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let mut guard = engine_slot.lock().await;
                    if let Some(engine) = guard.as_mut() {
                        if let Err(e) = engine.tick().await {
                            tracing::warn!(error = %e, "cognitive AQ tick error");
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    break;
                }
            }
        }
    })
}

/// Configuration for the budget window timer.
struct BudgetWindowTimerConfig {
    engine_slot: CognitiveEngineSlot,
    master_loop_task_id: TaskId,
    cognitive_budget: Option<exoskeleton_core::CognitiveBudgetConfig>,
    budget_tracker: Option<Arc<std::sync::Mutex<crate::budget::CognitiveBudgetTracker>>>,
    tool_budget_gate: Option<Arc<std::sync::Mutex<crate::budget::ToolBudgetGate>>>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    window_secs: u64,
}

/// Spawn the budget window management timer (Sprint 9).
///
/// Runs independently of the Cognitive AQ tick loop. When a budget window
/// expires, replenishes AQ budgets and resets Exoskeleton-layer counters.
fn spawn_budget_window_timer(mut cfg: BudgetWindowTimerConfig) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(cfg.window_secs));
        interval.tick().await; // Skip immediate first tick
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    // 1. Reset Exoskeleton-layer cognitive budget
                    if let Some(ref tracker) = cfg.budget_tracker {
                        match tracker.lock() {
                            Ok(mut guard) => {
                                if let Err(e) = guard.reset_window() {
                                    tracing::error!(error = %e, "budget window reset failed");
                                }
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "budget tracker mutex poisoned");
                            }
                        }
                    }

                    // 2. Replenish AQ budgets on master loop task
                    if let Some(ref cb) = cfg.cognitive_budget {
                        let total_tokens =
                            cb.local_token_budget.saturating_add(cb.frontier_token_budget);
                        let mut guard = cfg.engine_slot.lock().await;
                        if let Some(engine) = guard.as_mut() {
                            if let Err(e) = engine.replenish_budget(
                                cfg.master_loop_task_id,
                                actionqueue_core::budget::BudgetDimension::Token,
                                total_tokens,
                            ) {
                                tracing::error!(error = %e, "budget replenish (Token) failed");
                            }
                            if let Err(e) = engine.replenish_budget(
                                cfg.master_loop_task_id,
                                actionqueue_core::budget::BudgetDimension::CostCents,
                                cb.frontier_cost_budget_cents,
                            ) {
                                tracing::error!(error = %e, "budget replenish (CostCents) failed");
                            }
                        }
                    }

                    // 3. Reset tool budget gate
                    if let Some(ref gate) = cfg.tool_budget_gate {
                        match gate.lock() {
                            Ok(mut guard) => {
                                guard.reset_window();
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "tool budget gate mutex poisoned");
                            }
                        }
                    }

                    tracing::info!("budget window reset complete");
                }
                _ = cfg.shutdown_rx.changed() => {
                    break;
                }
            }
        }
    })
}

/// Submit the master loop recurring task to the Cognitive AQ.
async fn schedule_master_loop(
    engine_slot: &CognitiveEngineSlot,
    config: &VesselConfig,
) -> Result<TaskId, ExoError> {
    let payload = CognitivePayload {
        task_type: CognitiveTaskType::MasterLoop,
        data: serde_json::to_value(MasterLoopPayload {
            vessel_id: config.vessel_id,
        })
        .map_err(|e| ExoError::Engine(format!("master loop payload serialization: {e}")))?,
    };

    let task_id = TaskId::new();
    let payload_bytes = serde_json::to_vec(&payload)
        .map_err(|e| ExoError::Engine(format!("master loop payload serialization: {e}")))?;

    let spec = TaskSpec::new(
        task_id,
        TaskPayload::with_content_type(payload_bytes, "application/json"),
        // 10_000 iterations at 60s/tick ≈ 7 days. Vessel restart re-schedules.
        RunPolicy::repeat(10_000, config.master_loop_interval_secs)
            .map_err(|e| ExoError::Engine(format!("master loop run policy: {e}")))?,
        TaskConstraints::new(3, None, Some("exo:master_loop".into()))
            .map_err(|e| ExoError::Engine(format!("master loop constraints: {e}")))?,
        TaskMetadata::new(
            vec!["exo".into(), "master_loop".into()],
            100,
            Some("PODAARA master loop".into()),
        ),
    )
    .map_err(|e| ExoError::Engine(format!("master loop task spec: {e}")))?;

    let mut guard = engine_slot.lock().await;
    let engine = guard
        .as_mut()
        .ok_or_else(|| ExoError::Engine("cognitive engine not available".into()))?;
    engine
        .submit_task(spec)
        .map_err(|e| ExoError::Engine(format!("master loop task submission: {e}")))?;

    Ok(task_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── E4S1-T17: WI default_registry has built-in connectors ──

    #[test]
    fn default_registry_has_nine_connectors() {
        let registry = default_registry();
        assert_eq!(registry.len(), 9, "expected 9 built-in connectors");
        assert!(registry.get("delay").is_some());
        assert!(registry.get("http.request").is_some());
        assert!(registry.get("fs.read").is_some());
        assert!(registry.get("fs.write").is_some());
        assert!(registry.get("code.read").is_some());
        assert!(registry.get("code.edit").is_some());
        assert!(registry.get("code.write").is_some());
        assert!(registry.get("shell.exec").is_some());
        assert!(registry.get("sandbox.exec").is_some());
    }

    // ── E4S1-T18: peer.resolve registered when observatory_url is set ──

    #[test]
    fn peer_resolve_registered_when_observatory_url_set() {
        let registry = default_registry();
        assert_eq!(registry.len(), 9);

        // Simulate the conditional registration from start_with_registry_and_backends
        let observatory_url = "http://observatory:3000".to_string();
        registry.register(Arc::new(PeerResolveConnector::new(observatory_url, None)));

        assert_eq!(registry.len(), 10);
        assert!(registry.get("peer.resolve").is_some());
    }
}
