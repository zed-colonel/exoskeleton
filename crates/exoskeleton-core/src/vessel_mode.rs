use serde::{Deserialize, Serialize};

/// Vessel operating mode. Affects which tools pass the policy gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum VesselMode {
    /// Normal operation — policy engine uses configured rules.
    #[default]
    Normal,
    /// Planning — only read-only tools allowed regardless of policy config.
    /// Agent should produce a PlanDraft artifact.
    Planning,
    /// Executing — policy engine uses configured rules.
    /// Requires a PlanApproved artifact linking to the active PlanDraft.
    Executing,
}

#[cfg(test)]
mod tests {
    use super::VesselMode;

    #[test]
    fn vessel_mode_default_is_normal() {
        assert_eq!(VesselMode::default(), VesselMode::Normal);
    }

    #[test]
    fn vessel_mode_serde_roundtrip() {
        for (mode, expected) in [
            (VesselMode::Normal, "\"normal\""),
            (VesselMode::Planning, "\"planning\""),
            (VesselMode::Executing, "\"executing\""),
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(json, expected);
            let parsed: VesselMode = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, mode);
        }
    }
}
