#![allow(clippy::unwrap_used)]

use assert_cmd::Command;
use chrono::DateTime;
use chrono::Utc;
use github_repo_meta_fetch::model::Issue;
use github_repo_meta_fetch::model::IssueState;
use github_repo_meta_fetch::model::PullRequest;
use github_repo_meta_fetch::store;
use predicates::str::contains;

fn dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn seed_db(path: &std::path::Path) {
    let conn = store::open(path).unwrap();
    store::repo_meta::ensure(&conn, "octocat", "hello").unwrap();
    let i = Issue {
        node_id: "I1".into(),
        number: 1,
        title: "t".into(),
        state: IssueState::Open,
        state_reason: None,
        author: Some("a".into()),
        body: "b".into(),
        created_at: dt("2026-01-01T00:00:00Z"),
        updated_at: dt("2026-01-02T00:00:00Z"),
        closed_at: None,
        milestone: None,
        labels: vec![],
        assignees: vec![],
        deleted: false,
    };
    store::issues::upsert_issue(&conn, &i).unwrap();
}

fn seed_db_with_pr(path: &std::path::Path) {
    seed_db(path);
    let conn = store::open(path).unwrap();
    let pr = PullRequest {
        node_id: "PR1".into(),
        number: 1,
        title: "First PR".into(),
        state: "OPEN".into(),
        is_draft: false,
        merged: false,
        merged_at: None,
        merged_by: None,
        base_ref: "main".into(),
        head_ref: "feature".into(),
        additions: 5,
        deletions: 1,
        changed_files: 2,
        author: Some("octocat".into()),
        body: "pr body".into(),
        created_at: dt("2026-01-01T00:00:00Z"),
        updated_at: dt("2026-01-02T00:00:00Z"),
        closed_at: None,
        milestone: None,
        labels: vec![],
        assignees: vec![],
        deleted: false,
    };
    store::prs::upsert_pull_request(&conn, &pr).unwrap();
}

#[test]
fn render_from_db_writes_tree() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("c.sqlite3");
    seed_db(&db);
    let out = dir.path().join("out");
    Command::cargo_bin("meta-fetch")
        .unwrap()
        .args([
            "render",
            "--db",
            db.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(out.join("issues/0001.md").exists());
    assert!(out.join("README.md").exists());
}

#[test]
fn status_from_db_prints_slug() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("c.sqlite3");
    seed_db(&db);
    Command::cargo_bin("meta-fetch")
        .unwrap()
        .args(["status", "--db", db.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("octocat/hello"));
}

#[test]
fn status_prints_pr_counts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("c.sqlite3");
    seed_db_with_pr(&db);
    Command::cargo_bin("meta-fetch")
        .unwrap()
        .args(["status", "--db", db.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("prs:"));
}

#[test]
fn render_writes_prs_tree() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("c.sqlite3");
    seed_db_with_pr(&db);
    let out = dir.path().join("out");
    Command::cargo_bin("meta-fetch")
        .unwrap()
        .args([
            "render",
            "--db",
            db.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(out.join("prs/0001.md").exists());
}
