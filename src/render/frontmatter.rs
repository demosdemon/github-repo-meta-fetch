use std::fmt::Write as _;

use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Utc;

use crate::model::Issue;

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

/// Render YAML frontmatter with a FIXED key order. `related` is sorted
/// ascending. Arrays (`labels`, `assignees`) are rendered in the order given —
/// the caller sorts.
#[must_use]
pub fn render(issue: &Issue, related: &[i64], html_url: &str) -> String {
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
        let fm = render(&issue(), &[51, 38], "https://github.com/o/r/issues/42");
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
url: \"https://github.com/o/r/issues/42\"
---
";
        assert_eq!(fm, expected);
    }
}
