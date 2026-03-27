# Exoskeleton Development Roadmap v3

**Date:** 2026-03-27
**Revision:** v3 — Post-Epoch 5 comprehensive revision
**Reference:** [Roadmap v2](ROADMAP-v2.md), [Post-Epoch 5 Audit](POST-EPOCH-5-AUDIT.md), [Application State v1.0-alpha](APPLICATION-STATE-v1.0-alpha.md)

---

## How to Read This Document

This roadmap supersedes [ROADMAP-v2.md](ROADMAP-v2.md) (2026-03-21). It reflects the completion of all pre-production epochs (0, 3, 1, O, Decoherence, 2, 4, 5) and revises remaining work based on the comprehensive post-E5 audit.

**Key change from v2:** ActionQueue already implements the platform features (multi-tenant, RBAC, actor system, HTTP API) that v2's Epoch 7 planned to build. This dramatically reduces Epoch 7 scope from 7-9 sprints to ~3-5.

---

## Current State (2026-03-27)

### Completed Epochs

| Epoch | Theme | Sprints | Key Deliverables |
|-------|-------|---------|------------------|
| **0** | Unblock & Harden | 3 | http.request fix, prompt externalization, CI pipeline |
| **3** | Deployment & DevEx | 4 | Embedded mode, snapshot forking, ts-rs codegen, context viz |
| **1** | Cognitive Depth | 3 | Structured Plan, working memory, conversations, parallel threads, trust decay, episodic eviction |
| **O** | Observatory as Control Plane | 5 | Fleet management, Docker lifecycle, bootstrap orchestration, auth, TLS, vessel proxy |
| **DC** | Decoherence Fix | 1 | Bootstrap grace period, seed episodic memory, charter calibration, thread configurability |
| **2** | World Interaction | 4 | shell.exec, sandbox.exec, 6 transforms, WASM runtime (wasmtime Component Model), capability model, ConnectorRegistry, 2 reference WASM connectors |
| **4** | Communication & Relationships | 5 | Vessel-to-vessel messaging, WebSocket host functions, 3 WASM connectors (webhook.send, web.search, discord), streaming connector interface, capability escalation, Observatory PrincipalSelector |
| **5** | Self-Awareness & Evolution | 4 | IntrospectionService, multi-turn Decide, watch primitives, Meta-Cognition thread, Creative Synthesis thread, charter governance, tool discovery/hot-loading, Observatory relationship graph + trust charts + memory search |

### Test Counts

| Repo | Tests | Notes |
|------|-------|-------|
| Exoskeleton (Rust) | ~1,100 | 7 crates + 47 acceptance tests |
| WorldInterface (Rust) | ~630 | 10 crates + integration tests |
| Observatory (Rust) | ~58 | 1 crate |
| Observatory (TypeScript) | ~430 | 97 test files |
| ActionQueue (Rust) | ~940 | 11 crates, all features enabled |
| **Total** | **~3,160** | |

### Repository Layout

| Repo | Location | Crates | Role |
|------|----------|--------|------|
| Exoskeleton | `~/src/exoskeleton` | 7 | Agent runtime |
| Observatory | `~/src/observatory` | 1 + React SPA | Control plane |
| WorldInterface | `~/src/worldinterface` | 10 | Tool execution / workflow engine |
| ActionQueue | `~/src/actionqueue` | 11 | Task scheduling engine |

### Registered Connectors

| Connector | Type | Added | Notes |
|-----------|------|-------|-------|
| `delay` | Native | S0 | Scheduling primitive |
| `http.request` | Native | S0 | HTTP client, idempotency headers |
| `fs.read` | Native | S0 | Filesystem read |
| `fs.write` | Native | S0 | Filesystem write, marker-based idempotency |
| `shell.exec` | Native | E2-S1 | Container interaction, Align-gated (trust >= 0.6) |
| `sandbox.exec` | Native | E2-S2 | Isolated code execution environment |
| `peer.resolve` | Native | E4-S1 | Vessel-to-vessel communication |
| `json-validate` | WASM | E2-S4 | Reference connector, JSON schema validation |
| `streaming-echo` | WASM | E2-S4 | Reference streaming connector |
| `webhook.send` | WASM | E4-S2 | Outbound webhook delivery |
| `web.search` | WASM | E4-S2 | Web search integration |
| `discord` | WASM | E4-S3 | Discord Gateway (streaming, bidirectional) |

### ActionQueue Platform Features (Already Implemented)

These features exist in ActionQueue behind feature flags and are **not listed in v2**:

| Feature | Crate | Flag | What It Provides |
|---------|-------|------|------------------|
| **Actor System** | `actionqueue-actor` | `actor` | Actor registration, heartbeat monitoring, capability-based routing, department grouping |
| **Platform** | `actionqueue-platform` | `platform` | Multi-tenant isolation, RBAC (Operator/Auditor/Gatekeeper/Custom), append-only ledgers (audit, decision, relationship, incident) |
| **HTTP API** | `actionqueue-daemon` | — | REST API (`/api/v1/` introspection, `/api/v2/` actor/platform), Prometheus metrics, control endpoints |
| **Approval Workflows** | `actionqueue-platform` | `platform` | DAG-based Operator-Auditor-Gatekeeper approval chains |

---

## Remaining Work

```
Completed: Epoch 0, 3, 1, O, DC, 2, 4, 5
                │
                ├───────────────────┐
                │                   │
        Epoch 6: Scale       Epoch 7: Digicorp
        (2 sprints)          (3–5 sprints, revised down from 7–9)
        k8s, systemd, Nix    Platform integration + OrgSpec
                             (AQ platform features already exist)
```

Epochs 6 and 7 are independent of each other. Either can proceed first.

---

## Epoch 6: Scale & Production

**Theme:** Harden deployment infrastructure for production operation.

**Unchanged from v2.** Everything prior operates at localhost / single-host Docker scale. Production deployment requires Kubernetes orchestration, alternative deployment methods, and operational tooling.

### Work Items

| ID | Item | Complexity | Notes |
|----|------|------------|-------|
| W-45 | Kubernetes manifests | L | Helm chart or raw manifests. StatefulSet per vessel, shared Observatory. |
| W-46 | systemd unit files | S | Bare-metal deployment for non-Docker environments. |
| W-47 | Nix flake | M | Reproducible build + development environment. |
| W-10 | exoskeleton-host vessel.rs unit tests | M | Boot orchestration coverage. |

### Exit Criteria
- Kubernetes deployment documented and tested
- At least one alternative deployment method (systemd or Nix) available
- vessel.rs boot orchestration has unit test coverage

### Estimated: 2 sprints

---

## Epoch 7: Digicorp — Platform Integration & Organizational Coordination

**Theme:** Multi-agent organizations through a shared coordination hub.

**Scope revised significantly from v2.** The v2 plan assumed ActionQueue had no platform capabilities — it estimated 7-9 sprints to build multi-tenant dispatch, RBAC, actor system, HTTP API, and then integrate them. The post-E5 audit revealed that **all of these already exist** in ActionQueue behind feature flags.

**What remains:** Integration work — connecting Exoskeleton vessels to AQ's existing platform features via WorldInterface adapters, building the Observatory visualization layer, and proving cooperative multi-vessel goals.

### Phase 1: Platform Integration (2-3 sprints)

| ID | Item | Complexity | Notes |
|----|------|------------|-------|
| W-59 | Platform AQ WI adapter | L | WI connector: `platform.submit`, `platform.claim`, `platform.status`. Bridges per-vessel Tool AQ to shared AQ platform. Uses existing `actionqueue-daemon` HTTP API. |
| W-58 | Platform AQ deployment | M | Deploy AQ daemon as its own service with `--features actor,platform` enabled. Configuration, Docker compose integration with Observatory. **Not building the platform — deploying what exists.** |
| W-60 | OrgSpec role mapping | L | Map Exoskeleton vessel identities to AQ actor registrations. Operator/Auditor/Gatekeeper roles as dispatch policies. |

### Phase 2: Cooperative Goals + Observatory (2 sprints)

| ID | Item | Complexity | Notes |
|----|------|------------|-------|
| W-36 | Cooperative goals / shared missions | XL | Shared goal tracking through Platform AQ. Mission decomposition into sub-tasks routed by capability. |
| W-35 | Cross-agent message flow visualization | L | Observatory visualization of task flow through Platform AQ. |
| W-37 | Comparative side-by-side timelines | L | Multi-vessel tick timelines for coordinated work analysis. |

### Phase 3: Platform WorldInterface (deferred or optional)

The v2 plan included a "Platform WI service" (W-61, W-62) — a shared workflow engine running multi-step workflows independently of vessels. This is deferred because:

1. Individual vessels already have full WI capabilities (native + WASM connectors, FlowSpec workflows)
2. The Platform AQ already provides shared task coordination
3. A platform WI service adds complexity without clear near-term value
4. If needed, it can be built later using the same WI crate stack with a network-accessible daemon (the `worldinterface-daemon` already exists)

### Exit Criteria

**Phase 1:**
- Platform AQ runs as its own service, accessible via HTTP API
- Vessels submit tasks and claim tasks by capability via WI adapter
- OrgSpec roles constrain platform operations
- Platform survives restart (WAL durability — already guaranteed by AQ)

**Phase 2:**
- Two+ specialized vessels coordinate on a shared goal
- Observatory shows platform task queue and cross-vessel flow

### Estimated: 3-5 sprints (revised from 7-9)

---

## Floating Items — Observatory Polish

Slotted into any epoch as capacity allows:

| ID | Item | Complexity | Notes |
|----|------|------------|-------|
| W-31 | Artifact full-text search | M | Daemon endpoint + Observatory UI |
| W-32 | Cross-tick snapshot comparison | M | Two-snapshot diff view |
| W-38 | Force-directed fleet topology | M | d3-force graph (pairs with W-25, natural fit for E7 Phase 2) |

---

## Schema Migration Strategy

**New item not in v2.** Before production deployment (E6), all persistent stores need a migration story:

| Store | Location | Format | Migration Need |
|-------|----------|--------|----------------|
| 8 Exoskeleton SQLite stores | `{data_dir}/*.db` | Schema in Rust code | Need versioned migrations |
| WI ContextStore | `{data_dir}/context.db` | SQLite | Write-once, less churn |
| Observatory Fleet Store | `observatory.db` | SQLite | Need versioned migrations |
| AQ WAL | `{data_dir}/wal` | Postcard binary v5 | Version field in header |
| AQ Snapshots | `{data_dir}/snapshot` | Schema v8 | Version field in header |

Options: `refinery` (SQL migration runner), `sqlx` migrations, or hand-rolled version checks. Decision deferred to E6 sprint planning.

---

## Summary

| Phase | Theme | Sprints | Cumulative |
|-------|-------|---------|------------|
| ~~Decoherence Fix~~ | ~~Bootstrap stabilization~~ | ~~1~~ | ~~1~~ |
| ~~**Epoch 2**~~ | ~~World Interaction~~ | ~~4~~ | ~~5~~ |
| ~~**Epoch 4**~~ | ~~Communication & Relationships~~ | ~~5~~ | ~~10~~ |
| ~~**Epoch 5**~~ | ~~Self-Awareness & Evolution~~ | ~~4~~ | ~~14~~ |
| **Epoch 6** | Scale & Production | 2 | 16 |
| **Epoch 7** | Digicorp (Platform Integration) | 3-5 | 19-21 |
| — | Polish (floating) | — | — |

**Completed:** 25 sprints across 8 epochs
**Remaining:** ~4-7 sprints across 2 epochs + floating items

### Key Changes from v2

1. **Epochs 0-5 all complete** — marked with updated test counts and deliverables
2. **ActionQueue platform features documented** — actor, platform, RBAC, HTTP API already exist
3. **Epoch 7 reduced from 7-9 to 3-5 sprints** — integration work, not greenfield
4. **Platform WI (W-61, W-62) deferred** — unclear near-term value given per-vessel WI
5. **Schema migration strategy added** — new concern for production readiness
6. **Connector catalog updated** — 7 native + 5 WASM connectors
7. **W-38 (fleet topology) naturally fits E7 Phase 2** — flagged for scheduling
