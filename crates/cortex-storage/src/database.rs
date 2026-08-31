use std::path::Path;

use cortex_application::ApplicationError;
use sqlx::{
    SqlitePool,
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use crate::{OperationStore, SqliteAuditPort, SqliteRepositories};

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
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
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
}

pub(crate) fn storage_error(category: &'static str) -> ApplicationError {
    ApplicationError::Storage(category.to_owned())
}
