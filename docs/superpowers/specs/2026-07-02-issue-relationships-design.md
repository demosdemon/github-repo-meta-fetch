# Design: Issue relationships (dependencies and sub-issues)

**Status:** Approved — not yet implemented
**Date:** 2026-07-02

## Summary

Sync GitHub issue relationships — dependency edges ("blocked by" / "blocks") and
hierarchy edges (parent / sub-issues) — into the SQLite cache, and project them
into the rendered Markdown tree two ways:

1. Five new keys in per-issue YAML frontmatter: `parent`, `sub_issues`,
   `blocked`, `blocked_by`, `blocking`.
2. A new derived index file, `hierarchy.md`, showing parent/sub-issue trees.

`status` additionally reports stored edge counts. Both relationship features
are issue-only. GitHub does not support dependencies or
sub-issues on pull requests, so PR rendering is untouched.

## Goals

- Surface actionability signals (is this issue blocked? by what?) to offline
  readers, especially AI agents scanning frontmatter.
- Surface task decomposition (parent / sub-issue trees) both per-issue and as a
  repo-wide tree.
- Preserve GitHub's user-arranged sub-issue ordering — it encodes intentional
  priority/sequence.
- Handle cross-repository relationship targets losslessly.
- Keep output deterministic (byte-identical re-renders from the same cache).

## Non-goals

- Blocking/dependency data in the cross-cutting `by-label` / `by-milestone` /
  `by-state` index tables (cheap to add later; purely a render change).
- A "blocked issues" index file.
- Relationship keys on pull request frontmatter.

## Data source

The vendored GraphQL schema (`github-schema.json`) already exposes the needed
fields on `Issue` — no schema refresh is required:

| Field | Type | Meaning |
|---|---|---|
| `parent` | `Issue` (nullable) | The parent issue, if this issue is a sub-issue |
| `subIssues` | `IssueConnection` | Child issues, in user-arranged order |
| `blockedBy` | `IssueConnection` | Issues that block this one |
| `blocking` | `IssueConnection` | Issues this one blocks |

GitHub caps sub-issues at 100 per parent and dependencies at 50 per issue, and
enforces an 8-level nesting limit and cycle-freedom server-side. Targets may
live in other repositories.

Both features are generally available (sub-issues since April 2025, issue
dependencies since August 2025) and the fields appear in the vendored schema,
which is introspected without any `GraphQL-Features` preview header — so no
opt-in header should be required. Because a missing opt-in would fail
*silently* (empty connections, not errors), the pre-implementation checklist
below requires confirming a live query returns real relationship data before
building on it.

## Store

New migration `0003_issue_relationships.sql`:

```sql
CREATE TABLE issue_relationships (
    rel          TEXT NOT NULL CHECK (rel IN ('blocks', 'parent')),
    src_node_id  TEXT NOT NULL,   -- blocker / parent
    dst_node_id  TEXT NOT NULL,   -- blocked / sub-issue
    position     INTEGER CHECK (rel = 'parent' OR position IS NULL),
                                  -- sub-issue order within parent; NULL for 'blocks'
    -- endpoint snapshots (repo is NULL when the endpoint is this repo)
    src_repo TEXT, src_number INTEGER NOT NULL, src_state TEXT NOT NULL, src_title TEXT NOT NULL,
    dst_repo TEXT, dst_number INTEGER NOT NULL, dst_state TEXT NOT NULL, dst_title TEXT NOT NULL,
    PRIMARY KEY (rel, src_node_id, dst_node_id)
);
CREATE INDEX idx_rel_src ON issue_relationships(src_node_id);
CREATE INDEX idx_rel_dst ON issue_relationships(dst_node_id);
```

Design points:

- **Canonical direction, stored once.** Every edge is stored exactly one way
  (`blocker → blocked`, `parent → child`). Render derives the reverse views
  (`blocked_by`, `parent`) by querying the opposite column. This prevents the
  two directions from ever disagreeing.
- **Endpoint snapshots.** Each endpoint carries `repo` (`owner/name`, NULL for
  the synced repo), `number`, `state`, and `title` captured at fetch time. They
  arrive free in the GraphQL response. For same-repo endpoints render prefers
  the live `issues` row; snapshots exist so cross-repo endpoints — which have
  no local row — are still renderable. Snapshot `state` values are normalized
  through the same `IssueState` mapping as the `issues.state` column
  (lowercase `open`/`closed`), never stored as raw GraphQL enum casing.
- **One parent per child**, enforced defensively at the DB layer:
  `CREATE UNIQUE INDEX idx_rel_one_parent ON issue_relationships(dst_node_id)
  WHERE rel = 'parent'`. The replace-incident-edges logic already guarantees
  this; the index catches a future logic bug instead of silently corrupting
  the tree.
- **`position`** records a child's index within its parent's `subIssues`
  connection and is meaningful only for `rel = 'parent'` rows; the `CHECK`
  constraint keeps it `NULL` on `'blocks'` rows.
- **Cheap to evolve.** The schema is purely additive and the cache is fully
  rebuildable from GitHub via `--full`. If a future relationship kind needs a
  table rebuild, `0002_pull_requests.sql` already establishes that migration
  pattern.

## Sync

### Query changes

`IssuesPage` (`src/github/queries/issues.graphql`) gains four sub-selections
per issue node:

```graphql
parent { id number state title repository { nameWithOwner } }
subIssues(first: 50) {
  pageInfo { hasNextPage endCursor }
  nodes { id number state title repository { nameWithOwner } }
}
blockedBy(first: 50) { ...same shape... }
blocking(first: 50) { ...same shape... }
```

Three new drain queries (`SubIssuesPage`, `BlockedByPage`, `BlockingPage`)
paginate overflow per issue, following the existing comment/timeline drain
pattern. Given GitHub's per-issue caps (100 sub-issues, 50 dependencies),
drains fire rarely.

### Persistence: replace incident edges

Inside each item's existing per-item transaction, after upserting issue `X`:

1. Read the current `position` of the single row where `rel = 'parent'` and
   `dst_node_id = X` — `X`'s position under its own parent, if any (needed by
   step 3). Positions where `X` is the parent are always overwritten fresh
   from the `subIssues` fetch and need no preservation.
2. `DELETE FROM issue_relationships WHERE src_node_id = ?1 OR dst_node_id = ?1`.
3. Reinsert from the fresh fetch:
   - one `('parent', parent.id, X)` row if `X` has a parent — `position` taken
     from step 1 **only when the stored row's `src_node_id` matches the
     freshly fetched `parent.id`** (the child-side fetch cannot see its own
     index among siblings). When the link is newly discovered or `X` was
     reparented, `position` is `NULL` (ordered last among siblings until the
     parent syncs) — carrying a stale position under a *different* parent
     would be meaningless;
   - one `('parent', X, child)` row per `subIssues` node, `position` = the
     child's **absolute** index across the whole connection. Inserting any
     `'parent'` row first deletes an existing parent row for that child:
     after a reparent, the *new* parent can sync before the child or the old
     parent do, and the stale `(old parent, child)` row — incident to
     neither synced issue — would otherwise collide with the
     one-parent-per-child unique index — the drain loop
     carries a running offset, so page 2 continues at 50 rather than
     restarting at 0 (a per-page index would corrupt the ordering this
     feature exists to preserve);
   - one `('blocks', blocker, X)` row per `blockedBy` node;
   - one `('blocks', X, blocked)` row per `blocking` node.

Because `X`'s fetch covers **all four incident roles**, deleting and
reinserting everything incident to `X` is complete: whichever endpoint of an
edge re-syncs first creates, refreshes, or removes the edge. Correctness never
depends on GitHub bumping `updatedAt` on both endpoints of a relationship
change — one endpoint suffices.

Incremental detection therefore rests on the assumption that linking or
unlinking bumps `updatedAt` on **at least one** endpoint. GitHub's timeline
vocabulary supports this on *both* sides: dependencies emit
`BLOCKED_BY_ADDED/REMOVED_EVENT` on the blocked issue and
`BLOCKING_ADDED/REMOVED_EVENT` on the blocking issue; sub-issue changes emit
`SUB_ISSUE_ADDED/REMOVED_EVENT` on the parent and
`PARENT_ISSUE_ADDED/REMOVED_EVENT` on the child — and timeline events
accompany an `updatedAt` bump on the issue that receives them. The
pre-implementation checklist requires verifying this against a live
repository; if any link/unlink case turns out not to bump either endpoint,
widen the limitation note below to cover it (such changes would then refresh
only on `--full`).

> [!NOTE]
> A pure drag-reorder of sub-issues may not bump the parent's `updatedAt`, so
> an order change can lag until the parent otherwise changes or a `--full` run
> re-fetches it. Accepted limitation.

`--full` reconciliation needs no special handling: soft-deleted issues keep
their edges in the table, and render filters them out (see below).

### Backfill for existing caches

Incremental sync only visits issues whose `updatedAt` is at or after the
stored watermark, so issues untouched since before this feature ships would
never have their relationships fetched — leaving `issue_relationships`,
`hierarchy.md`, and the new frontmatter keys silently incomplete for
historical issues. To backfill automatically, migration `0003` also resets
the **issues** row of `sync_state` — clearing `updated_watermark` *and*
`resume_cursor`, and setting `run_phase` back to `idle`: the next sync walks
every issue once from page 1 (no early stop) and populates edges as it goes.
Clearing the cursor matters: a run paused via `--no-wait` at upgrade time
would otherwise resume mid-pagination and silently skip the
most-recently-updated issues, then stamp the watermark as complete. The
pull-request row is untouched — this feature is issue-only, and clearing it
would force a full PR re-walk (with all its comment/review drains) for zero
benefit. The backfill walk is *not* a `--full` run — it performs no deletion
reconciliation — and pause/resume works normally. `sync --full` achieves the
same backfill.

### Rate limiting

Adding one nullable object and three `first: 50` connections per issue node
roughly doubles-to-triples `IssuesPage`'s GraphQL point cost per page. The
EWMA estimator adapts from observed headers automatically, but the
conservative per-type ceiling for `IssuesPage` is raised from 30 to 90
(refine against observed `X-RateLimit` headers during implementation), and
the three drain queries get their own estimator entries with ceilings in
line with the existing per-item drain queries (10). If real-world costs push frequent `--no-wait` pauses on large
repos, the connection page sizes are the tuning knob: dependencies are capped
at 50 per issue so `blockedBy`/`blocking` never drain at `first: 50`, while
`subIssues` could shrink below 50 at the cost of more drain calls.

## Render

### Frontmatter

Five keys, inserted between `related` and `url` in the fixed key order:

```yaml
related: [38, 51]
parent: "#12"
sub_issues: ["#51", "#47", "#63"]
blocked: 2
blocked_by: ["#9", "acme/infra#4"]
blocking: ["#102"]
url: "https://github.com/o/r/issues/42"
```

| Key | Value | Ordering |
|---|---|---|
| `parent` | reference string or `null` | — |
| `sub_issues` | list of reference strings | GitHub's user-arranged order (`position` ASC, NULLs last, tiebreak by number) |
| `blocked` | integer ≥ 0 | count of currently **open** blockers |
| `blocked_by` | list of reference strings | same-repo first by number ascending, then external by `owner/repo` lexicographic, then number |
| `blocking` | list of reference strings | same as `blocked_by` |

Reference format: every relationship entry is a quoted string. Same-repo
targets render as `"#N"`; cross-repo targets render as the full
`"owner/repo#N"` form. The uniform type keeps parsing trivial for consumers
(one string shape per entry, distinguished by the presence of a repo prefix),
mirroring GitHub's own reference shorthand.

> [!NOTE]
> This is a wire-format decision: the rendered Markdown is committed output
> that external tooling may parse. The existing `related` key keeps its
> historical bare-number format. The relationship format is additive-only and
> carries no compatibility guarantee yet.

State resolution for `blocked` and filtering:

- Same-repo endpoint: use the live `issues` row. Soft-deleted issues are
  excluded from every list, from the `blocked` count, and from `parent` — a
  soft-deleted same-repo parent renders as `parent: null`.
- Cross-repo endpoint: use the stored state snapshot.

All five keys are always emitted (empty lists as `[]`, absent parent as
`null`, unblocked as `blocked: 0`) so the frontmatter shape stays fixed.

### `hierarchy.md`

A new derived file at the output root, beside `labels.md` and `milestones.md`,
wiped and rebuilt on every render:

```markdown
# Issue hierarchy

- [#12 Ship v2 sync](issues/0012.md) (open)
  - [#51 Rework watermarks](issues/0051.md) (closed)
  - acme/infra#4 Provision runners (open) — external
  - [#47 Drain cursors](issues/0047.md) (open)
```

- **Deleted issues are nonexistent.** The tree applies the same rule as
  frontmatter: soft-deleted issues never appear, neither as parents nor as
  children. Consequences follow from this single rule:
  - **Roots:** live same-repo issues that have at least one live (or
    cross-repo) sub-issue and no **live** same-repo parent. This promotes the
    live children of a deleted parent to roots instead of orphaning them — a
    normal workflow (delete a tracking issue, keep its sub-issues) must not
    make live issues invisible. An issue whose parent is cross-repo is also a
    local root (its external parent is not shown as an ancestor). Roots sort
    by number.
  - A live leaf whose parent is deleted participates in no hierarchy and is
    simply absent — consistent with its frontmatter showing `parent: null`.
- **Children:** ordered by the identical rule as the frontmatter `sub_issues`
  key (`position` ASC, NULLs last, tiebreak by number) — implemented as one
  shared helper so the two render paths cannot drift. Same-repo children link
  to their entity file (zero-padded per `repo_meta.padding_width`); cross-repo
  children render as plain text using snapshot number, title, and state,
  suffixed `— external`.
- **Link-text escaping:** this is the first render surface that places issue
  titles inside Markdown link text (existing index tables link only the bare
  number and escape `|` for table cells). Titles used as link text must
  escape `\`, `[`, and `]` so a title containing brackets cannot break the
  link structure; the same escaping applies to cross-repo plain-text titles
  for consistency.
- **Defense:** traversal keeps a visited set (cycle guard) and stops at depth
  8, matching GitHub's nesting limit. GitHub prevents cycles server-side; the
  guard only protects against inconsistent cache states.
- **Empty case:** when no hierarchies exist the file is still written,
  containing the heading and `No issue hierarchies.` — the output tree stays
  shape-stable.

## Observability

`status` gains one line reporting stored edge counts by kind (e.g.
`relationships: 14 dependency, 37 sub-issue`). This is deliberately minimal:
the incremental-detection assumption above is verified at implementation time
but not enforceable at runtime, and an edge count an operator can compare
against expectations is the cheapest signal that relationship data has gone
silently stale.

## Failure modes

- **Stale cross-repo snapshots.** A cross-repo endpoint's snapshot — state,
  title, and `owner/name` (repo renames/transfers) — only refreshes when the
  local endpoint re-syncs. `blocked` counts involving cross-repo blockers can
  therefore be stale between syncs; `--full` refreshes everything.
- **Invisible targets.** Relationship targets in repositories the token cannot
  read may return as `null` nodes. Mapping skips `null` nodes defensively;
  this never fails the sync.
- **Resumed runs.** Edge writes live inside the existing per-item transaction,
  so pause/resume semantics (checkpointed cursors, watermark advance on clean
  completion) are unchanged.

## Pre-implementation verification

Two external-behavior assumptions must be confirmed against a live repository
(with a real token) before the query/store design is finalized:

1. A plain GraphQL query selecting `parent`, `subIssues`, `blockedBy`, and
   `blocking` — with no `GraphQL-Features` header — returns real relationship
   data, not empty connections. (`GithubClient` currently has no per-call
   header injection point; if a header turns out to be required, the client
   needs an extension point and this design must be amended.)
2. Linking and unlinking a dependency and a sub-issue each bump `updatedAt`
   on at least one endpoint. If any case does not, widen the reorder
   limitation note to cover it.

## Testing

- **Frontmatter unit tests:** new keys with a cross-repo mix, ordering rules,
  `blocked` counting (open vs closed vs deleted blockers), fixed key order.
- **Store unit tests:** replace-incident-edges semantics from both endpoints;
  `position` preservation when a child re-syncs after its parent — including
  the reparent case (stale position under the old parent is *not* carried to
  the new one); absolute `position` continuity across drained `subIssues`
  pages.
- **Ordering determinism:** siblings that tie with `position = NULL` render
  in a stable order (number tiebreak) in both `sub_issues` and `hierarchy.md`.
- **Snapshot tests (insta):** an issue with relationships; `hierarchy.md` with
  nesting, an external child, the empty case, and orphan promotion (deleted
  parent with live children).
- **Estimator tests:** the three new query types get direct ceiling tests in
  `src/ratelimit/estimator.rs`, alongside the existing `IssuesPage`/`PrsPage`
  ones.
- **Determinism:** generalize `tests/determinism.rs` from its current three
  hardcoded paths to walk and byte-compare the entire output tree, so
  `hierarchy.md` and any future derived file are covered without further
  edits.
- **Toolchain:** no new dependencies; code compiles on MSRV 1.96.0.
