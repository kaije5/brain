//! SCRUM-132: normalized vault change events trigger idempotent automation;
//! failed handlers do not advance run state and are redeliverable.

mod support;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use cortex_application::{
    ApplicationError, AutomationEngine, AutomationRunLog, AutomationRunRecord, VaultChangeHandler,
    VaultChangeKind, VaultChangeTrigger,
};
use cortex_domain::{OperationId, WorkspaceId};

#[derive(Default, Clone)]
struct MemoryRunLog {
    runs: Arc<Mutex<BTreeSet<(WorkspaceId, String, OperationId)>>>,
}

impl AutomationRunLog for MemoryRunLog {
    async fn has_run(
        &self,
        workspace_id: WorkspaceId,
        rule_id: &str,
        event_id: OperationId,
    ) -> Result<bool, ApplicationError> {
        Ok(self.runs.lock().expect("log mutex").contains(&(
            workspace_id,
            rule_id.to_owned(),
            event_id,
        )))
    }

    async fn record(&self, run: AutomationRunRecord) -> Result<(), ApplicationError> {
        self.runs.lock().expect("log mutex").insert((
            run.workspace_id(),
            run.rule_id().to_owned(),
            run.event_id(),
        ));
        Ok(())
    }
}

#[derive(Default, Clone)]
struct RecordingHandler {
    handled: Arc<Mutex<Vec<(String, VaultChangeKind)>>>,
    fail: Arc<Mutex<bool>>,
}

impl RecordingHandler {
    fn handled_count(&self) -> usize {
        self.handled.lock().expect("handled mutex").len()
    }

    fn set_fail(&self, fail: bool) {
        *self.fail.lock().expect("fail mutex") = fail;
    }
}

impl VaultChangeHandler for RecordingHandler {
    async fn handle(
        &self,
        _workspace_id: WorkspaceId,
        trigger: &VaultChangeTrigger,
    ) -> Result<(), ApplicationError> {
        if *self.fail.lock().expect("fail mutex") {
            return Err(ApplicationError::Storage("handler failed".into()));
        }
        self.handled
            .lock()
            .expect("handled mutex")
            .push((trigger.resource_id().to_owned(), trigger.kind()));
        Ok(())
    }
}

fn trigger() -> VaultChangeTrigger {
    VaultChangeTrigger::new(
        OperationId::new(),
        VaultChangeKind::Updated,
        "markdown-vault",
        "path:notes/launch.md",
    )
    .expect("trigger")
}

#[tokio::test]
async fn redelivered_events_are_deduplicated_per_rule() {
    let workspace_id = WorkspaceId::new();
    let handler = RecordingHandler::default();
    let engine = AutomationEngine::new(handler.clone(), MemoryRunLog::default());
    let event = trigger();

    let first = engine
        .handle(workspace_id, "reindex-on-change", &event)
        .await
        .expect("first delivery");
    let second = engine
        .handle(workspace_id, "reindex-on-change", &event)
        .await
        .expect("redelivery");

    assert_eq!(first, cortex_application::AutomationOutcome::Ran);
    assert_eq!(second, cortex_application::AutomationOutcome::Deduplicated);
    assert_eq!(handler.handled_count(), 1, "handler ran exactly once");
}

#[tokio::test]
async fn different_rules_and_events_independently_trigger() {
    let workspace_id = WorkspaceId::new();
    let handler = RecordingHandler::default();
    let engine = AutomationEngine::new(handler.clone(), MemoryRunLog::default());
    let event = trigger();
    let other = VaultChangeTrigger::new(
        OperationId::new(),
        VaultChangeKind::Renamed,
        "markdown-vault",
        "path:tasks/moved.md",
    )
    .expect("trigger");

    engine
        .handle(workspace_id, "reindex-on-change", &event)
        .await
        .expect("first rule");
    engine
        .handle(workspace_id, "notify-on-change", &event)
        .await
        .expect("second rule");
    engine
        .handle(workspace_id, "reindex-on-change", &other)
        .await
        .expect("other event");

    assert_eq!(handler.handled_count(), 3);
}

#[tokio::test]
async fn failed_handlers_advance_nothing_and_redeliver_after_recovery() {
    let workspace_id = WorkspaceId::new();
    let handler = RecordingHandler::default();
    let log = MemoryRunLog::default();
    let engine = AutomationEngine::new(handler.clone(), log.clone());
    let event = trigger();
    handler.set_fail(true);

    let outcome = engine
        .handle(workspace_id, "reindex-on-change", &event)
        .await;
    assert!(outcome.is_err(), "handler failure surfaces as typed error");
    assert_eq!(handler.handled_count(), 0);
    assert!(
        !log.has_run(workspace_id, "reindex-on-change", event.event_id())
            .await
            .expect("log lookup"),
        "run state must not advance when the handler fails"
    );

    handler.set_fail(false);
    let recovered = engine
        .handle(workspace_id, "reindex-on-change", &event)
        .await
        .expect("redelivery after recovery");
    assert_eq!(recovered, cortex_application::AutomationOutcome::Ran);
    assert_eq!(handler.handled_count(), 1);
}

#[tokio::test]
async fn triggers_carry_normalized_provider_identity_not_paths_of_a_transport() {
    let workspace_id = WorkspaceId::new();
    let handler = RecordingHandler::default();
    let engine = AutomationEngine::new(handler.clone(), MemoryRunLog::default());
    // The trigger addresses the provider resource; the same normalized shape
    // would arrive from a watcher, a sync product, or manual replay.
    let event = VaultChangeTrigger::new(
        OperationId::new(),
        VaultChangeKind::Created,
        "markdown-vault",
        "path:notes/renamed.md",
    )
    .expect("trigger");
    engine
        .handle(workspace_id, "reindex-on-change", &event)
        .await
        .expect("handled");
    let (resource, kind) = &handler.handled.lock().expect("handled mutex")[0];
    assert_eq!(resource, "path:notes/renamed.md");
    assert_eq!(*kind, VaultChangeKind::Created);
}

#[test]
fn malformed_trigger_identities_are_rejected() {
    assert!(matches!(
        VaultChangeTrigger::new(
            OperationId::new(),
            VaultChangeKind::Deleted,
            "  ",
            "resource"
        ),
        Err(ApplicationError::Validation { .. })
    ));
    assert!(matches!(
        VaultChangeTrigger::new(
            OperationId::new(),
            VaultChangeKind::Deleted,
            "markdown-vault",
            ""
        ),
        Err(ApplicationError::Validation { .. })
    ));
}
