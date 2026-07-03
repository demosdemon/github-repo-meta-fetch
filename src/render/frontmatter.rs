use std::fmt::Write as _;

use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Utc;

use crate::model::Issue;
use crate::model::IssueState;
use crate::model::RelTarget;
use crate::model::Relationships;
use crate::model::cmp_reference;
use crate::model::cmp_siblings;

pub(super) fn yaml_str(s: &str) -> String {
    // Always quote to keep output stable and safe for colons/leading specials.
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

pub(super) fn yaml_inline_list(items: &[String]) -> String {
    if items.is_empty() {
        return "[]".to_string();
    }
    let inner: Vec<String> = items.iter().map(|s| yaml_str(s)).collect();
    format!("[{}]", inner.join(", "))
}

pub(super) fn rfc3339(dt: &DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(super) fn num_list(nums: &[i64]) -> String {
    if nums.is_empty() {
        return "[]".to_string();
    }
    let mut v = nums.to_vec();
    v.sort_unstable();
    format!(
        "[{}]",
        v.iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Render a sorted relationship list as an inline YAML list of reference
/// strings.
fn ref_list(
    targets: &[RelTarget],
    cmp: fn(&RelTarget, &RelTarget) -> std::cmp::Ordering,
) -> String {
    let mut sorted = targets.to_vec();
    sorted.sort_by(cmp);
    let refs: Vec<String> = sorted.iter().map(RelTarget::reference).collect();
    yaml_inline_list(&refs)
}

/// Render YAML frontmatter with a FIXED key order. `related` is sorted
/// ascending. Arrays (`labels`, `assignees`) are rendered in the order given —
/// the caller sorts. Relationship keys (`parent`, `sub_issues`, `blocked`,
/// `blocked_by`, `blocking`) come from `rels`: `sub_issues` is ordered by
/// GitHub sub-issue position (`cmp_siblings`), `blocked_by`/`blocking` by
/// same-repo-then-external reference (`cmp_reference`), and `blocked` counts
/// only the OPEN entries of `blocked_by`.
#[must_use]
pub fn render(issue: &Issue, related: &[i64], rels: &Relationships, html_url: &str) -> String {
    let mut out = String::new();
    // `write!` on a `String` is infallible; `.ok()` discards the always-Ok result
    // without triggering `let_underscore_drop`.
    writeln!(out, "---").ok();
    writeln!(out, "number: {}", issue.number).ok();
    writeln!(out, "title: {}", yaml_str(&issue.title)).ok();
    writeln!(out, "state: {}", issue.state.as_str()).ok();
    writeln!(
        out,
        "state_reason: {}",
        issue
            .state_reason
            .as_deref()
            .map_or_else(|| "null".into(), yaml_str)
    )
    .ok();
    writeln!(out, "labels: {}", yaml_inline_list(&issue.labels)).ok();
    writeln!(out, "assignees: {}", yaml_inline_list(&issue.assignees)).ok();
    writeln!(
        out,
        "milestone: {}",
        issue
            .milestone
            .as_deref()
            .map_or_else(|| "null".into(), yaml_str)
    )
    .ok();
    writeln!(
        out,
        "author: {}",
        issue
            .author
            .as_deref()
            .map_or_else(|| "null".into(), yaml_str)
    )
    .ok();
    writeln!(out, "created_at: {}", rfc3339(&issue.created_at)).ok();
    writeln!(out, "updated_at: {}", rfc3339(&issue.updated_at)).ok();
    writeln!(
        out,
        "closed_at: {}",
        issue
            .closed_at
            .map_or_else(|| "null".into(), |d| rfc3339(&d))
    )
    .ok();
    writeln!(out, "related: {}", num_list(related)).ok();
    writeln!(
        out,
        "parent: {}",
        rels.parent
            .as_ref()
            .map_or_else(|| "null".into(), |t| yaml_str(&t.reference()))
    )
    .ok();
    writeln!(
        out,
        "sub_issues: {}",
        ref_list(&rels.sub_issues, cmp_siblings)
    )
    .ok();
    let blocked = rels
        .blocked_by
        .iter()
        .filter(|t| t.state == IssueState::Open)
        .count();
    writeln!(out, "blocked: {blocked}").ok();
    writeln!(
        out,
        "blocked_by: {}",
        ref_list(&rels.blocked_by, cmp_reference)
    )
    .ok();
    writeln!(out, "blocking: {}", ref_list(&rels.blocking, cmp_reference)).ok();
    writeln!(out, "url: {}", yaml_str(html_url)).ok();
    writeln!(out, "---").ok();
    out
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use chrono::Utc;

    use super::*;
    use crate::model::IssueState;
    use crate::model::RelTarget;
    use crate::model::Relationships;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn issue() -> Issue {
        Issue {
            node_id: "I1".into(),
            number: 42,
            title: "Bug: x".into(),
            state: IssueState::Open,
            state_reason: None,
            author: Some("octocat".into()),
            body: "b".into(),
            created_at: dt("2026-01-05T00:00:00Z"),
            updated_at: dt("2026-06-10T00:00:00Z"),
            closed_at: None,
            milestone: Some("v1.0".into()),
            labels: vec!["bug".into(), "area: sync".into()],
            assignees: vec!["octocat".into()],
            deleted: false,
        }
    }

    #[test]
    fn deterministic_key_order_and_related_sorted() {
        let fm = render(
            &issue(),
            &[51, 38],
            &Relationships::default(),
            "https://github.com/o/r/issues/42",
        );
        let expected = "\
---
number: 42
title: \"Bug: x\"
state: open
state_reason: null
labels: [\"bug\", \"area: sync\"]
assignees: [\"octocat\"]
milestone: \"v1.0\"
author: \"octocat\"
created_at: 2026-01-05T00:00:00Z
updated_at: 2026-06-10T00:00:00Z
closed_at: null
related: [38, 51]
parent: null
sub_issues: []
blocked: 0
blocked_by: []
blocking: []
url: \"https://github.com/o/r/issues/42\"
---
";
        assert_eq!(fm, expected);
    }

    fn rel(repo: Option<&str>, number: i64, state: IssueState, position: Option<i64>) -> RelTarget {
        RelTarget {
            node_id: format!("N{number}"),
            repo: repo.map(str::to_string),
            number,
            state,
            title: "t".into(),
            position,
        }
    }

    #[test]
    fn relationship_keys_sorted_and_counted() {
        let rels = Relationships {
            parent: Some(rel(None, 12, IssueState::Open, None)),
            // Deliberately unsorted; position order is 51, 47, 63.
            sub_issues: vec![
                rel(None, 63, IssueState::Open, Some(2)),
                rel(None, 51, IssueState::Open, Some(0)),
                rel(None, 47, IssueState::Open, Some(1)),
            ],
            blocked_by: vec![
                rel(Some("acme/infra"), 4, IssueState::Open, None),
                rel(None, 9, IssueState::Open, None),
                rel(None, 3, IssueState::Closed, None),
            ],
            blocking: vec![rel(None, 102, IssueState::Open, None)],
        };
        let fm = render(&issue(), &[], &rels, "https://github.com/o/r/issues/42");
        assert!(fm.contains("parent: \"#12\"\n"), "{fm}");
        assert!(
            fm.contains("sub_issues: [\"#51\", \"#47\", \"#63\"]\n"),
            "{fm}"
        );
        // Closed blocker #3 does not count; open #9 and acme/infra#4 do.
        assert!(fm.contains("blocked: 2\n"), "{fm}");
        assert!(
            fm.contains("blocked_by: [\"#3\", \"#9\", \"acme/infra#4\"]\n"),
            "{fm}"
        );
        assert!(fm.contains("blocking: [\"#102\"]\n"), "{fm}");
    }
}
