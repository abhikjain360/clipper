mod auth;
mod cleanup;
mod entity;
mod error;
mod migration;
mod rate_limit;
mod routes;
mod state;
mod ws;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Router, middleware,
    routing::{delete, get, post, put},
};
use clap::{Parser, Subcommand};
use tower_http::trace::TraceLayer;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::error::{ServerError, ServerResult};
use crate::rate_limit::RateLimiter;
use crate::state::AppState;

#[derive(Parser)]
#[command(name = "clipper-server")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize the server database
    Init {
        #[arg(long, short = 'd')]
        data_dir: PathBuf,
    },
    /// Run the server
    Serve {
        #[arg(long, short = 'd')]
        data_dir: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8787")]
        addr: String,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    if let Err(error) = run().await {
        error!(%error, "server failed");
        std::process::exit(error.exit_code());
    }
}

async fn run() -> ServerResult<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Init { data_dir } => init_server(data_dir).await?,
        Command::Serve { data_dir, addr } => serve(data_dir, addr).await?,
    }

    Ok(())
}

async fn init_server(data_dir: PathBuf) -> ServerResult<()> {
    let state = AppState::open(data_dir).await?;

    // Check if already initialized
    use sea_orm::EntityTrait;
    let existing = entity::server_config::Entity::find_by_id(1)
        .one(state.db())
        .await?;

    if existing.is_some() {
        info!("Server already initialized. To reinitialize, delete the database.");
        return Ok(());
    }

    use sea_orm::{ActiveModelTrait, Set};
    let now = chrono::Utc::now().to_rfc3339();

    let config = entity::server_config::ActiveModel {
        id: Set(1),
        created_at: Set(now.clone()),
        updated_at: Set(now),
    };
    config.insert(state.db()).await?;

    info!(data_dir = %state.data_dir().display(), "Server initialized successfully.");

    Ok(())
}

async fn serve(data_dir: PathBuf, addr: String) -> ServerResult<()> {
    let state = AppState::open(data_dir).await?;

    // Verify server is initialized
    use sea_orm::EntityTrait;
    entity::server_config::Entity::find_by_id(1)
        .one(state.db())
        .await?
        .ok_or(ServerError::NotInitialized)?;

    let limiter = Arc::new(RateLimiter::new());

    // private routes
    let authed = Router::new()
        .route("/api/auth/logout", post(routes::auth::logout))
        .route("/api/clipboard", post(routes::clipboard::upload))
        .route("/api/clipboard", get(routes::clipboard::list))
        .route("/api/files/init", post(routes::files::init_upload))
        .route("/api/files/{id}/blob", put(routes::files::upload_blob))
        .route(
            "/api/files/{id}/complete",
            post(routes::files::complete_upload),
        )
        .route("/api/files", get(routes::files::list_files))
        .route("/api/files/{id}/blob", get(routes::files::download_blob))
        .route("/api/files/{id}", delete(routes::files::delete_file))
        .route("/api/objects/init", post(routes::objects::init_object))
        .route(
            "/api/objects/{id}/payloads/{payload_id}",
            get(routes::objects::download_payload).put(routes::objects::upload_payload),
        )
        .route(
            "/api/objects/{id}/complete",
            post(routes::objects::complete_object),
        )
        .route("/api/objects/{id}", delete(routes::objects::delete_object))
        .route("/api/objects", get(routes::objects::list_objects))
        .route("/api/sync/bootstrap", get(routes::sync::bootstrap))
        .route("/api/ws", get(ws::ws_handler))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ));

    // public routes
    let app = Router::new()
        .route("/api/health", get(routes::health::health))
        .route(
            "/api/auth/register/start",
            post(routes::auth::register_start),
        )
        .route(
            "/api/auth/register/finish",
            post(routes::auth::register_finish),
        )
        .route("/api/auth/challenge", post(routes::auth::challenge))
        .route("/api/auth/login", post(routes::auth::login))
        .merge(authed)
        .layer(axum::Extension(limiter.clone()))
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    // cleanup task
    tokio::spawn(cleanup::run_cleanup_loop(state.clone()));

    // Spawn rate limiter pruning
    tokio::spawn({
        let limiter = limiter.clone();
        async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                limiter.prune();
            }
        }
    });

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("Listening on {}", addr);

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    Ok(())
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        error!(%error, "failed to install Ctrl+C handler");
        return;
    }
    info!("Shutdown signal received");
}
