pub mod frontmatter;
pub mod indexes;
pub mod issue;
pub mod pr;

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::Connection;

use crate::model::Issue;
use crate::model::PullRequest;
use crate::render::indexes::IndexRow;
use crate::render::indexes::LabelRow;
use crate::render::indexes::MilestoneRow;
use crate::store;

/// Grouped issue index rows: by-label, by-milestone, by-state.
type GroupedRows = (
    BTreeMap<String, Vec<IndexRow>>,
    BTreeMap<String, Vec<IndexRow>>,
    BTreeMap<String, Vec<IndexRow>>,
);

/// Filesystem-safe, deterministic slug for a label/milestone index filename.
///
/// Note: distinct label/milestone names that map to the same slug (e.g. `a b`
/// and `a-b`) will share one index file (last writer wins). Acceptable for v1 —
/// GitHub label names are typically ASCII-distinct and this collision is rare
/// in practice.
fn index_slug(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if s.is_empty() {
        s.push('_');
    }
    s
}

/// Remove a directory (if present) and recreate it empty — index dirs are fully
/// derived.
fn reset_dir(dir: &Path) -> std::io::Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::create_dir_all(dir)
}

/// Group `(entity, IndexRow)` pairs into by-label / by-milestone / by-state
/// maps. Per-entity projections are supplied by the caller so this stays
/// entity-agnostic.
fn build_groups<E>(
    pairs: &[(E, IndexRow)],
    state_key: impl Fn(&E) -> String,
    labels: impl Fn(&E) -> &[String],
    milestone: impl Fn(&E) -> Option<&str>,
) -> GroupedRows {
    let mut by_label: BTreeMap<String, Vec<IndexRow>> = BTreeMap::new();
    let mut by_milestone: BTreeMap<String, Vec<IndexRow>> = BTreeMap::new();
    let mut by_state: BTreeMap<String, Vec<IndexRow>> = BTreeMap::new();

    for (e, row) in pairs {
        for label in labels(e) {
            by_label.entry(label.clone()).or_default().push(row.clone());
        }
        if let Some(ms) = milestone(e) {
            by_milestone
                .entry(ms.to_string())
                .or_default()
                .push(row.clone());
        }
        by_state.entry(state_key(e)).or_default().push(row.clone());
    }

    (by_label, by_milestone, by_state)
}

/// Write by-label / by-milestone / by-state index tables under `dir`.
/// `states` is the fixed bucket ordering for by-state (e.g.
/// `["open","closed"]`).
fn write_indexes(dir: &Path, groups: &GroupedRows, states: &[&str]) -> std::io::Result<()> {
    let (by_label, by_milestone, by_state) = groups;

    let by_label_dir = dir.join("by-label");
    reset_dir(&by_label_dir)?;
    for (label, rows) in by_label {
        std::fs::write(
            by_label_dir.join(format!("{}.md", index_slug(label))),
            indexes::issue_table(rows),
        )?;
    }

    let by_milestone_dir = dir.join("by-milestone");
    reset_dir(&by_milestone_dir)?;
    for (ms, rows) in by_milestone {
        std::fs::write(
            by_milestone_dir.join(format!("{}.md", index_slug(ms))),
            indexes::issue_table(rows),
        )?;
    }

    let by_state_dir = dir.join("by-state");
    reset_dir(&by_state_dir)?;
    for state in states {
        let rows = by_state.get(*state).map_or(&[][..], Vec::as_slice);
        std::fs::write(
            by_state_dir.join(format!("{state}.md")),
            indexes::issue_table(rows),
        )?;
    }

    Ok(())
}

/// Remove `.md` files in `dir` not present in `expected`.
fn prune_stale_md(
    dir: &Path,
    expected: &std::collections::BTreeSet<String>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            let name = entry.file_name().to_string_lossy().to_string();
            if Path::new(&name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
                && !expected.contains(&name)
            {
                std::fs::remove_file(entry.path())?;
            }
        }
    }
    Ok(())
}

/// Write `labels.md` and `milestones.md` into `out`.
fn write_taxonomy_docs(
    conn: &Connection,
    out: &Path,
    by_milestone: &BTreeMap<String, Vec<IndexRow>>,
) -> anyhow::Result<()> {
    let usage_map: BTreeMap<String, i64> = store::taxonomy::label_usage_counts(conn)?
        .into_iter()
        .collect();
    let label_rows: Vec<LabelRow> = store::taxonomy::all_labels(conn)?
        .into_iter()
        .map(|l| LabelRow {
            count: usage_map.get(&l.name).copied().unwrap_or(0),
            name: l.name,
            color: l.color,
            description: l.description,
        })
        .collect();
    std::fs::write(out.join("labels.md"), indexes::labels_doc(&label_rows))?;

    let milestone_rows: Vec<MilestoneRow> = store::taxonomy::all_milestones(conn)?
        .into_iter()
        .map(|m| {
            let open = by_milestone
                .get(&m.title)
                .map_or(0, |rows| rows.iter().filter(|r| r.state == "open").count());
            let closed = by_milestone.get(&m.title).map_or(0, |rows| {
                rows.iter().filter(|r| r.state == "closed").count()
            });
            MilestoneRow {
                due_on: m.due_on.map(|d| d.date_naive().to_string()),
                title: m.title,
                state: m.state,
                open: i64::try_from(open).unwrap_or(i64::MAX),
                closed: i64::try_from(closed).unwrap_or(i64::MAX),
            }
        })
        .collect();
    std::fs::write(
        out.join("milestones.md"),
        indexes::milestones_doc(&milestone_rows),
    )?;

    Ok(())
}

/// Project the repo DB into a Markdown tree rooted at `out`. Non-deleted issues
/// only. Deleted issues whose files exist are removed. Writes README,
/// labels.md, milestones.md, issues/, prs/, and cross-cutting
/// by-label/by-milestone/by-state index tables.
///
/// # Errors
///
/// Returns an error if the database cannot be queried, `repo_meta` is missing,
/// or any filesystem operation fails.
pub fn render_tree(conn: &Connection, out: &Path) -> anyhow::Result<()> {
    let meta = store::repo_meta::get(conn)?.ok_or_else(|| anyhow::anyhow!("repo_meta missing"))?;
    let width = usize::try_from(meta.padding_width).unwrap_or(4);
    let issues_dir = out.join("issues");
    std::fs::create_dir_all(&issues_dir)?;

    // Render each non-deleted issue; collect (Issue, IndexRow) pairs for grouping.
    let mut expected: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut pairs: Vec<(Issue, IndexRow)> = Vec::new();
    let numbers: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT number FROM issues WHERE deleted=0 ORDER BY number")?;

        stmt.query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?
    };

    for number in numbers {
        let Some(iss) = store::issues::get_issue_by_number(conn, number)? else {
            continue;
        };
        let related = store::issues::related_numbers(conn, &iss.node_id)?;
        let comments = store::issues::list_comments(conn, &iss.node_id)?;
        let url = format!(
            "https://github.com/{}/{}/issues/{number}",
            meta.owner, meta.repo
        );
        let doc = crate::render::issue::render(&iss, &related, &comments, &url);
        let fname = format!("{number:0width$}.md");
        std::fs::write(issues_dir.join(&fname), doc)?;
        expected.insert(fname.clone());

        let row = IndexRow {
            number: iss.number,
            title: iss.title.clone(),
            state: iss.state.as_str().to_string(),
            assignees: iss.assignees.clone(),
            updated_at: iss
                .updated_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            file_rel: format!("../{fname}"),
        };
        pairs.push((iss, row));
    }

    prune_stale_md(&issues_dir, &expected)?;

    let groups = build_groups(
        &pairs,
        |i: &Issue| i.state.as_str().to_string(),
        |i: &Issue| i.labels.as_slice(),
        |i: &Issue| i.milestone.as_deref(),
    );
    write_indexes(&issues_dir, &groups, &["open", "closed"])?;
    write_taxonomy_docs(conn, out, &groups.1)?;

    render_prs(conn, out, width, &meta)?;

    // README with counts + sync metadata.
    let open: i64 = conn.query_row(
        "SELECT COUNT(*) FROM issues WHERE deleted=0 AND state='open'",
        [],
        |r| r.get(0),
    )?;
    let closed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM issues WHERE deleted=0 AND state='closed'",
        [],
        |r| r.get(0),
    )?;
    let deleted: i64 = conn.query_row("SELECT COUNT(*) FROM issues WHERE deleted=1", [], |r| {
        r.get(0)
    })?;

    let issues_state = crate::store::sync_state::get(conn, "issues")?;
    let watermark = issues_state.updated_watermark.map_or_else(
        || "never".to_string(),
        |w| w.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    );
    let run_phase = format!("{:?}", issues_state.run_phase);
    let last_full = meta
        .last_full_sync_at
        .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
        .map_or_else(
            || "never".to_string(),
            |dt: chrono::DateTime<chrono::Utc>| {
                dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            },
        );

    let (pr_open, pr_draft, pr_closed, pr_merged) = store::prs::effective_state_counts(conn)?;
    let pr_deleted: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pull_requests WHERE deleted=1",
        [],
        |r| r.get(0),
    )?;
    let prs_state = crate::store::sync_state::get(conn, "pull_requests")?;
    let pr_watermark = prs_state.updated_watermark.map_or_else(
        || "never".to_string(),
        |w| w.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    );
    let pr_phase = format!("{:?}", prs_state.run_phase);

    let readme = format!(
        "# {}/{}\n\n\
         - open issues: {open}\n- closed issues: {closed}\n- soft-deleted issues: {deleted}\n\
         - issues watermark: {watermark}\n- issues sync phase: {run_phase}\n\
         - open PRs: {pr_open}\n- draft PRs: {pr_draft}\n- closed PRs: {pr_closed}\n- merged PRs: {pr_merged}\n\
         - soft-deleted PRs: {pr_deleted}\n- PRs watermark: {pr_watermark}\n- PRs sync phase: {pr_phase}\n\
         - last full sync: {last_full}\n",
        meta.owner, meta.repo
    );
    std::fs::write(out.join("README.md"), readme)?;
    Ok(())
}

/// Project all non-deleted PRs into `<out>/prs/` (files +
/// by-state/by-label/by-milestone indexes).
///
/// # Errors
///
/// Returns an error if the database cannot be queried or any filesystem
/// operation fails.
fn render_prs(
    conn: &Connection,
    out: &Path,
    width: usize,
    meta: &store::repo_meta::RepoMeta,
) -> anyhow::Result<()> {
    let prs_dir = out.join("prs");
    std::fs::create_dir_all(&prs_dir)?;

    let mut expected: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut pairs: Vec<(PullRequest, IndexRow)> = Vec::new();

    for number in store::prs::pr_numbers(conn)? {
        let Some(pr) = store::prs::get_pr_by_number(conn, number)? else {
            continue;
        };
        let related = store::issues::related_numbers(conn, &pr.node_id)?;
        let closes = store::prs::closes_numbers(conn, &pr.node_id)?;
        let comments = store::issues::list_comments(conn, &pr.node_id)?;
        let reviews = store::prs::list_reviews(conn, &pr.node_id)?;
        let threads = store::prs::list_review_threads(conn, &pr.node_id)?;
        let url = format!(
            "https://github.com/{}/{}/pull/{number}",
            meta.owner, meta.repo
        );
        let doc =
            crate::render::pr::render(&pr, &related, &closes, &comments, &reviews, &threads, &url);
        let fname = format!("{number:0width$}.md");
        std::fs::write(prs_dir.join(&fname), doc)?;
        expected.insert(fname.clone());

        let row = IndexRow {
            number: pr.number,
            title: pr.title.clone(),
            state: pr.effective_state().as_str().to_string(),
            assignees: pr.assignees.clone(),
            updated_at: pr
                .updated_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            file_rel: format!("../{fname}"),
        };
        pairs.push((pr, row));
    }

    prune_stale_md(&prs_dir, &expected)?;

    let groups = build_groups(
        &pairs,
        |p: &PullRequest| p.effective_state().as_str().to_string(),
        |p: &PullRequest| p.labels.as_slice(),
        |p: &PullRequest| p.milestone.as_deref(),
    );
    write_indexes(&prs_dir, &groups, &["open", "draft", "closed", "merged"])?;

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

    fn issue(node: &str, num: i64, deleted: bool) -> Issue {
        Issue {
            node_id: node.into(),
            number: num,
            title: "t".into(),
            state: IssueState::Open,
            state_reason: None,
            author: None,
            body: "b".into(),
            created_at: dt("2026-01-01T00:00:00Z"),
            updated_at: dt("2026-01-01T00:00:00Z"),
            closed_at: None,
            milestone: None,
            labels: vec![],
            assignees: vec![],
            deleted,
        }
    }

    #[test]
    fn renders_taxonomy_and_indexes() {
        use crate::model::Label;
        use crate::model::Milestone;
        let conn = store::open_in_memory().unwrap();
        store::repo_meta::ensure(&conn, "o", "r").unwrap();
        store::taxonomy::replace_labels(&conn, &[Label {
            node_id: "L1".into(),
            name: "bug".into(),
            color: "f00".into(),
            description: Some("d".into()),
        }])
        .unwrap();
        store::taxonomy::replace_milestones(&conn, &[Milestone {
            node_id: "M1".into(),
            number: 1,
            title: "v1".into(),
            state: "open".into(),
            description: None,
            due_on: None,
        }])
        .unwrap();
        let mut i = issue("I1", 1, false);
        i.labels = vec!["bug".into()];
        i.milestone = Some("v1".into());
        store::issues::upsert_issue(&conn, &i).unwrap();

        let tmp = tempfile::tempdir().unwrap();
        render_tree(&conn, tmp.path()).unwrap();

        assert!(tmp.path().join("labels.md").exists());
        assert!(tmp.path().join("milestones.md").exists());
        assert!(tmp.path().join("issues/by-label/bug.md").exists());
        assert!(tmp.path().join("issues/by-milestone/v1.md").exists());
        assert!(tmp.path().join("issues/by-state/open.md").exists());

        let labels_md = std::fs::read_to_string(tmp.path().join("labels.md")).unwrap();
        assert!(labels_md.contains("bug"));
        let by_label = std::fs::read_to_string(tmp.path().join("issues/by-label/bug.md")).unwrap();
        assert!(by_label.contains("0001.md"));
    }

    #[test]
    fn renders_and_prunes_deleted() {
        let conn = store::open_in_memory().unwrap();
        store::repo_meta::ensure(&conn, "o", "r").unwrap();
        store::issues::upsert_issue(&conn, &issue("I1", 1, false)).unwrap();
        store::issues::upsert_issue(&conn, &issue("I2", 2, false)).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        render_tree(&conn, tmp.path()).unwrap();
        assert!(tmp.path().join("issues/0001.md").exists());
        assert!(tmp.path().join("issues/0002.md").exists());
        assert!(tmp.path().join("README.md").exists());

        // Now soft-delete issue 2 and re-render → file removed.
        store::issues::upsert_issue(&conn, &issue("I2", 2, true)).unwrap();
        render_tree(&conn, tmp.path()).unwrap();
        assert!(!tmp.path().join("issues/0002.md").exists());
    }

    #[test]
    fn renders_prs_subtree_and_buckets() {
        use crate::model::PullRequest;
        let conn = store::open_in_memory().unwrap();
        store::repo_meta::ensure(&conn, "o", "r").unwrap();

        let mk = |node: &str, num: i64, state: &str, merged: bool, draft: bool| PullRequest {
            node_id: node.into(),
            number: num,
            title: "t".into(),
            state: state.into(),
            is_draft: draft,
            merged,
            merged_at: None,
            merged_by: None,
            base_ref: "main".into(),
            head_ref: "f".into(),
            additions: 0,
            deletions: 0,
            changed_files: 0,
            author: None,
            body: "b".into(),
            created_at: dt("2026-01-01T00:00:00Z"),
            updated_at: dt("2026-01-01T00:00:00Z"),
            closed_at: None,
            milestone: None,
            labels: vec!["bug".into()],
            assignees: vec![],
            deleted: false,
        };
        store::prs::upsert_pull_request(&conn, &mk("P1", 10, "MERGED", true, false)).unwrap();
        store::prs::upsert_pull_request(&conn, &mk("P2", 11, "OPEN", false, true)).unwrap();
        store::prs::upsert_pull_request(&conn, &mk("P3", 12, "OPEN", false, false)).unwrap();

        let tmp = tempfile::tempdir().unwrap();
        render_tree(&conn, tmp.path()).unwrap();

        assert!(tmp.path().join("prs/0010.md").exists());
        assert!(tmp.path().join("prs/by-state/merged.md").exists());
        assert!(tmp.path().join("prs/by-state/draft.md").exists());
        assert!(tmp.path().join("prs/by-state/open.md").exists());
        assert!(tmp.path().join("prs/by-state/closed.md").exists());
        assert!(tmp.path().join("prs/by-label/bug.md").exists());

        let merged = std::fs::read_to_string(tmp.path().join("prs/by-state/merged.md")).unwrap();
        assert!(merged.contains("0010.md"));
        let draft = std::fs::read_to_string(tmp.path().join("prs/by-state/draft.md")).unwrap();
        assert!(draft.contains("0011.md"));

        let readme = std::fs::read_to_string(tmp.path().join("README.md")).unwrap();
        assert!(readme.contains("open PRs: 1"));
        assert!(readme.contains("merged PRs: 1"));
    }

    #[test]
    fn renders_and_prunes_deleted_prs() {
        use crate::model::PullRequest;
        let conn = store::open_in_memory().unwrap();
        store::repo_meta::ensure(&conn, "o", "r").unwrap();

        let pr = |node: &str, num: i64, deleted: bool| PullRequest {
            node_id: node.into(),
            number: num,
            title: "t".into(),
            state: "OPEN".into(),
            is_draft: false,
            merged: false,
            merged_at: None,
            merged_by: None,
            base_ref: "main".into(),
            head_ref: "f".into(),
            additions: 0,
            deletions: 0,
            changed_files: 0,
            author: None,
            body: "b".into(),
            created_at: dt("2026-01-01T00:00:00Z"),
            updated_at: dt("2026-01-01T00:00:00Z"),
            closed_at: None,
            milestone: None,
            labels: vec![],
            assignees: vec![],
            deleted,
        };
        store::prs::upsert_pull_request(&conn, &pr("P1", 1, false)).unwrap();
        store::prs::upsert_pull_request(&conn, &pr("P2", 2, false)).unwrap();

        let tmp = tempfile::tempdir().unwrap();
        render_tree(&conn, tmp.path()).unwrap();
        assert!(tmp.path().join("prs/0001.md").exists());
        assert!(tmp.path().join("prs/0002.md").exists());

        // Now soft-delete PR 2 and re-render → file removed, PR 1 remains.
        store::prs::upsert_pull_request(&conn, &pr("P2", 2, true)).unwrap();
        render_tree(&conn, tmp.path()).unwrap();
        assert!(!tmp.path().join("prs/0002.md").exists());
        assert!(tmp.path().join("prs/0001.md").exists());
    }
}
