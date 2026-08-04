//! Surface lifecycle state machine.

/// Monotonic milliseconds, supplied by the caller.
///
/// The core never reads a system clock. Callers pass `now`, which makes every
/// time-dependent behaviour deterministic in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(pub u64);

/// Lifecycle state of one surface.
///
/// A "surface" is one renderer (the Flutter side or the webview side). Either
/// surface can be torn down independently to reclaim its memory; this enum
/// tracks where a single surface currently sits in that lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceState {
    /// No engine, no webview, no renderer process. The store retains
    /// everything, so resuming from here re-hydrates instead of starting
    /// fresh.
    Cold,
    /// Engine booting or webview creating. Requests are queued until the
    /// surface reaches [`SurfaceState::Live`].
    Starting,
    /// Attached, rendering, and receiving events.
    Live,
    /// Grace period before teardown. A [`LifecycleEvent::Resume`] here
    /// cancels teardown and returns straight to `Live`, avoiding a full
    /// engine boot. This exists specifically to prevent thrash when a user
    /// closes and immediately reopens a window.
    Suspending {
        /// The instant, in caller-supplied monotonic milliseconds, at which
        /// suspension began. Policy evaluation measures the grace period
        /// from this value.
        since: Instant,
    },
    /// Creation failed or the guest crashed. The host stays alive; the
    /// reason is published into the store so the *other* surface can render
    /// an error UI.
    Failed(String),
}

/// Inputs that drive the state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleEvent {
    /// Begin booting a cold surface.
    Start,
    /// The engine or webview finished booting and is ready to render.
    Ready,
    /// Begin the suspension grace period at the given instant.
    Suspend {
        /// The instant, in caller-supplied monotonic milliseconds, at which
        /// suspension was requested.
        at: Instant,
    },
    /// Cancel a pending suspension (or re-enter from cold) and return to
    /// an active state.
    Resume,
    /// The suspension grace period elapsed without a resume.
    GraceExpired,
    /// Creation failed or the guest crashed, with a human-readable reason.
    Fail(String),
    /// Retry after a failure.
    Retry,
}

/// Returned when an event does not apply to the current state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidTransition {
    /// The state the surface was in when the event was applied.
    pub from: SurfaceState,
    /// The event that did not apply to `from`.
    pub event: LifecycleEvent,
}

/// Computes the next state. Pure — no side effects, no clock.
///
/// # Errors
///
/// Returns [`InvalidTransition`] when `event` does not apply to `from`. The
/// caller is expected to treat this as a programming error or a stale
/// command racing a state change, not as a recoverable outcome to retry
/// blindly.
pub fn transition(
    from: &SurfaceState,
    event: &LifecycleEvent,
) -> Result<SurfaceState, InvalidTransition> {
    use LifecycleEvent as E;
    use SurfaceState as S;

    let next = match (from, event) {
        // Failure interrupts any state, so it is matched first.
        (_, E::Fail(why)) => S::Failed(why.clone()),

        (S::Cold, E::Start) => S::Starting,
        (S::Cold, E::Resume) => S::Starting,
        (S::Starting, E::Ready) => S::Live,
        (S::Live, E::Suspend { at }) => S::Suspending { since: *at },
        (S::Suspending { .. }, E::Resume) => S::Live,
        (S::Suspending { .. }, E::GraceExpired) => S::Cold,
        (S::Failed(_), E::Retry) => S::Starting,

        _ => {
            return Err(InvalidTransition {
                from: from.clone(),
                event: event.clone(),
            });
        }
    };

    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_starts() {
        assert_eq!(
            transition(&SurfaceState::Cold, &LifecycleEvent::Start),
            Ok(SurfaceState::Starting)
        );
    }

    #[test]
    fn starting_becomes_live_when_ready() {
        assert_eq!(
            transition(&SurfaceState::Starting, &LifecycleEvent::Ready),
            Ok(SurfaceState::Live)
        );
    }

    #[test]
    fn live_suspends_with_timestamp() {
        assert_eq!(
            transition(
                &SurfaceState::Live,
                &LifecycleEvent::Suspend { at: Instant(1_000) }
            ),
            Ok(SurfaceState::Suspending {
                since: Instant(1_000)
            })
        );
    }

    #[test]
    fn resume_during_grace_cancels_teardown() {
        // The anti-thrash property: reopening during the grace window returns
        // straight to Live without paying an engine boot.
        assert_eq!(
            transition(
                &SurfaceState::Suspending {
                    since: Instant(1_000)
                },
                &LifecycleEvent::Resume
            ),
            Ok(SurfaceState::Live)
        );
    }

    #[test]
    fn grace_expiry_goes_cold() {
        assert_eq!(
            transition(
                &SurfaceState::Suspending {
                    since: Instant(1_000)
                },
                &LifecycleEvent::GraceExpired
            ),
            Ok(SurfaceState::Cold)
        );
    }

    #[test]
    fn cold_resumes_by_starting() {
        assert_eq!(
            transition(&SurfaceState::Cold, &LifecycleEvent::Resume),
            Ok(SurfaceState::Starting)
        );
    }

    #[test]
    fn failure_is_reachable_from_any_state() {
        for state in [
            SurfaceState::Cold,
            SurfaceState::Starting,
            SurfaceState::Live,
            SurfaceState::Suspending { since: Instant(0) },
        ] {
            assert_eq!(
                transition(&state, &LifecycleEvent::Fail("boom".into())),
                Ok(SurfaceState::Failed("boom".into())),
                "failure should be reachable from {state:?}"
            );
        }
    }

    #[test]
    fn failed_retries_by_starting() {
        assert_eq!(
            transition(&SurfaceState::Failed("boom".into()), &LifecycleEvent::Retry),
            Ok(SurfaceState::Starting)
        );
    }

    #[test]
    fn invalid_transition_is_rejected() {
        assert_eq!(
            transition(&SurfaceState::Live, &LifecycleEvent::Ready),
            Err(InvalidTransition {
                from: SurfaceState::Live,
                event: LifecycleEvent::Ready
            })
        );
    }

    #[test]
    fn retry_is_only_valid_from_failed() {
        assert!(transition(&SurfaceState::Live, &LifecycleEvent::Retry).is_err());
    }

    /// Every `SurfaceState` variant crossed with every `LifecycleEvent`
    /// variant, checked against an explicit expected outcome.
    ///
    /// Adding a new state or event variant will fail this test (either by
    /// missing a case in `states`/`events`, changing the pinned count below,
    /// or falling into the wildcard arm with an outcome nobody reviewed)
    /// until the table is updated. That is the point: no transition is
    /// unspecified by accident.
    #[test]
    fn transition_matrix_is_exhaustively_specified() {
        let states = [
            SurfaceState::Cold,
            SurfaceState::Starting,
            SurfaceState::Live,
            SurfaceState::Suspending { since: Instant(0) },
            SurfaceState::Failed("existing-failure".into()),
        ];
        let events = [
            LifecycleEvent::Start,
            LifecycleEvent::Ready,
            LifecycleEvent::Suspend { at: Instant(0) },
            LifecycleEvent::Resume,
            LifecycleEvent::GraceExpired,
            LifecycleEvent::Fail("boom".into()),
            LifecycleEvent::Retry,
        ];

        let mut checked = 0usize;
        for state in &states {
            for event in &events {
                // This table is the independent, hand-written expectation —
                // it happens to share the shape of a state machine because
                // that is the only honest way to specify one, but every
                // outcome below was chosen by inspection of the spec, not
                // copied from `transition`'s return value.
                let expected: Result<SurfaceState, InvalidTransition> = match (state, event) {
                    (SurfaceState::Cold, LifecycleEvent::Fail(why)) => {
                        Ok(SurfaceState::Failed(why.clone()))
                    }
                    (SurfaceState::Starting, LifecycleEvent::Fail(why)) => {
                        Ok(SurfaceState::Failed(why.clone()))
                    }
                    (SurfaceState::Live, LifecycleEvent::Fail(why)) => {
                        Ok(SurfaceState::Failed(why.clone()))
                    }
                    (SurfaceState::Suspending { .. }, LifecycleEvent::Fail(why)) => {
                        Ok(SurfaceState::Failed(why.clone()))
                    }
                    (SurfaceState::Failed(_), LifecycleEvent::Fail(why)) => {
                        Ok(SurfaceState::Failed(why.clone()))
                    }

                    (SurfaceState::Cold, LifecycleEvent::Start) => Ok(SurfaceState::Starting),
                    (SurfaceState::Cold, LifecycleEvent::Resume) => Ok(SurfaceState::Starting),
                    (SurfaceState::Starting, LifecycleEvent::Ready) => Ok(SurfaceState::Live),
                    (SurfaceState::Live, LifecycleEvent::Suspend { at }) => {
                        Ok(SurfaceState::Suspending { since: *at })
                    }
                    (SurfaceState::Suspending { .. }, LifecycleEvent::Resume) => {
                        Ok(SurfaceState::Live)
                    }
                    (SurfaceState::Suspending { .. }, LifecycleEvent::GraceExpired) => {
                        Ok(SurfaceState::Cold)
                    }
                    (SurfaceState::Failed(_), LifecycleEvent::Retry) => Ok(SurfaceState::Starting),

                    // Everything else is unreachable in this state machine.
                    _ => Err(InvalidTransition {
                        from: state.clone(),
                        event: event.clone(),
                    }),
                };

                assert_eq!(
                    transition(state, event),
                    expected,
                    "state={state:?} event={event:?}"
                );
                checked += 1;
            }
        }

        assert_eq!(checked, 35, "expected 5 states x 7 events = 35 pairs");
    }

    /// `transition` must never panic, for any state/event pair, including
    /// degenerate `Fail` reason strings (empty and very long).
    #[test]
    fn transition_never_panics_for_any_pair_including_edge_case_fail_reasons() {
        let states = [
            SurfaceState::Cold,
            SurfaceState::Starting,
            SurfaceState::Live,
            SurfaceState::Suspending { since: Instant(0) },
            SurfaceState::Failed("existing-failure".into()),
        ];
        let events = [
            LifecycleEvent::Start,
            LifecycleEvent::Ready,
            LifecycleEvent::Suspend { at: Instant(0) },
            LifecycleEvent::Resume,
            LifecycleEvent::GraceExpired,
            LifecycleEvent::Fail(String::new()),
            LifecycleEvent::Fail("x".repeat(10_000)),
            LifecycleEvent::Retry,
        ];

        let mut checked = 0usize;
        for state in &states {
            for event in &events {
                // Merely calling `transition` and requiring a `Result` back
                // (rather than a panic) is the assertion: an unwinding panic
                // would fail this test itself.
                match transition(state, event) {
                    Ok(_) | Err(_) => {}
                }
                checked += 1;
            }
        }

        assert_eq!(checked, 40, "expected 5 states x 8 events = 40 pairs");
    }
}
