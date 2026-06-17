use std::fmt::Write as _;

use crate::model::Comment;
use crate::model::Issue;
use crate::render::frontmatter;
use crate::render::frontmatter::rfc3339;

/// Full per-issue document: frontmatter + title + body + comments.
#[must_use]
pub fn render(issue: &Issue, related: &[i64], comments: &[Comment], html_url: &str) -> String {
    let mut out = frontmatter::render(issue, related, html_url);
    out.push('\n');
    writeln!(out, "# #{} — {}", issue.number, issue.title).ok();
    out.push('\n');
    out.push_str(issue.body.trim_end());
    out.push_str("\n\n");
    writeln!(out, "## Comments ({})", comments.len()).ok();
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
    out
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use chrono::Utc;

    use super::*;
    use crate::model::IssueState;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn snapshot_issue_document() {
        let issue = Issue {
            node_id: "I1".into(),
            number: 42,
            title: "Bug".into(),
            state: IssueState::Open,
            state_reason: None,
            author: Some("octocat".into()),
            body: "Something is broken.".into(),
            created_at: dt("2026-01-05T00:00:00Z"),
            updated_at: dt("2026-06-10T00:00:00Z"),
            closed_at: None,
            milestone: None,
            labels: vec!["bug".into()],
            assignees: vec![],
            deleted: false,
        };
        let comments = vec![Comment {
            node_id: "C1".into(),
            subject_node_id: "I1".into(),
            author: Some("hubot".into()),
            created_at: dt("2026-01-06T00:00:00Z"),
            body: "Thanks for the report.".into(),
        }];
        let doc = render(&issue, &[], &comments, "https://github.com/o/r/issues/42");
        insta::assert_snapshot!(doc, @r#"
---
number: 42
title: "Bug"
state: open
state_reason: null
labels: ["bug"]
assignees: []
milestone: null
author: "octocat"
created_at: 2026-01-05T00:00:00Z
updated_at: 2026-06-10T00:00:00Z
closed_at: null
related: []
url: "https://github.com/o/r/issues/42"
---

# #42 — Bug

Something is broken.

## Comments (1)

### hubot · 2026-01-06T00:00:00Z
Thanks for the report.
"#);
    }
}
