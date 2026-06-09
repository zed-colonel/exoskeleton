//! Initiative thread — self-directed behavior and engagement generation.
//!
//! Runs every 3 ticks at High priority. Examines vessel state and produces
//! working memory nudges that encourage the Decide step to take proactive action.

use exoskeleton_core::{ThreadFlavor, ThreadPriority, ThreadRole, ThreadSchedule, ThreadSpec};

use super::INITIATIVE_ID;

pub const INITIATIVE_CHARTER: &str = "You are the Initiative thread. \
    See charter-initiative prompt for full instructions.";

pub fn spec() -> ThreadSpec {
    ThreadSpec {
        thread_id: INITIATIVE_ID,
        role: ThreadRole::Initiative,
        flavor: ThreadFlavor::Cognitive,
        name: "Initiative".into(),
        charter: INITIATIVE_CHARTER.into(),
        priority: ThreadPriority::High,
        token_budget: 4096,
        schedule: ThreadSchedule::EveryNTicks(3),
        workspace_root: None,
    }
}
