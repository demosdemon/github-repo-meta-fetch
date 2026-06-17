use chrono::DateTime;
use rusqlite::Connection;
use rusqlite::OptionalExtension;

use crate::ratelimit::budget::Bucket;
use crate::ratelimit::budget::Resource;

/// Per-token rate-limit state DB. `SQLite` calls run inline on the runtime;
/// each is a fast, local file operation.
pub struct RateLimitStore {
    conn: Connection,
    fingerprint: String,
}

impl RateLimitStore {
    /// Open an in-memory `SQLite` database for testing.
    pub fn open_in_memory(fingerprint: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn, fingerprint)
    }

    /// Open a file-backed `SQLite` database with WAL mode and a 30 s busy
    /// timeout.
    pub fn open(path: &std::path::Path, fingerprint: &str) -> rusqlite::Result<Self> {
        if let Some(dir) = path.parent() {
            // Ignore the error; if the directory can't be created Connection::open will
            // fail.
            drop(std::fs::create_dir_all(dir));
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        Self::init(conn, fingerprint)
    }

    fn init(conn: Connection, fingerprint: &str) -> rusqlite::Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS budgets (
                fingerprint TEXT NOT NULL,
                resource    TEXT NOT NULL,
                limit_      INTEGER NOT NULL,
                remaining   INTEGER NOT NULL,
                used        INTEGER NOT NULL,
                reset_ts    INTEGER NOT NULL,
                PRIMARY KEY (fingerprint, resource)
            );",
        )?;
        Ok(Self {
            conn,
            fingerprint: fingerprint.to_owned(),
        })
    }

    /// Overwrite the cached bucket from an authoritative header observation.
    pub fn record(&self, resource: Resource, b: &Bucket) -> rusqlite::Result<()> {
        // Bucket fields are u64 from GitHub headers; values are well within i64 range.
        // Use try_from to avoid a lossy cast; map errors to IntegralValueOutOfRange.
        let limit_i64 =
            i64::try_from(b.limit).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, 0))?;
        let remaining_i64 = i64::try_from(b.remaining)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(4, 0))?;
        let used_i64 =
            i64::try_from(b.used).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(5, 0))?;

        self.conn.execute(
            "INSERT INTO budgets (fingerprint, resource, limit_, remaining, used, reset_ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(fingerprint, resource) DO UPDATE SET
                limit_=excluded.limit_, remaining=excluded.remaining,
                used=excluded.used, reset_ts=excluded.reset_ts",
            rusqlite::params![
                self.fingerprint,
                resource.as_str(),
                limit_i64,
                remaining_i64,
                used_i64,
                b.reset.timestamp(),
            ],
        )?;
        Ok(())
    }

    /// Fetch the cached bucket for `resource`, or `None` if never observed.
    pub fn get(&self, resource: Resource) -> rusqlite::Result<Option<Bucket>> {
        self.conn
            .query_row(
                "SELECT limit_, remaining, used, reset_ts FROM budgets
                 WHERE fingerprint=?1 AND resource=?2",
                rusqlite::params![self.fingerprint, resource.as_str()],
                |row| {
                    let limit_raw: i64 = row.get(0)?;
                    let remaining_raw: i64 = row.get(1)?;
                    let used_raw: i64 = row.get(2)?;
                    let reset_ts: i64 = row.get(3)?;

                    // Convert signed DB integers back to u64; stored values are always ≥ 0.
                    let limit = u64::try_from(limit_raw).unwrap_or(0);
                    let remaining = u64::try_from(remaining_raw).unwrap_or(0);
                    let used = u64::try_from(used_raw).unwrap_or(0);

                    // Convert Unix timestamp to DateTime<Utc>; error if out of range.
                    let reset = DateTime::from_timestamp(reset_ts, 0)
                        .ok_or(rusqlite::Error::IntegralValueOutOfRange(3, reset_ts))?;

                    Ok(Bucket {
                        limit,
                        remaining,
                        used,
                        reset,
                    })
                },
            )
            .optional()
    }

    /// Atomically check the floor and, if proceeding, decrement `remaining` by
    /// `est_cost`.
    ///
    /// Uses `BEGIN IMMEDIATE` so concurrent processes sharing one `SQLite` file
    /// serialize at the write-lock acquisition point (closes the TOCTOU
    /// window).
    ///
    /// Returns `Ok(true)` if the caller may proceed, `Ok(false)` if it must
    /// pause.
    pub fn try_reserve(
        &mut self,
        resource: Resource,
        floor: u64,
        est_cost: u64,
    ) -> rusqlite::Result<bool> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        let remaining_raw: Option<i64> = tx
            .query_row(
                "SELECT remaining FROM budgets WHERE fingerprint=?1 AND resource=?2",
                rusqlite::params![self.fingerprint, resource.as_str()],
                |r| r.get(0),
            )
            .optional()?;

        let remaining = match remaining_raw {
            // No row yet — treat as unlimited, allow the call.
            None => {
                tx.commit()?;
                return Ok(true);
            }
            Some(r) => u64::try_from(r).unwrap_or(0),
        };

        if remaining.saturating_sub(est_cost) < floor {
            tx.commit()?;
            return Ok(false);
        }

        // Map est_cost to i64 to avoid a lossy cast; GitHub values fit comfortably.
        let cost_i64 =
            i64::try_from(est_cost).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?;

        tx.execute(
            "UPDATE budgets SET remaining = remaining - ?3
             WHERE fingerprint=?1 AND resource=?2",
            rusqlite::params![self.fingerprint, resource.as_str(), cost_i64],
        )?;
        tx.commit()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;

    fn bucket(remaining: u64) -> Bucket {
        Bucket {
            limit: 5000,
            remaining,
            used: 5000 - remaining,
            reset: chrono::Utc
                .timestamp_opt(1_781_564_821, 0)
                .single()
                .unwrap(),
        }
    }

    #[test]
    fn record_then_get_round_trips() {
        let s = RateLimitStore::open_in_memory("fp").unwrap();
        s.record(Resource::GraphQL, &bucket(4000)).unwrap();
        assert_eq!(s.get(Resource::GraphQL).unwrap().unwrap().remaining, 4000);
    }

    #[test]
    fn try_reserve_decrements_when_above_floor() {
        let mut s = RateLimitStore::open_in_memory("fp").unwrap();
        s.record(Resource::GraphQL, &bucket(4000)).unwrap();
        assert!(s.try_reserve(Resource::GraphQL, 500, 30).unwrap());
        assert_eq!(s.get(Resource::GraphQL).unwrap().unwrap().remaining, 3970);
    }

    #[test]
    fn try_reserve_refuses_below_floor_without_decrement() {
        let mut s = RateLimitStore::open_in_memory("fp").unwrap();
        s.record(Resource::GraphQL, &bucket(520)).unwrap();
        assert!(!s.try_reserve(Resource::GraphQL, 500, 30).unwrap());
        assert_eq!(s.get(Resource::GraphQL).unwrap().unwrap().remaining, 520);
    }

    #[test]
    fn unknown_bucket_allows() {
        let mut s = RateLimitStore::open_in_memory("fp").unwrap();
        assert!(s.try_reserve(Resource::Core, 500, 30).unwrap());
    }
}
