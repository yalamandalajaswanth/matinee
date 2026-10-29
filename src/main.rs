use std::io::ErrorKind;
use std::net::SocketAddr;

use anyhow::{Context, Result};
use clap::Parser;
use matinee::app::build_router;
use matinee::config::Cli;
use matinee::state::AppState;
use tracing_subscriber::EnvFilter;

/// Show usage and wait, so a double-clicked console window does not vanish.
#[cfg(windows)]
fn bare_launch_help() -> bool {
    use clap::CommandFactory;
    if !matinee::config::is_bare_launch(std::env::args_os()) {
        return false;
    }
    println!("{}", Cli::command().render_help());
    println!(
        r#"Example:
  matinee.exe --library "D:\Movies"
"#
    );
    println!("Press Enter to exit");
    let _ = std::io::stdin().read_line(&mut String::new());
    true
}

#[cfg(not(windows))]
fn bare_launch_help() -> bool {
    false
}

#[tokio::main]
async fn main() -> Result<()> {
    if bare_launch_help() {
        return Ok(());
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Cli::parse().into_config()?;
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("cannot create data dir {}", config.data_dir.display()))?;
    let db_path = config.data_dir.join("library.db");

    let state =
        AppState::open(config.libraries.clone(), &db_path).context("cannot open library.db")?;
    let count = state.rescan().await.context("initial scan failed")?;

    let addr = SocketAddr::new(config.bind, config.port);
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == ErrorKind::AddrInUse => {
            eprintln!(
                "Port {} is already in use. Is matinee already running? \
                 Use --port to pick another.",
                config.port
            );
            std::process::exit(1);
        }
        Err(e) => return Err(e).with_context(|| format!("cannot listen on {addr}")),
    };

    matinee::banner::print(
        count,
        state.library_count(),
        config.bind,
        listener.local_addr()?.port(),
        !config.no_qr,
    );

    axum::serve(
        listener,
        build_router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("server error")?;
    Ok(())
}
