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
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "consumed by render_tree once a later task wires the CLAUDE.md write into \
                  the pipeline; the test build already reads it via the tests below"
    )
)]
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
}
