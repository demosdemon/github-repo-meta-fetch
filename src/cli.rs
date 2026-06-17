use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;
use clap::Parser;
use clap::Subcommand;

use crate::model::RepoSlug;

/// Which sync phase(s) to run; when empty, all phases run.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnlyArg {
    Issues,
    Prs,
}

impl OnlyArg {
    fn to_target(self) -> crate::sync::OnlyTarget {
        match self {
            OnlyArg::Issues => crate::sync::OnlyTarget::Issues,
            OnlyArg::Prs => crate::sync::OnlyTarget::Prs,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "meta-fetch",
    version,
    about = "Sync GitHub repo metadata to a Markdown tree"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Args, Debug)]
pub struct Sync {
    repo: RepoSlug,
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long)]
    db: Option<PathBuf>,
    #[arg(long, default_value = "10%")]
    reserve: crate::config::Reserve,
    #[arg(long)]
    cost_ceiling: Option<u64>,
    #[arg(long)]
    max_wait: Option<String>,
    #[arg(long)]
    no_wait: bool,
    #[arg(long)]
    full: bool,
    #[arg(long, value_enum, action = clap::ArgAction::Append)]
    only: Vec<OnlyArg>,
}

#[derive(Args, Debug)]
pub struct Render {
    repo: Option<RepoSlug>,
    #[arg(long)]
    db: Option<PathBuf>,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct Status {
    repo: Option<RepoSlug>,
    #[arg(long)]
    db: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Incremental fetch + render.
    Sync(Sync),
    /// Re-project from the local `SQLite` cache (no network).
    Render(Render),
    /// Show last sync time, watermark, checkpoint, and rate budgets.
    Status(Status),
}

/// Resolve the repo DB path: explicit --db wins, else derive from owner/repo.
///
/// # Errors
///
/// Returns an error if neither `db` nor `repo` is provided.
pub fn resolve_db(
    db: Option<&std::path::Path>,
    repo: Option<&RepoSlug>,
) -> anyhow::Result<PathBuf> {
    if let Some(p) = db {
        return Ok(p.to_path_buf());
    }
    let slug = repo.ok_or_else(|| anyhow::anyhow!("provide <owner/repo> or --db PATH"))?;
    Ok(crate::paths::repo_db_path(&slug.owner, &slug.repo))
}

/// Resolve --out: explicit wins; else `"{repo_name}-meta"`.
#[must_use]
pub fn resolve_out(out: Option<&std::path::Path>, repo_name: &str) -> PathBuf {
    out.map_or_else(
        || PathBuf::from(format!("{repo_name}-meta")),
        std::path::Path::to_path_buf,
    )
}

/// Dispatch the parsed CLI command.
///
/// # Errors
///
/// Propagates any error from the subcommand handler.
pub async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Command::Sync(cmd) => cmd.run().await,
        Command::Render(cmd) => cmd.run(),
        Command::Status(cmd) => cmd.run(),
    }
}

impl Sync {
    async fn run(self) -> anyhow::Result<ExitCode> {
        let Self {
            repo,
            out,
            db,
            reserve,
            cost_ceiling,
            max_wait,
            no_wait,
            full,
            only,
        } = self;

        let db_path = resolve_db(db.as_deref(), Some(&repo))?;
        let conn = crate::store::open(&db_path)?;
        crate::store::repo_meta::ensure(&conn, &repo.owner, &repo.repo)?;

        let token = crate::github::auth::resolve_token(std::env::var("GITHUB_TOKEN").ok())?;
        let fingerprint = crate::fingerprint::token_fingerprint(&token);
        let octo = crate::github::auth::build_client(&token)?;
        let client = crate::github::GithubClient::new(octo);
        let mut rl = crate::ratelimit::store::RateLimitStore::open(
            &crate::paths::rate_limit_db_path(),
            &fingerprint,
        )?;

        let max_wait = max_wait
            .map(|s| humantime::parse_duration(&s))
            .transpose()?;

        let outcome = {
            let mut syncer = crate::sync::Syncer {
                client: &client,
                conn: &conn,
                rl: &mut rl,
                reserve,
                cost_ceiling,
                no_wait,
                max_wait,
                full,
                only: only.iter().map(|o| o.to_target()).collect(),
            };
            syncer.run(&repo.owner, &repo.repo).await?
        };

        // Grow padding to the current max number across issues + pull_requests, then
        // render.
        let max_num: i64 = conn.query_row(
            "SELECT MAX(m) FROM (
            SELECT COALESCE(MAX(number),0) AS m FROM issues
            UNION ALL
            SELECT COALESCE(MAX(number),0) AS m FROM pull_requests)",
            [],
            |r| r.get(0),
        )?;
        crate::store::repo_meta::grow_padding_width(
            &conn,
            crate::store::repo_meta::width_for(max_num),
        )?;
        let meta = crate::store::repo_meta::get(&conn)?
            .ok_or_else(|| anyhow::anyhow!("repo_meta missing after sync"))?;
        let out_dir = resolve_out(out.as_deref(), &meta.repo);
        crate::render::render_tree(&conn, &out_dir)?;

        if outcome == crate::sync::Outcome::Paused {
            eprintln!("paused at rate-limit floor; re-run to resume");
            // Exit code 75 (EX_TEMPFAIL) signals a retryable failure; emit it only
            // after all I/O above is flushed.
            return Ok(ExitCode::from(75));
        }
        Ok(ExitCode::SUCCESS)
    }
}

impl Render {
    fn run(self) -> anyhow::Result<ExitCode> {
        let Self { repo, db, out } = self;
        let db_path = resolve_db(db.as_deref(), repo.as_ref())?;
        let conn = crate::store::open(&db_path)?;
        let meta = crate::store::repo_meta::get(&conn)?
            .ok_or_else(|| anyhow::anyhow!("no synced data in {}", db_path.display()))?;
        let out_dir = resolve_out(out.as_deref(), &meta.repo);
        crate::render::render_tree(&conn, &out_dir)?;
        Ok(ExitCode::SUCCESS)
    }
}

impl Status {
    fn run(self) -> anyhow::Result<ExitCode> {
        let Self { repo, db } = self;
        let db_path = resolve_db(db.as_deref(), repo.as_ref())?;
        let conn = crate::store::open(&db_path)?;
        let meta = crate::store::repo_meta::get(&conn)?
            .ok_or_else(|| anyhow::anyhow!("no synced data in {}", db_path.display()))?;
        let s = crate::store::sync_state::get(&conn, "issues")?;
        let open: i64 = conn.query_row("SELECT COUNT(*) FROM issues WHERE deleted=0", [], |r| {
            r.get(0)
        })?;
        let deleted: i64 =
            conn.query_row("SELECT COUNT(*) FROM issues WHERE deleted=1", [], |r| {
                r.get(0)
            })?;
        let (pr_open, pr_draft, pr_closed, pr_merged) =
            crate::store::prs::effective_state_counts(&conn)?;
        let pr_deleted: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pull_requests WHERE deleted=1",
            [],
            |r| r.get(0),
        )?;
        let ps = crate::store::sync_state::get(&conn, "pull_requests")?;
        println!("{}/{}", meta.owner, meta.repo);
        println!("issues: {open} active, {deleted} soft-deleted");
        println!("run_phase: {:?}", s.run_phase);
        println!("watermark: {:?}", s.updated_watermark);
        println!("last_full_sync_at: {:?}", meta.last_full_sync_at);
        println!(
            "prs: {pr_open} open, {pr_draft} draft, {pr_closed} closed, {pr_merged} merged, {pr_deleted} soft-deleted"
        );
        println!("prs run_phase: {:?}", ps.run_phase);
        println!("prs watermark: {:?}", ps.updated_watermark);
        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn db_explicit_wins() {
        let p = resolve_db(Some(std::path::Path::new("/tmp/x.sqlite3")), None).unwrap();
        assert_eq!(p, PathBuf::from("/tmp/x.sqlite3"));
    }

    #[test]
    fn db_from_slug() {
        let slug = RepoSlug::from_str("octocat/hello").unwrap();
        let p = resolve_db(None, Some(&slug)).unwrap();
        assert!(p.to_string_lossy().ends_with("octocat/hello.sqlite3"));
    }

    #[test]
    fn db_requires_something() {
        assert!(resolve_db(None, None).is_err());
    }

    #[test]
    fn out_defaults_to_repo_meta() {
        assert_eq!(resolve_out(None, "hello"), PathBuf::from("hello-meta"));
    }

    #[test]
    fn cli_parses_sync() {
        let cli = Cli::try_parse_from(["meta-fetch", "sync", "octocat/hello", "--full"]).unwrap();
        match cli.command {
            Command::Sync(Sync { repo, full, .. }) => {
                assert_eq!(repo.repo, "hello");
                assert!(full);
            }
            _ => panic!("expected sync"),
        }
    }

    #[test]
    fn cli_parses_only_repeatable() {
        let cli = Cli::try_parse_from([
            "meta-fetch",
            "sync",
            "octocat/hello",
            "--only",
            "prs",
            "--only",
            "issues",
        ])
        .unwrap();
        match cli.command {
            Command::Sync(Sync { only, .. }) => {
                assert_eq!(only.len(), 2);
                assert!(only.contains(&OnlyArg::Prs));
                assert!(only.contains(&OnlyArg::Issues));
            }
            _ => panic!("expected sync"),
        }
    }
}
