use cortex_domain::{PrincipalId, WorkspaceId};
use cortex_storage::SqliteDatabase;
use tempfile::TempDir;

#[tokio::test]
async fn fresh_database_exposes_typed_storage_adapters() -> Result<(), String> {
    let temp = TempDir::new().map_err(|error| format!("temp directory failed: {error}"))?;
    let path = temp.path().join("cortex.db");
    let database = SqliteDatabase::connect_and_migrate(&path)
        .await
        .map_err(|error| format!("migration failed: {error:?}"))?;
    let workspace_id = WorkspaceId::new();
    let principal_id = PrincipalId::new();
    let repositories = database.repositories();
    repositories
        .create_workspace(workspace_id, "owner")
        .await
        .map_err(|error| format!("workspace failed: {error:?}"))?;
    repositories
        .create_principal(workspace_id, principal_id, "owner")
        .await
        .map_err(|error| format!("principal failed: {error:?}"))?;
    let _operations = database.operation_store();
    let _audit = database.audit_port();
    Ok(())
}

#[tokio::test]
async fn migration_is_repeatable_through_the_typed_database_api() -> Result<(), String> {
    let temp = TempDir::new().map_err(|error| format!("temp directory failed: {error}"))?;
    let path = temp.path().join("cortex.db");
    let first = SqliteDatabase::connect_and_migrate(&path)
        .await
        .map_err(|error| format!("first migration failed: {error:?}"))?;
    drop(first);
    let second = SqliteDatabase::connect_and_migrate(&path)
        .await
        .map_err(|error| format!("repeat migration failed: {error:?}"))?;

    let _repositories = second.repositories();
    Ok(())
}
