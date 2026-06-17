pub mod issues;
pub mod prs;
pub mod taxonomy;

use std::time::Duration;

use rusqlite::Connection;

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

/// Top-level sync driver: runs taxonomy + selected entity phases while honoring
/// the rate-limit reserve floor and checkpoint/resume semantics.
pub struct Syncer<'a> {
    pub client: &'a GithubClient,
    pub conn: &'a Connection,
    pub rl: &'a mut RateLimitStore,
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

        tracing::info!(owner = %owner, repo = %repo, full = self.full, "starting sync");
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

        // Stamp last_full_sync only when a --full run reconciled BOTH phases.
        if self.full && do_issues && do_prs {
            crate::store::repo_meta::set_last_full_sync(self.conn, chrono::Utc::now().timestamp())?;
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
        let full = self.full;
        let qt = phase.query_type();

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
                        crate::sync::issues::sync_issues(
                            self.client,
                            self.conn,
                            owner,
                            repo,
                            full,
                            budget_ok,
                        )
                        .await?
                    }
                    Phase::Prs => {
                        crate::sync::prs::sync_prs(
                            self.client,
                            self.conn,
                            owner,
                            repo,
                            full,
                            budget_ok,
                        )
                        .await?
                    }
                }
            };

            match stop {
                SyncStop::Completed => {
                    tracing::info!(phase = ?phase, "phase complete");
                    return Ok(Outcome::Completed);
                }
                SyncStop::Paused => {
                    if self.no_wait {
                        tracing::info!("paused at rate-limit floor (no-wait); checkpoint saved");
                        return Ok(Outcome::Paused);
                    }
                    let reset = self.rl.get(budget::Resource::GraphQL)?.map(|b| b.reset);
                    let wait = reset.map_or(Duration::ZERO, |r| {
                        (r - chrono::Utc::now()).to_std().unwrap_or(Duration::ZERO)
                    });
                    let wait = self.max_wait.map_or(wait, |cap| wait.min(cap));
                    tracing::info!(
                        wait_secs = wait.as_secs(),
                        "rate-limit floor reached; waiting until reset"
                    );
                    tokio::time::sleep(wait).await;
                }
            }
        }
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
}
