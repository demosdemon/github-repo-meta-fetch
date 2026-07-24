# Self-describing Markdown tree: rendering `CLAUDE.md`

**Status:** implemented (`56acaa3`..`616cd3d`)
**Date:** 2026-07-24

## Problem

The Markdown tree that `meta-fetch render` produces is designed to be read by AI
coding agents, but it does not describe itself. An agent dropped into the tree
has to infer the layout from directory listings, guess at the frontmatter key
set, and discover by trial that index directories are derived, that a missing
`NNNN.md` means the item was soft-deleted, and that a pull request's state is
not an issue's state.

Today that knowledge lives outside the tree, in hand-maintained agent
definitions and skills. Every change to `src/render/` silently invalidates
them, and nothing fails when they drift.

## Solution

Render a `CLAUDE.md` at the root of the tree, alongside `README.md`, describing
the schema and how to walk it. The document ships with the data it describes,
so it cannot be stale relative to the renderer that produced it, and tests
enforce that it stays accurate.

`README.md` keeps its current job — counts, watermarks, sync phases — and
`CLAUDE.md` points at it for anything freshness-related. The two do not
overlap.

## Architecture

A new leaf module, `src/render/claude_md.rs`:

```rust
/// Render the agent-facing schema guide for a projected tree.
#[must_use]
pub fn render(owner: &str, repo: &str, width: usize) -> String
```

The document body lives beside it as `src/render/claude_md_template.md` and is
pulled in with `include_str!`, matching the precedent set by the SQL files in
`store/migrations/`. A ninety-line Markdown document embedded as a Rust string
literal would be unreadable and unformattable; a sibling `.md` file is neither.

Because `format!` requires a literal format string, substitution is three
explicit `str::replace` calls:

| Placeholder | Substituted with | Why an agent needs it |
| :--- | :--- | :--- |
| `{{OWNER_REPO}}` | `octocat/hello-world` | Repository identity; enough to reconstruct `github.com` URLs |
| `{{WIDTH}}` | `4` | The zero-pad width, so no directory listing is needed to learn it |
| `{{EXAMPLE}}` | `0042` | `format!("{:0width$}", 42)` — a concrete filename consistent with the stated width |

The signature takes `(owner, repo, width)` rather than `&RepoMeta` so the
module depends on nothing from `store`. It has one input shape, one output, and
no I/O; it is exercised in tests with three string literals.

Nothing else is interpolated. Label and milestone inventories stay in
`labels.md` and `milestones.md`, where they already live — duplicating them
here would make the document churn on every taxonomy change while teaching an
agent nothing it cannot read one file away.

The module is declared `pub mod claude_md;` alongside its `render/` siblings,
all of which are public.

### Call site

A `write_claude_md` helper in `render/mod.rs`, matching the `write_*_doc`
convention already used by `write_hierarchy_doc` and `write_taxonomy_docs`:

```rust
write_hierarchy_doc(conn, out, width)?;
write_claude_md(out, &meta.owner, &meta.repo, width)?;
```

The helper is not a bare `fs::write`, because of the overwrite guard below.

### Overwrite guard

`CLAUDE.md` is not `README.md`. It is the one filename Claude Code loads as
standing instructions, so silently replacing a hand-written one — by running
`render --out .` inside a repository that has its own — has a materially larger
blast radius than replacing a README, even though the write mechanism is
identical. The asymmetry is real and is not waved away by analogy to the
existing `README.md` behaviour.

Every rendered `CLAUDE.md` therefore opens with a sentinel line:

```markdown
<!-- meta-fetch:generated — rewritten on every render; edits will be lost. -->
```

The guard matches a **stable prefix**, not the whole line:

```rust
/// Recognises a `CLAUDE.md` as this tool's own output.
///
/// FROZEN. Every tree ever rendered carries this exact byte sequence, and it
/// is the only thing distinguishing our output from a hand-written file.
/// Changing it strands every previously rendered tree as unrecognisable —
/// permanently skipped and warned about. Amend the prose after it freely;
/// never this.
const SENTINEL: &str = "<!-- meta-fetch:generated";
```

Splitting the line this way is the whole point: the frozen commitment is
twenty-five bytes with no version, date, or wording in it, while the
human-readable remainder of the line stays free to be reworded, translated, or
extended without stranding anything. Freezing the *entire* sentence would make
every future typo fix a silent compatibility break.

`write_claude_md` writes when `<out>/CLAUDE.md` is absent, or when its first
line starts with `SENTINEL` after trimming. Otherwise it leaves the file
untouched and emits a `tracing::warn!` — the crate's established warning
channel — naming the path it declined to overwrite.

A failure to *read* the existing file (permissions, a directory at that path,
non-UTF-8 bytes) is treated exactly like a foreign document: skip, warn, and
carry on. The read error is never propagated with `?`. Refusing to overwrite
what cannot be inspected is the conservative branch, and it keeps the promise
that a foreign `CLAUDE.md` can never fail a projection.

The sentinel carries no timestamp or repository identity, so it cannot perturb
the byte-identical re-render guarantee.

This is settled now rather than deferred because the *mechanism* cannot be
retrofitted. Once trees generated without a sentinel exist in the wild, a guard
added later cannot distinguish them from a document a human wrote, and would
either refuse to update its own output or resume clobbering foreign files.

## Document contents

Nine sections, in this order. The budget is **190 lines / ~7 kB**.

That number is measured, not estimated — it is the sum of the nine sections as
actually drafted (the two frontmatter blocks alone are 66 lines with their
fences and annotations; the layout diagram is 23; the guardrails 35). Two
earlier estimates of ninety and 140 were both guesses made before the sections
existed, and both were wrong by enough to make the ceiling unreachable. The
lesson is worth keeping: re-measure the budget against real content before
treating it as a constraint.

190 is a ceiling to defend, not a target to grow into. The file is loaded into
the context of every agent that reads the tree, so length is a recurring cost.
For scale, it replaces a hand-maintained 121-line agent definition that carried
the same knowledge less accurately.

If it must shrink, compress the two frontmatter sections' annotations first.
They are the largest contributors and are already pinned by hard parity tests,
so tightening them is a formatting change that cannot silently lose accuracy.
Cutting a whole section is the only way to recover more than a few lines.

1. **Preamble.** The sentinel comment, then what this directory is, that it is
   derived and read-only, and that `README.md` carries the sync watermarks
   bounding data freshness.
2. **Layout.** The tree diagram, annotated per file.
3. **Filenames and slugs.** The pad width is `{{WIDTH}}` and only ever grows.
   Index slugs replace every character outside `[A-Za-z0-9._-]` with `-`, so
   the label `area: sync` becomes `by-label/area--sync.md`. Distinct names can
   collide on one slug (`a b` and `a-b` both yield `a-b.md`), in which case the
   last one written wins and the other has no index file — rare, but it means
   an absent `by-label/` file is not proof the label is unused.
4. **Issue frontmatter.** A fenced `yaml` block listing all eighteen keys in
   emitted order — `number`, `title`, `state`, `state_reason`, `labels`,
   `assignees`, `milestone`, `author`, `created_at`, `updated_at`, `closed_at`,
   `related`, `parent`, `sub_issues`, `blocked`, `blocked_by`, `blocking`,
   `url` — annotated where the type is not obvious: `related` holds bare
   numbers, relationship keys hold reference strings (`"#7"` same-repo,
   `"owner/repo#7"` cross-repo), and `blocked` counts only the *open* entries
   of `blocked_by`.
5. **Pull request frontmatter.** The same treatment for all twenty-one keys —
   `number`, `title`, `state`, `draft`, `base`, `head`, `author`, `created_at`,
   `updated_at`, `closed_at`, `merged_at`, `merged_by`, `additions`,
   `deletions`, `changed_files`, `labels`, `assignees`, `milestone`, `closes`,
   `related`, `url`.
6. **Index tables.** The issue/PR index header `| # | title | state |
   assignees | updated |`, with the `#` (number) cell linking to `../NNNN.md`,
   plus the header rows of `labels.md` and `milestones.md`.
7. **`hierarchy.md`.** Nested bullets, capped at GitHub's nesting limit, with
   cross-repository targets rendered as plain text marked *external*. The cap
   is written numerically in the fixed phrase `8 levels`, not spelled out, so
   the parity test below can locate it.
8. **Answering a question.** Routing rules: filters and enumerations go to the
   index tables; taxonomy questions go to `labels.md` and `milestones.md`;
   hierarchy goes to `hierarchy.md` or the frontmatter relationship keys;
   free-text search greps `[0-9]*.md` to skip index files.

   Lookup by number gets its own paragraph, because issues, pull requests, and
   GitHub Discussions **share one number sequence per repository** — `#37` is
   an issue, a PR, or a discussion; only the first two have files here, and
   nothing about the number alone says which. Any lookup by number must check
   `issues/NNNN.md` *and* `prs/NNNN.md`. This applies equally to bare numbers a
   user supplies and to the numbers appearing in `related` and `closes`.
9. **Before you answer.** Five guardrails:
   - This is a cached snapshot, not live GitHub. Recency-sensitive answers must
     cite the watermark from `README.md`.
   - A missing `issues/NNNN.md` usually means `#NNNN` is a **pull request** —
     check `prs/NNNN.md` before concluding anything. Only when the number is
     absent from *both* directories is it a discussion (which this tool never
     syncs), soft-deleted, transferred, or not yet synced.
   - Issue state (`open`/`closed`, refined by `state_reason`) is not pull
     request effective state (`open`/`draft`/`closed`/`merged`).
   - A same-repository relationship link can point at a file that does not
     exist yet, until a sync reaches the target issue.
   - Nothing in the tree may be edited; it is derived data owned by the tool.

## Testing

The interesting failure is not "the file is missing" — it is "the file
describes a schema the renderer no longer emits." Every hardcoded fact in the
template gets a test that compares it against a real render or the constant it
claims to mirror. A fact the document asserts without a corresponding test is
a fact that will eventually be wrong.

Two helpers do the extraction:

```rust
/// Extract frontmatter key names from the `yaml` block under a `##` heading.
fn documented_keys(doc: &str, heading: &str) -> Vec<String>

/// Extract key names from a rendered `---`-delimited frontmatter block.
fn emitted_keys(rendered: &str) -> Vec<String>
```

**Frontmatter parity**

- `documented_issue_frontmatter_matches_render` asserts
  `documented_keys(&doc, "## Issue frontmatter")` equals
  `emitted_keys(&frontmatter::render(…))` — order included.
- `documented_pr_frontmatter_matches_render` asserts
  `documented_keys(&doc, "## Pull request frontmatter")` equals the keys of the
  frontmatter block emitted by `pr::render(…)`.

**Table header parity** — `documented_headers_match_render` asserts each of the
three header rows quoted in the document appears verbatim in the corresponding
renderer output: the issue/PR index header in `indexes::issue_table(…)`, the
`labels.md` header in `indexes::labels_doc(…)`, and the `milestones.md` header
in `indexes::milestones_doc(…)`.

**Constant and behaviour parity**

- `documented_depth_cap_matches_const` asserts the rendered document contains
  `format!("{} levels", hierarchy::MAX_DEPTH)`. This requires widening that
  constant from private to `pub(super)` so a sibling module under `render/` can
  read it — the minimum visibility that makes the test possible.
- `documented_slug_example_matches_render` asserts the worked slug example in
  the document holds against the real function: `super::index_slug("area: sync")`
  equals `"area--sync"`. `index_slug` is private to `render/mod.rs`, but
  `claude_md` is a descendant module and can reach it without a visibility
  change. Testing the charset rule directly is not worth the machinery; testing
  the example an agent actually copies is.

**Substitution completeness** — `render_leaves_no_placeholders` asserts the
rendered output contains no `{{`. The three substitutions are independent
`str::replace` calls, which silently no-op when a placeholder is misspelled on
either side; without this assertion a literal `{{WIDTH}}` reaching an agent's
context would be caught only by someone eyeballing the snapshot.

**Sentinel stability** — `rendered_doc_carries_sentinel` asserts the first line
of the rendered document starts with `SENTINEL`. It fails if the template's
opening line is ever reworded past the frozen prefix, which is exactly the
change that would strand existing trees.

Adding a frontmatter key without documenting it fails CI. So does changing an
index header, the depth cap, the slug charset, the sentinel prefix, or leaving
a placeholder unsubstituted. Renaming a heading in the template fails too —
loudly rather than silently.

One documented fact is deliberately **not** parity-tested: that issues and pull
requests share a number sequence. That is a property of GitHub's data model,
not of this renderer, so there is no local constant or output to compare it
against — a test would only assert that the sentence still exists. It is called
out here so a future reader treats the gap as considered rather than missed.

Alongside those:

- An `insta` snapshot of the fully rendered document, so any wording change is
  reviewed rather than merged unseen.
- A presence assertion for `CLAUDE.md` in `tests/determinism.rs`, which already
  byte-compares two full renders — so determinism coverage comes for free once
  the file is in the tree.
- Three guard tests in `render/mod.rs`: `preserves_foreign_claude_md` renders
  into a directory holding a `CLAUDE.md` without the sentinel and asserts the
  contents are unchanged; `overwrites_own_claude_md` renders twice into one
  directory and asserts the second render replaces the first render's output;
  `unreadable_claude_md_does_not_fail_render` puts a *directory* at
  `<out>/CLAUDE.md` and asserts `render_tree` still returns `Ok` — pinning the
  "a foreign document never fails a projection" promise against the read-error
  path, which is the branch most likely to regress into a `?`.

## Scope

**In scope:** the module, the template, the `render_tree` call site, the tests
above, and updating this repository's own `README.md` output-tree diagram and
`CLAUDE.md` architecture notes.

**Out of scope:**

- An `AGENTS.md` sibling for cross-agent portability. It is a cheap retrofit —
  the same bytes under a second filename — and there is no evidence yet that it
  is wanted.
- Retiring the external agent definitions and skills that currently duplicate
  this knowledge. They live in other repositories and are handled separately
  once this lands.

## Assumptions and consequences

- `<out>/CLAUDE.md` is rewritten on every render, but only when the tool
  recognises the file as its own output — see the overwrite guard above. A
  hand-written `CLAUDE.md` in the output directory survives, at the cost of the
  tree not carrying its guide until the file is moved aside.
- The guard protects `CLAUDE.md` specifically, not the whole tree. `README.md`,
  `labels.md`, `milestones.md`, and `hierarchy.md` keep their existing
  unconditional-overwrite behaviour. That inconsistency is deliberate:
  `CLAUDE.md` is the only one of these filenames that carries standing
  instructions to an agent, so it is the only one whose accidental replacement
  changes behaviour rather than just losing text.
- Failure modes are unchanged apart from the guard: string formatting, a read
  of the existing file's first line when one is present, and one
  `std::fs::write` on the same error path as the sibling documents. The read is
  of content, not metadata — presence alone cannot distinguish our own output
  from a foreign file — and its errors are swallowed into the skip-and-warn
  branch rather than propagated.
- The rendered document adds roughly 7 kB to every tree and produces a one-time
  diff on the first render after upgrading. Subsequent renders change it only
  when the renderer changes or the pad width grows.
