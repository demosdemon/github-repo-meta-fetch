use chrono::DateTime;
use chrono::Utc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueState {
    Open,
    Closed,
}

impl IssueState {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            IssueState::Open => "open",
            IssueState::Closed => "closed",
        }
    }
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "open" => Some(IssueState::Open),
            "closed" => Some(IssueState::Closed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossRefEvent {
    CrossReferenced,
    Connected,
    Disconnected,
    MarkedAsDuplicate,
    UnmarkedAsDuplicate,
    ClosingReference,
}

impl CrossRefEvent {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            CrossRefEvent::CrossReferenced => "CROSS_REFERENCED_EVENT",
            CrossRefEvent::Connected => "CONNECTED_EVENT",
            CrossRefEvent::Disconnected => "DISCONNECTED_EVENT",
            CrossRefEvent::MarkedAsDuplicate => "MARKED_AS_DUPLICATE_EVENT",
            CrossRefEvent::UnmarkedAsDuplicate => "UNMARKED_AS_DUPLICATE_EVENT",
            CrossRefEvent::ClosingReference => "CLOSING_REFERENCE",
        }
    }
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "CROSS_REFERENCED_EVENT" => Some(CrossRefEvent::CrossReferenced),
            "CONNECTED_EVENT" => Some(CrossRefEvent::Connected),
            "DISCONNECTED_EVENT" => Some(CrossRefEvent::Disconnected),
            "MARKED_AS_DUPLICATE_EVENT" => Some(CrossRefEvent::MarkedAsDuplicate),
            "UNMARKED_AS_DUPLICATE_EVENT" => Some(CrossRefEvent::UnmarkedAsDuplicate),
            "CLOSING_REFERENCE" => Some(CrossRefEvent::ClosingReference),
            _ => None,
        }
    }
    /// Events that establish an active relationship (vs. ones that revoke it).
    #[must_use]
    pub fn is_link(&self) -> bool {
        matches!(
            self,
            CrossRefEvent::CrossReferenced
                | CrossRefEvent::Connected
                | CrossRefEvent::MarkedAsDuplicate
                | CrossRefEvent::ClosingReference
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSlug {
    pub owner: String,
    pub repo: String,
}

impl std::str::FromStr for RepoSlug {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (owner, repo) = s
            .split_once('/')
            .ok_or_else(|| format!("expected owner/repo, got {s:?}"))?;
        if owner.is_empty() || repo.is_empty() || repo.contains('/') {
            return Err(format!("invalid owner/repo: {s:?}"));
        }
        Ok(RepoSlug {
            owner: owner.to_string(),
            repo: repo.to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub node_id: String,
    pub number: i64,
    pub title: String,
    pub state: IssueState,
    pub state_reason: Option<String>,
    pub author: Option<String>,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub milestone: Option<String>,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub node_id: String,
    pub subject_node_id: String,
    pub author: Option<String>,
    pub created_at: DateTime<Utc>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossRef {
    pub issue_node_id: String,
    pub referenced_issue_number: i64,
    pub event_type: CrossRefEvent,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub node_id: String,
    pub name: String,
    pub color: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Milestone {
    pub node_id: String,
    pub number: i64,
    pub title: String,
    pub state: String,
    pub description: Option<String>,
    pub due_on: Option<DateTime<Utc>>,
}

/// Effective, mutually-exclusive PR state used for `by-state` bucketing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Draft,
    Closed,
    Merged,
}

impl PrState {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            PrState::Open => "open",
            PrState::Draft => "draft",
            PrState::Closed => "closed",
            PrState::Merged => "merged",
        }
    }

    /// Derive the effective state from raw parts: merged > closed > draft >
    /// open.
    #[must_use]
    pub fn from_parts(state: &str, is_draft: bool, merged: bool) -> Self {
        if merged {
            PrState::Merged
        } else if state == "CLOSED" {
            PrState::Closed
        } else if is_draft {
            PrState::Draft
        } else {
            PrState::Open
        }
    }
}

/// The state of a single pull-request review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    Pending,
}

impl ReviewState {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            ReviewState::Approved => "APPROVED",
            ReviewState::ChangesRequested => "CHANGES_REQUESTED",
            ReviewState::Commented => "COMMENTED",
            ReviewState::Dismissed => "DISMISSED",
            ReviewState::Pending => "PENDING",
        }
    }
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "APPROVED" => Some(ReviewState::Approved),
            "CHANGES_REQUESTED" => Some(ReviewState::ChangesRequested),
            "COMMENTED" => Some(ReviewState::Commented),
            "DISMISSED" => Some(ReviewState::Dismissed),
            "PENDING" => Some(ReviewState::Pending),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub node_id: String,
    pub number: i64,
    pub title: String,
    /// Raw GraphQL state: `OPEN` | `CLOSED` | `MERGED`.
    pub state: String,
    pub is_draft: bool,
    pub merged: bool,
    pub merged_at: Option<DateTime<Utc>>,
    pub merged_by: Option<String>,
    pub base_ref: String,
    pub head_ref: String,
    pub additions: i64,
    pub deletions: i64,
    pub changed_files: i64,
    pub author: Option<String>,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub milestone: Option<String>,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub deleted: bool,
}

impl PullRequest {
    /// Effective bucket state: merged > closed > draft > open.
    #[must_use]
    pub fn effective_state(&self) -> PrState {
        PrState::from_parts(&self.state, self.is_draft, self.merged)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub node_id: String,
    pub pr_node_id: String,
    pub author: Option<String>,
    pub state: ReviewState,
    pub body: String,
    /// `None` for `PENDING` reviews.
    pub submitted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewComment {
    pub node_id: String,
    pub thread_node_id: String,
    pub author: Option<String>,
    pub created_at: DateTime<Utc>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewThread {
    pub node_id: String,
    pub pr_node_id: String,
    pub path: String,
    /// `None` for fully-outdated threads.
    pub line: Option<i64>,
    pub is_resolved: bool,
    pub is_outdated: bool,
    /// Sourced from the thread's first comment's `diffHunk`.
    pub diff_hunk: String,
    pub comments: Vec<ReviewComment>,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn slug_parses_owner_repo() {
        let s = RepoSlug::from_str("octocat/hello-world").unwrap();
        assert_eq!(s.owner, "octocat");
        assert_eq!(s.repo, "hello-world");
    }

    #[test]
    fn slug_rejects_bad_input() {
        assert!(RepoSlug::from_str("noslash").is_err());
        assert!(RepoSlug::from_str("a/b/c").is_err());
        assert!(RepoSlug::from_str("/x").is_err());
    }

    #[test]
    fn cross_ref_event_round_trips() {
        for e in [
            CrossRefEvent::CrossReferenced,
            CrossRefEvent::Connected,
            CrossRefEvent::Disconnected,
            CrossRefEvent::MarkedAsDuplicate,
            CrossRefEvent::UnmarkedAsDuplicate,
            CrossRefEvent::ClosingReference,
        ] {
            assert_eq!(CrossRefEvent::parse(e.as_str()), Some(e));
        }
        assert!(CrossRefEvent::CrossReferenced.is_link());
        assert!(!CrossRefEvent::Disconnected.is_link());
    }

    #[test]
    fn issue_state_round_trips() {
        assert_eq!(IssueState::parse("OPEN"), Some(IssueState::Open));
        assert_eq!(IssueState::Closed.as_str(), "closed");
    }

    #[test]
    fn review_state_round_trips() {
        for s in [
            ReviewState::Approved,
            ReviewState::ChangesRequested,
            ReviewState::Commented,
            ReviewState::Dismissed,
            ReviewState::Pending,
        ] {
            assert_eq!(ReviewState::parse(s.as_str()), Some(s));
        }
        assert_eq!(ReviewState::parse("APPROVED"), Some(ReviewState::Approved));
        assert_eq!(ReviewState::parse("nope"), None);
    }

    #[test]
    fn closing_reference_is_a_link_event() {
        assert!(CrossRefEvent::ClosingReference.is_link());
        assert_eq!(
            CrossRefEvent::parse("CLOSING_REFERENCE"),
            Some(CrossRefEvent::ClosingReference)
        );
    }

    #[test]
    fn effective_pr_state_precedence() {
        let base = PullRequest {
            node_id: "P".into(),
            number: 1,
            title: "t".into(),
            state: "OPEN".into(),
            is_draft: false,
            merged: false,
            merged_at: None,
            merged_by: None,
            base_ref: "main".into(),
            head_ref: "f".into(),
            additions: 0,
            deletions: 0,
            changed_files: 0,
            author: None,
            body: String::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            closed_at: None,
            milestone: None,
            labels: vec![],
            assignees: vec![],
            deleted: false,
        };
        let merged = PullRequest {
            merged: true,
            state: "MERGED".into(),
            is_draft: true,
            ..base.clone()
        };
        assert_eq!(merged.effective_state(), PrState::Merged);
        let closed = PullRequest {
            state: "CLOSED".into(),
            ..base.clone()
        };
        assert_eq!(closed.effective_state(), PrState::Closed);
        let draft = PullRequest {
            is_draft: true,
            ..base.clone()
        };
        assert_eq!(draft.effective_state(), PrState::Draft);
        assert_eq!(base.effective_state(), PrState::Open);
        assert_eq!(PrState::Merged.as_str(), "merged");
    }
}
