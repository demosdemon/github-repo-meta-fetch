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

    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    render::render_tree(&conn, a.path()).unwrap();
    render::render_tree(&conn, b.path()).unwrap();

    // Issues subtree is byte-identical.
    let fa = std::fs::read_to_string(a.path().join("issues/0001.md")).unwrap();
    let fb = std::fs::read_to_string(b.path().join("issues/0001.md")).unwrap();
    assert_eq!(fa, fb);

    // PRs subtree is byte-identical.
    let pa = std::fs::read_to_string(a.path().join("prs/0001.md")).unwrap();
    let pb = std::fs::read_to_string(b.path().join("prs/0001.md")).unwrap();
    assert_eq!(pa, pb);

    // README is byte-identical.
    let ra = std::fs::read_to_string(a.path().join("README.md")).unwrap();
    let rb = std::fs::read_to_string(b.path().join("README.md")).unwrap();
    assert_eq!(ra, rb);
    // Confirm PR counts appear in the README.
    assert!(ra.contains("open PRs: 1"), "README missing open PRs count");
}
