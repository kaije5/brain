use std::{path::Path, time::Duration};

use cortex_application::ApplicationError;
use sqlx::{
    SqlitePool,
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use crate::{OperationStore, SqliteAuditPort, SqliteModelRoutingStore, SqliteRepositories};

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// The `SQLite` connection pool owned by the Cortex daemon.
#[derive(Clone)]
pub struct SqliteDatabase {
    pool: SqlitePool,
}

impl SqliteDatabase {
    /// Opens a local database, enables safety pragmas, and applies all ordered migrations.
    ///
    /// # Errors
    ///
    /// Returns a redacted storage error when opening the database or applying a migration fails.
    pub async fn connect_and_migrate(path: impl AsRef<Path>) -> Result<Self, ApplicationError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5))
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .map_err(|_| storage_error("sqlite open failed"))?;
        MIGRATOR
            .run(&pool)
            .await
            .map_err(|_| storage_error("sqlite migration failed"))?;
        Ok(Self { pool })
    }

    #[must_use]
    pub fn repositories(&self) -> SqliteRepositories {
        SqliteRepositories::new(self.pool.clone())
    }

    #[must_use]
    pub fn audit_port(&self) -> SqliteAuditPort {
        SqliteAuditPort::new(self.pool.clone())
    }

    #[must_use]
    pub fn operation_store(&self) -> OperationStore {
        OperationStore::new(self.pool.clone())
    }

    #[must_use]
    pub fn model_routing_store(&self) -> SqliteModelRoutingStore {
        SqliteModelRoutingStore::new(self.pool.clone())
    }

    #[cfg(test)]
    pub(crate) fn test_pool(&self) -> &SqlitePool {
        &self.pool
    }
}

pub(crate) fn storage_error(category: &'static str) -> ApplicationError {
    ApplicationError::Storage(category.to_owned())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tempfile::TempDir;

    use super::SqliteDatabase;

    #[tokio::test]
    async fn migration_enables_wal_foreign_keys_and_all_initial_tables() -> Result<(), String> {
        let temp = TempDir::new().map_err(|error| format!("temp directory failed: {error}"))?;
        let database = SqliteDatabase::connect_and_migrate(temp.path().join("cortex.db"))
            .await
            .map_err(|error| format!("migration failed: {error:?}"))?;
        let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&database.pool)
            .await
            .map_err(|error| format!("journal query failed: {error}"))?;
        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&database.pool)
            .await
            .map_err(|error| format!("foreign key query failed: {error}"))?;
        let actual: BTreeSet<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_schema WHERE type = 'table'")
                .fetch_all(&database.pool)
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
            "search_document",
            "search_document_fts",
            "embedding",
            "operation",
            "audit_event",
            "remote_enrollment",
        ];

        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
        assert_eq!(foreign_keys, 1);
        assert!(expected.iter().all(|table| actual.contains(*table)));
        Ok(())
    }

    #[tokio::test]
    async fn migration_foreign_keys_reject_orphans() -> Result<(), String> {
        let temp = TempDir::new().map_err(|error| format!("temp directory failed: {error}"))?;
        let database = SqliteDatabase::connect_and_migrate(temp.path().join("cortex.db"))
            .await
            .map_err(|error| format!("migration failed: {error:?}"))?;
        let orphan = sqlx::query(
            "INSERT INTO memory_source (workspace_id, memory_id, source_id) VALUES (?, ?, ?)",
        )
        .bind("01900000-0000-7000-8000-000000000001")
        .bind("01900000-0000-7000-8000-000000000002")
        .bind("01900000-0000-7000-8000-000000000003")
        .execute(&database.pool)
        .await;

        assert!(orphan.is_err());
        Ok(())
    }
}
