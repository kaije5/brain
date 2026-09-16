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
    AgentLimits, AgentRunner, AuthorizedCapabilities, InferenceMessage, InferenceProvider,
    InferenceRequest, InferenceResponse, SystemPrompt, ToolCall,
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
        name: "cortex_knowledge_create".to_owned(),
        arguments: arguments.to_owned(),
    }
}

#[tokio::test]
async fn malformed_tool_arguments_never_call_the_application_service() {
    let provider = Arc::new(FakeProvider::from_responses(vec![tool_response(vec![
        note_call("call-1", "{\"title\":42,\"body\":\"x\"}"),
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
            "remember x",
            allowed([Capability::KnowledgeCreate]),
        )
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
    let valid = "{\"title\":\"Cortex\",\"body\":\"local first\"}";
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
            allowed([Capability::KnowledgeCreate]),
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
            "{\"title\":\"Cortex\",\"body\":\"local first\"}",
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
            allowed([Capability::KnowledgeCreate]),
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
        .run(context(), "wait", allowed([Capability::KnowledgeCreate]))
        .await;

    assert_eq!(outcome, Err(ApplicationError::InferenceTimeout));
    assert_eq!(service.recorded_calls(), 0);
}

#[tokio::test]
async fn iteration_limit_stops_an_unfinished_agent_loop() {
    let provider = Arc::new(FakeProvider::from_responses(vec![tool_response(vec![
        note_call("call-3", "{\"title\":\"Cortex\",\"body\":\"local first\"}"),
    ])]));
    let service = Arc::new(RecordingService::default());
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        limits(1, Duration::from_secs(1)),
    );

    let outcome = runner
        .run(context(), "loop", allowed([Capability::KnowledgeCreate]))
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
            allowed([Capability::KnowledgeRetrieve, Capability::TaskList]),
        )
        .await;

    assert_eq!(outcome, Ok("ready".to_owned()));
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].tools.len(), 2);
    // Catalog order: task query precedes knowledge retrieval.
    assert_eq!(requests[0].tools[0].name, "cortex_task_list");
    assert_eq!(requests[0].tools[1].name, "cortex_knowledge_search");
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
        .run(
            context(),
            "delete task",
            allowed([Capability::KnowledgeRetrieve]),
        )
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
            "{\"title\":\"Cortex\",\"body\":\"bounded\"}",
        )],
    }]));
    let service = Arc::new(RecordingService::default());
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        constrained_limits(2, Duration::from_secs(1), 1, 128, 16 * 1024),
    );

    let outcome = runner
        .run(context(), "bounded", allowed([Capability::KnowledgeCreate]))
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
            "{\"title\":\"Cortex\",\"body\":\"bounded\"}",
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
        .run(context(), "bounded", allowed([Capability::KnowledgeCreate]))
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
            "{\"title\":\"Cortex\",\"body\":\"one\"}",
        )]),
        tool_response(vec![note_call(
            "call-history-2",
            "{\"title\":\"Cortex\",\"body\":\"two\"}",
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
            allowed([Capability::KnowledgeCreate]),
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
            "{\"title\":\"Cortex\",\"body\":\"durable\"}",
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
            allowed([Capability::KnowledgeCreate]),
        )
        .await;

    assert_eq!(outcome, Err(ApplicationError::InferenceTimeout));
    assert_eq!(service.recorded_calls(), 1);
    assert!(service.completed());
    assert_eq!(provider.requests().len(), 1);
}

#[tokio::test]
async fn configured_duplicate_occurrence_limit_is_honored() {
    let valid = "{\"title\":\"Cortex\",\"body\":\"local first\"}";
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
            allowed([Capability::KnowledgeCreate]),
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

#[tokio::test]
async fn provider_failure_fails_the_turn_safely_without_side_effects() {
    let provider = Arc::new(FakeProvider {
        responses: Mutex::new(VecDeque::from([Err(
            ApplicationError::InferenceUnavailable,
        )])),
        requests: Mutex::new(Vec::new()),
        delay: Duration::ZERO,
    });
    let service = Arc::new(RecordingService::default());
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        limits(3, Duration::from_secs(1)),
    );

    let outcome = runner
        .run(
            context(),
            "remember x",
            allowed([Capability::KnowledgeCreate]),
        )
        .await;

    assert_eq!(outcome, Err(ApplicationError::InferenceUnavailable));
    assert_eq!(
        service.recorded_calls(),
        0,
        "no tool executes when the provider fails"
    );
}

#[tokio::test]
async fn streaming_reports_each_assistant_segment_in_order() {
    let provider = FakeProvider::from_responses(vec![
        InferenceResponse {
            content: Some("checking your notes".to_owned()),
            tool_calls: vec![note_call("call-1", r#"{ "title": "t", "body": "c" }"#)],
        },
        final_response("all done"),
    ]);
    let chunks = Arc::new(Mutex::new(Vec::new()));
    let recorded = chunks.clone();
    let on_chunk = move |chunk: &str| {
        chunks.lock().expect("chunk log").push(chunk.to_owned());
    };
    let runner = AgentRunner::new(
        Arc::new(provider),
        Arc::new(RecordingService::default()),
        limits(4, Duration::from_secs(5)),
    );

    let output = runner
        .run_streaming(
            context(),
            "stream please",
            allowed([Capability::KnowledgeCreate]),
            &on_chunk,
        )
        .await
        .expect("streaming run succeeds");

    assert_eq!(output, "all done");
    assert_eq!(
        recorded.lock().unwrap().clone(),
        vec!["checking your notes".to_owned(), "all done".to_owned()],
        "intermediate text preceding tool calls and the final answer must both stream"
    );
}

#[tokio::test]
async fn non_streaming_runs_do_not_report_chunks() {
    let provider = FakeProvider::from_responses(vec![final_response("quiet answer")]);
    let chunks = Arc::new(Mutex::new(Vec::new()));
    let recorded = chunks.clone();
    let on_chunk = move |chunk: &str| {
        chunks.lock().expect("chunk log").push(chunk.to_owned());
    };
    let runner = AgentRunner::new(
        Arc::new(provider),
        Arc::new(RecordingService::default()),
        limits(4, Duration::from_secs(5)),
    );

    let output = runner
        .run_streaming(context(), "hello", allowed([]), &on_chunk)
        .await
        .expect("run succeeds");
    assert_eq!(output, "quiet answer");
    assert_eq!(recorded.lock().unwrap().clone(), vec!["quiet answer"]);
}

fn stable_prompt() -> SystemPrompt {
    SystemPrompt::new("stable cortex instructions").expect("stable tier is valid")
}

#[tokio::test]
async fn system_prompt_prepends_a_byte_stable_system_message_to_every_request() {
    let provider = Arc::new(FakeProvider::from_responses(vec![
        tool_response(vec![note_call(
            "call-sys-1",
            "{\"title\":\"Cortex\",\"body\":\"local first\"}",
        )]),
        final_response("done"),
    ]));
    let service = Arc::new(RecordingService::default());
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        limits(2, Duration::from_secs(1)),
    )
    .with_system_prompt(
        stable_prompt()
            .with_context("session context")
            .expect("context tier is valid"),
    );

    let outcome = runner
        .run(context(), "greet", allowed([Capability::KnowledgeCreate]))
        .await;

    assert_eq!(outcome, Ok("done".to_owned()));
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert!(matches!(
            request.messages.first(),
            Some(InferenceMessage::System { .. })
        ));
    }
    assert_eq!(
        requests[0].messages,
        requests[1].messages[..requests[0].messages.len()]
    );
}

#[tokio::test]
async fn compaction_replaces_old_turns_with_a_summary_and_the_run_still_succeeds() {
    let provider = Arc::new(FakeProvider::from_responses(vec![
        tool_response(vec![note_call(
            "call-compact-1",
            "{\"title\":\"Cortex\",\"body\":\"first turn\"}",
        )]),
        tool_response(vec![note_call(
            "call-compact-2",
            "{\"title\":\"Cortex\",\"body\":\"second turn\"}",
        )]),
        InferenceResponse {
            content: Some("summary of the earlier turns".to_owned()),
            tool_calls: Vec::new(),
        },
        final_response("completed after compaction"),
    ]));
    let service = Arc::new(RecordingService::responding(
        json!({ "data": "x".repeat(700) }),
        Duration::ZERO,
    ));
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        constrained_limits(4, Duration::from_secs(5), 1, 8 * 1024, 4 * 1024),
    )
    .with_compaction_threshold(512)
    .expect("compaction threshold below request limit");

    let outcome = runner
        .run(
            context(),
            "compact me",
            allowed([Capability::KnowledgeCreate]),
        )
        .await;

    assert_eq!(outcome, Ok("completed after compaction".to_owned()));
    let requests = provider.requests();
    assert_eq!(requests.len(), 4, "two turns, one compaction, one final");
    let summary_request = &requests[2];
    let summarized = summary_request.messages.iter().any(|message| {
        matches!(message, InferenceMessage::User { content }
            if content.contains("Summarize the following conversation excerpt"))
    });
    assert!(summarized, "compaction must send a summarization request");
    let final_request = &requests[3];
    assert!(
        final_request
            .messages
            .iter()
            .any(|message| matches!(message,
            InferenceMessage::User { content }
            if content.contains("summary of the earlier turns"))),
        "the summary must replace the compacted turns"
    );
    assert!(
        !final_request
            .messages
            .iter()
            .any(|message| matches!(message,
            InferenceMessage::Tool { content, .. }
            if content.to_string().contains("second turn"))),
        "compacted tool results must be gone from the final request"
    );
}

#[tokio::test]
async fn compaction_summary_is_never_streamed_to_the_user() {
    let provider = Arc::new(FakeProvider::from_responses(vec![
        tool_response(vec![note_call(
            "call-stream-compact",
            "{\"title\":\"Cortex\",\"body\":\"first turn\"}",
        )]),
        tool_response(vec![note_call(
            "call-stream-compact-2",
            "{\"title\":\"Cortex\",\"body\":\"second turn\"}",
        )]),
        InferenceResponse {
            content: Some("internal summary".to_owned()),
            tool_calls: Vec::new(),
        },
        final_response("final visible answer"),
    ]));
    let service = Arc::new(RecordingService::responding(
        json!({ "data": "x".repeat(700) }),
        Duration::ZERO,
    ));
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        constrained_limits(4, Duration::from_secs(5), 1, 8 * 1024, 4 * 1024),
    )
    .with_compaction_threshold(512)
    .expect("compaction threshold below request limit");
    let chunks = Arc::new(Mutex::new(Vec::new()));
    let recorded = chunks.clone();
    let on_chunk = move |chunk: &str| {
        chunks.lock().expect("chunk log").push(chunk.to_owned());
    };

    let outcome = runner
        .run_streaming(
            context(),
            "compact and stream",
            allowed([Capability::KnowledgeCreate]),
            &on_chunk,
        )
        .await
        .expect("streaming run with compaction succeeds");

    assert_eq!(outcome, "final visible answer");
    assert_eq!(
        recorded.lock().unwrap().clone(),
        vec!["final visible answer".to_owned()],
        "compaction summaries are internal and must not stream"
    );
}

#[tokio::test]
async fn compaction_rejects_unusable_summaries_as_malformed_output() {
    let provider = Arc::new(FakeProvider::from_responses(vec![
        tool_response(vec![note_call(
            "call-bad-summary",
            "{\"title\":\"Cortex\",\"body\":\"first turn\"}",
        )]),
        final_response("   "),
        final_response("unreachable"),
    ]));
    let service = Arc::new(RecordingService::responding(
        json!({ "data": "x".repeat(700) }),
        Duration::ZERO,
    ));
    let runner = AgentRunner::new(
        Arc::clone(&provider),
        Arc::clone(&service),
        constrained_limits(4, Duration::from_secs(5), 1, 8 * 1024, 4 * 1024),
    )
    .with_compaction_threshold(512)
    .expect("compaction threshold below request limit");

    let outcome = runner
        .run(
            context(),
            "compact me",
            allowed([Capability::KnowledgeCreate]),
        )
        .await;

    assert!(matches!(
        outcome,
        Err(ApplicationError::MalformedModelOutput { .. })
    ));
}

#[test]
fn compaction_threshold_must_leave_room_below_the_request_limit() {
    let make_runner = || {
        AgentRunner::new(
            Arc::new(FakeProvider::from_responses(vec![final_response("x")])),
            Arc::new(RecordingService::default()),
            limits(1, Duration::from_secs(1)),
        )
    };
    assert!(make_runner().with_compaction_threshold(0).is_err());
    assert!(make_runner().with_compaction_threshold(64 * 1024).is_err());
    assert!(make_runner().with_compaction_threshold(32 * 1024).is_ok());
}
