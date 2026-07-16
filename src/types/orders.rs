use crate::types::common::{Denomination, TokenRef};
use alloy::primitives::{Address, Bytes, FixedBytes};
use rocket::form::{FromForm, FromFormField};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Debug, Clone, FromForm, Serialize, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct OrdersPaginationParams {
    #[field(name = "state")]
    pub state: Option<OrderState>,
    #[field(name = "page")]
    #[param(example = 1)]
    pub page: Option<u16>,
    #[field(name = "pageSize")]
    #[param(example = 20)]
    pub page_size: Option<u16>,
    #[field(name = "denomination")]
    #[param(example = "wrapped")]
    pub denomination: Option<Denomination>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromFormField, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum OrderSide {
    #[field(value = "input")]
    Input,
    #[field(value = "output")]
    Output,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, FromFormField, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OrderState {
    #[field(value = "active")]
    Active,
    #[field(value = "inactive")]
    Inactive,
    #[field(value = "all")]
    All,
}

// Order summaries expose the same `limit` / `strategy` taxonomy as order
// detail (see `crate::types::order::OrderType`). Re-exported here so callers of
// this module keep a stable name.
pub use crate::types::order::OrderType as OrderSummaryOrderType;

#[derive(Debug, Clone, FromForm, Serialize, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct OrdersByTokenParams {
    #[field(name = "state")]
    pub state: Option<OrderState>,
    #[field(name = "side")]
    pub side: Option<OrderSide>,
    #[field(name = "page")]
    #[param(example = 1)]
    pub page: Option<u16>,
    #[field(name = "pageSize")]
    #[param(example = 20)]
    pub page_size: Option<u16>,
    #[field(name = "denomination")]
    #[param(example = "wrapped")]
    pub denomination: Option<Denomination>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrderSummary {
    #[schema(value_type = String, example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub order_hash: FixedBytes<32>,
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub owner: Address,
    #[schema(example = 8453)]
    pub chain_id: u32,
    #[schema(value_type = String, example = "0x01")]
    pub order_bytes: Bytes,
    #[schema(example = true)]
    pub active: bool,
    #[schema(example = 1718452900)]
    pub removed_at: Option<u64>,
    #[schema(example = "limit")]
    pub order_type: OrderSummaryOrderType,
    pub input_token: TokenRef,
    pub output_token: TokenRef,
    #[schema(example = "500000")]
    pub output_vault_balance: String,
    #[schema(example = "500000")]
    pub max_output: Option<String>,
    #[schema(example = "0.0005")]
    pub io_ratio: String,
    #[schema(example = 1718452800)]
    pub created_at: u64,
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub orderbook_id: Address,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrdersPagination {
    #[schema(example = 1)]
    pub page: u32,
    #[schema(example = 20)]
    pub page_size: u32,
    #[schema(example = 100)]
    pub total_orders: u64,
    #[schema(example = 5)]
    pub total_pages: u64,
    #[schema(example = true)]
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrdersListResponse {
    pub orders: Vec<OrderSummary>,
    pub pagination: OrdersPagination,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrderByTxEntry {
    #[schema(value_type = String, example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub order_hash: FixedBytes<32>,
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub owner: Address,
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub orderbook_id: Address,
    pub input_token: TokenRef,
    pub output_token: TokenRef,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrdersByTxResponse {
    #[schema(value_type = String, example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub tx_hash: FixedBytes<32>,
    #[schema(example = 12345678)]
    pub block_number: u64,
    #[schema(example = 1718452800)]
    pub timestamp: u64,
    pub orders: Vec<OrderByTxEntry>,
}
