//! Pull-request synchronisation: maps `PrsPage` nodes into domain types and
//! (in [`sync_prs`]) paginates `repository.pullRequests` with per-PR rate-limit
//! gating, mirroring [`crate::sync::issues`].

use std::collections::HashSet;

use chrono::DateTime;
use chrono::Utc;
use graphql_client::GraphQLQuery as _;
use rusqlite::Connection;

use crate::github::gql::PrsPage;
use crate::github::gql::ReviewThreadsPage;
use crate::github::gql::ReviewsPage;
use crate::github::gql::prs_page;
use crate::github::gql::review_threads_page;
use crate::github::gql::reviews_page;
use crate::model::Comment;
use crate::model::CrossRef;
use crate::model::CrossRefEvent;
use crate::model::PullRequest;
use crate::model::Review;
use crate::model::ReviewComment;
use crate::model::ReviewState;
use crate::model::ReviewThread;
use crate::store::issues::replace_comments;
use crate::store::issues::upsert_cross_ref;
use crate::store::prs::mark_deleted_except;
use crate::store::prs::replace_review_threads;
use crate::store::prs::replace_reviews;
use crate::store::prs::upsert_pull_request;
use crate::store::sync_state;
use crate::store::sync_state::RunPhase;
use crate::sync::issues::SyncStop;
use crate::sync::next_cursor;

pub(crate) const ENTITY: &str = "pull_requests";

/// A PR node mapped into domain types, plus follow-up page cursors.
pub struct MappedPr {
    pub pr: PullRequest,
    pub comments: Vec<Comment>,
    pub reviews: Vec<Review>,
    pub threads: Vec<ReviewThread>,
    /// Closing-ref cross-refs (both directions); no PR timeline (see spec §2).
    pub cross_refs: Vec<CrossRef>,
    /// `endCursor` of the embedded comments page when it has a next page.
    pub comments_more: Option<String>,
    /// `endCursor` of the embedded reviews page when it has a next page.
    pub reviews_more: Option<String>,
    /// `endCursor` of the embedded review-threads page when it has a next page.
    pub threads_more: Option<String>,
}

/// Map a single PR node into domain types and follow-up cursors.
#[must_use]
pub fn map_pr_node(node: &prs_page::PrsPageRepositoryPullRequestsNodes) -> MappedPr {
    let labels = node
        .labels
        .as_ref()
        .and_then(|l| l.nodes.as_ref())
        .map(|nodes| nodes.iter().flatten().map(|n| n.name.clone()).collect())
        .unwrap_or_default();
    let assignees = node
        .assignees
        .nodes
        .as_ref()
        .map(|nodes| nodes.iter().flatten().map(|n| n.login.clone()).collect())
        .unwrap_or_default();

    let pr = PullRequest {
        node_id: node.id.clone(),
        number: node.number,
        title: node.title.clone(),
        state: match &node.state {
            prs_page::PullRequestState::OPEN => "OPEN".to_string(),
            prs_page::PullRequestState::CLOSED => "CLOSED".to_string(),
            prs_page::PullRequestState::MERGED => "MERGED".to_string(),
            prs_page::PullRequestState::Other(s) => s.clone(),
        },
        is_draft: node.is_draft,
        merged: node.merged,
        merged_at: node.merged_at,
        merged_by: node.merged_by.as_ref().map(|m| m.login.clone()),
        base_ref: node.base_ref_name.clone(),
        head_ref: node.head_ref_name.clone(),
        additions: node.additions,
        deletions: node.deletions,
        changed_files: node.changed_files,
        author: node.author.as_ref().map(|a| a.login.clone()),
        body: node.body.clone(),
        created_at: node.created_at,
        updated_at: node.updated_at,
        closed_at: node.closed_at,
        milestone: node.milestone.as_ref().map(|m| m.title.clone()),
        labels,
        assignees,
        deleted: false,
    };

    let comments = node
        .comments
        .nodes
        .as_ref()
        .map(|nodes| {
            nodes
                .iter()
                .flatten()
                .map(|c| Comment {
                    node_id: c.id.clone(),
                    subject_node_id: node.id.clone(),
                    author: c.author.as_ref().map(|a| a.login.clone()),
                    created_at: c.created_at,
                    body: c.body.clone(),
                })
                .collect()
        })
        .unwrap_or_default();

    let reviews = node
        .reviews
        .as_ref()
        .map(|r| map_reviews(&node.id, r.nodes.as_deref().unwrap_or(&[])))
        .unwrap_or_default();

    let threads = map_threads(
        &node.id,
        node.review_threads.nodes.as_deref().unwrap_or(&[]),
    );
    let cross_refs = map_closing_refs(&pr, node);

    MappedPr {
        pr,
        comments,
        reviews,
        threads,
        cross_refs,
        comments_more: next_cursor(
            node.comments.page_info.has_next_page,
            node.comments.page_info.end_cursor.as_deref(),
        ),
        reviews_more: node.reviews.as_ref().and_then(|r| {
            next_cursor(r.page_info.has_next_page, r.page_info.end_cursor.as_deref())
        }),
        threads_more: next_cursor(
            node.review_threads.page_info.has_next_page,
            node.review_threads.page_info.end_cursor.as_deref(),
        ),
    }
}

/// The raw `SCREAMING_SNAKE` string for a generated review-state enum.
trait ReviewStateRaw {
    fn as_raw(&self) -> &str;
}

impl ReviewStateRaw for prs_page::PullRequestReviewState {
    fn as_raw(&self) -> &str {
        use prs_page::PullRequestReviewState as S;
        match self {
            S::APPROVED => "APPROVED",
            S::CHANGES_REQUESTED => "CHANGES_REQUESTED",
            S::COMMENTED => "COMMENTED",
            S::DISMISSED => "DISMISSED",
            S::PENDING => "PENDING",
            S::Other(raw) => raw,
        }
    }
}

impl ReviewStateRaw for reviews_page::PullRequestReviewState {
    fn as_raw(&self) -> &str {
        use reviews_page::PullRequestReviewState as S;
        match self {
            S::APPROVED => "APPROVED",
            S::CHANGES_REQUESTED => "CHANGES_REQUESTED",
            S::COMMENTED => "COMMENTED",
            S::DISMISSED => "DISMISSED",
            S::PENDING => "PENDING",
            S::Other(raw) => raw,
        }
    }
}

/// Map any generated review-state enum to the domain `ReviewState`, warning on
/// unknowns.
fn review_state<S: ReviewStateRaw>(s: &S) -> ReviewState {
    let raw = s.as_raw();
    ReviewState::parse(raw).unwrap_or_else(|| {
        tracing::warn!(raw, "unknown PullRequestReviewState; treating as Commented");
        ReviewState::Commented
    })
}

fn map_reviews(
    pr_node_id: &str,
    nodes: &[Option<prs_page::PrsPageRepositoryPullRequestsNodesReviewsNodes>],
) -> Vec<Review> {
    nodes
        .iter()
        .flatten()
        .map(|r| Review {
            node_id: r.id.clone(),
            pr_node_id: pr_node_id.to_string(),
            author: r.author.as_ref().map(|a| a.login.clone()),
            state: review_state(&r.state),
            body: r.body.clone(),
            submitted_at: r.submitted_at,
        })
        .collect()
}

fn map_threads(
    pr_node_id: &str,
    nodes: &[Option<prs_page::PrsPageRepositoryPullRequestsNodesReviewThreadsNodes>],
) -> Vec<ReviewThread> {
    nodes
        .iter()
        .flatten()
        .map(|t| {
            let comments: Vec<ReviewComment> = t
                .comments
                .nodes
                .as_ref()
                .map(|cs| {
                    cs.iter()
                        .flatten()
                        .map(|c| ReviewComment {
                            node_id: c.id.clone(),
                            thread_node_id: t.id.clone(),
                            author: c.author.as_ref().map(|a| a.login.clone()),
                            created_at: c.created_at,
                            body: c.body.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let diff_hunk = t
                .comments
                .nodes
                .as_ref()
                .and_then(|cs| cs.iter().flatten().next())
                .map(|c| c.diff_hunk.clone())
                .unwrap_or_default();
            // Spec §4: within-thread comments are NOT separately drained; a thread
            // exceeding the inline page is truncated. Log rather than drop silently.
            if t.comments.page_info.has_next_page {
                tracing::warn!(
                    thread = %t.id,
                    captured = comments.len(),
                    "review thread has more comments than the inline page; tail truncated"
                );
            }
            ReviewThread {
                node_id: t.id.clone(),
                pr_node_id: pr_node_id.to_string(),
                path: t.path.clone(),
                line: t.line,
                is_resolved: t.is_resolved,
                is_outdated: t.is_outdated,
                diff_hunk,
                comments,
            }
        })
        .collect()
}

/// Build both-direction `CLOSING_REFERENCE` cross-refs from
/// `closingIssuesReferences`.
fn map_closing_refs(
    pr: &PullRequest,
    node: &prs_page::PrsPageRepositoryPullRequestsNodes,
) -> Vec<CrossRef> {
    let mut out = Vec::new();
    let Some(refs) = node.closing_issues_references.as_ref() else {
        return out;
    };
    let Some(nodes) = refs.nodes.as_ref() else {
        return out;
    };
    for issue in nodes.iter().flatten() {
        // PR → issue
        out.push(CrossRef {
            issue_node_id: pr.node_id.clone(),
            referenced_issue_number: issue.number,
            event_type: CrossRefEvent::ClosingReference,
            created_at: pr.created_at,
        });
        // issue → PR
        out.push(CrossRef {
            issue_node_id: issue.id.clone(),
            referenced_issue_number: pr.number,
            event_type: CrossRefEvent::ClosingReference,
            created_at: pr.created_at,
        });
    }
    out
}

/// Fetch remaining review pages for one PR, appending to `reviews`.
/// Returns the last response's headers, or `None` if nothing was fetched.
async fn drain_reviews(
    client: &crate::github::GithubClient,
    pr_node_id: &str,
    mut cursor: Option<String>,
    reviews: &mut Vec<Review>,
) -> anyhow::Result<Option<http::HeaderMap>> {
    let mut last_headers = None;
    while let Some(after) = cursor {
        let body = ReviewsPage::build_query(reviews_page::Variables {
            id: pr_node_id.to_string(),
            cursor: Some(after),
        });
        let res = client
            .graphql::<_, reviews_page::ResponseData>(&body)
            .await?;
        let page_info = match &res.data.node {
            Some(reviews_page::ReviewsPageNode::PullRequest(prn)) => {
                prn.reviews.as_ref().map(|conn| {
                    for r in conn.nodes.iter().flatten().flatten() {
                        reviews.push(Review {
                            node_id: r.id.clone(),
                            pr_node_id: pr_node_id.to_string(),
                            author: r.author.as_ref().map(|a| a.login.clone()),
                            state: review_state(&r.state),
                            body: r.body.clone(),
                            submitted_at: r.submitted_at,
                        });
                    }
                    (
                        conn.page_info.has_next_page,
                        conn.page_info.end_cursor.clone(),
                    )
                })
            }
            _ => None,
        };
        last_headers = Some(res.headers);
        cursor = match page_info {
            Some((has_next, end)) => next_cursor(has_next, end.as_deref()),
            None => break,
        };
    }
    Ok(last_headers)
}

/// Fetch remaining review-thread pages for one PR, appending to `threads`.
/// Returns the last response's headers, or `None` if nothing was fetched.
async fn drain_review_threads(
    client: &crate::github::GithubClient,
    pr_node_id: &str,
    mut cursor: Option<String>,
    threads: &mut Vec<ReviewThread>,
) -> anyhow::Result<Option<http::HeaderMap>> {
    let mut last_headers = None;
    while let Some(after) = cursor {
        let body = ReviewThreadsPage::build_query(review_threads_page::Variables {
            id: pr_node_id.to_string(),
            cursor: Some(after),
        });
        let res = client
            .graphql::<_, review_threads_page::ResponseData>(&body)
            .await?;
        let page_info = match &res.data.node {
            Some(review_threads_page::ReviewThreadsPageNode::PullRequest(prn)) => {
                for t in prn.review_threads.nodes.iter().flatten().flatten() {
                    let comments: Vec<ReviewComment> = t
                        .comments
                        .nodes
                        .as_ref()
                        .map(|cs| {
                            cs.iter()
                                .flatten()
                                .map(|c| ReviewComment {
                                    node_id: c.id.clone(),
                                    thread_node_id: t.id.clone(),
                                    author: c.author.as_ref().map(|a| a.login.clone()),
                                    created_at: c.created_at,
                                    body: c.body.clone(),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let diff_hunk = t
                        .comments
                        .nodes
                        .as_ref()
                        .and_then(|cs| cs.iter().flatten().next())
                        .map(|c| c.diff_hunk.clone())
                        .unwrap_or_default();
                    if t.comments.page_info.has_next_page {
                        tracing::warn!(thread = %t.id, captured = comments.len(), "review thread has more comments than the inline page; tail truncated");
                    }
                    threads.push(ReviewThread {
                        node_id: t.id.clone(),
                        pr_node_id: pr_node_id.to_string(),
                        path: t.path.clone(),
                        line: t.line,
                        is_resolved: t.is_resolved,
                        is_outdated: t.is_outdated,
                        diff_hunk,
                        comments,
                    });
                }
                Some((
                    prn.review_threads.page_info.has_next_page,
                    prn.review_threads.page_info.end_cursor.clone(),
                ))
            }
            _ => None,
        };
        last_headers = Some(res.headers);
        cursor = match page_info {
            Some((has_next, end)) => next_cursor(has_next, end.as_deref()),
            None => break,
        };
    }
    Ok(last_headers)
}

/// Incrementally synchronise pull requests for `owner/repo`.
///
/// Mirrors [`crate::sync::issues::sync_issues`] but gates the budget **per PR**
/// (after each PR's nested drains) rather than per page, because PR drains are
/// heavy. On a budget pause it returns [`SyncStop::Paused`] **without**
/// advancing the current page's cursor, so a resume refetches the in-progress
/// page idempotently (node-id–keyed upserts).
///
/// # Errors
///
/// Returns an error on GraphQL transport/decoding failure, a missing
/// `repository`, or any persistence failure.
pub async fn sync_prs<F>(
    client: &crate::github::GithubClient,
    conn: &Connection,
    owner: &str,
    repo: &str,
    full: bool,
    mut budget_ok: F,
) -> anyhow::Result<SyncStop>
where
    F: FnMut(&http::HeaderMap) -> bool,
{
    let state = sync_state::get(conn, ENTITY)?;
    let watermark = state.updated_watermark;
    let started_fresh = state.resume_cursor.is_none();
    let mut cursor = state.resume_cursor;
    let mut run_min: Option<DateTime<Utc>> = None;
    let mut seen: HashSet<String> = HashSet::new();

    loop {
        let body = PrsPage::build_query(prs_page::Variables {
            owner: owner.to_string(),
            repo: repo.to_string(),
            cursor: cursor.clone(),
        });
        let res = client.graphql::<_, prs_page::ResponseData>(&body).await?;
        let prs = res
            .data
            .repository
            .ok_or_else(|| anyhow::anyhow!("graphql response had no repository"))?
            .pull_requests;
        let page_info = prs.page_info;
        let nodes = prs.nodes.unwrap_or_default();
        let mut latest_headers = res.headers;
        tracing::debug!(
            count = nodes.iter().filter(|n| n.is_some()).count(),
            has_next = page_info.has_next_page,
            "fetched PR page"
        );

        let mut crossed = false;
        for node in nodes.into_iter().flatten() {
            let m = map_pr_node(&node);

            if !full
                && let Some(wm) = watermark
                && m.pr.updated_at < wm
            {
                crossed = true;
                break;
            }
            run_min = Some(run_min.map_or(m.pr.updated_at, |c| c.min(m.pr.updated_at)));
            if full {
                seen.insert(m.pr.node_id.clone());
            }

            let mut comments = m.comments;
            if let Some(c) = m.comments_more
                && let Some(h) = crate::sync::issues::drain_comments(
                    client,
                    &m.pr.node_id,
                    Some(c),
                    &mut comments,
                )
                .await?
            {
                latest_headers = h;
            }
            let mut reviews = m.reviews;
            if let Some(c) = m.reviews_more
                && let Some(h) = drain_reviews(client, &m.pr.node_id, Some(c), &mut reviews).await?
            {
                latest_headers = h;
            }
            let mut threads = m.threads;
            if let Some(c) = m.threads_more
                && let Some(h) =
                    drain_review_threads(client, &m.pr.node_id, Some(c), &mut threads).await?
            {
                latest_headers = h;
            }

            let tx = conn.unchecked_transaction()?;
            upsert_pull_request(&tx, &m.pr)?;
            replace_comments(&tx, &m.pr.node_id, &comments)?;
            replace_reviews(&tx, &m.pr.node_id, &reviews)?;
            replace_review_threads(&tx, &m.pr.node_id, &threads)?;
            for x in &m.cross_refs {
                upsert_cross_ref(&tx, x)?;
            }
            tx.commit()?;

            // Per-PR budget gate (see spec §5). On breach, do NOT advance the
            // page cursor: the checkpoint still points at this page's `after`,
            // so resume refetches it idempotently.
            if !budget_ok(&latest_headers) {
                return Ok(SyncStop::Paused);
            }
        }

        sync_state::set_cursor(
            conn,
            ENTITY,
            page_info.end_cursor.as_deref(),
            RunPhase::Paginating,
        )?;

        if crossed || !page_info.has_next_page {
            break;
        }
        cursor = page_info.end_cursor;
    }

    sync_state::complete(conn, ENTITY, run_min.or(watermark))?;

    if full && started_fresh {
        mark_deleted_except(conn, &seen)?;
    }

    Ok(SyncStop::Completed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::gql::prs_page;
    use crate::model::CrossRefEvent;
    use crate::model::ReviewState;

    fn pr_node_fixture() -> prs_page::PrsPageRepositoryPullRequestsNodes {
        // mergedBy carries a __typename discriminant for serde(tag) deserialization
        let json = r#"{
          "id":"PR_1","number":42,"title":"Add PRs","body":"body","state":"MERGED",
          "isDraft":false,"merged":true,"mergedAt":"2026-06-14T00:00:00Z",
          "mergedBy":{"__typename":"User","login":"demosdemon"},
          "baseRefName":"main","headRefName":"feature/prs",
          "additions":412,"deletions":87,"changedFiles":9,
          "createdAt":"2026-06-10T00:00:00Z","updatedAt":"2026-06-14T00:00:00Z",
          "closedAt":"2026-06-14T00:00:00Z",
          "author":{"__typename":"User","login":"octocat"},"milestone":{"title":"v1.0"},
          "labels":{"nodes":[{"name":"bug"}]},"assignees":{"nodes":[{"login":"octocat"}]},
          "comments":{"totalCount":1,"pageInfo":{"hasNextPage":false,"endCursor":null},
            "nodes":[{"id":"C1","createdAt":"2026-06-10T01:00:00Z","body":"hi","author":{"__typename":"User","login":"octocat"}}]},
          "reviews":{"pageInfo":{"hasNextPage":false,"endCursor":null},
            "nodes":[{"id":"R1","state":"APPROVED","submittedAt":"2026-06-14T00:00:00Z","body":"lgtm","author":{"__typename":"User","login":"demosdemon"}}]},
          "reviewThreads":{"pageInfo":{"hasNextPage":false,"endCursor":null},
            "nodes":[{"id":"T1","path":"src/x.rs","line":88,"isResolved":true,"isOutdated":false,
              "comments":{"pageInfo":{"hasNextPage":false,"endCursor":null},
                "nodes":[{"id":"RC1","createdAt":"2026-06-11T00:00:00Z","body":"fix this","diffHunk":"@@ -1 +1 @@","author":{"__typename":"User","login":"demosdemon"}}]}}]},
          "closingIssuesReferences":{"nodes":[{"id":"ISSUE_41","number":41}]}
        }"#;
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn thread_comment_tail_is_truncated_at_inline_page() {
        // Spec §10 / §4 note: within-thread comment draining is NOT implemented.
        // When a thread's inline page reports hasNextPage=true the mapper captures
        // only the inline comments and logs a warning — it does NOT fetch further
        // pages.
        let json = r#"{
          "id":"PR_2","number":99,"title":"Cap test","body":"","state":"OPEN",
          "isDraft":false,"merged":false,"mergedAt":null,
          "mergedBy":null,
          "baseRefName":"main","headRefName":"feature/cap",
          "additions":1,"deletions":0,"changedFiles":1,
          "createdAt":"2026-06-01T00:00:00Z","updatedAt":"2026-06-01T00:00:00Z",
          "closedAt":null,
          "author":{"__typename":"User","login":"x"},"milestone":null,
          "labels":{"nodes":[]},"assignees":{"nodes":[]},
          "comments":{"totalCount":0,"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]},
          "reviews":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]},
          "reviewThreads":{"pageInfo":{"hasNextPage":false,"endCursor":null},
            "nodes":[{"id":"T99","path":"src/lib.rs","line":1,
              "isResolved":false,"isOutdated":false,
              "comments":{"pageInfo":{"hasNextPage":true,"endCursor":"MORE"},
                "nodes":[{"id":"RC_INLINE","createdAt":"2026-06-01T00:00:00Z",
                  "body":"only one inline","diffHunk":"@@ -1 +1 @@",
                  "author":{"__typename":"User","login":"x"}}]}}]},
          "closingIssuesReferences":null
        }"#;
        let node: prs_page::PrsPageRepositoryPullRequestsNodes =
            serde_json::from_str(json).unwrap();
        let m = map_pr_node(&node);
        // The mapper captures only the one inline comment; the tail is NOT fetched.
        assert_eq!(m.threads.len(), 1);
        assert_eq!(
            m.threads[0].comments.len(),
            1,
            "only the inline page should be captured; tail must not be fetched"
        );
        // The threads_more cursor is absent (thread-level pagination is separate from
        // within-thread comment pagination — the outer thread page has no next page).
        assert_eq!(m.threads_more, None);
    }

    #[test]
    fn maps_pr_with_nested_connections() {
        let node = pr_node_fixture();
        let m = map_pr_node(&node);
        assert_eq!(m.pr.number, 42);
        assert!(m.pr.merged);
        assert_eq!(m.pr.merged_by.as_deref(), Some("demosdemon"));
        assert_eq!(m.pr.base_ref, "main");
        assert_eq!(m.pr.changed_files, 9);
        assert_eq!(m.pr.labels, vec!["bug".to_string()]);
        assert_eq!(m.comments.len(), 1);
        assert_eq!(m.reviews.len(), 1);
        assert_eq!(m.reviews[0].state, ReviewState::Approved);
        assert_eq!(m.threads.len(), 1);
        assert_eq!(m.threads[0].diff_hunk, "@@ -1 +1 @@");
        assert_eq!(m.threads[0].comments.len(), 1);
        assert_eq!(m.threads[0].line, Some(88));
        assert_eq!(m.comments_more, None);
        assert_eq!(m.reviews_more, None);
        assert_eq!(m.threads_more, None);
        // closing refs produce TWO cross-ref rows (both directions)
        assert_eq!(m.cross_refs.len(), 2);
        assert!(
            m.cross_refs
                .iter()
                .all(|x| x.event_type == CrossRefEvent::ClosingReference)
        );
        // PR→issue
        assert!(
            m.cross_refs
                .iter()
                .any(|x| x.issue_node_id == "PR_1" && x.referenced_issue_number == 41)
        );
        // issue→PR
        assert!(
            m.cross_refs
                .iter()
                .any(|x| x.issue_node_id == "ISSUE_41" && x.referenced_issue_number == 42)
        );
    }
}
