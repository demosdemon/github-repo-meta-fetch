use rusqlite::Connection;
use rusqlite::OptionalExtension;

use crate::model::IssueState;
use crate::model::RelEdge;
use crate::model::RelKind;
use crate::model::RelTarget;
use crate::model::Relationships;
use crate::store::Executor;

/// Replace every relationship edge incident to `node_id` with the given set
/// (built from that issue's fresh fetch, which covers all four incident
/// roles: as parent, as child, as blocker, as blocked).
///
/// A `'parent'` edge whose child is `node_id` and whose `position` is `None`
/// keeps the previously stored position when the parent is unchanged (the
/// child-side fetch cannot see its own index among siblings). Inserting any
/// `'parent'` edge first deletes an existing parent row for that child, so a
/// reparent observed from the new parent's side displaces the stale row
/// instead of colliding with the one-parent-per-child unique index.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if any statement fails.
pub fn replace_incident_edges(
    conn: &impl Executor,
    node_id: &str,
    edges: &[RelEdge],
) -> rusqlite::Result<()> {
    // Remember this issue's position under its own parent before the delete.
    let prior: Option<(String, Option<i64>)> = conn
        .query_row(
            "SELECT src_node_id, position FROM issue_relationships
             WHERE rel = 'parent' AND dst_node_id = ?1",
            [node_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;

    conn.execute(
        "DELETE FROM issue_relationships WHERE src_node_id = ?1 OR dst_node_id = ?1",
        [node_id],
    )?;

    let mut delete_parent = conn.prepare_cached(
        "DELETE FROM issue_relationships WHERE rel = 'parent' AND dst_node_id = ?1",
    )?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO issue_relationships
            (rel, src_node_id, dst_node_id, position,
             src_repo, src_number, src_state, src_title,
             dst_repo, dst_number, dst_state, dst_title)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
    )?;

    for e in edges {
        let position = match (e.rel, e.position) {
            (RelKind::Parent, None) if e.dst.node_id == node_id => prior
                .as_ref()
                .filter(|(prev_src, _)| *prev_src == e.src.node_id)
                .and_then(|(_, pos)| *pos),
            (_, p) => p,
        };
        if e.rel == RelKind::Parent {
            delete_parent.execute([e.dst.node_id.as_str()])?;
        }
        insert.execute(rusqlite::params![
            e.rel.as_str(),
            e.src.node_id,
            e.dst.node_id,
            position,
            e.src.repo,
            e.src.number,
            e.src.state.as_str(),
            e.src.title,
            e.dst.repo,
            e.dst.number,
            e.dst.state.as_str(),
            e.dst.title,
        ])?;
    }
    Ok(())
}

/// A resolved parent → child edge for `hierarchy.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentEdge {
    pub parent: RelTarget,
    pub child: RelTarget,
}

/// Map one resolved endpoint row triple into a [`RelTarget`]. Column order:
/// `node_id, repo, number, state, title, position`.
fn target_from_row(r: &rusqlite::Row<'_>, base: usize) -> rusqlite::Result<RelTarget> {
    let state_str: String = r.get(base + 3)?;
    let state = IssueState::parse(&state_str)
        .ok_or(rusqlite::Error::IntegralValueOutOfRange(base + 3, 0))?;
    Ok(RelTarget {
        node_id: r.get(base)?,
        repo: r.get(base + 1)?,
        number: r.get(base + 2)?,
        state,
        title: r.get(base + 4)?,
        position: r.get(base + 5)?,
    })
}

/// Targets on one side of edges matching (`rel`, other-side column = node).
/// The SQL constants resolve one endpoint: live `issues` values win via
/// `COALESCE`; snapshots fill in when no live row exists.
fn targets(
    conn: &Connection,
    node_id: &str,
    rel: &str,
    sql: &str,
) -> rusqlite::Result<Vec<RelTarget>> {
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map(rusqlite::params![node_id, rel], |r| target_from_row(r, 0))?;
    rows.collect()
}

/// Resolve the `src` endpoint of edges arriving at `?1` (`dst_node_id`).
const SRC_TARGETS_SQL: &str = "\
    SELECT r.src_node_id, r.src_repo,
           COALESCE(i.number, r.src_number),
           COALESCE(i.state, r.src_state),
           COALESCE(i.title, r.src_title),
           r.position
    FROM issue_relationships r
    LEFT JOIN issues i ON r.src_repo IS NULL AND i.node_id = r.src_node_id
    WHERE r.dst_node_id = ?1 AND r.rel = ?2
      AND (i.deleted IS NULL OR i.deleted = 0)";

/// Resolve the `dst` endpoint of edges leaving `?1` (`src_node_id`).
const DST_TARGETS_SQL: &str = "\
    SELECT r.dst_node_id, r.dst_repo,
           COALESCE(i.number, r.dst_number),
           COALESCE(i.state, r.dst_state),
           COALESCE(i.title, r.dst_title),
           r.position
    FROM issue_relationships r
    LEFT JOIN issues i ON r.dst_repo IS NULL AND i.node_id = r.dst_node_id
    WHERE r.src_node_id = ?1 AND r.rel = ?2
      AND (i.deleted IS NULL OR i.deleted = 0)";

/// All relationships of one issue, resolved for rendering (unsorted).
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if any query or row conversion fails.
pub fn relationships_for(conn: &Connection, node_id: &str) -> rusqlite::Result<Relationships> {
    Ok(Relationships {
        parent: targets(conn, node_id, "parent", SRC_TARGETS_SQL)?
            .into_iter()
            .next(),
        sub_issues: targets(conn, node_id, "parent", DST_TARGETS_SQL)?,
        blocked_by: targets(conn, node_id, "blocks", SRC_TARGETS_SQL)?,
        blocking: targets(conn, node_id, "blocks", DST_TARGETS_SQL)?,
    })
}

/// Every parent → child edge with both endpoints resolved, excluding edges
/// where either same-repo endpoint is soft-deleted. Unsorted.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if the query or any row conversion fails.
pub fn all_parent_edges(conn: &Connection) -> rusqlite::Result<Vec<ParentEdge>> {
    let sql = "\
        SELECT r.src_node_id, r.src_repo,
               COALESCE(pi.number, r.src_number),
               COALESCE(pi.state, r.src_state),
               COALESCE(pi.title, r.src_title),
               NULL,
               r.dst_node_id, r.dst_repo,
               COALESCE(ci.number, r.dst_number),
               COALESCE(ci.state, r.dst_state),
               COALESCE(ci.title, r.dst_title),
               r.position
        FROM issue_relationships r
        LEFT JOIN issues pi ON r.src_repo IS NULL AND pi.node_id = r.src_node_id
        LEFT JOIN issues ci ON r.dst_repo IS NULL AND ci.node_id = r.dst_node_id
        WHERE r.rel = 'parent'
          AND (pi.deleted IS NULL OR pi.deleted = 0)
          AND (ci.deleted IS NULL OR ci.deleted = 0)";
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map([], |r| {
        Ok(ParentEdge {
            parent: target_from_row(r, 0)?,
            child: target_from_row(r, 6)?,
        })
    })?;
    rows.collect()
}

/// Stored edge counts: `(dependency_edges, sub_issue_edges)`.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if the query fails.
pub fn edge_counts(conn: &Connection) -> rusqlite::Result<(i64, i64)> {
    let blocks: i64 = conn.query_row(
        "SELECT COUNT(*) FROM issue_relationships WHERE rel='blocks'",
        [],
        |r| r.get(0),
    )?;
    let parent: i64 = conn.query_row(
        "SELECT COUNT(*) FROM issue_relationships WHERE rel='parent'",
        [],
        |r| r.get(0),
    )?;
    Ok((blocks, parent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::IssueState;
    use crate::model::RelEndpoint;

    fn ep(node: &str, number: i64) -> RelEndpoint {
        RelEndpoint {
            node_id: node.into(),
            repo: None,
            number,
            state: IssueState::Open,
            title: format!("issue {number}"),
        }
    }

    fn ext_ep(node: &str, repo: &str, number: i64) -> RelEndpoint {
        RelEndpoint {
            repo: Some(repo.into()),
            ..ep(node, number)
        }
    }

    fn count(conn: &rusqlite::Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM issue_relationships", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn migration_resets_only_issues_sync_state() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::store::migrations().to_version(&mut conn, 2).unwrap();
        conn.execute_batch(
            "INSERT INTO sync_state (entity_type, updated_watermark, resume_cursor, run_phase)
             VALUES ('issues', 123, 'CUR', 'paginating'),
                    ('pull_requests', 456, 'PC', 'done');",
        )
        .unwrap();
        crate::store::migrations().to_latest(&mut conn).unwrap();

        let s = crate::store::sync_state::get(&conn, "issues").unwrap();
        assert_eq!(s.updated_watermark, None);
        assert_eq!(s.resume_cursor, None);
        assert_eq!(s.run_phase, crate::store::sync_state::RunPhase::Idle);
        let p = crate::store::sync_state::get(&conn, "pull_requests").unwrap();
        assert!(p.updated_watermark.is_some());
        assert_eq!(p.resume_cursor.as_deref(), Some("PC"));
    }

    #[test]
    fn schema_rejects_position_on_blocks_and_double_parent() {
        let conn = crate::store::open_in_memory().unwrap();
        let insert = "INSERT INTO issue_relationships
            (rel, src_node_id, dst_node_id, position,
             src_repo, src_number, src_state, src_title,
             dst_repo, dst_number, dst_state, dst_title)
            VALUES (?1,?2,?3,?4,NULL,1,'open','a',NULL,2,'open','b')";
        assert!(
            conn.execute(insert, rusqlite::params!["blocks", "A", "B", 3_i64])
                .is_err(),
            "position on a blocks row must violate CHECK"
        );
        conn.execute(insert, rusqlite::params![
            "parent",
            "P1",
            "C",
            Option::<i64>::None
        ])
        .unwrap();
        assert!(
            conn.execute(insert, rusqlite::params![
                "parent",
                "P2",
                "C",
                Option::<i64>::None
            ])
            .is_err(),
            "second parent for one child must violate the partial unique index"
        );
    }

    #[test]
    fn replace_covers_all_incident_roles() {
        let conn = crate::store::open_in_memory().unwrap();
        // X (I2) has parent I1, child I3, blocker I4, and blocks I5.
        let edges = vec![
            RelEdge {
                rel: RelKind::Parent,
                src: ep("I1", 1),
                dst: ep("I2", 2),
                position: None,
            },
            RelEdge {
                rel: RelKind::Parent,
                src: ep("I2", 2),
                dst: ep("I3", 3),
                position: Some(0),
            },
            RelEdge {
                rel: RelKind::Blocks,
                src: ep("I4", 4),
                dst: ep("I2", 2),
                position: None,
            },
            RelEdge {
                rel: RelKind::Blocks,
                src: ep("I2", 2),
                dst: ep("I5", 5),
                position: None,
            },
        ];
        replace_incident_edges(&conn, "I2", &edges).unwrap();
        assert_eq!(count(&conn), 4);

        // Re-sync with fewer edges → removed edges disappear.
        replace_incident_edges(&conn, "I2", &edges[..1]).unwrap();
        assert_eq!(count(&conn), 1);
    }

    #[test]
    fn child_sync_preserves_position_under_same_parent_only() {
        let conn = crate::store::open_in_memory().unwrap();
        // Parent P1 syncs: child C at position 2.
        replace_incident_edges(&conn, "P1", &[RelEdge {
            rel: RelKind::Parent,
            src: ep("P1", 1),
            dst: ep("C", 9),
            position: Some(2),
        }])
        .unwrap();

        // Child C re-syncs, still under P1, child-side position unknown (None).
        replace_incident_edges(&conn, "C", &[RelEdge {
            rel: RelKind::Parent,
            src: ep("P1", 1),
            dst: ep("C", 9),
            position: None,
        }])
        .unwrap();
        let pos: Option<i64> = conn
            .query_row(
                "SELECT position FROM issue_relationships WHERE rel='parent' AND dst_node_id='C'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            pos,
            Some(2),
            "position preserved when the parent is unchanged"
        );

        // Child C re-syncs, reparented to P2 → stale position NOT carried over.
        replace_incident_edges(&conn, "C", &[RelEdge {
            rel: RelKind::Parent,
            src: ep("P2", 7),
            dst: ep("C", 9),
            position: None,
        }])
        .unwrap();
        let (src, pos): (String, Option<i64>) = conn
            .query_row(
                "SELECT src_node_id, position FROM issue_relationships WHERE rel='parent' AND dst_node_id='C'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(src, "P2");
        assert_eq!(pos, None, "reparent resets position");
    }

    #[test]
    fn new_parent_syncing_first_displaces_stale_parent_row() {
        let conn = crate::store::open_in_memory().unwrap();
        // Old parent P1 syncs with child C.
        replace_incident_edges(&conn, "P1", &[RelEdge {
            rel: RelKind::Parent,
            src: ep("P1", 1),
            dst: ep("C", 9),
            position: Some(0),
        }])
        .unwrap();
        // C is reparented to P2 on GitHub; P2 syncs first. The stale (P1, C)
        // row is incident to neither P2 nor C-as-seen-by-P2's-delete, but the
        // insert must displace it rather than hit the unique index.
        replace_incident_edges(&conn, "P2", &[RelEdge {
            rel: RelKind::Parent,
            src: ep("P2", 7),
            dst: ep("C", 9),
            position: Some(1),
        }])
        .unwrap();
        let src: String = conn
            .query_row(
                "SELECT src_node_id FROM issue_relationships WHERE rel='parent' AND dst_node_id='C'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(src, "P2");
        assert_eq!(count(&conn), 1);
    }

    #[test]
    fn stores_cross_repo_snapshots() {
        let conn = crate::store::open_in_memory().unwrap();
        replace_incident_edges(&conn, "I2", &[RelEdge {
            rel: RelKind::Blocks,
            src: ext_ep("EXT1", "acme/infra", 4),
            dst: ep("I2", 2),
            position: None,
        }])
        .unwrap();
        let (repo, number, state, title): (Option<String>, i64, String, String) = conn
            .query_row(
                "SELECT src_repo, src_number, src_state, src_title FROM issue_relationships",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(repo.as_deref(), Some("acme/infra"));
        assert_eq!(number, 4);
        assert_eq!(state, "open");
        assert_eq!(title, "issue 4");
    }

    use chrono::DateTime;
    use chrono::Utc;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn live_issue(
        conn: &rusqlite::Connection,
        node: &str,
        number: i64,
        state: IssueState,
        deleted: bool,
    ) {
        crate::store::issues::upsert_issue(conn, &crate::model::Issue {
            node_id: node.into(),
            number,
            title: format!("live {number}"),
            state,
            state_reason: None,
            author: None,
            body: String::new(),
            created_at: dt("2026-01-01T00:00:00Z"),
            updated_at: dt("2026-01-01T00:00:00Z"),
            closed_at: None,
            milestone: None,
            labels: vec![],
            assignees: vec![],
            deleted,
        })
        .unwrap();
    }

    #[test]
    fn relationships_resolve_live_rows_and_snapshots() {
        let conn = crate::store::open_in_memory().unwrap();
        live_issue(&conn, "I2", 2, IssueState::Open, false);
        live_issue(&conn, "I4", 4, IssueState::Closed, false); // closed blocker
        live_issue(&conn, "I6", 6, IssueState::Open, true); // deleted blocker
        let edges = vec![
            RelEdge {
                rel: RelKind::Parent,
                src: ep("I1", 1),
                dst: ep("I2", 2),
                position: None,
            },
            RelEdge {
                rel: RelKind::Parent,
                src: ep("I2", 2),
                dst: ep("I3", 3),
                position: Some(0),
            },
            RelEdge {
                rel: RelKind::Blocks,
                src: ep("I4", 4),
                dst: ep("I2", 2),
                position: None,
            },
            RelEdge {
                rel: RelKind::Blocks,
                src: ep("I6", 6),
                dst: ep("I2", 2),
                position: None,
            },
            RelEdge {
                rel: RelKind::Blocks,
                src: ext_ep("EXT1", "acme/infra", 4),
                dst: ep("I2", 2),
                position: None,
            },
            RelEdge {
                rel: RelKind::Blocks,
                src: ep("I2", 2),
                dst: ep("I5", 5),
                position: None,
            },
        ];
        replace_incident_edges(&conn, "I2", &edges).unwrap();

        let r = relationships_for(&conn, "I2").unwrap();
        // Parent I1 has no live row → snapshot values survive.
        assert_eq!(r.parent.as_ref().unwrap().number, 1);
        assert_eq!(r.sub_issues.len(), 1);
        assert_eq!(r.sub_issues[0].position, Some(0));
        // Deleted blocker I6 excluded; live-closed I4 resolved from issues table.
        assert_eq!(r.blocked_by.len(), 2);
        let i4 = r
            .blocked_by
            .iter()
            .find(|t| t.number == 4 && t.repo.is_none())
            .unwrap();
        assert_eq!(i4.state, IssueState::Closed);
        assert_eq!(i4.title, "live 4");
        let ext = r.blocked_by.iter().find(|t| t.repo.is_some()).unwrap();
        assert_eq!(ext.reference(), "acme/infra#4");
        assert_eq!(r.blocking.len(), 1);
        assert_eq!(r.blocking[0].number, 5);
    }

    #[test]
    fn deleted_parent_resolves_to_none() {
        let conn = crate::store::open_in_memory().unwrap();
        live_issue(&conn, "I1", 1, IssueState::Open, true); // deleted parent
        replace_incident_edges(&conn, "I2", &[RelEdge {
            rel: RelKind::Parent,
            src: ep("I1", 1),
            dst: ep("I2", 2),
            position: None,
        }])
        .unwrap();
        let r = relationships_for(&conn, "I2").unwrap();
        assert_eq!(r.parent, None);
    }

    #[test]
    fn all_parent_edges_excludes_deleted_endpoints() {
        let conn = crate::store::open_in_memory().unwrap();
        live_issue(&conn, "P", 1, IssueState::Open, false);
        live_issue(&conn, "C1", 2, IssueState::Open, false);
        live_issue(&conn, "C2", 3, IssueState::Open, true); // deleted child
        replace_incident_edges(&conn, "P", &[
            RelEdge {
                rel: RelKind::Parent,
                src: ep("P", 1),
                dst: ep("C1", 2),
                position: Some(0),
            },
            RelEdge {
                rel: RelKind::Parent,
                src: ep("P", 1),
                dst: ep("C2", 3),
                position: Some(1),
            },
        ])
        .unwrap();
        let edges = all_parent_edges(&conn).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].parent.node_id, "P");
        assert_eq!(edges[0].child.node_id, "C1");
        assert_eq!(edges[0].child.position, Some(0));
    }

    #[test]
    fn edge_counts_by_kind() {
        let conn = crate::store::open_in_memory().unwrap();
        replace_incident_edges(&conn, "I2", &[
            RelEdge {
                rel: RelKind::Blocks,
                src: ep("I4", 4),
                dst: ep("I2", 2),
                position: None,
            },
            RelEdge {
                rel: RelKind::Blocks,
                src: ep("I2", 2),
                dst: ep("I5", 5),
                position: None,
            },
            RelEdge {
                rel: RelKind::Parent,
                src: ep("I1", 1),
                dst: ep("I2", 2),
                position: None,
            },
        ])
        .unwrap();
        assert_eq!(edge_counts(&conn).unwrap(), (2, 1));
    }
}
