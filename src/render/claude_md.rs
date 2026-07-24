//! The agent-facing schema guide rendered to `<out>/CLAUDE.md`.
//!
//! The tree this crate projects is meant to be read by AI coding agents. This
//! module makes it self-describing, so an agent does not have to infer the
//! layout or guess the frontmatter key set. Every fact the document states is
//! pinned by a test in this file that compares it against the code that
//! actually produces it.

/// Recognises a `CLAUDE.md` as this tool's own output.
///
/// FROZEN — never change these bytes. Every tree ever rendered carries this
/// exact sequence, and it is the only thing distinguishing our output from a
/// file a human wrote. Changing it strands every previously rendered tree as
/// unrecognisable, permanently skipped and warned about. The prose *after* the
/// sentinel on that line is free to be reworded; this prefix is not.
pub(super) const SENTINEL: &str = "<!-- meta-fetch:generated";

const TEMPLATE: &str = include_str!("claude_md_template.md");

/// Render the agent-facing schema guide for a projected tree.
///
/// Takes plain values rather than `&RepoMeta` so this module depends on
/// nothing from `store`: one input shape, one output, no I/O.
#[must_use]
pub fn render(owner: &str, repo: &str, width: usize) -> String {
    TEMPLATE
        .replace("{{OWNER_REPO}}", &format!("{owner}/{repo}"))
        .replace("{{WIDTH}}", &width.to_string())
        .replace("{{EXAMPLE}}", &format!("{:0width$}", 42))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Issue;
    use crate::model::IssueState;
    use crate::model::PullRequest;
    use crate::model::Relationships;
    use crate::render::frontmatter;
    use crate::render::hierarchy;
    use crate::render::index_slug;
    use crate::render::indexes;
    use crate::render::pr;

    /// Leading `key:` names from top-level lines. Blank lines, comment lines,
    /// and indented continuations are ignored, so annotations in the template
    /// do not read as keys.
    fn key_names(block: &str) -> Vec<String> {
        block
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter(|l| !l.starts_with(char::is_whitespace))
            .filter_map(|l| l.split_once(':').map(|(k, _)| k.trim().to_string()))
            .collect()
    }

    /// Key names from the fenced `yaml` block under the given `##` heading.
    fn documented_keys(doc: &str, heading: &str) -> Vec<String> {
        let after = doc
            .split_once(heading)
            .unwrap_or_else(|| panic!("heading not found in CLAUDE.md: {heading}"))
            .1;
        let block = after
            .split_once("```yaml\n")
            .unwrap_or_else(|| panic!("no yaml block under heading: {heading}"))
            .1
            .split_once("\n```")
            .unwrap_or_else(|| panic!("unterminated yaml block under: {heading}"))
            .0;
        key_names(block)
    }

    /// Key names from a rendered `---`-delimited frontmatter block.
    fn emitted_keys(rendered: &str) -> Vec<String> {
        let body = rendered
            .strip_prefix("---\n")
            .expect("frontmatter must open with ---")
            .split_once("\n---")
            .expect("frontmatter must close with ---")
            .0;
        key_names(body)
    }

    fn dt(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn issue_fixture() -> Issue {
        Issue {
            node_id: "I_1".into(),
            number: 42,
            title: "Bug".into(),
            state: IssueState::Open,
            state_reason: None,
            author: Some("octocat".into()),
            body: "body".into(),
            created_at: dt("2026-01-01T00:00:00Z"),
            updated_at: dt("2026-01-02T00:00:00Z"),
            closed_at: None,
            milestone: None,
            labels: vec![],
            assignees: vec![],
            deleted: false,
        }
    }

    fn pr_fixture() -> PullRequest {
        PullRequest {
            node_id: "PR_1".into(),
            number: 108,
            title: "Fix".into(),
            state: "OPEN".into(),
            is_draft: false,
            merged: false,
            merged_at: None,
            merged_by: None,
            base_ref: "main".into(),
            head_ref: "feature".into(),
            additions: 10,
            deletions: 2,
            changed_files: 3,
            author: Some("octocat".into()),
            body: "body".into(),
            created_at: dt("2026-01-01T00:00:00Z"),
            updated_at: dt("2026-01-02T00:00:00Z"),
            closed_at: None,
            milestone: None,
            labels: vec![],
            assignees: vec![],
            deleted: false,
        }
    }

    #[test]
    fn documented_issue_frontmatter_matches_render() {
        let doc = render("octocat", "hello-world", 4);
        let emitted = frontmatter::render(
            &issue_fixture(),
            &[],
            &Relationships::default(),
            "https://github.com/octocat/hello-world/issues/42",
        );
        assert_eq!(
            documented_keys(&doc, "## Issue frontmatter"),
            emitted_keys(&emitted),
            "CLAUDE.md documents different issue frontmatter keys than \
             frontmatter::render emits — update the template"
        );
    }

    #[test]
    fn documented_pr_frontmatter_matches_render() {
        let doc = render("octocat", "hello-world", 4);
        let emitted = pr::render(
            &pr_fixture(),
            &[],
            &[],
            &[],
            &[],
            &[],
            "https://github.com/octocat/hello-world/pull/108",
        );
        assert_eq!(
            documented_keys(&doc, "## Pull request frontmatter"),
            emitted_keys(&emitted),
            "CLAUDE.md documents different PR frontmatter keys than \
             pr::render emits — update the template"
        );
    }

    #[test]
    fn render_leaves_no_placeholders() {
        let doc = render("octocat", "hello-world", 4);
        assert!(
            !doc.contains("{{"),
            "an unsubstituted placeholder reached the output:\n{doc}"
        );
    }

    #[test]
    fn rendered_doc_carries_sentinel() {
        let doc = render("octocat", "hello-world", 4);
        let first = doc.lines().next().expect("document must not be empty");
        assert!(
            first.starts_with(SENTINEL),
            "first line must open with the frozen sentinel, got: {first}"
        );
    }

    #[test]
    fn render_interpolates_repo_and_width() {
        let doc = render("octocat", "hello-world", 4);
        assert!(doc.contains("octocat/hello-world"), "owner/repo missing");
        assert!(doc.contains("0042.md"), "example filename missing");

        let wide = render("octocat", "hello-world", 6);
        assert!(
            wide.contains("000042.md"),
            "example filename must follow the pad width"
        );
    }

    #[test]
    fn documented_headers_match_render() {
        let doc = render("octocat", "hello-world", 4);
        let cases = [
            (
                "| # | title | state | assignees | updated |",
                indexes::issue_table(&[]),
            ),
            (
                "| name | color | count | description |",
                indexes::labels_doc(&[]),
            ),
            (
                "| # | title | state | due | open | closed |",
                indexes::milestones_doc(&[]),
            ),
        ];
        for (header, rendered) in cases {
            assert!(
                doc.contains(header),
                "CLAUDE.md is missing header: {header}"
            );
            // Anchor to the header row's trailing newline, not just a
            // substring match — otherwise a column silently appended to the
            // rendered header (e.g. an extra `| new |` on the end) would
            // still contain the documented prefix and pass vacuously.
            assert!(
                rendered.contains(&format!("{header}\n")),
                "renderer no longer emits the header CLAUDE.md documents: {header}"
            );
        }
    }

    #[test]
    fn documented_depth_cap_matches_const() {
        let doc = render("octocat", "hello-world", 4);
        let phrase = format!("{} levels", hierarchy::MAX_DEPTH);
        assert!(
            doc.contains(&phrase),
            "CLAUDE.md must state the nesting cap as `{phrase}`"
        );
    }

    #[test]
    fn documented_slug_example_matches_render() {
        let doc = render("octocat", "hello-world", 4);
        assert!(
            doc.contains("by-label/area--sync.md"),
            "CLAUDE.md must carry the worked slug example"
        );
        assert_eq!(
            index_slug("area: sync"),
            "area--sync",
            "the slug rule changed; the worked example in CLAUDE.md is now wrong"
        );
    }

    #[test]
    fn snapshot_claude_md() {
        insta::assert_snapshot!(render("octocat", "hello-world", 4));
    }
}
