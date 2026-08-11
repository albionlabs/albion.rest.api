use crate::types::common::{Denomination, TokenRef};
use alloy::primitives::{Address, FixedBytes, B256};
use rocket::form::FromForm;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Debug, Clone, FromForm, Serialize, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct TradesPaginationParams {
    #[field(name = "page")]
    #[param(example = 1)]
    pub page: Option<u32>,
    #[field(name = "pageSize")]
    #[param(example = 20)]
    pub page_size: Option<u32>,
    #[field(name = "startTime")]
    #[param(example = 1718452800)]
    pub start_time: Option<u64>,
    #[field(name = "endTime")]
    #[param(example = 1718539200)]
    pub end_time: Option<u64>,
    #[field(name = "denomination")]
    #[param(example = "wrapped")]
    pub denomination: Option<Denomination>,
}

#[derive(Debug, Clone, FromForm, Serialize, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct TradesByTxParams {
    #[field(name = "denomination")]
    #[param(example = "wrapped")]
    pub denomination: Option<Denomination>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradeByAddress {
    #[schema(value_type = String, example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub tx_hash: FixedBytes<32>,
    #[schema(value_type = String, example = "0x0000000000000000000000000000000000000001")]
    pub sender: Address,
    #[schema(example = "1000000")]
    pub input_amount: String,
    #[schema(example = "500000")]
    pub output_amount: String,
    pub input_token: TokenRef,
    pub output_token: TokenRef,
    #[schema(value_type = Option<String>)]
    pub order_hash: Option<FixedBytes<32>>,
    #[schema(example = 1718452800)]
    pub timestamp: u64,
    #[schema(example = 12345678)]
    pub block_number: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradesPagination {
    #[schema(example = 1)]
    pub page: u32,
    #[schema(example = 20)]
    pub page_size: u32,
    #[schema(example = 100)]
    pub total_trades: u64,
    #[schema(example = 5)]
    pub total_pages: u64,
    #[schema(example = true)]
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradesByAddressResponse {
    pub trades: Vec<TradeByAddress>,
    pub pagination: TradesPagination,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradesByOrderHashesRequest {
    #[schema(
        value_type = Vec<String>,
        example = json!(["0x000000000000000000000000000000000000000000000000000000000000abcd"])
    )]
    pub order_hashes: Vec<String>,
    #[schema(example = 1718452800)]
    pub start_time: Option<u64>,
    #[schema(example = 1718539200)]
    pub end_time: Option<u64>,
    #[schema(example = "wrapped")]
    pub denomination: Option<Denomination>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradesByOrderHashEntry {
    #[schema(value_type = String, example = "0x000000000000000000000000000000000000000000000000000000000000abcd")]
    pub order_hash: B256,
    pub trades: Vec<TradeByAddress>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradesByOrderHashesResponse {
    pub trades_by_order_hash: Vec<TradesByOrderHashEntry>,
    #[schema(example = 3)]
    pub total_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradeRequest {
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: Address,
    #[schema(value_type = String, example = "0x4200000000000000000000000000000000000006")]
    pub output_token: Address,
    #[schema(example = "1000000")]
    pub maximum_input: String,
    #[schema(example = "0.0006")]
    pub maximum_io_ratio: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradeResult {
    #[schema(example = "900000")]
    pub input_amount: String,
    #[schema(example = "500000")]
    pub output_amount: String,
    #[schema(example = "0.00055")]
    pub actual_io_ratio: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradeByTxEntry {
    #[schema(value_type = String, example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub order_hash: FixedBytes<32>,
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub order_owner: Address,
    pub request: TradeRequest,
    pub result: TradeResult,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradesTotals {
    #[schema(example = "900000")]
    pub total_input_amount: String,
    #[schema(example = "500000")]
    pub total_output_amount: String,
    #[schema(example = "0.00055")]
    pub average_io_ratio: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradesByTxResponse {
    #[schema(value_type = String, example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub tx_hash: FixedBytes<32>,
    #[schema(example = 12345678)]
    pub block_number: u64,
    #[schema(example = 1718452800)]
    pub timestamp: u64,
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub sender: Address,
    pub trades: Vec<TradeByTxEntry>,
    pub totals: TradesTotals,
}
