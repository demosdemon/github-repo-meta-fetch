# github-repo-meta-fetch
Synchronizes issues, pull requests, and other metadata about a GitHub repo into a Markdown file tree for AI agents to review offline.

## Usage

```bash
export GITHUB_TOKEN=...     # or rely on `gh auth login`
meta-fetch sync octocat/hello-world          # incremental fetch + render to ./hello-world-meta
meta-fetch sync octocat/hello-world --full   # also reconcile deletions
meta-fetch render octocat/hello-world        # re-render from the local cache (no network)
meta-fetch status octocat/hello-world        # show watermark, phase, counts, budgets
```

Flags: `--out DIR`, `--db PATH`, `--reserve 10%|500`, `--cost-ceiling N`, `--max-wait 45m`, `--no-wait`.

The repo DB lives under `$XDG_DATA_HOME/github-repo-meta-fetch/{owner}/{repo}.sqlite3`; the shared per-token rate-limit DB under `$XDG_STATE_HOME/github-repo-meta-fetch/rate-limits.sqlite3`. On Windows these live under the native AppData locations instead of `$XDG_*`. The Markdown tree is pure output — safe to commit to git.
