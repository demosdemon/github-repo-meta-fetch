use rusqlite::Connection;
use rusqlite::OptionalExtension;

use crate::model::Label;
use crate::model::Milestone;

/// Replace all labels with a fresh set (full refresh).
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if any database operation fails.
pub fn replace_labels(conn: &Connection, labels: &[Label]) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM labels", [])?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO labels (node_id, name, color, description) VALUES (?1,?2,?3,?4)",
        )?;
        for l in labels {
            stmt.execute(rusqlite::params![l.node_id, l.name, l.color, l.description])?;
        }
    }
    tx.commit()
}

/// Replace all milestones with a fresh set (full refresh).
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if any database operation fails.
pub fn replace_milestones(conn: &Connection, ms: &[Milestone]) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM milestones", [])?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO milestones (node_id, number, title, state, description, due_on)
             VALUES (?1,?2,?3,?4,?5,?6)",
        )?;
        for m in ms {
            stmt.execute(rusqlite::params![
                m.node_id,
                m.number,
                m.title,
                m.state,
                m.description,
                m.due_on.map(|d| d.timestamp())
            ])?;
        }
    }
    tx.commit()
}

/// All labels, ordered by name.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if the query or any row conversion fails.
pub fn all_labels(conn: &Connection) -> rusqlite::Result<Vec<Label>> {
    let mut stmt =
        conn.prepare("SELECT node_id, name, color, description FROM labels ORDER BY name ASC")?;
    let rows = stmt.query_map([], |r| {
        Ok(Label {
            node_id: r.get(0)?,
            name: r.get(1)?,
            color: r.get(2)?,
            description: r.get(3)?,
        })
    })?;
    rows.collect()
}

/// All milestones, ordered by title.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if the query or any row conversion fails.
pub fn all_milestones(conn: &Connection) -> rusqlite::Result<Vec<Milestone>> {
    let mut stmt = conn.prepare(
        "SELECT node_id, number, title, state, description, due_on \
         FROM milestones ORDER BY title ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        let due_on = r
            .get::<_, Option<i64>>(5)?
            .and_then(|s| chrono::DateTime::from_timestamp(s, 0));
        Ok(Milestone {
            node_id: r.get(0)?,
            number: r.get(1)?,
            title: r.get(2)?,
            state: r.get(3)?,
            description: r.get(4)?,
            due_on,
        })
    })?;
    rows.collect()
}

/// Client-side usage counts: number of non-deleted issues carrying each label
/// name. Returns `(label_name, count)` sorted by name.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if the query or any row conversion fails.
pub fn label_usage_counts(conn: &Connection) -> rusqlite::Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT j.value AS name, COUNT(*) AS cnt
         FROM issues i, json_each(i.labels_json) j
         WHERE i.deleted = 0
         GROUP BY j.value ORDER BY j.value ASC",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    rows.collect()
}

/// Retrieve the stored `ETag` for a named resource, or `None` if not yet
/// recorded.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] on any database or conversion failure.
pub fn get_etag(conn: &Connection, resource: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT etag FROM etags WHERE resource=?1",
        [resource],
        |r| r.get(0),
    )
    .optional()
}

/// Upsert the `ETag` for a named resource.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if the upsert fails.
pub fn set_etag(conn: &Connection, resource: &str, etag: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO etags (resource, etag) VALUES (?1,?2)
         ON CONFLICT(resource) DO UPDATE SET etag=excluded.etag",
        rusqlite::params![resource, etag],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use chrono::Utc;

    use super::*;
    use crate::model::Issue;
    use crate::model::IssueState;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn issue_with_labels(node: &str, num: i64, labels: &[&str], deleted: bool) -> Issue {
        Issue {
            node_id: node.into(),
            number: num,
            title: "t".into(),
            state: IssueState::Open,
            state_reason: None,
            author: None,
            body: String::new(),
            created_at: dt("2026-01-01T00:00:00Z"),
            updated_at: dt("2026-01-01T00:00:00Z"),
            closed_at: None,
            milestone: None,
            labels: labels.iter().map(|s| (*s).to_string()).collect(),
            assignees: vec![],
            deleted,
        }
    }

    #[test]
    fn label_usage_counts_exclude_deleted() {
        let conn = crate::store::open_in_memory().unwrap();
        crate::store::issues::upsert_issue(
            &conn,
            &issue_with_labels("I1", 1, &["bug", "ui"], false),
        )
        .unwrap();
        crate::store::issues::upsert_issue(&conn, &issue_with_labels("I2", 2, &["bug"], false))
            .unwrap();
        crate::store::issues::upsert_issue(&conn, &issue_with_labels("I3", 3, &["bug"], true))
            .unwrap();
        let counts = label_usage_counts(&conn).unwrap();
        assert_eq!(counts, vec![("bug".to_string(), 2), ("ui".to_string(), 1)]);
    }

    #[test]
    fn etag_round_trips() {
        let conn = crate::store::open_in_memory().unwrap();
        assert_eq!(get_etag(&conn, "labels").unwrap(), None);
        set_etag(&conn, "labels", "W/\"abc\"").unwrap();
        assert_eq!(
            get_etag(&conn, "labels").unwrap().as_deref(),
            Some("W/\"abc\"")
        );
    }

    #[test]
    fn replace_labels_is_full_refresh() {
        let conn = crate::store::open_in_memory().unwrap();
        let l = Label {
            node_id: "L1".into(),
            name: "bug".into(),
            color: "f00".into(),
            description: None,
        };
        replace_labels(&conn, std::slice::from_ref(&l)).unwrap();
        replace_labels(&conn, std::slice::from_ref(&l)).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM labels", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
