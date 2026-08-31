use std::collections::BTreeSet;

use cortex_storage::SqliteDatabase;
use tempfile::TempDir;

#[tokio::test]
async fn fresh_database_enables_wal_foreign_keys_and_all_initial_tables() -> Result<(), String> {
    let temp = TempDir::new().map_err(|error| format!("temp directory failed: {error}"))?;
    let path = temp.path().join("cortex.db");
    let database = SqliteDatabase::connect_and_migrate(&path)
        .await
        .map_err(|error| format!("migration failed: {error:?}"))?;

    let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(database.pool())
        .await
        .map_err(|error| format!("journal query failed: {error}"))?;
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(database.pool())
        .await
        .map_err(|error| format!("foreign key query failed: {error}"))?;
    let actual: BTreeSet<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_schema WHERE type = 'table'")
            .fetch_all(database.pool())
            .await
            .map_err(|error| format!("schema query failed: {error}"))?
            .into_iter()
            .collect();
    let expected = [
        "workspace",
        "principal",
        "capability_grant",
        "note",
        "task",
        "source",
        "memory_assertion",
        "memory_source",
        "embedding",
        "operation",
        "audit_event",
    ];

    assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
    assert_eq!(foreign_keys, 1);
    assert!(expected.iter().all(|table| actual.contains(*table)));
    Ok(())
}

#[tokio::test]
async fn migration_is_repeatable_and_foreign_keys_reject_orphans() -> Result<(), String> {
    let temp = TempDir::new().map_err(|error| format!("temp directory failed: {error}"))?;
    let path = temp.path().join("cortex.db");
    let first = SqliteDatabase::connect_and_migrate(&path)
        .await
        .map_err(|error| format!("first migration failed: {error:?}"))?;
    drop(first);
    let second = SqliteDatabase::connect_and_migrate(&path)
        .await
        .map_err(|error| format!("repeat migration failed: {error:?}"))?;

    let orphan = sqlx::query(
        "INSERT INTO memory_source (workspace_id, memory_id, source_id) VALUES (?, ?, ?)",
    )
    .bind("01900000-0000-7000-8000-000000000001")
    .bind("01900000-0000-7000-8000-000000000002")
    .bind("01900000-0000-7000-8000-000000000003")
    .execute(second.pool())
    .await;

    assert!(orphan.is_err());
    Ok(())
}
