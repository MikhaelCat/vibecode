//! Process entry point: environment → state → axum server → graceful shutdown.
use std::sync::Arc;

use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    travel_ai::obs::init();

    let cfg = Arc::new(travel_ai::config::Config::from_env()?);
    tracing::info!(
        bind = %cfg.bind,
        model = %cfg.model,
        mock_llm = !cfg.has_real_llm(),
        database = %cfg.database_url,
        "starting travel-ai"
    );

    let state = travel_ai::build_state(cfg.clone()).await?;
    let app = travel_ai::api::router(state);
    let listener = TcpListener::bind(cfg.bind).await?;

    tracing::info!(address = %listener.local_addr()?, "server is listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
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
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
