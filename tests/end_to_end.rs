#![allow(clippy::unwrap_used)]
use github_repo_meta_fetch::github::GithubClient;
use github_repo_meta_fetch::render;
use github_repo_meta_fetch::store;
use github_repo_meta_fetch::sync;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_string_contains;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn rl(t: ResponseTemplate) -> ResponseTemplate {
    t.insert_header("x-ratelimit-resource", "graphql")
        .insert_header("x-ratelimit-limit", "5000")
        .insert_header("x-ratelimit-remaining", "4990")
        .insert_header("x-ratelimit-used", "10")
        .insert_header("x-ratelimit-reset", "1781564821")
}

fn issue_page() -> String {
    r#"{"data":{"repository":{"issues":{
      "pageInfo":{"hasNextPage":false,"endCursor":null},
      "nodes":[{"id":"I_1","number":1,"title":"t","body":"b","state":"OPEN","stateReason":null,
        "createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-02T00:00:00Z","closedAt":null,
        "author":{"__typename":"User","login":"o"},"milestone":null,"labels":{"nodes":[]},"assignees":{"nodes":[]},
        "comments":{"totalCount":0,"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]},
        "timelineItems":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}}]}}}}"#.to_string()
}

fn pr_page() -> String {
    r#"{"data":{"repository":{"pullRequests":{
      "pageInfo":{"hasNextPage":false,"endCursor":null},
      "nodes":[{"id":"PR_1","number":1,"title":"first pr","body":"pr body","state":"OPEN",
        "isDraft":false,"merged":false,"mergedAt":null,"mergedBy":null,
        "baseRefName":"main","headRefName":"feature","additions":10,"deletions":2,"changedFiles":3,
        "createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-02T00:00:00Z","closedAt":null,
        "author":{"__typename":"User","login":"o"},"milestone":null,
        "labels":{"nodes":[]},"assignees":{"nodes":[]},
        "comments":{"totalCount":0,"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]},
        "reviews":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]},
        "reviewThreads":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]},
        "closingIssuesReferences":{"nodes":[]}}]}}}}"#
        .to_string()
}

async fn run_once() -> tempfile::TempDir {
    let server = MockServer::start().await;
    // PR queries — disambiguated by body content (order-independent).
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("pullRequests("))
        .respond_with(rl(ResponseTemplate::new(200).set_body_string(pr_page())))
        .mount(&server)
        .await;
    // Issue queries — disambiguated by body content (order-independent).
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("issues("))
        .respond_with(rl(ResponseTemplate::new(200).set_body_string(issue_page())))
        .mount(&server)
        .await;
    for p in ["/repos/o/r/labels", "/repos/o/r/milestones"] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
            .mount(&server)
            .await;
    }
    let octo = octocrab::Octocrab::builder()
        .base_uri(server.uri())
        .unwrap()
        .personal_token("t".to_string())
        .build()
        .unwrap();
    let client = GithubClient::new(octo);
    let conn = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn, "o", "r").unwrap();
    sync::taxonomy::sync_labels(&client, &conn, "o", "r")
        .await
        .unwrap();
    sync::taxonomy::sync_milestones(&client, &conn, "o", "r")
        .await
        .unwrap();
    sync::issues::sync_issues(&client, &conn, "o", "r", false, |_h| true)
        .await
        .unwrap();
    sync::prs::sync_prs(&client, &conn, "o", "r", false, |_h| true)
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    render::render_tree(&conn, dir.path()).unwrap();
    dir
}

#[tokio::test]
async fn two_runs_render_identically() {
    let a = run_once().await;
    let b = run_once().await;
    let fa = std::fs::read_to_string(a.path().join("issues/0001.md")).unwrap();
    let fb = std::fs::read_to_string(b.path().join("issues/0001.md")).unwrap();
    assert_eq!(fa, fb);
    // labels.md / milestones.md / by-state are also produced
    assert!(a.path().join("labels.md").exists());
    assert!(a.path().join("issues/by-state/open.md").exists());
}

#[tokio::test]
async fn e2e_renders_pr_subtree_and_readme_counts() {
    let dir = run_once().await;

    // The PR document must exist.
    assert!(
        dir.path().join("prs/0001.md").exists(),
        "expected prs/0001.md to be rendered"
    );
    // by-state buckets should be present.
    assert!(dir.path().join("prs/by-state/open.md").exists());

    // README must contain PR count lines.
    let readme = std::fs::read_to_string(dir.path().join("README.md")).unwrap();
    assert!(
        readme.contains("open PRs: 1"),
        "README should report 1 open PR, got:\n{readme}"
    );
    assert!(
        readme.contains("merged PRs: 0"),
        "README should report 0 merged PRs, got:\n{readme}"
    );
}
