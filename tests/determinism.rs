#![allow(clippy::unwrap_used)]

use chrono::DateTime;
use chrono::Utc;
use github_repo_meta_fetch::model::Issue;
use github_repo_meta_fetch::model::IssueState;
use github_repo_meta_fetch::model::PullRequest;
use github_repo_meta_fetch::render;
use github_repo_meta_fetch::store;

fn dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

/// Collect every file under `root` as (relative path, contents).
fn tree_files(root: &std::path::Path) -> std::collections::BTreeMap<String, String> {
    fn walk(
        root: &std::path::Path,
        dir: &std::path::Path,
        out: &mut std::collections::BTreeMap<String, String>,
    ) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                out.insert(rel, std::fs::read_to_string(&path).unwrap());
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn sample_pr() -> PullRequest {
    PullRequest {
        node_id: "PR_1".into(),
        number: 1,
        title: "deterministic pr".into(),
        state: "OPEN".into(),
        is_draft: false,
        merged: false,
        merged_at: None,
        merged_by: None,
        base_ref: "main".into(),
        head_ref: "feature".into(),
        additions: 10,
        deletions: 2,
        changed_files: 3,
        author: Some("octocat".into()),
        body: "pr body content".into(),
        created_at: dt("2026-01-01T00:00:00Z"),
        updated_at: dt("2026-01-02T00:00:00Z"),
        closed_at: None,
        milestone: None,
        labels: vec!["bug".into()],
        assignees: vec![],
        deleted: false,
    }
}

#[test]
fn rendering_twice_is_byte_identical() {
    let conn = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn, "o", "r").unwrap();
    let i = Issue {
        node_id: "I1".into(),
        number: 1,
        title: "t".into(),
        state: IssueState::Open,
        state_reason: None,
        author: Some("a".into()),
        body: "body".into(),
        created_at: dt("2026-01-01T00:00:00Z"),
        updated_at: dt("2026-01-02T00:00:00Z"),
        closed_at: None,
        milestone: None,
        labels: vec!["bug".into()],
        assignees: vec![],
        deleted: false,
    };
    store::issues::upsert_issue(&conn, &i).unwrap();
    store::prs::upsert_pull_request(&conn, &sample_pr()).unwrap();

    let child = Issue {
        node_id: "I2".into(),
        number: 2,
        ..i.clone()
    };
    store::issues::upsert_issue(&conn, &child).unwrap();
    let ep = |node: &str, number: i64| github_repo_meta_fetch::model::RelEndpoint {
        node_id: node.into(),
        repo: None,
        number,
        state: IssueState::Open,
        title: "t".into(),
    };
    store::relationships::replace_incident_edges(&conn, "I1", &[
        github_repo_meta_fetch::model::RelEdge {
            rel: github_repo_meta_fetch::model::RelKind::Parent,
            src: ep("I1", 1),
            dst: ep("I2", 2),
            position: Some(0),
        },
        github_repo_meta_fetch::model::RelEdge {
            rel: github_repo_meta_fetch::model::RelKind::Blocks,
            src: ep("I2", 2),
            dst: ep("I1", 1),
            position: None,
        },
    ])
    .unwrap();

    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    render::render_tree(&conn, a.path()).unwrap();
    render::render_tree(&conn, b.path()).unwrap();

    let fa = tree_files(a.path());
    let fb = tree_files(b.path());
    assert_eq!(fa, fb, "full output trees must be byte-identical");
    assert!(
        fa.contains_key("hierarchy.md"),
        "hierarchy.md must be rendered"
    );
    assert!(fa.contains_key("CLAUDE.md"), "CLAUDE.md must be rendered");
    assert!(
        fa["CLAUDE.md"].contains("o/r"),
        "CLAUDE.md must name the repository it projects"
    );
    assert!(
        fa["issues/0001.md"].contains("blocked: 1"),
        "relationship keys must appear in frontmatter"
    );
    assert!(
        fa["README.md"].contains("open PRs: 1"),
        "README missing open PRs count"
    );
}
