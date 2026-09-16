//! Deterministic retry-policy tests (SCRUM-85).
//!
//! Clock, sleep, and jitter are injected, so attempt counts, delays, and
//! Retry-After floors are asserted exactly. Categories come from the SCRUM-84
//! taxonomy; no test string-matches provider messages.

use std::{
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use cortex_application::ApplicationError;
use cortex_inference::{
    AttemptFailure, ProviderError, ProviderFailureCategory, RetryClock, RetryJitter, RetryPolicy,
    RetrySleep, decorrelated_jitter_ms, run_with_retries,
};

fn error_of(category: ProviderFailureCategory, retry_after_ms: Option<u64>) -> AttemptFailure {
    let error = ProviderError::from_category(category);
    let error = match retry_after_ms {
        Some(ms) => error.with_retry_after(Duration::from_millis(ms)),
        None => error,
    };
    AttemptFailure::classified(error)
}

#[derive(Clone)]
struct FixedClock {
    current: Arc<RwLock<Instant>>,
}

impl FixedClock {
    fn start() -> Self {
        Self {
            current: Arc::new(RwLock::new(Instant::now())),
        }
    }
    #[allow(dead_code)] // reserved for in-flight deadline-advance scenarios
    fn advance(&self, by: Duration) {
        *self.current.write().unwrap() += by;
    }
}

impl RetryClock for FixedClock {
    fn now(&self) -> Instant {
        *self.current.read().unwrap()
    }
}

#[derive(Default, Clone)]
struct RecordedSleep {
    delays: Arc<Mutex<Vec<Duration>>>,
}

impl RetrySleep for RecordedSleep {
    fn sleep(
        &self,
        delay: Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        self.delays.lock().unwrap().push(delay);
        Box::pin(std::future::ready(()))
    }
}

#[derive(Clone)]
struct FixedJitter {
    value: u64,
}

impl RetryJitter for FixedJitter {
    fn uniform_ms(&self, low_inclusive: u64, high_inclusive: u64) -> u64 {
        self.value.clamp(low_inclusive, high_inclusive)
    }
}

const BASE: u64 = 250;
const CAP: u64 = 8_000;
const POLICY: RetryPolicy = RetryPolicy::new(3, BASE, CAP);

#[tokio::test]
async fn fails_twice_then_succeeds_on_the_third_attempt() {
    let clock = FixedClock::start();
    let sleep = RecordedSleep::default();
    let jitter = FixedJitter { value: 0 };
    let calls = Arc::new(Mutex::new(0_u32));

    let (value, report) = run_with_retries(POLICY, &clock, &sleep, &jitter, None, |_attempt| {
        let calls = calls.clone();
        *calls.lock().unwrap() += 1;
        let count = *calls.lock().unwrap();
        let result: Result<u32, AttemptFailure> = if count < 3 {
            Err(error_of(ProviderFailureCategory::ServerError, None))
        } else {
            Ok(7)
        };
        std::future::ready(result)
    })
    .await
    .expect("third attempt succeeds");

    assert_eq!(value, 7);
    assert_eq!(*calls.lock().unwrap(), 3);
    assert_eq!(report.retryable_failures(), 2);
}

#[test]
fn decorrelated_jitter_stays_bounded_and_grows_with_the_previous_sleep() {
    let jitter = FixedJitter { value: u64::MAX };
    // First sleep: random(base, base * 3) clamped to the cap.
    assert_eq!(decorrelated_jitter_ms(&jitter, BASE, 0, CAP), 3 * BASE);
    // Previous * 3 above the cap clamps to the cap.
    assert_eq!(decorrelated_jitter_ms(&jitter, BASE, CAP / 3 + 1, CAP), CAP);
    // A lower jitter draw can never fall below the base.
    let low = FixedJitter { value: 0 };
    assert_eq!(decorrelated_jitter_ms(&low, BASE, 4 * BASE, CAP), BASE);
}

#[tokio::test]
async fn non_retryable_categories_are_attempted_exactly_once() {
    let terminal = [
        ProviderFailureCategory::Auth,
        ProviderFailureCategory::QuotaOrBilling,
        ProviderFailureCategory::InvalidRequest,
        ProviderFailureCategory::ContextOverflow,
        ProviderFailureCategory::MalformedResponse,
    ];
    for category in terminal {
        let clock = FixedClock::start();
        let sleep = RecordedSleep::default();
        let jitter = FixedJitter { value: 0 };
        let calls = Arc::new(Mutex::new(0_u32));

        let error = run_with_retries(POLICY, &clock, &sleep, &jitter, None, |_attempt| {
            let calls = calls.clone();
            *calls.lock().unwrap() += 1;
            let result: Result<u32, AttemptFailure> = Err(error_of(category, None));
            std::future::ready(result)
        })
        .await
        .expect_err("non-retryable category surfaces");

        assert_eq!(*calls.lock().unwrap(), 1, "{category:?} must not retry");
        assert!(
            sleep.delays.lock().unwrap().is_empty(),
            "{category:?} must not sleep"
        );
        assert!(
            matches!(
                error,
                ApplicationError::AuthenticationFailed
                    | ApplicationError::QuotaExceeded
                    | ApplicationError::InvalidInferenceRequest
                    | ApplicationError::ContextOverflow
                    | ApplicationError::MalformedModelOutput { .. }
            ),
            "{category:?} mapped to {error:?}"
        );
    }
}

#[tokio::test]
async fn retry_after_raises_the_sleep_floor() {
    let clock = FixedClock::start();
    let sleep = RecordedSleep::default();
    // Jitter would normally suggest 250 ms; the provider says 2 seconds.
    let jitter = FixedJitter { value: 0 };

    let _ = run_with_retries(POLICY, &clock, &sleep, &jitter, None, |_attempt| {
        let result: Result<u32, AttemptFailure> =
            Err(error_of(ProviderFailureCategory::RateLimit, Some(2_000)));
        std::future::ready(result)
    })
    .await
    .expect_err("exhausted attempts surface the classified error");

    let delays = sleep.delays.lock().unwrap();
    assert!(!delays.is_empty());
    assert!(
        delays.iter().all(|delay| *delay >= Duration::from_secs(2)),
        "Retry-After must raise the floor, got {delays:?}"
    );
}

#[tokio::test]
async fn retry_after_beyond_the_deadline_stops_instead_of_sleeping() {
    let clock = FixedClock::start();
    let sleep = RecordedSleep::default();
    let jitter = FixedJitter { value: 0 };
    let deadline = Some(clock.now() + Duration::from_secs(1));
    let calls = Arc::new(Mutex::new(0_u32));

    let error = run_with_retries(POLICY, &clock, &sleep, &jitter, deadline, |_attempt| {
        *calls.lock().unwrap() += 1;
        let result: Result<u32, AttemptFailure> =
            Err(error_of(ProviderFailureCategory::RateLimit, Some(60_000)));
        std::future::ready(result)
    })
    .await
    .expect_err("a Retry-After past the deadline surfaces instead of sleeping");

    assert!(matches!(error, ApplicationError::RateLimited { .. }));
    assert_eq!(*calls.lock().unwrap(), 1, "no retry past the deadline");
    assert!(
        sleep.delays.lock().unwrap().is_empty(),
        "no sleep past the deadline"
    );
}

#[tokio::test]
async fn a_passed_deadline_prevents_the_first_retry() {
    let clock = FixedClock::start();
    let sleep = RecordedSleep::default();
    let jitter = FixedJitter { value: 0 };
    let calls = Arc::new(Mutex::new(0_u32));

    // The deadline is already in the past.
    let deadline = clock
        .now()
        .checked_sub(Duration::from_secs(1))
        .map(Some)
        .expect("clock is past the unix epoch");
    let error = run_with_retries(POLICY, &clock, &sleep, &jitter, deadline, |_attempt| {
        *calls.lock().unwrap() += 1;
        let result: Result<u32, AttemptFailure> =
            Err(error_of(ProviderFailureCategory::Timeout, None));
        std::future::ready(result)
    })
    .await
    .expect_err("deadline prevents retry");

    assert_eq!(*calls.lock().unwrap(), 1);
    assert!(matches!(error, ApplicationError::InferenceTimeout));
}

#[tokio::test]
async fn streaming_failure_marked_terminal_is_never_replayed() {
    let clock = FixedClock::start();
    let sleep = RecordedSleep::default();
    let jitter = FixedJitter { value: 0 };
    let calls = Arc::new(Mutex::new(0_u32));

    // A stream that already emitted a delta must surface a terminal error
    // even though the category itself is retryable.
    let error = run_with_retries(POLICY, &clock, &sleep, &jitter, None, |_attempt| {
        *calls.lock().unwrap() += 1;
        let result: Result<u32, AttemptFailure> = {
            let retryable_error = error_of(ProviderFailureCategory::ServerError, None);
            Err(AttemptFailure::terminal(retryable_error.error))
        };
        std::future::ready(result)
    })
    .await
    .expect_err("post-delta failures are terminal");

    assert_eq!(
        *calls.lock().unwrap(),
        1,
        "no automatic replay after emitted output"
    );
    assert!(sleep.delays.lock().unwrap().is_empty());
    assert!(matches!(error, ApplicationError::InferenceUnavailable));
}

#[tokio::test]
async fn zero_grant_default_policy_matches_the_documented_bounds() {
    let policy = RetryPolicy::default();
    assert_eq!(policy.max_attempts, 3);
    assert_eq!(policy.base_ms, 250);
    assert_eq!(policy.cap_ms, 8_000);

    // Invalid bounds fall back to the documented defaults.
    let zero = RetryPolicy::new(0, 0, 0);
    assert_eq!(zero.max_attempts, 3);
    assert_eq!(zero.base_ms, 250);
    assert_eq!(zero.cap_ms, 250);
}

#[test]
fn retry_after_parsing_degrades_safely_on_invalid_values() {
    use cortex_inference::parse_retry_after;
    use reqwest::header::HeaderValue;

    // Delta seconds.
    assert_eq!(
        parse_retry_after(Some(&HeaderValue::from_static("5"))),
        Some(Duration::from_secs(5))
    );
    // Malformed values degrade to None → normal backoff.
    assert_eq!(
        parse_retry_after(Some(&HeaderValue::from_static("soon"))),
        None
    );
    assert_eq!(parse_retry_after(None), None);

    // A past HTTP date yields zero (retry immediately, bounded by backoff).
    let past = "Wed, 01 Jan 2020 00:00:00 GMT";
    assert_eq!(
        parse_retry_after(Some(&HeaderValue::from_str(past).expect("header"))),
        Some(Duration::ZERO)
    );
}

#[tokio::test]
async fn every_attempt_targets_the_same_model_and_identical_request_body() {
    use serde_json::Value;

    use cortex_inference::{
        InferenceMessage, InferenceProvider, InferenceRequest, OpenAiCompatibleConfig,
        OpenAiCompatibleProvider, OpenAiTransport, ProviderLimits,
    };
    use std::sync::Mutex as StdMutex;

    #[derive(Clone)]
    struct RecordingTransport {
        failures_left: Arc<StdMutex<u32>>,
        requests: Arc<StdMutex<Vec<Value>>>,
    }

    impl OpenAiTransport for RecordingTransport {
        async fn get_json(
            &self,
            _endpoint: &str,
            _bearer: Option<&str>,
            _timeout: Duration,
            _max_response_bytes: usize,
        ) -> Result<Vec<u8>, ProviderError> {
            Err(ProviderError::from_category(
                ProviderFailureCategory::Unavailable,
            ))
        }

        async fn post_json(
            &self,
            _endpoint: &str,
            _bearer: Option<&str>,
            body: Value,
            _timeout: Duration,
            _max_response_bytes: usize,
        ) -> Result<Vec<u8>, ProviderError> {
            self.requests.lock().unwrap().push(body.clone());
            let mut failures = self.failures_left.lock().unwrap();
            if *failures > 0 {
                *failures -= 1;
                return Err(ProviderError::from_category(
                    ProviderFailureCategory::ServerError,
                ));
            }
            Ok(serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": "done"}}]
            })
            .to_string()
            .into_bytes())
        }
    }

    let requests: Arc<StdMutex<Vec<Value>>> = Arc::new(StdMutex::new(Vec::new()));
    let config = OpenAiCompatibleConfig::new(
        "http://127.0.0.1:9/v1",
        "test-model",
        None,
        Duration::from_secs(5),
        ProviderLimits::new(64 * 1024, 64 * 1024, 4096).expect("limits"),
    )
    .expect("config");
    let provider = OpenAiCompatibleProvider::with_transport(
        config,
        RecordingTransport {
            failures_left: Arc::new(StdMutex::new(2)),
            requests: Arc::clone(&requests),
        },
    )
    .with_retry_policy(RetryPolicy::default());

    let request = InferenceRequest {
        messages: vec![InferenceMessage::User {
            content: "hello".to_owned(),
        }],
        tools: Vec::new(),
    };
    let response = provider
        .complete(request)
        .await
        .expect("third attempt wins");
    assert_eq!(response.content.as_deref(), Some("done"));
    assert!(response.tool_calls.is_empty());

    // All three attempts sent a byte-identical request for the same model:
    // no hidden fallback, no request mutation between attempts.
    let recorded = requests.lock().unwrap();
    assert_eq!(recorded.len(), 3, "two retries after the first failure");
    let first = recorded[0].clone();
    for body in recorded.iter() {
        assert_eq!(body, &first, "every attempt replays an identical request");
        assert_eq!(body["model"], "test-model");
    }
}
