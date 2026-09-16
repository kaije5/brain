mod support;

use cortex_application::{
    ApplicationError, ApplicationService, Capability, CapabilityGrant, CommandContext, GrantPolicy,
    MemoryCorrectInput, MemoryCreateInput, MemoryRepository,
};
use cortex_domain::{Lifecycle, OperationId, PrincipalId};
use uuid::Uuid;

use support::{Fixture, debug_error};

fn new_memory() -> MemoryCreateInput {
    MemoryCreateInput {
        statement: "Original".to_owned(),
        normalized_subject: "subject".to_owned(),
        normalized_predicate: "predicate".to_owned(),
        normalized_object: "object".to_owned(),
        sources: Vec::new(),
    }
}

fn correction() -> MemoryCorrectInput {
    MemoryCorrectInput {
        statement: "Updated".to_owned(),
        normalized_subject: "subject".to_owned(),
        normalized_predicate: "predicate".to_owned(),
        normalized_object: "object".to_owned(),
        sources: Vec::new(),
    }
}

#[tokio::test]
async fn stale_delete_is_rejected_and_leaves_newer_memory_active() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let memory = fixture
        .service
        .create_memory(fixture.context(), new_memory())
        .await
        .map_err(debug_error)?;
    let updated = fixture
        .service
        .correct_memory(
            fixture.context(),
            memory.entity_id,
            memory.revision,
            correction(),
        )
        .await
        .map_err(debug_error)?;
    let result = fixture
        .service
        .delete_memory(fixture.context(), memory.entity_id, memory.revision)
        .await;

    assert!(matches!(
        result,
        Err(ApplicationError::Conflict {
            entity: "memory"
        })
    ));
    let persisted = MemoryRepository::find(&fixture.state, fixture.workspace_id, memory.entity_id)
        .await
        .map_err(debug_error)?
        .ok_or("newer memory missing")?;
    assert_eq!(persisted.revision(), updated.revision);
    assert_eq!(persisted.lifecycle(), Lifecycle::Active);
    assert_eq!(persisted.statement(), "Updated");
    Ok(())
}

#[tokio::test]
async fn repeated_operation_is_returned_before_revision_validation() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let memory = fixture
        .service
        .create_memory(fixture.context(), new_memory())
        .await
        .map_err(debug_error)?;
    let operation_id = OperationId::new();
    let first = fixture
        .service
        .correct_memory(
            fixture.context_for(operation_id),
            memory.entity_id,
            memory.revision,
            correction(),
        )
        .await
        .map_err(debug_error)?;
    let replay = fixture
        .service
        .correct_memory(
            fixture.context_for(operation_id),
            memory.entity_id,
            memory.revision,
            MemoryCorrectInput {
                statement: "Ignored replay".to_owned(),
                ..correction()
            },
        )
        .await
        .map_err(debug_error)?;

    assert_eq!(replay, first);
    let persisted = MemoryRepository::find(&fixture.state, fixture.workspace_id, memory.entity_id)
        .await
        .map_err(debug_error)?
        .ok_or("memory missing")?;
    assert_eq!(persisted.statement(), "Updated");
    Ok(())
}

#[tokio::test]
async fn repeated_operation_still_requires_the_current_capability() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let operation_id = OperationId::new();
    let first = fixture
        .service
        .create_memory(fixture.context_for(operation_id), new_memory())
        .await
        .map_err(debug_error)?;
    let denied = ApplicationService::new(
        GrantPolicy::new([]),
        fixture.state.clone(),
        fixture.state.clone(),
        fixture.state.clone(),
    );

    let replay = denied
        .correct_memory(
            fixture.context_for(operation_id),
            first.entity_id,
            first.revision,
            correction(),
        )
        .await;

    assert!(matches!(replay, Err(ApplicationError::PolicyDenied(_))));
    let persisted = MemoryRepository::find(&fixture.state, fixture.workspace_id, first.entity_id)
        .await
        .map_err(debug_error)?
        .ok_or("memory missing")?;
    assert_eq!(persisted.statement(), "Original");
    Ok(())
}

#[tokio::test]
async fn repeated_operation_rejects_a_different_granted_principal() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let operation_id = OperationId::new();
    let first = fixture
        .service
        .create_memory(fixture.context_for(operation_id), new_memory())
        .await
        .map_err(debug_error)?;
    let other_principal = PrincipalId::new();
    let other_service = ApplicationService::new(
        GrantPolicy::new([CapabilityGrant::new(
            fixture.workspace_id,
            other_principal,
            Capability::MemoryCreate,
        )]),
        fixture.state.clone(),
        fixture.state.clone(),
        fixture.state.clone(),
    );

    let replay = other_service
        .create_memory(
            CommandContext::from_authenticated(
                fixture.workspace_id,
                other_principal,
                operation_id,
                Uuid::now_v7(),
            ),
            MemoryCreateInput {
                statement: "other principal".to_owned(),
                ..new_memory()
            },
        )
        .await;

    assert_eq!(
        replay,
        Err(ApplicationError::Conflict {
            entity: "operation"
        })
    );
    let persisted = MemoryRepository::find(&fixture.state, fixture.workspace_id, first.entity_id)
        .await
        .map_err(debug_error)?;
    assert!(persisted.is_some());
    Ok(())
}

#[tokio::test]
async fn repeated_operation_rejects_a_different_allowed_capability() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let operation_id = OperationId::new();
    let memory = fixture
        .service
        .create_memory(fixture.context_for(operation_id), new_memory())
        .await
        .map_err(debug_error)?;

    let replay = fixture
        .service
        .delete_memory(fixture.context_for(operation_id), memory.entity_id, memory.revision)
        .await;

    assert_eq!(
        replay,
        Err(ApplicationError::Conflict {
            entity: "operation"
        })
    );
    let persisted = MemoryRepository::find(&fixture.state, fixture.workspace_id, memory.entity_id)
        .await
        .map_err(debug_error)?
        .ok_or("memory missing")?;
    assert_eq!(persisted.lifecycle(), Lifecycle::Active);
    Ok(())
}

#[tokio::test]
async fn repeated_operation_rejects_a_different_target() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let first_memory = fixture
        .service
        .create_memory(fixture.context(), new_memory())
        .await
        .map_err(debug_error)?;
    let second_memory = fixture
        .service
        .create_memory(fixture.context(), new_memory())
        .await
        .map_err(debug_error)?;
    let operation_id = OperationId::new();
    fixture
        .service
        .correct_memory(
            fixture.context_for(operation_id),
            first_memory.entity_id,
            first_memory.revision,
            correction(),
        )
        .await
        .map_err(debug_error)?;

    let replay = fixture
        .service
        .correct_memory(
            fixture.context_for(operation_id),
            second_memory.entity_id,
            second_memory.revision,
            MemoryCorrectInput {
                statement: "second target".to_owned(),
                ..correction()
            },
        )
        .await;

    assert_eq!(
        replay,
        Err(ApplicationError::Conflict {
            entity: "operation"
        })
    );
    let second = MemoryRepository::find(&fixture.state, fixture.workspace_id, second_memory.entity_id)
        .await
        .map_err(debug_error)?
        .ok_or("second memory missing")?;
    assert_eq!(second.statement(), "Original");
    Ok(())
}
