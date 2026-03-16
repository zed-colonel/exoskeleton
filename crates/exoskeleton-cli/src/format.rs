//! Human-readable formatters for daemon API responses.
//!
//! Each function takes a `serde_json::Value` (or slice thereof) and returns
//! a formatted `String` suitable for terminal display. The formatters extract
//! key fields and present them in readable layouts. Unknown or missing fields
//! are handled gracefully with placeholder text.

use serde_json::Value;

/// Format a StateSnapshot for `exo inspect`.
pub fn format_snapshot(value: &Value) -> String {
    let vessel_id = value
        .get("vessel_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let tick_number = value
        .get("tick_number")
        .and_then(|v| v.as_u64())
        .map(|n| n.to_string())
        .unwrap_or_else(|| "unknown".into());
    let status = value
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let mission = value
        .get("mission")
        .and_then(|v| v.as_str())
        .unwrap_or("(none)");
    let plan = value
        .get("plan")
        .and_then(|v| v.as_str())
        .unwrap_or("(none)");
    let last_action = value
        .get("last_action_summary")
        .and_then(|v| v.as_str())
        .unwrap_or("(none)");
    let updated_at = value
        .get("updated_at")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let thread_count = value
        .get("thread_summaries")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let budget = value.get("budget_status");
    let budget_line = if let Some(b) = budget {
        let local = b
            .get("local_tokens_remaining")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let frontier = b
            .get("frontier_tokens_remaining")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let thrash = b
            .get("thrash_level")
            .and_then(|v| v.as_str())
            .unwrap_or("none");
        format!("local: {local}, frontier: {frontier}, thrash: {thrash}")
    } else {
        "(no budget data)".into()
    };

    format!(
        "\
=== Vessel Status ===
  Vessel ID:    {vessel_id}
  Tick:         {tick_number}
  Status:       {status}
  Mission:      {mission}
  Plan:         {plan}
  Last Action:  {last_action}
  Threads:      {thread_count} active
  Budget:       {budget_line}
  Updated:      {updated_at}"
    )
}

/// Format a list of TickRecords for `exo inspect ticks`.
pub fn format_ticks(values: &[Value]) -> String {
    if values.is_empty() {
        return "No ticks recorded yet.".into();
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "{:<38}  {:>6}  {:<10}  {:<20}  {}",
        "Tick ID", "Number", "Phase", "Started At", "Duration"
    ));
    lines.push(format!(
        "{:<38}  {:>6}  {:<10}  {:<20}  {}",
        str::repeat("\u{2500}", 36),
        str::repeat("\u{2500}", 6),
        str::repeat("\u{2500}", 10),
        str::repeat("\u{2500}", 20),
        str::repeat("\u{2500}", 10),
    ));

    for tick in values {
        let tick_id = tick
            .get("tick_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let number = tick
            .get("tick_number")
            .and_then(|v| v.as_u64())
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".into());
        let phase = tick.get("phase").and_then(|v| v.as_str()).unwrap_or("?");
        let started = tick
            .get("started_at")
            .and_then(|v| v.as_str())
            .map(truncate_timestamp)
            .unwrap_or_else(|| "?".into());
        let duration = compute_duration(tick);

        lines.push(format!(
            "{:<38}  {:>6}  {:<10}  {:<20}  {}",
            tick_id, number, phase, started, duration
        ));
    }

    lines.join("\n")
}

/// Format a single TickRecord for `exo inspect tick <id>`.
pub fn format_tick(value: &Value) -> String {
    let tick_id = value
        .get("tick_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let number = value
        .get("tick_number")
        .and_then(|v| v.as_u64())
        .map(|n| n.to_string())
        .unwrap_or_else(|| "?".into());
    let phase = value.get("phase").and_then(|v| v.as_str()).unwrap_or("?");
    let started = value
        .get("started_at")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let completed = value
        .get("completed_at")
        .and_then(|v| v.as_str())
        .unwrap_or("(in progress)");
    let rationale = value
        .get("decision_rationale")
        .and_then(|v| v.as_str())
        .unwrap_or("(none)");

    let actions = value
        .get("actions_taken")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let llm_calls = value
        .get("llm_calls")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let threads = value
        .get("thread_contributions")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let mut out = format!(
        "\
=== Tick Detail ===
  Tick ID:      {tick_id}
  Number:       {number}
  Phase:        {phase}
  Started:      {started}
  Completed:    {completed}
  Rationale:    {rationale}
  Actions:      {actions}
  LLM Calls:    {llm_calls}
  Thread Contributions: {threads}"
    );

    // Show action details if present
    if let Some(arr) = value.get("actions_taken").and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            out.push_str("\n\n  Actions:");
            for a in arr {
                let tool = a.get("tool_name").and_then(|v| v.as_str()).unwrap_or("?");
                let outcome = a.get("outcome").and_then(|v| v.as_str()).unwrap_or("?");
                out.push_str(&format!("\n    - {tool}: {outcome}"));
            }
        }
    }

    out
}

/// Format thread list for `exo thread list`.
pub fn format_threads(values: &[Value]) -> String {
    if values.is_empty() {
        return "No threads registered.".into();
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "{:<10}  {:<24}  {:<10}  {:<16}  {:<10}",
        "ID (short)", "Name", "Priority", "Schedule", "Status"
    ));
    lines.push(format!(
        "{:<10}  {:<24}  {:<10}  {:<16}  {:<10}",
        str::repeat("\u{2500}", 10),
        str::repeat("\u{2500}", 24),
        str::repeat("\u{2500}", 10),
        str::repeat("\u{2500}", 16),
        str::repeat("\u{2500}", 10),
    ));

    for thread in values {
        let id = thread
            .get("thread_id")
            .and_then(|v| v.as_str())
            .map(short_id)
            .unwrap_or_else(|| "?".into());
        let name = thread.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let priority = thread
            .get("priority")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let schedule = format_schedule(thread.get("schedule"));
        let status = thread.get("status").and_then(|v| v.as_str()).unwrap_or("?");

        lines.push(format!(
            "{:<10}  {:<24}  {:<10}  {:<16}  {:<10}",
            id, name, priority, schedule, status
        ));
    }

    lines.join("\n")
}

/// Format budget status for `exo budget`.
pub fn format_budget(value: &Value) -> String {
    let mut out = String::from("=== Budget Status ===\n");

    if let Some(cog) = value.get("cognitive") {
        out.push_str("\n  Cognitive Budget:\n");
        let local = cog
            .get("local_tokens_remaining")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let frontier = cog
            .get("frontier_tokens_remaining")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cost = cog
            .get("frontier_cost_cents_remaining")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let frontier_calls = cog
            .get("frontier_calls_this_window")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let failures = cog
            .get("consecutive_failures")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let thrash = cog
            .get("thrash_level")
            .and_then(|v| v.as_str())
            .unwrap_or("none");
        let window_secs = cog
            .get("window_duration_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let window_start = cog
            .get("window_start")
            .and_then(|v| v.as_str())
            .unwrap_or("?");

        out.push_str(&format!("    Local Tokens:     {local}\n"));
        out.push_str(&format!("    Frontier Tokens:  {frontier}\n"));
        out.push_str(&format!(
            "    Frontier Cost:    {:.2} cents\n",
            cost as f64 / 100.0
        ));
        out.push_str(&format!("    Frontier Calls:   {frontier_calls}\n"));
        out.push_str(&format!("    Failures:         {failures}\n"));
        out.push_str(&format!("    Thrash Level:     {thrash}\n"));
        out.push_str(&format!(
            "    Window:           {window_secs}s (from {window_start})\n"
        ));
    } else {
        out.push_str("\n  Cognitive Budget:   (not configured)\n");
    }

    if let Some(tool) = value.get("tool") {
        out.push_str("\n  Tool Budget:\n");
        let remaining = tool
            .get("invocations_remaining")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let this_window = tool
            .get("invocations_this_window")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let window_start = tool
            .get("window_start")
            .and_then(|v| v.as_str())
            .unwrap_or("?");

        out.push_str(&format!("    Invocations Left: {remaining}\n"));
        out.push_str(&format!("    Used This Window: {this_window}\n"));
        out.push_str(&format!("    Window Start:     {window_start}\n"));
    } else {
        out.push_str("\n  Tool Budget:        (not configured)\n");
    }

    out
}

/// Format engine status for `exo engines`.
pub fn format_engines(value: &Value) -> String {
    let mut out = String::from("=== Engine Status ===\n");

    if let Some(cog) = value.get("cognitive") {
        out.push_str("\n  Cognitive AQ:\n");
        let available = cog
            .get("available")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let last_tick = cog
            .get("last_tick_number")
            .and_then(|v| v.as_u64())
            .map(|n| n.to_string())
            .unwrap_or_else(|| "(none)".into());
        let last_at = cog
            .get("last_tick_at")
            .and_then(|v| v.as_str())
            .unwrap_or("(never)");
        let status = cog
            .get("vessel_status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let threads = cog
            .get("active_threads")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let indicator = if available { "UP" } else { "DOWN" };
        out.push_str(&format!("    Status:           {indicator}\n"));
        out.push_str(&format!("    Vessel Status:    {status}\n"));
        out.push_str(&format!("    Last Tick:        #{last_tick}\n"));
        out.push_str(&format!("    Last Tick At:     {last_at}\n"));
        out.push_str(&format!("    Active Threads:   {threads}\n"));
    }

    if let Some(tool) = value.get("tool") {
        out.push_str("\n  Tool AQ (WI Host):\n");
        let available = tool
            .get("available")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let caps = tool
            .get("capabilities_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let flows = tool
            .get("active_flows")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let indicator = if available { "UP" } else { "DOWN" };
        out.push_str(&format!("    Status:           {indicator}\n"));
        out.push_str(&format!("    Capabilities:     {caps}\n"));
        out.push_str(&format!("    Active Flows:     {flows}\n"));
    }

    out
}

/// Format event list for `exo events`.
pub fn format_events(values: &[Value]) -> String {
    if values.is_empty() {
        return "No events recorded yet.".into();
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "{:<20}  {:<22}  {}",
        "Timestamp", "Type", "Summary"
    ));
    lines.push(format!(
        "{:<20}  {:<22}  {}",
        str::repeat("\u{2500}", 20),
        str::repeat("\u{2500}", 22),
        str::repeat("\u{2500}", 40),
    ));

    for event in values {
        let timestamp = event
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(truncate_timestamp)
            .unwrap_or_else(|| "?".into());
        let event_type = event
            .get("event_type")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let summary = event
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("(no summary)");

        lines.push(format!(
            "{:<20}  {:<22}  {}",
            timestamp, event_type, summary
        ));
    }

    lines.join("\n")
}

/// Format relationship snapshot for `exo relationship show`.
pub fn format_relationships(value: &Value) -> String {
    let principals = value
        .get("principals")
        .and_then(|v| v.as_array())
        .or_else(|| value.as_array());

    let principals = match principals {
        Some(arr) if !arr.is_empty() => arr,
        _ => return "No relationship data available.".into(),
    };

    let mut lines = Vec::new();
    lines.push(format!(
        "{:<38}  {:>6}  {:>12}  {}",
        "Principal ID", "Trust", "Interactions", "Last Seen"
    ));
    lines.push(format!(
        "{:<38}  {:>6}  {:>12}  {}",
        str::repeat("\u{2500}", 36),
        str::repeat("\u{2500}", 6),
        str::repeat("\u{2500}", 12),
        str::repeat("\u{2500}", 20),
    ));

    for p in principals {
        let pid = p
            .get("principal_id")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let trust = p
            .get("trust_level")
            .and_then(|v| v.as_f64())
            .map(|t| format!("{:.2}", t))
            .unwrap_or_else(|| "?".into());
        let interactions = p
            .get("interaction_count")
            .and_then(|v| v.as_u64())
            .map(|n| n.to_string())
            .unwrap_or_else(|| "0".into());
        let last_seen = p
            .get("last_interaction")
            .and_then(|v| v.as_str())
            .map(truncate_timestamp)
            .unwrap_or_else(|| "(never)".into());

        lines.push(format!(
            "{:<38}  {:>6}  {:>12}  {}",
            pid, trust, interactions, last_seen
        ));
    }

    lines.join("\n")
}

// ── Helpers ──

/// Truncate an ISO timestamp to "YYYY-MM-DD HH:MM:SS" for display.
fn truncate_timestamp(s: &str) -> String {
    // ISO timestamps look like "2026-03-12T14:30:00.123456Z"
    // Truncate to 19 chars for display.
    let cleaned = s.replace('T', " ");
    if cleaned.len() > 19 {
        cleaned[..19].to_string()
    } else {
        cleaned
    }
}

/// Extract a short ID (first 8 hex chars) from a UUID string.
fn short_id(uuid_str: &str) -> String {
    if uuid_str.len() >= 8 {
        uuid_str[..8].to_string()
    } else {
        uuid_str.to_string()
    }
}

/// Compute a human-readable duration from started_at and completed_at.
fn compute_duration(tick: &Value) -> String {
    let started = tick.get("started_at").and_then(|v| v.as_str());
    let completed = tick.get("completed_at").and_then(|v| v.as_str());

    match (started, completed) {
        (Some(s), Some(c)) => {
            if let (Ok(start), Ok(end)) = (
                chrono::DateTime::parse_from_rfc3339(s),
                chrono::DateTime::parse_from_rfc3339(c),
            ) {
                let dur = end - start;
                let ms = dur.num_milliseconds();
                if ms < 1000 {
                    format!("{ms}ms")
                } else {
                    format!("{:.1}s", ms as f64 / 1000.0)
                }
            } else {
                "(parse err)".into()
            }
        }
        _ => "(running)".into(),
    }
}

/// Format a thread schedule value.
fn format_schedule(schedule: Option<&Value>) -> String {
    match schedule {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(obj)) => {
            // Handle tagged enum format like {"EveryNTicks": 5}
            if let Some((key, val)) = obj.iter().next() {
                format!("{key}({val})")
            } else {
                "?".into()
            }
        }
        _ => "?".into(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // ── E0-T21: format_snapshot produces expected layout ──

    #[test]
    fn format_snapshot_produces_expected_layout() {
        let value = json!({
            "vessel_id": "550e8400-e29b-41d4-a716-446655440000",
            "tick_number": 42,
            "status": "Active",
            "mission": "Monitor infrastructure",
            "plan": "Check API health",
            "last_action_summary": "Called health endpoint",
            "updated_at": "2026-03-15T10:30:00Z",
            "thread_summaries": [
                {"name": "ThreatMon"},
                {"name": "SelfCrit"},
                {"name": "MemConsol"}
            ],
            "budget_status": {
                "local_tokens_remaining": 50000,
                "frontier_tokens_remaining": 10000,
                "thrash_level": "none"
            }
        });

        let output = format_snapshot(&value);

        assert!(output.contains("=== Vessel Status ==="), "missing header");
        assert!(
            output.contains("550e8400-e29b-41d4-a716-446655440000"),
            "missing vessel_id"
        );
        assert!(output.contains("42"), "missing tick number");
        assert!(output.contains("Active"), "missing status");
        assert!(output.contains("Monitor infrastructure"), "missing mission");
        assert!(output.contains("Check API health"), "missing plan");
        assert!(output.contains("3 active"), "missing thread count");
        assert!(
            output.contains("local: 50000"),
            "missing budget local tokens"
        );
    }

    // ── E0-T22: format_ticks produces tabular output ──

    #[test]
    fn format_ticks_produces_tabular_output() {
        let values = vec![
            json!({
                "tick_id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                "tick_number": 1,
                "phase": "Completed",
                "started_at": "2026-03-15T10:00:00.000Z",
                "completed_at": "2026-03-15T10:00:00.523Z"
            }),
            json!({
                "tick_id": "11111111-2222-3333-4444-555555555555",
                "tick_number": 2,
                "phase": "Decide",
                "started_at": "2026-03-15T10:01:00.000Z"
            }),
        ];

        let output = format_ticks(&values);

        assert!(output.contains("Tick ID"), "missing Tick ID header");
        assert!(output.contains("Number"), "missing Number header");
        assert!(output.contains("Phase"), "missing Phase header");
        assert!(output.contains("Started At"), "missing Started At header");
        assert!(output.contains("Duration"), "missing Duration header");
        assert!(output.contains("\u{2500}"), "missing box-drawing separator");
        assert!(output.contains("1"), "missing tick number 1");
        assert!(output.contains("2"), "missing tick number 2");
        assert!(
            output.contains("523ms"),
            "missing duration for completed tick"
        );
        assert!(output.contains("(running)"), "missing running indicator");
    }

    // ── E0-T23: format_tick produces detailed single-tick view ──

    #[test]
    fn format_tick_produces_detailed_view() {
        let value = json!({
            "tick_id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "tick_number": 7,
            "phase": "Completed",
            "started_at": "2026-03-15T10:00:00Z",
            "completed_at": "2026-03-15T10:00:01.200Z",
            "decision_rationale": "High threat detected, escalating",
            "actions_taken": [
                {"tool_name": "http.request", "outcome": "Success"},
                {"tool_name": "fs.write", "outcome": "Success"}
            ],
            "llm_calls": [
                {"backend": "local", "tokens": 1500}
            ],
            "thread_contributions": [
                {"thread": "ThreatMon"},
                {"thread": "SelfCrit"},
                {"thread": "MemConsol"}
            ]
        });

        let output = format_tick(&value);

        assert!(output.contains("=== Tick Detail ==="), "missing header");
        assert!(
            output.contains("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
            "missing tick_id"
        );
        assert!(output.contains("7"), "missing tick number");
        assert!(
            output.contains("High threat detected, escalating"),
            "missing rationale"
        );
        assert!(output.contains("Actions:      2"), "missing action count");
        assert!(output.contains("LLM Calls:    1"), "missing LLM call count");
        assert!(
            output.contains("Thread Contributions: 3"),
            "missing thread count"
        );
        assert!(output.contains("http.request"), "missing first action tool");
        assert!(output.contains("fs.write"), "missing second action tool");
    }

    // ── E0-T24: format_threads produces table with short IDs ──

    #[test]
    fn format_threads_produces_table_with_short_ids() {
        let values = vec![
            json!({
                "thread_id": "550e8400-e29b-41d4-a716-446655440000",
                "name": "Threat Monitor",
                "priority": "Critical",
                "schedule": "EveryTick",
                "status": "Active"
            }),
            json!({
                "thread_id": "661f9511-f3ac-52e5-b827-557766551111",
                "name": "Self-Critique",
                "priority": "High",
                "schedule": "EveryTick",
                "status": "Active"
            }),
            json!({
                "thread_id": "772a0622-04bd-63f6-c938-668877662222",
                "name": "Memory Consolidation",
                "priority": "Normal",
                "schedule": {"EveryNTicks": 5},
                "status": "Active"
            }),
        ];

        let output = format_threads(&values);

        assert!(output.contains("ID (short)"), "missing ID header");
        assert!(output.contains("Name"), "missing Name header");
        assert!(output.contains("Priority"), "missing Priority header");
        assert!(output.contains("Schedule"), "missing Schedule header");
        assert!(output.contains("Status"), "missing Status header");
        assert!(output.contains("Threat Monitor"), "missing thread name");
        assert!(output.contains("Critical"), "missing Critical priority");
        assert!(output.contains("High"), "missing High priority");
        assert!(output.contains("Normal"), "missing Normal priority");
        assert!(output.contains("EveryTick"), "missing EveryTick schedule");
        assert!(
            output.contains("EveryNTicks(5)"),
            "missing EveryNTicks schedule"
        );
        // Thread IDs should be truncated to 8 chars
        assert!(output.contains("550e8400"), "missing short thread ID");
        assert!(
            !output.contains("550e8400-e29b"),
            "thread ID should be truncated"
        );
    }

    // ── E0-T25: format_budget produces cognitive + tool sections ──

    #[test]
    fn format_budget_produces_cognitive_and_tool_sections() {
        let value = json!({
            "cognitive": {
                "local_tokens_remaining": 900000,
                "frontier_tokens_remaining": 95000,
                "frontier_cost_cents_remaining": 500,
                "frontier_calls_this_window": 3,
                "consecutive_failures": 0,
                "thrash_level": "none",
                "window_duration_secs": 3600,
                "window_start": "2026-03-15T10:00:00Z"
            },
            "tool": {
                "invocations_remaining": 950,
                "invocations_this_window": 50,
                "window_start": "2026-03-15T10:00:00Z"
            }
        });

        let output = format_budget(&value);

        assert!(output.contains("=== Budget Status ==="), "missing header");
        assert!(
            output.contains("Cognitive Budget:"),
            "missing cognitive section"
        );
        assert!(output.contains("Tool Budget:"), "missing tool section");
        assert!(
            output.contains("Local Tokens:"),
            "missing local tokens line"
        );
        assert!(output.contains("900000"), "missing local token value");
        assert!(
            output.contains("Frontier Tokens:"),
            "missing frontier tokens line"
        );
        assert!(output.contains("95000"), "missing frontier token value");
        assert!(output.contains("Frontier Cost:"), "missing cost line");
        assert!(output.contains("5.00 cents"), "missing formatted cost");
        assert!(output.contains("Thrash Level:"), "missing thrash level");
        assert!(
            output.contains("Invocations Left:"),
            "missing tool invocations"
        );
        assert!(output.contains("950"), "missing tool invocation value");

        // Also test unconfigured budget
        let empty = json!({});
        let empty_output = format_budget(&empty);
        assert!(
            empty_output.contains("(not configured)"),
            "missing unconfigured fallback"
        );
        // Should have (not configured) for both sections
        assert_eq!(
            empty_output.matches("(not configured)").count(),
            2,
            "should show (not configured) for both cognitive and tool"
        );
    }

    // ── E0-T26: format_engines produces UP/DOWN indicators ──

    #[test]
    fn format_engines_produces_up_down_indicators() {
        let value = json!({
            "cognitive": {
                "available": true,
                "last_tick_number": 42,
                "last_tick_at": "2026-03-15T10:30:00Z",
                "vessel_status": "Active",
                "active_threads": 3
            },
            "tool": {
                "available": false,
                "capabilities_count": 4,
                "active_flows": 0
            }
        });

        let output = format_engines(&value);

        assert!(output.contains("=== Engine Status ==="), "missing header");
        assert!(
            output.contains("Cognitive AQ:"),
            "missing cognitive section"
        );
        assert!(
            output.contains("Tool AQ (WI Host):"),
            "missing tool section"
        );
        // Cognitive should be UP, tool should be DOWN
        let cog_section = output.split("Tool AQ").next().unwrap();
        assert!(cog_section.contains("UP"), "cognitive should be UP");
        let tool_section = output.split("Tool AQ").nth(1).unwrap();
        assert!(tool_section.contains("DOWN"), "tool should be DOWN");
        assert!(output.contains("Last Tick:"), "missing last tick line");
        assert!(output.contains("#42"), "missing tick number");
        assert!(output.contains("Capabilities:"), "missing capabilities");
        assert!(output.contains("4"), "missing capabilities count");
    }

    // ── E0-T27: format_events produces timestamped table ──

    #[test]
    fn format_events_produces_timestamped_table() {
        let values = vec![
            json!({
                "timestamp": "2026-03-15T10:00:00.123456Z",
                "event_type": "VesselStarted",
                "summary": "Vessel boot completed"
            }),
            json!({
                "timestamp": "2026-03-15T10:01:00.500Z",
                "event_type": "ActionExecuted",
                "summary": "Called external API"
            }),
            json!({
                "timestamp": "2026-03-15T10:02:00.000Z",
                "event_type": "TickCompleted",
                "summary": "Tick 5 finished"
            }),
        ];

        let output = format_events(&values);

        assert!(output.contains("Timestamp"), "missing Timestamp header");
        assert!(output.contains("Type"), "missing Type header");
        assert!(output.contains("Summary"), "missing Summary header");
        assert!(output.contains("VesselStarted"), "missing VesselStarted");
        assert!(output.contains("ActionExecuted"), "missing ActionExecuted");
        assert!(output.contains("TickCompleted"), "missing TickCompleted");
        assert!(
            output.contains("Vessel boot completed"),
            "missing first summary"
        );
        assert!(
            output.contains("Called external API"),
            "missing second summary"
        );
        assert!(output.contains("Tick 5 finished"), "missing third summary");
        // Timestamps should be truncated (no fractional seconds, no 'T')
        assert!(
            output.contains("2026-03-15 10:00:00"),
            "timestamp not truncated"
        );
        assert!(
            !output.contains(".123456Z"),
            "fractional seconds should be stripped"
        );
    }

    // ── E0-T28: format_relationships produces trust table ──

    #[test]
    fn format_relationships_produces_trust_table() {
        let value = json!({
            "principals": [
                {
                    "principal_id": "operator-alice",
                    "trust_level": 0.85,
                    "interaction_count": 42,
                    "last_interaction": "2026-03-15T10:30:00.000Z"
                },
                {
                    "principal_id": "service-bot",
                    "trust_level": 0.50,
                    "interaction_count": 7,
                    "last_interaction": "2026-03-14T08:00:00.000Z"
                }
            ]
        });

        let output = format_relationships(&value);

        assert!(
            output.contains("Principal ID"),
            "missing Principal ID header"
        );
        assert!(output.contains("Trust"), "missing Trust header");
        assert!(
            output.contains("Interactions"),
            "missing Interactions header"
        );
        assert!(output.contains("Last Seen"), "missing Last Seen header");
        assert!(output.contains("operator-alice"), "missing first principal");
        assert!(output.contains("service-bot"), "missing second principal");
        assert!(output.contains("0.85"), "missing first trust level");
        assert!(output.contains("0.50"), "missing second trust level");
        assert!(output.contains("42"), "missing interaction count");

        // Empty principals
        let empty = json!({"principals": []});
        assert_eq!(
            format_relationships(&empty),
            "No relationship data available."
        );
    }

    // ── E0-T29: Formatters handle empty input ──

    #[test]
    fn formatters_handle_empty_input() {
        assert_eq!(format_ticks(&[]), "No ticks recorded yet.");
        assert_eq!(format_threads(&[]), "No threads registered.");
        assert_eq!(format_events(&[]), "No events recorded yet.");
        assert_eq!(
            format_relationships(&json!({})),
            "No relationship data available."
        );
    }

    // ── E0-T30: Formatters handle missing fields gracefully ──

    #[test]
    fn formatters_handle_missing_fields_gracefully() {
        let empty = json!({});

        let snapshot_out = format_snapshot(&empty);
        assert!(
            snapshot_out.contains("unknown"),
            "snapshot should show unknown for vessel_id"
        );

        let tick_out = format_tick(&empty);
        assert!(
            tick_out.contains("unknown"),
            "tick should show unknown for tick_id"
        );

        let budget_out = format_budget(&empty);
        assert!(
            budget_out.contains("(not configured)"),
            "budget should show not configured"
        );

        // format_engines should not panic on empty object
        let engines_out = format_engines(&empty);
        assert!(
            engines_out.contains("=== Engine Status ==="),
            "engines should show header"
        );
    }

    // ── E0-T31: Helper functions produce correct output ──

    #[test]
    fn helper_functions_produce_correct_output() {
        // truncate_timestamp
        assert_eq!(
            truncate_timestamp("2026-03-12T14:30:00.123456Z"),
            "2026-03-12 14:30:00"
        );
        assert_eq!(
            truncate_timestamp("2026-03-12T14:30:00Z"),
            "2026-03-12 14:30:00"
        );
        assert_eq!(truncate_timestamp("short"), "short");

        // short_id
        assert_eq!(short_id("550e8400-e29b-41d4-a716-446655440000"), "550e8400");
        assert_eq!(short_id("abc"), "abc");

        // compute_duration
        let tick_completed = json!({
            "started_at": "2026-03-15T10:00:00.000Z",
            "completed_at": "2026-03-15T10:00:00.523Z"
        });
        assert_eq!(compute_duration(&tick_completed), "523ms");

        let tick_slow = json!({
            "started_at": "2026-03-15T10:00:00.000Z",
            "completed_at": "2026-03-15T10:00:02.500Z"
        });
        assert_eq!(compute_duration(&tick_slow), "2.5s");

        let tick_running = json!({
            "started_at": "2026-03-15T10:00:00.000Z"
        });
        assert_eq!(compute_duration(&tick_running), "(running)");

        let tick_empty = json!({});
        assert_eq!(compute_duration(&tick_empty), "(running)");

        // format_schedule
        assert_eq!(
            format_schedule(Some(&Value::String("EveryTick".into()))),
            "EveryTick"
        );
        assert_eq!(
            format_schedule(Some(&json!({"EveryNTicks": 5}))),
            "EveryNTicks(5)"
        );
        assert_eq!(format_schedule(None), "?");
    }
}
