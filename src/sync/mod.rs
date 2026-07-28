pub mod issues;
pub mod prs;
pub(crate) mod relationships;
pub mod taxonomy;

use std::time::Duration;

use chrono::DateTime;
use chrono::Utc;
use rusqlite::Connection;

use crate::clock::Clock;
use crate::config::Reserve;
use crate::github::GithubClient;
use crate::ratelimit::budget;
use crate::ratelimit::estimator::CostEstimator;
use crate::ratelimit::estimator::QueryType;
use crate::ratelimit::store::RateLimitStore;
use crate::sync::issues::SyncStop;

/// `Some(cursor)` when the page reports a next page and a cursor is present.
pub(crate) fn next_cursor(has_next: bool, end: Option<&str>) -> Option<String> {
    if has_next {
        end.map(ToString::to_string)
    } else {
        None
    }
}

/// The terminal outcome of a [`Syncer::run`] invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The sync walked to completion.
    Completed,
    /// The sync paused on the reserve floor with `--no-wait`; resumable.
    Paused,
}

/// Which entity phases a sync should run. Empty ⇒ all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnlyTarget {
    Issues,
    Prs,
}

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

/// Everything an entity walk needs that varies neither between pages nor
/// across a pause.
///
/// Exists to keep the phase functions under clippy's seven-argument limit as
/// cross-cutting concerns accumulate; `clock` was the second such concern
/// after `conn`.
pub struct WalkCtx<'a> {
    /// The GitHub API client for this walk.
    pub client: &'a GithubClient,
    /// The SQLite connection this walk reads from and persists into.
    pub conn: &'a Connection,
    /// The repository owner.
    pub owner: &'a str,
    /// The repository name.
    pub repo: &'a str,
    /// Wall-clock source; production passes [`crate::clock::SystemClock`].
    pub clock: &'a dyn Clock,
}

/// Top-level sync driver: runs taxonomy + selected entity phases while honoring
/// the rate-limit reserve floor and checkpoint/resume semantics.
pub struct Syncer<'a> {
    pub client: &'a GithubClient,
    pub conn: &'a Connection,
    pub rl: &'a mut RateLimitStore,
    /// Wall-clock source; production passes [`crate::clock::SystemClock`].
    pub clock: &'a dyn Clock,
    pub reserve: Reserve,
    /// `--cost-ceiling` override forwarded to the [`CostEstimator`]; `None`
    /// uses the per-`QueryType` conservative ceiling.
    pub cost_ceiling: Option<u64>,
    pub no_wait: bool,
    pub max_wait: Option<Duration>,
    pub full: bool,
    /// Phases to run; empty ⇒ both issues and PRs.
    pub only: Vec<OnlyTarget>,
}

impl Syncer<'_> {
    fn wants(&self, t: OnlyTarget) -> bool {
        self.only.is_empty() || self.only.contains(&t)
    }

    /// Run a full sync (taxonomy + selected entity phases), honoring the
    /// reserve floor.
    ///
    /// # Errors
    ///
    /// Returns an error on any GraphQL/HTTP transport, persistence, or
    /// rate-limit store failure.
    pub async fn run(&mut self, owner: &str, repo: &str) -> anyhow::Result<Outcome> {
        let mut estimator = CostEstimator::new(self.cost_ceiling);

        // `explicit_full`, not `full`: this is the `--full` flag as given, before
        // any phase resolves whether it actually needs a full walk (logged
        // per-phase below) -- naming it `full` here previously read as
        // contradicting the very next "no full walk recorded" line.
        tracing::info!(owner = %owner, repo = %repo, explicit_full = self.full, "starting sync");
        crate::sync::taxonomy::sync_labels(self.client, self.conn, owner, repo).await?;
        crate::sync::taxonomy::sync_milestones(self.client, self.conn, owner, repo).await?;

        let do_issues = self.wants(OnlyTarget::Issues);
        let do_prs = self.wants(OnlyTarget::Prs);

        if do_issues
            && self
                .run_phase(owner, repo, Phase::Issues, &mut estimator)
                .await?
                == Outcome::Paused
        {
            return Ok(Outcome::Paused);
        }
        if do_prs
            && self
                .run_phase(owner, repo, Phase::Prs, &mut estimator)
                .await?
                == Outcome::Paused
        {
            return Ok(Outcome::Paused);
        }

        tracing::info!("sync complete");
        Ok(Outcome::Completed)
    }

    /// Drive one entity phase through the pause/wait/resume loop.
    async fn run_phase(
        &mut self,
        owner: &str,
        repo: &str,
        phase: Phase,
        estimator: &mut CostEstimator,
    ) -> anyhow::Result<Outcome> {
        let reserve = self.reserve;
        let qt = phase.query_type();
        let state = crate::store::sync_state::get(self.conn, phase.entity())?;
        let started_fresh = state.resume_cursor.is_none();
        // A phase walks everything when forced, or when it has never recorded
        // a completed full walk. The marker is per-phase, so `--only issues`
        // decides on the issues marker alone.
        let full = self.full || state.last_full_sync_at.is_none();
        if full && !self.full {
            tracing::info!(phase = ?phase, "no full walk recorded; walking everything");
        }
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let ctx = WalkCtx {
            client: self.client,
            conn: self.conn,
            owner,
            repo,
            clock: self.clock,
        };

        loop {
            // budget_ok: reconcile the shared store from the authoritative
            // post-call headers, feed the estimator the observed used-delta, then
            // atomically reserve the next call's estimated cost against the floor
            // (BEGIN IMMEDIATE) so two processes sharing one token DB can't both
            // slip past the floor. The reborrows of `self.rl` and `estimator` are
            // scoped to this block so `self.rl.get` after the match is legal.
            let stop = {
                let rl = &mut *self.rl;
                let estimator = &mut *estimator;
                let budget_ok = |headers: &http::HeaderMap| -> bool {
                    let Some((res, b)) = budget::parse_rate_headers(headers) else {
                        // Unparseable headers: proceed rather than wedge the sync.
                        return true;
                    };
                    // Only the GraphQL bucket governs the sync phases.
                    if res != budget::Resource::GraphQL {
                        return true;
                    }
                    // used-delta since the last observation, read before record() overwrites the
                    // cached bucket.
                    let prev_used = rl
                        .get(budget::Resource::GraphQL)
                        .ok()
                        .flatten()
                        .map(|prev| prev.used);
                    let used_delta = b.used.saturating_sub(prev_used.unwrap_or(b.used));
                    estimator.observe(qt, used_delta);
                    rl.record(res, &b).ok();
                    let est = estimator.estimate(qt);
                    let floor = reserve.floor_for(b.limit);
                    // A transient DB error must not wedge the sync, so map it to "proceed".
                    rl.try_reserve(budget::Resource::GraphQL, floor, est)
                        .unwrap_or(true)
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
            crate::store::sync_state::mark_reconciled(self.conn, phase.entity(), self.clock.now())?;
        }
        Ok(Outcome::Completed)
    }
}

/// Internal phase selector for [`Syncer::run_phase`].
#[derive(Debug, Clone, Copy)]
enum Phase {
    Issues,
    Prs,
}

impl Phase {
    fn query_type(self) -> QueryType {
        match self {
            Phase::Issues => QueryType::IssuesPage,
            Phase::Prs => QueryType::PrsPage,
        }
    }

    /// The `sync_state.entity_type` key for this phase.
    ///
    /// Doubles as the table name for deletion reconciliation — the entity
    /// keys and the table names deliberately coincide as `issues` and
    /// `pull_requests`. Renaming one without the other fails at runtime
    /// against a nonexistent table, not at compile time.
    fn entity(self) -> &'static str {
        match self {
            Phase::Issues => crate::sync::issues::ENTITY,
            Phase::Prs => crate::sync::prs::ENTITY,
        }
    }
}

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
        assert_eq!(
            wait_for(None, now, Some(Duration::from_secs(60))),
            Duration::ZERO
        );
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
