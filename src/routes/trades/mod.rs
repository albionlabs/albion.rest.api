pub(crate) mod get_by_address;
pub(crate) mod get_by_order_hashes;
pub(crate) mod get_by_taker;
pub(crate) mod get_by_token;
pub(crate) mod get_by_tx;

use crate::error::ApiError;
use crate::types::common::{Denomination, TokenRef};
use crate::types::trades::{
    TradeByAddress, TradesByAddressResponse, TradesPagination, TradesPaginationParams,
};
use crate::wrap_ratio::{
    persist_wrap_ratio_snapshots_best_effort, read_wrap_ratio_responses_for_addresses,
    wrap_ratio_values_from_responses, WrapRatioValue,
};
use alloy::primitives::{Address, B256};
use async_trait::async_trait;
use rain_orderbook_common::raindex_client::trades::{
    GetTradesByOrderHashesFilters, GetTradesFilters, GetTradesTokenFilter, OrderHashes,
    RaindexTrade, RaindexTradesByOrderHashResult, RaindexTradesListResult,
};
use rain_orderbook_common::raindex_client::types::{PaginationParams, TimeFilter};
use rain_orderbook_common::raindex_client::{RaindexClient, RaindexError};
use rocket::serde::json::Json;
use rocket::Route;
use std::collections::HashMap;

pub(crate) type TradeWrapRatioMap = HashMap<(Address, u64), WrapRatioValue>;

#[async_trait]
pub(crate) trait TradesDataSource: Send + Sync {
    async fn get_trades_by_tx(&self, tx_hash: B256) -> Result<RaindexTradesListResult, ApiError>;

    async fn get_trades_for_owner(
        &self,
        owner: Address,
        pagination: PaginationParams,
        time_filter: TimeFilter,
    ) -> Result<RaindexTradesListResult, ApiError>;

    async fn get_trades_for_token(
        &self,
        token: Address,
        page: u16,
        page_size: u16,
        time_filter: TimeFilter,
    ) -> Result<RaindexTradesListResult, ApiError>;

    async fn get_trades_for_taker(
        &self,
        taker: Address,
        page: u16,
        page_size: u16,
        time_filter: TimeFilter,
    ) -> Result<RaindexTradesListResult, ApiError>;

    async fn get_trades_by_order_hashes(
        &self,
        order_hashes: Vec<B256>,
        time_filter: TimeFilter,
    ) -> Result<RaindexTradesByOrderHashResult, ApiError>;

    async fn get_current_wrap_ratios_for_tokens(
        &self,
        _token_addresses: &[Address],
    ) -> Result<HashMap<Address, WrapRatioValue>, ApiError> {
        Ok(HashMap::new())
    }
}

pub(crate) struct RaindexTradesDataSource<'a> {
    pub client: &'a RaindexClient,
    pub pool: &'a crate::db::DbPool,
}

#[async_trait]
impl TradesDataSource for RaindexTradesDataSource<'_> {
    async fn get_trades_by_tx(&self, tx_hash: B256) -> Result<RaindexTradesListResult, ApiError> {
        self.client
            .get_trades_for_transaction(None, None, tx_hash)
            .await
            .map_err(|e| match e {
                RaindexError::TransactionIndexingTimeout { tx_hash, attempts } => {
                    ApiError::NotYetIndexed(format!(
                        "transaction {tx_hash:#x} not yet indexed after {attempts} attempts"
                    ))
                }
                other => {
                    tracing::error!(error = %other, "failed to query trades for transaction");
                    ApiError::Internal("failed to query trades".into())
                }
            })
    }

    async fn get_trades_for_owner(
        &self,
        owner: Address,
        pagination: PaginationParams,
        time_filter: TimeFilter,
    ) -> Result<RaindexTradesListResult, ApiError> {
        let filters = GetTradesFilters {
            owners: vec![owner],
            time_filter: Some(time_filter),
            ..Default::default()
        };

        self.client
            .get_trades(None, Some(filters), pagination.page, pagination.page_size)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query trades for owner");
                ApiError::Internal("failed to query trades".into())
            })
    }

    async fn get_trades_for_token(
        &self,
        token: Address,
        page: u16,
        page_size: u16,
        time_filter: TimeFilter,
    ) -> Result<RaindexTradesListResult, ApiError> {
        let filters = GetTradesFilters {
            tokens: Some(GetTradesTokenFilter {
                inputs: Some(vec![token]),
                outputs: Some(vec![token]),
            }),
            time_filter: Some(time_filter),
            ..Default::default()
        };

        self.client
            .get_trades(None, Some(filters), Some(page), Some(page_size))
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query trades for token");
                ApiError::Internal("failed to query trades".into())
            })
    }

    async fn get_trades_for_taker(
        &self,
        taker: Address,
        page: u16,
        page_size: u16,
        time_filter: TimeFilter,
    ) -> Result<RaindexTradesListResult, ApiError> {
        let filters = GetTradesFilters {
            takers: vec![taker],
            time_filter: Some(time_filter),
            ..Default::default()
        };

        self.client
            .get_trades(None, Some(filters), Some(page), Some(page_size))
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query trades for taker");
                ApiError::Internal("failed to query trades".into())
            })
    }

    async fn get_trades_by_order_hashes(
        &self,
        order_hashes: Vec<B256>,
        time_filter: TimeFilter,
    ) -> Result<RaindexTradesByOrderHashResult, ApiError> {
        let filters = GetTradesByOrderHashesFilters {
            time_filter: Some(time_filter),
            ..Default::default()
        };

        self.client
            .get_trades_by_order_hashes(None, OrderHashes(order_hashes), Some(filters))
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query trades by order hashes");
                ApiError::Internal("failed to query trades".into())
            })
    }

    async fn get_current_wrap_ratios_for_tokens(
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

pub(super) fn map_trade_for_list(
    trade: &RaindexTrade,
    denomination: Denomination,
    trade_wrap_ratios: &TradeWrapRatioMap,
) -> Result<TradeByAddress, ApiError> {
    let tx_hash = trade.transaction().id();
    let input_vc = trade.input_vault_balance_change();
    let output_vc = trade.output_vault_balance_change();

    let input_token_data = input_vc.token();
    let output_token_data = output_vc.token();

    let timestamp: u64 = trade.timestamp().try_into().map_err(|_| {
        tracing::error!("timestamp does not fit in u64");
        ApiError::Internal("timestamp overflow".into())
    })?;
    let block_number = trade_block_number(trade)?;
    let wrap_ratios = if denomination == Denomination::Unwrapped {
        wrap_ratio_map_for_trade(
            input_token_data.address(),
            output_token_data.address(),
            block_number,
            trade_wrap_ratios,
        )
    } else {
        HashMap::new()
    };
    let input_amount = if denomination == Denomination::Unwrapped {
        crate::denomination::convert_wrapped_amount_for_token(
            input_vc.formatted_amount(),
            input_token_data.address(),
            &wrap_ratios,
        )?
    } else {
        input_vc.formatted_amount()
    };
    let output_amount = if denomination == Denomination::Unwrapped {
        crate::denomination::convert_wrapped_amount_for_token(
            output_vc.formatted_amount(),
            output_token_data.address(),
            &wrap_ratios,
        )?
    } else {
        output_vc.formatted_amount()
    };

    Ok(TradeByAddress {
        tx_hash,
        sender: trade.transaction().from(),
        input_amount,
        output_amount,
        input_token: TokenRef {
            address: input_token_data.address(),
            symbol: input_token_data.symbol().unwrap_or_default(),
            decimals: input_token_data.decimals(),
        },
        output_token: TokenRef {
            address: output_token_data.address(),
            symbol: output_token_data.symbol().unwrap_or_default(),
            decimals: output_token_data.decimals(),
        },
        order_hash: Some(trade.order_hash()),
        timestamp,
        block_number,
    })
}

pub(super) async fn build_trades_list_response(
    ds: &dyn TradesDataSource,
    result: RaindexTradesListResult,
    page: u32,
    page_size: u32,
    denomination: Denomination,
) -> Result<Json<TradesByAddressResponse>, ApiError> {
    let trade_wrap_ratios =
        current_wrap_ratios_for_trades(ds, denomination, result.trades()).await?;
    let trades = result
        .trades()
        .iter()
        .map(|trade| map_trade_for_list(trade, denomination, &trade_wrap_ratios))
        .collect::<Result<Vec<_>, ApiError>>()?;

    let total_trades = result.total_count();
    let total_pages = if page_size > 0 {
        total_trades.div_ceil(u64::from(page_size))
    } else {
        0
    };
    let has_more = u64::from(page) < total_pages;

    Ok(Json(TradesByAddressResponse {
        trades,
        pagination: TradesPagination {
            page,
            page_size,
            total_trades,
            total_pages,
            has_more,
        },
    }))
}

pub(super) async fn current_wrap_ratios_for_trades(
    ds: &dyn TradesDataSource,
    denomination: Denomination,
    trades: &[RaindexTrade],
) -> Result<TradeWrapRatioMap, ApiError> {
    if denomination == Denomination::Wrapped || trades.is_empty() {
        return Ok(HashMap::new());
    }

    let mut token_addresses = Vec::new();
    for trade in trades {
        token_addresses.push(trade.input_vault_balance_change().token().address());
        token_addresses.push(trade.output_vault_balance_change().token().address());
    }
    token_addresses.sort_unstable();
    token_addresses.dedup();

    let current_ratios = ds
        .get_current_wrap_ratios_for_tokens(&token_addresses)
        .await?;
    let mut ratios = HashMap::new();

    for trade in trades {
        let block_number = trade_block_number(trade)?;
        for token in [
            trade.input_vault_balance_change().token().address(),
            trade.output_vault_balance_change().token().address(),
        ] {
            if let Some(ratio) = current_ratios.get(&token) {
                ratios.insert((token, block_number), ratio.clone());
            }
        }
    }

    Ok(ratios)
}

pub(super) fn wrap_ratio_map_for_trade(
    input_token: Address,
    output_token: Address,
    block_number: u64,
    trade_wrap_ratios: &TradeWrapRatioMap,
) -> crate::denomination::WrapRatioMap {
    let mut ratios = HashMap::new();
    if let Some(ratio) = trade_wrap_ratios.get(&(input_token, block_number)) {
        ratios.insert(input_token, ratio.clone());
    }
    if let Some(ratio) = trade_wrap_ratios.get(&(output_token, block_number)) {
        ratios.insert(output_token, ratio.clone());
    }
    ratios
}

pub(super) fn trade_block_number(trade: &RaindexTrade) -> Result<u64, ApiError> {
    trade.transaction().block_number().try_into().map_err(|_| {
        tracing::error!("block number does not fit in u64");
        ApiError::Internal("block number overflow".into())
    })
}

pub(super) fn trades_pagination_params(
    params: TradesPaginationParams,
) -> Result<(u32, u32, u16, u16, TimeFilter), ApiError> {
    let page = params.page.unwrap_or(1);
    let page_size = params.page_size.unwrap_or(20);

    let sdk_page = page
        .try_into()
        .map_err(|_| ApiError::BadRequest("page value too large".into()))?;
    let sdk_page_size = page_size
        .try_into()
        .map_err(|_| ApiError::BadRequest("page_size value too large".into()))?;
    let time_filter = TimeFilter {
        start: params.start_time,
        end: params.end_time,
    };

    Ok((page, page_size, sdk_page, sdk_page_size, time_filter))
}

pub fn routes() -> Vec<Route> {
    rocket::routes![
        get_by_tx::get_trades_by_tx,
        get_by_order_hashes::get_trades_by_order_hashes,
        get_by_token::get_trades_by_token,
        get_by_taker::get_trades_by_taker,
        get_by_address::get_trades_by_address
    ]
}
