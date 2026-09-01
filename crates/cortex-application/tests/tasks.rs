mod support;

use cortex_application::{TaskCreateInput, TaskRepository};
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
