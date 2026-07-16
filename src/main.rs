#[macro_use]
extern crate rocket;

mod app_state;
mod auth;
mod cache;
mod catchers;
mod cli;
mod config;
mod db;
mod db_integrity;
mod denomination;
mod erc4626;
mod error;
mod fairings;
mod raindex;
mod registry_artifact;
mod routes;
mod sync_status;
mod telemetry;
mod types;
mod wrap_ratio;

pub(crate) const CHAIN_ID: u32 = 8453;

#[cfg(test)]
mod test_helpers;

use clap::Parser;
use rocket::fs::{FileServer, Options};
use rocket_cors::{AllowedHeaders, AllowedMethods, AllowedOrigins, CorsOptions};
use std::collections::HashSet;
use std::path::PathBuf;
use utoipa::openapi::security::{Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi};
use utoipa_swagger_ui::SwaggerUi;

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            let mut scheme = Http::new(HttpAuthScheme::Basic);
            scheme.description = Some(
                "Use your API key as the username and API secret as the password.".to_string(),
            );
            components.add_security_scheme("basicAuth", SecurityScheme::Http(scheme));
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum StartupError {
    #[error("invalid HTTP method in CORS config: {0}")]
    InvalidMethod(String),
    #[error("CORS configuration failed: {0}")]
    Cors(#[from] rocket_cors::Error),
}

#[derive(Debug, thiserror::Error)]
enum StartupRegistryError {
    #[error("failed to read private registry artifact")]
    PrivateArtifactRead(#[source] registry_artifact::RegistryArtifactStoreError),
    #[error("failed to query private registry history")]
    PrivateRegistryHistory(#[source] sqlx::Error),
    #[error("configured registry_url is empty")]
    MissingConfiguredRegistry,
    #[error("private registry artifact does not match latest successful history")]
    PrivateRegistryMismatch,
    #[error("private registry artifact exists but no successful history row was found")]
    PrivateArtifactWithoutHistory,
    #[error("private registry history exists but artifact file is missing")]
    HistoryWithoutPrivateArtifact,
    #[error("failed to load private registry")]
    PrivateRegistryLoad(#[source] raindex::RaindexProviderError),
    #[error("failed to load configured registry")]
    ConfiguredRegistryLoad(#[source] raindex::RaindexProviderError),
}

#[derive(OpenApi)]
#[openapi(
    paths(
        routes::health::get_health,
        routes::health::get_health_detailed,
        routes::sync_status::get_sync_status,
        routes::tokens::get_tokens,
        routes::tokens::get_wrap_ratios,
        routes::tokens::get_wrap_ratio_by_address,
        routes::tokens::get_wrap_ratio_history_by_address,
        routes::tokens::get_token_details,
        routes::tokens::get_token_details_by_address,
        routes::tokens::get_token_proofs,
        routes::swap::post_swap_quote,
        routes::swap::post_swap_calldata,
        routes::swap::post_swap_calldata_v2,
        routes::order::post_order_dca,
        routes::order::post_order_solver,
        routes::order::get_order,
        routes::order::post_order_cancel,
        routes::orders::get_orders_by_tx,
        routes::orders::get_orders_by_address,
        routes::orders::get_orders_by_token,
        routes::vaults::get_vaults,
        routes::vaults::get_vault_totals,
        routes::admin::put_registry,
        routes::trades::get_by_tx::get_trades_by_tx,
        routes::trades::get_by_order_hashes::get_trades_by_order_hashes,
        routes::trades::get_by_token::get_trades_by_token,
        routes::trades::get_by_taker::get_trades_by_taker,
        routes::trades::get_by_address::get_trades_by_address,
        routes::registry::get_registry,
        routes::registry::get_registry_history,
    ),
    components(),
    modifiers(&SecurityAddon),
    tags(
        (name = "Health", description = "Health check endpoints"),
        (name = "Tokens", description = "Token information endpoints"),
        (name = "Swap", description = "Swap quote and calldata endpoints"),
        (name = "Order", description = "Order deployment and management endpoints"),
        (name = "Orders", description = "Order listing and query endpoints"),
        (name = "Vaults", description = "Orderbook vault position and total endpoints"),
        (name = "Admin", description = "Administrative endpoints"),
        (name = "Trades", description = "Trade listing and query endpoints"),
        (name = "Registry", description = "Registry information endpoints"),
    ),
    info(
        title = "Albion REST API",
        version = "0.1.0",
        description = "REST API for Albion orderbook operations",
    )
)]
struct ApiDoc;

fn configure_cors() -> Result<rocket_cors::Cors, StartupError> {
    let allowed_methods: AllowedMethods = ["Get", "Post", "Put", "Options"]
        .iter()
        .map(|s| {
            std::str::FromStr::from_str(s).map_err(|_| StartupError::InvalidMethod(s.to_string()))
        })
        .collect::<Result<_, _>>()?;

    Ok(CorsOptions {
        allowed_origins: AllowedOrigins::all(),
        allowed_methods,
        allowed_headers: AllowedHeaders::all(),
        allow_credentials: false,
        expose_headers: HashSet::from([
            "X-Request-Id".to_string(),
            "Retry-After".to_string(),
            "X-RateLimit-Limit".to_string(),
            "X-RateLimit-Remaining".to_string(),
            "X-RateLimit-Reset".to_string(),
        ]),
        ..Default::default()
    }
    .to_cors()?)
}

pub(crate) fn rocket(
    pool: db::DbPool,
    rate_limiter: fairings::RateLimiter,
    raindex_config: raindex::SharedRaindexProvider,
    app_state: app_state::ApplicationState,
    docs_dir: String,
    usage_log_max_concurrency: usize,
    sync_status_fetcher: Option<sync_status::SyncStatusFetcher>,
) -> Result<rocket::Rocket<rocket::Build>, StartupError> {
    let cors = configure_cors()?;

    let figment = rocket::Config::figment().merge((rocket::Config::LOG_LEVEL, "normal"));

    let options = Options::Index | Options::NormalizeDirs;

    Ok(rocket::custom(figment)
        .manage(pool)
        .manage(rate_limiter)
        .manage(raindex_config)
        .manage(app_state)
        .manage(sync_status_fetcher)
        .mount("/", routes::health::routes())
        .mount("/", routes::sync_status::routes())
        .mount("/v1/tokens", routes::tokens::routes())
        .mount("/v1/swap", routes::swap::routes())
        .mount("/v2/swap", routes::swap::routes_v2())
        .mount("/v1/order", routes::order::routes())
        .mount("/v1/orders", routes::orders::routes())
        .mount("/v1/vaults", routes::vaults::routes())
        .mount("/v1/trades", routes::trades::routes())
        .mount("/", routes::registry::routes())
        .mount("/admin", routes::admin::routes())
        .mount("/docs", FileServer::new(docs_dir, options))
        .mount(
            "/",
            SwaggerUi::new("/swagger/<tail..>").url("/api-doc/openapi.json", ApiDoc::openapi()),
        )
        .register("/", catchers::catchers())
        .attach(fairings::RequestLogger)
        .attach(fairings::UsageLogger::new(usage_log_max_concurrency))
        .attach(fairings::RateLimitHeadersFairing)
        .attach(cors))
}

async fn load_configured_raindex(
    cfg: &config::Config,
    local_db_path: PathBuf,
    additional_rpcs: std::collections::HashMap<String, Vec<String>>,
) -> Result<raindex::RaindexProvider, StartupRegistryError> {
    if cfg.registry_url.is_empty() {
        return Err(StartupRegistryError::MissingConfiguredRegistry);
    }

    tracing::info!("loading raindex registry from config");
    raindex::RaindexProvider::load(&cfg.registry_url, Some(local_db_path), additional_rpcs)
        .await
        .map_err(StartupRegistryError::ConfiguredRegistryLoad)
}

async fn load_startup_raindex(
    cfg: &config::Config,
    pool: &db::DbPool,
    registry_artifact_store: &registry_artifact::RegistryArtifactStore,
    local_db_path: PathBuf,
    additional_rpcs: std::collections::HashMap<String, Vec<String>>,
) -> Result<raindex::RaindexProvider, StartupRegistryError> {
    let private_registry_artifact = registry_artifact_store
        .load()
        .await
        .map_err(StartupRegistryError::PrivateArtifactRead)?;

    let latest_private_registry = db::registry_history::latest_successful_private_registry(pool)
        .await
        .map_err(StartupRegistryError::PrivateRegistryHistory)?;

    let private_registry_source = match (
        private_registry_artifact.filter(|artifact| !artifact.is_empty()),
        latest_private_registry,
    ) {
        (Some(artifact), Some(row)) => {
            let payload_sha256 = registry_artifact::artifact_sha256(&artifact);
            if row.payload_sha256 != payload_sha256 {
                tracing::error!(
                    expected_payload_sha256 = %row.payload_sha256,
                    actual_payload_sha256 = %payload_sha256,
                    path = %registry_artifact_store.path().display(),
                    "private registry artifact does not match latest successful history"
                );
                if cfg.allow_registry_fallback {
                    None
                } else {
                    return Err(StartupRegistryError::PrivateRegistryMismatch);
                }
            } else {
                Some(artifact)
            }
        }
        (Some(artifact), None) => {
            let payload_sha256 = registry_artifact::artifact_sha256(&artifact);
            tracing::error!(
                payload_sha256 = %payload_sha256,
                path = %registry_artifact_store.path().display(),
                "private registry artifact exists but no successful history row was found"
            );
            if cfg.allow_registry_fallback {
                None
            } else {
                return Err(StartupRegistryError::PrivateArtifactWithoutHistory);
            }
        }
        (None, Some(_)) => {
            tracing::error!(
                path = %registry_artifact_store.path().display(),
                "private registry history exists but artifact file is missing"
            );
            if cfg.allow_registry_fallback {
                None
            } else {
                return Err(StartupRegistryError::HistoryWithoutPrivateArtifact);
            }
        }
        (None, None) => None,
    };

    if let Some(private_registry_source) = private_registry_source {
        tracing::info!(
            path = %registry_artifact_store.path().display(),
            "loading private registry artifact from file"
        );
        match raindex::RaindexProvider::load(
            &private_registry_source,
            Some(local_db_path.clone()),
            additional_rpcs.clone(),
        )
        .await
        {
            Ok(provider) => {
                tracing::info!("loaded private raindex registry");
                return Ok(provider);
            }
            Err(e) if cfg.allow_registry_fallback => {
                tracing::error!(
                    error = %e.safe_summary(),
                    path = %registry_artifact_store.path().display(),
                    "failed to load private registry artifact; falling back to configured registry"
                );
            }
            Err(e) => return Err(StartupRegistryError::PrivateRegistryLoad(e)),
        }
    }

    load_configured_raindex(cfg, local_db_path, additional_rpcs).await
}

#[rocket::main]
async fn main() {
    let parsed = cli::Cli::parse();

    let command = match parsed.command {
        Some(cmd) => cmd,
        None => {
            cli::print_usage();
            return;
        }
    };

    let config_path = match &command {
        cli::Command::Serve { config } | cli::Command::Keys { config, .. } => config.clone(),
    };

    let cfg = match config::Config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to load config from {}: {e}", config_path.display());
            std::process::exit(1);
        }
    };

    let log_guard = match telemetry::init(&cfg.log_dir) {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("failed to initialize telemetry: {e}");
            std::process::exit(1);
        }
    };

    let pool = match db::init(&cfg.database_url, cfg.database_max_connections).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "failed to initialize database");
            drop(log_guard);
            std::process::exit(1);
        }
    };

    tracing::info!(
        global_rpm = cfg.rate_limit_global_rpm,
        per_key_rpm = cfg.rate_limit_per_key_rpm,
        database_max_connections = cfg.database_max_connections,
        usage_log_max_concurrency = cfg.usage_log_max_concurrency,
        response_cache_max_entries = cfg.response_cache_max_entries,
        response_cache_ttl_seconds = cfg.response_cache_ttl_seconds,
        "rate limiter configured"
    );

    match command {
        cli::Command::Serve { .. } => {
            let registry_artifact_store = registry_artifact::RegistryArtifactStore::new(
                std::path::PathBuf::from(&cfg.private_registry_path),
            );
            let response_caches = cache::RouteResponseCaches::new(
                cfg.response_cache_max_entries,
                std::time::Duration::from_secs(cfg.response_cache_ttl_seconds),
            );

            let local_db_path = std::path::PathBuf::from(&cfg.local_db_path);
            if let Some(parent) = local_db_path
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
            {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    tracing::error!(error = %e, path = %parent.display(), "failed to create local db directory");
                    drop(log_guard);
                    std::process::exit(1);
                }
            }

            // Resolve ${VAR} placeholders in additional_rpcs against the
            // environment once, then inject them into every registry load path
            // so a rate-limited public RPC can never freeze local-db sync.
            let additional_rpcs = config::resolve_rpc_overrides(&cfg.additional_rpcs);
            for (network, urls) in &additional_rpcs {
                tracing::info!(network = %network, count = urls.len(), "resolved additional RPCs");
            }

            // Guard against the upstream bootstrap-db rollback failure mode:
            // clear orphan rows when target_watermarks is empty but data tables
            // hold rows. Non-fatal — a failed check must not block startup.
            match db_integrity::clear_orphan_rows_if_needed(&local_db_path) {
                Ok(true) => tracing::warn!(
                    db_path = %local_db_path.display(),
                    "db_integrity: cleared orphan rows on startup"
                ),
                Ok(false) => {}
                Err(e) => tracing::warn!(
                    error = %e,
                    db_path = %local_db_path.display(),
                    "db_integrity: check failed; proceeding with raindex client init"
                ),
            }

            let raindex_config = match load_startup_raindex(
                &cfg,
                &pool,
                &registry_artifact_store,
                local_db_path.clone(),
                additional_rpcs,
            )
            .await
            {
                Ok(config) => {
                    tracing::info!("raindex registry loaded");
                    config
                }
                Err(e) => {
                    tracing::error!(error = ?e, "failed to load raindex registry");
                    drop(log_guard);
                    std::process::exit(1);
                }
            };

            // Build the /sync/status fetcher from the same local DB the
            // scheduler writes to. Only available when the provider actually
            // has a local DB configured; otherwise the handler returns 503.
            let sync_status_fetcher = match raindex_config.db_path() {
                Some(db_path) => {
                    match sync_status::SyncStatusFetcher::new(
                        &db_path,
                        cfg.sync_freshness_threshold_seconds,
                    ) {
                        Ok(fetcher) => Some(fetcher),
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to build sync status fetcher; /sync/status will report unavailable");
                            None
                        }
                    }
                }
                None => {
                    tracing::warn!(
                        "no local db path configured; /sync/status will report unavailable"
                    );
                    None
                }
            };

            let shared_raindex = tokio::sync::RwLock::new(raindex_config);
            let rate_limiter =
                fairings::RateLimiter::new(cfg.rate_limit_global_rpm, cfg.rate_limit_per_key_rpm);

            if !std::path::Path::new(&cfg.docs_dir).is_dir() {
                tracing::error!(docs_dir = %cfg.docs_dir, "docs_dir is not a valid directory");
                drop(log_guard);
                std::process::exit(1);
            }
            tracing::info!(docs_dir = %cfg.docs_dir, "serving documentation at /docs");

            let app_state =
                app_state::ApplicationState::new(registry_artifact_store, response_caches);

            let rocket = match rocket(
                pool,
                rate_limiter,
                shared_raindex,
                app_state,
                cfg.docs_dir,
                cfg.usage_log_max_concurrency,
                sync_status_fetcher,
            ) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(error = %e, "failed to build Rocket instance");
                    drop(log_guard);
                    std::process::exit(1);
                }
            };

            if let Err(e) = rocket.launch().await {
                tracing::error!(error = %e, "Rocket launch failed");
                drop(log_guard);
                std::process::exit(1);
            }
        }
        cli::Command::Keys { command, .. } => {
            if let Err(e) = cli::handle_keys_command(command, pool).await {
                tracing::error!(error = %e, "keys command failed");
                drop(log_guard);
                std::process::exit(1);
            }
        }
    }

    drop(log_guard);
}

#[cfg(test)]
mod tests {
    use crate::test_helpers::{basic_auth_header, client, mock_raindex_registry_url, seed_api_key};
    use rocket::http::{Header, Status};
    use utoipa::OpenApi;

    #[rocket::async_test]
    async fn test_health_endpoint() {
        let client = client().await;
        let response = client.get("/health").dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["status"], "ok");
    }

    #[test]
    fn test_openapi_includes_token_proofs_schema() {
        let openapi = serde_json::to_value(super::ApiDoc::openapi()).expect("serialize openapi");
        let proofs_path = &openapi["paths"]["/v1/tokens/{address}/proofs"]["get"];
        let swap_calldata_v2_path = &openapi["paths"]["/v2/swap/calldata"]["post"];

        assert_eq!(proofs_path["tags"][0], "Tokens");
        assert_eq!(
            proofs_path["parameters"][0]["description"],
            "Wrapped, unwrapped, or legacy ST0x token address"
        );
        assert_eq!(
            proofs_path["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/TokenProofsResponse"
        );
        assert_eq!(
            proofs_path["responses"]["404"]["description"],
            "Wrapped ST0x token or SFT vault not found"
        );

        let schemas = &openapi["components"]["schemas"];
        assert!(schemas["TokenProofsResponse"]["properties"]["metadata"].is_object());
        assert!(schemas["TokenProofMetadata"]["properties"]["metaHash"].is_object());
        assert!(schemas["TokenProofReceipt"]["properties"]["receiptId"].is_object());
        assert!(schemas["TokenProofReceipt"]["properties"]["txHash"].is_object());
        assert!(schemas["TokenProofReceipt"]["properties"]["type"].is_object());
        assert_eq!(swap_calldata_v2_path["tags"][0], "Swap");
        assert_eq!(
            swap_calldata_v2_path["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/SwapCalldataV2Request"
        );
        assert_eq!(
            schemas["SwapCalldataMode"]["enum"],
            serde_json::json!(["buyUpTo", "spendExact", "spendUpTo"])
        );
    }

    #[test]
    fn test_openapi_documents_token_details_activity_limit() {
        let openapi = serde_json::to_value(super::ApiDoc::openapi()).expect("serialize openapi");
        let details_path = &openapi["paths"]["/v1/tokens/{address}/details"]["get"];
        let parameters = details_path["parameters"]
            .as_array()
            .expect("parameters is an array");

        assert!(parameters
            .iter()
            .any(|parameter| parameter["name"] == "activityLimit"));
        assert!(!parameters
            .iter()
            .any(|parameter| parameter["name"] == "activity_limit"));
    }

    fn test_config(
        registry_url: String,
        private_registry_path: std::path::PathBuf,
        local_db_path: std::path::PathBuf,
        allow_registry_fallback: bool,
    ) -> crate::config::Config {
        crate::config::Config {
            log_dir: "./logs".to_string(),
            database_url: "sqlite::memory:".to_string(),
            database_max_connections: 5,
            usage_log_max_concurrency: 2,
            response_cache_max_entries: 0,
            response_cache_ttl_seconds: 0,
            registry_url,
            private_registry_path: private_registry_path.to_string_lossy().into_owned(),
            allow_registry_fallback,
            rate_limit_global_rpm: 600,
            rate_limit_per_key_rpm: 60,
            docs_dir: "./docs/book".to_string(),
            local_db_path: local_db_path.to_string_lossy().into_owned(),
            sync_freshness_threshold_seconds: 300,
            additional_rpcs: std::collections::HashMap::new(),
        }
    }

    async fn insert_successful_registry_history(pool: &crate::db::DbPool, artifact: &str) {
        crate::db::registry_history::insert_private_registry_change(
            pool,
            &crate::db::registry_history::NewPrivateRegistryHistory {
                source_commit: "1111111111111111111111111111111111111111",
                payload_sha256: &crate::registry_artifact::artifact_sha256(artifact),
                actor_key_id: "deploy",
                actor_label: "deploy",
                actor_owner: "deploy",
                validation_status: crate::db::registry_history::VALIDATION_STATUS_SUCCESS,
                validation_error: None,
            },
        )
        .await
        .expect("insert registry history");
    }

    #[rocket::async_test]
    async fn test_load_startup_raindex_falls_back_when_private_registry_fails() {
        let dir = tempfile::tempdir().expect("temp dir");
        let private_registry_path = dir.path().join("private-registry.data");
        let local_db_path = dir.path().join("raindex.db");
        let invalid_artifact = "data:text/plain;base64,dGhpcyBpcyBub3QgYSByZWdpc3RyeQo=";
        let fallback_registry_url = mock_raindex_registry_url().await;
        let cfg = test_config(
            fallback_registry_url,
            private_registry_path.clone(),
            local_db_path.clone(),
            true,
        );
        let pool = crate::db::init("sqlite::memory:", 5)
            .await
            .expect("database init");
        let store = crate::registry_artifact::RegistryArtifactStore::new(private_registry_path);

        store
            .persist(invalid_artifact)
            .await
            .expect("persist invalid artifact");
        insert_successful_registry_history(&pool, invalid_artifact).await;

        let provider = super::load_startup_raindex(
            &cfg,
            &pool,
            &store,
            local_db_path,
            std::collections::HashMap::new(),
        )
        .await;

        assert!(provider.is_ok());
    }

    #[rocket::async_test]
    async fn test_load_startup_raindex_errors_when_fallback_disabled() {
        let dir = tempfile::tempdir().expect("temp dir");
        let private_registry_path = dir.path().join("private-registry.data");
        let local_db_path = dir.path().join("raindex.db");
        let invalid_artifact = "data:text/plain;base64,dGhpcyBpcyBub3QgYSByZWdpc3RyeQo=";
        let fallback_registry_url = mock_raindex_registry_url().await;
        let cfg = test_config(
            fallback_registry_url,
            private_registry_path.clone(),
            local_db_path.clone(),
            false,
        );
        let pool = crate::db::init("sqlite::memory:", 5)
            .await
            .expect("database init");
        let store = crate::registry_artifact::RegistryArtifactStore::new(private_registry_path);

        store
            .persist(invalid_artifact)
            .await
            .expect("persist invalid artifact");
        insert_successful_registry_history(&pool, invalid_artifact).await;

        let err = super::load_startup_raindex(
            &cfg,
            &pool,
            &store,
            local_db_path,
            std::collections::HashMap::new(),
        )
        .await
        .expect_err("private registry load should fail");

        assert!(matches!(
            err,
            super::StartupRegistryError::PrivateRegistryLoad(_)
        ));
    }

    #[rocket::async_test]
    async fn test_protected_route_returns_401_without_auth() {
        let client = client().await;
        let response = client.get("/v1/tokens").dispatch().await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_protected_route_returns_401_with_wrong_secret() {
        let client = client().await;
        let (key_id, _) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, "wrong-secret");
        let response = client
            .get("/v1/tokens")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_protected_route_succeeds_with_valid_auth() {
        let client = client().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/tokens")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_ne!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_inactive_key_returns_401() {
        let client = client().await;
        let (key_id, secret) = seed_api_key(&client).await;

        let pool = client
            .rocket()
            .state::<crate::db::DbPool>()
            .expect("pool in state");
        sqlx::query("UPDATE api_keys SET active = 0 WHERE key_id = ?")
            .bind(&key_id)
            .execute(pool)
            .await
            .expect("deactivate key");

        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/tokens")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }
}
