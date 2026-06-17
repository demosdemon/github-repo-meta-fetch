pub mod issues;
pub mod prs;
pub mod repo_meta;
pub mod sync_state;
pub mod taxonomy;

use chrono::DateTime;
use chrono::Utc;
use rusqlite::Connection;
use rusqlite_migration::M;
use rusqlite_migration::Migrations;

/// Unix-seconds for a UTC timestamp.
pub(crate) fn ts(dt: &DateTime<Utc>) -> i64 {
    dt.timestamp()
}

/// Reconstruct a UTC timestamp from Unix-seconds, erroring on out-of-range.
/// `idx` is the column index reported in the resulting error.
pub(crate) fn from_ts(secs: i64, idx: usize) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::from_timestamp(secs, 0).ok_or(rusqlite::Error::IntegralValueOutOfRange(idx, secs))
}

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(include_str!("migrations/0001_init.sql")),
        M::up(include_str!("migrations/0002_pull_requests.sql")),
    ])
}

/// Open (and migrate) a repo data DB.
///
/// # Errors
///
/// Returns an error if the directory cannot be created, the database cannot be
/// opened, or migrations fail.
pub fn open(path: &std::path::Path) -> anyhow::Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    migrations().to_latest(&mut conn)?;
    Ok(conn)
}

/// Open an in-memory migrated DB (for tests).
///
/// # Errors
///
/// Returns an error if the database cannot be opened or migrations fail.
pub fn open_in_memory() -> anyhow::Result<Connection> {
    let mut conn = Connection::open_in_memory()?;
    migrations().to_latest(&mut conn)?;
    Ok(conn)
}

/// SQL execution capability shared by `Connection` and `Transaction`.
///
/// Intentionally does NOT expose `transaction()` / `unchecked_transaction()`,
/// so any function that accepts `&impl Executor` is statically incapable of
/// opening a (possibly nested) transaction.
///
/// Not dyn-compatible (the `impl Params` arguments desugar to method-level
/// generics); always take `&impl Executor`, never `Box<dyn Executor>`.
pub trait Executor {
    fn execute(&self, sql: &str, params: impl rusqlite::Params) -> rusqlite::Result<usize>;
    fn prepare_cached(&self, sql: &str) -> rusqlite::Result<rusqlite::CachedStatement<'_>>;
    fn query_row<T, F>(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
        f: F,
    ) -> rusqlite::Result<T>
    where
        F: FnOnce(&rusqlite::Row<'_>) -> rusqlite::Result<T>;
}

impl Executor for Connection {
    fn execute(&self, sql: &str, params: impl rusqlite::Params) -> rusqlite::Result<usize> {
        Connection::execute(self, sql, params)
    }
    fn prepare_cached(&self, sql: &str) -> rusqlite::Result<rusqlite::CachedStatement<'_>> {
        Connection::prepare_cached(self, sql)
    }
    fn query_row<T, F>(&self, sql: &str, params: impl rusqlite::Params, f: F) -> rusqlite::Result<T>
    where
        F: FnOnce(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    {
        Connection::query_row(self, sql, params, f)
    }
}

impl Executor for rusqlite::Transaction<'_> {
    fn execute(&self, sql: &str, params: impl rusqlite::Params) -> rusqlite::Result<usize> {
        (**self).execute(sql, params)
    }
    fn prepare_cached(&self, sql: &str) -> rusqlite::Result<rusqlite::CachedStatement<'_>> {
        (**self).prepare_cached(sql)
    }
    fn query_row<T, F>(&self, sql: &str, params: impl rusqlite::Params, f: F) -> rusqlite::Result<T>
    where
        F: FnOnce(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    {
        (**self).query_row(sql, params, f)
    }
}

/// Mark every non-deleted row in `table` whose `node_id` is not in `seen` as
/// deleted. Returns the count newly marked. `table` MUST be a trusted literal
/// (it is interpolated into SQL); callers pass `"issues"` or `"pull_requests"`.
pub(crate) fn mark_deleted_except<S: std::hash::BuildHasher>(
    conn: &Connection,
    table: &'static str,
    seen: &std::collections::HashSet<String, S>,
) -> rusqlite::Result<usize> {
    let seen_json = serde_json::to_string(&seen.iter().collect::<Vec<_>>())
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    let sql = format!(
        "UPDATE {table} SET deleted = 1
         WHERE deleted = 0 AND node_id NOT IN (SELECT value FROM json_each(?1))"
    );
    conn.execute(&sql, rusqlite::params![seen_json])
}

#[cfg(test)]
mod executor_tests {
    use super::*;

    #[test]
    fn executor_works_for_connection_and_transaction() {
        let mut conn = open_in_memory().unwrap();
        // Connection impl.
        Executor::execute(
            &conn,
            "INSERT INTO repo_meta (id, owner, repo) VALUES (1, 'o', 'r')",
            [],
        )
        .unwrap();
        // Transaction impl.
        let tx = conn.transaction().unwrap();
        let owner: String =
            Executor::query_row(&tx, "SELECT owner FROM repo_meta WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(owner, "o");
        let mut stmt =
            Executor::prepare_cached(&tx, "SELECT repo FROM repo_meta WHERE id=1").unwrap();
        let repo: String = stmt.query_row([], |r| r.get(0)).unwrap();
        assert_eq!(repo, "r");
        drop(stmt);
        tx.commit().unwrap();
    }
}
