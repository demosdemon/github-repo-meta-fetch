use rusqlite::Connection;
use rusqlite::OptionalExtension;

use crate::model::Comment;
use crate::model::CrossRef;
use crate::model::CrossRefEvent;
use crate::model::Issue;
use crate::model::IssueState;
use crate::store::Executor;
use crate::store::from_ts;
use crate::store::ts;

/// Insert or replace an issue (keyed by `node_id`). Idempotent.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if the insert fails or if JSON serialisation
/// of the `labels` or `assignees` vectors fails.
pub fn upsert_issue(conn: &impl Executor, i: &Issue) -> rusqlite::Result<()> {
    let labels_json = serde_json::to_string(&i.labels)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    let assignees_json = serde_json::to_string(&i.assignees)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

    conn.execute(
        "INSERT INTO issues (node_id, number, title, state, state_reason, author, body,
            created_at, updated_at, closed_at, milestone, labels_json, assignees_json, deleted)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
         ON CONFLICT(node_id) DO UPDATE SET
            number=excluded.number, title=excluded.title, state=excluded.state,
            state_reason=excluded.state_reason, author=excluded.author, body=excluded.body,
            created_at=excluded.created_at, updated_at=excluded.updated_at,
            closed_at=excluded.closed_at, milestone=excluded.milestone,
            labels_json=excluded.labels_json, assignees_json=excluded.assignees_json,
            deleted=excluded.deleted",
        rusqlite::params![
            i.node_id,
            i.number,
            i.title,
            i.state.as_str(),
            i.state_reason,
            i.author,
            i.body,
            ts(&i.created_at),
            ts(&i.updated_at),
            i.closed_at.as_ref().map(ts),
            i.milestone,
            labels_json,
            assignees_json,
            i.deleted,
        ],
    )?;
    Ok(())
}

/// Replace all comments for a subject (issue OR pull request) with the given
/// set.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if the delete or any insert fails.
pub fn replace_comments(
    conn: &impl Executor,
    subject_node_id: &str,
    comments: &[Comment],
) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM comments WHERE subject_node_id=?1", [
        subject_node_id,
    ])?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO comments (node_id, subject_node_id, author, created_at, body)
         VALUES (?1,?2,?3,?4,?5)",
    )?;
    for c in comments {
        stmt.execute(rusqlite::params![
            c.node_id,
            c.subject_node_id,
            c.author,
            ts(&c.created_at),
            c.body
        ])?;
    }
    Ok(())
}

/// List all comments for a subject, sorted chronologically then by node id.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if the query or any row conversion fails.
pub fn list_comments(conn: &Connection, subject_node_id: &str) -> rusqlite::Result<Vec<Comment>> {
    let mut stmt = conn.prepare(
        "SELECT node_id, subject_node_id, author, created_at, body FROM comments
         WHERE subject_node_id=?1 ORDER BY created_at ASC, node_id ASC",
    )?;
    let rows = stmt.query_map([subject_node_id], |r| {
        let secs: i64 = r.get(3)?;
        Ok(Comment {
            node_id: r.get(0)?,
            subject_node_id: r.get(1)?,
            author: r.get(2)?,
            created_at: from_ts(secs, 3)?,
            body: r.get(4)?,
        })
    })?;
    rows.collect()
}

/// Look up a single issue by its repository-local number.
///
/// Returns `Ok(None)` when no row with `number=?1` exists.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] on any database or conversion failure.
pub fn get_issue_by_number(conn: &Connection, number: i64) -> rusqlite::Result<Option<Issue>> {
    conn.query_row(
        "SELECT node_id, number, title, state, state_reason, author, body, created_at,
                updated_at, closed_at, milestone, labels_json, assignees_json, deleted
         FROM issues WHERE number=?1",
        [number],
        |r| {
            let state_str: String = r.get(3)?;
            let state = IssueState::parse(&state_str)
                .ok_or(rusqlite::Error::IntegralValueOutOfRange(3, 0))?;

            let created_secs: i64 = r.get(7)?;
            let updated_secs: i64 = r.get(8)?;
            let closed_secs: Option<i64> = r.get(9)?;

            Ok(Issue {
                node_id: r.get(0)?,
                number: r.get(1)?,
                title: r.get(2)?,
                state,
                state_reason: r.get(4)?,
                author: r.get(5)?,
                body: r.get(6)?,
                created_at: from_ts(created_secs, 7)?,
                updated_at: from_ts(updated_secs, 8)?,
                closed_at: closed_secs.map(|s| from_ts(s, 9)).transpose()?,
                milestone: r.get(10)?,
                labels: serde_json::from_str(&r.get::<_, String>(11)?).unwrap_or_default(),
                assignees: serde_json::from_str(&r.get::<_, String>(12)?).unwrap_or_default(),
                deleted: r.get(13)?,
            })
        },
    )
    .optional()
}

/// Record a cross-reference event and recompute `is_active` for the (issue,
/// referenced) pair. A pair is active iff its latest event (by `created_at`) is
/// a linking event.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if any database operation fails.
pub fn upsert_cross_ref(conn: &impl Executor, x: &CrossRef) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO cross_refs
            (issue_node_id, referenced_issue_number, event_type, created_at, is_active)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            x.issue_node_id,
            x.referenced_issue_number,
            x.event_type.as_str(),
            x.created_at.timestamp(),
            x.event_type.is_link()
        ],
    )?;
    // Recompute is_active for the whole pair: active iff the latest event is a
    // link.
    let latest_event: Option<String> = conn
        .query_row(
            "SELECT event_type FROM cross_refs
             WHERE issue_node_id=?1 AND referenced_issue_number=?2
             ORDER BY created_at DESC, event_type DESC LIMIT 1",
            rusqlite::params![x.issue_node_id, x.referenced_issue_number],
            |r| r.get(0),
        )
        .optional()?;
    let active = latest_event
        .as_deref()
        .and_then(CrossRefEvent::parse)
        .is_some_and(|e| e.is_link());
    conn.execute(
        "UPDATE cross_refs SET is_active=?3
         WHERE issue_node_id=?1 AND referenced_issue_number=?2",
        rusqlite::params![x.issue_node_id, x.referenced_issue_number, active],
    )?;
    Ok(())
}

/// Distinct active referenced issue numbers for an issue, ascending.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if the query or any row conversion fails.
pub fn related_numbers(conn: &Connection, issue_node_id: &str) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT referenced_issue_number FROM cross_refs
         WHERE issue_node_id=?1 AND is_active=1 ORDER BY referenced_issue_number ASC",
    )?;
    let rows = stmt.query_map([issue_node_id], |r| r.get(0))?;
    rows.collect()
}

/// Mark every non-deleted issue whose `node_id` is NOT in `seen` as deleted.
/// Returns the number newly marked deleted.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if any database operation fails.
pub fn mark_deleted_except<S: std::hash::BuildHasher>(
    conn: &Connection,
    seen: &std::collections::HashSet<String, S>,
) -> rusqlite::Result<usize> {
    crate::store::mark_deleted_except(conn, "issues", seen)
}

#[cfg(test)]
mod full_tests {
    use chrono::DateTime;
    use chrono::Utc;

    use super::*;
    use crate::model::Issue;
    use crate::model::IssueState;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn iss(node: &str, n: i64) -> Issue {
        Issue {
            node_id: node.into(),
            number: n,
            title: "t".into(),
            state: IssueState::Open,
            state_reason: None,
            author: None,
            body: String::new(),
            created_at: dt("2026-01-01T00:00:00Z"),
            updated_at: dt("2026-01-01T00:00:00Z"),
            closed_at: None,
            milestone: None,
            labels: vec![],
            assignees: vec![],
            deleted: false,
        }
    }

    #[test]
    fn marks_unseen_deleted() {
        let conn = crate::store::open_in_memory().unwrap();
        upsert_issue(&conn, &iss("I1", 1)).unwrap();
        upsert_issue(&conn, &iss("I2", 2)).unwrap();
        let mut seen = std::collections::HashSet::new();
        seen.insert("I1".to_string());
        assert_eq!(mark_deleted_except(&conn, &seen).unwrap(), 1);
        assert!(get_issue_by_number(&conn, 2).unwrap().unwrap().deleted);
        assert!(!get_issue_by_number(&conn, 1).unwrap().unwrap().deleted);
    }

    #[test]
    fn revival_via_upsert() {
        // an issue marked deleted, then re-upserted (seen again) is revived.
        let conn = crate::store::open_in_memory().unwrap();
        upsert_issue(&conn, &iss("I1", 1)).unwrap();
        let seen = std::collections::HashSet::new();
        assert_eq!(mark_deleted_except(&conn, &seen).unwrap(), 1);
        assert!(get_issue_by_number(&conn, 1).unwrap().unwrap().deleted);
        upsert_issue(&conn, &iss("I1", 1)).unwrap(); // deleted=false in the model → revives
        assert!(!get_issue_by_number(&conn, 1).unwrap().unwrap().deleted);
    }

    #[test]
    fn empty_seen_marks_all_nondeleted() {
        let conn = crate::store::open_in_memory().unwrap();
        upsert_issue(&conn, &iss("I1", 1)).unwrap();
        upsert_issue(&conn, &iss("I2", 2)).unwrap();
        let seen = std::collections::HashSet::new();
        assert_eq!(mark_deleted_except(&conn, &seen).unwrap(), 2);
        assert!(get_issue_by_number(&conn, 1).unwrap().unwrap().deleted);
        assert!(get_issue_by_number(&conn, 2).unwrap().unwrap().deleted);
    }
}

#[cfg(test)]
mod xref_tests {
    use chrono::DateTime;
    use chrono::Utc;

    use super::*;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn xref(num: i64, ev: CrossRefEvent, when: &str) -> CrossRef {
        CrossRef {
            issue_node_id: "I_1".into(),
            referenced_issue_number: num,
            event_type: ev,
            created_at: dt(when),
        }
    }

    #[test]
    fn connect_then_disconnect_is_inactive() {
        let conn = crate::store::open_in_memory().unwrap();
        upsert_cross_ref(
            &conn,
            &xref(7, CrossRefEvent::Connected, "2026-01-01T00:00:00Z"),
        )
        .unwrap();
        upsert_cross_ref(
            &conn,
            &xref(7, CrossRefEvent::Disconnected, "2026-02-01T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(related_numbers(&conn, "I_1").unwrap(), Vec::<i64>::new());
    }

    #[test]
    fn disconnect_then_reconnect_is_active() {
        let conn = crate::store::open_in_memory().unwrap();
        upsert_cross_ref(
            &conn,
            &xref(7, CrossRefEvent::Connected, "2026-01-01T00:00:00Z"),
        )
        .unwrap();
        upsert_cross_ref(
            &conn,
            &xref(7, CrossRefEvent::Disconnected, "2026-02-01T00:00:00Z"),
        )
        .unwrap();
        upsert_cross_ref(
            &conn,
            &xref(7, CrossRefEvent::Connected, "2026-03-01T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(related_numbers(&conn, "I_1").unwrap(), vec![7]);
    }

    #[test]
    fn related_is_distinct_and_sorted() {
        let conn = crate::store::open_in_memory().unwrap();
        upsert_cross_ref(
            &conn,
            &xref(9, CrossRefEvent::CrossReferenced, "2026-01-01T00:00:00Z"),
        )
        .unwrap();
        upsert_cross_ref(
            &conn,
            &xref(3, CrossRefEvent::MarkedAsDuplicate, "2026-01-01T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(related_numbers(&conn, "I_1").unwrap(), vec![3, 9]);
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use chrono::Utc;

    use super::*;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn sample_issue() -> Issue {
        Issue {
            node_id: "I_1".into(),
            number: 42,
            title: "Bug".into(),
            state: IssueState::Open,
            state_reason: None,
            author: Some("octocat".into()),
            body: "desc".into(),
            created_at: dt("2026-01-05T00:00:00Z"),
            updated_at: dt("2026-06-10T00:00:00Z"),
            closed_at: None,
            milestone: Some("v1.0".into()),
            labels: vec!["bug".into()],
            assignees: vec!["octocat".into()],
            deleted: false,
        }
    }

    #[test]
    fn upsert_is_idempotent_and_readable() {
        let conn = crate::store::open_in_memory().unwrap();
        upsert_issue(&conn, &sample_issue()).unwrap();
        upsert_issue(&conn, &sample_issue()).unwrap();
        let got = get_issue_by_number(&conn, 42).unwrap().unwrap();
        assert_eq!(got, sample_issue());
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM issues", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);

        // Closed issue with a state_reason and closed_at round-trips too.
        let closed = Issue {
            state: IssueState::Closed,
            state_reason: Some("completed".into()),
            closed_at: Some(dt("2026-06-11T00:00:00Z")),
            ..sample_issue()
        };
        upsert_issue(&conn, &closed).unwrap();
        let got_closed = get_issue_by_number(&conn, 42).unwrap().unwrap();
        assert_eq!(got_closed, closed);
    }

    #[test]
    fn comments_replace_and_sort() {
        let conn = crate::store::open_in_memory().unwrap();
        upsert_issue(&conn, &sample_issue()).unwrap();
        let c2 = Comment {
            node_id: "C2".into(),
            subject_node_id: "I_1".into(),
            author: Some("a".into()),
            created_at: dt("2026-02-02T00:00:00Z"),
            body: "two".into(),
        };
        let c1 = Comment {
            node_id: "C1".into(),
            subject_node_id: "I_1".into(),
            author: Some("b".into()),
            created_at: dt("2026-01-01T00:00:00Z"),
            body: "one".into(),
        };
        replace_comments(&conn, "I_1", &[c2.clone(), c1.clone()]).unwrap();
        let got = list_comments(&conn, "I_1").unwrap();
        assert_eq!(got, vec![c1, c2]); // chronological
        replace_comments(&conn, "I_1", &[]).unwrap();
        assert_eq!(list_comments(&conn, "I_1").unwrap().len(), 0);
    }
}
