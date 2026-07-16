//! Sync-freshness reader for `/sync/status`. Opens its own rusqlite
//! connection against the local DB that the in-process scheduler is writing
//! to. We only read `target_watermarks` here — no triggers fire, so we don't
//! need to register custom SQLite functions.

use crate::error::ApiError;
use crate::types::sync_status::{SyncStatusEntry, SyncStatusResponse};
use alloy::primitives::Address;
use rusqlite::Connection;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::task::spawn_blocking;

pub(crate) struct SyncStatusFetcher {
    conn: Arc<Mutex<Connection>>,
    threshold_seconds: u64,
}

impl SyncStatusFetcher {
    pub(crate) fn new(db_path: &Path, threshold_seconds: u64) -> Result<Self, String> {
        let conn = Connection::open(db_path)
            .map_err(|e| format!("failed to open raindex db for sync status: {e}"))?;
        // WAL mode + a 5s busy timeout so a writer checkpointing the WAL
        // doesn't make /sync/status flap.
        conn.pragma_update(None, "journal_mode", "wal")
            .map_err(|e| format!("failed to set WAL: {e}"))?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|e| format!("failed to set busy_timeout: {e}"))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            threshold_seconds,
        })
    }

    pub(crate) async fn fetch(&self) -> Result<SyncStatusResponse, ApiError> {
        let conn = Arc::clone(&self.conn);
        let threshold = self.threshold_seconds;

        spawn_blocking(move || {
            let conn = conn.lock().map_err(|e| {
                tracing::error!(error = %e, "failed to lock sync status connection");
                ApiError::Internal("sync status query failed".into())
            })?;

            let mut stmt = conn
                .prepare(
                    "SELECT chain_id, raindex_address, last_block, updated_at \
                     FROM target_watermarks \
                     ORDER BY chain_id, raindex_address",
                )
                .map_err(|e| {
                    tracing::error!(error = %e, "failed to prepare sync status query");
                    ApiError::Internal("sync status query failed".into())
                })?;

            let now_seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .map_err(|e| {
                    tracing::error!(error = %e, "system time before unix epoch");
                    ApiError::Internal("sync status query failed".into())
                })?;

            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })
                .map_err(|e| {
                    tracing::error!(error = %e, "sync status query failed");
                    ApiError::Internal("sync status query failed".into())
                })?;

            let mut entries = Vec::new();
            for row_result in rows {
                let (chain_id_i64, ob_addr, last_block_i64, updated_at_ms_i64) =
                    row_result.map_err(|e| {
                        tracing::error!(error = %e, "failed to read sync status row");
                        ApiError::Internal("sync status query failed".into())
                    })?;

                // In the current upstream schema `target_watermarks.updated_at`
                // is the wall-clock time (in milliseconds) at which the
                // watermark row was last written by the sync pipeline
                // (upsert sets it to strftime('%s','now')*1000). So
                // `now - updated_at` measures how long since the last
                // successful sync write — the staleness signal we want.
                let last_synced_at = (updated_at_ms_i64.max(0) as u64) / 1000;
                let seconds_behind = now_seconds.saturating_sub(last_synced_at);

                let address = Address::from_str(&ob_addr).map_err(|e| {
                    tracing::error!(error = %e, address = %ob_addr, "invalid orderbook address in target_watermarks");
                    ApiError::Internal("sync status query failed".into())
                })?;

                entries.push(SyncStatusEntry {
                    chain_id: chain_id_i64.max(0) as u32,
                    orderbook_address: address,
                    last_synced_block: last_block_i64.max(0) as u64,
                    last_synced_block_timestamp: last_synced_at,
                    seconds_behind_chain: seconds_behind,
                    fresh: seconds_behind <= threshold,
                });
            }

            // Aggregate freshness rule: empty `target_watermarks` is treated
            // as STALE rather than fresh. An empty watermark row set means
            // either we've never synced or someone wiped the table; in
            // either case readers are not getting current data, which is
            // exactly what monitoring should flag.
            let aggregate_fresh = !entries.is_empty() && entries.iter().all(|e| e.fresh);

            Ok(SyncStatusResponse {
                status: if aggregate_fresh { "fresh" } else { "stale" }.to_string(),
                threshold_seconds: threshold,
                chains: entries,
            })
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "sync status blocking task failed");
            ApiError::Internal("sync status query failed".into())
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use std::fs;
    use std::path::PathBuf;

    fn temp_db_with_watermarks(rows: &[(u32, &str, u64, u64)]) -> (PathBuf, tempfile::TempDir) {
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
        .expect("create");
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

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    #[tokio::test]
    async fn fetch_reports_fresh_when_within_threshold() {
        let (path, _tmp) = temp_db_with_watermarks(&[(
            8453,
            "0x000000000000000000000000000000000000beef",
            42,
            now_ms() - 10_000, // 10s behind
        )]);
        let fetcher = SyncStatusFetcher::new(&path, 60).unwrap();
        let resp = fetcher.fetch().await.unwrap();
        assert_eq!(resp.status, "fresh");
        assert_eq!(resp.threshold_seconds, 60);
        assert_eq!(resp.chains.len(), 1);
        assert!(resp.chains[0].fresh);
        assert!(resp.chains[0].seconds_behind_chain <= 11);
    }

    #[tokio::test]
    async fn fetch_reports_stale_when_beyond_threshold() {
        let (path, _tmp) = temp_db_with_watermarks(&[(
            8453,
            "0x000000000000000000000000000000000000beef",
            42,
            now_ms() - 10 * 60 * 1000, // 10 minutes behind
        )]);
        let fetcher = SyncStatusFetcher::new(&path, 300).unwrap();
        let resp = fetcher.fetch().await.unwrap();
        assert_eq!(resp.status, "stale");
        assert!(!resp.chains[0].fresh);
        assert!(resp.chains[0].seconds_behind_chain >= 590);
    }

    #[tokio::test]
    async fn fetch_aggregate_stale_if_any_chain_stale() {
        let (path, _tmp) = temp_db_with_watermarks(&[
            (
                8453,
                "0x000000000000000000000000000000000000beef",
                42,
                now_ms() - 5_000, // fresh
            ),
            (
                42161,
                "0x000000000000000000000000000000000000feed",
                100,
                now_ms() - 10 * 60 * 1000, // stale
            ),
        ]);
        let fetcher = SyncStatusFetcher::new(&path, 300).unwrap();
        let resp = fetcher.fetch().await.unwrap();
        assert_eq!(resp.status, "stale");
        assert_eq!(resp.chains.len(), 2);
        assert!(resp.chains.iter().any(|c| c.fresh));
        assert!(resp.chains.iter().any(|c| !c.fresh));
    }

    #[tokio::test]
    async fn fetch_empty_watermarks_is_stale() {
        let (path, _tmp) = temp_db_with_watermarks(&[]);
        let fetcher = SyncStatusFetcher::new(&path, 300).unwrap();
        let resp = fetcher.fetch().await.unwrap();
        assert_eq!(resp.status, "stale");
        assert!(resp.chains.is_empty());
    }

    #[tokio::test]
    async fn fetch_errors_when_db_path_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope/missing.db");
        // Connection::open will create the file if the parent dir exists, so
        // we point at a non-existent parent dir to force a real error.
        let err = SyncStatusFetcher::new(&path, 300);
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn fetch_returns_internal_error_when_table_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.db");
        let _ = Connection::open(&path).unwrap();
        // Don't create target_watermarks table — fetch should error.
        let fetcher = SyncStatusFetcher::new(&path, 300).unwrap();
        let err = fetcher.fetch().await.unwrap_err();
        match err {
            ApiError::Internal(msg) => assert!(msg.contains("sync status query failed")),
            other => panic!("unexpected: {other:?}"),
        }
        let _ = fs::remove_file(&path);
    }

    #[tokio::test]
    async fn fetch_uses_configured_threshold_in_response() {
        let (path, _dir) = temp_db_with_watermarks(&[(
            8453,
            "0x000000000000000000000000000000000000beef",
            42,
            now_ms() - 1_000,
        )]);
        let fetcher = SyncStatusFetcher::new(&path, 123).unwrap();
        let resp = fetcher.fetch().await.unwrap();
        assert_eq!(resp.threshold_seconds, 123);
    }
}
