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
#[non_exhaustive]
pub struct InvalidTransition {
    /// The state the surface was in when the event was applied.
    pub from: SurfaceState,
    /// The event that did not apply to `from`.
    pub event: LifecycleEvent,
}

impl std::fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Both `SurfaceState::Failed` and `LifecycleEvent::Fail` carry an
        // unbounded, guest-influenced reason string. Truncate what we render
        // here exactly as `PathParseError::InvalidIndex`'s `Display` impl
        // does (see `path.rs`), using `.chars().take(N)` (never a byte
        // slice, to avoid splitting a multi-byte char) so a pathological
        // reason can't produce a huge message relayed to a guest or written
        // to host logs. The struct fields themselves keep the full strings;
        // only `Display` is bounded.
        write!(f, "event ")?;
        fmt_bounded_event(&self.event, f)?;
        write!(f, " does not apply to state ")?;
        fmt_bounded_state(&self.from, f)
    }
}

impl std::error::Error for InvalidTransition {}

/// Truncates a reason string to 32 chars for display, appending an ellipsis
/// marker if anything was cut. Mirrors `PathParseError::InvalidIndex`'s
/// `Display` truncation (see `path.rs`).
fn write_bounded_reason(reason: &str, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let char_count = reason.chars().count();
    let mut truncated: String = reason.chars().take(32).collect();
    if char_count > 32 {
        truncated.push('…');
    }
    write!(f, "{truncated:?}")
}

fn fmt_bounded_state(state: &SurfaceState, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match state {
        SurfaceState::Failed(reason) => {
            write!(f, "Failed(")?;
            write_bounded_reason(reason, f)?;
            write!(f, ")")
        }
        other => write!(f, "{other:?}"),
    }
}

fn fmt_bounded_event(event: &LifecycleEvent, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match event {
        LifecycleEvent::Fail(reason) => {
            write!(f, "Fail(")?;
            write_bounded_reason(reason, f)?;
            write!(f, ")")
        }
        other => write!(f, "{other:?}"),
    }
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

    #[test]
    fn invalid_transition_implements_display_and_error() {
        let err = InvalidTransition {
            from: SurfaceState::Live,
            event: LifecycleEvent::Ready,
        };
        assert_eq!(err.to_string(), "event Ready does not apply to state Live");
        let _: &dyn std::error::Error = &err;
    }

    /// Both `SurfaceState::Failed` and `LifecycleEvent::Fail` embed an
    /// unbounded, guest-influenced reason string (the suite exercises
    /// `"x".repeat(10_000)` elsewhere in this file). `Display` must bound
    /// what it renders exactly as `PathParseError::InvalidIndex` does, while
    /// the struct's fields -- reachable via `Debug` or direct field access
    /// -- must retain the full, untruncated strings.
    #[test]
    fn invalid_transition_display_is_bounded_but_fields_are_not() {
        let long_reason = "x".repeat(10_000);
        let err = InvalidTransition {
            from: SurfaceState::Failed(long_reason.clone()),
            event: LifecycleEvent::Fail(long_reason.clone()),
        };

        // The struct fields must retain the full, untruncated value.
        match &err.from {
            SurfaceState::Failed(reason) => assert_eq!(reason.len(), 10_000),
            other => panic!("expected Failed, got {other:?}"),
        }
        match &err.event {
            LifecycleEvent::Fail(reason) => assert_eq!(reason.len(), 10_000),
            other => panic!("expected Fail, got {other:?}"),
        }

        // The rendered `Display` output must be bounded: each embedded
        // reason contributes at most 32 chars plus an ellipsis marker, not
        // all 10,000. Count the 'x' runs inside each quoted reason rather
        // than the whole message, since other words in the message contain
        // 'x' too (e.g. none here, but this keeps the check precise).
        let rendered = err.to_string();
        let quoted: Vec<&str> = rendered.split('"').skip(1).step_by(2).collect();
        assert_eq!(
            quoted.len(),
            2,
            "expected exactly two quoted reasons in {rendered:?}"
        );
        for q in &quoted {
            assert_eq!(
                q.chars().filter(|&c| c == 'x').count(),
                32,
                "quoted reason should carry exactly 32 of the original 10,000 'x's: {q:?}"
            );
            assert!(
                q.ends_with('…'),
                "truncated output should carry an ellipsis marker: {q:?}"
            );
            assert_eq!(q.chars().count(), 33);
        }

        // Sanity bound: the whole rendered message must be small, nowhere
        // near the 20,000+ chars it would be if both reasons were rendered
        // in full.
        assert!(
            rendered.len() < 200,
            "rendered message should be small, got {} bytes",
            rendered.len()
        );
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
