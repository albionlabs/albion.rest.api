use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::TracingSpan;
use crate::sync_status::SyncStatusFetcher;
use crate::types::sync_status::SyncStatusResponse;
use rocket::http::Status;
use rocket::serde::json::Json;
use rocket::{Route, State};
use tracing::Instrument;

#[utoipa::path(
    get,
    path = "/sync/status",
    tag = "Health",
    responses(
        (status = 200, description = "All chains fresh (within threshold)", body = SyncStatusResponse),
        (status = 503, description = "At least one chain is stale or no watermarks recorded", body = SyncStatusResponse),
        (status = 500, description = "Failed to read local DB", body = ApiErrorResponse),
    )
)]
#[get("/sync/status")]
pub async fn get_sync_status(
    fetcher: &State<Option<SyncStatusFetcher>>,
    span: TracingSpan,
) -> Result<(Status, Json<SyncStatusResponse>), ApiError> {
    async move {
        let Some(fetcher) = fetcher.as_ref() else {
            // The fetcher is only None when the local DB path isn't
            // configured (`local_db_path` missing) — i.e. dev runs.
            // Treat this as "service has no sync to report", but still
            // return 503 so monitors flag it.
            tracing::warn!("sync status requested but fetcher unavailable");
            let resp = SyncStatusResponse {
                status: "stale".into(),
                threshold_seconds: 0,
                chains: Vec::new(),
            };
            return Ok((Status::ServiceUnavailable, Json(resp)));
        };

        let resp = fetcher.fetch().await?;
        let status = if resp.status == "fresh" {
            Status::Ok
        } else {
            Status::ServiceUnavailable
        };

        // Log at info for fresh, warn for stale — operators can grep.
        if resp.status == "fresh" {
            tracing::info!(
                chains = resp.chains.len(),
                threshold_seconds = resp.threshold_seconds,
                "sync status: fresh"
            );
        } else {
            let max_behind = resp
                .chains
                .iter()
                .map(|c| c.seconds_behind_chain)
                .max()
                .unwrap_or(0);
            tracing::warn!(
                chains = resp.chains.len(),
                threshold_seconds = resp.threshold_seconds,
                max_seconds_behind = max_behind,
                "sync status: stale"
            );
        }

        Ok((status, Json(resp)))
    }
    .instrument(span.0)
    .await
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_sync_status]
}

#[cfg(test)]
mod tests {
    use crate::sync_status::SyncStatusFetcher;
    use crate::test_helpers::TestClientBuilder;
    use rocket::http::Status;
    use rusqlite::{params, Connection};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    fn seed_watermark_db(
        rows: &[(u32, &str, u64, u64)],
    ) -> (std::path::PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test.db");
        let conn = Connection::open(&path).expect("open");
        conn.execute_batch(
            "CREATE TABLE target_watermarks (
                chain_id INTEGER NOT NULL,
                raindex_address TEXT NOT NULL,
                last_block INTEGER NOT NULL,
                last_hash TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (chain_id, raindex_address)
            );",
        )
        .expect("schema");
        for (chain_id, addr, block, updated_at_ms) in rows {
            conn.execute(
                "INSERT INTO target_watermarks (chain_id, raindex_address, last_block, last_hash, updated_at) \
                 VALUES (?1, ?2, ?3, '0x', ?4)",
                params![*chain_id as i64, addr, *block as i64, *updated_at_ms as i64],
            )
            .expect("insert");
        }
        drop(conn);
        (path, dir)
    }

    #[rocket::async_test]
    async fn returns_200_when_fresh() {
        let (path, _dir) = seed_watermark_db(&[(
            8453,
            "0x000000000000000000000000000000000000beef",
            42,
            now_ms() - 5_000,
        )]);
        let fetcher = SyncStatusFetcher::new(&path, 300).unwrap();
        let client = TestClientBuilder::new()
            .sync_status_fetcher(fetcher)
            .build()
            .await;

        let response = client.get("/sync/status").dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["status"], "fresh");
        assert_eq!(body["threshold_seconds"], 300);
        assert!(body["chains"][0]["fresh"].as_bool().unwrap());
    }

    #[rocket::async_test]
    async fn returns_503_when_stale() {
        let (path, _dir) = seed_watermark_db(&[(
            8453,
            "0x000000000000000000000000000000000000beef",
            42,
            now_ms() - 10 * 60 * 1000,
        )]);
        let fetcher = SyncStatusFetcher::new(&path, 300).unwrap();
        let client = TestClientBuilder::new()
            .sync_status_fetcher(fetcher)
            .build()
            .await;

        let response = client.get("/sync/status").dispatch().await;
        assert_eq!(response.status(), Status::ServiceUnavailable);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["status"], "stale");
        assert!(!body["chains"][0]["fresh"].as_bool().unwrap());
    }

    #[rocket::async_test]
    async fn returns_503_when_no_fetcher_configured() {
        // Default builder doesn't set a fetcher (state is `None`), simulating
        // the dev-config path where local_db_path isn't set.
        let client = TestClientBuilder::new().build().await;
        let response = client.get("/sync/status").dispatch().await;
        assert_eq!(response.status(), Status::ServiceUnavailable);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["status"], "stale");
        assert_eq!(body["chains"].as_array().unwrap().len(), 0);
    }

    #[rocket::async_test]
    async fn endpoint_is_unauthenticated() {
        // Smoke test: no Authorization header, still 200. Mirrors the /health
        // contract — operators should be able to point a load balancer or
        // monitoring probe at this without provisioning a key.
        let (path, _dir) = seed_watermark_db(&[(
            8453,
            "0x0000000000000000000000000000000000000001",
            10,
            now_ms() - 1_000,
        )]);
        let fetcher = SyncStatusFetcher::new(&path, 300).unwrap();
        let client = TestClientBuilder::new()
            .sync_status_fetcher(fetcher)
            .build()
            .await;
        let response = client.get("/sync/status").dispatch().await;
        assert_eq!(response.status(), Status::Ok);
    }
}
