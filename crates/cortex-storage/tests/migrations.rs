use cortex_domain::{PrincipalId, WorkspaceId};
use cortex_storage::SqliteDatabase;
use sqlx::{SqlitePool, migrate::Migrator};
use std::borrow::Cow;
use tempfile::TempDir;

#[tokio::test]
async fn migration_six_handles_existing_knowledge_grants() -> Result<(), String> {
    let temp = TempDir::new().map_err(|error| error.to_string())?;
    let path = temp.path().join("cortex.db");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let pool = SqlitePool::connect(&url)
        .await
        .map_err(|error| error.to_string())?;
    let through_five = Migrator {
        migrations: Cow::Owned(
            sqlx::migrate!("./migrations")
                .migrations
                .iter()
                .take(5)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    };
    through_five
        .run(&pool)
        .await
        .map_err(|error| error.to_string())?;
    sqlx::query("INSERT INTO workspace(id, name) VALUES ('w', 'owner')")
        .execute(&pool)
        .await
        .map_err(|error| error.to_string())?;
    sqlx::query("INSERT INTO principal(id, workspace_id, name) VALUES ('p', 'w', 'owner')")
        .execute(&pool)
        .await
        .map_err(|error| error.to_string())?;
    for capability in [
        "cortex_note_create",
        "cortex_knowledge_create",
        "cortex_note_delete",
        "cortex_note_restore",
    ] {
        sqlx::query("INSERT INTO capability_grant(workspace_id, principal_id, capability) VALUES ('w', 'p', ?)")
            .bind(capability).execute(&pool).await.map_err(|error| error.to_string())?;
    }
    pool.close().await;

    let _database = SqliteDatabase::connect_and_migrate(&path)
        .await
        .map_err(|error| format!("upgrade failed: {error:?}"))?;
    let pool = SqlitePool::connect(&url)
        .await
        .map_err(|error| error.to_string())?;
    let capabilities: Vec<String> =
        sqlx::query_scalar("SELECT capability FROM capability_grant ORDER BY capability")
            .fetch_all(&pool)
            .await
            .map_err(|error| error.to_string())?;
    assert_eq!(
        capabilities,
        ["cortex_knowledge_create", "cortex_knowledge_delete"]
    );
    Ok(())
}

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
