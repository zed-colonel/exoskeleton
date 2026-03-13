//! Thread scheduling logic.
//!
//! Determines which threads should execute on a given tick based on their
//! schedule configuration and execution history.

use exoskeleton_core::{ThreadSchedule, ThreadStatus};

/// Determine whether a thread is due to execute on the given tick.
///
/// Only `Active` threads can be due. The decision depends on the thread's
/// [`ThreadSchedule`]:
///
/// - **`EveryTick`** -- always due.
/// - **`EveryNTicks(n)`** -- due when `current_tick - last_run_tick >= n`,
///   or when the thread has never run (`last_run_tick` is `None`).
/// - **`OnDemand`** -- never due via scheduling; must be triggered explicitly.
pub fn is_thread_due(
    schedule: ThreadSchedule,
    status: ThreadStatus,
    current_tick: u64,
    last_run_tick: Option<u64>,
) -> bool {
    // Only Active threads can be due.
    if status != ThreadStatus::Active {
        return false;
    }
    match schedule {
        ThreadSchedule::EveryTick => true,
        ThreadSchedule::EveryNTicks(n) => match last_run_tick {
            None => true, // Never run -> due immediately.
            Some(last) => current_tick.saturating_sub(last) >= n as u64,
        },
        ThreadSchedule::OnDemand => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tick_always_due() {
        assert!(is_thread_due(
            ThreadSchedule::EveryTick,
            ThreadStatus::Active,
            0,
            None,
        ));
        assert!(is_thread_due(
            ThreadSchedule::EveryTick,
            ThreadStatus::Active,
            100,
            Some(99),
        ));
        assert!(is_thread_due(
            ThreadSchedule::EveryTick,
            ThreadStatus::Active,
            42,
            Some(42),
        ));
    }

    #[test]
    fn every_n_ticks_due_when_interval_elapsed() {
        // last_run=2, current=5, n=3 -> elapsed=3 >= 3 -> due
        assert!(is_thread_due(
            ThreadSchedule::EveryNTicks(3),
            ThreadStatus::Active,
            5,
            Some(2),
        ));
        // Also due when elapsed exceeds n.
        assert!(is_thread_due(
            ThreadSchedule::EveryNTicks(3),
            ThreadStatus::Active,
            10,
            Some(2),
        ));
    }

    #[test]
    fn every_n_ticks_not_due_when_interval_not_elapsed() {
        // last_run=4, current=5, n=3 -> elapsed=1 < 3 -> not due
        assert!(!is_thread_due(
            ThreadSchedule::EveryNTicks(3),
            ThreadStatus::Active,
            5,
            Some(4),
        ));
        // Exactly n-1 elapsed.
        assert!(!is_thread_due(
            ThreadSchedule::EveryNTicks(3),
            ThreadStatus::Active,
            6,
            Some(4),
        ));
    }

    #[test]
    fn every_n_ticks_due_when_never_run() {
        assert!(is_thread_due(
            ThreadSchedule::EveryNTicks(5),
            ThreadStatus::Active,
            0,
            None,
        ));
        assert!(is_thread_due(
            ThreadSchedule::EveryNTicks(100),
            ThreadStatus::Active,
            1,
            None,
        ));
    }

    #[test]
    fn non_active_never_due() {
        let non_active = [
            ThreadStatus::Suspended,
            ThreadStatus::Completed,
            ThreadStatus::Failed,
        ];
        let schedules = [
            ThreadSchedule::EveryTick,
            ThreadSchedule::EveryNTicks(1),
            ThreadSchedule::OnDemand,
        ];
        for status in &non_active {
            for schedule in &schedules {
                assert!(
                    !is_thread_due(*schedule, *status, 100, None),
                    "{:?} with {:?} should not be due",
                    status,
                    schedule,
                );
                assert!(
                    !is_thread_due(*schedule, *status, 100, Some(0)),
                    "{:?} with {:?} (last_run=0) should not be due",
                    status,
                    schedule,
                );
            }
        }
    }
}
