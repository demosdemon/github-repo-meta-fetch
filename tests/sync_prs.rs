#![allow(clippy::unwrap_used)]
use github_repo_meta_fetch::github::GithubClient;
use github_repo_meta_fetch::store;
use github_repo_meta_fetch::sync;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_string_contains;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn pr_rows(conn: &rusqlite::Connection) -> Vec<(i64, String, bool, bool)> {
    let mut stmt = conn
        .prepare("SELECT number, title, merged, deleted FROM pull_requests ORDER BY number")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn review_rows(conn: &rusqlite::Connection) -> Vec<(String, String)> {
    let mut stmt = conn
        .prepare("SELECT node_id, pr_node_id FROM reviews ORDER BY node_id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn thread_rows(conn: &rusqlite::Connection) -> Vec<(String, String)> {
    let mut stmt = conn
        .prepare("SELECT node_id, pr_node_id FROM review_threads ORDER BY node_id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn review_comment_rows(conn: &rusqlite::Connection) -> Vec<(String, String)> {
    let mut stmt = conn
        .prepare("SELECT node_id, thread_node_id FROM review_comments ORDER BY node_id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn comment_rows(conn: &rusqlite::Connection) -> Vec<(String, String)> {
    let mut stmt = conn
        .prepare("SELECT node_id, subject_node_id FROM comments ORDER BY node_id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn rl(t: ResponseTemplate) -> ResponseTemplate {
    t.insert_header("x-ratelimit-resource", "graphql")
        .insert_header("x-ratelimit-limit", "5000")
        .insert_header("x-ratelimit-remaining", "4990")
        .insert_header("x-ratelimit-used", "10")
        .insert_header("x-ratelimit-reset", "1781564821")
}

fn pr_node(num: i64, updated: &str) -> String {
    format!(
        r#"{{"id":"PR_{num}","number":{num},"title":"t{num}","body":"b","state":"OPEN",
        "isDraft":false,"merged":false,"mergedAt":null,"mergedBy":null,
        "baseRefName":"main","headRefName":"f{num}","additions":1,"deletions":0,"changedFiles":1,
        "createdAt":"2026-01-01T00:00:00Z","updatedAt":"{updated}","closedAt":null,
        "author":{{"__typename":"User","login":"o"}},"milestone":null,
        "labels":{{"nodes":[]}},"assignees":{{"nodes":[]}},
        "comments":{{"totalCount":0,"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[]}},
        "reviews":{{"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[]}},
        "reviewThreads":{{"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[]}},
        "closingIssuesReferences":{{"nodes":[]}}}}"#
    )
}

fn page(node: &str, has_next: bool, end: &str) -> String {
    let cursor = if has_next {
        format!("\"{end}\"")
    } else {
        "null".into()
    };
    format!(
        r#"{{"data":{{"repository":{{"pullRequests":{{"pageInfo":{{"hasNextPage":{has_next},"endCursor":{cursor}}},"nodes":[{node}]}}}}}}}}"#
    )
}

fn client_for(server: &MockServer) -> GithubClient {
    let octo = octocrab::Octocrab::builder()
        .base_uri(server.uri())
        .unwrap()
        .personal_token("t".to_string())
        .build()
        .unwrap();
    GithubClient::new(octo)
}

#[tokio::test]
async fn paginates_prs_three_pages() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("\"cursor\":\"C2\""))
        .respond_with(rl(ResponseTemplate::new(200).set_body_string(page(
            &pr_node(1, "2026-01-01T00:00:00Z"),
            false,
            "",
        ))))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("\"cursor\":\"C1\""))
        .respond_with(rl(ResponseTemplate::new(200).set_body_string(page(
            &pr_node(2, "2026-02-01T00:00:00Z"),
            true,
            "C2",
        ))))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(rl(ResponseTemplate::new(200).set_body_string(page(
            &pr_node(3, "2026-03-01T00:00:00Z"),
            true,
            "C1",
        ))))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let conn = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn, "o", "r").unwrap();

    let stop = sync::prs::sync_prs(&client, &conn, "o", "r", false, |_h| true)
        .await
        .unwrap();
    assert!(matches!(stop, sync::issues::SyncStop::Completed));
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM pull_requests", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 3);
    let s = store::sync_state::get(&conn, "pull_requests").unwrap();
    assert_eq!(s.run_phase, store::sync_state::RunPhase::Done);
}

#[tokio::test]
async fn pauses_per_pr_when_budget_low() {
    // Single page with two PRs; budget_ok returns false after the first PR.
    let server = MockServer::start().await;
    let two = format!(
        "{},{}",
        pr_node(1, "2026-02-01T00:00:00Z"),
        pr_node(2, "2026-01-01T00:00:00Z")
    );
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(rl(
            ResponseTemplate::new(200).set_body_string(page(&two, false, ""))
        ))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let conn = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn, "o", "r").unwrap();

    let mut checks = 0;
    let stop = sync::prs::sync_prs(&client, &conn, "o", "r", false, |_h| {
        checks += 1;
        checks > 1 // true once we've passed the first PR; false on the first
        // check → pause after PR #1
    })
    .await
    .unwrap();
    assert!(matches!(stop, sync::issues::SyncStop::Paused));
    // Exactly one PR persisted; cursor NOT advanced (resume refetches the page).
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM pull_requests", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    let s = store::sync_state::get(&conn, "pull_requests").unwrap();
    assert_eq!(s.resume_cursor, None);
    // First-page mid-page pause writes no checkpoint (set_cursor runs only after
    // the node loop), so the state stays Idle — and crucially must NOT be Done.
    assert_ne!(s.run_phase, store::sync_state::RunPhase::Done);
}

#[tokio::test]
async fn only_issues_skips_pr_queries() {
    use github_repo_meta_fetch::config::Reserve;
    use github_repo_meta_fetch::ratelimit::store::RateLimitStore;
    use github_repo_meta_fetch::sync;
    let server = MockServer::start().await;

    // Labels/milestones (taxonomy always runs) — minimal 200s.
    Mock::given(method("GET"))
        .and(path("/repos/o/r/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/milestones"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server)
        .await;

    // Issues: one empty page.
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("issues("))
        .respond_with(rl(ResponseTemplate::new(200).set_body_string(
            r#"{"data":{"repository":{"issues":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}}}}"#,
        )))
        .mount(&server)
        .await;

    // PRs: must NOT be called.
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("pullRequests("))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let conn = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn, "o", "r").unwrap();
    let mut rl_store = RateLimitStore::open_in_memory("fp").unwrap();
    let mut syncer = sync::Syncer {
        client: &client,
        conn: &conn,
        rl: &mut rl_store,
        reserve: Reserve::default(),
        cost_ceiling: None,
        no_wait: true,
        max_wait: None,
        full: false,
        only: vec![sync::OnlyTarget::Issues],
    };
    let out = syncer.run("o", "r").await.unwrap();
    assert_eq!(out, sync::Outcome::Completed);
    server.verify().await; // the expect(0) PR mock holds
}

#[tokio::test]
async fn pr_comment_drain_persists_second_page() {
    let server = MockServer::start().await;
    // Follow-up comments page (PullRequest node) — mount first (more specific).
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("\"cursor\":\"CMORE\""))
        .respond_with(rl(ResponseTemplate::new(200).set_body_string(
            r#"{"data":{"node":{"__typename":"PullRequest","comments":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{"id":"C2","createdAt":"2026-01-02T00:00:00Z","body":"second","author":{"__typename":"User","login":"o"}}]}}}}"#,
        )))
        .mount(&server)
        .await;
    // PR page with an inline first comment + hasNextPage.
    let node = r#"{"id":"PR_1","number":1,"title":"t","body":"b","state":"OPEN","isDraft":false,"merged":false,"mergedAt":null,"mergedBy":null,"baseRefName":"main","headRefName":"f","additions":0,"deletions":0,"changedFiles":0,"createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z","closedAt":null,"author":{"__typename":"User","login":"o"},"milestone":null,"labels":{"nodes":[]},"assignees":{"nodes":[]},"comments":{"totalCount":2,"pageInfo":{"hasNextPage":true,"endCursor":"CMORE"},"nodes":[{"id":"C1","createdAt":"2026-01-01T00:00:00Z","body":"first","author":{"__typename":"User","login":"o"}}]},"reviews":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]},"reviewThreads":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]},"closingIssuesReferences":{"nodes":[]}}"#;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("pullRequests("))
        .respond_with(rl(
            ResponseTemplate::new(200).set_body_string(page(node, false, ""))
        ))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let conn = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn, "o", "r").unwrap();
    sync::prs::sync_prs(&client, &conn, "o", "r", false, |_h| true)
        .await
        .unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM comments WHERE subject_node_id='PR_1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 2, "both inline and drained PR comments must persist");
}

/// Mount two PR pages on `server`:
///   page 1 (no cursor)   -> PR #2 (updated 2026-02-01), hasNextPage=true,
/// cursor "C1"   page 2 (cursor "C1") -> PR #1 (updated 2026-01-01),
/// hasNextPage=false Matchers allow repeats so a resumed run sees the same
/// responses.
async fn mount_two_pr_pages(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("\"cursor\":\"C1\""))
        .respond_with(rl(ResponseTemplate::new(200).set_body_string(page(
            &pr_node(1, "2026-01-01T00:00:00Z"),
            false,
            "",
        ))))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(rl(ResponseTemplate::new(200).set_body_string(page(
            &pr_node(2, "2026-02-01T00:00:00Z"),
            true,
            "C1",
        ))))
        .mount(server)
        .await;
}

#[tokio::test]
async fn resume_matches_uninterrupted_prs() {
    use sync::issues::SyncStop;

    // Interrupted then resumed run
    let server_a = MockServer::start().await;
    mount_two_pr_pages(&server_a).await;
    let client_a = client_for(&server_a);
    let conn_a = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn_a, "o", "r").unwrap();

    // budget_ok returns false after the FIRST PR (per-PR gate).
    // The closure is called once per PR; returning false on the first call
    // pauses after PR #2 (first page, first PR) without advancing the cursor.
    let mut call_count = 0usize;
    let stop = sync::prs::sync_prs(&client_a, &conn_a, "o", "r", false, |_h| {
        call_count += 1;
        call_count > 1
    })
    .await
    .unwrap();
    assert!(matches!(stop, SyncStop::Paused));
    let n_after_pause: i64 = conn_a
        .query_row("SELECT COUNT(*) FROM pull_requests", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n_after_pause, 1);
    let s = store::sync_state::get(&conn_a, "pull_requests").unwrap();
    assert_ne!(
        s.run_phase,
        store::sync_state::RunPhase::Done,
        "pause must not complete the run"
    );
    assert_eq!(
        s.resume_cursor, None,
        "cursor must not advance on a mid-page (first-page) pause"
    );

    // Resume to completion (budget always ok).
    let stop = sync::prs::sync_prs(&client_a, &conn_a, "o", "r", false, |_h| true)
        .await
        .unwrap();
    assert!(matches!(stop, SyncStop::Completed));

    // Uninterrupted run
    let server_b = MockServer::start().await;
    mount_two_pr_pages(&server_b).await;
    let client_b = client_for(&server_b);
    let conn_b = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn_b, "o", "r").unwrap();
    let stop = sync::prs::sync_prs(&client_b, &conn_b, "o", "r", false, |_h| true)
        .await
        .unwrap();
    assert!(matches!(stop, SyncStop::Completed));

    // Byte-identity: all five PR-related tables must match
    assert_eq!(pr_rows(&conn_a), vec![
        (1, "t1".to_string(), false, false),
        (2, "t2".to_string(), false, false),
    ]);
    assert_eq!(pr_rows(&conn_a), pr_rows(&conn_b));
    assert_eq!(review_rows(&conn_a), review_rows(&conn_b));
    assert_eq!(thread_rows(&conn_a), thread_rows(&conn_b));
    assert_eq!(review_comment_rows(&conn_a), review_comment_rows(&conn_b));
    assert_eq!(comment_rows(&conn_a), comment_rows(&conn_b));
}
