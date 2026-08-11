mod calldata;
#[cfg(test)]
mod calldata_fork_tests;
mod denomination;
mod quote;

use crate::cache::RouteResponseCaches;
use crate::db::DbPool;
use crate::error::ApiError;
use crate::types::swap::{SwapCalldataResponse, SwapDenomination};
use crate::wrap_ratio::{
    persist_wrap_ratio_snapshots_best_effort, read_wrap_ratio_responses_for_addresses,
    wrap_ratio_values_from_responses, WrapRatioValue,
};
use alloy::primitives::Address;
use async_trait::async_trait;
use rain_orderbook_common::raindex_client::orders::{
    GetOrdersFilters, GetOrdersTokenFilter, RaindexOrder,
};
use rain_orderbook_common::raindex_client::take_orders::TakeOrdersRequest;
use rain_orderbook_common::raindex_client::RaindexClient;
use rain_orderbook_common::raindex_client::RaindexError;
use rain_orderbook_common::take_orders::{
    build_take_order_candidates_for_pair, NoopInjector, TakeOrderCandidate,
};
use rocket::Route;
use std::collections::HashMap;

#[async_trait]
pub(crate) trait SwapDataSource: Send + Sync {
    async fn validate_supported_tokens(
        &self,
        input_token: Address,
        output_token: Address,
    ) -> Result<(), ApiError>;

    async fn get_orders_for_pair(
        &self,
        input_token: Address,
        output_token: Address,
    ) -> Result<Vec<RaindexOrder>, ApiError>;

    async fn build_candidates_for_pair(
        &self,
        orders: &[RaindexOrder],
        input_token: Address,
        output_token: Address,
    ) -> Result<Vec<TakeOrderCandidate>, ApiError>;

    async fn get_calldata(
        &self,
        request: TakeOrdersRequest,
    ) -> Result<SwapCalldataResponse, ApiError>;

    async fn get_wrap_ratios_for_tokens(
        &self,
        _token_addresses: &[Address],
    ) -> Result<HashMap<Address, WrapRatioValue>, ApiError> {
        Ok(HashMap::new())
    }
}

pub(crate) struct RaindexSwapDataSource<'a> {
    pub client: &'a RaindexClient,
    pub caches: &'a RouteResponseCaches,
    pub pool: &'a DbPool,
}

fn swap_candidates_cache_key(
    orders: &[RaindexOrder],
    input_token: Address,
    output_token: Address,
) -> String {
    let mut order_keys = orders
        .iter()
        .map(|order| {
            format!(
                "{}:{}:{}",
                order.chain_id(),
                order.raindex(),
                order.order_hash()
            )
        })
        .collect::<Vec<_>>();
    order_keys.sort_unstable();
    let order_keys = order_keys.join(",");
    format!("swap-candidates/latest/default/{input_token}/{output_token}/{order_keys}")
}

#[async_trait]
impl<'a> SwapDataSource for RaindexSwapDataSource<'a> {
    async fn validate_supported_tokens(
        &self,
        input_token: Address,
        output_token: Address,
    ) -> Result<(), ApiError> {
        let tokens = self.client.get_all_tokens().map_err(|e| {
            tracing::error!(error = %e, "failed to retrieve curated tokens");
            ApiError::Internal("failed to retrieve curated tokens".into())
        })?;

        let input_supported = tokens.values().any(|token| token.address == input_token);
        let output_supported = tokens.values().any(|token| token.address == output_token);

        if input_supported && output_supported {
            tracing::info!(input_token = %input_token, output_token = %output_token, "validated supported swap tokens");
            return Ok(());
        }

        tracing::warn!(
            input_token = %input_token,
            output_token = %output_token,
            input_supported,
            output_supported,
            "swap request rejected for unsupported curated tokens"
        );
        Err(ApiError::BadRequest(
            "unsupported token for this API".into(),
        ))
    }

    async fn get_orders_for_pair(
        &self,
        input_token: Address,
        output_token: Address,
    ) -> Result<Vec<RaindexOrder>, ApiError> {
        let filters = GetOrdersFilters {
            active: Some(true),
            tokens: Some(GetOrdersTokenFilter {
                inputs: Some(vec![input_token]),
                outputs: Some(vec![output_token]),
            }),
            has_positive_output_vault_balance: Some(true),
            ..Default::default()
        };
        self.client
            .get_orders(None, Some(filters), None, None)
            .await
            .map(|r| r.orders().to_vec())
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query orders for pair");
                ApiError::Internal("failed to query orders".into())
            })
    }

    async fn build_candidates_for_pair(
        &self,
        orders: &[RaindexOrder],
        input_token: Address,
        output_token: Address,
    ) -> Result<Vec<TakeOrderCandidate>, ApiError> {
        let fetch = || async {
            build_take_order_candidates_for_pair(
                orders,
                input_token,
                output_token,
                None,
                None,
                Address::ZERO,
                &NoopInjector,
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to build order candidates");
                ApiError::Internal("failed to build order candidates".into())
            })
        };

        if !self.caches.is_enabled() {
            return fetch().await;
        }

        self.caches
            .swap_candidates
            .get_or_try_insert(
                swap_candidates_cache_key(orders, input_token, output_token),
                fetch,
            )
            .await
            .map_err(|e| (*e).clone())
    }

    async fn get_calldata(
        &self,
        request: TakeOrdersRequest,
    ) -> Result<SwapCalldataResponse, ApiError> {
        let result = self
            .client
            .get_take_orders_calldata(request)
            .await
            .map_err(map_raindex_error)?;

        if let Some(approval_info) = result.approval_info() {
            let formatted_amount = approval_info.formatted_amount().to_string();
            Ok(SwapCalldataResponse {
                to: approval_info.spender(),
                data: alloy::primitives::Bytes::new(),
                value: alloy::primitives::U256::ZERO,
                estimated_input: formatted_amount.clone(),
                denomination: SwapDenomination::Wrapped,
                approvals: vec![crate::types::common::Approval {
                    token: approval_info.token(),
                    spender: approval_info.spender(),
                    amount: formatted_amount,
                    symbol: String::new(),
                    approval_data: approval_info.calldata().clone(),
                }],
            })
        } else if let Some(take_orders_info) = result.take_orders_info() {
            let expected_sell = take_orders_info.expected_sell().format().map_err(|e| {
                tracing::error!(error = %e, "failed to format expected sell");
                ApiError::Internal("failed to format expected sell".into())
            })?;
            Ok(SwapCalldataResponse {
                to: take_orders_info.raindex(),
                data: take_orders_info.calldata().clone(),
                value: alloy::primitives::U256::ZERO,
                estimated_input: expected_sell,
                denomination: SwapDenomination::Wrapped,
                approvals: vec![],
            })
        } else {
            Err(ApiError::Internal(
                "unexpected calldata result state".into(),
            ))
        }
    }

    async fn get_wrap_ratios_for_tokens(
        &self,
        token_addresses: &[Address],
    ) -> Result<HashMap<Address, WrapRatioValue>, ApiError> {
        let tokens: Vec<_> = self
            .client
            .get_all_tokens()
            .map_err(|e| {
                tracing::error!(error = %e, "failed to retrieve curated tokens");
                ApiError::Internal("failed to retrieve curated tokens".into())
            })?
            .into_values()
            .collect();

        let responses = read_wrap_ratio_responses_for_addresses(&tokens, token_addresses).await?;
        persist_wrap_ratio_snapshots_best_effort(self.pool, &responses).await;
        Ok(wrap_ratio_values_from_responses(responses))
    }
}

fn map_raindex_error(e: RaindexError) -> ApiError {
    match &e {
        RaindexError::NoLiquidity | RaindexError::InsufficientLiquidity { .. } => {
            tracing::warn!(error = %e, "no liquidity found");
            ApiError::NotFound("no liquidity found for this pair".into())
        }
        RaindexError::SameTokenPair
        | RaindexError::NonPositiveAmount
        | RaindexError::NegativePriceCap
        | RaindexError::FromHexError(_)
        | RaindexError::Float(_) => {
            tracing::warn!(error = %e, "invalid request parameters");
            ApiError::BadRequest(e.to_string())
        }
        RaindexError::PreflightError(_) => {
            tracing::warn!(error = %e, "preflight simulation failed");
            ApiError::BadRequest(e.to_readable_msg())
        }
        _ => {
            tracing::error!(error = %e, "calldata generation failed");
            ApiError::Internal("failed to generate calldata".into())
        }
    }
}

pub use calldata::*;
pub use quote::*;

pub fn routes() -> Vec<Route> {
    rocket::routes![quote::post_swap_quote, calldata::post_swap_calldata]
}

pub fn routes_v2() -> Vec<Route> {
    rocket::routes![calldata::post_swap_calldata_v2]
}

#[cfg(test)]
mod tests {
    use super::swap_candidates_cache_key;
    use alloy::primitives::address;
    use rain_orderbook_common::raindex_client::orders::RaindexOrder;
    use serde_json::json;

    fn mock_order(chain_id: u32, order_hash: &str) -> RaindexOrder {
        let mut value = crate::test_helpers::order_json();
        value["chainId"] = json!(chain_id);
        value["orderHash"] = json!(order_hash);
        serde_json::from_value(value).expect("deserialize mock order")
    }

    #[test]
    fn test_swap_candidates_cache_key_is_order_insensitive() {
        let input_token = address!("833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
        let output_token = address!("4200000000000000000000000000000000000006");
        let order_a = mock_order(
            8453,
            "0x0000000000000000000000000000000000000000000000000000000000000001",
        );
        let order_b = mock_order(
            8453,
            "0x0000000000000000000000000000000000000000000000000000000000000002",
        );

        assert_eq!(
            swap_candidates_cache_key(
                &[order_a.clone(), order_b.clone()],
                input_token,
                output_token,
            ),
            swap_candidates_cache_key(&[order_b, order_a], input_token, output_token)
        );
    }
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use super::SwapDataSource;
    use crate::error::ApiError;
    use crate::types::swap::SwapCalldataResponse;
    use alloy::primitives::Address;
    use async_trait::async_trait;
    use rain_orderbook_common::raindex_client::orders::RaindexOrder;
    use rain_orderbook_common::raindex_client::take_orders::TakeOrdersRequest;
    use rain_orderbook_common::take_orders::TakeOrderCandidate;

    pub struct MockSwapDataSource {
        pub supported_tokens: Result<(), ApiError>,
        pub orders: Result<Vec<RaindexOrder>, ApiError>,
        pub candidates: Vec<TakeOrderCandidate>,
        pub calldata_result: Result<SwapCalldataResponse, ApiError>,
    }

    #[async_trait]
    impl SwapDataSource for MockSwapDataSource {
        async fn validate_supported_tokens(
            &self,
            _input_token: Address,
            _output_token: Address,
        ) -> Result<(), ApiError> {
            self.supported_tokens.clone()
        }

        async fn get_orders_for_pair(
            &self,
            _input_token: Address,
            _output_token: Address,
        ) -> Result<Vec<RaindexOrder>, ApiError> {
            match &self.orders {
                Ok(orders) => Ok(orders.clone()),
                Err(_) => Err(ApiError::Internal("failed to query orders".into())),
            }
        }

        async fn build_candidates_for_pair(
            &self,
            _orders: &[RaindexOrder],
            _input_token: Address,
            _output_token: Address,
        ) -> Result<Vec<TakeOrderCandidate>, ApiError> {
            Ok(self.candidates.clone())
        }

        async fn get_calldata(
            &self,
            _request: TakeOrdersRequest,
        ) -> Result<SwapCalldataResponse, ApiError> {
            self.calldata_result.clone()
        }
    }
}
