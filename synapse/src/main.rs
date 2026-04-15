#![allow(clippy::must_use_candidate, clippy::missing_errors_doc)]

mod args;

use args::Args;
use clap::Parser;
use synapse_config::Config;
use synapse_server::Server;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Load configuration
    let config = Config::load(&args.config)?;

    // Initialize telemetry
    let _telemetry_guard = synapse_telemetry::init(config.telemetry.as_ref(), "info")?;

    tracing::info!(
        config_path = %args.config.display(),
        "starting synapse"
    );

    // Build server
    let server = Box::pin(Server::new(config)).await?;

    // Set up graceful shutdown
    let shutdown = CancellationToken::new();
    let shutdown_clone = shutdown.clone();

    tokio::spawn(async move {
        shutdown_signal().await;
        shutdown_clone.cancel();
    });

    // Run server
    server.serve(shutdown).await?;

    tracing::info!("synapse stopped");
    Ok(())
}

/// Wait for a shutdown signal (`SIGINT` or `SIGTERM`)
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }

    tracing::info!("shutdown signal received");
}
