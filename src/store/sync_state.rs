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
    /// When this phase last completed a walk of its entire history, fresh or
    /// resumed. `None` until one completes.
    pub last_full_sync_at: Option<DateTime<Utc>>,
    /// When this phase last reconciled deletions, which requires a full walk
    /// that both started fresh and ran to completion. `None` until one does.
    pub last_reconciled_at: Option<DateTime<Utc>>,
}

/// Read the sync state for `entity_type`.  Returns a default [`RunPhase::Idle`]
/// state when no row exists.
///
/// # Errors
///
/// Returns any [`rusqlite::Error`] other than `QueryReturnedNoRows`.
pub fn get(conn: &Connection, entity_type: &str) -> rusqlite::Result<SyncState> {
    conn.query_row(
        "SELECT updated_watermark, resume_cursor, run_phase, last_full_sync_at, \
         last_reconciled_at FROM sync_state WHERE entity_type=?1",
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
                last_full_sync_at: r
                    .get::<_, Option<i64>>(3)?
                    .and_then(|s| DateTime::from_timestamp(s, 0)),
                last_reconciled_at: r
                    .get::<_, Option<i64>>(4)?
                    .and_then(|s| DateTime::from_timestamp(s, 0)),
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
                last_full_sync_at: None,
                last_reconciled_at: None,
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
/// `full_sync_at` stamps `last_full_sync_at` when this pass walked the entire
/// history; pass `None` for an incremental pass. The `COALESCE` is what keeps
/// an incremental pass from clearing a marker an earlier full pass set.
///
/// # Errors
///
/// Propagates any [`rusqlite::Error`] from the underlying execute call.
pub fn complete(
    conn: &Connection,
    entity_type: &str,
    watermark: Option<DateTime<Utc>>,
    full_sync_at: Option<DateTime<Utc>>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO sync_state \
            (entity_type, updated_watermark, resume_cursor, run_phase, last_full_sync_at) \
         VALUES (?1, ?2, NULL, 'done', ?3) \
         ON CONFLICT(entity_type) DO UPDATE SET \
            updated_watermark=excluded.updated_watermark, \
            resume_cursor=NULL, \
            run_phase='done', \
            last_full_sync_at=COALESCE(excluded.last_full_sync_at, \
                                       sync_state.last_full_sync_at)",
        rusqlite::params![
            entity_type,
            watermark.map(|w| w.timestamp()),
            full_sync_at.map(|f| f.timestamp())
        ],
    )?;
    Ok(())
}

/// Record that this phase reconciled deletions at `at`.
///
/// Written separately from [`complete`], which stamps `last_full_sync_at`
/// inside the phase function, before and independently of the
/// `mark_deleted_except` + `mark_reconciled` pair `run_phase` performs after
/// its retry loop exits. That window is narrow, but a crash inside it leaves
/// rows soft-deleted with this marker unset. Because `last_full_sync_at` was
/// already stamped by then, the *next* run does not see an unstamped phase
/// and so does not implicitly re-walk everything — the divergence only
/// over-reports outstanding reconciliation (`status` and the rendered
/// `README.md` show `reconciled: None` even though the deletes already
/// landed), and clears only when the caller runs an explicit `--full`.
///
/// # Errors
///
/// Propagates any [`rusqlite::Error`] from the underlying execute call.
pub fn mark_reconciled(
    conn: &Connection,
    entity_type: &str,
    at: DateTime<Utc>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sync_state SET last_reconciled_at=?2 WHERE entity_type=?1",
        rusqlite::params![entity_type, at.timestamp()],
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

        complete(&conn, "issues", Some(dt("2026-06-10T00:00:00Z")), None).unwrap();
        let s = get(&conn, "issues").unwrap();
        assert_eq!(s.run_phase, RunPhase::Done);
        assert_eq!(s.resume_cursor, None);
        assert_eq!(s.updated_watermark, Some(dt("2026-06-10T00:00:00Z")));
    }

    #[test]
    fn full_pass_stamps_and_incremental_pass_preserves() {
        let conn = crate::store::open_in_memory().unwrap();
        let at = dt("2026-07-20T09:31:52Z");

        // A full pass stamps exactly the instant handed in. Asserting the
        // exact value also pins complete()'s parameter order: a transposition
        // with `watermark` would show up here immediately.
        complete(&conn, "issues", Some(dt("2026-06-10T00:00:00Z")), Some(at)).unwrap();
        let s = get(&conn, "issues").unwrap();
        assert_eq!(s.last_full_sync_at, Some(at));
        assert_eq!(s.updated_watermark, Some(dt("2026-06-10T00:00:00Z")));

        // A later incremental pass advances the watermark but must not clear
        // the marker -- this is the COALESCE path.
        complete(&conn, "issues", Some(dt("2026-06-11T00:00:00Z")), None).unwrap();
        let s = get(&conn, "issues").unwrap();
        assert_eq!(s.last_full_sync_at, Some(at));
        assert_eq!(s.updated_watermark, Some(dt("2026-06-11T00:00:00Z")));
    }

    #[test]
    fn fresh_state_has_no_markers() {
        let conn = crate::store::open_in_memory().unwrap();
        let s = get(&conn, "issues").unwrap();
        assert_eq!(s.last_full_sync_at, None);
        assert_eq!(s.last_reconciled_at, None);
    }

    #[test]
    fn mark_reconciled_touches_one_phase_only() {
        let conn = crate::store::open_in_memory().unwrap();
        let at = dt("2026-07-20T09:31:52Z");
        complete(&conn, "issues", None, Some(at)).unwrap();
        complete(&conn, "pull_requests", None, Some(at)).unwrap();

        mark_reconciled(&conn, "issues", at).unwrap();

        let i = get(&conn, "issues").unwrap();
        let p = get(&conn, "pull_requests").unwrap();
        assert_eq!(i.last_reconciled_at, Some(at));
        assert_eq!(p.last_reconciled_at, None);
        // Other columns survive.
        assert_eq!(i.last_full_sync_at, Some(at));
        assert_eq!(i.run_phase, RunPhase::Done);
    }
}
