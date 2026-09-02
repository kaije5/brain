use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use cortex_application::{AgentCapabilityExecutor, ApplicationError, Capability, CommandContext};
use cortex_domain::{OperationId, PrincipalId, WorkspaceId};
use cortex_inference::{
    AgentLimits, AgentRunner, InferenceProvider, InferenceRequest, InferenceResponse, ToolCall,
};
use serde_json::{Value, json};
use uuid::Uuid;

struct FakeProvider {
    responses: Mutex<VecDeque<Result<InferenceResponse, ApplicationError>>>,
    delay: Duration,
}

impl FakeProvider {
    fn from_responses(responses: Vec<InferenceResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().map(Ok).collect()),
            delay: Duration::ZERO,
        }
    }

    fn delayed(delay: Duration) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from([Ok(final_response("late"))])),
            delay,
        }
    }
}

impl InferenceProvider for FakeProvider {
    async fn complete(
        &self,
        _request: InferenceRequest,
    ) -> Result<InferenceResponse, ApplicationError> {
        tokio::time::sleep(self.delay).await;
        self.responses
            .lock()
            .map_err(|_| ApplicationError::Internal)?
            .pop_front()
            .ok_or(ApplicationError::InferenceUnavailable)?
    }
}

#[derive(Default)]
struct RecordingService {
    calls: AtomicUsize,
    payloads: Mutex<Vec<Value>>,
}

impl RecordingService {
    fn recorded_calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl AgentCapabilityExecutor for RecordingService {
    async fn execute_agent_tool(
        &self,
        _context: CommandContext,
        _capability: Capability,
        payload: Value,
    ) -> Result<Value, ApplicationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.payloads
            .lock()
            .map_err(|_| ApplicationError::Internal)?
            .push(payload);
        Ok(json!({ "accepted": true }))
    }
}

fn context() -> CommandContext {
    CommandContext::from_authenticated(
        WorkspaceId::new(),
        PrincipalId::new(),
        OperationId::new(),
        Uuid::now_v7(),
    )
}

fn limits(max_iterations: u8, timeout: Duration) -> AgentLimits {
    AgentLimits::new(max_iterations, timeout, 1)
        .unwrap_or_else(|error| panic!("test limits must be valid: {error:?}"))
}

fn tool_response(calls: Vec<ToolCall>) -> InferenceResponse {
    InferenceResponse {
        content: None,
        tool_calls: calls,
    }
}

fn final_response(content: &str) -> InferenceResponse {
    InferenceResponse {
        content: Some(content.to_owned()),
        tool_calls: Vec::new(),
    }
}

fn note_call(id: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: "cortex_note_create".to_owned(),
        arguments: arguments.to_owned(),
    }
}

#[tokio::test]
async fn malformed_tool_arguments_never_call_the_application_service() {
    let provider = Arc::new(FakeProvider::from_responses(vec![tool_response(vec![
        note_call("call-1", "{\"title\":42,\"content\":\"x\"}"),
    ])]));
    let service = Arc::new(RecordingService::default());
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        limits(2, Duration::from_secs(1)),
    );

    let outcome = runner.run(context(), "remember x").await;

    assert!(matches!(
        outcome,
        Err(ApplicationError::MalformedModelOutput { .. })
    ));
    assert_eq!(service.recorded_calls(), 0);
}

#[tokio::test]
async fn capability_semantics_are_validated_before_service_invocation() {
    let provider = Arc::new(FakeProvider::from_responses(vec![tool_response(vec![
        ToolCall {
            id: "call-memory".to_owned(),
            name: "cortex_memory_create".to_owned(),
            arguments: concat!(
                "{\"statement\":\"Cortex is local\",",
                "\"normalized_subject\":\"cortex\",",
                "\"normalized_predicate\":\"is\",",
                "\"normalized_object\":\"local\",",
                "\"sources\":[]}"
            )
            .to_owned(),
        },
    ])]));
    let service = Arc::new(RecordingService::default());
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        limits(2, Duration::from_secs(1)),
    );

    let outcome = runner.run(context(), "remember sourced fact").await;

    assert!(matches!(
        outcome,
        Err(ApplicationError::MalformedModelOutput { .. })
    ));
    assert_eq!(service.recorded_calls(), 0);
}

#[tokio::test]
async fn duplicate_tool_call_is_rejected_before_a_second_side_effect() {
    let valid = "{\"title\":\"Cortex\",\"content\":\"local first\"}";
    let provider = Arc::new(FakeProvider::from_responses(vec![tool_response(vec![
        note_call("call-7", valid),
        note_call("call-7", valid),
    ])]));
    let service = Arc::new(RecordingService::default());
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        limits(2, Duration::from_secs(1)),
    );

    let outcome = runner.run(context(), "remember Cortex").await;

    assert!(matches!(
        outcome,
        Err(ApplicationError::MalformedModelOutput { .. })
    ));
    assert_eq!(service.recorded_calls(), 1);
}

#[tokio::test]
async fn successful_tool_result_is_returned_to_provider_before_final_text() {
    let provider = Arc::new(FakeProvider::from_responses(vec![
        tool_response(vec![note_call(
            "call-2",
            "{\"title\":\"Cortex\",\"content\":\"local first\"}",
        )]),
        final_response("Remembered."),
    ]));
    let service = Arc::new(RecordingService::default());
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        limits(2, Duration::from_secs(1)),
    );

    let outcome = runner.run(context(), "remember Cortex").await;

    assert_eq!(outcome, Ok("Remembered.".to_owned()));
    assert_eq!(service.recorded_calls(), 1);
}

#[tokio::test]
async fn total_agent_timeout_cancels_before_any_late_side_effect() {
    let provider = Arc::new(FakeProvider::delayed(Duration::from_secs(1)));
    let service = Arc::new(RecordingService::default());
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        limits(2, Duration::from_millis(10)),
    );

    let outcome = runner.run(context(), "wait").await;

    assert_eq!(outcome, Err(ApplicationError::InferenceTimeout));
    assert_eq!(service.recorded_calls(), 0);
}

#[tokio::test]
async fn iteration_limit_stops_an_unfinished_agent_loop() {
    let provider = Arc::new(FakeProvider::from_responses(vec![tool_response(vec![
        note_call(
            "call-3",
            "{\"title\":\"Cortex\",\"content\":\"local first\"}",
        ),
    ])]));
    let service = Arc::new(RecordingService::default());
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        limits(1, Duration::from_secs(1)),
    );

    let outcome = runner.run(context(), "loop").await;

    assert!(matches!(
        outcome,
        Err(ApplicationError::MalformedModelOutput { .. })
    ));
    assert_eq!(service.recorded_calls(), 1);
}
