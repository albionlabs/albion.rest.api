use crate::types::common::{Approval, Denomination, TokenRef};
use alloy::primitives::{Address, Bytes, FixedBytes, U256};
use rocket::form::FromForm;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PeriodUnit {
    Days,
    Hours,
    Minutes,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeployDcaOrderRequest {
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: Address,
    #[schema(value_type = String, example = "0x4200000000000000000000000000000000000006")]
    pub output_token: Address,
    #[schema(example = "1000000")]
    pub budget_amount: String,
    #[schema(example = 4)]
    pub period: u32,
    #[schema(example = "hours")]
    pub period_unit: PeriodUnit,
    #[schema(example = "0.0005")]
    pub start_io: String,
    #[schema(example = "0.0003")]
    pub floor_io: String,
    #[schema(value_type = Option<String>)]
    pub input_vault_id: Option<U256>,
    #[schema(value_type = Option<String>)]
    pub output_vault_id: Option<U256>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploySolverOrderRequest {
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: Address,
    #[schema(value_type = String, example = "0x4200000000000000000000000000000000000006")]
    pub output_token: Address,
    #[schema(example = "1000000")]
    pub amount: String,
    #[schema(example = "0.0005")]
    pub io_ratio: String,
    #[schema(value_type = Option<String>)]
    pub input_vault_id: Option<U256>,
    #[schema(value_type = Option<String>)]
    pub output_vault_id: Option<U256>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeployOrderResponse {
    #[schema(value_type = String, example = "0xDEF171Fe48CF0115B1d80b88dc8eAB59176FEe57")]
    pub to: Address,
    #[schema(value_type = String, example = "0xabcdef...")]
    pub data: Bytes,
    #[schema(value_type = String, example = "0x0")]
    pub value: U256,
    pub approvals: Vec<Approval>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CancelOrderRequest {
    #[schema(value_type = String, example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub order_hash: FixedBytes<32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CancelTransaction {
    #[schema(value_type = String, example = "0xDEF171Fe48CF0115B1d80b88dc8eAB59176FEe57")]
    pub to: Address,
    #[schema(value_type = String, example = "0xabcdef...")]
    pub data: Bytes,
    #[schema(value_type = String, example = "0x0")]
    pub value: U256,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenReturn {
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub token: Address,
    #[schema(example = "USDC")]
    pub symbol: String,
    #[schema(example = "1000000")]
    pub amount: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CancelSummary {
    #[schema(example = 2)]
    pub vaults_to_withdraw: u32,
    pub tokens_returned: Vec<TokenReturn>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CancelOrderResponse {
    pub transactions: Vec<CancelTransaction>,
    pub summary: CancelSummary,
}

#[derive(Debug, Clone, FromForm, Serialize, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct OrderDetailParams {
    #[field(name = "denomination")]
    #[param(example = "wrapped")]
    pub denomination: Option<Denomination>,
}

/// Externally visible order taxonomy. The albion.dex client expects
/// `z.enum(['limit', 'strategy'])`, so this is the only order-type surface the
/// API serializes. `determine_order_type` (see `routes::order`) classifies an
/// order into one of these two variants from its rainlang/bytecode.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OrderType {
    Limit,
    Strategy,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrderDetailsInfo {
    #[serde(rename = "type")]
    #[schema(example = "strategy")]
    pub type_: OrderType,
    #[schema(example = "0.0005")]
    pub io_ratio: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrderTradeEntry {
    #[schema(example = "trade-1")]
    pub id: String,
    #[schema(value_type = String, example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub tx_hash: FixedBytes<32>,
    #[schema(example = "1000000")]
    pub input_amount: String,
    #[schema(example = "500000")]
    pub output_amount: String,
    #[schema(example = 1718452800)]
    pub timestamp: u64,
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub sender: Address,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrderDetail {
    #[schema(value_type = String, example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub order_hash: FixedBytes<32>,
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub owner: Address,
    pub order_details: OrderDetailsInfo,
    pub input_token: TokenRef,
    pub output_token: TokenRef,
    #[schema(value_type = String, example = "0x1")]
    pub input_vault_id: U256,
    #[schema(value_type = String, example = "0x2")]
    pub output_vault_id: U256,
    #[schema(example = "1000000")]
    pub input_vault_balance: String,
    #[schema(example = "500000")]
    pub output_vault_balance: String,
    #[schema(example = "0.0005")]
    pub io_ratio: String,
    #[schema(example = 1718452800)]
    pub created_at: u64,
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub orderbook_id: Address,
    pub trades: Vec<OrderTradeEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_period_unit_variants() {
        let variants = [
            ("\"minutes\"", PeriodUnit::Minutes),
            ("\"hours\"", PeriodUnit::Hours),
            ("\"days\"", PeriodUnit::Days),
        ];
        for (json, expected) in variants {
            let parsed: PeriodUnit = serde_json::from_str(json).unwrap();
            assert_eq!(parsed, expected);
        }
    }

    #[test]
    fn test_period_unit_rejects_invalid() {
        let result = serde_json::from_str::<PeriodUnit>("\"seconds\"");
        assert!(result.is_err());
        let result = serde_json::from_str::<PeriodUnit>("\"weeks\"");
        assert!(result.is_err());
        let result = serde_json::from_str::<PeriodUnit>("\"months\"");
        assert!(result.is_err());
    }

    #[test]
    fn test_order_details_info_type_rename() {
        let info = OrderDetailsInfo {
            type_: OrderType::Strategy,
            io_ratio: "0.0005".into(),
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"type\":\"strategy\""));
        assert!(!json.contains("\"type_\""));
    }

    #[test]
    fn test_order_type_serializes_limit_and_strategy() {
        assert_eq!(
            serde_json::to_string(&OrderType::Limit).unwrap(),
            "\"limit\""
        );
        assert_eq!(
            serde_json::to_string(&OrderType::Strategy).unwrap(),
            "\"strategy\""
        );
    }
}
