//! SCRUM-128/129: deterministic planning consumes provider-backed task inputs
//! and records Cortex-owned work-block linkage keyed by stable task identity.

mod support;

use std::{
    collections::BTreeMap,
    num::NonZeroU32,
    sync::{Arc, Mutex},
};

use chrono::{Duration, TimeZone, Utc};
use cortex_application::{
    PlanningService, PlanningWindow, ProviderTask, ProviderTaskPriority, ProviderTaskStatus,
    TaskProvider, TaskSchedulingMetadata, WorkBlockIntent, WorkBlockLedger, WorkBlockLinkage,
    plan_schedule, work_block_intent,
};
use cortex_domain::{TaskId, WorkspaceId};
use support::task_provider::{FakeTaskProvider, make_task};

#[derive(Default, Clone)]
struct MemoryLedger {
    linkages: Arc<Mutex<BTreeMap<TaskId, WorkBlockLinkage>>>,
}

impl WorkBlockLedger for MemoryLedger {
    async fn record(
        &self,
        linkage: WorkBlockLinkage,
    ) -> Result<(), cortex_application::ApplicationError> {
        self.linkages
            .lock()
            .expect("ledger mutex")
            .insert(linkage.task_id(), linkage);
        Ok(())
    }

    async fn find(
        &self,
        task_id: TaskId,
    ) -> Result<Option<WorkBlockLinkage>, cortex_application::ApplicationError> {
        Ok(self
            .linkages
            .lock()
            .expect("ledger mutex")
            .get(&task_id)
            .cloned())
    }

    async fn clear(
        &self,
        _workspace_id: WorkspaceId,
    ) -> Result<(), cortex_application::ApplicationError> {
        self.linkages.lock().expect("ledger mutex").clear();
        Ok(())
    }
}

fn scheduling(
    deadline_at: Option<chrono::DateTime<Utc>>,
    duration_minutes: Option<NonZeroU32>,
) -> TaskSchedulingMetadata {
    TaskSchedulingMetadata::new(
        None,
        deadline_at,
        duration_minutes,
        None,
        None,
        None::<&str>,
        None::<&str>,
    )
    .expect("scheduling")
}

fn window() -> PlanningWindow {
    PlanningWindow::new(
        Utc.with_ymd_and_hms(2026, 9, 16, 9, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2026, 9, 16, 17, 0, 0).unwrap(),
    )
}

fn scheduling_rank_of(block: &cortex_application::PlannedWorkBlock) -> (i64, TaskId) {
    (block.starts_at().timestamp_millis(), block.task_id())
}

#[tokio::test]
async fn plans_only_open_tasks_in_deadline_priority_order() {
    let workspace_id = WorkspaceId::new();
    let provider = FakeTaskProvider::new();
    let urgent = TaskId::new();
    let later = TaskId::new();
    let done = TaskId::new();
    provider.insert(make_task(
        "tasks/urgent.md",
        urgent,
        1,
        workspace_id,
        ProviderTaskPriority::Urgent,
        scheduling(None, None),
    ));
    provider.insert(make_task(
        "tasks/later.md",
        later,
        1,
        workspace_id,
        ProviderTaskPriority::Normal,
        scheduling(
            Some(Utc.with_ymd_and_hms(2026, 9, 16, 12, 0, 0).unwrap()),
            None,
        ),
    ));
    let mut completed = make_task(
        "tasks/done.md",
        done,
        1,
        workspace_id,
        ProviderTaskPriority::Urgent,
        scheduling(None, None),
    );
    completed = ProviderTask::new(
        completed.provenance().clone(),
        completed.task_id(),
        completed.title(),
        completed.body(),
        ProviderTaskStatus::Completed,
        completed.priority(),
        scheduling(None, None),
    )
    .expect("completed task");
    provider.insert(completed);

    let blocks = plan_schedule(
        &provider
            .search(
                &cortex_application::TaskQuery::new(
                    workspace_id,
                    Option::<String>::None,
                    std::num::NonZeroUsize::new(100).unwrap(),
                )
                .expect("query"),
            )
            .await
            .expect("search")
            .into_items(),
        window(),
    );

    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].task_id(), later, "earliest deadline first");
    assert_eq!(blocks[1].task_id(), urgent);
    assert_eq!(
        blocks[0].ends_at(),
        blocks[1].starts_at(),
        "non-overlapping"
    );
}

#[tokio::test]
async fn planning_is_deterministic_across_runs() {
    let workspace_id = WorkspaceId::new();
    let provider = FakeTaskProvider::new();
    for index in 0..5 {
        provider.insert(make_task(
            &format!("tasks/t{index}.md"),
            TaskId::new(),
            1,
            workspace_id,
            ProviderTaskPriority::Normal,
            scheduling(None, None),
        ));
    }
    let tasks = provider
        .search(
            &cortex_application::TaskQuery::new(
                workspace_id,
                Option::<String>::None,
                std::num::NonZeroUsize::new(100).unwrap(),
            )
            .expect("query"),
        )
        .await
        .expect("search")
        .into_items();
    let first = plan_schedule(&tasks, window());
    let second = plan_schedule(&tasks, window());
    assert_eq!(first, second);
    let ranked: Vec<_> = first.iter().map(scheduling_rank_of).collect();
    let mut sorted = ranked.clone();
    sorted.sort();
    assert_eq!(ranked, sorted, "blocks are emitted in deterministic order");
}

#[tokio::test]
async fn respects_duration_and_window_bounds() {
    let workspace_id = WorkspaceId::new();
    let provider = FakeTaskProvider::new();
    let long = TaskId::new();
    let overflow = TaskId::new();
    provider.insert(make_task(
        "tasks/long.md",
        long,
        1,
        workspace_id,
        ProviderTaskPriority::Normal,
        scheduling(None, Some(NonZeroU32::new(120).unwrap())),
    ));
    provider.insert(make_task(
        "tasks/overflow.md",
        overflow,
        1,
        workspace_id,
        ProviderTaskPriority::Urgent,
        scheduling(None, Some(NonZeroU32::new(600).unwrap())),
    ));

    let blocks = plan_schedule(
        &provider
            .search(
                &cortex_application::TaskQuery::new(
                    workspace_id,
                    Option::<String>::None,
                    std::num::NonZeroUsize::new(100).unwrap(),
                )
                .expect("query"),
            )
            .await
            .expect("search")
            .into_items(),
        window(),
    );

    assert_eq!(blocks.len(), 1, "block that cannot fit is skipped");
    assert_eq!(
        blocks[0].ends_at() - blocks[0].starts_at(),
        Duration::minutes(120)
    );
}

#[test]
fn intent_follows_priority_and_context() {
    let workspace_id = WorkspaceId::new();
    let urgent = make_task(
        "tasks/urgent.md",
        TaskId::new(),
        1,
        workspace_id,
        ProviderTaskPriority::Urgent,
        scheduling(None, None),
    );
    assert_eq!(work_block_intent(&urgent), WorkBlockIntent::Deep);
    let low = make_task(
        "tasks/low.md",
        TaskId::new(),
        1,
        workspace_id,
        ProviderTaskPriority::Low,
        scheduling(None, None),
    );
    assert_eq!(work_block_intent(&low), WorkBlockIntent::Admin);
    let normal = make_task(
        "tasks/normal.md",
        TaskId::new(),
        1,
        workspace_id,
        ProviderTaskPriority::Normal,
        scheduling(None, None),
    );
    assert_eq!(work_block_intent(&normal), WorkBlockIntent::Review);
}

#[tokio::test]
async fn service_records_linkage_after_successful_plan() {
    let workspace_id = WorkspaceId::new();
    let provider = FakeTaskProvider::new();
    let task_id = TaskId::new();
    provider.insert(make_task(
        "tasks/one.md",
        task_id,
        1,
        workspace_id,
        ProviderTaskPriority::High,
        scheduling(None, None),
    ));
    let service = PlanningService::new(provider, MemoryLedger::default());

    let blocks = service
        .plan_window(workspace_id, window())
        .await
        .expect("plan");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].intent(), WorkBlockIntent::Deep);

    let linkage = service.linkage_for(task_id).await.expect("linkage");
    assert!(linkage.is_some(), "linkage recorded after successful plan");
}

#[tokio::test]
async fn linkage_survives_task_rename_and_move() {
    let workspace_id = WorkspaceId::new();
    let provider = FakeTaskProvider::new();
    let task_id = TaskId::new();
    provider.insert(make_task(
        "tasks/one.md",
        task_id,
        1,
        workspace_id,
        ProviderTaskPriority::High,
        scheduling(None, None),
    ));
    let service = PlanningService::new(provider.clone(), MemoryLedger::default());
    service
        .plan_window(workspace_id, window())
        .await
        .expect("plan");

    provider
        .rename("tasks/one.md", "archive/2026/renamed.md", workspace_id)
        .expect("rename");

    let linkage = service.linkage_for(task_id).await.expect("linkage");
    assert!(
        linkage.is_some(),
        "work-block linkage survives task rename/move through stable identity"
    );
}

#[tokio::test]
async fn failed_provider_reads_do_not_advance_planner_state() {
    let workspace_id = WorkspaceId::new();
    let provider = FakeTaskProvider::new();
    provider.insert(make_task(
        "tasks/one.md",
        TaskId::new(),
        1,
        workspace_id,
        ProviderTaskPriority::High,
        scheduling(None, None),
    ));
    let ledger = MemoryLedger::default();
    let service = PlanningService::new(provider.clone(), ledger.clone());

    provider.set_fail_reads(true);
    let outcome = service.plan_window(workspace_id, window()).await;
    assert!(outcome.is_err(), "provider failure surfaces as typed error");
    assert!(
        ledger.linkages.lock().expect("ledger mutex").is_empty(),
        "planner state must not advance when the provider read fails"
    );
}
