//! Incremental issue synchronisation.
//!
//! Paginates `repository.issues` ordered by `UPDATED_AT DESC`, early-stopping
//! when an issue's `updatedAt` falls strictly below the stored watermark.
//! Follows nested comment and timeline (cross-reference) pages, persists each
//! issue idempotently in its own transaction, checkpoints the cursor per page,
//! and advances the watermark only on clean completion. Pauses (saving the
//! checkpoint) when the `budget_ok` closure signals the rate-limit reserve
//! floor would be breached.

use std::collections::HashSet;

use chrono::DateTime;
use chrono::Utc;
use graphql_client::GraphQLQuery as _;
use rusqlite::Connection;

use crate::github::gql::CommentsPage;
use crate::github::gql::IssuesPage;
use crate::github::gql::TimelinePage;
use crate::github::gql::comments_page;
use crate::github::gql::issues_page;
use crate::github::gql::timeline_page;
use crate::model::Comment;
use crate::model::CrossRef;
use crate::model::CrossRefEvent;
use crate::model::Issue;
use crate::model::IssueState;
use crate::store::issues::replace_comments;
use crate::store::issues::upsert_cross_ref;
use crate::store::issues::upsert_issue;
use crate::store::sync_state;
use crate::store::sync_state::RunPhase;
use crate::sync::next_cursor;

const ENTITY: &str = "issues";

/// A single issue node mapped into domain types, plus follow-up page cursors.
pub struct MappedIssue {
    pub issue: Issue,
    pub comments: Vec<Comment>,
    pub cross_refs: Vec<CrossRef>,
    /// `endCursor` of the embedded comments page when it has a next page.
    pub comments_more: Option<String>,
    /// `endCursor` of the embedded timeline page when it has a next page.
    pub timeline_more: Option<String>,
}

/// Outcome of a sync run.
pub enum SyncStop {
    /// The run walked to completion (watermark advanced).
    Completed,
    /// The run paused on the budget floor (checkpoint cursor saved).
    Paused,
}

/// Map the optional generated author wrapper to its login string.
fn author_login(
    author: Option<&issues_page::IssuesPageRepositoryIssuesNodesAuthor>,
) -> Option<String> {
    author.map(|a| a.login.clone())
}

/// Map a single issue node into domain types and follow-up cursors.
#[must_use]
pub fn map_issue_node(node: &issues_page::IssuesPageRepositoryIssuesNodes) -> MappedIssue {
    let state = match node.state {
        issues_page::IssueState::OPEN => IssueState::Open,
        _ => IssueState::Closed,
    };

    let state_reason = node
        .state_reason
        .as_ref()
        .map(|v| format!("{v:?}").to_lowercase());

    let milestone = node.milestone.as_ref().map(|m| m.title.clone());

    let labels: Vec<String> = node
        .labels
        .as_ref()
        .and_then(|l| l.nodes.as_ref())
        .map(|nodes| nodes.iter().flatten().map(|n| n.name.clone()).collect())
        .unwrap_or_default();

    let assignees: Vec<String> = node
        .assignees
        .nodes
        .as_ref()
        .map(|nodes| nodes.iter().flatten().map(|n| n.login.clone()).collect())
        .unwrap_or_default();

    let issue = Issue {
        node_id: node.id.clone(),
        number: node.number,
        title: node.title.clone(),
        state,
        state_reason,
        author: author_login(node.author.as_ref()),
        body: node.body.clone(),
        created_at: node.created_at,
        updated_at: node.updated_at,
        closed_at: node.closed_at,
        milestone,
        labels,
        assignees,
        deleted: false,
    };

    let comments: Vec<Comment> = node
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

    let comments_more = next_cursor(
        node.comments.page_info.has_next_page,
        node.comments.page_info.end_cursor.as_deref(),
    );
    let timeline_more = next_cursor(
        node.timeline_items.page_info.has_next_page,
        node.timeline_items.page_info.end_cursor.as_deref(),
    );

    let cross_refs = map_timeline_items(
        &node.id,
        node.timeline_items.nodes.as_deref().unwrap_or(&[]),
    );

    MappedIssue {
        issue,
        comments,
        cross_refs,
        comments_more,
        timeline_more,
    }
}

/// One timeline node's cross-reference projection: `(referenced_number, event,
/// created_at)`. Returns `None` for non-cross-ref events (and for
/// marked/unmarked-duplicate events whose canonical target is absent).
trait TimelineItem {
    fn cross_ref(&self) -> Option<(i64, CrossRefEvent, chrono::DateTime<chrono::Utc>)>;
}

/// Map a slice of generated timeline nodes into cross-refs via
/// [`TimelineItem`].
fn map_timeline_items<I: TimelineItem>(issue_node_id: &str, nodes: &[Option<I>]) -> Vec<CrossRef> {
    nodes
        .iter()
        .flatten()
        .filter_map(|item| {
            let (number, event_type, created_at) = item.cross_ref()?;
            Some(CrossRef {
                issue_node_id: issue_node_id.to_string(),
                referenced_issue_number: number,
                event_type,
                created_at,
            })
        })
        .collect()
}

impl TimelineItem for issues_page::IssuesPageRepositoryIssuesNodesTimelineItemsNodes {
    fn cross_ref(&self) -> Option<(i64, CrossRefEvent, chrono::DateTime<chrono::Utc>)> {
        use issues_page::IssuesPageRepositoryIssuesNodesTimelineItemsNodes as Item;
        use issues_page::IssuesPageRepositoryIssuesNodesTimelineItemsNodesOnConnectedEventSubject as Subject;
        use issues_page::IssuesPageRepositoryIssuesNodesTimelineItemsNodesOnCrossReferencedEventSource as Source;
        use issues_page::IssuesPageRepositoryIssuesNodesTimelineItemsNodesOnDisconnectedEventSubject as DisSubject;
        use issues_page::IssuesPageRepositoryIssuesNodesTimelineItemsNodesOnMarkedAsDuplicateEventCanonical as MarkCanon;
        use issues_page::IssuesPageRepositoryIssuesNodesTimelineItemsNodesOnUnmarkedAsDuplicateEventCanonical as UnmarkCanon;
        match self {
            Item::CrossReferencedEvent(e) => {
                let n = match &e.source {
                    Source::Issue(i) => i.number,
                    Source::PullRequest(p) => p.number,
                };
                Some((n, CrossRefEvent::CrossReferenced, e.created_at))
            }
            Item::ConnectedEvent(e) => {
                let n = match &e.subject {
                    Subject::Issue(i) => i.number,
                    Subject::PullRequest(p) => p.number,
                };
                Some((n, CrossRefEvent::Connected, e.created_at))
            }
            Item::DisconnectedEvent(e) => {
                let n = match &e.subject {
                    DisSubject::Issue(i) => i.number,
                    DisSubject::PullRequest(p) => p.number,
                };
                Some((n, CrossRefEvent::Disconnected, e.created_at))
            }
            Item::MarkedAsDuplicateEvent(e) => {
                let n = match e.canonical.as_ref()? {
                    MarkCanon::Issue(i) => i.number,
                    MarkCanon::PullRequest(p) => p.number,
                };
                Some((n, CrossRefEvent::MarkedAsDuplicate, e.created_at))
            }
            Item::UnmarkedAsDuplicateEvent(e) => {
                let n = match e.canonical.as_ref()? {
                    UnmarkCanon::Issue(i) => i.number,
                    UnmarkCanon::PullRequest(p) => p.number,
                };
                Some((n, CrossRefEvent::UnmarkedAsDuplicate, e.created_at))
            }
            _ => None,
        }
    }
}

impl TimelineItem for timeline_page::TimelinePageNodeOnIssueTimelineItemsNodes {
    fn cross_ref(&self) -> Option<(i64, CrossRefEvent, chrono::DateTime<chrono::Utc>)> {
        use timeline_page::TimelinePageNodeOnIssueTimelineItemsNodes as Item;
        use timeline_page::TimelinePageNodeOnIssueTimelineItemsNodesOnConnectedEventSubject as Subject;
        use timeline_page::TimelinePageNodeOnIssueTimelineItemsNodesOnCrossReferencedEventSource as Source;
        use timeline_page::TimelinePageNodeOnIssueTimelineItemsNodesOnDisconnectedEventSubject as DisSubject;
        use timeline_page::TimelinePageNodeOnIssueTimelineItemsNodesOnMarkedAsDuplicateEventCanonical as MarkCanon;
        use timeline_page::TimelinePageNodeOnIssueTimelineItemsNodesOnUnmarkedAsDuplicateEventCanonical as UnmarkCanon;
        match self {
            Item::CrossReferencedEvent(e) => {
                let n = match &e.source {
                    Source::Issue(i) => i.number,
                    Source::PullRequest(p) => p.number,
                };
                Some((n, CrossRefEvent::CrossReferenced, e.created_at))
            }
            Item::ConnectedEvent(e) => {
                let n = match &e.subject {
                    Subject::Issue(i) => i.number,
                    Subject::PullRequest(p) => p.number,
                };
                Some((n, CrossRefEvent::Connected, e.created_at))
            }
            Item::DisconnectedEvent(e) => {
                let n = match &e.subject {
                    DisSubject::Issue(i) => i.number,
                    DisSubject::PullRequest(p) => p.number,
                };
                Some((n, CrossRefEvent::Disconnected, e.created_at))
            }
            Item::MarkedAsDuplicateEvent(e) => {
                let n = match e.canonical.as_ref()? {
                    MarkCanon::Issue(i) => i.number,
                    MarkCanon::PullRequest(p) => p.number,
                };
                Some((n, CrossRefEvent::MarkedAsDuplicate, e.created_at))
            }
            Item::UnmarkedAsDuplicateEvent(e) => {
                let n = match e.canonical.as_ref()? {
                    UnmarkCanon::Issue(i) => i.number,
                    UnmarkCanon::PullRequest(p) => p.number,
                };
                Some((n, CrossRefEvent::UnmarkedAsDuplicate, e.created_at))
            }
            _ => None,
        }
    }
}

/// Shared accessor over the two structurally-identical generated comment node
/// types.
trait CommentNode {
    fn id(&self) -> &str;
    fn body(&self) -> &str;
    fn created_at(&self) -> chrono::DateTime<chrono::Utc>;
    fn author_login(&self) -> Option<String>;
}

impl CommentNode for comments_page::CommentsPageNodeOnIssueCommentsNodes {
    fn id(&self) -> &str {
        &self.id
    }
    fn body(&self) -> &str {
        &self.body
    }
    fn created_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.created_at
    }
    fn author_login(&self) -> Option<String> {
        self.author.as_ref().map(|a| a.login.clone())
    }
}

impl CommentNode for comments_page::CommentsPageNodeOnPullRequestCommentsNodes {
    fn id(&self) -> &str {
        &self.id
    }
    fn body(&self) -> &str {
        &self.body
    }
    fn created_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.created_at
    }
    fn author_login(&self) -> Option<String> {
        self.author.as_ref().map(|a| a.login.clone())
    }
}

/// Map a slice of generated comment nodes (id/createdAt/body/author) into
/// `Comment`s.
fn map_comment_nodes<C>(subject_node_id: &str, nodes: &[Option<C>]) -> Vec<Comment>
where
    C: CommentNode,
{
    nodes
        .iter()
        .flatten()
        .map(|c| Comment {
            node_id: c.id().to_string(),
            subject_node_id: subject_node_id.to_string(),
            author: c.author_login(),
            created_at: c.created_at(),
            body: c.body().to_string(),
        })
        .collect()
}

/// Fetch all remaining comment pages for one subject (issue or PR), appending
/// to `comments`. Returns the last response's headers (for per-call budget
/// gating), or `None` if no follow-up page was fetched.
#[must_use = "the returned headers carry rate-limit state for per-call budget gating"]
pub(crate) async fn drain_comments(
    client: &crate::github::GithubClient,
    subject_node_id: &str,
    mut cursor: Option<String>,
    comments: &mut Vec<Comment>,
) -> anyhow::Result<Option<http::HeaderMap>> {
    let mut last_headers = None;
    while let Some(after) = cursor {
        let body = CommentsPage::build_query(comments_page::Variables {
            id: subject_node_id.to_string(),
            cursor: Some(after),
        });
        let res = client
            .graphql::<_, comments_page::ResponseData>(&body)
            .await?;
        let page_info = match &res.data.node {
            Some(comments_page::CommentsPageNode::Issue(i)) => {
                comments.extend(map_comment_nodes(
                    subject_node_id,
                    i.comments.nodes.as_deref().unwrap_or(&[]),
                ));
                Some((
                    i.comments.page_info.has_next_page,
                    i.comments.page_info.end_cursor.clone(),
                ))
            }
            Some(comments_page::CommentsPageNode::PullRequest(p)) => {
                comments.extend(map_comment_nodes(
                    subject_node_id,
                    p.comments.nodes.as_deref().unwrap_or(&[]),
                ));
                Some((
                    p.comments.page_info.has_next_page,
                    p.comments.page_info.end_cursor.clone(),
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

/// Fetch all remaining timeline pages for one issue, appending to `cross_refs`.
async fn drain_timeline(
    client: &crate::github::GithubClient,
    issue_node_id: &str,
    mut cursor: Option<String>,
    cross_refs: &mut Vec<CrossRef>,
) -> anyhow::Result<()> {
    while let Some(after) = cursor {
        let body = TimelinePage::build_query(timeline_page::Variables {
            id: issue_node_id.to_string(),
            cursor: Some(after),
        });
        let res = client
            .graphql::<_, timeline_page::ResponseData>(&body)
            .await?;
        let Some(timeline_page::TimelinePageNode::Issue(issue)) = res.data.node else {
            break;
        };
        cross_refs.extend(map_timeline_items(
            issue_node_id,
            issue.timeline_items.nodes.as_deref().unwrap_or(&[]),
        ));
        cursor = next_cursor(
            issue.timeline_items.page_info.has_next_page,
            issue.timeline_items.page_info.end_cursor.as_deref(),
        );
    }
    Ok(())
}

/// Incrementally synchronise issues for `owner/repo`.
///
/// Paginates `repository.issues` (`UPDATED_AT DESC`), early-stopping when an
/// issue's `updatedAt` is strictly below the stored watermark (unless `full`).
/// Each issue (with its fully-paginated comments and cross-refs) is persisted
/// in its own transaction; the page cursor is checkpointed after every page.
/// Returns [`SyncStop::Paused`] when `budget_ok` returns `false` between pages
/// (the checkpoint is already saved), otherwise [`SyncStop::Completed`].
///
/// # Errors
///
/// Returns an error on GraphQL transport/decoding failures, a missing
/// `repository`, or any persistence failure.
pub async fn sync_issues<F>(
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
    // Capture whether this is a fresh (non-resumed) run before mutating the cursor.
    let started_fresh = state.resume_cursor.is_none();
    let mut cursor = state.resume_cursor;
    let mut run_min: Option<DateTime<Utc>> = None;
    // Collect seen node_ids only when doing a full run (for deletion
    // reconciliation).
    let mut seen: HashSet<String> = HashSet::new();

    loop {
        let body = IssuesPage::build_query(issues_page::Variables {
            owner: owner.to_string(),
            repo: repo.to_string(),
            cursor: cursor.clone(),
        });
        let res = client
            .graphql::<_, issues_page::ResponseData>(&body)
            .await?;

        let issues = res
            .data
            .repository
            .ok_or_else(|| anyhow::anyhow!("graphql response had no repository"))?
            .issues;
        let page_info = issues.page_info;
        let nodes = issues.nodes.unwrap_or_default();
        let page_nodes_len = nodes.iter().filter(|n| n.is_some()).count();
        tracing::debug!(
            count = page_nodes_len,
            has_next = page_info.has_next_page,
            "fetched issues page"
        );

        let mut crossed = false;
        for node in nodes.into_iter().flatten() {
            let m = map_issue_node(&node);

            if !full
                && let Some(wm) = watermark
                && m.issue.updated_at < wm
            {
                crossed = true;
                break;
            }

            run_min = Some(run_min.map_or(m.issue.updated_at, |c| c.min(m.issue.updated_at)));

            // Track every processed node_id for deletion reconciliation on full runs.
            if full {
                seen.insert(m.issue.node_id.clone());
            }

            let mut comments = m.comments;
            if let Some(c) = m.comments_more {
                let _headers =
                    drain_comments(client, &m.issue.node_id, Some(c), &mut comments).await?;
            }
            let mut cross_refs = m.cross_refs;
            if let Some(c) = m.timeline_more {
                drain_timeline(client, &m.issue.node_id, Some(c), &mut cross_refs).await?;
            }

            let tx = conn.unchecked_transaction()?;
            upsert_issue(&tx, &m.issue)?;
            replace_comments(&tx, &m.issue.node_id, &comments)?;
            for x in &cross_refs {
                upsert_cross_ref(&tx, x)?;
            }
            tx.commit()?;
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

        if !budget_ok(&res.headers) {
            return Ok(SyncStop::Paused);
        }

        cursor = page_info.end_cursor;
    }

    sync_state::complete(conn, ENTITY, run_min.or(watermark))?;

    // Soft-delete reconciliation runs only on a fresh full pass. A resumed full
    // run has an incomplete seen-set (it re-walked only the remaining pages), so
    // reconciling would wrongly delete entities seen on the skipped pages.
    if full && started_fresh {
        crate::store::issues::mark_deleted_except(conn, &seen)?;
    }

    Ok(SyncStop::Completed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::gql::issues_page;

    #[test]
    fn maps_basic_fields_from_fixture() {
        let fixture = serde_json::json!({
          "id":"I_1","number":42,"title":"Bug","body":"x","state":"OPEN","stateReason":null,
          "createdAt":"2026-01-05T00:00:00Z","updatedAt":"2026-06-10T00:00:00Z","closedAt":null,
          "author":{"__typename":"User","login":"octocat"},"milestone":{"title":"v1.0"},
          "labels":{"nodes":[{"name":"bug"}]},"assignees":{"nodes":[{"login":"octocat"}]},
          "comments":{"totalCount":0,"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]},
          "timelineItems":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}
        });
        let node: issues_page::IssuesPageRepositoryIssuesNodes =
            serde_json::from_value(fixture).unwrap();
        let m = map_issue_node(&node);
        assert_eq!(m.issue.number, 42);
        assert_eq!(m.issue.state, IssueState::Open);
        assert_eq!(m.issue.author.as_deref(), Some("octocat"));
        assert_eq!(m.issue.milestone.as_deref(), Some("v1.0"));
        assert_eq!(m.issue.labels, vec!["bug".to_string()]);
        assert_eq!(m.issue.assignees, vec!["octocat".to_string()]);
        assert_eq!(m.comments_more, None);
        assert_eq!(m.timeline_more, None);
    }

    #[test]
    fn maps_state_reason_more_cursors_and_cross_refs() {
        let fixture = serde_json::json!({
          "id":"I_2","number":7,"title":"Closed","body":"","state":"CLOSED","stateReason":"COMPLETED",
          "createdAt":"2026-01-05T00:00:00Z","updatedAt":"2026-06-10T00:00:00Z",
          "closedAt":"2026-06-11T00:00:00Z",
          "author":null,"milestone":null,
          "labels":{"nodes":[]},"assignees":{"nodes":[]},
          "comments":{"totalCount":1,"pageInfo":{"hasNextPage":true,"endCursor":"CC"},"nodes":[
            {"id":"C_1","createdAt":"2026-02-01T00:00:00Z","body":"hi","author":{"__typename":"User","login":"bot"}}
          ]},
          "timelineItems":{"pageInfo":{"hasNextPage":true,"endCursor":"TT"},"nodes":[
            {"__typename":"CrossReferencedEvent","createdAt":"2026-03-01T00:00:00Z",
             "source":{"__typename":"Issue","number":99}}
          ]}
        });
        let node: issues_page::IssuesPageRepositoryIssuesNodes =
            serde_json::from_value(fixture).unwrap();
        let m = map_issue_node(&node);
        assert_eq!(m.issue.state, IssueState::Closed);
        assert_eq!(m.issue.state_reason.as_deref(), Some("completed"));
        assert_eq!(m.issue.author, None);
        assert_eq!(m.comments.len(), 1);
        assert_eq!(m.comments[0].author.as_deref(), Some("bot"));
        assert_eq!(m.comments_more.as_deref(), Some("CC"));
        assert_eq!(m.timeline_more.as_deref(), Some("TT"));
        assert_eq!(m.cross_refs.len(), 1);
        assert_eq!(m.cross_refs[0].referenced_issue_number, 99);
        assert_eq!(m.cross_refs[0].event_type, CrossRefEvent::CrossReferenced);
    }
}
