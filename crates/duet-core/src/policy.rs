//! Teardown policy evaluation.

use crate::lifecycle::{Instant, SurfaceState};

/// When a surface's resources should be released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Policy {
    /// Suspend once every window for this surface is closed.
    OnLastWindowClosed {
        /// How long, in caller-supplied monotonic milliseconds, to wait in
        /// `Suspending` before tearing down.
        grace_ms: u64,
    },
    /// Suspend once every window for this surface is hidden, even if still
    /// open (e.g. minimized or occluded).
    OnHidden {
        /// How long, in caller-supplied monotonic milliseconds, to wait in
        /// `Suspending` before tearing down.
        grace_ms: u64,
    },
    /// Suspend after this long without interaction, regardless of window
    /// state.
    IdleTimeout {
        /// How long, in caller-supplied monotonic milliseconds, a surface
        /// may sit without interaction before it is suspended.
        after_ms: u64,
    },
    /// Never suspend automatically.
    Never,
}

impl Default for Policy {
    /// The default policy: suspend five seconds after the last window
    /// closes. This is the anti-thrash default — long enough to survive a
    /// quick close-and-reopen, short enough to reclaim memory promptly.
    fn default() -> Self {
        Policy::OnLastWindowClosed { grace_ms: 5_000 }
    }
}

/// Everything policy evaluation is allowed to look at.
///
/// `now` is passed in rather than read, so evaluation stays pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyInput {
    /// The surface's current lifecycle state.
    pub state: SurfaceState,
    /// Number of windows currently open for this surface.
    pub open_windows: usize,
    /// Number of open windows for this surface that are currently visible
    /// (not minimized or fully occluded). Always `<= open_windows`.
    pub visible_windows: usize,
    /// The instant of the last input event, command, or store write from
    /// this surface, in caller-supplied monotonic milliseconds.
    pub last_interaction: Instant,
    /// The current instant, in caller-supplied monotonic milliseconds, as
    /// observed by the caller. The core never reads a system clock.
    pub now: Instant,
}

/// What the caller should do with the surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// No lifecycle transition is warranted right now.
    NoChange,
    /// Move `Live` -> `Suspending`.
    Suspend,
    /// Move `Suspending` -> `Cold`.
    Teardown,
}

/// Decides what should happen to a surface. Pure — no side effects, no
/// clock.
///
/// Only [`SurfaceState::Live`] can begin suspending, and only
/// [`SurfaceState::Suspending`] can be torn down; every other state (`Cold`,
/// `Starting`, `Failed`) is left alone regardless of policy, since there is
/// nothing to release yet or the surface is already mid-transition.
pub fn evaluate(policy: &Policy, input: &PolicyInput) -> Decision {
    // While suspending, the only question is whether the grace period
    // elapsed. Window counts and idle time no longer matter: the decision
    // to suspend already happened.
    if let SurfaceState::Suspending { since } = input.state {
        let grace_ms = match policy {
            Policy::Never => return Decision::NoChange,
            Policy::OnLastWindowClosed { grace_ms } | Policy::OnHidden { grace_ms } => *grace_ms,
            // Idle timeout has no separate grace: the idle interval already
            // served as the grace period before `Suspending` was entered.
            Policy::IdleTimeout { .. } => 0,
        };
        return if input.now.0.saturating_sub(since.0) >= grace_ms {
            Decision::Teardown
        } else {
            Decision::NoChange
        };
    }

    // Only a live surface can begin suspending.
    if input.state != SurfaceState::Live {
        return Decision::NoChange;
    }

    match policy {
        Policy::Never => Decision::NoChange,
        Policy::OnLastWindowClosed { .. } => {
            if input.open_windows == 0 {
                Decision::Suspend
            } else {
                Decision::NoChange
            }
        }
        Policy::OnHidden { .. } => {
            if input.visible_windows == 0 {
                Decision::Suspend
            } else {
                Decision::NoChange
            }
        }
        Policy::IdleTimeout { after_ms } => {
            if input.now.0.saturating_sub(input.last_interaction.0) >= *after_ms {
                Decision::Suspend
            } else {
                Decision::NoChange
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::{Instant, SurfaceState};

    fn live(open: usize, visible: usize) -> PolicyInput {
        PolicyInput {
            state: SurfaceState::Live,
            open_windows: open,
            visible_windows: visible,
            last_interaction: Instant(0),
            now: Instant(0),
        }
    }

    #[test]
    fn default_policy_is_five_second_grace_on_last_window_closed() {
        assert_eq!(
            Policy::default(),
            Policy::OnLastWindowClosed { grace_ms: 5_000 }
        );
    }

    #[test]
    fn never_policy_never_suspends() {
        assert_eq!(evaluate(&Policy::Never, &live(0, 0)), Decision::NoChange);
    }

    #[test]
    fn last_window_closed_suspends_at_zero_open_windows() {
        let policy = Policy::OnLastWindowClosed { grace_ms: 5_000 };
        assert_eq!(evaluate(&policy, &live(1, 0)), Decision::NoChange);
        assert_eq!(evaluate(&policy, &live(0, 0)), Decision::Suspend);
    }

    #[test]
    fn on_hidden_ignores_open_count_and_watches_visibility() {
        let policy = Policy::OnHidden { grace_ms: 1_000 };
        // Window is open but hidden: suspend.
        assert_eq!(evaluate(&policy, &live(1, 0)), Decision::Suspend);
        assert_eq!(evaluate(&policy, &live(1, 1)), Decision::NoChange);
    }

    #[test]
    fn idle_timeout_suspends_only_after_the_interval() {
        let policy = Policy::IdleTimeout { after_ms: 1_000 };
        let mut input = live(1, 1);
        input.last_interaction = Instant(0);
        input.now = Instant(999);
        assert_eq!(evaluate(&policy, &input), Decision::NoChange);
        input.now = Instant(1_000);
        assert_eq!(evaluate(&policy, &input), Decision::Suspend);
    }

    #[test]
    fn suspending_tears_down_only_after_grace_elapses() {
        let policy = Policy::OnLastWindowClosed { grace_ms: 5_000 };
        let mut input = live(0, 0);
        input.state = SurfaceState::Suspending {
            since: Instant(1_000),
        };
        input.now = Instant(5_999);
        assert_eq!(evaluate(&policy, &input), Decision::NoChange);
        input.now = Instant(6_000);
        assert_eq!(evaluate(&policy, &input), Decision::Teardown);
    }

    #[test]
    fn never_policy_does_not_tear_down_even_while_suspending() {
        let mut input = live(0, 0);
        input.state = SurfaceState::Suspending { since: Instant(0) };
        input.now = Instant(u64::MAX);
        assert_eq!(evaluate(&Policy::Never, &input), Decision::NoChange);
    }

    #[test]
    fn idle_timeout_tears_down_immediately_once_suspending() {
        let mut input = live(1, 1);
        input.state = SurfaceState::Suspending { since: Instant(10) };
        input.now = Instant(10);
        assert_eq!(
            evaluate(&Policy::IdleTimeout { after_ms: 1_000 }, &input),
            Decision::Teardown
        );
    }

    #[test]
    fn non_live_non_suspending_states_are_left_alone() {
        for state in [
            SurfaceState::Cold,
            SurfaceState::Starting,
            SurfaceState::Failed("boom".into()),
        ] {
            let mut input = live(0, 0);
            input.state = state.clone();
            assert_eq!(
                evaluate(&Policy::OnLastWindowClosed { grace_ms: 0 }, &input),
                Decision::NoChange,
                "state {state:?} should be left alone"
            );
        }
    }

    /// Independent re-derivation of the spec, used only inside the
    /// exhaustive matrix test below. Written from the doc comments on
    /// `Policy`, `PolicyInput`, and `evaluate`, not by inspecting
    /// `evaluate`'s control flow.
    fn expected_decision(policy: &Policy, input: &PolicyInput) -> Decision {
        match &input.state {
            // Nothing to release yet, or already mid-transition: policy
            // never touches these states.
            SurfaceState::Cold | SurfaceState::Starting | SurfaceState::Failed(_) => {
                Decision::NoChange
            }
            SurfaceState::Suspending { since } => match policy {
                Policy::Never => Decision::NoChange,
                Policy::OnLastWindowClosed { grace_ms } | Policy::OnHidden { grace_ms } => {
                    if input.now.0.saturating_sub(since.0) >= *grace_ms {
                        Decision::Teardown
                    } else {
                        Decision::NoChange
                    }
                }
                // The idle interval already served as the grace period, so
                // there is no additional wait once `Suspending` is entered.
                Policy::IdleTimeout { .. } => Decision::Teardown,
            },
            SurfaceState::Live => match policy {
                Policy::Never => Decision::NoChange,
                Policy::OnLastWindowClosed { .. } => {
                    if input.open_windows == 0 {
                        Decision::Suspend
                    } else {
                        Decision::NoChange
                    }
                }
                Policy::OnHidden { .. } => {
                    if input.visible_windows == 0 {
                        Decision::Suspend
                    } else {
                        Decision::NoChange
                    }
                }
                Policy::IdleTimeout { after_ms } => {
                    if input.now.0.saturating_sub(input.last_interaction.0) >= *after_ms {
                        Decision::Suspend
                    } else {
                        Decision::NoChange
                    }
                }
            },
        }
    }

    /// Every `Policy` variant (with representative parameters, including
    /// `grace_ms: 0` / `after_ms: 0`) crossed with every `SurfaceState`
    /// variant and a small set of window-count/time combinations, checked
    /// against [`expected_decision`].
    ///
    /// The combinations were chosen to include: both window counts zero,
    /// `open_windows` and `visible_windows` mismatched (`open > visible >
    /// 0`), an idle interval of zero, an idle interval that has clearly
    /// elapsed, and `now` earlier than `last_interaction` (the
    /// `saturating_sub` clock-backwards guard).
    ///
    /// Adding a new `Policy` or `SurfaceState` variant will silently be
    /// excluded from this matrix until both the variant list here and
    /// `expected_decision` are updated, at which point the pinned count
    /// below will fail and force the update.
    #[test]
    fn policy_matrix_is_exhaustively_specified() {
        let policies = [
            Policy::OnLastWindowClosed { grace_ms: 0 },
            Policy::OnLastWindowClosed { grace_ms: 1_000 },
            Policy::OnHidden { grace_ms: 0 },
            Policy::OnHidden { grace_ms: 1_000 },
            Policy::IdleTimeout { after_ms: 0 },
            Policy::IdleTimeout { after_ms: 1_000 },
            Policy::Never,
        ];
        let states = [
            SurfaceState::Cold,
            SurfaceState::Starting,
            SurfaceState::Live,
            SurfaceState::Suspending {
                since: Instant(1_000),
            },
            SurfaceState::Failed("boom".into()),
        ];
        // (open_windows, visible_windows, last_interaction, now)
        let combos: [(usize, usize, Instant, Instant); 4] = [
            (0, 0, Instant(0), Instant(0)),
            (1, 0, Instant(0), Instant(2_000)),
            (2, 1, Instant(1_000), Instant(1_000)),
            (1, 1, Instant(2_000), Instant(500)), // clock went backwards
        ];

        let mut checked = 0usize;
        for policy in &policies {
            for state in &states {
                for &(open_windows, visible_windows, last_interaction, now) in &combos {
                    let input = PolicyInput {
                        state: state.clone(),
                        open_windows,
                        visible_windows,
                        last_interaction,
                        now,
                    };
                    let expected = expected_decision(policy, &input);
                    assert_eq!(
                        evaluate(policy, &input),
                        expected,
                        "policy={policy:?} state={state:?} open={open_windows} visible={visible_windows} last_interaction={last_interaction:?} now={now:?}"
                    );
                    checked += 1;
                }
            }
        }

        assert_eq!(
            checked, 140,
            "expected 7 policies x 5 states x 4 combos = 140 cases"
        );
    }

    #[test]
    fn grace_boundary_is_inclusive_at_exact_expiry() {
        let policy = Policy::OnLastWindowClosed { grace_ms: 5_000 };
        let mut input = live(0, 0);
        input.state = SurfaceState::Suspending {
            since: Instant(1_000),
        };

        input.now = Instant(1_000 + 5_000);
        assert_eq!(
            evaluate(&policy, &input),
            Decision::Teardown,
            "now == since + grace_ms must tear down"
        );
    }

    #[test]
    fn grace_boundary_one_ms_early_does_not_tear_down() {
        let policy = Policy::OnLastWindowClosed { grace_ms: 5_000 };
        let mut input = live(0, 0);
        input.state = SurfaceState::Suspending {
            since: Instant(1_000),
        };

        input.now = Instant(1_000 + 5_000 - 1);
        assert_eq!(
            evaluate(&policy, &input),
            Decision::NoChange,
            "now == since + grace_ms - 1 must not tear down yet"
        );
    }

    #[test]
    fn zero_grace_tears_down_immediately_on_entering_suspending() {
        let policy = Policy::OnLastWindowClosed { grace_ms: 0 };
        let mut input = live(0, 0);
        input.state = SurfaceState::Suspending { since: Instant(42) };
        input.now = Instant(42);
        assert_eq!(evaluate(&policy, &input), Decision::Teardown);
    }

    #[test]
    fn clock_going_backwards_during_suspension_does_not_panic_and_does_not_tear_down() {
        // `now < since` should not happen in a correct caller, but the core
        // must not panic on it. `saturating_sub` clamps the elapsed time to
        // zero, which is treated as "no time has passed yet" -- the surface
        // stays suspended rather than tearing down on a clock glitch. This
        // is the safer failure mode: a spurious teardown loses renderer
        // state, whereas staying suspended a little longer costs only
        // memory.
        let policy = Policy::OnLastWindowClosed { grace_ms: 1_000 };
        let mut input = live(0, 0);
        input.state = SurfaceState::Suspending {
            since: Instant(10_000),
        };
        input.now = Instant(1); // earlier than `since`

        assert_eq!(evaluate(&policy, &input), Decision::NoChange);
    }

    #[test]
    fn clock_going_backwards_with_zero_grace_still_tears_down() {
        // With a zero grace period even the clamped-to-zero elapsed time
        // satisfies `>= 0`, so teardown still happens. This documents that
        // the backwards-clock guard prevents *spurious* teardown from
        // underflow, not teardown under a zero-grace policy.
        let policy = Policy::OnLastWindowClosed { grace_ms: 0 };
        let mut input = live(0, 0);
        input.state = SurfaceState::Suspending {
            since: Instant(10_000),
        };
        input.now = Instant(1);

        assert_eq!(evaluate(&policy, &input), Decision::Teardown);
    }

    #[test]
    fn idle_timeout_saturating_sub_guard_does_not_panic_on_backwards_clock() {
        let policy = Policy::IdleTimeout { after_ms: 1_000 };
        let mut input = live(1, 1);
        input.last_interaction = Instant(10_000);
        input.now = Instant(1); // earlier than last_interaction

        assert_eq!(evaluate(&policy, &input), Decision::NoChange);
    }

    #[test]
    fn maximum_instant_and_maximum_grace_does_not_overflow() {
        let policy = Policy::OnLastWindowClosed { grace_ms: u64::MAX };
        let mut input = live(0, 0);
        input.state = SurfaceState::Suspending { since: Instant(0) };
        input.now = Instant(u64::MAX);

        // now - since == u64::MAX == grace_ms, so the boundary is met
        // exactly without ever computing `since + grace_ms` (which would
        // overflow). This is why the implementation compares
        // `now - since >= grace_ms` rather than `now >= since + grace_ms`.
        assert_eq!(evaluate(&policy, &input), Decision::Teardown);
    }

    #[test]
    fn idle_timeout_zero_suspends_immediately_when_live() {
        let policy = Policy::IdleTimeout { after_ms: 0 };
        let mut input = live(1, 1);
        input.last_interaction = Instant(500);
        input.now = Instant(500);

        assert_eq!(evaluate(&policy, &input), Decision::Suspend);
    }

    #[test]
    fn round_trip_suspend_then_teardown_via_grace_expiry() {
        use crate::lifecycle::{LifecycleEvent, transition};

        let policy = Policy::OnLastWindowClosed { grace_ms: 5_000 };

        // Cold -> Start -> Ready -> Live.
        let state = transition(&SurfaceState::Cold, &LifecycleEvent::Start).unwrap();
        assert_eq!(state, SurfaceState::Starting);
        let state = transition(&state, &LifecycleEvent::Ready).unwrap();
        assert_eq!(state, SurfaceState::Live);

        // Last window closes: policy says suspend.
        let input = PolicyInput {
            state: state.clone(),
            open_windows: 0,
            visible_windows: 0,
            last_interaction: Instant(0),
            now: Instant(1_000),
        };
        assert_eq!(evaluate(&policy, &input), Decision::Suspend);

        // Apply the decision: Live -> Suspending { since: 1_000 }.
        let state = transition(&state, &LifecycleEvent::Suspend { at: Instant(1_000) }).unwrap();
        assert_eq!(
            state,
            SurfaceState::Suspending {
                since: Instant(1_000)
            }
        );

        // Before grace elapses: no change.
        let mut input = input;
        input.state = state.clone();
        input.now = Instant(1_000 + 4_999);
        assert_eq!(evaluate(&policy, &input), Decision::NoChange);

        // After grace elapses: tear down.
        input.now = Instant(1_000 + 5_000);
        assert_eq!(evaluate(&policy, &input), Decision::Teardown);

        // Apply the decision: Suspending -> Cold.
        let state = transition(&state, &LifecycleEvent::GraceExpired).unwrap();
        assert_eq!(state, SurfaceState::Cold);
    }

    #[test]
    fn round_trip_resume_during_grace_never_reaches_cold() {
        // The anti-thrash property, exercised end to end: closing and
        // immediately reopening a window must return to `Live` without the
        // surface ever passing through `Cold` (which would force a full
        // engine boot on the next open).
        use crate::lifecycle::{LifecycleEvent, transition};

        let policy = Policy::OnLastWindowClosed { grace_ms: 5_000 };

        let state = SurfaceState::Live;
        let state = transition(&state, &LifecycleEvent::Suspend { at: Instant(1_000) }).unwrap();
        assert_eq!(
            state,
            SurfaceState::Suspending {
                since: Instant(1_000)
            }
        );

        // Policy would still say NoChange this early -- the user reopens
        // before grace expires.
        let input = PolicyInput {
            state: state.clone(),
            open_windows: 1,
            visible_windows: 1,
            last_interaction: Instant(1_000),
            now: Instant(2_000),
        };
        assert_eq!(evaluate(&policy, &input), Decision::NoChange);

        // Resume arrives during the grace window.
        let state = transition(&state, &LifecycleEvent::Resume).unwrap();
        assert_eq!(
            state,
            SurfaceState::Live,
            "must return straight to Live, never Cold"
        );
        assert_ne!(state, SurfaceState::Cold);
    }
}
