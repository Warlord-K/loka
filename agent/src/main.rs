use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{error, info, warn};

mod acp;
mod api;
mod apps;
mod browser;
mod browser_view;
mod screen;
mod claude;
mod cli;
mod codex;
mod config;
mod cursor;
mod db;
mod git;
mod grok;
mod cmd_hub;
mod key_proxy;
mod mcp_hub;
mod model_catalog;
mod omp;
mod pr_actions;
mod opencode;
mod preflight;
mod quest_storage;
mod remote_runner;
mod resources;
mod sandbox_wrap;
mod tools_check;
mod setup_scan;
mod shell;
mod terminal;
mod world;
#[cfg(test)]
mod bench_live;
#[cfg(test)]
mod test_env;
#[cfg(test)]
fn passed_on_to_clis(name: &str) -> bool {
    cli::passed_on(name)
}
#[cfg(test)]
mod parity_live;

fn main() -> anyhow::Result<()> {
    if cmd_hub::is_shell_invocation() {
        cmd_hub::run_shell();
    }
    loka_shared::limits::hide_process_memory();
    cli::capture_host_secrets();
    match std::env::args().nth(1).as_deref() {
        Some("browser") => {
            let args: Vec<String> = std::env::args().skip(2).collect();
            std::process::exit(browser::gate(&args));
        }
        Some("apps") => {
            let args: Vec<String> = std::env::args().skip(2).collect();
            std::process::exit(apps::gate(&args));
        }
        Some("screen") => {
            let args: Vec<String> = std::env::args().skip(2).collect();
            std::process::exit(screen::gate(&args));
        }
        Some("mcp-shim") => {
            let args: Vec<String> = std::env::args().skip(2).collect();
            mcp_hub::shim::run(&args)
        }
        Some("mcp-launch") => {
            let spec = std::env::args().nth(2).ok_or_else(|| anyhow::anyhow!("missing MCP launch specification"))?;
            codex::launch_mcp(&spec)
        }
        Some("mcp") => tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
            if std::env::args().nth(2).as_deref() == Some("runner") {
                return remote_runner::serve_mcp().await;
            }
            dotenvy::dotenv().ok();
            loka_shared::environment::initialize();
            world::mcp::serve().await
        }),
        _ => tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(serve()),
    }
}

async fn serve() -> anyhow::Result<()> {
    let service = if std::env::args().nth(1).as_deref() == Some("runner") { "runner" } else { "world" };
    {
        use tracing_subscriber::prelude::*;
        tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::from_default_env().add_directive("loka_agent=info".parse()?))
            .with(tracing_subscriber::fmt::layer().json())
            .with(loka_shared::telemetry::log_shipper(service))
            .init();
    }
    loka_shared::telemetry::start(service);
    loka_shared::limits::raise_open_files();

    if service == "runner" {
        return remote_runner::run().await;
    }

    dotenvy::dotenv().ok();
    loka_shared::environment::initialize();

    let config = config::Config::from_env()?;
    let backend_names: Vec<&str> = config.backends.keys().map(|s| s.as_str()).collect();
    info!(repos = config.repos.len(), backends = ?backend_names, "Loka world worker starting");

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_notify = Arc::new(tokio::sync::Notify::new());
    let (api_shutdown_tx, api_shutdown_rx) = tokio::sync::watch::channel(());

    tokio::fs::create_dir_all(&config.repos_dir).await?;
    tokio::fs::create_dir_all(&config.workspaces_dir).await?;

    if config.github_token.is_empty() {
        warn!("GITHUB_TOKEN is not set, GitHub features are disabled");
    }
    git::workspace::configure_git_auth(&config.github_token, &git::workspace::GitHost::INHERITED).await?;

    git::workspace::configure_git_signing(config.signing.as_ref(), &git::workspace::GitHost::INHERITED).await?;

    if let Err(e) = config.write_opencode_global_config() {
        warn!(error = %e, "Failed to write opencode MCP config");
    }

    preflight::check_backends(&config).await;

    let db = db::Database::connect(&config.database_url).await?;
    info!("Connected to Postgres");

    for repo in &config.repos {
        info!(repo = %repo.repo, "Ensuring repo is cloned");
        match git::workspace::ensure_cloned(&config.repos_dir, &repo.repo, &config.github_token).await {
            Ok(_) => info!(repo = %repo.repo, "Repo ready"),
            Err(e) => error!(repo = %repo.repo, error = %e, "Failed to clone repo"),
        }
    }

    let api_handle = tokio::spawn(api::serve(api_shutdown_rx));

    let sig_shutdown = shutdown.clone();
    let sig_notify = shutdown_notify.clone();
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("failed to register SIGTERM handler");
            tokio::select! {
                _ = ctrl_c => info!("Received SIGINT"),
                _ = sigterm.recv() => info!("Received SIGTERM"),
            }
        }
        #[cfg(not(unix))]
        {
            ctrl_c.await.ok();
            info!("Received SIGINT");
        }
        sig_shutdown.store(true, Ordering::SeqCst);
        sig_notify.notify_waiters();
        let _ = api_shutdown_tx.send(());
        info!("Shutdown signal received, draining current task...");
    });

    world::run(config, db, shutdown, shutdown_notify).await?;
    let _ = api_handle.await;
    Ok(())
}
