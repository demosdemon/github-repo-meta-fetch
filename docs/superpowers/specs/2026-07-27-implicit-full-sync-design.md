# Implicit full sync: per-phase full-walk tracking

**Status:** implemented (`df07705`..`0dc9ac6`)
**Date:** 2026-07-27

## Problem

`meta-fetch sync` walks a repository incrementally: each phase paginates
`UPDATED_AT DESC` and early-stops once an item's `updatedAt` drops below the
stored watermark. `--full` disables that early stop, walks everything, and
reconciles deletions by soft-deleting cached rows that no longer appear
upstream.

Nothing makes a full walk happen. A user who only ever runs plain `sync` gets a
cache that never reconciles deletions — issues that were deleted, transferred,
or converted upstream linger in the tree indefinitely, and no output signals
this. The current `repo_meta.last_full_sync_at` column records whether a full
walk ever happened, but only `status` reads it; nothing acts on it.

The flag is also easy to lose across a pause. A `--full` run that hits the
rate-limit reserve floor with `--no-wait` exits 75 with its cursor
checkpointed, and the user is told to re-run. If they re-run without `--full`,
the resumed pass early-stops on the pre-existing watermark and silently walks
less than it should. Correctness depends on the user remembering a flag.

## Solution

Track "this phase has completed a full walk" per phase, in `sync_state`, and
have each phase walk fully when its own marker is unset.

A phase walks everything when `--full` is passed, or when it has never recorded
a completed full walk. Both conditions are decided per phase, so
`sync --only issues` consults the issues marker alone — the state of the pull
requests phase has no bearing on how the issues phase should walk.

This makes the first sync of any repository a reconciling full walk without the
user asking for it, converts an existing incremental-only cache on its next
run, and makes a paused full walk resume correctly whether or not the user
remembers `--full`.

`--full` keeps its current meaning: force a full walk even when one has already
been recorded. There is no flag to suppress an implicit full walk. On a fresh
database one would buy nothing — with no watermark the early stop never fires,
so the walk covers everything regardless — and on an established cache,
suppressing the one-time reconciliation is the behaviour this design exists to
end.

## Prerequisite: an injected clock

This design adds a persisted timestamp whose value is worth asserting in tests,
so the crate's reads of wall-clock time are made injectable first, as a
self-contained change landing ahead of the feature.

The surface is small. The crate has exactly two production reads of the clock,
both in `src/sync/mod.rs`: the whole-repository stamp at line 101, which this
design deletes outright, and the rate-limit wait computation at line 192.
`src/model.rs:540-541` calls `Utc::now()` inside a `#[cfg(test)]` block, where
a fixed literal is both sufficient and more deterministic;
`src/ratelimit/store.rs:182` already uses one.

A new leaf module, `src/clock.rs`:

```rust
/// A source of wall-clock time, injected wherever the crate reads "now" so
/// tests can pin it.
pub trait Clock {
    fn now(&self) -> DateTime<Utc>;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock pinned to a fixed instant.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}
```

`FixedClock` is `pub` rather than `#[cfg(test)]` because the integration tests
under `tests/` link against the library as an external crate and cannot see
test-only items.

`Syncer` gains `pub clock: &'a dyn Clock`, and the phase functions receive it
through `WalkCtx` (see "Implementation → `sync/issues.rs`, `sync/prs.rs`").
Trait object rather than a generic parameter: the clock is read at most a
handful of times per run, so dispatch cost is irrelevant, and `&dyn` keeps the
generic parameter from propagating through `Syncer`, `run_phase`, `WalkCtx`,
and both phase functions. Callers write `clock: &crate::clock::SystemClock`;
static promotion of the unit struct makes that a `'static` reference, so no
binding is needed at the call site.

The clock covers reading time, not sleeping. `run_phase` still sleeps on the
real `tokio::time::sleep`. To make the part that the clock does govern
testable, the wait computation at `src/sync/mod.rs:191-194` is extracted to a
pure function:

```rust
/// Time to wait for `reset`, clamped at zero and capped by `max_wait`.
fn wait_for(reset: Option<DateTime<Utc>>, now: DateTime<Utc>, max_wait: Option<Duration>) -> Duration
```

## Schema

A new migration, `src/store/migrations/0004_phase_full_sync.sql`:

```sql
ALTER TABLE sync_state ADD COLUMN last_full_sync_at INTEGER;
ALTER TABLE sync_state ADD COLUMN last_reconciled_at INTEGER;

-- Backfill: a repository that already completed a --full run covering both
-- phases must not be surprised by another full walk on upgrade.
UPDATE sync_state
SET last_full_sync_at = (SELECT last_full_sync_at FROM repo_meta WHERE id = 1);

ALTER TABLE repo_meta DROP COLUMN last_full_sync_at;
```

Two markers, not one, because completing a full walk and reconciling deletions
are separable — see "Reconciliation across a pause". `last_reconciled_at` gets
no backfill: the old column recorded that a `--full` run finished, never
whether that run was the fresh pass that also reconciled, so there is nothing
faithful to derive. Every repository starts with it `NULL` and populates it on
the next reconciling walk. Reporting "unknown" is the honest answer where the
old schema genuinely did not know.

The backfill is a no-op for a repository that never ran `--full`: the subquery
yields `NULL`, every phase stays unstamped, and the next `sync` performs the
implicit full walk. That is the intended upgrade path.

The whole-repository timestamp is removed rather than derived. It has two
readers — `status` (`src/cli.rs:239`) and the rendered tree's `README.md`
(`src/render/mod.rs:338`) — and both already report every other sync fact per
phase, so both take the per-phase markers directly. An aggregate would be
strictly less information than the two values it summarises. Dropping the
column in the same migration that backfills from it avoids a second migration
later. The drop is legal: `repo_meta.last_full_sync_at` is not a primary key,
is not `UNIQUE`, is not indexed, and is not referenced by a `CHECK`, view, or
trigger, and the bundled SQLite is well past the 3.35 requirement for
`DROP COLUMN`.

It is, however, **irreversible**. Migrations here are forward-only —
`src/store/mod.rs:25-31` registers `M::up` with no `.down()` — and 0001-0003
only ever add tables and columns or reset rows, so this is the project's first
migration to destroy data. Reverting means writing a corrective forward
migration that re-adds the column and derives it as the `MIN` across the
per-phase markers (`NULL` if either is unset), or restoring from a backup.
That derivation is exact for repositories whose phases were both stamped by the
0004 backfill and lossless-but-coarser afterwards, so the retrofit is cheap;
it is the destruction of the column, not the value, that cannot be undone.

`repo_meta` retains `owner`, `repo`, and `padding_width`. `RepoMeta` loses its
`last_full_sync_at` field and `store::repo_meta::set_last_full_sync` is deleted.

## Marker semantics

`sync_state.last_full_sync_at` means **this phase has walked its entire history
at least once**. It is stamped whenever a full pass reaches
`SyncStop::Completed`, whether that pass started fresh or resumed from a
checkpoint.

Deletion reconciliation is deliberately *not* part of the marker's meaning. It
stays gated on "this walk started fresh and completed", because a walk resumed
from a checkpoint left by an *earlier process* re-walked only the pages after
that checkpoint and so holds an incomplete seen-set.

The two could have been tied together — stamp only when a pass both completed
and reconciled — but that does not converge. On a repository too large to walk
within one rate-limit window, every attempt would pause, resume, complete
without reconciling, decline to stamp, and then restart the whole walk from
scratch on the next run:

```
Run 1  fresh, implicit full   -> pages 1..k, PAUSED (exit 75)
Run 2  resumed, implicit full -> pages k..n, COMPLETED, not fresh
Run 3  fresh, implicit full   -> re-walks 1..n from scratch, PAUSED
Run 4  ...
```

Stamping on any clean full pass converges after at most one pause/resume cycle.
The cost is that such a walk may not have reconciled deletions — a no-op on a
fresh database, since there is nothing cached to soft-delete, and otherwise the
residual gap described below.

## Reconciliation across a pause

Making full walks implicit exposes a pre-existing hole that must be narrowed
first, because it stops being opt-in.

`sync_issues` captures `started_fresh` from the resume cursor at function entry
(`src/sync/issues.rs:618`) and owns its seen-set as a local. But `run_phase`
*re-calls* it after a pause (`src/sync/mod.rs:119-201`), so the continuation
reads the cursor its predecessor just checkpointed, computes
`started_fresh == false`, and skips reconciliation. This is not specific to
`--no-wait`: the default sleep-until-reset path returns from `sync_issues` and
calls it again in the same process, hitting the identical condition. Because
`complete` then clears the cursor, the following invocation starts fresh and
re-walks from the top — so on a repository too large to walk within one
rate-limit window, deletion reconciliation is unreachable through *any*
invocation, `--full` included.

The cause is that per-walk accumulator state lives at per-call scope. The fix
is to move it out to the retry loop, which is the true boundary of a walk:
`started_fresh` and the seen-set become `run_phase` locals, and reconciliation
runs after the loop exits rather than inside the phase function. The merged
control flow is given in full under "Implementation → `sync/mod.rs`".

`sync_issues` and `sync_prs` accordingly take `seen: &mut HashSet<String>`,
populate it exactly as before, and drop both their `started_fresh` local and
their `mark_deleted_except` call. Reconciliation still runs after `complete`,
preserving the current ordering. `Phase::entity()` serves as the table name
too — the `sync_state` entity keys and the table names coincide as `issues` and
`pull_requests` — so `store::mark_deleted_except` can be called directly rather
than through the per-table wrappers. That coincidence is load-bearing and
invisible at the call site, so `Phase::entity()` carries a doc comment saying
so; renaming one without the other fails at runtime against a nonexistent
table, not at compile time.

Peak memory is unchanged: the seen-set already held every node id for the
duration of a full walk; it now merely outlives the calls it spans.

`run_max` deliberately stays per-call. It has the same shape of problem — a
resumed call's `run_max` covers only the tail pages — but the existing
`run_max.max(watermark)` guard already handles it conservatively, and the
comment at `src/sync/issues.rs:691-694` documents that as intentional: keeping
the larger value costs a re-walk, never a skip. Hoisting it would be a
watermark-accuracy improvement unrelated to this design's purpose.

This closes the default path. It does **not** close `--no-wait`, where the
process exits at the reserve floor and the in-memory seen-set dies with it; a
later resume still reconciles nothing. Closing that requires persisting the
seen-set, which is deferred — see Scope. What the residual gap does get is a
signal: `sync_state.last_reconciled_at`, stamped beside the reconciliation
above, so a walk that completed without reconciling is visible in `status` and
the rendered `README.md` rather than indistinguishable from one that did.

## Implementation

### `store/sync_state.rs`

`SyncState` gains a field, typed to match its sibling rather than the `i64` the
dropped `repo_meta` column used:

```rust
/// When this phase last completed a walk of its entire history, fresh or
/// resumed. `None` until one completes.
pub last_full_sync_at: Option<DateTime<Utc>>,
/// When this phase last reconciled deletions, which requires a full walk
/// that both started fresh and ran to completion. `None` until one does.
pub last_reconciled_at: Option<DateTime<Utc>>,
```

`get` selects and decodes both the same way as `updated_watermark`, mapping an
out-of-range timestamp to `None` rather than panicking. The no-row default is
`None` for each.

`complete` grows a `full_sync_at: Option<DateTime<Utc>>` parameter and stamps in
the same statement that advances the watermark, so the two cannot diverge if
the process dies between them:

```sql
INSERT INTO sync_state (entity_type, updated_watermark, resume_cursor, run_phase, last_full_sync_at)
VALUES (?1, ?2, NULL, 'done', ?3)
ON CONFLICT(entity_type) DO UPDATE SET
   updated_watermark = excluded.updated_watermark,
   resume_cursor     = NULL,
   run_phase         = 'done',
   last_full_sync_at = COALESCE(excluded.last_full_sync_at, sync_state.last_full_sync_at)
```

`?3` binds `full_sync_at` and is `NULL` on an incremental pass. The `COALESCE`
is what keeps a later incremental pass from clearing a marker an earlier full
pass set — plain `excluded.last_full_sync_at` would erase it on every
incremental run.

`last_reconciled_at` is written separately, by a small
`mark_reconciled(conn, entity_type, at)` — a single-column `UPDATE`. It cannot
fold into `complete`, which runs inside the phase function, before and
independently of the reconciliation `run_phase` performs after the retry loop.
A crash between the two leaves rows soft-deleted but the marker unset, so the
next full walk reconciles again; `mark_deleted_except` is idempotent, making
that a redundant pass rather than a corruption.

The store stays clock-free: like every other timestamp it persists, this one
arrives as a parameter. That leaves `watermark` and `full_sync_at` adjacent and
identically typed, and so silently transposable — which is exactly what the
injected clock defuses, since a test can now assert the stamped value equals a
`FixedClock`'s instant and fail immediately on a swap.

### `sync/issues.rs`, `sync/prs.rs`

**Signature.** `sync_issues` and `sync_prs` take six parameters today
(`src/sync/issues.rs:604`, `src/sync/prs.rs:433`). Adding `clock` and `seen`
would make eight, and clippy's `too_many_arguments` warns above seven. There is
no `clippy.toml` raising that threshold, and `just lint` runs with `-D
warnings`, so this would fail CI — and the repository's lint policy forbids
`#[allow]`, admitting only a deliberate `#[expect]`.

Rather than suppress it, the invariants of a walk move into a context struct in
`sync/mod.rs`:

```rust
/// Everything an entity walk needs that varies neither between pages nor
/// across a pause.
pub struct WalkCtx<'a> {
    pub client: &'a GithubClient,
    pub conn: &'a Connection,
    pub owner: &'a str,
    pub repo: &'a str,
    pub clock: &'a dyn Clock,
}
```

Both phase functions become
`sync_{issues,prs}(ctx: &WalkCtx<'_>, full, seen, budget_ok)` — four
parameters, down from six, with headroom for the next cross-cutting concern
rather than a suppression to re-litigate. `run_phase` builds the `WalkCtx`
once; `Syncer` already owns four of the five fields.

This is chosen over `#[expect]` because the argument list is being edited at
every call site regardless: the 15 direct calls in `tests/` (below) must change
either way, so the struct costs little beyond the suppression it replaces.

**Behaviour.** Neither function's `full` parameter changes meaning — it still
means "walk everything". Each stamps when completing, reading the clock from
the context:

```rust
sync_state::complete(ctx.conn, ENTITY, run_max.max(watermark), full.then(|| ctx.clock.now()))?;
```

The phase that performed the full walk is the thing that records having done
so. Both also shed their `started_fresh` local and `mark_deleted_except` call,
per "Reconciliation across a pause" — which leaves
`store::issues::mark_deleted_except` (`src/store/issues.rs:215`) and
`store::prs::mark_deleted_except` (`src/store/prs.rs:313`) with no production
caller. Delete both wrappers and retarget their unit tests
(`src/store/issues.rs:261,272,284`, `src/store/prs.rs:512`) at
`store::mark_deleted_except`, rather than leaving thin wrappers alive solely to
be tested.

### `sync/mod.rs`

`Syncer::full` keeps its `bool` type and its "force a full walk" meaning.
`Phase` gains an `entity()` accessor returning the existing `ENTITY` constants.
`sync::prs::ENTITY` is already `pub(crate)`; `sync::issues::ENTITY` is private
to its own module and must be widened to match, since a parent module cannot
reach a private item in its child. `run_phase` then resolves the decision once,
before the wait/retry loop:

```rust
// Per-walk state, resolved once and spanning every retry.
//
// A phase walks everything when forced, or when it has never recorded a
// completed full walk. The marker is per-phase, so `--only issues` decides
// on the issues marker alone.
let state = crate::store::sync_state::get(self.conn, phase.entity())?;
let full = self.full || state.last_full_sync_at.is_none();
let started_fresh = state.resume_cursor.is_none();
let mut seen: HashSet<String> = HashSet::new();
let ctx = WalkCtx {
    client: self.client, conn: self.conn, owner, repo, clock: self.clock,
};

loop {
    let stop = {
        let budget_ok = /* unchanged */;
        match phase {
            Phase::Issues => sync_issues(&ctx, full, &mut seen, budget_ok).await?,
            Phase::Prs => sync_prs(&ctx, full, &mut seen, budget_ok).await?,
        }
    };
    match stop {
        // Was `return Ok(Outcome::Completed)`; now breaks so the
        // reconciliation below is reachable.
        SyncStop::Completed => break,
        SyncStop::Paused if self.no_wait => return Ok(Outcome::Paused),
        SyncStop::Paused => {
            let reset = self.rl.get(budget::Resource::GraphQL)?.map(|b| b.reset);
            tokio::time::sleep(wait_for(reset, self.clock.now(), self.max_wait)).await;
        }
    }
}

if full && started_fresh {
    crate::store::mark_deleted_except(self.conn, phase.entity(), &seen)?;
    crate::store::sync_state::mark_reconciled(self.conn, phase.entity(), self.clock.now())?;
}
Ok(Outcome::Completed)
```

Resolving before the loop rather than inside it keeps a phase's mode fixed
across a pause-and-wait cycle. The same `state` read also supplies
`started_fresh` for reconciliation, so the hoist adds no extra query.
`sync_issues` and `sync_prs` re-read `sync_state` for their own cursor and
watermark; that redundant indexed lookup is the price of keeping them
independently callable with an explicit `full`, which is how the existing
integration tests drive them.

When the implicit walk engages, `run_phase` logs it before starting:

```rust
tracing::info!(phase = ?phase, "no full walk recorded; walking everything");
```

`main.rs` defaults the subscriber to `INFO` on stderr, so this is visible
without `RUST_LOG`.

The whole-repository stamp at `src/sync/mod.rs:99-102` — and with it the
`do_issues && do_prs` condition guarding it and its `Utc::now()` call — is
deleted. Phases now stamp themselves, from the injected clock.

The remaining clock read, the wait computation at lines 191-194, becomes
`wait_for(reset, self.clock.now(), self.max_wait)`.

### `cli.rs`

No flag is added or changed; `--full` already exists. The only edit to `Sync` is
that its `Syncer` literal at `src/cli.rs:154` supplies the production clock,
`clock: &crate::clock::SystemClock`. This is the crate's sole injection point —
every other clock read descends from it.

`Status` replaces the single `last_full_sync_at` line with a per-phase line
beside each phase's existing `watermark:` line, keeping the issues and pull
request blocks self-contained:

```
octocat/hello-world
issues: 128 active, 3 soft-deleted
relationships: 12 dependency, 30 sub-issue
run_phase: Done
watermark: Some(2026-07-26T18:04:11Z)
full_sync: Some(2026-07-20T09:31:52Z)
reconciled: Some(2026-07-20T09:31:52Z)
prs: 8 open, 2 draft, 90 closed, 40 merged, 1 soft-deleted
prs run_phase: Done
prs watermark: Some(2026-07-26T17:58:03Z)
prs full_sync: Some(2026-07-24T11:02:40Z)
prs reconciled: None
```

The PR block above is the case this pairing exists to expose: a full walk
completed, but it paused and resumed under `--no-wait`, so nothing was
reconciled and soft-deletes are outstanding. Before this change the two were
indistinguishable.

### `render/mod.rs`

`write_readme_doc` composes the tree's `README.md` and currently ends with a
`- last full sync: {last_full}` line fed by the dropped column
(`src/render/mod.rs:337-339`, emitted at line 366). Every other sync fact in
that document is already per-phase — `issues watermark`, `issues sync phase`,
`PRs watermark`, `PRs sync phase` — so the trailing aggregate line is replaced
by two lines placed beside their siblings:

```
- issues watermark: {watermark}
- issues sync phase: {run_phase}
- issues last full sync: {issues_full}
- issues last reconciled: {issues_recon}
...
- PRs watermark: {pr_watermark}
- PRs sync phase: {pr_phase}
- PRs last full sync: {prs_full}
- PRs last reconciled: {prs_recon}
```

All four reuse the existing `"never"`-for-`None` and
`to_rfc3339_opts(SecondsFormat::Secs, true)` formatting already applied to the
watermarks, and all read `sync_state`, which `write_readme_doc` already loads
for both phases. No test asserts the removed line: `tests/end_to_end.rs:124-132`
and `tests/determinism.rs:143` assert PR counts only, and there is no README
snapshot.

The tree's own `CLAUDE.md` describes `README.md` only as "entity counts, sync
watermarks, …" (`src/render/claude_md_template.md`), which stays accurate, so
neither the template nor its parity snapshot changes.

## Testing

- **`clock`** — `FixedClock` returns its instant; `SystemClock` returns a value
  that advances. Kept minimal; the trait's value is realised in the tests below.
- **`wait_for`** — a reset in the future yields the difference; a reset in the
  past clamps to zero; `max_wait` caps a longer wait; `None` yields zero.
- **`store::sync_state`** — a full pass stamps the marker with exactly the
  instant passed, which also pins `complete`'s parameter order against a
  transposition with `watermark`; a following incremental pass advances the
  watermark while preserving the marker (the `COALESCE` path); a fresh state
  reports `None`.
- **Migration** — build a database at v3 with
  `migrations().to_version(&mut conn, 3)` (the pattern at
  `src/store/relationships.rs:254`), set `repo_meta.last_full_sync_at` and both
  `sync_state` rows, migrate to v4, and assert both phases carry the backfilled
  value and that `last_reconciled_at` is `NULL` on both — it has no backfill
  source. A second case asserts a `NULL` source leaves both phases unstamped.
- **`Syncer`** — extend the existing mock-server tests in `tests/sync_issues.rs`:
  a phase with no marker walks past the watermark and stamps a `FixedClock`'s
  instant on completion; a second run against the same database early-stops. A
  `--only prs` run leaves the issues marker untouched.
- **Call-site blast radius** — larger than the `Syncer` literals alone. Three
  literals gain a `clock` field (`tests/sync_issues.rs:324,408`,
  `tests/sync_prs.rs:239`), as does the production one at `src/cli.rs:154`. On
  top of that, 15 direct calls to `sync_issues`/`sync_prs` that bypass `Syncer`
  must be rewritten to the `WalkCtx` form: 7 in `tests/sync_issues.rs`, 6 in
  `tests/sync_prs.rs`, 2 in `tests/end_to_end.rs`. A shared test helper
  constructing a `WalkCtx` keeps that churn to one place.
- **Resumed pass** — a full pass that pauses and is resumed to completion
  stamps its marker; assert the next run is incremental.
- **Reconciliation across a pause** — the regression test for the hole above.
  Drive a full walk whose `budget_ok` returns `false` once mid-walk with
  `no_wait: false`, so `run_phase` sleeps (use a reset already in the past, via
  `FixedClock`, so `wait_for` yields zero and the test does not actually wait)
  and re-calls the phase. Seed a cached item absent from the mock's pages and
  assert it is soft-deleted, and that `last_reconciled_at` is stamped — this
  fails on the current code, which skips reconciliation on the continuation
  call.
- **`--no-wait` remains unreconciled, and says so** — the same scenario with
  `no_wait: true` and a fresh resuming run asserts the item survives *and* that
  `last_full_sync_at` is stamped while `last_reconciled_at` stays `None`. This
  pins both the documented residual gap and the signal that now exposes it.
- **`mark_reconciled`** — stamps the given instant on the named phase only,
  leaving the other phase and every other column untouched.
- **`store::repo_meta`** — `migrations_apply_and_meta_round_trips`
  (`src/store/repo_meta.rs:121-129`) asserts `m.last_full_sync_at == None` and
  loses that assertion with the field.
- **`status` and rendered `README.md`** — no existing test asserts either
  removed line (the status tests are `status_from_db_prints_slug` and
  `status_prints_pr_counts` in `tests/cli_dispatch.rs`, asserting unrelated
  substrings). Add coverage for the per-phase lines in both outputs rather than
  updating assertions that do not exist.

## Scope

The work lands as three commits:

1. **Clock injection** — `src/clock.rs`, `WalkCtx`, `wait_for`, and the call
   sites. Behaviour-preserving; the `WalkCtx` refactor rides here because it is
   what keeps the clock parameter under the argument-count limit.
2. **Seen-set hoist** — the reconciliation fix and its regression test. A
   self-contained bug fix, complete on its own.
3. **Per-phase markers** — migration 0004, the implicit full walk, and both
   `status`/`README.md` displays.

Ordering matters in one place: `mark_reconciled` cannot be called from the
hoisted code in commit 2, because `last_reconciled_at` does not exist until
commit 3's migration. Commit 2 therefore reconciles without stamping, and
commit 3 adds the stamp beside the existing call. The hoist precedes the marker
deliberately — it is the change that makes implicit full walks safe to turn on.

Out of scope: what deletion reconciliation soft-deletes (only *when* it runs
changes), the rate-limit reserve machinery, and the taxonomy phase (already a
full refresh on every run). The rendered tree changes only in `README.md`'s
sync-status lines; every per-issue and per-PR document, every index, and every
render snapshot is untouched.

**Persisting the seen-set is deferred.** It is what would close reconciliation
under `--no-wait`, and it needs its own design: a table keyed by entity type,
a write per item on top of the existing per-item transaction, and a staleness
rule for the set left behind by a walk that is abandoned rather than resumed.
Folding that into this change would couple a storage question to a scheduling
one.

Abstracting sleeping is also out of scope. `Clock` covers reading time only, so
`run_phase`'s pause path still sleeps for real; only the duration it computes
becomes testable. Making the wait itself instant in tests would mean injecting
a sleeper too, or adopting `tokio::time::pause`, neither of which this design
needs.

## Assumptions and consequences

- **`Clock` is injected, not ambient.** Nothing prevents a future `Utc::now()`
  from being reintroduced; the trait makes the right thing easy, not the wrong
  thing impossible. With two production call sites, a lint is not warranted.
- **A backfilled marker is trusted.** A repository whose `repo_meta` recorded a
  full sync has both phases marked as of that time, matching the old column's
  meaning: `--full` covering both phases. No re-verification is performed.
- **`--full --only <phase>` history cannot be recovered.** The old column was
  stamped only when a `--full` run covered *both* phases
  (`src/sync/mod.rs:99-102`), so a repository whose owner only ever ran
  `--full --only issues` has that history recorded nowhere. Its issues phase is
  already fully walked and reconciled, yet the backfill finds `NULL` and the
  next `sync` walks it again. This is a consequence of the old schema, not a
  migration defect, and the redundant walk is a one-time cost — but it will look
  like a bug to anyone who worked that way.
- **Plain `sync` can now block, or exit 75, far more often.** Reaching the
  reserve floor was never gated on `--full` — `budget_ok` is consulted on
  every page (`src/sync/issues.rs:688`), so an incremental run with enough
  recent activity could always pause. What changes is the likelihood: the
  first post-upgrade `sync` of a repository whose history exceeds one
  rate-limit window will now routinely reach the floor with no `--full` in
  sight, simply because that run has a full walk to do. The default path does
  *not* exit 75 for this: `run_phase` sleeps until reset and continues
  (`src/sync/mod.rs:222-234`), and `--max-wait` is unbounded by default, so the
  practical consequence for most callers is a long block — up to an hour per
  window, more for a repository spanning several — where the same invocation
  used to return in seconds. Only `--no-wait` exits 75, and that remains what
  automation should treat as retryable. A capped `--max-wait` doesn't avoid the
  block either: once the cap expires, `run_phase` retries immediately, but the
  floor hasn't actually reset, so the retry typically drains exactly one page
  before pausing again (`src/sync/issues.rs:688`) — it degrades to roughly one
  page per `--max-wait` interval rather than failing. The README edits below
  need to say all of this, not just the exit code.
- **The first sync of a new repository now reconciles deletions.** On an empty
  cache this is a no-op, but the run does build a seen-set of every node id,
  costing memory proportional to the repository's item count. That cost already
  existed for `--full` runs.
- **An established incremental-only cache pays for one full walk** on its next
  `sync`, on both phases. For a large repository this can exceed a single
  rate-limit window; the run pauses and resumes as any `--full` run does. This
  is a one-time cost and the direct point of the change.
- **`--no-wait` still cannot reconcile deletions, but no longer silently.** The
  seen-set hoist fixes the in-process wait path, not the exit-75 path, where the
  set dies with the process. A user who habitually runs `--no-wait` on a
  repository too large for one rate-limit window gets full walks and correct
  watermarks but never soft-deletes. `last_reconciled_at` makes that legible in
  both `status` and the rendered `README.md` — a standing `reconciled: None`
  beside a populated `full_sync` is exactly this situation, and the remedy is an
  uninterrupted `--full`. Closing the gap itself still requires persisting the
  seen-set, deferred in Scope.
- **A phase that errors mid-walk does not stamp.** `complete` is reached only
  on `SyncStop::Completed`; an error propagates before it, leaving the marker
  unset and the cursor checkpointed, so the next run resumes and still walks
  fully.
- **README.md needs updating** — the `--full` example at line 31, the deletion
  reconciliation paragraph at line 132, and the flag table at line 159 all
  describe full walks as opt-in. The repository's `CLAUDE.md` needs the same
  correction: its `sync/` architecture bullet says `--full` "walks everything
  and reconciles deletions … only on a fresh full run", which goes stale once
  an implicit trigger exists. The rendered tree's `CLAUDE.md` does not document
  sync flags, so `claude_md_template.md` and its parity tests are unaffected.
