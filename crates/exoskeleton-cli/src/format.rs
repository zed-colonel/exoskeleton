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
