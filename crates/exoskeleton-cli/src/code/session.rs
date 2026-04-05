//! Session state persistence for `exo code`.
//!
//! Stores TUI-local state (scroll position, debug visibility, preferences) in
//! `~/.config/exo/sessions/{vessel_id}/`. Uses the `directories` crate for
//! XDG-compliant paths.
//!
//! Session state is TUI-side only. Conversation history lives on the vessel.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Session state persisted between TUI runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionState {
    /// Vessel this session connects to.
    pub vessel_id: String,
    /// Active conversation ID (for history loading).
    pub conversation_id: String,
    /// Scroll position in the conversation area.
    pub scroll_position: u16,
    /// Whether the debug banner was visible when the session was saved.
    pub debug_visible: bool,
    /// When this session state was saved.
    pub timestamp: DateTime<Utc>,
}

/// User preferences persisted across sessions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Preferences {
    /// Default debug banner visibility.
    #[serde(default)]
    pub debug_view: bool,
}

/// Maximum age for a session to be considered "recent" for auto-resume.
const SESSION_MAX_AGE_HOURS: i64 = 24;

/// Get the base configuration directory for exo.
///
/// Returns `~/.config/exo/` on Linux (XDG_CONFIG_HOME/exo/).
fn config_base_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "exoskeleton", "exo")
        .map(|dirs| dirs.config_dir().to_path_buf())
}

/// Get the session directory for a specific vessel.
///
/// Returns `~/.config/exo/sessions/{vessel_id}/`.
pub fn session_dir(vessel_id: &str) -> Option<PathBuf> {
    config_base_dir().map(|base| base.join("sessions").join(vessel_id))
}

/// Path to the session state file for a vessel.
fn session_file_path(vessel_id: &str) -> Option<PathBuf> {
    session_dir(vessel_id).map(|dir| dir.join("last_session.json"))
}

/// Path to the preferences file for a vessel.
fn preferences_file_path(vessel_id: &str) -> Option<PathBuf> {
    session_dir(vessel_id).map(|dir| dir.join("preferences.json"))
}

/// Save session state to disk.
///
/// Creates the directory structure if it does not exist.
pub fn save_session(state: &SessionState) -> Result<(), SessionError> {
    let path = session_file_path(&state.vessel_id).ok_or(SessionError::NoConfigDir)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| SessionError::Io(format!("create dir: {e}")))?;
    }

    let json =
        serde_json::to_string_pretty(state).map_err(|e| SessionError::Serialize(e.to_string()))?;
    std::fs::write(&path, json).map_err(|e| SessionError::Io(format!("write session: {e}")))?;

    Ok(())
}

/// Load session state from disk.
///
/// Returns `Ok(None)` if the file does not exist.
pub fn load_session(vessel_id: &str) -> Result<Option<SessionState>, SessionError> {
    let path = match session_file_path(vessel_id) {
        Some(p) => p,
        None => return Ok(None),
    };

    if !path.exists() {
        return Ok(None);
    }

    let json = std::fs::read_to_string(&path)
        .map_err(|e| SessionError::Io(format!("read session: {e}")))?;
    let state: SessionState = serde_json::from_str(&json)
        .map_err(|e| SessionError::Serialize(format!("parse session: {e}")))?;

    Ok(Some(state))
}

/// Check whether a session state is recent enough for auto-resume.
pub fn is_session_recent(state: &SessionState) -> bool {
    let age = Utc::now().signed_duration_since(state.timestamp);
    age.num_hours() < SESSION_MAX_AGE_HOURS
}

/// Save user preferences to disk.
pub fn save_preferences(vessel_id: &str, prefs: &Preferences) -> Result<(), SessionError> {
    let path = preferences_file_path(vessel_id).ok_or(SessionError::NoConfigDir)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| SessionError::Io(format!("create dir: {e}")))?;
    }

    let json =
        serde_json::to_string_pretty(prefs).map_err(|e| SessionError::Serialize(e.to_string()))?;
    std::fs::write(&path, json).map_err(|e| SessionError::Io(format!("write preferences: {e}")))?;

    Ok(())
}

/// Load user preferences from disk.
///
/// Returns default preferences if the file does not exist.
pub fn load_preferences(vessel_id: &str) -> Result<Preferences, SessionError> {
    let path = match preferences_file_path(vessel_id) {
        Some(p) => p,
        None => return Ok(Preferences::default()),
    };

    if !path.exists() {
        return Ok(Preferences::default());
    }

    let json = std::fs::read_to_string(&path)
        .map_err(|e| SessionError::Io(format!("read preferences: {e}")))?;
    let prefs: Preferences = serde_json::from_str(&json)
        .map_err(|e| SessionError::Serialize(format!("parse preferences: {e}")))?;

    Ok(prefs)
}

/// Errors from session persistence operations.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("no XDG config directory available")]
    NoConfigDir,
    #[error("I/O error: {0}")]
    Io(String),
    #[error("serialization error: {0}")]
    Serialize(String),
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    // ── T9: session_state_serialization_round_trip ──

    #[test]
    fn session_state_serialization_round_trip() {
        let state = SessionState {
            vessel_id: "vessel-abc-123".into(),
            conversation_id: "conv-xyz-456".into(),
            scroll_position: 42,
            debug_visible: true,
            timestamp: Utc::now(),
        };

        let json = serde_json::to_string_pretty(&state).unwrap();
        let parsed: SessionState = serde_json::from_str(&json).unwrap();

        assert_eq!(state.vessel_id, parsed.vessel_id);
        assert_eq!(state.conversation_id, parsed.conversation_id);
        assert_eq!(state.scroll_position, parsed.scroll_position);
        assert_eq!(state.debug_visible, parsed.debug_visible);
    }

    // ── T10: session_state_recent_check ──

    #[test]
    fn session_state_recent_check() {
        let recent = SessionState {
            vessel_id: "v".into(),
            conversation_id: "c".into(),
            scroll_position: 0,
            debug_visible: false,
            timestamp: Utc::now() - chrono::Duration::hours(1),
        };
        assert!(is_session_recent(&recent));

        let stale = SessionState {
            vessel_id: "v".into(),
            conversation_id: "c".into(),
            scroll_position: 0,
            debug_visible: false,
            timestamp: Utc::now() - chrono::Duration::hours(25),
        };
        assert!(!is_session_recent(&stale));
    }

    // ── T11: session_state_missing_file_returns_none ──

    #[test]
    fn session_state_missing_file_returns_none() {
        let result = load_session("nonexistent-vessel-id-999");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    // ── T12: session_dir_path_includes_vessel_id ──

    #[test]
    fn session_dir_path_includes_vessel_id() {
        let dir = session_dir("vessel-test-42");
        if let Some(ref path) = dir {
            let path_str = path.to_string_lossy();
            assert!(
                path_str.contains("vessel-test-42"),
                "session dir should contain vessel_id, got: {path_str}"
            );
            assert!(
                path_str.contains("sessions"),
                "session dir should contain 'sessions', got: {path_str}"
            );
        }
    }

    // ── T13: preferences_round_trip ──

    #[test]
    fn preferences_round_trip() {
        let prefs = Preferences { debug_view: true };
        let json = serde_json::to_string_pretty(&prefs).unwrap();
        let parsed: Preferences = serde_json::from_str(&json).unwrap();
        assert_eq!(prefs, parsed);
    }

    #[test]
    fn preferences_default_debug_view_false() {
        let prefs = Preferences::default();
        assert!(!prefs.debug_view);
    }

    #[test]
    fn session_state_json_format() {
        let state = SessionState {
            vessel_id: "vessel-abc-123".into(),
            conversation_id: "conv-xyz-456".into(),
            scroll_position: 42,
            debug_visible: true,
            timestamp: chrono::DateTime::parse_from_rfc3339("2026-04-04T18:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };

        let json = serde_json::to_string_pretty(&state).unwrap();
        assert!(json.contains("\"vessel_id\""));
        assert!(json.contains("\"conversation_id\""));
        assert!(json.contains("\"scroll_position\""));
        assert!(json.contains("\"debug_visible\""));
        assert!(json.contains("\"timestamp\""));
    }
}
