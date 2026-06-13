mod auth;
mod cleanup;
mod config;
mod entity;
mod error;
mod migration;
mod rate_limit;
mod routes;
mod secret;
mod secret_storage;
mod state;
mod storage_quota;
mod ws;

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use axum::{
    Router,
    http::{Method, header},
    middleware,
    routing::{get, post},
};
use clap::{Parser, Subcommand};
use tower_http::{
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::{
    auth::hash_access_key,
    config::{ConfigOverrides, ServerConfig},
    entity::access_keys,
    error::{ServerError, ServerResult},
    rate_limit::TrustedProxies,
    secret::{ServerSecrets, generate_root_base64},
    state::AppState,
};

#[derive(Parser)]
#[command(name = "clipper-server")]
struct Cli {
    /// Path to a TOML config file. CLI flags override config file values.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize the server database
    Init {
        #[arg(long, short = 'd')]
        data_dir: Option<PathBuf>,
    },
    /// Add a one-time registration access key
    AddAccessKey {
        #[arg(long, short = 'd')]
        data_dir: Option<PathBuf>,
        #[arg(long, value_name = "KEY")]
        access_key: Option<String>,
        #[arg(long, value_name = "RFC3339")]
        expires_at: Option<String>,
    },
    /// Mint a fresh server pepper (base64, 32 bytes). Store it in
    /// CLIPPER_SERVER_SECRET or a file referenced by
    /// CLIPPER_SERVER_SECRET_FILE before running init/serve.
    GenerateSecret,
    /// Run the server
    Serve {
        #[command(flatten)]
        overrides: Box<ConfigOverrides>,
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
        Command::Init { data_dir } => {
            let mut overrides = ConfigOverrides::default();
            overrides.server.data_dir = data_dir;
            let config = load_config(cli.config.as_deref(), overrides)?;
            let secrets = ServerSecrets::load_from_env()?;
            init_server(config, secrets).await?;
        }
        Command::AddAccessKey {
            data_dir,
            access_key,
            expires_at,
        } => {
            let mut overrides = ConfigOverrides::default();
            overrides.server.data_dir = data_dir;
            let config = load_config(cli.config.as_deref(), overrides)?;
            let secrets = ServerSecrets::load_from_env()?;
            let access_key = read_access_key(access_key)?;
            add_access_key(config, secrets, &access_key, expires_at).await?;
        }
        Command::GenerateSecret => {
            // Plain stdout, single line — easy to pipe into an env file
            // or `systemd-creds encrypt`.
            println!("{}", generate_root_base64());
        }
        Command::Serve { overrides } => {
            let config = load_config(cli.config.as_deref(), *overrides)?;
            let secrets = ServerSecrets::load_from_env()?;
            serve(config, secrets).await?;
        }
    }

    Ok(())
}

fn load_config(path: Option<&Path>, cli_overrides: ConfigOverrides) -> ServerResult<ServerConfig> {
    let mut config = ServerConfig::default();

    if let Some(path) = path {
        let contents = std::fs::read_to_string(path)?;
        let file_overrides = toml::from_str::<ConfigOverrides>(&contents).map_err(|error| {
            ServerError::Config(format!(
                "failed to parse config `{}`: {error}",
                path.display()
            ))
        })?;
        config.apply_overrides(file_overrides);
    }

    config.apply_overrides(ConfigOverrides::from_env().map_err(ServerError::Config)?);
    config.apply_overrides(cli_overrides);
    config.validate_config().map_err(ServerError::Config)?;

    Ok(config)
}

async fn init_server(config: ServerConfig, secrets: ServerSecrets) -> ServerResult<()> {
    let access_key_hash_salt_bytes = config.crypto.access_key_hash_salt_bytes;
    let state = AppState::open(config, secrets).await?;

    // Check if already initialized
    use sea_orm::EntityTrait;
    let existing = entity::server_config::Entity::find_by_id(1)
        .one(state.db())
        .await?;

    if existing.is_some() {
        _ = load_access_key_hash_salt(&state).await?;
        info!("Server already initialized. To reinitialize, delete the database.");
        return Ok(());
    }

    use sea_orm::{ActiveModelTrait, Set};
    let now = chrono::Utc::now().to_rfc3339();

    let plaintext_salt = clipper_core::crypto::generate_random_bytes(access_key_hash_salt_bytes);
    let wrapped_salt = secret_storage::wrap_access_key_hash_salt(state.secrets(), &plaintext_salt)?;

    // One server-wide OPAQUE setup (oprf_seed ‖ sk_S ‖ fake_sk), generated once
    // and stored wrapped. It must stay stable: every user's password file and
    // export_key are bound to its oprf_seed.
    let opaque_server_setup = clipper_core::crypto::opaque_new_server_setup();
    let wrapped_opaque_server_setup =
        secret_storage::wrap_opaque_server_setup(state.secrets(), &opaque_server_setup)?;

    let config = entity::server_config::ActiveModel {
        id: Set(1),
        created_at: Set(now.clone()),
        updated_at: Set(now),
        access_key_hash_salt: Set(wrapped_salt),
        opaque_server_setup: Set(wrapped_opaque_server_setup),
    };
    config.insert(state.db()).await?;

    info!(data_dir = %state.data_dir().display(), "Server initialized successfully.");

    Ok(())
}

fn read_access_key(access_key: Option<String>) -> ServerResult<String> {
    let access_key = match access_key {
        Some(access_key) => access_key,
        None => rpassword::prompt_password("Access key: ")?,
    };
    if access_key.is_empty() {
        return Err(ServerError::Config("access key must not be empty".into()));
    }
    Ok(access_key)
}

async fn add_access_key(
    config: ServerConfig,
    secrets: ServerSecrets,
    access_key: &str,
    expires_at: Option<String>,
) -> ServerResult<()> {
    let expires_at = expires_at
        .map(|expires_at| {
            chrono::DateTime::parse_from_rfc3339(&expires_at)
                .map(|dt| dt.with_timezone(&chrono::Utc).to_rfc3339())
                .map_err(|error| ServerError::Config(format!("invalid expires-at: {error}")))
        })
        .transpose()?;

    use sea_orm::{ActiveModelTrait, Set};
    let access_key_hash_params = config.crypto.access_key_hash_params;
    let state = AppState::open(config, secrets).await?;
    let salt = load_access_key_hash_salt(&state).await?;
    let key_hash = hash_access_key(
        access_key,
        &salt,
        &state.secrets().access_key_pepper,
        &access_key_hash_params,
    )?;
    let now = chrono::Utc::now().to_rfc3339();

    access_keys::ActiveModel {
        key_hash: Set(key_hash),
        created_at: Set(now),
        expires_at: Set(expires_at),
        used_at: Set(None),
        used_by_user_id: Set(None),
    }
    .insert(state.db())
    .await?;

    info!("Access key added.");

    Ok(())
}

async fn serve(config: ServerConfig, secrets: ServerSecrets) -> ServerResult<()> {
    let state = AppState::open(config, secrets).await?;
    let addr = state.config().server.addr.clone();
    let trusted_proxies = TrustedProxies::new(state.config().server.trusted_proxies.clone());
    let rate_limit_prune_interval_secs = state.config().rate_limit.prune_interval_secs;

    // Verify the server is initialized and this process has the matching
    // pepper before accepting traffic.
    _ = load_access_key_hash_salt(&state).await?;

    if !trusted_proxies.is_empty() {
        info!(
            trusted_proxy_count = trusted_proxies.len(),
            "Trusting proxy forwarded client IP headers"
        );
    }
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            Method::DELETE,
            Method::GET,
            Method::OPTIONS,
            Method::POST,
            Method::PUT,
        ])
        .allow_headers([header::ACCEPT, header::AUTHORIZATION, header::CONTENT_TYPE]);

    // private routes
    let authed = Router::new()
        .route("/api/auth/logout", post(routes::auth::logout))
        .route("/api/ws-ticket", post(ws::mint_ws_ticket))
        .route("/api/objects/init", post(routes::objects::init_object))
        .route(
            "/api/objects/{id}/payloads/{payload_id}",
            get(routes::objects::download_payload).put(routes::objects::upload_payload),
        )
        .route(
            "/api/objects/{id}/complete",
            post(routes::objects::complete_object),
        )
        .route(
            "/api/objects/{id}",
            get(routes::objects::get_object).delete(routes::objects::delete_object),
        )
        .route("/api/objects", get(routes::objects::list_objects))
        .route("/api/ws", get(ws::ws_handler))
        // Layer order (outermost first): per-client limit before token
        // validation bounds invalid-token database churn; the per-user limit
        // needs the AuthInfo extension, so it sits inside auth.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            rate_limit::user_rate_limit_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            rate_limit::api_rate_limit_middleware,
        ));

    // public auth routes share the same limiter at the router layer.
    let public_auth = Router::new()
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
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            rate_limit::auth_rate_limit_middleware,
        ));

    // public routes
    let app = Router::new()
        .route("/api/health", get(routes::health::health))
        .route("/api/ws-ticket/connect", get(ws::ws_ticket_handler))
        .merge(public_auth)
        .merge(authed)
        .layer(axum::Extension(trusted_proxies))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    // cleanup task
    tokio::spawn(cleanup::run_cleanup_loop(state.clone()));

    // rate limiter pruning
    tokio::spawn({
        let state = state.clone();
        async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                rate_limit_prune_interval_secs,
            ));
            loop {
                interval.tick().await;
                state.rate_limiter().prune();
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

async fn load_access_key_hash_salt(state: &AppState) -> ServerResult<Vec<u8>> {
    use sea_orm::EntityTrait;

    let server_config = entity::server_config::Entity::find_by_id(1)
        .one(state.db())
        .await?
        .ok_or(ServerError::NotInitialized)?;
    secret_storage::unwrap_access_key_hash_salt(
        state.secrets(),
        &server_config.access_key_hash_salt,
    )
    .map_err(|_| {
        ServerError::Config(
            "server secret cannot decrypt existing server configuration; check CLIPPER_SERVER_SECRET"
                .into(),
        )
    })
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        error!(%error, "failed to install Ctrl+C handler");
        return;
    }
    info!("Shutdown signal received");
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use sea_orm::{ActiveModelTrait, Set};

    use super::*;

    #[tokio::test]
    async fn load_access_key_hash_salt_rejects_wrong_secret() {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let mut config = ServerConfig::default();
        config.server.data_dir = data_dir.path().to_path_buf();
        let correct_secrets =
            ServerSecrets::from_root(&[0x11_u8; clipper_core::crypto::SERVER_SECRET_BYTES]);
        let state = AppState::open(config.clone(), correct_secrets)
            .await
            .expect("state");

        let now = Utc::now().to_rfc3339();
        let plaintext_salt = clipper_core::crypto::generate_access_key_hash_salt();
        let wrapped_salt =
            secret_storage::wrap_access_key_hash_salt(state.secrets(), &plaintext_salt)
                .expect("wrap salt");
        let wrapped_opaque_server_setup = secret_storage::wrap_opaque_server_setup(
            state.secrets(),
            &clipper_core::crypto::opaque_new_server_setup(),
        )
        .expect("wrap opaque_server_setup");
        entity::server_config::ActiveModel {
            id: Set(1),
            created_at: Set(now.clone()),
            updated_at: Set(now),
            access_key_hash_salt: Set(wrapped_salt),
            opaque_server_setup: Set(wrapped_opaque_server_setup),
        }
        .insert(state.db())
        .await
        .expect("insert server config");

        assert_eq!(
            load_access_key_hash_salt(&state).await.expect("load salt"),
            plaintext_salt
        );

        let wrong_secrets =
            ServerSecrets::from_root(&[0x22_u8; clipper_core::crypto::SERVER_SECRET_BYTES]);
        let wrong_state =
            AppState::open_with_db_and_config(state.db().clone(), config, wrong_secrets)
                .await
                .expect("wrong state");

        let error = load_access_key_hash_salt(&wrong_state)
            .await
            .expect_err("wrong secret should fail");
        assert!(matches!(error, ServerError::Config(message) if message.contains("server secret")));
    }
}
