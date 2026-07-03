//! Shared mapping from generated GraphQL relationship nodes to domain edges.
//!
//! The seven generated node types (four inline on the issues page, three from
//! the drain queries) are structurally identical; [`RelNode`] gives them one
//! accessor surface, mirroring the `CommentNode`/`TimelineItem` pattern in
//! `sync::issues`.

use crate::github::gql::blocked_by_page;
use crate::github::gql::blocking_page;
use crate::github::gql::issues_page;
use crate::github::gql::sub_issues_page;
use crate::model::Issue;
use crate::model::IssueState;
use crate::model::RelEndpoint;

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
