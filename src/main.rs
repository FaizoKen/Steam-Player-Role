use std::sync::Arc;

use axum::routing::{delete, get, post};
use axum::Router;
use sqlx::PgPool;
use tokio::sync::mpsc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

mod config;
mod db;
mod error;
mod models;
mod routes;
mod schema;
mod services;
mod tasks;

use services::quota::{PublisherQuotas, QuotaGovernor};
use services::rolelogic::RoleLogicClient;
use services::steam_api::SteamApiClient;
use services::sync::{ConfigSyncEvent, PlayerSyncEvent};

pub struct AppState {
    pub pool: PgPool,
    pub config: config::AppConfig,
    pub player_sync_tx: mpsc::Sender<PlayerSyncEvent>,
    pub config_sync_tx: mpsc::Sender<ConfigSyncEvent>,
    pub steam_client: SteamApiClient,
    pub rl_client: RoleLogicClient,
    pub http: reqwest::Client,
    /// Central daily-quota budget + pacing for our Steam Web API key.
    pub quota: std::sync::Arc<QuotaGovernor>,
    /// Per-publisher-key governors for CheckAppOwnership calls.
    pub publisher_quotas: PublisherQuotas,
    pub verify_html: bytes::Bytes,
    pub players_html: bytes::Bytes,
    /// Origins permitted to issue cookie-authenticated state-changing
    /// requests (the per-handler `csrf::verify_origin` allowlist).
    pub allowed_origins: Vec<String>,
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "steam_player_role=info,tower_http=info".into()),
        )
        .init();

    let app_config = config::AppConfig::from_env();
    let listen_addr = app_config.listen_addr.clone();

    let pool = db::create_pool(&app_config.database_url).await;
    db::run_migrations(&pool).await;
    tracing::info!("Database connected and migrations applied");

    let (player_sync_tx, player_sync_rx) = mpsc::channel::<PlayerSyncEvent>(512);
    let (config_sync_tx, config_sync_rx) = mpsc::channel::<ConfigSyncEvent>(64);

    // Central quota governor — loads today's spend from the durable ledger so
    // a restart resumes accounting instead of resetting to zero.
    let quota = QuotaGovernor::new(
        pool.clone(),
        "main".to_string(),
        app_config.steam_api_daily_quota,
        app_config.quota_interactive_reserve,
        app_config.quota_safety_fraction,
    )
    .await;
    let publisher_quotas = PublisherQuotas::new(
        pool.clone(),
        app_config.steam_api_daily_quota,
        app_config.quota_safety_fraction,
    );

    let refresh_workers = app_config.refresh_workers;
    let steam_client = SteamApiClient::new(&app_config.steam_api_key, Arc::clone(&quota));
    let rl_client = RoleLogicClient::new();
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("Failed to build HTTP client");
    let verify_html = bytes::Bytes::from(routes::verification::render_verify_page(
        &app_config.base_url,
    ));
    let players_html =
        bytes::Bytes::from(routes::players::render_players_page(&app_config.base_url));

    // Origins that can drive cookie-authenticated state changes (direct-nav
    // admin writes). Iframe writes carry a Bearer token and skip this check.
    let mut allowed_origins = vec![config::derive_origin(&app_config.base_url)];
    if let Some(dash) = app_config.rl_dashboard_origin.as_deref() {
        allowed_origins.push(dash.to_string());
    }

    let state = Arc::new(AppState {
        pool,
        config: app_config,
        player_sync_tx,
        config_sync_tx,
        steam_client,
        rl_client,
        http,
        quota: Arc::clone(&quota),
        publisher_quotas,
        verify_html,
        players_html,
        allowed_origins,
    });

    // Persist the quota ledger on an interval.
    tokio::spawn(Arc::clone(&quota).run_flusher());

    // Background refresh workers, partitioned by hashtext(steam_id) % N so
    // they never double-process. They share the one governor's budget.
    for worker_id in 0..refresh_workers {
        tokio::spawn(tasks::refresh_worker::run(
            Arc::clone(&state),
            worker_id,
            refresh_workers,
        ));
    }
    tokio::spawn(tasks::player_sync_worker::run(
        player_sync_rx,
        Arc::clone(&state),
    ));
    tokio::spawn(tasks::config_sync_worker::run(
        config_sync_rx,
        Arc::clone(&state),
    ));
    tokio::spawn(tasks::cleanup_expired(Arc::clone(&state)));

    let app = Router::new()
        .nest(
            "/steam-player-role",
            Router::new()
                // Plugin endpoints (called by RoleLogic)
                .route("/register", post(routes::plugin::register))
                .route("/config", get(routes::plugin::get_config))
                .route("/config", post(routes::plugin::post_config))
                .route("/config", delete(routes::plugin::delete_config))
                // Iframe role-config (embedded by the RoleLogic dashboard)
                .route(
                    "/admin/{guild_id}/role/{role_id}",
                    get(routes::admin::role_config_page),
                )
                .route(
                    "/admin/{guild_id}/role/{role_id}/data",
                    get(routes::admin::role_config_data),
                )
                .route(
                    "/admin/{guild_id}/role/{role_id}/save",
                    post(routes::admin::role_config_save),
                )
                .route(
                    "/admin/{guild_id}/role/{role_id}/preview",
                    get(routes::admin::role_config_preview)
                        .post(routes::admin::role_config_preview_edit),
                )
                // Per-guild settings (players-list visibility)
                .route(
                    "/admin/{guild_id}/view-permission",
                    post(routes::admin::set_view_permission),
                )
                // Verification endpoints (user-facing)
                .route("/verify", get(routes::verification::verify_page))
                .route("/verify/login", get(routes::verification::login))
                .route("/verify/status", get(routes::verification::status))
                .route("/verify/steam", get(routes::verification::steam_login))
                .route("/verify/callback", get(routes::verification::callback))
                .route("/verify/unlink", post(routes::verification::unlink))
                .route("/verify/recheck", post(routes::verification::recheck))
                .route("/verify/logout", post(routes::verification::logout))
                // Player list (public)
                .route("/players/{guild_id}", get(routes::players::players_page))
                .route(
                    "/players/{guild_id}/data",
                    get(routes::players::players_data),
                )
                // Health & static
                .route("/favicon.ico", get(routes::health::favicon))
                .route("/dweeb/status", get(routes::dweeb::status))
                .route("/health", get(routes::health::health)),
        )
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state);

    tracing::info!("Server starting on {listen_addr}");

    let listener = tokio::net::TcpListener::bind(&listen_addr)
        .await
        .expect("Failed to bind listener");

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Shutdown signal received, draining connections...");
        })
        .await
        .expect("Server error");

    tracing::info!("Server stopped");
}
