use crate::app_state::ApplicationState;
use crate::auth::AdminKey;
use crate::db::{registry_history, DbPool};
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::raindex::{RaindexProvider, SharedRaindexProvider};
use crate::registry_artifact::artifact_sha256;
use rocket::http::Status;
use rocket::serde::json::Json;
use rocket::{Route, State};
use serde::{Deserialize, Serialize};
use tracing::Instrument;
use utoipa::ToSchema;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UploadRegistryArtifactRequest {
    pub registry_artifact: String,
    pub source_commit: String,
}

#[utoipa::path(
    put,
    path = "/admin/registry",
    tag = "Admin",
    security(("basicAuth" = [])),
    request_body = UploadRegistryArtifactRequest,
    responses(
        (status = 200, description = "Registry artifact updated"),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[put("/registry", data = "<request>")]
pub async fn put_registry(
    _global: GlobalRateLimit,
    admin: AdminKey,
    shared_raindex: &State<SharedRaindexProvider>,
    app_state: &State<ApplicationState>,
    pool: &State<DbPool>,
    span: TracingSpan,
    request: Json<UploadRegistryArtifactRequest>,
) -> Result<Status, ApiError> {
    let mut req = request.into_inner();
    req.source_commit = req.source_commit.trim().to_string();
    async move {
        tracing::info!(
            source_commit = %req.source_commit,
            admin_key_id = %admin.0.key_id,
            "request received"
        );

        validate_request(&req)?;
        let payload_sha256 = artifact_sha256(&req.registry_artifact);

        let (db_path, additional_rpcs) = {
            let guard = shared_raindex.read().await;
            (guard.db_path(), guard.additional_rpcs())
        };

        // Apply the same additional_rpcs injection to admin-uploaded private
        // registry artifacts so a rate-limited public RPC can't freeze sync
        // regardless of registry source.
        let new_provider =
            match RaindexProvider::load(&req.registry_artifact, db_path, additional_rpcs).await {
                Ok(provider) => provider,
                Err(e) => {
                    let validation_error = e.safe_summary();
                    tracing::warn!(
                        source_commit = %req.source_commit,
                        payload_sha256 = %payload_sha256,
                        admin_key_id = %admin.0.key_id,
                        validation_error = %validation_error,
                        "failed to validate registry artifact"
                    );

                    insert_history(
                        pool,
                        &req,
                        &payload_sha256,
                        &admin,
                        registry_history::VALIDATION_STATUS_FAILED,
                        Some(validation_error),
                    )
                    .await?;

                    return Err(ApiError::BadRequest(
                        "failed to validate registry artifact".into(),
                    ));
                }
            };

        let artifact_store = &app_state.registry_artifact_store;
        let _update_guard = artifact_store.lock_update().await;

        let previous_artifact = artifact_store.load().await.map_err(|e| {
            tracing::error!(error = %e, "failed to read previous private registry artifact");
            ApiError::Internal("failed to persist registry artifact".into())
        })?;

        artifact_store
            .persist(&req.registry_artifact)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to persist private registry artifact");
                ApiError::Internal("failed to persist registry artifact".into())
            })?;

        if let Err(e) = insert_history(
            pool,
            &req,
            &payload_sha256,
            &admin,
            registry_history::VALIDATION_STATUS_SUCCESS,
            None,
        )
        .await
        {
            tracing::error!(
                error = %e,
                "failed to persist private registry history; restoring previous artifact"
            );
            if let Err(restore_error) = artifact_store.restore(previous_artifact.as_deref()).await {
                tracing::error!(
                    error = %restore_error,
                    "failed to restore previous private registry artifact"
                );
            }
            return Err(e);
        }

        let mut guard = shared_raindex.write().await;
        *guard = new_provider;
        drop(guard);
        app_state.response_caches.invalidate_all();

        tracing::info!(
            source_commit = %req.source_commit,
            payload_sha256 = %payload_sha256,
            admin_key_id = %admin.0.key_id,
            "registry artifact updated"
        );

        Ok(Status::Ok)
    }
    .instrument(span.0)
    .await
}

pub fn routes() -> Vec<Route> {
    rocket::routes![put_registry]
}

fn validate_request(req: &UploadRegistryArtifactRequest) -> Result<(), ApiError> {
    if req.registry_artifact.is_empty() {
        return Err(ApiError::BadRequest(
            "registry_artifact must not be empty".into(),
        ));
    }
    if !req.registry_artifact.starts_with("data:text/plain;base64,") {
        return Err(ApiError::BadRequest(
            "registry_artifact must be a data:text/plain;base64 URI".into(),
        ));
    }
    if req.source_commit.is_empty() {
        return Err(ApiError::BadRequest(
            "source_commit must not be empty".into(),
        ));
    }
    if req.source_commit.len() != 40
        || !req
            .source_commit
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ApiError::BadRequest(
            "source_commit must be a 40-character hexadecimal commit SHA".into(),
        ));
    }

    Ok(())
}

async fn insert_history(
    pool: &DbPool,
    req: &UploadRegistryArtifactRequest,
    payload_sha256: &str,
    admin: &AdminKey,
    validation_status: &str,
    validation_error: Option<&str>,
) -> Result<(), ApiError> {
    registry_history::insert_private_registry_change(
        pool,
        &registry_history::NewPrivateRegistryHistory {
            source_commit: &req.source_commit,
            payload_sha256,
            actor_key_id: &admin.0.key_id,
            actor_label: &admin.0.label,
            actor_owner: &admin.0.owner,
            validation_status,
            validation_error,
        },
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "failed to persist private registry history");
        ApiError::Internal("failed to persist registry history".into())
    })
}

#[cfg(test)]
mod tests {
    use super::{validate_request, UploadRegistryArtifactRequest};
    use crate::db::registry_history::{self, PrivateRegistryHistoryRow};
    use crate::test_helpers::{
        basic_auth_header, mock_raindex_registry_artifact, seed_admin_key, seed_api_key,
        TestClientBuilder,
    };
    use rocket::http::{ContentType, Header, Status};
    use serde_json::json;

    const COMMIT_ONE: &str = "1111111111111111111111111111111111111111";
    const BAD_COMMIT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const RESTART_COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    async fn history_rows(
        client: &rocket::local::asynchronous::Client,
    ) -> Vec<PrivateRegistryHistoryRow> {
        let pool = client
            .rocket()
            .state::<crate::db::DbPool>()
            .expect("pool in state");
        registry_history::list_private_registry_history(pool)
            .await
            .expect("query registry history")
    }

    fn upload_body(artifact: &str, commit: &str) -> String {
        json!({
            "registry_artifact": artifact,
            "source_commit": commit
        })
        .to_string()
    }

    #[rocket::async_test]
    async fn test_put_registry_artifact_with_admin_key() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_admin_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let artifact = mock_raindex_registry_artifact();
        let commit = "fb6b06ea12c941157000d60621184d2f99b55f71";

        let response = client
            .put("/admin/registry")
            .header(Header::new("Authorization", header.clone()))
            .header(ContentType::JSON)
            .body(upload_body(&artifact, commit))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);
        assert_eq!(response.into_string().await, None);

        let get_response = client
            .get("/registry")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(get_response.status(), Status::Ok);
        let get_body: serde_json::Value =
            serde_json::from_str(&get_response.into_string().await.unwrap()).unwrap();
        assert_eq!(get_body["registry_type"], "private_artifact");
        assert_eq!(get_body["source_commit"], commit);
        assert!(get_body.get("registry_url").is_none());

        let history = history_rows(&client).await;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].source_commit, commit);
        assert_eq!(history[0].validation_status, "success");
        assert!(history[0].validation_error.is_none());
        assert_ne!(history[0].payload_sha256, artifact);
        assert!(!history[0].payload_sha256.contains("data:"));
        assert_eq!(history[0].actor_key_id, key_id);
        assert_eq!(history[0].actor_label, "admin-key");
        assert_eq!(history[0].actor_owner, "admin-owner");
        assert!(!history[0].changed_at.is_empty());
    }

    #[rocket::async_test]
    async fn test_put_registry_with_non_admin_key_returns_403() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let response = client
            .put("/admin/registry")
            .header(Header::new("Authorization", header))
            .header(ContentType::JSON)
            .body(upload_body(&mock_raindex_registry_artifact(), COMMIT_ONE))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Forbidden);
        assert!(history_rows(&client).await.is_empty());
    }

    #[rocket::async_test]
    async fn test_put_registry_without_auth_returns_401() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .put("/admin/registry")
            .header(ContentType::JSON)
            .body(upload_body(&mock_raindex_registry_artifact(), COMMIT_ONE))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
        assert!(history_rows(&client).await.is_empty());
    }

    #[test]
    fn test_validate_request_rejects_invalid_upload_shapes() {
        let cases = [
            ("", COMMIT_ONE),
            ("https://registry.example.com", COMMIT_ONE),
            ("data:text/plain;base64,abc", "not-a-sha"),
        ];

        for (registry_artifact, source_commit) in cases {
            let req = UploadRegistryArtifactRequest {
                registry_artifact: registry_artifact.to_string(),
                source_commit: source_commit.to_string(),
            };
            assert!(validate_request(&req).is_err());
        }
    }

    #[test]
    fn test_trimmed_source_commit_is_valid() {
        let mut req = UploadRegistryArtifactRequest {
            registry_artifact: "data:text/plain;base64,abc".to_string(),
            source_commit: format!(" {COMMIT_ONE} "),
        };

        req.source_commit = req.source_commit.trim().to_string();

        assert_eq!(req.source_commit, COMMIT_ONE);
        assert!(validate_request(&req).is_ok());
    }

    #[rocket::async_test]
    async fn test_put_registry_failed_validation_does_not_replace_existing_artifact() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("private-registry.data");
        let client = TestClientBuilder::new()
            .private_registry_path(path.clone())
            .build()
            .await;
        let (key_id, secret) = seed_admin_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let artifact = mock_raindex_registry_artifact();

        let first_response = client
            .put("/admin/registry")
            .header(Header::new("Authorization", header.clone()))
            .header(ContentType::JSON)
            .body(upload_body(&artifact, COMMIT_ONE))
            .dispatch()
            .await;
        assert_eq!(first_response.status(), Status::Ok);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read artifact"),
            artifact
        );

        let invalid_artifact = "data:text/plain;base64,dGhpcyBpcyBub3QgdmFsaWQ=";
        let failed_response = client
            .put("/admin/registry")
            .header(Header::new("Authorization", header.clone()))
            .header(ContentType::JSON)
            .body(upload_body(invalid_artifact, BAD_COMMIT))
            .dispatch()
            .await;
        assert_eq!(failed_response.status(), Status::BadRequest);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read artifact after failure"),
            artifact
        );

        let get_after = client
            .get("/registry")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(get_after.status(), Status::Ok);
        let after_body: serde_json::Value =
            serde_json::from_str(&get_after.into_string().await.unwrap()).unwrap();
        assert_eq!(after_body["registry_type"], "private_artifact");
        assert_eq!(after_body["source_commit"], COMMIT_ONE);

        let history = history_rows(&client).await;
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].source_commit, BAD_COMMIT);
        assert_eq!(history[0].validation_status, "failed");
        assert_eq!(history[1].source_commit, COMMIT_ONE);
        assert_eq!(history[1].validation_status, "success");
    }

    #[rocket::async_test]
    async fn test_put_registry_persists_artifact_for_restart_without_exposing_data_uri() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("private-registry.data");
        let db_path = dir.path().join("st0x.db");
        let database_url = format!("sqlite://{}", db_path.display());
        let client = TestClientBuilder::new()
            .private_registry_path(path.clone())
            .database_url(database_url.clone())
            .build()
            .await;
        let (admin_key_id, admin_secret) = seed_admin_key(&client).await;
        let admin_header = basic_auth_header(&admin_key_id, &admin_secret);
        let artifact = mock_raindex_registry_artifact();

        let response = client
            .put("/admin/registry")
            .header(Header::new("Authorization", admin_header))
            .header(ContentType::JSON)
            .body(upload_body(&artifact, RESTART_COMMIT))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        drop(response);

        let stored_artifact = std::fs::read_to_string(&path).expect("read artifact");
        assert_eq!(stored_artifact, artifact);

        drop(client);

        let restarted_client = TestClientBuilder::new()
            .private_registry_path(path.clone())
            .database_url(database_url)
            .build()
            .await;
        let (key_id, secret) = seed_api_key(&restarted_client).await;
        let header = basic_auth_header(&key_id, &secret);

        let get_response = restarted_client
            .get("/registry")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(get_response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&get_response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["registry_type"], "private_artifact");
        assert_eq!(body["source_commit"], RESTART_COMMIT);
        assert!(body["payload_sha256"].as_str().is_some());
        assert!(body.get("registry_url").is_none());
    }
}
