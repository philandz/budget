use axum::{routing::get, Json, Router};
use budget::handler::portfolio::PortfolioHandler;
use budget::handler::BudgetHandler;
use budget::manager::biz::portfolio::biz::PortfolioBiz;
use budget::manager::biz::portfolio::refresh::RefreshJob;
use budget::manager::biz::BudgetBiz;
use budget::manager::client::IdentityClient;
use budget::manager::repository::portfolio::PortfolioRepository;
use budget::manager::repository::BudgetRepository;
use budget::pb::service::budget::budget_service_server::BudgetServiceServer;
use budget::pb::service::portfolio::portfolio_service_server::PortfolioServiceServer;
use std::{net::SocketAddr, sync::Arc};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let rust_log = std::env::var("RUST_LOG").ok();
    philand_logging::init(
        "budget",
        rust_log
            .as_deref()
            .or(Some("budget=debug,tower_http=debug")),
    );

    let app_info = philand_application::from_env_with_prefix("BUDGET_APP");
    tracing::info!("starting {}", app_info.user_agent());

    let config = philand_configs::BudgetServiceConfig::from_env()
        .map_err(|e| anyhow::anyhow!("Failed to load config: {e}"))?;
    tracing::info!(
        "Config loaded: gRPC={}, HTTP={}",
        config.grpc_port,
        config.http_port
    );

    let repo = BudgetRepository::new(&config)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to init repository: {e}"))?;
    tracing::info!("Storage initialized");

    if let Err(e) = config.register_consul().await {
        tracing::warn!("Consul registration failed: {e}. Continuing without Consul.");
    }

    let identity_url =
        std::env::var("IDENTITY_GRPC_URL").unwrap_or_else(|_| "http://127.0.0.1:50101".to_string());
    let identity_client = IdentityClient::connect(&identity_url)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to identity gRPC: {e}"))?;
    tracing::info!("Identity gRPC client connected to {}", identity_url);

    let biz = Arc::new(BudgetBiz::new(repo, config.clone(), identity_client));
    let grpc_handler = BudgetHandler::new(biz.clone());

    // Wire Portfolio service in front of the same Budget repository pool.
    // The identity client and BudgetBiz are shared so role resolution uses
    // the same logic as the existing service.
    let portfolio_repo = Arc::new(PortfolioRepository::new(
        sqlx::MySqlPool::connect(&config.database_url)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to open portfolio pool: {e}"))?,
    ));
    let portfolio_biz = Arc::new(PortfolioBiz::new(
        (*portfolio_repo).clone(),
        IdentityClient::connect(&identity_url)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to identity gRPC (portfolio): {e}"))?,
        biz.clone(),
    ));
    let portfolio_handler = PortfolioHandler::new(portfolio_biz.clone());

    // Reuse the same pool for the scheduled refresh job.
    let portfolio_refresh_repo = portfolio_repo.clone();

    // gRPC server
    let grpc_addr: SocketAddr = format!("{}:{}", config.grpc_host, config.grpc_port).parse()?;
    let grpc_server = tonic::transport::Server::builder()
        .add_service(BudgetServiceServer::new(grpc_handler))
        .add_service(PortfolioServiceServer::new(portfolio_handler))
        .serve(grpc_addr);
    tracing::info!("gRPC server listening on {}", grpc_addr);

    // HTTP server (health only — business routes served via gRPC through gateway)
    let http_addr: SocketAddr = format!("{}:{}", config.http_host, config.http_port).parse()?;
    let http_app = Router::new().route("/health", get(health_check));
    let http_listener = tokio::net::TcpListener::bind(http_addr).await?;
    tracing::info!("HTTP server listening on {}", http_addr);

    // Scheduled price refresh job. Reads its interval from
    // PORTFOLIO_REFRESH_INTERVAL_SECS. Provider flags (e.g.
    // PORTFOLIO_ENABLE_SJC) are evaluated once at startup.
    let refresh = RefreshJob::new(portfolio_refresh_repo.clone());
    tracing::info!(
        "Portfolio refresh job scheduled (interval = {}s)",
        refresh.interval_secs
    );
    let refresh_handle = tokio::spawn(refresh.run());

    // Scheduled maturity scan. Reads its interval from
    // PORTFOLIO_MATURITY_SCAN_INTERVAL_SECS (default 3600s = hourly).
    let maturity_biz = Arc::clone(&portfolio_biz);
    let maturity_interval = std::env::var("PORTFOLIO_MATURITY_SCAN_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(3600);
    tracing::info!(
        "Portfolio maturity scan scheduled (interval = {}s)",
        maturity_interval
    );
    let maturity_handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(maturity_interval));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await; // skip first immediate tick
        loop {
            ticker.tick().await;
            match maturity_biz.run_maturity_scan().await {
                Ok(n) if n > 0 => {
                    tracing::info!("maturity scan transitioned {n} assets to MATURED");
                }
                Ok(_) => tracing::debug!("maturity scan: no due fixed deposits"),
                Err(e) => tracing::warn!("maturity scan failed: {e}"),
            }
        }
    });

    tokio::select! {
        res = grpc_server => {
            if let Err(e) = res { tracing::error!("gRPC server error: {}", e); }
        }
        res = axum::serve(http_listener, http_app) => {
            if let Err(e) = res { tracing::error!("HTTP server error: {}", e); }
        }
        res = refresh_handle => {
            if let Err(e) = res { tracing::error!("refresh job error: {}", e); }
        }
        res = maturity_handle => {
            if let Err(e) = res { tracing::error!("maturity job error: {}", e); }
        }
    }

    Ok(())
}

pub async fn health_check() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "service": "budget" }))
}
