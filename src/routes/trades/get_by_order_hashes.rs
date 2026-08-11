use super::{
    current_wrap_ratios_for_trades, map_trade_for_list, RaindexTradesDataSource, TradesDataSource,
};
use crate::auth::AuthenticatedKey;
use crate::db::DbPool;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::common::Denomination;
use crate::types::trades::{
    TradesByOrderHashEntry, TradesByOrderHashesRequest, TradesByOrderHashesResponse,
};
use alloy::primitives::B256;
use rain_orderbook_common::raindex_client::trades::RaindexTradesByOrderHashResult;
use rain_orderbook_common::raindex_client::types::TimeFilter;
use rocket::serde::json::Json;
use rocket::State;
use std::str::FromStr;
use tracing::Instrument;

#[utoipa::path(
    post,
    path = "/v1/trades/query",
    tag = "Trades",
    security(("basicAuth" = [])),
    request_body = TradesByOrderHashesRequest,
    responses(
        (status = 200, description = "Trades grouped by requested order hash", body = TradesByOrderHashesResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/query", data = "<request>")]
pub async fn get_trades_by_order_hashes(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    pool: &State<DbPool>,
    span: TracingSpan,
    request: Json<TradesByOrderHashesRequest>,
) -> Result<Json<TradesByOrderHashesResponse>, ApiError> {
    async move {
        let request = request.into_inner();
        tracing::info!(
            order_hashes_count = request.order_hashes.len(),
            start_time = request.start_time,
            end_time = request.end_time,
            "request received"
        );
        let client = {
            let raindex = shared_raindex.read().await;
            raindex.client().clone()
        };
        let ds = RaindexTradesDataSource {
            client: &client,
            pool: pool.inner(),
        };
        process_get_trades_by_order_hashes(&ds, request).await
    }
    .instrument(span.0)
    .await
}

pub(super) async fn process_get_trades_by_order_hashes(
    ds: &dyn TradesDataSource,
    request: TradesByOrderHashesRequest,
) -> Result<Json<TradesByOrderHashesResponse>, ApiError> {
    let order_hashes = parse_order_hashes(&request.order_hashes)?;
    let time_filter = TimeFilter {
        start: request.start_time,
        end: request.end_time,
    };
    let denomination = request.denomination.unwrap_or_default();

    tracing::info!(
        order_hashes_count = order_hashes.len(),
        "querying trades by order hashes"
    );
    let result = ds
        .get_trades_by_order_hashes(order_hashes, time_filter)
        .await?;

    build_trades_by_order_hashes_response(ds, result, denomination).await
}

fn parse_order_hashes(order_hashes: &[String]) -> Result<Vec<B256>, ApiError> {
    order_hashes
        .iter()
        .map(|hash| {
            B256::from_str(hash).map_err(|e| {
                tracing::warn!(input = %hash, error = %e, "invalid order hash");
                ApiError::BadRequest("invalid order hash".into())
            })
        })
        .collect()
}

async fn build_trades_by_order_hashes_response(
    ds: &dyn TradesDataSource,
    result: RaindexTradesByOrderHashResult,
    denomination: Denomination,
) -> Result<Json<TradesByOrderHashesResponse>, ApiError> {
    let all_trades = result
        .trades_by_order_hash()
        .iter()
        .flat_map(|entry| entry.trades().iter().cloned())
        .collect::<Vec<_>>();
    let trade_wrap_ratios = current_wrap_ratios_for_trades(ds, denomination, &all_trades).await?;
    let trades_by_order_hash = result
        .trades_by_order_hash()
        .iter()
        .map(|entry| {
            let trades = entry
                .trades()
                .iter()
                .map(|trade| map_trade_for_list(trade, denomination, &trade_wrap_ratios))
                .collect::<Result<Vec<_>, ApiError>>()?;
            Ok(TradesByOrderHashEntry {
                order_hash: entry.order_hash(),
                trades,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;

    Ok(Json(TradesByOrderHashesResponse {
        trades_by_order_hash,
        total_count: result.total_count(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::order::test_fixtures::trade_json;
    use crate::test_helpers::{basic_auth_header, seed_api_key, TestClientBuilder};
    use alloy::primitives::{address, b256, Address};
    use async_trait::async_trait;
    use rain_orderbook_common::raindex_client::trades::RaindexTradesListResult;
    use rain_orderbook_common::raindex_client::types::{PaginationParams, TimeFilter};
    use rocket::http::{ContentType, Header, Status};
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone)]
    struct CapturedOrderHashesQuery {
        order_hashes: Vec<B256>,
        time_filter: TimeFilter,
    }

    struct MockTradesDataSource {
        result: Result<RaindexTradesByOrderHashResult, ApiError>,
        captured: Arc<Mutex<Option<CapturedOrderHashesQuery>>>,
    }

    #[async_trait]
    impl TradesDataSource for MockTradesDataSource {
        async fn get_trades_by_tx(
            &self,
            _tx_hash: B256,
        ) -> Result<RaindexTradesListResult, ApiError> {
            unimplemented!()
        }

        async fn get_trades_for_owner(
            &self,
            _owner: Address,
            _pagination: PaginationParams,
            _time_filter: TimeFilter,
        ) -> Result<RaindexTradesListResult, ApiError> {
            unimplemented!()
        }

        async fn get_trades_for_token(
            &self,
            _token: Address,
            _page: u16,
            _page_size: u16,
            _time_filter: TimeFilter,
        ) -> Result<RaindexTradesListResult, ApiError> {
            unimplemented!()
        }

        async fn get_trades_for_taker(
            &self,
            _taker: Address,
            _page: u16,
            _page_size: u16,
            _time_filter: TimeFilter,
        ) -> Result<RaindexTradesListResult, ApiError> {
            unimplemented!()
        }

        async fn get_trades_by_order_hashes(
            &self,
            order_hashes: Vec<B256>,
            time_filter: TimeFilter,
        ) -> Result<RaindexTradesByOrderHashResult, ApiError> {
            *self.captured.lock().unwrap() = Some(CapturedOrderHashesQuery {
                order_hashes,
                time_filter,
            });
            match &self.result {
                Ok(r) => Ok(r.clone()),
                Err(e) => Err(e.clone()),
            }
        }
    }

    fn hash_a() -> B256 {
        b256!("0x000000000000000000000000000000000000000000000000000000000000abcd")
    }

    fn hash_b() -> B256 {
        b256!("0x000000000000000000000000000000000000000000000000000000000000beef")
    }

    fn mock_grouped_result() -> RaindexTradesByOrderHashResult {
        serde_json::from_value(serde_json::json!({
            "tradesByOrderHash": [
                {
                    "orderHash": hash_a(),
                    "trades": [trade_json()]
                },
                {
                    "orderHash": hash_b(),
                    "trades": []
                }
            ],
            "totalCount": 1
        }))
        .unwrap()
    }

    #[rocket::async_test]
    async fn test_process_success_groups_results_and_preserves_empty_hashes() {
        let captured = Arc::new(Mutex::new(None));
        let ds = MockTradesDataSource {
            result: Ok(mock_grouped_result()),
            captured: Arc::clone(&captured),
        };
        let request = TradesByOrderHashesRequest {
            order_hashes: vec![hash_a().to_string(), hash_b().to_string()],
            start_time: Some(1700000000),
            end_time: Some(1700002000),
            denomination: None,
        };
        let result = process_get_trades_by_order_hashes(&ds, request)
            .await
            .unwrap();

        let response = result.into_inner();
        assert_eq!(response.total_count, 1);
        assert_eq!(response.trades_by_order_hash.len(), 2);
        assert_eq!(response.trades_by_order_hash[0].order_hash, hash_a());
        assert_eq!(response.trades_by_order_hash[0].trades.len(), 1);
        assert_eq!(
            response.trades_by_order_hash[0].trades[0].sender,
            address!("0000000000000000000000000000000000000002")
        );
        assert_eq!(response.trades_by_order_hash[1].order_hash, hash_b());
        assert!(response.trades_by_order_hash[1].trades.is_empty());

        let captured = captured.lock().unwrap().clone().unwrap();
        assert_eq!(captured.order_hashes, vec![hash_a(), hash_b()]);
        assert_eq!(captured.time_filter.start, Some(1700000000));
        assert_eq!(captured.time_filter.end, Some(1700002000));
    }

    #[rocket::async_test]
    async fn test_process_empty_requested_hashes_returns_empty_response() {
        let captured = Arc::new(Mutex::new(None));
        let ds = MockTradesDataSource {
            result: Ok(serde_json::from_value(serde_json::json!({
                "tradesByOrderHash": [],
                "totalCount": 0
            }))
            .unwrap()),
            captured: Arc::clone(&captured),
        };
        let request = TradesByOrderHashesRequest {
            order_hashes: vec![],
            start_time: None,
            end_time: None,
            denomination: None,
        };
        let result = process_get_trades_by_order_hashes(&ds, request)
            .await
            .unwrap();

        let response = result.into_inner();
        assert!(response.trades_by_order_hash.is_empty());
        assert_eq!(response.total_count, 0);
        assert!(captured
            .lock()
            .unwrap()
            .clone()
            .unwrap()
            .order_hashes
            .is_empty());
    }

    #[rocket::async_test]
    async fn test_process_rejects_invalid_hash() {
        let ds = MockTradesDataSource {
            result: Ok(mock_grouped_result()),
            captured: Arc::new(Mutex::new(None)),
        };
        let request = TradesByOrderHashesRequest {
            order_hashes: vec!["not-a-hash".to_string()],
            start_time: None,
            end_time: None,
            denomination: None,
        };
        let result = process_get_trades_by_order_hashes(&ds, request).await;
        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }

    #[rocket::async_test]
    async fn test_process_query_failure() {
        let ds = MockTradesDataSource {
            result: Err(ApiError::Internal("sdk error".into())),
            captured: Arc::new(Mutex::new(None)),
        };
        let request = TradesByOrderHashesRequest {
            order_hashes: vec![hash_a().to_string()],
            start_time: None,
            end_time: None,
            denomination: None,
        };
        let result = process_get_trades_by_order_hashes(&ds, request).await;
        assert!(matches!(result, Err(ApiError::Internal(_))));
    }

    #[rocket::async_test]
    async fn test_401_without_auth() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .post("/v1/trades/query")
            .header(ContentType::JSON)
            .body(r#"{"orderHashes":[]}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_invalid_hash_returns_400() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .post("/v1/trades/query")
            .header(Header::new("Authorization", header))
            .header(ContentType::JSON)
            .body(r#"{"orderHashes":["not-a-hash"]}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::BadRequest);
    }

    #[test]
    fn test_route_is_registered() {
        let routes = crate::routes::trades::routes();
        assert!(routes.iter().any(|route| route.uri.path() == "/query"));
    }
}
