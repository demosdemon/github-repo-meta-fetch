# Refresh the vendored GitHub GraphQL schema (run when GitHub adds fields we need).
update-schema:
    graphql-client introspect-schema \
      --output github-schema.json \
      --authorization "$(gh auth token)" \
      --header 'User-Agent: github-repo-meta-fetch/0.0 (https://github.com/demosdemon/github-repo-meta-fetch)' \
      'https://api.github.com/graphql'

# Format with nightly rustfmt (the .rustfmt.toml uses nightly-only options).
fmt:
    cargo +nightly fmt

# Lint with clippy, denying warnings.
lint:
    cargo clippy --all-targets --all-features -- -D warnings

# Run the full test suite.
test:
    cargo test
