mod auth;
mod cleanup;
mod entity;
mod migration;
mod rate_limit;
mod routes;
mod state;
mod ws;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    Router, middleware,
    routing::{delete, get, post, put},
};
use clap::{Parser, Subcommand};
use sea_orm::{Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::EnvFilter;

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
    /// Initialize the server with a passphrase
    Init {
        #[arg(long)]
        data_dir: PathBuf,
    },
    /// Run the server
    Serve {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8787")]
        addr: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Init { data_dir } => init_server(data_dir).await?,
        Command::Serve { data_dir, addr } => serve(data_dir, addr).await?,
    }

    Ok(())
}

async fn init_server(data_dir: PathBuf) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(&data_dir).await?;

    let db = connect_db(&data_dir).await?;
    migration::Migrator::up(&db, None).await?;

    // Check if already initialized
    use sea_orm::EntityTrait;
    let existing = entity::server_config::Entity::find_by_id(1)
        .one(&db)
        .await?;

    if existing.is_some() {
        println!("Server already initialized. To reinitialize, delete the database.");
        return Ok(());
    }

    // Read passphrase
    let passphrase = if let Ok(p) = std::env::var("CLIPPER_PASSPHRASE") {
        p
    } else {
        rpassword::prompt_password("Enter passphrase: ")?
    };

    if passphrase.is_empty() {
        anyhow::bail!("Passphrase cannot be empty");
    }

    let params = clipper_core::crypto::Argon2Params::default();
    let auth_salt = clipper_core::crypto::generate_salt();
    let enc_salt = clipper_core::crypto::generate_salt();
    let auth_hash =
        clipper_core::crypto::compute_auth_hash(passphrase.as_bytes(), &auth_salt, &params)?;

    use sea_orm::{ActiveModelTrait, Set};
    let now = chrono::Utc::now().to_rfc3339();

    let config = entity::server_config::ActiveModel {
        id: Set(1),
        auth_salt: Set(auth_salt.to_vec()),
        auth_hash: Set(auth_hash.to_vec()),
        enc_salt: Set(enc_salt.to_vec()),
        created_at: Set(now.clone()),
        updated_at: Set(now),
    };
    config.insert(&db).await?;

    // Create data subdirectories
    tokio::fs::create_dir_all(data_dir.join("clipboard")).await?;
    tokio::fs::create_dir_all(data_dir.join("files")).await?;

    println!("Server initialized successfully.");
    println!("Data directory: {}", data_dir.display());

    Ok(())
}

async fn serve(data_dir: PathBuf, addr: String) -> anyhow::Result<()> {
    let db = connect_db(&data_dir).await?;
    migration::Migrator::up(&db, None).await?;

    // Verify server is initialized
    use sea_orm::EntityTrait;
    entity::server_config::Entity::find_by_id(1)
        .one(&db)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("Server not initialized. Run `clipper-server init` first.")
        })?;

    tokio::fs::create_dir_all(data_dir.join("clipboard")).await?;
    tokio::fs::create_dir_all(data_dir.join("files")).await?;

    let state = AppState::new(db, data_dir);
    let limiter = Arc::new(RateLimiter::new());

    // Routes that require auth
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
        .route("/api/sync/bootstrap", get(routes::sync::bootstrap))
        .route("/api/ws", get(ws::ws_handler))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ));

    // Public routes
    let app = Router::new()
        .route("/api/health", get(routes::health::health))
        .route("/api/auth/challenge", post(routes::auth::challenge))
        .route("/api/auth/login", post(routes::auth::login))
        .merge(authed)
        .layer(axum::Extension(limiter.clone()))
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    // Spawn cleanup task
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

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn connect_db(data_dir: impl AsRef<Path>) -> anyhow::Result<DatabaseConnection> {
    let db_path = data_dir.as_ref().join("clipper.db");
    let url = format!("sqlite:{}?mode=rwc", db_path.display());
    let db = Database::connect(&url).await?;
    Ok(db)
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to install Ctrl+C handler");
    info!("Shutdown signal received");
}
