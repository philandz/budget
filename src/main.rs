use axum::{routing::get, Json, Router};
use budget::handler::BudgetHandler;
use budget::manager::biz::BudgetBiz;
use budget::manager::client::IdentityClient;
use budget::manager::repository::BudgetRepository;
use budget::pb::service::budget::budget_service_server::BudgetServiceServer;
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

    // gRPC server
    let grpc_addr: SocketAddr = format!("{}:{}", config.grpc_host, config.grpc_port).parse()?;
    let grpc_server = tonic::transport::Server::builder()
        .add_service(BudgetServiceServer::new(grpc_handler))
        .serve(grpc_addr);
    tracing::info!("gRPC server listening on {}", grpc_addr);

    // HTTP server (health only — business routes served via gRPC through gateway)
    let http_addr: SocketAddr = format!("{}:{}", config.http_host, config.http_port).parse()?;
    let http_app = Router::new().route("/health", get(health_check));
    let http_listener = tokio::net::TcpListener::bind(http_addr).await?;
    tracing::info!("HTTP server listening on {}", http_addr);

    tokio::select! {
        res = grpc_server => {
            if let Err(e) = res { tracing::error!("gRPC server error: {}", e); }
        }
        res = axum::serve(http_listener, http_app) => {
            if let Err(e) = res { tracing::error!("HTTP server error: {}", e); }
        }
    }

    Ok(())
}

pub async fn health_check() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "service": "budget" }))
}
