<!-- meta-fetch:generated — rewritten on every render; edits will be lost. -->

# {{OWNER_REPO}} — offline issue and pull request projection

This directory is **generated, derived data**. It is a snapshot of the GitHub
issues and pull requests of `{{OWNER_REPO}}`, projected into Markdown by
[github-repo-meta-fetch][gh] so an agent can read them without a network call,
an API token, or a rate limit.

Entity filenames are zero-padded to {{WIDTH}} digits — issue 42 is
`issues/{{EXAMPLE}}.md`.

[gh]: https://github.com/demosdemon/github-repo-meta-fetch

## Issue frontmatter

Every `issues/NNNN.md` opens with this block, keys always in this order:

```yaml
number: 42
title: "Sync drops the last page"
state: open                       # open | closed
state_reason: null                # "completed" | "not_planned" | "reopened"
labels: ["bug", "area: sync"]
assignees: ["octocat"]
milestone: "v1.0"                 # null when unassigned
author: "octocat"                 # null when the account is deleted
created_at: 2026-01-05T00:00:00Z
updated_at: 2026-06-10T00:00:00Z
closed_at: null
related: [12, 40]
parent: "#5"
sub_issues: ["#7", "#9"]
blocked: 1
blocked_by: ["#3"]
blocking: ["#8", "acme/infra#4"]
url: "https://github.com/{{OWNER_REPO}}/issues/42"
```

Two shapes are easy to confuse:

- `related` holds **bare numbers** — cross-referenced issues or PRs.
- `parent`, `sub_issues`, `blocked_by`, and `blocking` hold **reference
  strings**: `"#7"` for this repository, `"owner/repo#7"` for another one.
  Cross-repository targets have no file here.

`blocked` is a count, not a list: the number of `blocked_by` entries that are
still **open**. `blocked: 0` alongside a non-empty `blocked_by` means every
blocker is closed.

## Pull request frontmatter

Every `prs/NNNN.md` opens with this block, keys always in this order:

```yaml
number: 108
title: "Stamp the watermark with the newest updatedAt"
state: merged                     # open | draft | closed | merged
draft: false
base: "main"
head: "fix/watermark"
author: "octocat"
created_at: 2026-06-01T00:00:00Z
updated_at: 2026-06-14T00:00:00Z
closed_at: 2026-06-14T00:00:00Z
merged_at: 2026-06-14T00:00:00Z   # null unless merged
merged_by: "demosdemon"           # null unless merged
additions: 42
deletions: 7
changed_files: 3
labels: ["bug"]
assignees: []
milestone: null
closes: [37]                      # issues this PR closes, bare numbers
related: [12]
url: "https://github.com/{{OWNER_REPO}}/pull/108"
```

`state` here is an **effective** state that folds in `draft` and `merged`; it
is not the raw GitHub state. A merged PR is `merged`, never `closed`.
