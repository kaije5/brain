use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use cortex_application::{AgentCapabilityExecutor, ApplicationError, Capability, CommandContext};
use cortex_domain::{OperationId, PrincipalId, WorkspaceId};
use cortex_inference::{
    AgentLimits, AgentRunner, AuthorizedCapabilities, InferenceProvider, InferenceRequest,
    InferenceResponse, ToolCall,
};
use serde_json::{Value, json};
use uuid::Uuid;

struct FakeProvider {
    responses: Mutex<VecDeque<Result<InferenceResponse, ApplicationError>>>,
    requests: Mutex<Vec<InferenceRequest>>,
    delay: Duration,
}

impl FakeProvider {
    fn from_responses(responses: Vec<InferenceResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().map(Ok).collect()),
            requests: Mutex::new(Vec::new()),
            delay: Duration::ZERO,
        }
    }

    fn delayed(delay: Duration) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from([Ok(final_response("late"))])),
            requests: Mutex::new(Vec::new()),
            delay,
        }
    }

    fn requests(&self) -> Vec<InferenceRequest> {
        self.requests
            .lock()
            .map_or_else(|_| Vec::new(), |requests| requests.clone())
    }
}

impl InferenceProvider for FakeProvider {
    async fn complete(
        &self,
        request: InferenceRequest,
    ) -> Result<InferenceResponse, ApplicationError> {
        tokio::time::sleep(self.delay).await;
        self.requests
            .lock()
            .map_err(|_| ApplicationError::Internal)?
            .push(request);
        self.responses
            .lock()
            .map_err(|_| ApplicationError::Internal)?
            .pop_front()
            .ok_or(ApplicationError::InferenceUnavailable)?
    }
}

struct RecordingService {
    calls: AtomicUsize,
    payloads: Mutex<Vec<Value>>,
    contexts: Mutex<Vec<CommandContext>>,
    delay: Duration,
    response: Value,
    completed: AtomicBool,
}

impl RecordingService {
    fn responding(response: Value, delay: Duration) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            payloads: Mutex::new(Vec::new()),
            contexts: Mutex::new(Vec::new()),
            delay,
            response,
            completed: AtomicBool::new(false),
        }
    }

    fn recorded_calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn payloads(&self) -> Vec<Value> {
        self.payloads
            .lock()
            .map_or_else(|_| Vec::new(), |payloads| payloads.clone())
    }

    fn contexts(&self) -> Vec<CommandContext> {
        self.contexts
            .lock()
            .map_or_else(|_| Vec::new(), |contexts| contexts.clone())
    }

    fn completed(&self) -> bool {
        self.completed.load(Ordering::SeqCst)
    }
}

impl Default for RecordingService {
    fn default() -> Self {
        Self::responding(json!({ "accepted": true }), Duration::ZERO)
    }
}

impl AgentCapabilityExecutor for RecordingService {
    async fn execute_agent_tool(
        &self,
        context: CommandContext,
        _capability: Capability,
        payload: Value,
    ) -> Result<Value, ApplicationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.contexts
            .lock()
            .map_err(|_| ApplicationError::Internal)?
            .push(context);
        self.payloads
            .lock()
            .map_err(|_| ApplicationError::Internal)?
            .push(payload);
        tokio::time::sleep(self.delay).await;
        self.completed.store(true, Ordering::SeqCst);
        Ok(self.response.clone())
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
    constrained_limits(max_iterations, timeout, 1, 8 * 1024, 64 * 1024)
}

fn constrained_limits(
    max_iterations: u8,
    timeout: Duration,
    max_duplicate_calls: u8,
    max_message_bytes: usize,
    max_request_bytes: usize,
) -> AgentLimits {
    AgentLimits::new(
        max_iterations,
        timeout,
        max_duplicate_calls,
        max_message_bytes,
        max_request_bytes,
    )
    .unwrap_or_else(|error| panic!("test limits must be valid: {error:?}"))
}

fn allowed(capabilities: impl IntoIterator<Item = Capability>) -> AuthorizedCapabilities {
    AuthorizedCapabilities::new(capabilities)
        .unwrap_or_else(|error| panic!("test capability subset must be valid: {error:?}"))
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

    let outcome = runner
        .run(context(), "remember x", allowed([Capability::NoteCreate]))
        .await;

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

    let outcome = runner
        .run(
            context(),
            "remember sourced fact",
            allowed([Capability::MemoryCreate]),
        )
        .await;

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

    let outcome = runner
        .run(
            context(),
            "remember Cortex",
            allowed([Capability::NoteCreate]),
        )
        .await;

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

    let outcome = runner
        .run(
            context(),
            "remember Cortex",
            allowed([Capability::NoteCreate]),
        )
        .await;

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

    let outcome = runner
        .run(context(), "wait", allowed([Capability::NoteCreate]))
        .await;

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

    let outcome = runner
        .run(context(), "loop", allowed([Capability::NoteCreate]))
        .await;

    assert!(matches!(
        outcome,
        Err(ApplicationError::MalformedModelOutput { .. })
    ));
    assert_eq!(service.recorded_calls(), 1);
}

#[tokio::test]
async fn model_receives_only_the_authorized_capability_subset() {
    let provider = Arc::new(FakeProvider::from_responses(vec![final_response("ready")]));
    let service = Arc::new(RecordingService::default());
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        limits(1, Duration::from_secs(1)),
    );

    let outcome = runner
        .run(
            context(),
            "show tools",
            allowed([Capability::NoteSearch, Capability::TaskList]),
        )
        .await;

    assert_eq!(outcome, Ok("ready".to_owned()));
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].tools.len(), 2);
    assert_eq!(requests[0].tools[0].name, "cortex_note_search");
    assert_eq!(requests[0].tools[1].name, "cortex_task_list");
}

#[tokio::test]
async fn canonical_but_unauthorized_tool_is_rejected_before_executor() {
    let provider = Arc::new(FakeProvider::from_responses(vec![tool_response(vec![
        ToolCall {
            id: "call-denied".to_owned(),
            name: "cortex_task_delete".to_owned(),
            arguments: format!(
                "{{\"entity_id\":\"{}\",\"expected_revision\":1}}",
                Uuid::now_v7()
            ),
        },
    ])]));
    let service = Arc::new(RecordingService::default());
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        limits(2, Duration::from_secs(1)),
    );

    let outcome = runner
        .run(context(), "delete task", allowed([Capability::NoteSearch]))
        .await;

    assert!(matches!(
        outcome,
        Err(ApplicationError::MalformedModelOutput { .. })
    ));
    assert_eq!(service.recorded_calls(), 0);
}

#[tokio::test]
async fn oversized_assistant_content_with_tool_call_is_not_retained_or_executed() {
    let provider = Arc::new(FakeProvider::from_responses(vec![InferenceResponse {
        content: Some("x".repeat(129)),
        tool_calls: vec![note_call(
            "call-large-assistant",
            "{\"title\":\"Cortex\",\"content\":\"bounded\"}",
        )],
    }]));
    let service = Arc::new(RecordingService::default());
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        constrained_limits(2, Duration::from_secs(1), 1, 128, 16 * 1024),
    );

    let outcome = runner
        .run(context(), "bounded", allowed([Capability::NoteCreate]))
        .await;

    assert!(matches!(
        outcome,
        Err(ApplicationError::MalformedModelOutput { .. })
    ));
    assert_eq!(service.recorded_calls(), 0);
}

#[tokio::test]
async fn oversized_tool_result_is_not_added_to_conversation_history() {
    let provider = Arc::new(FakeProvider::from_responses(vec![tool_response(vec![
        note_call(
            "call-large-result",
            "{\"title\":\"Cortex\",\"content\":\"bounded\"}",
        ),
    ])]));
    let service = Arc::new(RecordingService::responding(
        json!({ "data": "x".repeat(512) }),
        Duration::ZERO,
    ));
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        constrained_limits(2, Duration::from_secs(1), 1, 384, 16 * 1024),
    );

    let outcome = runner
        .run(context(), "bounded", allowed([Capability::NoteCreate]))
        .await;

    assert!(matches!(
        outcome,
        Err(ApplicationError::MalformedModelOutput { .. })
    ));
    assert_eq!(service.recorded_calls(), 1);
    assert_eq!(provider.requests().len(), 1);
}

#[tokio::test]
async fn cumulative_history_is_bounded_before_another_inference_request() {
    let provider = Arc::new(FakeProvider::from_responses(vec![
        tool_response(vec![note_call(
            "call-history-1",
            "{\"title\":\"Cortex\",\"content\":\"one\"}",
        )]),
        tool_response(vec![note_call(
            "call-history-2",
            "{\"title\":\"Cortex\",\"content\":\"two\"}",
        )]),
        final_response("unreachable"),
    ]));
    let service = Arc::new(RecordingService::responding(
        json!({ "data": "x".repeat(400) }),
        Duration::ZERO,
    ));
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        constrained_limits(3, Duration::from_secs(1), 1, 512, 1_500),
    );

    let outcome = runner
        .run(
            context(),
            "bounded history",
            allowed([Capability::NoteCreate]),
        )
        .await;

    assert!(matches!(
        outcome,
        Err(ApplicationError::MalformedModelOutput { .. })
    ));
    assert_eq!(provider.requests().len(), 2);
    assert_eq!(service.recorded_calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn started_tool_execution_is_awaited_even_when_agent_deadline_expires() {
    let provider = Arc::new(FakeProvider::from_responses(vec![
        tool_response(vec![note_call(
            "call-blocking",
            "{\"title\":\"Cortex\",\"content\":\"durable\"}",
        )]),
        final_response("too late"),
    ]));
    let service = Arc::new(RecordingService::responding(
        json!({ "accepted": true }),
        Duration::from_secs(1),
    ));
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        limits(2, Duration::from_millis(10)),
    );

    let outcome = runner
        .run(
            context(),
            "commit safely",
            allowed([Capability::NoteCreate]),
        )
        .await;

    assert_eq!(outcome, Err(ApplicationError::InferenceTimeout));
    assert_eq!(service.recorded_calls(), 1);
    assert!(service.completed());
    assert_eq!(provider.requests().len(), 1);
}

#[tokio::test]
async fn configured_duplicate_occurrence_limit_is_honored() {
    let valid = "{\"title\":\"Cortex\",\"content\":\"local first\"}";
    let provider = Arc::new(FakeProvider::from_responses(vec![tool_response(vec![
        note_call("call-repeat", valid),
        note_call("call-repeat", valid),
        note_call("call-repeat", valid),
    ])]));
    let service = Arc::new(RecordingService::default());
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        constrained_limits(2, Duration::from_secs(1), 2, 8 * 1024, 64 * 1024),
    );

    let outcome = runner
        .run(
            context(),
            "repeat safely",
            allowed([Capability::NoteCreate]),
        )
        .await;

    assert!(matches!(
        outcome,
        Err(ApplicationError::MalformedModelOutput { .. })
    ));
    assert_eq!(service.recorded_calls(), 2);
    let contexts = service.contexts();
    assert_eq!(contexts.len(), 2);
    assert_eq!(contexts[0], contexts[1]);
}

fn task_update_call(id: &str, entity_id: Uuid, update: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: "cortex_task_update".to_owned(),
        arguments: format!("{{\"entity_id\":\"{entity_id}\",\"expected_revision\":1,{update}}}"),
    }
}

#[tokio::test]
async fn task_update_preserves_null_only_due_date_clear() {
    let provider = Arc::new(FakeProvider::from_responses(vec![
        tool_response(vec![task_update_call(
            "call-clear",
            Uuid::now_v7(),
            "\"due_at\":null",
        )]),
        final_response("cleared"),
    ]));
    let service = Arc::new(RecordingService::default());
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        limits(2, Duration::from_secs(1)),
    );

    let outcome = runner
        .run(
            context(),
            "clear due date",
            allowed([Capability::TaskUpdate]),
        )
        .await;

    assert_eq!(outcome, Ok("cleared".to_owned()));
    assert_eq!(service.payloads()[0]["due_at"], Value::Null);
}

#[tokio::test]
async fn task_update_preserves_due_date_null_alongside_title_change() {
    let provider = Arc::new(FakeProvider::from_responses(vec![
        tool_response(vec![task_update_call(
            "call-title-clear",
            Uuid::now_v7(),
            "\"title\":\"new title\",\"due_at\":null",
        )]),
        final_response("updated"),
    ]));
    let service = Arc::new(RecordingService::default());
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        limits(2, Duration::from_secs(1)),
    );

    let outcome = runner
        .run(
            context(),
            "update and clear",
            allowed([Capability::TaskUpdate]),
        )
        .await;

    assert_eq!(outcome, Ok("updated".to_owned()));
    assert_eq!(service.payloads()[0]["title"], "new title");
    assert_eq!(service.payloads()[0]["due_at"], Value::Null);
}
