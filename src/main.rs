use std::process::ExitCode;

use clap::Parser;
use github_repo_meta_fetch::cli;
use github_repo_meta_fetch::cli::Cli;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let env_filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(tracing::Level::INFO.into())
        .from_env_lossy();
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    match cli::run(cli).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(1)
        }
    }
}
