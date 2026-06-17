use std::process::Command;

/// Resolve a GitHub token: explicit env var first, then `gh auth token`.
pub fn resolve_token(env_token: Option<String>) -> anyhow::Result<String> {
    if let Some(t) = env_token.filter(|t| !t.trim().is_empty()) {
        return Ok(t);
    }
    let out = Command::new("gh").args(["auth", "token"]).output();
    match out {
        Ok(o) if o.status.success() => {
            let t = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if t.is_empty() {
                anyhow::bail!("no token: set GITHUB_TOKEN or run `gh auth login`");
            }
            Ok(t)
        }
        _ => anyhow::bail!("no token: set GITHUB_TOKEN or install gh and run `gh auth login`"),
    }
}

/// Build an Octocrab client from a token, with secondary-rate-limit retry
/// configured.
pub fn build_client(token: &str) -> anyhow::Result<octocrab::Octocrab> {
    use std::sync::Arc;

    use octocrab::service::middleware::retry::NoOpRateLimitMetrics;
    use octocrab::service::middleware::retry::RetryConfig;
    let client = octocrab::Octocrab::builder()
        .personal_token(token.to_string())
        .add_retry_config(RetryConfig::HandleRateLimits {
            metrics: Arc::new(NoOpRateLimitMetrics),
            max_retries: 3,
            min_wait_seconds: 5,
        })
        .build()?;
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_token_takes_precedence() {
        let t = resolve_token(Some("ghp_fromenv".into())).unwrap();
        assert_eq!(t, "ghp_fromenv");
    }

    #[test]
    fn blank_env_token_falls_through() {
        // Blank env token is ignored; falls through to `gh auth token` which may
        // succeed (gh installed+authed) or fail. Either way must not panic.
        let result = resolve_token(Some("   ".into()));
        if let Err(e) = result {
            assert!(e.to_string().contains("no token"));
        }
    }

    #[tokio::test]
    async fn build_client_succeeds() {
        assert!(build_client("ghp_x").is_ok());
    }
}
