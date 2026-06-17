use std::fmt::Write as _;

use chrono::DateTime;
use chrono::Utc;

use crate::model::Comment;
use crate::model::PullRequest;
use crate::model::Review;
use crate::model::ReviewThread;
use crate::render::frontmatter::num_list;
use crate::render::frontmatter::rfc3339;
use crate::render::frontmatter::yaml_inline_list;
use crate::render::frontmatter::yaml_str;

fn opt_ts(d: Option<&DateTime<Utc>>) -> String {
    d.map_or_else(|| "null".into(), rfc3339)
}

fn opt_str(s: Option<&str>) -> String {
    s.map_or_else(|| "null".into(), yaml_str)
}

/// Render the full per-PR document: frontmatter + body + conversation + reviews
/// + threads.
///
/// `related` and `closes` are issue/PR numbers (sorted here); `comments` is the
/// PR conversation; `reviews` and `threads` are pre-ordered by the store reads.
#[must_use]
pub fn render(
    pr: &PullRequest,
    related: &[i64],
    closes: &[i64],
    comments: &[Comment],
    reviews: &[Review],
    threads: &[ReviewThread],
    html_url: &str,
) -> String {
    let mut out = String::new();
    writeln!(out, "---").ok();
    writeln!(out, "number: {}", pr.number).ok();
    writeln!(out, "title: {}", yaml_str(&pr.title)).ok();
    writeln!(out, "state: {}", pr.effective_state().as_str()).ok();
    writeln!(out, "draft: {}", pr.is_draft).ok();
    writeln!(out, "base: {}", yaml_str(&pr.base_ref)).ok();
    writeln!(out, "head: {}", yaml_str(&pr.head_ref)).ok();
    writeln!(out, "author: {}", opt_str(pr.author.as_deref())).ok();
    writeln!(out, "created_at: {}", rfc3339(&pr.created_at)).ok();
    writeln!(out, "updated_at: {}", rfc3339(&pr.updated_at)).ok();
    writeln!(out, "closed_at: {}", opt_ts(pr.closed_at.as_ref())).ok();
    writeln!(out, "merged_at: {}", opt_ts(pr.merged_at.as_ref())).ok();
    writeln!(out, "merged_by: {}", opt_str(pr.merged_by.as_deref())).ok();
    writeln!(out, "additions: {}", pr.additions).ok();
    writeln!(out, "deletions: {}", pr.deletions).ok();
    writeln!(out, "changed_files: {}", pr.changed_files).ok();
    writeln!(out, "labels: {}", yaml_inline_list(&pr.labels)).ok();
    writeln!(out, "assignees: {}", yaml_inline_list(&pr.assignees)).ok();
    writeln!(out, "milestone: {}", opt_str(pr.milestone.as_deref())).ok();
    writeln!(out, "closes: {}", num_list(closes)).ok();
    writeln!(out, "related: {}", num_list(related)).ok();
    writeln!(out, "url: {}", yaml_str(html_url)).ok();
    writeln!(out, "---").ok();

    out.push('\n');
    writeln!(out, "# #{} — {}", pr.number, pr.title).ok();
    out.push('\n');
    out.push_str(pr.body.trim_end());
    out.push_str("\n\n");

    writeln!(out, "## Conversation ({})", comments.len()).ok();
    for c in comments {
        out.push('\n');
        writeln!(
            out,
            "### {} · {}",
            c.author.as_deref().unwrap_or("(ghost)"),
            rfc3339(&c.created_at)
        )
        .ok();
        out.push_str(c.body.trim_end());
        out.push('\n');
    }

    out.push('\n');
    writeln!(out, "## Reviews ({})", reviews.len()).ok();
    for r in reviews {
        out.push('\n');
        let when = r.submitted_at.as_ref().map_or_else(|| "—".into(), rfc3339);
        writeln!(
            out,
            "### {} · {} · {}",
            r.author.as_deref().unwrap_or("(ghost)"),
            r.state.as_str(),
            when
        )
        .ok();
        out.push_str(r.body.trim_end());
        out.push('\n');
    }

    out.push('\n');
    writeln!(out, "## Review threads ({})", threads.len()).ok();
    for t in threads {
        out.push('\n');
        let anchor = match t.line {
            Some(l) => format!("{}:{l}", t.path),
            None => t.path.clone(),
        };
        let status = if t.is_resolved {
            "resolved"
        } else {
            "unresolved"
        };
        let outdated = if t.is_outdated { " · outdated" } else { "" };
        writeln!(out, "### {anchor} · {status}{outdated}").ok();
        if !t.diff_hunk.is_empty() {
            writeln!(out, "```diff").ok();
            out.push_str(t.diff_hunk.trim_end());
            out.push('\n');
            writeln!(out, "```").ok();
        }
        for c in &t.comments {
            out.push('\n');
            writeln!(
                out,
                "**{} · {}:** {}",
                c.author.as_deref().unwrap_or("(ghost)"),
                rfc3339(&c.created_at),
                c.body.trim_end()
            )
            .ok();
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use chrono::Utc;

    use super::*;
    use crate::model::PullRequest;
    use crate::model::Review;
    use crate::model::ReviewComment;
    use crate::model::ReviewState;
    use crate::model::ReviewThread;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn sample_pr() -> PullRequest {
        PullRequest {
            node_id: "PR_1".into(),
            number: 42,
            title: "Add PRs".into(),
            state: "MERGED".into(),
            is_draft: false,
            merged: true,
            merged_at: Some(dt("2026-06-14T00:00:00Z")),
            merged_by: Some("demosdemon".into()),
            base_ref: "main".into(),
            head_ref: "feature/prs".into(),
            additions: 412,
            deletions: 87,
            changed_files: 9,
            author: Some("octocat".into()),
            body: "Implements PR sync.".into(),
            created_at: dt("2026-06-10T00:00:00Z"),
            updated_at: dt("2026-06-14T00:00:00Z"),
            closed_at: Some(dt("2026-06-14T00:00:00Z")),
            milestone: Some("v1.0".into()),
            labels: vec!["area: sync".into(), "bug".into()],
            assignees: vec!["demosdemon".into()],
            deleted: false,
        }
    }

    #[test]
    fn snapshot_pr_document() {
        let reviews = vec![Review {
            node_id: "R1".into(),
            pr_node_id: "PR_1".into(),
            author: Some("demosdemon".into()),
            state: ReviewState::Approved,
            body: "LGTM".into(),
            submitted_at: Some(dt("2026-06-14T00:00:00Z")),
        }];
        let threads = vec![ReviewThread {
            node_id: "T1".into(),
            pr_node_id: "PR_1".into(),
            path: "src/sync/mod.rs".into(),
            line: Some(88),
            is_resolved: true,
            is_outdated: false,
            diff_hunk: "@@ -85,3 +85,4 @@\n-old\n+new".into(),
            comments: vec![ReviewComment {
                node_id: "RC1".into(),
                thread_node_id: "T1".into(),
                author: Some("demosdemon".into()),
                created_at: dt("2026-06-11T00:00:00Z"),
                body: "pass the query type".into(),
            }],
        }];
        let doc = render(
            &sample_pr(),
            &[38, 41],
            &[41],
            &[],
            &reviews,
            &threads,
            "https://github.com/o/r/pull/42",
        );
        insta::assert_snapshot!(doc);
    }

    fn draft_pr() -> PullRequest {
        PullRequest {
            node_id: "PR_2".into(),
            number: 7,
            title: "WIP".into(),
            state: "OPEN".into(),
            is_draft: true,
            merged: false,
            merged_at: None,
            merged_by: None,
            base_ref: "main".into(),
            head_ref: "wip".into(),
            additions: 1,
            deletions: 0,
            changed_files: 1,
            author: None,
            body: "Work in progress.".into(),
            created_at: dt("2026-06-10T00:00:00Z"),
            updated_at: dt("2026-06-12T00:00:00Z"),
            closed_at: None,
            milestone: None,
            labels: vec![],
            assignees: vec![],
            deleted: false,
        }
    }

    #[test]
    fn snapshot_pr_edge_cases() {
        let reviews = vec![Review {
            node_id: "R1".into(),
            pr_node_id: "PR_2".into(),
            author: Some("octocat".into()),
            state: ReviewState::Pending,
            body: "still looking".into(),
            submitted_at: None,
        }];
        let threads = vec![ReviewThread {
            node_id: "T1".into(),
            pr_node_id: "PR_2".into(),
            path: "src/lib.rs".into(),
            line: None,
            is_resolved: false,
            is_outdated: true,
            diff_hunk: String::new(),
            comments: vec![ReviewComment {
                node_id: "RC1".into(),
                thread_node_id: "T1".into(),
                author: None,
                created_at: dt("2026-06-11T00:00:00Z"),
                body: "this moved".into(),
            }],
        }];
        let doc = render(
            &draft_pr(),
            &[],
            &[],
            &[],
            &reviews,
            &threads,
            "https://github.com/o/r/pull/7",
        );
        insta::assert_snapshot!(doc);
    }
}
