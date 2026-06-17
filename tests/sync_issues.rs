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

fn issue_node(num: i64, updated: &str) -> String {
    format!(
        r#"{{"id":"I_{num}","number":{num},"title":"t{num}","body":"b","state":"OPEN","stateReason":null,
      "createdAt":"2026-01-01T00:00:00Z","updatedAt":"{updated}","closedAt":null,
      "author":{{"__typename":"User","login":"o"}},"milestone":null,"labels":{{"nodes":[]}},"assignees":{{"nodes":[]}},
      "comments":{{"totalCount":0,"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[]}},
      "timelineItems":{{"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[]}}}}"#
    )
}
fn page(node: &str, has_next: bool, end: &str) -> String {
    let cursor = if has_next {
        format!("\"{end}\"")
    } else {
        "null".to_string()
    };
    format!(
        r#"{{"data":{{"repository":{{"issues":{{"pageInfo":{{"hasNextPage":{has_next},"endCursor":{cursor}}},"nodes":[{node}]}}}}}}}}"#
    )
}
fn rl_headers(t: ResponseTemplate) -> ResponseTemplate {
    t.insert_header("x-ratelimit-resource", "graphql")
        .insert_header("x-ratelimit-limit", "5000")
        .insert_header("x-ratelimit-remaining", "4990")
        .insert_header("x-ratelimit-used", "10")
        .insert_header("x-ratelimit-reset", "1781564821")
}

/// GraphQL rate-limit headers with explicit remaining/used (limit fixed at
/// 5000).
fn rl_headers_at(t: ResponseTemplate, remaining: u64, used: u64) -> ResponseTemplate {
    t.insert_header("x-ratelimit-resource", "graphql")
        .insert_header("x-ratelimit-limit", "5000")
        .insert_header("x-ratelimit-remaining", remaining.to_string())
        .insert_header("x-ratelimit-used", used.to_string())
        .insert_header("x-ratelimit-reset", "1781564821")
}

#[tokio::test]
async fn paginates_three_pages() {
    let server = MockServer::start().await;

    // Distinguish pages by the `after` cursor in the request body so ordering is
    // deterministic regardless of wiremock's matcher evaluation order:
    //   page 1 -> no cursor (request omits "after"/null cursor)
    //   page 2 -> cursor "C1"
    //   page 3 -> cursor "C2"
    // Mount the more-specific (cursor-bearing) matchers first so they win.
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("\"cursor\":\"C2\""))
        .respond_with(rl_headers(ResponseTemplate::new(200).set_body_string(
            page(&issue_node(1, "2026-01-01T00:00:00Z"), false, ""),
        )))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("\"cursor\":\"C1\""))
        .respond_with(rl_headers(ResponseTemplate::new(200).set_body_string(
            page(&issue_node(2, "2026-02-01T00:00:00Z"), true, "C2"),
        )))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(rl_headers(ResponseTemplate::new(200).set_body_string(
            page(&issue_node(3, "2026-03-01T00:00:00Z"), true, "C1"),
        )))
        .mount(&server)
        .await;

    let octo = octocrab::Octocrab::builder()
        .base_uri(server.uri())
        .unwrap()
        .personal_token("t".to_string())
        .build()
        .unwrap();
    let client = GithubClient::new(octo);
    let conn = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn, "o", "r").unwrap();

    let stop = sync::issues::sync_issues(&client, &conn, "o", "r", false, |_h| true)
        .await
        .unwrap();
    assert!(matches!(stop, sync::issues::SyncStop::Completed));
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM issues", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 3);
    let s = store::sync_state::get(&conn, "issues").unwrap();
    assert_eq!(s.run_phase, store::sync_state::RunPhase::Done);
    assert_eq!(
        s.updated_watermark
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "2026-01-01T00:00:00Z"
    );
}

fn issue_rows(conn: &rusqlite::Connection) -> Vec<(i64, String)> {
    let mut stmt = conn
        .prepare("SELECT number, title FROM issues ORDER BY number")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
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

/// Mount the three issue pages on `server`, distinguished by request cursor:
///   page 1 -> no cursor (number 3, next "C1")
///   page 2 -> cursor "C1" (number 2, next "C2")
///   page 3 -> cursor "C2" (number 1, last)
/// Matchers allow repeats (no `up_to_n_times`) so a resumed run that
/// re-requests pages 2 and 3 from the saved checkpoint still gets the right
/// responses.
async fn mount_three_pages(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("\"cursor\":\"C2\""))
        .respond_with(rl_headers(ResponseTemplate::new(200).set_body_string(
            page(&issue_node(1, "2026-01-01T00:00:00Z"), false, ""),
        )))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("\"cursor\":\"C1\""))
        .respond_with(rl_headers(ResponseTemplate::new(200).set_body_string(
            page(&issue_node(2, "2026-02-01T00:00:00Z"), true, "C2"),
        )))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(rl_headers(ResponseTemplate::new(200).set_body_string(
            page(&issue_node(3, "2026-03-01T00:00:00Z"), true, "C1"),
        )))
        .mount(server)
        .await;
}

#[tokio::test]
async fn checkpoint_then_resume_matches_uninterrupted() {
    use github_repo_meta_fetch::sync::issues::SyncStop;

    // Interrupted then resumed run
    let server_a = MockServer::start().await;
    mount_three_pages(&server_a).await;
    let client_a = client_for(&server_a);
    let conn_a = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn_a, "o", "r").unwrap();

    // budget_ok returns false after the FIRST page so the run pauses with a
    // checkpoint at cursor "C1".
    // `budget_ok` is invoked between pages; returning false on the first such
    // check pauses the run after page 1 (checkpoint saved at cursor "C1").
    let stop = sync::issues::sync_issues(&client_a, &conn_a, "o", "r", false, |_h| false)
        .await
        .unwrap();
    assert!(matches!(stop, SyncStop::Paused));
    assert_eq!(
        store::sync_state::get(&conn_a, "issues").unwrap().run_phase,
        store::sync_state::RunPhase::Paginating
    );
    let n_after_pause: i64 = conn_a
        .query_row("SELECT COUNT(*) FROM issues", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n_after_pause, 1);

    // Resume from the saved checkpoint cursor; this re-requests pages 2 and 3.
    let stop = sync::issues::sync_issues(&client_a, &conn_a, "o", "r", false, |_h| true)
        .await
        .unwrap();
    assert!(matches!(stop, SyncStop::Completed));

    // Uninterrupted run
    let server_b = MockServer::start().await;
    mount_three_pages(&server_b).await;
    let client_b = client_for(&server_b);
    let conn_b = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn_b, "o", "r").unwrap();
    let stop = sync::issues::sync_issues(&client_b, &conn_b, "o", "r", false, |_h| true)
        .await
        .unwrap();
    assert!(matches!(stop, SyncStop::Completed));

    // Equivalence: the two DBs' issue tables are identical
    let rows_a = issue_rows(&conn_a);
    let rows_b = issue_rows(&conn_b);
    assert_eq!(rows_a, vec![
        (1, "t1".to_string()),
        (2, "t2".to_string()),
        (3, "t3".to_string())
    ]);
    assert_eq!(rows_a, rows_b);
}

#[tokio::test]
async fn syncer_pauses_on_low_budget_no_wait() {
    use github_repo_meta_fetch::config::Reserve;
    use github_repo_meta_fetch::ratelimit::store::RateLimitStore;
    use github_repo_meta_fetch::sync::Outcome;
    use github_repo_meta_fetch::sync::Syncer;
    let server = MockServer::start().await;
    // labels + milestones: empty 200
    for p in ["/repos/o/r/labels", "/repos/o/r/milestones"] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
            .mount(&server)
            .await;
    }
    // one issue page with hasNextPage=true and LOW remaining headers
    // (remaining=100, limit=5000)
    let low = ResponseTemplate::new(200)
        .insert_header("x-ratelimit-resource", "graphql")
        .insert_header("x-ratelimit-limit", "5000")
        .insert_header("x-ratelimit-remaining", "100")
        .insert_header("x-ratelimit-used", "4900")
        .insert_header("x-ratelimit-reset", "1781564821")
        .set_body_string(page(&issue_node(5, "2026-05-01T00:00:00Z"), true, "NEXT"));
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(low)
        .mount(&server)
        .await;

    let client = client_for(&server);
    let conn = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn, "o", "r").unwrap();
    let mut rl = RateLimitStore::open_in_memory("fp").unwrap();
    let mut syncer = Syncer {
        client: &client,
        conn: &conn,
        rl: &mut rl,
        reserve: Reserve::Percent(0.10),
        cost_ceiling: Some(30),
        no_wait: true,
        max_wait: None,
        full: false,
        only: Vec::new(),
    };
    let outcome = syncer.run("o", "r").await.unwrap();
    assert_eq!(outcome, Outcome::Paused);
    // Exercises the LIVE try_reserve path (not the old decide path): budget_ok
    // feeds the estimator the header used-delta, record()s remaining=100, then
    // try_reserve(floor=500, est=30) sees 100-30=70 < 500 -> Ok(false) -> pause.
    // page 1 was persisted before the pause; floor 500 > remaining 100-30
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM issues", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(
        store::sync_state::get(&conn, "issues").unwrap().run_phase,
        store::sync_state::RunPhase::Paginating
    );
}

/// The estimator's `observe` (driven by header used-deltas) must feed the live
/// `try_reserve` gate. Here page 1 is cheap, then page 2's large used-delta
/// drives the EWMA estimate well above the flat per-type ceiling (30). At the
/// gate after page 2, remaining=550 with floor=500: the OLD flat-30 `decide`
/// path would have proceeded (550-30=520 >= 500), but the estimator-driven
/// `try_reserve` (est ~= 1305) pauses (550-1305 saturates to 0 < 500). The
/// pause therefore proves the used-delta -> `observe` -> `estimate` ->
/// `try_reserve` wiring is live.
#[tokio::test]
async fn syncer_pause_is_driven_by_estimator_used_delta() {
    use github_repo_meta_fetch::config::Reserve;
    use github_repo_meta_fetch::ratelimit::store::RateLimitStore;
    use github_repo_meta_fetch::sync::Outcome;
    use github_repo_meta_fetch::sync::Syncer;
    let server = MockServer::start().await;
    for p in ["/repos/o/r/labels", "/repos/o/r/milestones"] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
            .mount(&server)
            .await;
    }
    // page 2 (cursor "C1"): expensive used-delta (used jumps 100 -> 4450) and a
    // remaining (550) that only a large estimate can breach against floor 500.
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("\"cursor\":\"C1\""))
        .respond_with(rl_headers_at(
            ResponseTemplate::new(200).set_body_string(page(
                &issue_node(2, "2026-02-01T00:00:00Z"),
                true,
                "C2",
            )),
            550,
            4450,
        ))
        .mount(&server)
        .await;
    // page 1 (no cursor): cheap; plenty of remaining so the gate proceeds.
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(rl_headers_at(
            ResponseTemplate::new(200).set_body_string(page(
                &issue_node(3, "2026-03-01T00:00:00Z"),
                true,
                "C1",
            )),
            4900,
            100,
        ))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let conn = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn, "o", "r").unwrap();
    let mut rl = RateLimitStore::open_in_memory("fp").unwrap();
    let mut syncer = Syncer {
        client: &client,
        conn: &conn,
        rl: &mut rl,
        reserve: Reserve::Percent(0.10),
        cost_ceiling: None,
        no_wait: true,
        max_wait: None,
        full: false,
        only: Vec::new(),
    };
    let outcome = syncer.run("o", "r").await.unwrap();
    assert_eq!(outcome, Outcome::Paused);
    // Both pages persisted before the pause (gate runs AFTER each page).
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM issues", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2);
}

#[tokio::test]
async fn full_reconcile_marks_missing_issue_deleted() {
    use github_repo_meta_fetch::model::Issue;
    use github_repo_meta_fetch::model::IssueState;
    use github_repo_meta_fetch::sync::issues::SyncStop;

    // Pre-seed the DB with a stale issue (#99) that the server will NOT return.
    let conn = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn, "o", "r").unwrap();
    let stale = Issue {
        node_id: "I_99".into(),
        number: 99,
        title: "old".into(),
        state: IssueState::Open,
        state_reason: None,
        author: None,
        body: String::new(),
        created_at: chrono::DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        updated_at: chrono::DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        closed_at: None,
        milestone: None,
        labels: vec![],
        assignees: vec![],
        deleted: false,
    };
    store::issues::upsert_issue(&conn, &stale).unwrap();

    // Server returns a single page with only issue #1.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(rl_headers(ResponseTemplate::new(200).set_body_string(
            page(&issue_node(1, "2026-01-01T00:00:00Z"), false, ""),
        )))
        .mount(&server)
        .await;
    let octo = octocrab::Octocrab::builder()
        .base_uri(server.uri())
        .unwrap()
        .personal_token("t".to_string())
        .build()
        .unwrap();
    let client = GithubClient::new(octo);

    // FULL run, fresh (no resume cursor), completes uninterrupted.
    let stop = sync::issues::sync_issues(&client, &conn, "o", "r", true, |_h| true)
        .await
        .unwrap();
    assert!(matches!(stop, SyncStop::Completed));
    // #1 present & not deleted; #99 now soft-deleted.
    assert!(
        !store::issues::get_issue_by_number(&conn, 1)
            .unwrap()
            .unwrap()
            .deleted
    );
    assert!(
        store::issues::get_issue_by_number(&conn, 99)
            .unwrap()
            .unwrap()
            .deleted
    );
}
