//! Shared mapping from generated GraphQL relationship nodes to domain edges.
//!
//! The seven generated node types (four inline on the issues page, three from
//! the drain queries) are structurally identical; [`RelNode`] gives them one
//! accessor surface, mirroring the `CommentNode`/`TimelineItem` pattern in
//! `sync::issues`.

use graphql_client::GraphQLQuery as _;

use crate::github::GithubClient;
use crate::github::gql::BlockedByPage;
use crate::github::gql::BlockingPage;
use crate::github::gql::SubIssuesPage;
use crate::github::gql::blocked_by_page;
use crate::github::gql::blocking_page;
use crate::github::gql::issues_page;
use crate::github::gql::sub_issues_page;
use crate::model::Issue;
use crate::model::IssueState;
use crate::model::RelEdge;
use crate::model::RelEndpoint;
use crate::model::RelKind;
use crate::sync::next_cursor;

/// Accessor surface over a generated relationship target node.
pub(crate) trait RelNode {
    fn id(&self) -> &str;
    fn number(&self) -> i64;
    fn state(&self) -> IssueState;
    fn title(&self) -> &str;
    fn name_with_owner(&self) -> &str;
}

/// Implement [`RelNode`] for a generated node type whose `state` enum lives
/// in the given module.
macro_rules! impl_rel_node {
    ($ty:ty, $state:path) => {
        impl RelNode for $ty {
            fn id(&self) -> &str {
                &self.id
            }
            fn number(&self) -> i64 {
                self.number
            }
            fn state(&self) -> IssueState {
                use $state as S;
                match self.state {
                    S::OPEN => IssueState::Open,
                    _ => IssueState::Closed,
                }
            }
            fn title(&self) -> &str {
                &self.title
            }
            fn name_with_owner(&self) -> &str {
                &self.repository.name_with_owner
            }
        }
    };
}

impl_rel_node!(
    issues_page::IssuesPageRepositoryIssuesNodesParent,
    issues_page::IssueState
);
impl_rel_node!(
    issues_page::IssuesPageRepositoryIssuesNodesSubIssuesNodes,
    issues_page::IssueState
);
impl_rel_node!(
    issues_page::IssuesPageRepositoryIssuesNodesBlockedByNodes,
    issues_page::IssueState
);
impl_rel_node!(
    issues_page::IssuesPageRepositoryIssuesNodesBlockingNodes,
    issues_page::IssueState
);
impl_rel_node!(
    sub_issues_page::SubIssuesPageNodeOnIssueSubIssuesNodes,
    sub_issues_page::IssueState
);
impl_rel_node!(
    blocked_by_page::BlockedByPageNodeOnIssueBlockedByNodes,
    blocked_by_page::IssueState
);
impl_rel_node!(
    blocking_page::BlockingPageNodeOnIssueBlockingNodes,
    blocking_page::IssueState
);

/// Map a generated node to an endpoint. `repo_full` is the synced
/// `owner/repo`; the comparison is ASCII-case-insensitive (GitHub slugs are
/// case-insensitive, and `nameWithOwner` returns canonical casing).
pub(crate) fn endpoint<N: RelNode>(n: &N, repo_full: &str) -> RelEndpoint {
    let nwo = n.name_with_owner();
    let repo = if nwo.eq_ignore_ascii_case(repo_full) {
        None
    } else {
        Some(nwo.to_string())
    };
    RelEndpoint {
        node_id: n.id().to_string(),
        repo,
        number: n.number(),
        state: n.state(),
        title: n.title().to_string(),
    }
}

/// The synced issue itself as an edge endpoint (always same-repo).
pub(crate) fn self_endpoint(issue: &Issue) -> RelEndpoint {
    RelEndpoint {
        node_id: issue.node_id.clone(),
        repo: None,
        number: issue.number,
        state: issue.state,
        title: issue.title.clone(),
    }
}

/// Fetch remaining sub-issue pages for `self_ep`'s issue, appending
/// parent → child edges with positions continuing at `position`.
pub(crate) async fn drain_sub_issues(
    client: &GithubClient,
    self_ep: &RelEndpoint,
    repo_full: &str,
    mut cursor: Option<String>,
    mut position: i64,
    edges: &mut Vec<RelEdge>,
) -> anyhow::Result<()> {
    while let Some(after) = cursor {
        let body = SubIssuesPage::build_query(sub_issues_page::Variables {
            id: self_ep.node_id.clone(),
            cursor: Some(after),
        });
        let res = client
            .graphql::<_, sub_issues_page::ResponseData>(&body)
            .await?;
        let Some(sub_issues_page::SubIssuesPageNode::Issue(issue)) = res.data.node else {
            break;
        };
        for child in issue
            .sub_issues
            .nodes
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .flatten()
        {
            edges.push(RelEdge {
                rel: RelKind::Parent,
                src: self_ep.clone(),
                dst: endpoint(child, repo_full),
                position: Some(position),
            });
            position += 1;
        }
        cursor = next_cursor(
            issue.sub_issues.page_info.has_next_page,
            issue.sub_issues.page_info.end_cursor.as_deref(),
        );
    }
    Ok(())
}

/// Fetch remaining blocked-by pages, appending blocker → `self_ep` edges.
pub(crate) async fn drain_blocked_by(
    client: &GithubClient,
    self_ep: &RelEndpoint,
    repo_full: &str,
    mut cursor: Option<String>,
    edges: &mut Vec<RelEdge>,
) -> anyhow::Result<()> {
    while let Some(after) = cursor {
        let body = BlockedByPage::build_query(blocked_by_page::Variables {
            id: self_ep.node_id.clone(),
            cursor: Some(after),
        });
        let res = client
            .graphql::<_, blocked_by_page::ResponseData>(&body)
            .await?;
        let Some(blocked_by_page::BlockedByPageNode::Issue(issue)) = res.data.node else {
            break;
        };
        for blocker in issue
            .blocked_by
            .nodes
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .flatten()
        {
            edges.push(RelEdge {
                rel: RelKind::Blocks,
                src: endpoint(blocker, repo_full),
                dst: self_ep.clone(),
                position: None,
            });
        }
        cursor = next_cursor(
            issue.blocked_by.page_info.has_next_page,
            issue.blocked_by.page_info.end_cursor.as_deref(),
        );
    }
    Ok(())
}

/// Fetch remaining blocking pages, appending `self_ep` → blocked edges.
pub(crate) async fn drain_blocking(
    client: &GithubClient,
    self_ep: &RelEndpoint,
    repo_full: &str,
    mut cursor: Option<String>,
    edges: &mut Vec<RelEdge>,
) -> anyhow::Result<()> {
    while let Some(after) = cursor {
        let body = BlockingPage::build_query(blocking_page::Variables {
            id: self_ep.node_id.clone(),
            cursor: Some(after),
        });
        let res = client
            .graphql::<_, blocking_page::ResponseData>(&body)
            .await?;
        let Some(blocking_page::BlockingPageNode::Issue(issue)) = res.data.node else {
            break;
        };
        for blocked in issue
            .blocking
            .nodes
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .flatten()
        {
            edges.push(RelEdge {
                rel: RelKind::Blocks,
                src: self_ep.clone(),
                dst: endpoint(blocked, repo_full),
                position: None,
            });
        }
        cursor = next_cursor(
            issue.blocking.page_info.has_next_page,
            issue.blocking.page_info.end_cursor.as_deref(),
        );
    }
    Ok(())
}
