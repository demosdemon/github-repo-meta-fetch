use rusqlite::Connection;
use rusqlite::OptionalExtension;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoMeta {
    pub owner: String,
    pub repo: String,
    pub padding_width: u32,
    pub last_full_sync_at: Option<i64>,
}

/// Insert the `repo_meta` row if absent (idempotent). Does not change
/// `padding_width`.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if the insert fails.
pub fn ensure(conn: &Connection, owner: &str, repo: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO repo_meta (id, owner, repo) VALUES (1, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET owner=excluded.owner, repo=excluded.repo",
        rusqlite::params![owner, repo],
    )?;
    Ok(())
}

/// Retrieve the single `repo_meta` row, or `None` if it has not been seeded
/// yet.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] on database failure.
pub fn get(conn: &Connection) -> rusqlite::Result<Option<RepoMeta>> {
    conn.query_row(
        "SELECT owner, repo, padding_width, last_full_sync_at FROM repo_meta WHERE id=1",
        [],
        |r| {
            Ok(RepoMeta {
                owner: r.get(0)?,
                repo: r.get(1)?,
                padding_width: u32::try_from(r.get::<_, i64>(2)?).unwrap_or(4),
                last_full_sync_at: r.get(3)?,
            })
        },
    )
    .optional()
}

/// Update `last_full_sync_at` on the `repo_meta` row.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if the update fails.
pub fn set_last_full_sync(conn: &Connection, ts: i64) -> rusqlite::Result<()> {
    conn.execute("UPDATE repo_meta SET last_full_sync_at=?1 WHERE id=1", [ts])?;
    Ok(())
}

/// Compute the minimum width needed to represent `max_number`, never below 4.
#[must_use]
pub fn width_for(max_number: i64) -> u32 {
    let digits = if max_number <= 0 {
        1
    } else {
        max_number.ilog10() + 1
    };
    digits.max(4)
}

/// Ensure `padding_width` is at least `needed`; grow but never shrink. Returns
/// the new width.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] on database failure.
pub fn grow_padding_width(conn: &Connection, needed: u32) -> rusqlite::Result<u32> {
    let current_raw: i64 =
        conn.query_row("SELECT padding_width FROM repo_meta WHERE id=1", [], |r| {
            r.get(0)
        })?;
    let current = u32::try_from(current_raw).unwrap_or(4);
    let new = current.max(needed);
    if new != current {
        conn.execute("UPDATE repo_meta SET padding_width=?1 WHERE id=1", [new])?;
    }
    Ok(new)
}

#[cfg(test)]
mod width_tests {
    use super::*;

    #[test]
    fn width_floor_is_four() {
        assert_eq!(width_for(0), 4);
        assert_eq!(width_for(42), 4);
        assert_eq!(width_for(9999), 4);
    }

    #[test]
    fn width_grows_past_ten_thousand() {
        assert_eq!(width_for(10000), 5);
        assert_eq!(width_for(123_456), 6);
    }

    #[test]
    fn grow_only_never_shrinks() {
        let conn = crate::store::open_in_memory().unwrap();
        ensure(&conn, "a", "b").unwrap();
        assert_eq!(grow_padding_width(&conn, 6).unwrap(), 6);
        // a later smaller request keeps 6
        assert_eq!(grow_padding_width(&conn, 4).unwrap(), 6);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_and_meta_round_trips() {
        let conn = crate::store::open_in_memory().unwrap();
        ensure(&conn, "octocat", "hello-world").unwrap();
        let m = get(&conn).unwrap().unwrap();
        assert_eq!(m.owner, "octocat");
        assert_eq!(m.repo, "hello-world");
        assert_eq!(m.padding_width, 4);
        assert_eq!(m.last_full_sync_at, None);
    }

    #[test]
    fn ensure_is_idempotent() {
        let conn = crate::store::open_in_memory().unwrap();
        ensure(&conn, "a", "b").unwrap();
        ensure(&conn, "a", "b").unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM repo_meta", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
