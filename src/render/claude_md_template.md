<!-- meta-fetch:generated — rewritten on every render; edits will be lost. -->

# {{OWNER_REPO}} — offline issue and pull request projection

This directory is **generated, derived data**. It is a snapshot of the GitHub
issues and pull requests of `{{OWNER_REPO}}`, projected into Markdown by
[github-repo-meta-fetch][gh] so an agent can read them without a network call,
an API token, or a rate limit.

[gh]: https://github.com/demosdemon/github-repo-meta-fetch

## Layout

```text
.
├── README.md          entity counts, sync watermarks, last full sync
├── CLAUDE.md          this file
├── labels.md          every label: color, description, usage count
├── milestones.md      every milestone: state, due date, open/closed counts
├── hierarchy.md       repo-wide parent/sub-issue tree
├── issues/
│   ├── NNNN.md        one file per issue
│   ├── by-label/      one index table per label
│   ├── by-milestone/  one index table per milestone
│   └── by-state/      open.md, closed.md
└── prs/
    ├── NNNN.md        one file per pull request
    ├── by-label/
    ├── by-milestone/
    └── by-state/      open.md, draft.md, closed.md, merged.md
```

Each entity file is YAML frontmatter, then the body, then the full comment
thread. PR files add reviews and review threads with their diff hunks.

## Filenames and index slugs

Entity filenames are the issue or PR number zero-padded to {{WIDTH}} digits —
issue 42 is `issues/{{EXAMPLE}}.md`. The width never shrinks, so removing the
highest-numbered item never renumbers files. It does grow when a number gains
a digit — and when it does, every file is renamed at once.

Index filenames are slugs: every character outside `[A-Za-z0-9._-]` becomes
`-`. The label `area: sync` indexes as `by-label/area--sync.md`.

Two distinct names can collide on one slug (`a b` and `a-b` both yield
`a-b.md`), in which case the last one written wins and the other has no index
file. So an absent `by-label/` file is not proof that a label is unused —
check `labels.md`, which lists every label with its usage count.

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

## Index tables

Every file under `by-label/`, `by-milestone/`, and `by-state/` is a single
table, sorted by number ascending, whose first cell links to the entity file:

```markdown
| # | title | state | assignees | updated |
```

`labels.md` and `milestones.md` are repository-wide taxonomy tables:

```markdown
| name | color | count | description |
| # | title | state | due | open | closed |
```

These directories are fully derived — wiped and rebuilt on every render — so
they never disagree with the entity files.

## `hierarchy.md`

The repository-wide parent/sub-issue tree, as nested bullets (shown at pad
width 4 below regardless of this repository's actual width):

```markdown
- [#2 Epic](issues/0002.md) (open)
  - [#3 Sub-task](issues/0003.md) (open)
```

Traversal is capped at 8 levels, matching GitHub's sub-issue nesting limit.
Sub-issues in other repositories appear as plain text marked `— external`;
they have no file here.

## Answering a question

- **Filter or enumerate** — start from the index tables (`by-state/`,
  `by-label/`, `by-milestone/`) and intersect. Read entity files only for the
  survivors.
- **Taxonomy** — `labels.md` and `milestones.md`.
- **Hierarchy** — `hierarchy.md` for the whole tree; the `parent` and
  `sub_issues` frontmatter keys for one item's neighbours.
- **Blocking** — the `blocked_by` and `blocking` frontmatter keys.
- **Free text** — grep `issues/[0-9]*.md` and `prs/[0-9]*.md`. The numeric
  glob matters: it skips the index tables, which would otherwise return a hit
  for every item whose title matches.

**Lookup by number needs both directories.** Issues, pull requests, and GitHub
Discussions share one number sequence per repository, so `#37` is an issue, a
PR, or a discussion — only the first two have files here, and nothing about
the number alone says which. Check `issues/NNNN.md` *and* `prs/NNNN.md`. This
applies to a bare number a user gives you and to every number appearing in
`related` or `closes`.

## Before you answer

- **This is a snapshot, not live GitHub.** For anything recency-sensitive,
  read the watermark in `README.md` and say what it is — "as of the last sync
  at …". Data newer than the watermark is simply not here.
- **A missing `issues/NNNN.md` usually means `#NNNN` is a pull request.**
  Check `prs/NNNN.md` before concluding anything. Only when a number is absent
  from *both* directories is it a discussion (which this tool never syncs),
  deleted, transferred, or not yet synced.
- **Issue state is not PR state.** Issues are `open` or `closed`, refined by
  `state_reason`. Pull requests are `open`, `draft`, `closed`, or `merged` —
  and a merged PR is never `closed`.
- **A relationship link can dangle.** An edge may name a same-repository issue
  that a sync has not reached yet, so its file may not exist. That resolves on
  the next sync; it does not mean the issue is gone.
- **Never edit anything here.** The whole tree is derived data, rewritten on
  every render. Changes are lost, and they never reach GitHub.
