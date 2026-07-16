//! Startup-time guard against the upstream `bootstrap_phase` failure mode
//! observed on 2026-04-27.
//!
//! `ClientBootstrapAdapter::engine_run` (in `rain_orderbook_common`) takes a
//! shortcut when `target_watermarks` is empty for the orderbook: it treats the
//! DB as "fresh" and applies the dump WITHOUT first calling
//! `clear_orderbook_data`. If any other required table still has rows for
//! that orderbook (because a previous run was interrupted between
//! `clear_orderbook_data` and the dump replay, or because of any other
//! partial-failure path), the dump's INSERTs collide with the orphans on
//! UNIQUE/PK constraints, the entire dump transaction rolls back, and the API
//! silently serves whatever was in the DB before — exactly the symptom that
//! produced the original "stale trades" report.
//!
//! This module runs before `RaindexClient::new()` and clears every required
//! data table when it detects that pattern (watermark empty + orphan rows).
//! It is a heuristic that masks an upstream bug; if the upstream fix lands or
//! the failure mode changes, the WARN log here is the signal.

use rain_orderbook_common::local_db::functions;
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

/// Required data tables that `clear_orderbook_data` would normally purge.
/// Excludes `db_metadata` (schema version row, must persist) and
/// `target_watermarks` (already known empty when we run).
const TABLES_TO_CLEAR: &[&str] = &[
    "raw_events",
    "deposits",
    "withdrawals",
    "order_events",
    "order_ios",
    "take_orders",
    "take_order_contexts",
    "context_values",
    "clear_v3_events",
    "after_clear_v2_events",
    "meta_events",
    "erc20_tokens",
    "interpreter_store_sets",
    "vault_balance_changes",
    "running_vault_balances",
    "derived_trades",
    "derived_vault_deltas",
    "sync_status",
];

/// Returns `Ok(true)` if a clear was performed, `Ok(false)` if the DB was
/// already in a healthy state (or doesn't exist / has no schema yet). Errors
/// are returned but should be treated as non-fatal at the call site — failing
/// the integrity check shouldn't prevent the service from starting.
pub(crate) fn clear_orphan_rows_if_needed(db_path: &Path) -> Result<bool, String> {
    if !db_path.exists() {
        // Truly fresh install — let the lib bootstrap from scratch.
        return Ok(false);
    }

    let conn = Connection::open(db_path).map_err(|e| format!("integrity check: open db: {e}"))?;
    // Several tables have triggers that call FLOAT_NEGATE/FLOAT_SUM. Without
    // these registered, DELETE statements on those tables fail with
    // "no such function". The lib registers them on every connection it
    // opens; we mirror that here.
    functions::register_all(&conn)
        .map_err(|e| format!("integrity check: register sqlite functions: {e}"))?;
    conn.pragma_update(None, "journal_mode", "wal")
        .map_err(|e| format!("integrity check: enable WAL: {e}"))?;

    // If the schema isn't initialized yet, skip — the lib will create it.
    if !table_exists(&conn, "target_watermarks")? {
        return Ok(false);
    }

    let watermark_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM target_watermarks", [], |r| r.get(0))
        .map_err(|e| format!("integrity check: count watermarks: {e}"))?;
    if watermark_count > 0 {
        return Ok(false);
    }

    let mut total_orphans: i64 = 0;
    let mut per_table: Vec<(String, i64)> = Vec::new();
    for table in TABLES_TO_CLEAR {
        if !table_exists(&conn, table)? {
            continue;
        }
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .map_err(|e| format!("integrity check: count {table}: {e}"))?;
        total_orphans += count;
        if count > 0 {
            per_table.push((table.to_string(), count));
        }
    }

    if total_orphans == 0 {
        return Ok(false);
    }

    tracing::warn!(
        total_orphans,
        ?per_table,
        "target_watermarks empty but data tables hold orphan rows; clearing for clean re-bootstrap"
    );

    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("integrity check: begin tx: {e}"))?;
    for table in TABLES_TO_CLEAR {
        if !table_exists_with(&tx, table)? {
            continue;
        }
        tx.execute(&format!("DELETE FROM {table}"), [])
            .map_err(|e| format!("integrity check: DELETE FROM {table}: {e}"))?;
    }
    tx.commit()
        .map_err(|e| format!("integrity check: commit: {e}"))?;

    Ok(true)
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name = ?1",
        [name],
        |_| Ok(()),
    )
    .optional()
    .map(|opt| opt.is_some())
    .map_err(|e| format!("integrity check: check table {name}: {e}"))
}

fn table_exists_with(tx: &rusqlite::Transaction<'_>, name: &str) -> Result<bool, String> {
    tx.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name = ?1",
        [name],
        |_| Ok(()),
    )
    .optional()
    .map(|opt| opt.is_some())
    .map_err(|e| format!("integrity check: check table {name}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use std::path::PathBuf;

    /// Creates a DB with the subset of schema this module touches. We
    /// deliberately don't load the lib's full schema — we want to test the
    /// guard in isolation. The schema below mirrors enough of the real
    /// `target_watermarks` and one data table to exercise both branches.
    fn fresh_db_with_minimal_schema() -> (PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("integrity.db");
        let conn = Connection::open(&path).expect("open");
        conn.execute_batch(
            "CREATE TABLE target_watermarks (
                chain_id INTEGER NOT NULL,
                orderbook_address TEXT NOT NULL,
                last_block INTEGER NOT NULL,
                last_hash TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (chain_id, orderbook_address)
            );
            CREATE TABLE erc20_tokens (
                chain_id INTEGER NOT NULL,
                orderbook_address TEXT NOT NULL,
                token_address TEXT NOT NULL,
                name TEXT NOT NULL,
                symbol TEXT NOT NULL,
                decimals INTEGER NOT NULL,
                PRIMARY KEY (chain_id, orderbook_address, token_address)
            );
            CREATE TABLE order_events (
                chain_id INTEGER NOT NULL,
                orderbook_address TEXT NOT NULL,
                transaction_hash TEXT NOT NULL,
                log_index INTEGER NOT NULL,
                block_number INTEGER NOT NULL,
                block_timestamp INTEGER NOT NULL,
                order_owner TEXT NOT NULL,
                order_nonce TEXT NOT NULL,
                order_hash TEXT NOT NULL,
                event_type TEXT NOT NULL,
                PRIMARY KEY (chain_id, orderbook_address, transaction_hash, log_index)
            );",
        )
        .expect("schema");
        drop(conn);
        (path, dir)
    }

    fn insert_token(path: &Path, addr: &str) {
        let conn = Connection::open(path).unwrap();
        conn.execute(
            "INSERT INTO erc20_tokens (chain_id, orderbook_address, token_address, name, symbol, decimals) \
             VALUES (?1, ?2, ?3, 'X', 'X', 18)",
            params![8453_i64, "0xobid", addr],
        )
        .unwrap();
    }

    fn insert_watermark(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute(
            "INSERT INTO target_watermarks (chain_id, orderbook_address, last_block, last_hash, updated_at) \
             VALUES (8453, '0xobid', 100, '0x', 1234)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn no_op_when_db_path_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.db");
        assert!(!clear_orphan_rows_if_needed(&path).unwrap());
    }

    #[test]
    fn no_op_when_schema_not_initialized() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.db");
        // Open + close to create empty file; no tables.
        Connection::open(&path).unwrap();
        assert!(!clear_orphan_rows_if_needed(&path).unwrap());
    }

    #[test]
    fn no_op_when_watermarks_present() {
        let (path, _dir) = fresh_db_with_minimal_schema();
        insert_watermark(&path);
        insert_token(&path, "0xtoken1");
        assert!(!clear_orphan_rows_if_needed(&path).unwrap());

        // Token row still there — we did NOT touch a healthy DB.
        let conn = Connection::open(&path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM erc20_tokens", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn no_op_when_truly_empty() {
        let (path, _dir) = fresh_db_with_minimal_schema();
        // No watermarks AND no orphan rows — fresh install case.
        assert!(!clear_orphan_rows_if_needed(&path).unwrap());
    }

    #[test]
    fn clears_orphans_when_watermark_empty_but_data_present() {
        let (path, _dir) = fresh_db_with_minimal_schema();
        insert_token(&path, "0xtoken1");
        insert_token(&path, "0xtoken2");

        let conn = Connection::open(&path).unwrap();
        let before: i64 = conn
            .query_row("SELECT COUNT(*) FROM erc20_tokens", [], |r| r.get(0))
            .unwrap();
        drop(conn);
        assert_eq!(before, 2);

        let cleared = clear_orphan_rows_if_needed(&path).unwrap();
        assert!(cleared, "should have cleared orphan rows");

        let conn = Connection::open(&path).unwrap();
        let after: i64 = conn
            .query_row("SELECT COUNT(*) FROM erc20_tokens", [], |r| r.get(0))
            .unwrap();
        assert_eq!(after, 0);
    }

    #[test]
    fn clears_orphans_across_multiple_tables() {
        let (path, _dir) = fresh_db_with_minimal_schema();
        insert_token(&path, "0xtoken1");
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO order_events (chain_id, orderbook_address, transaction_hash, log_index, block_number, block_timestamp, order_owner, order_nonce, order_hash, event_type) \
             VALUES (8453, '0xobid', '0xtx', 0, 100, 1700000000, '0xowner', '0x00', '0xhash', 'AddOrderV3')",
            [],
        )
        .unwrap();
        drop(conn);

        let cleared = clear_orphan_rows_if_needed(&path).unwrap();
        assert!(cleared);

        let conn = Connection::open(&path).unwrap();
        let tokens: i64 = conn
            .query_row("SELECT COUNT(*) FROM erc20_tokens", [], |r| r.get(0))
            .unwrap();
        let events: i64 = conn
            .query_row("SELECT COUNT(*) FROM order_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tokens, 0);
        assert_eq!(events, 0);
    }

    #[test]
    fn skips_tables_that_do_not_exist_in_schema() {
        // The minimal schema only has erc20_tokens + order_events. The
        // function should silently skip the rest of TABLES_TO_CLEAR rather
        // than fail with "no such table".
        let (path, _dir) = fresh_db_with_minimal_schema();
        insert_token(&path, "0xtoken1");
        let cleared = clear_orphan_rows_if_needed(&path).unwrap();
        assert!(cleared);
    }
}
