mod support;

use cortex_application::{TaskCreateInput, TaskRepository, TaskUpdateInput};
use cortex_domain::{Lifecycle, TaskStatus};

use support::{Fixture, debug_error};

#[tokio::test]
async fn task_create_complete_delete_and_restore_preserve_state_transitions() -> Result<(), String>
{
    let fixture = Fixture::all_mutations();
    let created = fixture
        .service
        .create_task(
            fixture.context(),
            TaskCreateInput {
                title: "Ship Cortex".to_owned(),
                due_at: None,
            },
        )
        .await
        .map_err(debug_error)?;
    let completed = fixture
        .service
        .complete_task(fixture.context(), created.entity_id, created.revision)
        .await
        .map_err(debug_error)?;
    let task = TaskRepository::find(&fixture.state, fixture.workspace_id, completed.entity_id)
        .await
        .map_err(debug_error)?
        .ok_or("missing completed task")?;
    assert_eq!(task.status(), TaskStatus::Completed);

    let deleted = fixture
        .service
        .delete_task(fixture.context(), completed.entity_id, completed.revision)
        .await
        .map_err(debug_error)?;
    assert_eq!(deleted.lifecycle, Lifecycle::Deleted);
    assert_eq!(
        TaskRepository::find(&fixture.state, fixture.workspace_id, deleted.entity_id)
            .await
            .map_err(debug_error)?,
        None
    );

    let restored = fixture
        .service
        .restore_task(fixture.context(), deleted.entity_id, deleted.revision)
        .await
        .map_err(debug_error)?;
    let task = TaskRepository::find(&fixture.state, fixture.workspace_id, restored.entity_id)
        .await
        .map_err(debug_error)?
        .ok_or("missing restored task")?;
    assert_eq!(task.status(), TaskStatus::Completed);
    assert_eq!(task.lifecycle(), Lifecycle::Active);
    assert_eq!(fixture.state.audits()?.len(), 4);
    Ok(())
}

#[tokio::test]
async fn task_update_preserves_open_state_and_advances_revision() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let created = fixture
        .service
        .create_task(
            fixture.context(),
            TaskCreateInput {
                title: "Draft".to_owned(),
                due_at: None,
            },
        )
        .await
        .map_err(debug_error)?;
    let updated = fixture
        .service
        .update_task(
            fixture.context(),
            created.entity_id,
            created.revision,
            TaskUpdateInput {
                title: "Published".to_owned(),
                due_at: None,
            },
        )
        .await
        .map_err(debug_error)?;
    let task = TaskRepository::find(&fixture.state, fixture.workspace_id, updated.entity_id)
        .await
        .map_err(debug_error)?
        .ok_or("missing updated task")?;
    assert_eq!(task.title(), "Published");
    assert_eq!(task.status(), TaskStatus::Open);
    assert_eq!(task.revision(), updated.revision);
    Ok(())
}
