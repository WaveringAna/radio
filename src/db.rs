use std::str::FromStr;

use anyhow::Context;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

const MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Shared sqlite database handle for the application.
#[derive(Clone)]
pub(crate) struct Database {
    pool: SqlitePool,
}

impl Database {
    /// Connects to sqlite using the configured database URL.
    ///
    /// # Errors
    /// Returns an error when the connection string is invalid or sqlite cannot be opened.
    pub(crate) async fn connect(database_url: &str) -> anyhow::Result<Self> {
        let mut options = SqliteConnectOptions::from_str(database_url)
            .with_context(|| format!("parsing DATABASE_URL {database_url}"))?
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            // Wait for the write lock instead of failing fast with SQLITE_BUSY.
            // Lets a second process (e.g. the bulk importer) write concurrently
            // with the running server under WAL without spurious lock errors.
            .busy_timeout(std::time::Duration::from_secs(15));

        if !database_url.contains(":memory:") {
            options = options.create_if_missing(true);
        }

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .with_context(|| format!("connecting to sqlite database {database_url}"))?;

        Ok(Self { pool })
    }

    /// Runs migrations and basic startup cleanup.
    ///
    /// # Errors
    /// Returns an error when migrations or cleanup queries fail.
    pub(crate) async fn prepare(&self) -> anyhow::Result<()> {
        // Migration 12 is deliberately absent (see 0013's comment): databases
        // that ran the old, now-reverted version 12 still have that entry in
        // their history, and sqlx errors on an applied-but-unresolvable
        // version unless told to ignore the gap.
        let mut migrator = MIGRATOR;
        migrator.set_ignore_missing(true);
        migrator
            .run(&self.pool)
            .await
            .context("running sqlite migrations")?;
        Ok(())
    }

    /// Returns the shared sqlx pool.
    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}
