//! Bounded, decorrelated-jitter retries over the SCRUM-84 typed error
//! taxonomy (SCRUM-85; ADR-024).
//!
//! Retry orchestration sits above the raw HTTP parser: an operation returns
//! the classified [`ProviderError`], and eligibility is decided by the
//! category's typed recovery policy — never by message matching. A retry
//! always targets the same resolved `{profile_id, model}` with a
//! semantically identical request; there is no hidden fallback.
//!
//! Policy defaults: at most 3 total attempts; backoff uses decorrelated
//! jitter `min(cap, random(base, previous * 3))` with `base = 250 ms` and an
//! 8 s per-sleep cap. A valid `Retry-After` from the provider raises the
//! floor of a sleep (never earlier than instructed), and a `Retry-After`
//! beyond the remaining deadline stops the loop instead of sleeping past it.
//!
//! Clock, sleep, and jitter are injected so tests are deterministic and
//! fast. The production sleeper is abortable by dropping the future, so a
//! cancelled request interrupts pending backoff immediately.
//!
//! Streaming safety: retries cover request establishment and pre-first-delta
//! failures. Once any user-visible model/tool delta has been emitted, the
//! caller must report the failure through [`AttemptFailure::terminal`] so
//! this layer never replays output.

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use cortex_application::ApplicationError;

use crate::error::{ProviderError, ProviderFailureCategory, map_provider_error};

/// Total provider attempts per request: the initial attempt plus at most two
/// retries.
pub const RETRY_MAX_ATTEMPTS: u32 = 3;
/// Decorrelated-jitter base, in milliseconds.
pub const RETRY_BASE_MS: u64 = 250;
/// Per-sleep cap, in milliseconds.
pub const RETRY_CAP_MS: u64 = 8_000;

/// Bounded retry policy. Defaults follow SCRUM-85 and remain bounded; a
/// profile may override the values through typed configuration without
/// raising them past their validated caps.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_ms: u64,
    pub cap_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: RETRY_MAX_ATTEMPTS,
            base_ms: RETRY_BASE_MS,
            cap_ms: RETRY_CAP_MS,
        }
    }
}

impl RetryPolicy {
    /// The documented default policy (SCRUM-85).
    pub const DEFAULT: Self = Self::new(RETRY_MAX_ATTEMPTS, RETRY_BASE_MS, RETRY_CAP_MS);

    /// Builds a validated policy. Zero attempts or a base above the cap fall
    /// back to the documented defaults for the offending bound.
    #[must_use]
    pub const fn new(max_attempts: u32, base_ms: u64, cap_ms: u64) -> Self {
        let max_attempts = if max_attempts == 0 {
            RETRY_MAX_ATTEMPTS
        } else {
            max_attempts
        };
        let base_ms = if base_ms == 0 { RETRY_BASE_MS } else { base_ms };
        let cap_ms = if cap_ms < base_ms { base_ms } else { cap_ms };
        Self {
            max_attempts,
            base_ms,
            cap_ms,
        }
    }
}

/// Wall-clock source for deadline checks.
pub trait RetryClock: Send + Sync {
    fn now(&self) -> Instant;
}

/// System clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl RetryClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Backoff sleeper. Production implementations abort when the surrounding
/// future is dropped, so request cancellation interrupts pending backoff.
pub trait RetrySleep: Send + Sync {
    fn sleep(&self, delay: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

/// Tokio-backed sleeper.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioSleep;

impl RetrySleep for TokioSleep {
    fn sleep(&self, delay: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep(delay))
    }
}

/// Jitter source: a uniform integer in `low..=high`, milliseconds.
pub trait RetryJitter: Send + Sync {
    fn uniform_ms(&self, low_inclusive: u64, high_inclusive: u64) -> u64;
}

/// System-entropy jitter.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemJitter;

impl SystemJitter {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl RetryJitter for SystemJitter {
    fn uniform_ms(&self, low_inclusive: u64, high_inclusive: u64) -> u64 {
        use std::time::SystemTime;
        let span = high_inclusive.saturating_sub(low_inclusive) + 1;
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |elapsed| u64::from(elapsed.subsec_nanos()));
        low_inclusive + (nanos ^ (nanos >> 17)) % span
    }
}

/// One recorded retryable failure: structured attempt metadata for
/// diagnostics. Categories and delays only — never credentials, headers, or
/// response bodies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryAttempt {
    pub attempt: u32,
    pub delay_ms: u64,
    pub category: ProviderFailureCategory,
}

/// Diagnostics for a completed retry loop.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RetryReport {
    pub attempts: Vec<RetryAttempt>,
}

impl RetryReport {
    /// Number of attempts that returned a retryable failure.
    #[must_use]
    pub fn retryable_failures(&self) -> usize {
        self.attempts.len()
    }
}

/// Decorrelated jitter: `min(cap, random(base, previous * 3))`, with the
/// first sleep drawn from `base..=3 * base`.
#[must_use]
pub fn decorrelated_jitter_ms(
    jitter: &dyn RetryJitter,
    base_ms: u64,
    previous_sleep_ms: u64,
    cap_ms: u64,
) -> u64 {
    let previous = previous_sleep_ms.max(base_ms);
    let high = previous.saturating_mul(3).min(cap_ms).max(base_ms);
    jitter.uniform_ms(base_ms, high)
}

/// A failure the retry loop evaluates. `retryable` lets streaming callers
/// mark post-first-delta failures terminal while reusing the same typed
/// category for diagnostics.
#[derive(Clone, Debug)]
pub struct AttemptFailure {
    pub error: ProviderError,
    pub retryable: bool,
}

impl AttemptFailure {
    /// A failure whose retryability is exactly the category's typed recovery
    /// policy.
    #[must_use]
    pub fn classified(error: ProviderError) -> Self {
        let retryable = error.category().retryable();
        Self { error, retryable }
    }

    /// A terminal failure: never retried (e.g. a stream failed after the
    /// first emitted delta).
    #[must_use]
    pub fn terminal(error: ProviderError) -> Self {
        Self {
            error,
            retryable: false,
        }
    }
}

/// Runs `operation` under the policy. Each invocation receives the 1-based
/// attempt number; every attempt targets the same resolved selection and a
/// semantically identical request.
///
/// # Errors
/// Returns the typed application error of the last attempt: retryable
/// categories after `max_attempts`, and any terminal failure immediately.
pub async fn run_with_retries<T, F, Fut>(
    policy: RetryPolicy,
    clock: &dyn RetryClock,
    sleeper: &dyn RetrySleep,
    jitter: &dyn RetryJitter,
    deadline: Option<Instant>,
    mut operation: F,
) -> Result<(T, RetryReport), ApplicationError>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<T, AttemptFailure>>,
{
    let mut report = RetryReport::default();
    let mut previous_sleep_ms = policy.base_ms;
    for attempt in 1..=policy.max_attempts {
        match operation(attempt).await {
            Ok(value) => return Ok((value, report)),
            Err(failure) => {
                let AttemptFailure { error, retryable } = failure;
                let category = error.category();
                let delay = decorrelated_jitter_ms(
                    jitter,
                    policy.base_ms,
                    previous_sleep_ms,
                    policy.cap_ms,
                );
                let floor_ms =
                    u64::try_from(error.retry_after().map_or(0, |floor| floor.as_millis()))
                        .unwrap_or(u64::MAX);
                let delay = delay.max(floor_ms);
                report.attempts.push(RetryAttempt {
                    attempt,
                    delay_ms: delay,
                    category,
                });
                let last_attempt = attempt >= policy.max_attempts;
                if last_attempt || !retryable {
                    return Err(map_provider_error(&error));
                }
                if let Some(deadline) = deadline {
                    let now = clock.now();
                    let deadline_passed = now >= deadline;
                    let sleep_overruns = now
                        .checked_add(Duration::from_millis(delay))
                        .is_none_or(|wake| wake >= deadline);
                    if deadline_passed || sleep_overruns {
                        return Err(map_provider_error(&error));
                    }
                }
                sleeper.sleep(Duration::from_millis(delay)).await;
                previous_sleep_ms = delay;
            }
        }
    }
    Err(ApplicationError::Internal)
}

/// Convenience wrapper using the production clock, tokio sleeper, and system
/// jitter, with no overall deadline beyond the per-attempt transport
/// timeouts.
///
/// # Errors
/// Returns the typed application error of the last attempt under
/// [`RetryPolicy::DEFAULT`].
pub async fn run_with_default_policy<T, F, Fut>(
    operation: F,
) -> Result<(T, RetryReport), ApplicationError>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<T, AttemptFailure>>,
{
    let clock = SystemClock;
    let sleeper = TokioSleep;
    let jitter = SystemJitter::new();
    run_with_retries(
        RetryPolicy::default(),
        &clock,
        &sleeper,
        &jitter,
        None,
        operation,
    )
    .await
}
