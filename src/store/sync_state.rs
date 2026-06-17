use chrono::DateTime;
use chrono::Utc;
use rusqlite::Connection;

/// The phase of an entity sync run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPhase {
    Idle,
    Paginating,
    Done,
}

impl RunPhase {
    /// Return the canonical string representation of the phase.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RunPhase::Idle => "idle",
            RunPhase::Paginating => "paginating",
            RunPhase::Done => "done",
        }
    }

    /// Parse a string into a `RunPhase`, defaulting to [`RunPhase::Idle`] for
    /// unrecognised values.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "paginating" => RunPhase::Paginating,
            "done" => RunPhase::Done,
            _ => RunPhase::Idle,
        }
    }
}

/// Persisted synchronisation checkpoint for one entity type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncState {
    /// The entity type key (e.g. `"issues"`).
    pub entity_type: String,
    /// The timestamp of the most recently successfully synced item.  `None`
    /// when no run has completed yet.
    pub updated_watermark: Option<DateTime<Utc>>,
    /// The GraphQL `endCursor` to resume from, if a paginating run was
    /// interrupted.
    pub resume_cursor: Option<String>,
    /// Current phase of the sync run.
    pub run_phase: RunPhase,
}

/// Read the sync state for `entity_type`.  Returns a default [`RunPhase::Idle`]
/// state when no row exists.
///
/// # Errors
///
/// Returns any [`rusqlite::Error`] other than `QueryReturnedNoRows`.
pub fn get(conn: &Connection, entity_type: &str) -> rusqlite::Result<SyncState> {
    conn.query_row(
        "SELECT updated_watermark, resume_cursor, run_phase \
         FROM sync_state WHERE entity_type=?1",
        [entity_type],
        |r| {
            Ok(SyncState {
                entity_type: entity_type.to_string(),
                // Out-of-range timestamps are mapped to None rather than panicking.
                updated_watermark: r
                    .get::<_, Option<i64>>(0)?
                    .and_then(|s| DateTime::from_timestamp(s, 0)),
                resume_cursor: r.get(1)?,
                run_phase: RunPhase::parse(&r.get::<_, String>(2)?),
            })
        },
    )
    .or_else(|e| {
        if e == rusqlite::Error::QueryReturnedNoRows {
            Ok(SyncState {
                entity_type: entity_type.to_string(),
                updated_watermark: None,
                resume_cursor: None,
                run_phase: RunPhase::Idle,
            })
        } else {
            Err(e)
        }
    })
}

/// Persist the resume cursor and run phase without touching the watermark.
///
/// # Errors
///
/// Propagates any [`rusqlite::Error`] from the underlying execute call.
pub fn set_cursor(
    conn: &Connection,
    entity_type: &str,
    cursor: Option<&str>,
    phase: RunPhase,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO sync_state (entity_type, resume_cursor, run_phase) VALUES (?1, ?2, ?3) \
         ON CONFLICT(entity_type) DO UPDATE SET \
            resume_cursor=excluded.resume_cursor, run_phase=excluded.run_phase",
        rusqlite::params![entity_type, cursor, phase.as_str()],
    )?;
    Ok(())
}

/// Advance the watermark and mark the run done.  Clears the resume cursor.
///
/// # Errors
///
/// Propagates any [`rusqlite::Error`] from the underlying execute call.
pub fn complete(
    conn: &Connection,
    entity_type: &str,
    watermark: Option<DateTime<Utc>>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO sync_state (entity_type, updated_watermark, resume_cursor, run_phase) \
         VALUES (?1, ?2, NULL, 'done') \
         ON CONFLICT(entity_type) DO UPDATE SET \
            updated_watermark=excluded.updated_watermark, resume_cursor=NULL, run_phase='done'",
        rusqlite::params![entity_type, watermark.map(|w| w.timestamp())],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn missing_state_defaults_to_idle() {
        let conn = crate::store::open_in_memory().unwrap();
        let s = get(&conn, "issues").unwrap();
        assert_eq!(s.run_phase, RunPhase::Idle);
        assert_eq!(s.updated_watermark, None);
    }

    #[test]
    fn cursor_then_complete() {
        let conn = crate::store::open_in_memory().unwrap();
        set_cursor(&conn, "issues", Some("CUR1"), RunPhase::Paginating).unwrap();
        let s = get(&conn, "issues").unwrap();
        assert_eq!(s.resume_cursor.as_deref(), Some("CUR1"));
        assert_eq!(s.run_phase, RunPhase::Paginating);

        complete(&conn, "issues", Some(dt("2026-06-10T00:00:00Z"))).unwrap();
        let s = get(&conn, "issues").unwrap();
        assert_eq!(s.run_phase, RunPhase::Done);
        assert_eq!(s.resume_cursor, None);
        assert_eq!(s.updated_watermark, Some(dt("2026-06-10T00:00:00Z")));
    }
}
