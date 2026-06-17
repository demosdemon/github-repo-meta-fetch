use rusqlite::Connection;
use rusqlite::OptionalExtension;

use crate::model::PrState;
use crate::model::PullRequest;
use crate::model::Review;
use crate::model::ReviewComment;
use crate::model::ReviewState;
use crate::model::ReviewThread;
use crate::store::Executor;
use crate::store::from_ts;
use crate::store::ts;

/// Insert or replace a pull request (keyed by `node_id`). Idempotent.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] on insert failure or JSON serialisation
/// failure.
pub fn upsert_pull_request(conn: &impl Executor, p: &PullRequest) -> rusqlite::Result<()> {
    let labels_json = serde_json::to_string(&p.labels)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    let assignees_json = serde_json::to_string(&p.assignees)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    conn.execute(
        "INSERT INTO pull_requests (node_id, number, title, state, is_draft, merged, merged_at,
            merged_by, base_ref, head_ref, additions, deletions, changed_files, author, body,
            created_at, updated_at, closed_at, milestone, labels_json, assignees_json, deleted)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)
         ON CONFLICT(node_id) DO UPDATE SET
            number=excluded.number, title=excluded.title, state=excluded.state,
            is_draft=excluded.is_draft, merged=excluded.merged, merged_at=excluded.merged_at,
            merged_by=excluded.merged_by, base_ref=excluded.base_ref, head_ref=excluded.head_ref,
            additions=excluded.additions, deletions=excluded.deletions,
            changed_files=excluded.changed_files, author=excluded.author, body=excluded.body,
            created_at=excluded.created_at, updated_at=excluded.updated_at,
            closed_at=excluded.closed_at, milestone=excluded.milestone,
            labels_json=excluded.labels_json, assignees_json=excluded.assignees_json,
            deleted=excluded.deleted",
        rusqlite::params![
            p.node_id,
            p.number,
            p.title,
            p.state,
            p.is_draft,
            p.merged,
            p.merged_at.as_ref().map(ts),
            p.merged_by,
            p.base_ref,
            p.head_ref,
            p.additions,
            p.deletions,
            p.changed_files,
            p.author,
            p.body,
            ts(&p.created_at),
            ts(&p.updated_at),
            p.closed_at.as_ref().map(ts),
            p.milestone,
            labels_json,
            assignees_json,
            p.deleted,
        ],
    )?;
    Ok(())
}

/// Look up a single PR by number. Returns `Ok(None)` when absent.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] on database or conversion failure.
pub fn get_pr_by_number(conn: &Connection, number: i64) -> rusqlite::Result<Option<PullRequest>> {
    conn.query_row(
        "SELECT node_id, number, title, state, is_draft, merged, merged_at, merged_by, base_ref,
                head_ref, additions, deletions, changed_files, author, body, created_at,
                updated_at, closed_at, milestone, labels_json, assignees_json, deleted
         FROM pull_requests WHERE number=?1",
        [number],
        row_to_pr,
    )
    .optional()
}

fn row_to_pr(r: &rusqlite::Row) -> rusqlite::Result<PullRequest> {
    let created_secs: i64 = r.get(15)?;
    let updated_secs: i64 = r.get(16)?;
    let merged_secs: Option<i64> = r.get(6)?;
    let closed_secs: Option<i64> = r.get(17)?;
    Ok(PullRequest {
        node_id: r.get(0)?,
        number: r.get(1)?,
        title: r.get(2)?,
        state: r.get(3)?,
        is_draft: r.get(4)?,
        merged: r.get(5)?,
        merged_at: merged_secs.map(|s| from_ts(s, 6)).transpose()?,
        merged_by: r.get(7)?,
        base_ref: r.get(8)?,
        head_ref: r.get(9)?,
        additions: r.get(10)?,
        deletions: r.get(11)?,
        changed_files: r.get(12)?,
        author: r.get(13)?,
        body: r.get(14)?,
        created_at: from_ts(created_secs, 15)?,
        updated_at: from_ts(updated_secs, 16)?,
        closed_at: closed_secs.map(|s| from_ts(s, 17)).transpose()?,
        milestone: r.get(18)?,
        labels: serde_json::from_str(&r.get::<_, String>(19)?).unwrap_or_default(),
        assignees: serde_json::from_str(&r.get::<_, String>(20)?).unwrap_or_default(),
        deleted: r.get(21)?,
    })
}

/// Replace all reviews for a PR with the given set (full refresh).
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if any database operation fails.
pub fn replace_reviews(
    conn: &impl Executor,
    pr_node_id: &str,
    reviews: &[Review],
) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM reviews WHERE pr_node_id=?1", [pr_node_id])?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO reviews (node_id, pr_node_id, author, state, body, submitted_at)
         VALUES (?1,?2,?3,?4,?5,?6)",
    )?;
    for v in reviews {
        stmt.execute(rusqlite::params![
            v.node_id,
            v.pr_node_id,
            v.author,
            v.state.as_str(),
            v.body,
            v.submitted_at.as_ref().map(ts),
        ])?;
    }
    Ok(())
}

/// List reviews for a PR. Null `submitted_at` (PENDING) sorts first; node id
/// breaks ties.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] on query/conversion failure.
pub fn list_reviews(conn: &Connection, pr_node_id: &str) -> rusqlite::Result<Vec<Review>> {
    let mut stmt = conn.prepare(
        "SELECT node_id, pr_node_id, author, state, body, submitted_at FROM reviews
         WHERE pr_node_id=?1 ORDER BY submitted_at ASC, node_id ASC",
    )?;
    let rows = stmt.query_map([pr_node_id], |r| {
        let state_str: String = r.get(3)?;
        let state =
            ReviewState::parse(&state_str).ok_or(rusqlite::Error::IntegralValueOutOfRange(3, 0))?;
        let submitted: Option<i64> = r.get(5)?;
        Ok(Review {
            node_id: r.get(0)?,
            pr_node_id: r.get(1)?,
            author: r.get(2)?,
            state,
            body: r.get(4)?,
            submitted_at: submitted.map(|s| from_ts(s, 5)).transpose()?,
        })
    })?;
    rows.collect()
}

/// Replace all review threads (and their comments) for a PR with the given set.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if any database operation fails.
pub fn replace_review_threads(
    conn: &impl Executor,
    pr_node_id: &str,
    threads: &[ReviewThread],
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM review_comments WHERE thread_node_id IN
            (SELECT node_id FROM review_threads WHERE pr_node_id=?1)",
        [pr_node_id],
    )?;
    conn.execute("DELETE FROM review_threads WHERE pr_node_id=?1", [
        pr_node_id,
    ])?;

    let mut thread_stmt = conn.prepare_cached(
        "INSERT INTO review_threads
            (node_id, pr_node_id, path, line, is_resolved, is_outdated, diff_hunk)
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
    )?;
    let mut comment_stmt = conn.prepare_cached(
        "INSERT INTO review_comments (node_id, thread_node_id, author, created_at, body)
         VALUES (?1,?2,?3,?4,?5)",
    )?;
    for t in threads {
        thread_stmt.execute(rusqlite::params![
            t.node_id,
            t.pr_node_id,
            t.path,
            t.line,
            t.is_resolved,
            t.is_outdated,
            t.diff_hunk,
        ])?;
        for c in &t.comments {
            comment_stmt.execute(rusqlite::params![
                c.node_id,
                c.thread_node_id,
                c.author,
                ts(&c.created_at),
                c.body
            ])?;
        }
    }
    Ok(())
}

/// List review threads (with comments) for a PR, deterministically ordered.
/// Threads: `(path, line NULLS FIRST, node_id)`. Comments: `(created_at,
/// node_id)`.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] on query/conversion failure.
pub fn list_review_threads(
    conn: &Connection,
    pr_node_id: &str,
) -> rusqlite::Result<Vec<ReviewThread>> {
    let mut stmt = conn.prepare(
        "SELECT node_id, pr_node_id, path, line, is_resolved, is_outdated, diff_hunk
         FROM review_threads WHERE pr_node_id=?1 ORDER BY path ASC, line ASC, node_id ASC",
    )?;
    let mut threads: Vec<ReviewThread> = stmt
        .query_map([pr_node_id], |r| {
            Ok(ReviewThread {
                node_id: r.get(0)?,
                pr_node_id: r.get(1)?,
                path: r.get(2)?,
                line: r.get(3)?,
                is_resolved: r.get(4)?,
                is_outdated: r.get(5)?,
                diff_hunk: r.get(6)?,
                comments: Vec::new(),
            })
        })?
        .collect::<Result<_, _>>()?;
    let mut cstmt = conn.prepare(
        "SELECT node_id, thread_node_id, author, created_at, body FROM review_comments
         WHERE thread_node_id=?1 ORDER BY created_at ASC, node_id ASC",
    )?;
    for t in &mut threads {
        t.comments = cstmt
            .query_map([&t.node_id], |r| {
                let secs: i64 = r.get(3)?;
                Ok(ReviewComment {
                    node_id: r.get(0)?,
                    thread_node_id: r.get(1)?,
                    author: r.get(2)?,
                    created_at: from_ts(secs, 3)?,
                    body: r.get(4)?,
                })
            })?
            .collect::<Result<_, _>>()?;
    }
    Ok(threads)
}

/// Distinct active issue numbers a PR closes (subset of `related_numbers`),
/// ascending.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] on query/conversion failure.
pub fn closes_numbers(conn: &Connection, pr_node_id: &str) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT referenced_issue_number FROM cross_refs
         WHERE issue_node_id=?1 AND event_type=?2 AND is_active=1
         ORDER BY referenced_issue_number ASC",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![
            pr_node_id,
            crate::model::CrossRefEvent::ClosingReference.as_str()
        ],
        |r| r.get(0),
    )?;
    rows.collect()
}

/// Non-deleted PR numbers, ascending (for rendering).
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] on query/conversion failure.
pub fn pr_numbers(conn: &Connection) -> rusqlite::Result<Vec<i64>> {
    let mut stmt =
        conn.prepare("SELECT number FROM pull_requests WHERE deleted=0 ORDER BY number")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    rows.collect()
}

/// Mark every non-deleted PR whose `node_id` is NOT in `seen` as deleted.
/// Returns the number newly marked deleted.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] on database failure.
pub fn mark_deleted_except<S: std::hash::BuildHasher>(
    conn: &Connection,
    seen: &std::collections::HashSet<String, S>,
) -> rusqlite::Result<usize> {
    crate::store::mark_deleted_except(conn, "pull_requests", seen)
}

/// Counts of non-deleted PRs by effective state: `(open, draft, closed,
/// merged)`.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] on query/conversion failure.
pub fn effective_state_counts(conn: &Connection) -> rusqlite::Result<(i64, i64, i64, i64)> {
    let mut stmt =
        conn.prepare("SELECT state, is_draft, merged FROM pull_requests WHERE deleted=0")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, bool>(1)?,
            r.get::<_, bool>(2)?,
        ))
    })?;
    let (mut open, mut draft, mut closed, mut merged) = (0i64, 0i64, 0i64, 0i64);
    for row in rows {
        let (state, is_draft, is_merged) = row?;
        match PrState::from_parts(&state, is_draft, is_merged) {
            PrState::Open => open += 1,
            PrState::Draft => draft += 1,
            PrState::Closed => closed += 1,
            PrState::Merged => merged += 1,
        }
    }
    Ok((open, draft, closed, merged))
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use chrono::Utc;

    use super::*;
    use crate::model::PullRequest;
    use crate::model::Review;
    use crate::model::ReviewComment;
    use crate::model::ReviewState;
    use crate::model::ReviewThread;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn pr(node: &str, n: i64) -> PullRequest {
        PullRequest {
            node_id: node.into(),
            number: n,
            title: "t".into(),
            state: "MERGED".into(),
            is_draft: false,
            merged: true,
            merged_at: Some(dt("2026-06-14T00:00:00Z")),
            merged_by: Some("demosdemon".into()),
            base_ref: "main".into(),
            head_ref: "feature".into(),
            additions: 10,
            deletions: 2,
            changed_files: 3,
            author: Some("octocat".into()),
            body: "desc".into(),
            created_at: dt("2026-06-10T00:00:00Z"),
            updated_at: dt("2026-06-14T00:00:00Z"),
            closed_at: Some(dt("2026-06-14T00:00:00Z")),
            milestone: Some("v1.0".into()),
            labels: vec!["bug".into()],
            assignees: vec!["octocat".into()],
            deleted: false,
        }
    }

    #[test]
    fn pr_upsert_idempotent_round_trip() {
        let conn = crate::store::open_in_memory().unwrap();
        upsert_pull_request(&conn, &pr("P1", 42)).unwrap();
        upsert_pull_request(&conn, &pr("P1", 42)).unwrap();
        let got = get_pr_by_number(&conn, 42).unwrap().unwrap();
        assert_eq!(got, pr("P1", 42));
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM pull_requests", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn reviews_replace_and_order_pending_first() {
        let conn = crate::store::open_in_memory().unwrap();
        upsert_pull_request(&conn, &pr("P1", 42)).unwrap();
        let approved = Review {
            node_id: "R2".into(),
            pr_node_id: "P1".into(),
            author: Some("a".into()),
            state: ReviewState::Approved,
            body: "lgtm".into(),
            submitted_at: Some(dt("2026-06-14T00:00:00Z")),
        };
        let pending = Review {
            node_id: "R1".into(),
            pr_node_id: "P1".into(),
            author: Some("b".into()),
            state: ReviewState::Pending,
            body: String::new(),
            submitted_at: None,
        };
        replace_reviews(&conn, "P1", &[approved.clone(), pending.clone()]).unwrap();
        let got = list_reviews(&conn, "P1").unwrap();
        // null submitted_at sorts first
        assert_eq!(got, vec![pending, approved]);
        replace_reviews(&conn, "P1", &[]).unwrap();
        assert!(list_reviews(&conn, "P1").unwrap().is_empty());
    }

    #[test]
    fn threads_replace_with_comments_and_order() {
        let conn = crate::store::open_in_memory().unwrap();
        upsert_pull_request(&conn, &pr("P1", 42)).unwrap();
        let t_outdated = ReviewThread {
            node_id: "T2".into(),
            pr_node_id: "P1".into(),
            path: "a.rs".into(),
            line: None, // outdated → sorts first within the path
            is_resolved: false,
            is_outdated: true,
            diff_hunk: "@@ -1 +1 @@".into(),
            comments: vec![ReviewComment {
                node_id: "RC2".into(),
                thread_node_id: "T2".into(),
                author: Some("x".into()),
                created_at: dt("2026-06-12T00:00:00Z"),
                body: "old".into(),
            }],
        };
        let t_live = ReviewThread {
            node_id: "T1".into(),
            pr_node_id: "P1".into(),
            path: "a.rs".into(),
            line: Some(5),
            is_resolved: true,
            is_outdated: false,
            diff_hunk: "@@ -5 +5 @@".into(),
            comments: vec![ReviewComment {
                node_id: "RC1".into(),
                thread_node_id: "T1".into(),
                author: Some("y".into()),
                created_at: dt("2026-06-11T00:00:00Z"),
                body: "fix".into(),
            }],
        };
        replace_review_threads(&conn, "P1", &[t_live.clone(), t_outdated.clone()]).unwrap();
        let got = list_review_threads(&conn, "P1").unwrap();
        assert_eq!(got, vec![t_outdated, t_live]); // (path, line NULLS FIRST, node_id)
        assert_eq!(got[0].comments.len(), 1);
        // replacing clears prior comments too
        replace_review_threads(&conn, "P1", &[]).unwrap();
        assert!(list_review_threads(&conn, "P1").unwrap().is_empty());
        let rc: i64 = conn
            .query_row("SELECT COUNT(*) FROM review_comments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rc, 0);
    }

    #[test]
    fn closes_numbers_filters_to_closing_event() {
        use crate::model::CrossRef;
        use crate::model::CrossRefEvent;
        let conn = crate::store::open_in_memory().unwrap();
        upsert_pull_request(&conn, &pr("P1", 42)).unwrap();
        // A closing reference (PR 42 closes issue 41) + a plain cross-reference.
        crate::store::issues::upsert_cross_ref(&conn, &CrossRef {
            issue_node_id: "P1".into(),
            referenced_issue_number: 41,
            event_type: CrossRefEvent::ClosingReference,
            created_at: dt("2026-06-10T00:00:00Z"),
        })
        .unwrap();
        crate::store::issues::upsert_cross_ref(&conn, &CrossRef {
            issue_node_id: "P1".into(),
            referenced_issue_number: 7,
            event_type: CrossRefEvent::CrossReferenced,
            created_at: dt("2026-06-10T00:00:00Z"),
        })
        .unwrap();
        assert_eq!(closes_numbers(&conn, "P1").unwrap(), vec![41]);
        // related (reused issue helper) includes both
        assert_eq!(
            crate::store::issues::related_numbers(&conn, "P1").unwrap(),
            vec![7, 41]
        );
    }

    #[test]
    fn mark_deleted_except_and_counts() {
        let conn = crate::store::open_in_memory().unwrap();
        upsert_pull_request(&conn, &pr("P1", 1)).unwrap();
        let mut p2 = pr("P2", 2);
        p2.merged = false;
        p2.state = "OPEN".into();
        p2.is_draft = true;
        upsert_pull_request(&conn, &p2).unwrap();
        let mut seen = std::collections::HashSet::new();
        seen.insert("P1".to_string());
        assert_eq!(mark_deleted_except(&conn, &seen).unwrap(), 1);
        assert!(get_pr_by_number(&conn, 2).unwrap().unwrap().deleted);
        // counts exclude deleted; P1 merged, P2 now deleted
        let (open, draft, closed, merged) = effective_state_counts(&conn).unwrap();
        assert_eq!((open, draft, closed, merged), (0, 0, 0, 1));
    }
}
