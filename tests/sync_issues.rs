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
      "parent":null,
      "subIssues":{{"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[]}},
      "blockedBy":{{"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[]}},
      "blocking":{{"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[]}},
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
/// A `WalkCtx` pinned to a fixed instant, for tests that call a phase
/// function directly.
fn walk_ctx<'a>(
    client: &'a GithubClient,
    conn: &'a rusqlite::Connection,
    clock: &'a github_repo_meta_fetch::clock::FixedClock,
) -> sync::WalkCtx<'a> {
    sync::WalkCtx {
        client,
        conn,
        owner: "o",
        repo: "r",
        clock,
    }
}

/// The instant every direct-call test pins its clock to.
fn test_clock() -> github_repo_meta_fetch::clock::FixedClock {
    github_repo_meta_fetch::clock::FixedClock(
        chrono::DateTime::parse_from_rfc3339("2026-07-20T09:31:52Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
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

    let clk = test_clock();
    let ctx = walk_ctx(&client, &conn, &clk);
    let mut seen = std::collections::HashSet::new();
    let stop = sync::issues::sync_issues(&ctx, false, &mut seen, |_h| true)
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
        "2026-03-01T00:00:00Z"
    );
}

/// The watermark is the early-stop floor for the *next* run, so a completed
/// pass must stamp the newest `updatedAt` it saw. Stamping the oldest makes the
/// floor a no-op: every item still satisfies `updated_at >= watermark`, so the
/// following run re-walks the whole repo and rewrites the same value forever.
#[tokio::test]
async fn completed_run_stamps_newest_updated_at_and_next_run_early_stops() {
    let server = MockServer::start().await;

    // Pages descend by updatedAt: 2026-03-01, 2026-02-01, 2026-01-01.
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

    let client = client_for(&server);
    let conn = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn, "o", "r").unwrap();
    let clk = test_clock();
    let ctx = walk_ctx(&client, &conn, &clk);

    let mut seen = std::collections::HashSet::new();
    sync::issues::sync_issues(&ctx, false, &mut seen, |_h| true)
        .await
        .unwrap();

    let s = store::sync_state::get(&conn, "issues").unwrap();
    assert_eq!(
        s.updated_watermark
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "2026-03-01T00:00:00Z",
        "watermark must be the newest synced item, not the oldest"
    );

    // A second incremental run must early-stop instead of re-walking all three
    // pages. The floor is strict (`updated_at < watermark`), so page 1's item —
    // sitting exactly at the watermark — is re-synced, and the crossing is only
    // detected one page in: two requests, not the full three.
    let before = server.received_requests().await.unwrap().len();
    let mut seen = std::collections::HashSet::new();
    sync::issues::sync_issues(&ctx, false, &mut seen, |_h| true)
        .await
        .unwrap();
    let after = server.received_requests().await.unwrap().len();
    assert_eq!(
        after - before,
        2,
        "second run must early-stop on the watermark, not re-walk every page"
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
    let clk_a = test_clock();
    let ctx_a = walk_ctx(&client_a, &conn_a, &clk_a);

    // budget_ok returns false after the FIRST page so the run pauses with a
    // checkpoint at cursor "C1".
    // `budget_ok` is invoked between pages; returning false on the first such
    // check pauses the run after page 1 (checkpoint saved at cursor "C1").
    let mut seen = std::collections::HashSet::new();
    let stop = sync::issues::sync_issues(&ctx_a, false, &mut seen, |_h| false)
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
    let mut seen = std::collections::HashSet::new();
    let stop = sync::issues::sync_issues(&ctx_a, false, &mut seen, |_h| true)
        .await
        .unwrap();
    assert!(matches!(stop, SyncStop::Completed));

    // Uninterrupted run
    let server_b = MockServer::start().await;
    mount_three_pages(&server_b).await;
    let client_b = client_for(&server_b);
    let conn_b = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn_b, "o", "r").unwrap();
    let clk_b = test_clock();
    let ctx_b = walk_ctx(&client_b, &conn_b, &clk_b);
    let mut seen = std::collections::HashSet::new();
    let stop = sync::issues::sync_issues(&ctx_b, false, &mut seen, |_h| true)
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
    let clk = test_clock();
    let mut syncer = Syncer {
        client: &client,
        conn: &conn,
        rl: &mut rl,
        clock: &clk,
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
    let clk = test_clock();
    let mut syncer = Syncer {
        client: &client,
        conn: &conn,
        rl: &mut rl,
        clock: &clk,
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

// NOTE: this test deliberately deviates from the task-4 brief's Step 1.
// Calling `sync::issues::sync_issues` directly twice with one externally
// shared `seen` (as the brief's version did) can never go red: Task 3 already
// hoisted `seen` above `run_phase`'s retry loop, so an externally shared set
// accumulates regardless of the bug, and manually invoking
// `store::mark_deleted_except` on that accumulated set afterward bypasses
// `sync_issues`'s own (buggy) internal `started_fresh` gate entirely. Verified
// pre-fix with a throwaway version of this same test: it passed unmodified.
//
// This version drives the real code path with the bug -- `Syncer::run` ->
// `run_phase`'s pause/sleep/retry loop -- and asserts on the persisted
// `deleted` column, which only `run_phase`'s own reconciliation call can set.
// Flagged to the team lead before writing this version; proceeding on this
// shape rather than stalling, pending their confirmation.
#[tokio::test]
async fn reconciles_after_an_in_process_pause() {
    use github_repo_meta_fetch::config::Reserve;
    use github_repo_meta_fetch::ratelimit::store::RateLimitStore;
    use github_repo_meta_fetch::sync::OnlyTarget;
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
    // Page 1: low remaining headers -> budget_ok returns false -> run_phase
    // pauses, sleeps (reset is already in the past relative to the fixed
    // clock, so the sleep is instant), and re-calls sync_issues.
    let low = ResponseTemplate::new(200)
        .insert_header("x-ratelimit-resource", "graphql")
        .insert_header("x-ratelimit-limit", "5000")
        .insert_header("x-ratelimit-remaining", "100")
        .insert_header("x-ratelimit-used", "4900")
        .insert_header("x-ratelimit-reset", "1781564821")
        .set_body_string(page(&issue_node(1, "2026-06-10T00:00:00Z"), true, "CUR1"));
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("issues("))
        .respond_with(low)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // Page 2: healthy remaining, no next page -> the phase completes.
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("issues("))
        .respond_with(rl_headers(ResponseTemplate::new(200).set_body_string(
            page(&issue_node(2, "2026-06-09T00:00:00Z"), false, ""),
        )))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let conn = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn, "o", "r").unwrap();

    // A cached issue that the walk will not see: it must be soft-deleted.
    conn.execute(
        "INSERT INTO issues (node_id, number, title, state, body, created_at, updated_at, deleted)
         VALUES ('I_stale', 99, 'gone', 'OPEN', '', 0, 0, 0)",
        [],
    )
    .unwrap();

    let mut rl = RateLimitStore::open_in_memory("fp").unwrap();
    let clk = test_clock();
    let mut syncer = Syncer {
        client: &client,
        conn: &conn,
        rl: &mut rl,
        clock: &clk,
        reserve: Reserve::Percent(0.10),
        cost_ceiling: Some(30),
        no_wait: false,
        max_wait: None,
        full: true,
        only: vec![OnlyTarget::Issues],
    };
    let outcome = syncer.run("o", "r").await.unwrap();
    assert_eq!(outcome, Outcome::Completed);

    // Both pages were persisted.
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM issues WHERE deleted = 0", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 2);

    // Reconciliation spans the whole walk (both pages), so the stale issue
    // -- seen by neither page -- must be soft-deleted.
    let stale = store::issues::get_issue_by_number(&conn, 99)
        .unwrap()
        .unwrap();
    assert!(
        stale.deleted,
        "the stale issue should be soft-deleted after reconciliation across the in-process pause"
    );
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
    let clk = test_clock();
    let ctx = walk_ctx(&client, &conn, &clk);
    let mut seen = std::collections::HashSet::new();
    let stop = sync::issues::sync_issues(&ctx, true, &mut seen, |_h| true)
        .await
        .unwrap();
    assert!(matches!(stop, SyncStop::Completed));

    // sync_issues no longer reconciles on its own (that moved to run_phase's
    // caller); a direct call must reconcile explicitly, exactly as run_phase
    // now does after its retry loop exits.
    store::mark_deleted_except(&conn, "issues", &seen).unwrap();

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
