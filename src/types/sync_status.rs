use alloy::primitives::Address;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SyncStatusEntry {
    /// EIP-155 chain id of the synced orderbook.
    #[schema(example = 8453)]
    pub chain_id: u32,
    /// Orderbook contract address (lowercase 0x-prefixed hex).
    #[schema(value_type = String, example = "0xe522cb4a5fcb2eb31a52ff41a4653d85a4fd7c9d")]
    pub orderbook_address: Address,
    /// Highest block whose events have been ingested into the local DB.
    #[schema(example = 45259447)]
    pub last_synced_block: u64,
    /// Block timestamp (seconds since epoch) of `last_synced_block`. The
    /// upstream `target_watermarks.updated_at` field is in milliseconds; we
    /// expose seconds here for parity with other API timestamp fields.
    #[schema(example = 1777308251)]
    pub last_synced_block_timestamp: u64,
    /// `now - last_synced_block_timestamp`. The headline staleness metric:
    /// alert when this exceeds whatever your dashboards consider tolerable
    /// (the threshold the API itself uses lives in `SyncStatusResponse`).
    #[schema(example = 12)]
    pub seconds_behind_chain: u64,
    /// True iff `seconds_behind_chain <= threshold_seconds`. The aggregate
    /// `status` field on the response is `"fresh"` only if every entry's
    /// `fresh` is true.
    #[schema(example = true)]
    pub fresh: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SyncStatusResponse {
    /// `"fresh"` iff every chain is within the threshold; otherwise `"stale"`.
    /// Mirrored by the HTTP status code (200 vs 503) so a `curl -f` succeeds
    /// for healthy state and fails for stale.
    #[schema(example = "fresh")]
    pub status: String,
    /// Threshold the API used to decide freshness. Configured via
    /// `sync_freshness_threshold_seconds` in the TOML config.
    #[schema(example = 300)]
    pub threshold_seconds: u64,
    /// Per-chain sync state. Empty if no orderbooks are configured or the
    /// `target_watermarks` table is empty (which itself is reported as stale).
    pub chains: Vec<SyncStatusEntry>,
}
