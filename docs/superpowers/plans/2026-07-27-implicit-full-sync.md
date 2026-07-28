# Implicit Full Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `meta-fetch sync` walk a repository's entire history automatically when a phase has never recorded a completed full walk, tracked per phase.

**Architecture:** Three layered changes. First, wall-clock reads become injectable through a `Clock` trait, carried with the other per-walk invariants in a new `WalkCtx` struct. Second, the seen-set and the "did this walk start fresh" bit move from per-call scope in `sync_issues`/`sync_prs` up to `run_phase`'s retry loop, which fixes a shipping bug where a walk that pauses can never reconcile deletions. Third, `sync_state` gains `last_full_sync_at` and `last_reconciled_at` columns; a phase walks fully when `--full` is passed or its own `last_full_sync_at` is `NULL`.

**Tech Stack:** Rust 2024, `rusqlite` (bundled SQLite) with `rusqlite_migration`, `chrono`, `clap`, `tokio` (current-thread), `wiremock` for integration tests, `insta` for render snapshots.

**Design spec:** `docs/superpowers/specs/2026-07-27-implicit-full-sync-design.md`

## Global Constraints

- MSRV is **1.96.0** — CI gates on it. No newer language or stdlib features.
- Edition **2024**.
- `#[allow(...)]` is **forbidden**. Fix the lint, or use a narrowly-scoped `#[expect(..., reason = "...")]`. The crate-wide test exception for `clippy::unwrap_used` already exists in `src/lib.rs`; integration tests under `tests/` opt in with `#![allow(clippy::unwrap_used)]` at the top (pre-existing, do not touch).
- `[workspace.lints]` denies clippy `pedantic`; `just lint` runs `cargo clippy --all-targets --all-features -- -D warnings`. Clippy's `too_many_arguments` threshold is **7** and there is no `clippy.toml` — keep every function at or below 7 parameters.
- Formatting is **nightly** rustfmt: always `cargo +nightly fmt`, never stable.
- Every public item needs a doc comment; every `pub fn` returning `Result` needs an `# Errors` section (pedantic enforces this).
- Import traits as `use anyhow::Context;` — never `use ... as _;`.
- Commit messages: conventional commits, imperative mood.

## Task-to-commit mapping

The spec groups this work as three logical changes. Each task below commits on its own; if you prefer the spec's grouping, Tasks 1–3 squash into the clock commit, Task 4 is the hoist commit, and Tasks 5–7 are the marker commit.

---

### Task 1: The `Clock` trait

**Files:**
- Create: `src/clock.rs`
- Modify: `src/lib.rs` (add `pub mod clock;`)

**Interfaces:**
- Consumes: nothing.
- Produces: `github_repo_meta_fetch::clock::Clock` (trait, method `fn now(&self) -> DateTime<Utc>`), `clock::SystemClock` (unit struct), `clock::FixedClock(pub DateTime<Utc>)`.

- [ ] **Step 1: Write the failing test**

Create `src/clock.rs` containing only the test module for now:

```rust
#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn fixed_clock_returns_its_instant() {
        let at = dt("2026-07-20T09:31:52Z");
        let c = FixedClock(at);
        assert_eq!(c.now(), at);
        // Stable across reads — this is the property tests depend on.
        assert_eq!(c.now(), at);
    }

    #[test]
    fn system_clock_returns_a_plausible_now() {
        // Not asserting an exact instant; only that it is a real wall clock
        // somewhere after this code was written.
        let c = SystemClock;
        assert!(c.now() > dt("2020-01-01T00:00:00Z"));
    }

    #[test]
    fn clock_is_usable_behind_a_trait_object() {
        let at = dt("2026-07-20T09:31:52Z");
        let c: &dyn Clock = &FixedClock(at);
        assert_eq!(c.now(), at);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib clock::`
Expected: FAIL to compile — `cannot find type FixedClock in this scope`, `cannot find type SystemClock`, `cannot find trait Clock`.

- [ ] **Step 3: Write the implementation**

Prepend to `src/clock.rs`, above the test module:

```rust
use chrono::DateTime;
use chrono::Utc;

/// A source of wall-clock time.
///
/// Injected wherever the crate reads "now" so tests can pin it. Covers reading
/// time only — sleeping still goes through `tokio::time::sleep`.
pub trait Clock {
    /// The current instant, in UTC.
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
///
/// Public rather than `#[cfg(test)]` because the integration tests under
/// `tests/` link this crate externally and cannot see test-only items.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}
```

Add to `src/lib.rs`, keeping the module list alphabetical (between `cli` and `config`):

```rust
pub mod clock;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib clock::`
Expected: PASS, 3 tests.

- [ ] **Step 5: Lint and format**

Run: `cargo +nightly fmt && just lint`
Expected: no diff from fmt, clippy clean.

- [ ] **Step 6: Commit**

```bash
git add src/clock.rs src/lib.rs
git commit -m "feat(clock): add an injectable Clock trait

Wall-clock reads are about to become worth asserting in tests. Introduce
Clock with a SystemClock production impl and a FixedClock test impl.
FixedClock is pub, not cfg(test), because integration tests under tests/
link the crate externally."
```

---

### Task 2: Extract `wait_for`

**Files:**
- Modify: `src/sync/mod.rs:191-194` (the inline wait computation) and its test module

**Interfaces:**
- Consumes: nothing.
- Produces: `fn wait_for(reset: Option<DateTime<Utc>>, now: DateTime<Utc>, max_wait: Option<Duration>) -> Duration` — module-private in `crate::sync`.

- [ ] **Step 1: Write the failing test**

Append to `src/sync/mod.rs` (the file has no test module yet — create one at the end):

```rust
#[cfg(test)]
mod wait_tests {
    use chrono::DateTime;

    use super::*;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn future_reset_yields_the_difference() {
        let now = dt("2026-07-20T09:00:00Z");
        let reset = dt("2026-07-20T09:05:00Z");
        assert_eq!(wait_for(Some(reset), now, None), Duration::from_secs(300));
    }

    #[test]
    fn past_reset_clamps_to_zero() {
        let now = dt("2026-07-20T09:05:00Z");
        let reset = dt("2026-07-20T09:00:00Z");
        assert_eq!(wait_for(Some(reset), now, None), Duration::ZERO);
    }

    #[test]
    fn missing_reset_yields_zero() {
        let now = dt("2026-07-20T09:00:00Z");
        assert_eq!(wait_for(None, now, Some(Duration::from_secs(60))), Duration::ZERO);
    }

    #[test]
    fn max_wait_caps_a_longer_wait() {
        let now = dt("2026-07-20T09:00:00Z");
        let reset = dt("2026-07-20T09:05:00Z");
        let capped = wait_for(Some(reset), now, Some(Duration::from_secs(30)));
        assert_eq!(capped, Duration::from_secs(30));
    }

    #[test]
    fn max_wait_does_not_extend_a_shorter_wait() {
        let now = dt("2026-07-20T09:00:00Z");
        let reset = dt("2026-07-20T09:00:10Z");
        let wait = wait_for(Some(reset), now, Some(Duration::from_secs(600)));
        assert_eq!(wait, Duration::from_secs(10));
    }
}
```

Add the needed import at the top of `src/sync/mod.rs`, alongside the existing imports:

```rust
use chrono::DateTime;
use chrono::Utc;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib sync::wait_tests`
Expected: FAIL to compile — `cannot find function wait_for in this scope`.

- [ ] **Step 3: Write the implementation**

Add to `src/sync/mod.rs`, just above the `Syncer` struct:

```rust
/// How long to wait for `reset`, clamped at zero and capped by `max_wait`.
///
/// A reset already in the past — or absent entirely — yields
/// [`Duration::ZERO`] rather than blocking.
fn wait_for(
    reset: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    max_wait: Option<Duration>,
) -> Duration {
    let wait = reset.map_or(Duration::ZERO, |r| {
        (r - now).to_std().unwrap_or(Duration::ZERO)
    });
    max_wait.map_or(wait, |cap| wait.min(cap))
}
```

Replace the inline computation in `run_phase` (currently `src/sync/mod.rs:190-194`):

```rust
                    let reset = self.rl.get(budget::Resource::GraphQL)?.map(|b| b.reset);
                    let wait = wait_for(reset, chrono::Utc::now(), self.max_wait);
```

Delete the two now-dead lines that computed `wait` by hand. Keep the `tracing::info!` and `tokio::time::sleep(wait).await;` that follow.

> Note: `chrono::Utc::now()` here is temporary — Task 3 replaces it with `self.clock.now()`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib sync::wait_tests`
Expected: PASS, 5 tests.

- [ ] **Step 5: Verify nothing else broke**

Run: `just test`
Expected: full suite passes.

- [ ] **Step 6: Lint, format, commit**

```bash
cargo +nightly fmt && just lint
git add src/sync/mod.rs
git commit -m "refactor(sync): extract the rate-limit wait computation

wait_for is pure, so the clamping and capping rules get direct test
coverage that the inline version could not have without a real sleep."
```

---

### Task 3: `WalkCtx` and clock threading

**Files:**
- Modify: `src/sync/mod.rs` (add `WalkCtx`, add `Syncer::clock`, rewrite `run_phase` call sites)
- Modify: `src/sync/issues.rs:604-614` (signature)
- Modify: `src/sync/prs.rs:433-443` (signature)
- Modify: `src/cli.rs:154-164` (`Syncer` literal)
- Modify: `tests/sync_issues.rs` (7 direct calls + 2 `Syncer` literals)
- Modify: `tests/sync_prs.rs` (6 direct calls + 1 `Syncer` literal)
- Modify: `tests/end_to_end.rs` (2 direct calls)
- Modify: `src/model.rs:540-541` (replace `Utc::now()` in a test with a literal)

**Interfaces:**
- Consumes: `clock::Clock`, `clock::SystemClock` (Task 1); `wait_for` (Task 2).
- Produces:
  - `pub struct WalkCtx<'a> { pub client: &'a GithubClient, pub conn: &'a Connection, pub owner: &'a str, pub repo: &'a str, pub clock: &'a dyn Clock }` in `crate::sync`
  - `pub async fn sync_issues<F, S>(ctx: &WalkCtx<'_>, full: bool, seen: &mut HashSet<String, S>, budget_ok: F) -> anyhow::Result<SyncStop> where F: FnMut(&http::HeaderMap) -> bool, S: std::hash::BuildHasher`
  - `pub async fn sync_prs<F, S>(...)` — same shape

  > `seen` is generic over the hasher because a `pub fn` taking
  > `HashSet<String>` with the default hasher trips `clippy::implicit_hasher`,
  > and this crate already resolves that lint by genericizing rather than
  > suppressing: `mark_deleted_except<S: std::hash::BuildHasher>` at
  > `src/store/mod.rs:115`, `src/store/issues.rs:215`, and
  > `src/store/prs.rs:313`. Every caller builds with `HashSet::new()`, so `S`
  > infers as `RandomState` and no call site names it.
  - `Syncer` gains `pub clock: &'a dyn Clock`

> **Why `seen` moves now, before it is used differently:** adding `clock` alone would take these functions to 7 parameters and `seen` to 8, tripping `too_many_arguments`. `WalkCtx` absorbs five, so both fit. The functions still populate `seen` and reconcile internally in this task — Task 4 changes that.

- [ ] **Step 1: Add `WalkCtx` and the `Syncer` field**

In `src/sync/mod.rs`, add above `Syncer`:

```rust
/// Everything an entity walk needs that varies neither between pages nor
/// across a pause.
///
/// Exists to keep the phase functions under clippy's seven-argument limit as
/// cross-cutting concerns accumulate; `clock` was the second such concern
/// after `conn`.
pub struct WalkCtx<'a> {
    pub client: &'a GithubClient,
    pub conn: &'a Connection,
    pub owner: &'a str,
    pub repo: &'a str,
    pub clock: &'a dyn Clock,
}
```

Add to the `Syncer` struct, after `client`/`conn`/`rl`:

```rust
    /// Wall-clock source; production passes [`crate::clock::SystemClock`].
    pub clock: &'a dyn Clock,
```

Add the import at the top of `src/sync/mod.rs`:

```rust
use crate::clock::Clock;
```

- [ ] **Step 2: Change the phase function signatures**

In `src/sync/issues.rs`, replace the `sync_issues` signature:

```rust
pub async fn sync_issues<F, S>(
    ctx: &crate::sync::WalkCtx<'_>,
    full: bool,
    seen: &mut HashSet<String, S>,
    mut budget_ok: F,
) -> anyhow::Result<SyncStop>
where
    F: FnMut(&http::HeaderMap) -> bool,
    S: std::hash::BuildHasher,
{
```

> The hasher generic is required, not optional: without it `clippy::implicit_hasher`
> fires on these public functions, and this crate's policy is to fix the lint
> rather than `#[expect]` it. Precedent: `mark_deleted_except<S: BuildHasher>`
> in `src/store/mod.rs`, `src/store/issues.rs`, `src/store/prs.rs`.

Inside the body, delete the local `let mut seen: HashSet<String> = HashSet::new();` (the parameter replaces it) and rebind the old parameter names once at the top so the rest of the body is untouched:

```rust
    let WalkCtx { client, conn, owner, repo, clock: _ } = *ctx;
```

> `clock` is bound to `_` in this task — Task 5 starts using it. Destructuring here keeps the ~80 lines of body below unchanged.

Apply the identical change to `sync_prs` in `src/sync/prs.rs`.

- [ ] **Step 3: Rewrite `run_phase`'s dispatch**

In `src/sync/mod.rs`, before the `loop` in `run_phase`, build the context and the seen-set:

```rust
        let ctx = WalkCtx {
            client: self.client,
            conn: self.conn,
            owner,
            repo,
            clock: self.clock,
        };
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
```

Replace the two match arms that call the phase functions:

```rust
                match phase {
                    Phase::Issues => {
                        crate::sync::issues::sync_issues(&ctx, full, &mut seen, budget_ok).await?
                    }
                    Phase::Prs => {
                        crate::sync::prs::sync_prs(&ctx, full, &mut seen, budget_ok).await?
                    }
                }
```

Replace the temporary clock read from Task 2:

```rust
                    let wait = wait_for(reset, self.clock.now(), self.max_wait);
```

- [ ] **Step 4: Update the production `Syncer` literal**

In `src/cli.rs`, add to the `Syncer { ... }` literal at line 154:

```rust
                clock: &crate::clock::SystemClock,
```

> Static promotion makes `&SystemClock` a `'static` reference — no `let` binding needed.

- [ ] **Step 5: Add a test helper and update all test call sites**

Add this helper to **each** of `tests/sync_issues.rs`, `tests/sync_prs.rs`, and `tests/end_to_end.rs`, near the other helpers at the top:

```rust
/// A `WalkCtx` pinned to a fixed instant, for tests that call a phase
/// function directly.
fn walk_ctx<'a>(
    client: &'a GithubClient,
    conn: &'a rusqlite::Connection,
    clock: &'a github_repo_meta_fetch::clock::FixedClock,
) -> sync::WalkCtx<'a> {
    sync::WalkCtx { client, conn, owner: "o", repo: "r", clock }
}

/// The instant every direct-call test pins its clock to.
fn test_clock() -> github_repo_meta_fetch::clock::FixedClock {
    github_repo_meta_fetch::clock::FixedClock(
        chrono::DateTime::parse_from_rfc3339("2026-07-20T09:31:52Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
    )
}
```

Rewrite each direct call. There are 15: 7 in `tests/sync_issues.rs`, 6 in `tests/sync_prs.rs`, 2 in `tests/end_to_end.rs`. Each changes from:

```rust
    let stop = sync::issues::sync_issues(&client, &conn, "o", "r", false, |_h| true)
        .await
        .unwrap();
```

to:

```rust
    let clk = test_clock();
    let ctx = walk_ctx(&client, &conn, &clk);
    let mut seen = std::collections::HashSet::new();
    let stop = sync::issues::sync_issues(&ctx, false, &mut seen, |_h| true)
        .await
        .unwrap();
```

Preserve each call's existing `full` argument — several pass `true`. Add `clock: &clk,` to the three `Syncer` literals (`tests/sync_issues.rs:324,408`, `tests/sync_prs.rs:239`), binding `let clk = test_clock();` above each.

`chrono` and `rusqlite` must be dev-dependencies for the test crates to name those types. Verify with `cargo tree -e dev -i chrono` and `cargo tree -e dev -i rusqlite`; if either is missing, add it with `cargo add --dev chrono rusqlite` (do **not** hand-edit `Cargo.toml`).

- [ ] **Step 6: Remove the test-only `Utc::now()`**

In `src/model.rs:540-541`, replace both `chrono::Utc::now()` calls with a fixed literal so the test is deterministic:

```rust
            created_at: dt("2026-01-01T00:00:00Z"),
            updated_at: dt("2026-01-01T00:00:00Z"),
```

If the enclosing test module has no `dt` helper, add one:

```rust
    fn dt(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }
```

- [ ] **Step 7: Verify the whole suite passes**

Run: `just test`
Expected: PASS. This task is behaviour-preserving — any failure is a threading mistake, not an intended change.

- [ ] **Step 8: Verify no production `Utc::now()` remains outside `clock.rs`**

Run: `rg -n 'Utc::now' src/`
Expected: exactly one hit, `src/clock.rs` inside `impl Clock for SystemClock`.

- [ ] **Step 9: Lint, format, commit**

```bash
cargo +nightly fmt && just lint
git add -A
git commit -m "refactor(sync): thread an injected clock through the walk

Phase functions gain a WalkCtx carrying the five per-walk invariants,
which drops them from six arguments to four and leaves room for the
clock without tripping clippy's seven-argument limit.

Behaviour is unchanged; SystemClock reads the same wall clock the
inline Utc::now() calls did."
```

---

### Task 4: Hoist the seen-set into `run_phase`

**Files:**
- Modify: `src/sync/mod.rs` (`run_phase` control flow, add `Phase::entity`)
- Modify: `src/sync/issues.rs` (drop `started_fresh` and the reconcile call)
- Modify: `src/sync/prs.rs` (same)
- Delete: `src/store/issues.rs:215-220` and `src/store/prs.rs:313-318` (`mark_deleted_except` wrappers)
- Modify: `src/store/issues.rs`, `src/store/prs.rs` test modules (retarget at `store::mark_deleted_except`)
- Modify: `tests/sync_issues.rs` (new regression test)

**Interfaces:**
- Consumes: `WalkCtx`, the phase signatures from Task 3.
- Produces: `Phase::entity(self) -> &'static str` returning `"issues"` / `"pull_requests"`.

**Why this task exists:** `sync_issues` computes `started_fresh` from the resume cursor at entry (`src/sync/issues.rs:618`), but `run_phase` re-calls it after a pause — so the continuation reads the cursor its own predecessor just checkpointed, sees `started_fresh == false`, and skips reconciliation. This is not specific to `--no-wait`; the default sleep-until-reset path has the same hole. Since `complete` then clears the cursor, the next invocation restarts from the top, so on a repository too large for one rate-limit window, reconciliation is unreachable through *any* invocation.

> **Correction, made during execution.** An earlier version of this step
> proposed a test that called `sync_issues` directly twice with a shared
> external `seen` and asserted `seen.len() == 2`. That test cannot go red:
> Task 3 already moved `seen` to a `run_phase` local (`src/sync/mod.rs:164`,
> before the loop at `:166`), because making it a phase-function parameter
> meant something had to own it. The surviving bug is the `started_fresh` gate
> *inside* each phase function, which only manifests through the retry loop.
>
> The test must therefore drive `Syncer::run` with `no_wait: false` — so the
> in-process sleep-until-reset path is the one exercised — and assert on the
> **persisted `deleted` flag** of a pre-seeded stale row, not on a manually
> invoked `mark_deleted_except`. The `x-ratelimit-reset` header the test
> fixtures already use (`1781564821` = 2026-06-15) is earlier than the
> `FixedClock` instant (2026-07-20), so `wait_for` returns zero and the test
> does not really sleep.

- [ ] **Step 1: Write the failing regression test**

Add to `tests/sync_issues.rs`:

```rust
#[tokio::test]
async fn reconciles_after_an_in_process_pause() {
    // Two pages. budget_ok returns false once, between them, with no_wait
    // false — so run_phase sleeps and re-calls the phase. The continuation
    // must still reconcile, which today it does not.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("issues("))
        .respond_with(rl_headers(ResponseTemplate::new(200).set_body_string(page(
            &issue_node(1, "2026-06-10T00:00:00Z"),
            true,
            "CUR1",
        ))))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("issues("))
        .respond_with(rl_headers(ResponseTemplate::new(200).set_body_string(page(
            &issue_node(2, "2026-06-09T00:00:00Z"),
            false,
            "",
        ))))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let conn = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn, "o", "r").unwrap();

    // A cached issue that the walk will not see: it must be soft-deleted.
    conn.execute(
        "INSERT INTO issues (node_id, number, title, state, body, created_at, updated_at, deleted)
         VALUES ('I_stale', 99, 'gone', 'OPEN', '', 0, 0, 0)",
        [],
    )
    .unwrap();

    let clk = test_clock();
    let ctx = sync::WalkCtx {
        client: &client,
        conn: &conn,
        owner: "o",
        repo: "r",
        clock: &clk,
    };
    let mut seen = std::collections::HashSet::new();

    // Simulate run_phase's retry loop: pause once, then continue.
    let mut allow = false;
    let stop = sync::issues::sync_issues(&ctx, true, &mut seen, |_h| {
        let now = allow;
        allow = true;
        now
    })
    .await
    .unwrap();
    assert!(matches!(stop, sync::issues::SyncStop::Paused));

    let stop = sync::issues::sync_issues(&ctx, true, &mut seen, |_h| true)
        .await
        .unwrap();
    assert!(matches!(stop, sync::issues::SyncStop::Completed));

    // The seen-set spans both calls, so reconciliation is now correct.
    assert_eq!(seen.len(), 2, "seen-set must accumulate across the pause");

    let deleted = store::mark_deleted_except(&conn, "issues", &seen).unwrap();
    assert_eq!(deleted, 1, "the stale issue should be soft-deleted");
}
```

> `store::mark_deleted_except` is `pub(crate)` today. Widen it to `pub` in Step 4 so the integration test can call it — that is also what `run_phase` will use.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test sync_issues reconciles_after_an_in_process_pause`
Expected: FAIL to compile — `mark_deleted_except` is private. After Step 4 widens it, the meaningful assertion is `seen.len() == 2`, which fails today because each call allocates its own set.

- [ ] **Step 3: Add `Phase::entity`**

In `src/sync/mod.rs`, add to `impl Phase`:

```rust
    /// The `sync_state.entity_type` key for this phase.
    ///
    /// Doubles as the table name for deletion reconciliation — the entity
    /// keys and the table names deliberately coincide as `issues` and
    /// `pull_requests`. Renaming one without the other fails at runtime
    /// against a nonexistent table, not at compile time.
    fn entity(self) -> &'static str {
        match self {
            Phase::Issues => "issues",
            Phase::Prs => "pull_requests",
        }
    }
```

- [ ] **Step 4: Widen `store::mark_deleted_except` and delete the wrappers**

In `src/store/mod.rs`, change `pub(crate) fn mark_deleted_except` to `pub fn mark_deleted_except` and give it an `# Errors` section:

```rust
/// Mark every non-deleted row in `table` whose `node_id` is not in `seen` as
/// deleted. Returns the count newly marked. `table` MUST be a trusted literal
/// (it is interpolated into SQL); callers pass `"issues"` or
/// `"pull_requests"`.
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] if the update fails.
pub fn mark_deleted_except<S: std::hash::BuildHasher>(
```

Delete `src/store/issues.rs:215-220` and `src/store/prs.rs:313-318` (the two thin wrappers). In their test modules, replace calls like `mark_deleted_except(&conn, &seen)` with `crate::store::mark_deleted_except(&conn, "issues", &seen)` (or `"pull_requests"`). Remove the now-unused `use crate::store::prs::mark_deleted_except;` at `src/sync/prs.rs:28`.

- [ ] **Step 5: Strip reconciliation from the phase functions**

In `src/sync/issues.rs`, delete the `started_fresh` binding and the trailing reconcile block:

Two separate deletions. First, near the top of the function, remove the binding
and the comment above it:

```rust
    // Capture whether this is a fresh (non-resumed) run before mutating the cursor.
    let started_fresh = state.resume_cursor.is_none();
```

Second, at the bottom of the function, remove the reconcile block and its
comment, leaving `Ok(SyncStop::Completed)` as the last statement:

```rust
    // Soft-delete reconciliation runs only on a fresh full pass. A resumed full
    // run has an incomplete seen-set (it re-walked only the remaining pages), so
    // reconciling would wrongly delete entities seen on the skipped pages.
    if full && started_fresh {
        crate::store::issues::mark_deleted_except(conn, &seen)?;
    }
```

Keep everything else, including the `if full { seen.insert(...); }` population. Apply the same deletion to `src/sync/prs.rs`.

- [ ] **Step 6: Restructure `run_phase`**

In `src/sync/mod.rs`, `run_phase` becomes (the `budget_ok` closure body is unchanged — elided here only for brevity; do not delete it):

```rust
        let state = crate::store::sync_state::get(self.conn, phase.entity())?;
        let started_fresh = state.resume_cursor.is_none();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let ctx = WalkCtx {
            client: self.client,
            conn: self.conn,
            owner,
            repo,
            clock: self.clock,
        };

        loop {
            let stop = {
                let rl = &mut *self.rl;
                let estimator = &mut *estimator;
                let budget_ok = |headers: &http::HeaderMap| -> bool {
                    // ... unchanged ...
                };
                match phase {
                    Phase::Issues => {
                        crate::sync::issues::sync_issues(&ctx, full, &mut seen, budget_ok).await?
                    }
                    Phase::Prs => {
                        crate::sync::prs::sync_prs(&ctx, full, &mut seen, budget_ok).await?
                    }
                }
            };

            match stop {
                // Was `return Ok(Outcome::Completed)`; now breaks so the
                // reconciliation below is reachable.
                SyncStop::Completed => {
                    tracing::info!(phase = ?phase, "phase complete");
                    break;
                }
                SyncStop::Paused => {
                    if self.no_wait {
                        tracing::info!("paused at rate-limit floor (no-wait); checkpoint saved");
                        return Ok(Outcome::Paused);
                    }
                    let reset = self.rl.get(budget::Resource::GraphQL)?.map(|b| b.reset);
                    let wait = wait_for(reset, self.clock.now(), self.max_wait);
                    tracing::info!(
                        wait_secs = wait.as_secs(),
                        "rate-limit floor reached; waiting until reset"
                    );
                    tokio::time::sleep(wait).await;
                }
            }
        }

        // Reconciliation needs a walk that both started fresh and finished:
        // a walk resumed from an *earlier process*'s checkpoint re-walked only
        // the pages after it, so its seen-set is incomplete.
        if full && started_fresh {
            crate::store::mark_deleted_except(self.conn, phase.entity(), &seen)?;
        }
        Ok(Outcome::Completed)
```

- [ ] **Step 7: Run the regression test**

Run: `cargo test --test sync_issues reconciles_after_an_in_process_pause`
Expected: PASS.

- [ ] **Step 8: Run the full suite**

Run: `just test`
Expected: PASS. Existing `--full` reconciliation tests must still pass — the gate moved, it did not loosen.

- [ ] **Step 9: Lint, format, commit**

```bash
cargo +nightly fmt && just lint
git add -A
git commit -m "fix(sync): reconcile deletions after an in-process pause

sync_issues captured started_fresh at entry and owned its seen-set as a
local, but run_phase re-calls it after a pause. The continuation read the
cursor its own predecessor had just checkpointed, concluded it was a
resumed run, and skipped reconciliation -- on the default wait path as
well as under --no-wait. Because complete() then cleared the cursor, the
next invocation restarted from the top, so a repository too large for one
rate-limit window could never reconcile through any invocation.

Move both the seen-set and the started-fresh decision to run_phase, whose
retry loop is the true boundary of a walk. --no-wait remains unfixed: the
set dies with the process."
```

---

### Task 5: Per-phase markers — schema, store, and display

**Files:**
- Create: `src/store/migrations/0004_phase_full_sync.sql`
- Modify: `src/store/mod.rs:25-31` (register the migration)
- Modify: `src/store/sync_state.rs` (two fields, `complete` signature, `mark_reconciled`)
- Modify: `src/store/repo_meta.rs` (drop the field, delete `set_last_full_sync`, fix the test)
- Modify: `src/sync/mod.rs:99-102` (delete the whole-repo stamp)
- Modify: `src/sync/issues.rs`, `src/sync/prs.rs` (pass `full_sync_at` to `complete`)
- Modify: `src/cli.rs:239` (status display)
- Modify: `src/render/mod.rs:337-366` (rendered README display)

**Interfaces:**
- Consumes: `WalkCtx.clock` (Task 3).
- Produces:
  - `SyncState.last_full_sync_at: Option<DateTime<Utc>>`, `SyncState.last_reconciled_at: Option<DateTime<Utc>>`
  - `sync_state::complete(conn: &Connection, entity_type: &str, watermark: Option<DateTime<Utc>>, full_sync_at: Option<DateTime<Utc>>) -> rusqlite::Result<()>`
  - `sync_state::mark_reconciled(conn: &Connection, entity_type: &str, at: DateTime<Utc>) -> rusqlite::Result<()>`
  - `RepoMeta` **loses** `last_full_sync_at`; `repo_meta::set_last_full_sync` is **deleted**

- [ ] **Step 1: Write the failing store tests**

Add to the `tests` module in `src/store/sync_state.rs`:

```rust
    #[test]
    fn full_pass_stamps_and_incremental_pass_preserves() {
        let conn = crate::store::open_in_memory().unwrap();
        let at = dt("2026-07-20T09:31:52Z");

        // A full pass stamps exactly the instant handed in. Asserting the
        // exact value also pins complete()'s parameter order: a transposition
        // with `watermark` would show up here immediately.
        complete(&conn, "issues", Some(dt("2026-06-10T00:00:00Z")), Some(at)).unwrap();
        let s = get(&conn, "issues").unwrap();
        assert_eq!(s.last_full_sync_at, Some(at));
        assert_eq!(s.updated_watermark, Some(dt("2026-06-10T00:00:00Z")));

        // A later incremental pass advances the watermark but must not clear
        // the marker -- this is the COALESCE path.
        complete(&conn, "issues", Some(dt("2026-06-11T00:00:00Z")), None).unwrap();
        let s = get(&conn, "issues").unwrap();
        assert_eq!(s.last_full_sync_at, Some(at));
        assert_eq!(s.updated_watermark, Some(dt("2026-06-11T00:00:00Z")));
    }

    #[test]
    fn fresh_state_has_no_markers() {
        let conn = crate::store::open_in_memory().unwrap();
        let s = get(&conn, "issues").unwrap();
        assert_eq!(s.last_full_sync_at, None);
        assert_eq!(s.last_reconciled_at, None);
    }

    #[test]
    fn mark_reconciled_touches_one_phase_only() {
        let conn = crate::store::open_in_memory().unwrap();
        let at = dt("2026-07-20T09:31:52Z");
        complete(&conn, "issues", None, Some(at)).unwrap();
        complete(&conn, "pull_requests", None, Some(at)).unwrap();

        mark_reconciled(&conn, "issues", at).unwrap();

        let i = get(&conn, "issues").unwrap();
        let p = get(&conn, "pull_requests").unwrap();
        assert_eq!(i.last_reconciled_at, Some(at));
        assert_eq!(p.last_reconciled_at, None);
        // Other columns survive.
        assert_eq!(i.last_full_sync_at, Some(at));
        assert_eq!(i.run_phase, RunPhase::Done);
    }
```

Add to the `tests` module in `src/store/mod.rs`:

```rust
#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn v4_backfills_phase_markers_from_repo_meta() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations().to_version(&mut conn, 3).unwrap();
        conn.execute(
            "INSERT INTO repo_meta (id, owner, repo, last_full_sync_at) VALUES (1,'o','r',1700)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sync_state (entity_type, run_phase) VALUES ('issues','done')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sync_state (entity_type, run_phase) VALUES ('pull_requests','done')",
            [],
        )
        .unwrap();

        migrations().to_version(&mut conn, 4).unwrap();

        for entity in ["issues", "pull_requests"] {
            let (full, recon): (Option<i64>, Option<i64>) = conn
                .query_row(
                    "SELECT last_full_sync_at, last_reconciled_at FROM sync_state \
                     WHERE entity_type=?1",
                    [entity],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(full, Some(1700), "{entity} should carry the backfill");
            assert_eq!(recon, None, "{entity} reconciliation has no backfill source");
        }
    }

    #[test]
    fn v4_leaves_phases_unstamped_when_repo_meta_is_null() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations().to_version(&mut conn, 3).unwrap();
        conn.execute("INSERT INTO repo_meta (id, owner, repo) VALUES (1,'o','r')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO sync_state (entity_type, run_phase) VALUES ('issues','done')",
            [],
        )
        .unwrap();

        migrations().to_version(&mut conn, 4).unwrap();

        let full: Option<i64> = conn
            .query_row(
                "SELECT last_full_sync_at FROM sync_state WHERE entity_type='issues'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(full, None, "a NULL source must not stamp a marker");
    }

    #[test]
    fn v4_drops_the_repo_meta_column() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations().to_latest(&mut conn).unwrap();
        let err = conn.query_row("SELECT last_full_sync_at FROM repo_meta", [], |r| {
            r.get::<_, Option<i64>>(0)
        });
        assert!(err.is_err(), "the whole-repo column should be gone");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib store::`
Expected: FAIL to compile — `no field last_full_sync_at on SyncState`, `cannot find function mark_reconciled`, `complete` takes 3 arguments not 4.

- [ ] **Step 3: Write the migration**

Create `src/store/migrations/0004_phase_full_sync.sql`:

```sql
ALTER TABLE sync_state ADD COLUMN last_full_sync_at INTEGER;
ALTER TABLE sync_state ADD COLUMN last_reconciled_at INTEGER;

-- Backfill: a repository that already completed a --full run covering both
-- phases must not be surprised by another full walk on upgrade. A repository
-- that never ran --full yields NULL here and takes the implicit full walk,
-- which is the intended upgrade path.
--
-- last_reconciled_at gets no backfill: the old column recorded that a --full
-- run finished, never whether it was the fresh pass that also reconciled, so
-- there is nothing faithful to derive.
UPDATE sync_state
SET last_full_sync_at = (SELECT last_full_sync_at FROM repo_meta WHERE id = 1);

-- The per-phase markers are now the only record of full-walk history.
ALTER TABLE repo_meta DROP COLUMN last_full_sync_at;
```

Register it in `src/store/mod.rs`:

```rust
        M::up(include_str!("migrations/0004_phase_full_sync.sql")),
```

- [ ] **Step 4: Extend `SyncState` and `complete`, add `mark_reconciled`**

In `src/store/sync_state.rs`, add both fields to the struct:

```rust
    /// When this phase last completed a walk of its entire history, fresh or
    /// resumed. `None` until one completes.
    pub last_full_sync_at: Option<DateTime<Utc>>,
    /// When this phase last reconciled deletions, which requires a full walk
    /// that both started fresh and ran to completion. `None` until one does.
    pub last_reconciled_at: Option<DateTime<Utc>>,
```

Extend `get`'s SQL and row mapping, and both `None` defaults in the no-row branch:

```rust
        "SELECT updated_watermark, resume_cursor, run_phase, last_full_sync_at, \
         last_reconciled_at FROM sync_state WHERE entity_type=?1",
```

```rust
                last_full_sync_at: r
                    .get::<_, Option<i64>>(3)?
                    .and_then(|s| DateTime::from_timestamp(s, 0)),
                last_reconciled_at: r
                    .get::<_, Option<i64>>(4)?
                    .and_then(|s| DateTime::from_timestamp(s, 0)),
```

Replace `complete`:

```rust
/// Advance the watermark and mark the run done.  Clears the resume cursor.
///
/// `full_sync_at` stamps `last_full_sync_at` when this pass walked the entire
/// history; pass `None` for an incremental pass. The `COALESCE` is what keeps
/// an incremental pass from clearing a marker an earlier full pass set.
///
/// # Errors
///
/// Propagates any [`rusqlite::Error`] from the underlying execute call.
pub fn complete(
    conn: &Connection,
    entity_type: &str,
    watermark: Option<DateTime<Utc>>,
    full_sync_at: Option<DateTime<Utc>>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO sync_state \
            (entity_type, updated_watermark, resume_cursor, run_phase, last_full_sync_at) \
         VALUES (?1, ?2, NULL, 'done', ?3) \
         ON CONFLICT(entity_type) DO UPDATE SET \
            updated_watermark=excluded.updated_watermark, \
            resume_cursor=NULL, \
            run_phase='done', \
            last_full_sync_at=COALESCE(excluded.last_full_sync_at, \
                                       sync_state.last_full_sync_at)",
        rusqlite::params![
            entity_type,
            watermark.map(|w| w.timestamp()),
            full_sync_at.map(|f| f.timestamp())
        ],
    )?;
    Ok(())
}

/// Record that this phase reconciled deletions at `at`.
///
/// Written separately from [`complete`], which runs inside the phase function
/// before and independently of the reconciliation `run_phase` performs after
/// its retry loop. A crash between the two leaves rows soft-deleted with the
/// marker unset, so the next full walk reconciles again — `mark_deleted_except`
/// is idempotent, making that a redundant pass rather than a corruption.
///
/// # Errors
///
/// Propagates any [`rusqlite::Error`] from the underlying execute call.
pub fn mark_reconciled(
    conn: &Connection,
    entity_type: &str,
    at: DateTime<Utc>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sync_state SET last_reconciled_at=?2 WHERE entity_type=?1",
        rusqlite::params![entity_type, at.timestamp()],
    )?;
    Ok(())
}
```

Update the existing `cursor_then_complete` test's `complete` call to pass a fourth argument, `None`.

- [ ] **Step 5: Remove the whole-repo timestamp**

In `src/store/repo_meta.rs`: delete the `last_full_sync_at` field from `RepoMeta`, drop it from `get`'s `SELECT` and row mapping, delete `set_last_full_sync` entirely, and delete the `assert_eq!(m.last_full_sync_at, None);` line from `migrations_apply_and_meta_round_trips`.

In `src/sync/mod.rs`, delete the stamp block at lines 99-102 **and** the now-unused `do_issues && do_prs` condition guarding it. `do_issues` and `do_prs` are still used by the phase dispatch above — keep those bindings.

- [ ] **Step 6: Stamp from the phase functions**

In `src/sync/issues.rs`, change the destructure to bind the clock and update the `complete` call:

```rust
    let WalkCtx { client, conn, owner, repo, clock } = *ctx;
```

```rust
    sync_state::complete(conn, ENTITY, run_max.max(watermark), full.then(|| clock.now()))?;
```

Apply the identical change in `src/sync/prs.rs`.

- [ ] **Step 7: Update the `status` display**

In `src/cli.rs`, delete the `last_full_sync_at` line and add a marker line inside each phase block:

```rust
        println!("run_phase: {:?}", s.run_phase);
        println!("watermark: {:?}", s.updated_watermark);
        println!("full_sync: {:?}", s.last_full_sync_at);
        println!("reconciled: {:?}", s.last_reconciled_at);
```

and after the existing PR watermark line:

```rust
        println!("prs full_sync: {:?}", ps.last_full_sync_at);
        println!("prs reconciled: {:?}", ps.last_reconciled_at);
```

Move the `let (dep_edges, sub_edges)` / `relationships:` lines above `run_phase:` if needed so each phase's four lines stay contiguous.

- [ ] **Step 8: Update the rendered README**

In `src/render/mod.rs`, delete the `last_full` block at lines 337-345. Add a formatting helper near the top of the file:

```rust
/// Render an optional timestamp for the tree's `README.md`.
fn fmt_ts(t: Option<chrono::DateTime<chrono::Utc>>) -> String {
    t.map_or_else(
        || "never".to_string(),
        |dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
}
```

Replace the `watermark` / `pr_watermark` bindings with `fmt_ts` calls and add the four marker bindings:

```rust
    let watermark = fmt_ts(issues_state.updated_watermark);
    let issues_full = fmt_ts(issues_state.last_full_sync_at);
    let issues_recon = fmt_ts(issues_state.last_reconciled_at);
    // ... and for prs_state:
    let pr_watermark = fmt_ts(prs_state.updated_watermark);
    let prs_full = fmt_ts(prs_state.last_full_sync_at);
    let prs_recon = fmt_ts(prs_state.last_reconciled_at);
```

Update the `format!` string: drop the trailing `- last full sync: {last_full}\n` and insert the per-phase lines beside their siblings:

```rust
         - issues watermark: {watermark}\n\
         - issues sync phase: {run_phase}\n\
         - issues last full sync: {issues_full}\n\
         - issues last reconciled: {issues_recon}\n\
```

```rust
         - PRs watermark: {pr_watermark}\n\
         - PRs sync phase: {pr_phase}\n\
         - PRs last full sync: {prs_full}\n\
         - PRs last reconciled: {prs_recon}\n",
```

- [ ] **Step 9: Add display coverage**

Add to `tests/cli_dispatch.rs`:

```rust
#[test]
fn status_prints_per_phase_markers() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("c.sqlite3");
    seed_db_with_pr(&db);
    Command::cargo_bin("meta-fetch")
        .unwrap()
        .args(["status", "--db", db.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("full_sync:"))
        .stdout(contains("reconciled:"))
        .stdout(contains("prs full_sync:"))
        .stdout(contains("prs reconciled:"));
}

#[test]
fn status_no_longer_prints_the_whole_repo_marker() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("c.sqlite3");
    seed_db(&db);
    Command::cargo_bin("meta-fetch")
        .unwrap()
        .args(["status", "--db", db.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("last_full_sync_at:").not());
}
```

> These follow the shape of the existing `status_from_db_prints_slug` and reuse
> its `seed_db` / `seed_db_with_pr` helpers. `.not()` needs
> `use predicates::prelude::*;` — check whether that import is already at the
> top of the file and add it if not.

Add to `tests/end_to_end.rs`, alongside the existing README assertions:

```rust
    assert!(
        readme.contains("- issues last full sync: "),
        "README should report the issues full-walk marker:\n{readme}"
    );
    assert!(
        readme.contains("- PRs last reconciled: "),
        "README should report the PRs reconcile marker:\n{readme}"
    );
```

- [ ] **Step 10: Run the suite**

Run: `just test`
Expected: PASS. If a render snapshot fails, inspect it with `cargo insta review` — only `README.md` content should differ, and there is no README snapshot, so a snapshot failure means something unintended changed.

- [ ] **Step 11: Lint, format, commit**

```bash
cargo +nightly fmt && just lint
git add -A
git commit -m "feat(store): track full-walk and reconcile times per phase

sync_state gains last_full_sync_at and last_reconciled_at, replacing the
whole-repo repo_meta.last_full_sync_at. Per-phase is the right grain:
--only issues should decide on the issues history alone, and the old
column was stamped only when a --full run covered both phases.

Two markers rather than one because completing a full walk and
reconciling deletions are separable -- a walk resumed across a process
boundary does the former without the latter, and that is now legible in
status and the rendered README instead of silent.

The migration drops a column, which is irreversible under this
forward-only framework; reverting means a corrective migration deriving
the old value as MIN across the per-phase markers."
```

---

### Task 6: Drive the implicit full walk

**Files:**
- Modify: `src/sync/mod.rs` (`run_phase` decision + `mark_reconciled` stamp)
- Modify: `tests/sync_issues.rs` (behaviour tests)

**Interfaces:**
- Consumes: `Phase::entity` (Task 4), `SyncState.last_full_sync_at`, `sync_state::mark_reconciled` (Task 5).
- Produces: no new API — `Syncer.full` keeps its `bool` type and its "force a full walk" meaning.

- [ ] **Step 1: Write the failing behaviour tests**

Add to `tests/sync_issues.rs`:

```rust
#[tokio::test]
async fn first_sync_walks_fully_then_goes_incremental() {
    let server = MockServer::start().await;
    // One page, one issue, older than the watermark seeded below. An
    // incremental walk would early-stop before persisting it; a full walk
    // must not.
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(rl_headers(ResponseTemplate::new(200).set_body_string(page(
            &issue_node(1, "2026-01-01T00:00:00Z"),
            false,
            "",
        ))))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let conn = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn, "o", "r").unwrap();
    // A watermark exists but no full walk was ever recorded.
    store::sync_state::complete(
        &conn,
        "issues",
        Some(
            chrono::DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        ),
        None,
    )
    .unwrap();

    let clk = test_clock();
    let mut rl = RateLimitStore::open_in_memory("fp").unwrap();
    let mut syncer = Syncer {
        client: &client,
        conn: &conn,
        rl: &mut rl,
        clock: &clk,
        reserve: Reserve::Percent(0.10),
        cost_ceiling: Some(30),
        no_wait: true,
        max_wait: None,
        full: false, // not forced -- the marker must drive it
        only: vec![sync::OnlyTarget::Issues],
    };
    syncer.run("o", "r").await.unwrap();

    // Walked despite being below the watermark.
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM issues", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "an unstamped phase must walk past the watermark");

    let s = store::sync_state::get(&conn, "issues").unwrap();
    assert_eq!(s.last_full_sync_at, Some(clk.0), "the walk should stamp the marker");
    assert_eq!(s.last_reconciled_at, Some(clk.0), "a fresh full walk reconciles");
}

#[tokio::test]
async fn only_prs_leaves_the_issues_marker_untouched() {
    // NOTE: this test goes in `tests/sync_prs.rs`, not `tests/sync_issues.rs`
    // -- it needs that file's `page` helper, which builds a `pullRequests`
    // page, and its `rl` header helper. An empty node string yields
    // `"nodes":[]`, which is the empty page this test wants.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(rl(
            ResponseTemplate::new(200).set_body_string(page("", false, "")),
        ))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let conn = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn, "o", "r").unwrap();

    let clk = test_clock();
    let mut rl = RateLimitStore::open_in_memory("fp").unwrap();
    let mut syncer = Syncer {
        client: &client,
        conn: &conn,
        rl: &mut rl,
        clock: &clk,
        reserve: Reserve::Percent(0.10),
        cost_ceiling: Some(30),
        no_wait: true,
        max_wait: None,
        full: false,
        only: vec![sync::OnlyTarget::Prs],
    };
    syncer.run("o", "r").await.unwrap();

    let i = store::sync_state::get(&conn, "issues").unwrap();
    let p = store::sync_state::get(&conn, "pull_requests").unwrap();
    assert_eq!(i.last_full_sync_at, None, "the skipped phase must stay unstamped");
    assert_eq!(p.last_full_sync_at, Some(clk.0), "the run phase should stamp");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test sync_issues first_sync_walks_fully`
Expected: FAIL — `assert_eq!(n, 1)` gets `0`, because the walk early-stopped on the seeded watermark.

- [ ] **Step 3: Resolve the mode from the marker**

In `src/sync/mod.rs`, `run_phase` already reads `state` (Task 4, Step 6). Replace the `let full = self.full;` binding near the top of the function with a marker-aware one, placed right after the `state` read:

```rust
        // A phase walks everything when forced, or when it has never recorded
        // a completed full walk. The marker is per-phase, so `--only issues`
        // decides on the issues marker alone.
        let full = self.full || state.last_full_sync_at.is_none();
        if full && !self.full {
            tracing::info!(phase = ?phase, "no full walk recorded; walking everything");
        }
```

Resolve it **before** the retry loop so a phase's mode stays fixed across a pause-and-wait cycle.

- [ ] **Step 4: Stamp the reconcile marker**

In the same function, extend the post-loop reconciliation block:

```rust
        if full && started_fresh {
            crate::store::mark_deleted_except(self.conn, phase.entity(), &seen)?;
            crate::store::sync_state::mark_reconciled(
                self.conn,
                phase.entity(),
                self.clock.now(),
            )?;
        }
```

- [ ] **Step 5: Run the tests**

Run: `cargo test --test sync_issues`
Expected: PASS.

- [ ] **Step 6: Add the `--no-wait` gap test**

Add to `tests/sync_issues.rs`, pinning the documented residual limitation so a future change to it is deliberate:

```rust
#[tokio::test]
async fn no_wait_resume_stamps_the_walk_but_not_reconciliation() {
    // A walk resumed from an earlier process's checkpoint completes and
    // stamps last_full_sync_at, but its seen-set is incomplete, so it must
    // not claim to have reconciled.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(rl_headers(ResponseTemplate::new(200).set_body_string(page(
            &issue_node(2, "2026-06-09T00:00:00Z"),
            false,
            "",
        ))))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let conn = store::open_in_memory().unwrap();
    store::repo_meta::ensure(&conn, "o", "r").unwrap();
    // A checkpoint left behind by a prior, now-exited process.
    store::sync_state::set_cursor(&conn, "issues", Some("CUR1"), store::sync_state::RunPhase::Paginating)
        .unwrap();

    let clk = test_clock();
    let mut rl = RateLimitStore::open_in_memory("fp").unwrap();
    let mut syncer = Syncer {
        client: &client,
        conn: &conn,
        rl: &mut rl,
        clock: &clk,
        reserve: Reserve::Percent(0.10),
        cost_ceiling: Some(30),
        no_wait: true,
        max_wait: None,
        full: false,
        only: vec![sync::OnlyTarget::Issues],
    };
    syncer.run("o", "r").await.unwrap();

    let s = store::sync_state::get(&conn, "issues").unwrap();
    assert_eq!(s.last_full_sync_at, Some(clk.0), "the walk completed");
    assert_eq!(
        s.last_reconciled_at, None,
        "a cross-process resume has an incomplete seen-set and must not claim reconciliation"
    );
}
```

- [ ] **Step 7: Run the full suite**

Run: `just test`
Expected: PASS.

- [ ] **Step 8: Lint, format, commit**

```bash
cargo +nightly fmt && just lint
git add -A
git commit -m "feat(sync): walk fully when a phase has no recorded full walk

sync now reconciles deletions without being asked. A phase walks
everything when --full is passed or its own last_full_sync_at is NULL,
so the first sync of a repository is a reconciling full walk and an
existing incremental-only cache converts on its next run. It also makes
a paused walk resume correctly whether or not the caller remembers
--full.

Note for automation: pausing at the reserve floor was never gated on
--full, but the first post-upgrade sync of a large repository will now
routinely exit 75 (EX_TEMPFAIL) without --full being passed. Treat 75 as
retryable."
```

---

### Task 7: Documentation

**Files:**
- Modify: `README.md:31`, `README.md:132`, `README.md:159`
- Modify: `CLAUDE.md` (the `sync/` architecture bullet)
- Modify: `docs/superpowers/specs/2026-07-27-implicit-full-sync-design.md` (status line)

**Interfaces:**
- Consumes: the finished behaviour from Tasks 4-6.
- Produces: nothing code-facing.

- [ ] **Step 1: Update the README example**

`README.md:31` currently presents `--full` as the way to get a full walk. Change the comment to reflect that it now *forces* one:

```markdown
meta-fetch sync octocat/hello-world --full  # force a full walk even if one is recorded
```

- [ ] **Step 2: Update the deletion reconciliation paragraph**

Replace the paragraph at `README.md:132` with:

```markdown
Deletion is the one thing an incremental pass cannot see (a deleted issue no longer appears in any page). A full walk covers the entire repository and soft-deletes cached items that no longer exist upstream; their rendered files are pruned on the next render. You do not have to ask for it: each phase records when it last walked everything, and a phase with no such record walks fully on its next run. So the first sync of a repository reconciles deletions on its own, and a cache that has only ever been synced incrementally converts on its next run. `--full` forces a walk even when one is already recorded.

Reconciliation is stricter than the walk itself: it needs a walk that both started fresh and ran to completion, because a walk resumed from a checkpoint has only seen the pages after it. A walk that pauses at the rate-limit floor and waits for the reset still reconciles — it never left the process. A walk resumed after `--no-wait` exited does not, because the set of items it saw died with the previous process.

`meta-fetch status` and the tree's own `README.md` report both facts per phase. A populated `full_sync` beside `reconciled: never` means the history is current but soft-deletes are outstanding; a single uninterrupted `--full` clears it.
```

- [ ] **Step 3: Update the flag table and note the exit code**

At `README.md:159`, change the `--full` row's description from `Walk everything; reconcile deletions` to `Force a full walk even when one is recorded`. Add a sentence near the exit-code documentation stating that a first sync of a large repository can now exit 75 without `--full`, and that automation should treat 75 as retryable.

- [ ] **Step 4: Update `CLAUDE.md`**

In the `sync/` architecture bullet, the sentence describing `--full` as the only path to a full walk is now wrong. Replace it with a description of the per-phase `last_full_sync_at` marker driving implicit full walks, `--full` as the force override, and `last_reconciled_at` recording whether reconciliation actually ran. Add the spec path (`docs/superpowers/specs/2026-07-27-implicit-full-sync-design.md`) alongside the existing relationships spec reference.

- [ ] **Step 5: Mark the spec implemented**

Change the spec's `**Status:** proposal` line to `**Status:** implemented` followed by the commit range, matching the convention in `docs/superpowers/specs/2026-07-24-tree-claude-md-design.md`.

- [ ] **Step 6: Verify**

Run: `just test && just lint`
Expected: PASS — docs-only, but confirms nothing was disturbed.

Run: `rg -n 'last_full_sync_at' README.md CLAUDE.md`
Expected: no stale references to the removed whole-repo column.

- [ ] **Step 7: Commit**

```bash
git add README.md CLAUDE.md docs/
git commit -m "docs: describe implicit full walks and the reconcile marker

--full is now a force override rather than the only path to a full walk,
and the two per-phase markers distinguish a completed walk from one that
also reconciled. Also flags that plain sync can now exit 75 on a large
repository's first run, which automation must treat as retryable."
```

---

## Verification checklist

Run before considering the plan complete:

- [ ] `just test` — full suite green
- [ ] `just lint` — clippy clean under `-D warnings`
- [ ] `cargo +nightly fmt --check` — no diff
- [ ] `cargo msrv verify` (or build with 1.96.0) — MSRV gate holds
- [ ] `rg -n 'Utc::now' src/` — one hit only, in `src/clock.rs`
- [ ] `rg -n 'last_full_sync_at' src/store/repo_meta.rs` — no hits
- [ ] A second `meta-fetch sync` against the same DB performs no full walk (check `full_sync` in `status` is stable and the run is fast)
