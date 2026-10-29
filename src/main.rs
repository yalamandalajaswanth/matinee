use std::net::SocketAddr;

use anyhow::{Context, Result};
use clap::Parser;
use deovr_server::app::build_router;
use deovr_server::config::Cli;
use deovr_server::db::Db;
use deovr_server::state::AppState;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Cli::parse().into_config()?;
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("cannot create data dir {}", config.data_dir.display()))?;
    let db = Db::open(&config.data_dir.join("library.db")).context("cannot open library.db")?;

    let state = AppState::new(config.libraries.clone(), db);
    let count = state.rescan().await.context("initial scan failed")?;
    tracing::info!(
        "deovr-server v{} - {count} videos in {} libraries",
        env!("CARGO_PKG_VERSION"),
        state.library_count()
    );

    let addr = SocketAddr::new(config.bind, config.port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("cannot listen on {addr} (is the port already in use?)"))?;
    tracing::info!("listening on http://{}", listener.local_addr()?);

    axum::serve(
        listener,
        build_router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("server error")?;
    Ok(())
}
