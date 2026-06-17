use std::path::PathBuf;

use etcetera::AppStrategy;
use etcetera::AppStrategyArgs;
use etcetera::choose_app_strategy;

const APP: &str = "github-repo-meta-fetch";

// `choose_app_strategy` is the CLI-oriented strategy: XDG base dirs on Unix
// (Linux and macOS alike) and native AppData on Windows.
// (`choose_native_strategy` would instead use `~/Library` on macOS —
// appropriate for GUI apps, not this.)
fn strategy() -> impl AppStrategy {
    choose_app_strategy(AppStrategyArgs {
        top_level_domain: "codes".to_string(),
        author: "leblanc".to_string(),
        app_name: APP.to_string(),
    })
    .expect("home dir must resolve")
}

/// Default repo DB path:
/// {data_dir}/github-repo-meta-fetch/{owner}/{repo}.sqlite3
#[must_use]
pub fn repo_db_path(owner: &str, repo: &str) -> PathBuf {
    strategy()
        .data_dir()
        .join(owner)
        .join(format!("{repo}.sqlite3"))
}

/// Rate-limit state DB: {state_dir}/github-repo-meta-fetch/rate-limits.sqlite3
/// Falls back to the data dir on platforms without a state dir (e.g. Windows).
#[must_use]
pub fn rate_limit_db_path() -> PathBuf {
    let s = strategy();
    s.state_dir()
        .unwrap_or_else(|| s.data_dir())
        .join("rate-limits.sqlite3")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_db_path_includes_owner_and_repo() {
        let p = repo_db_path("octocat", "hello-world");
        let s = p.to_string_lossy();
        assert!(s.ends_with("octocat/hello-world.sqlite3"), "got {s}");
        assert!(s.contains(APP), "path must be namespaced: {s}");
    }

    #[test]
    fn rate_limit_path_is_shared_filename() {
        let p = rate_limit_db_path();
        assert!(p.to_string_lossy().ends_with("rate-limits.sqlite3"));
        assert!(p.to_string_lossy().contains(APP));
    }
}
