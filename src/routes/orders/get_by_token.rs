use super::{
    active_filter_for_state, build_orders_list_response, current_wrap_ratios_for_orders,
    get_order_quotes_for_summaries, OrdersListDataSource, RaindexOrdersListDataSource,
    DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE,
};
use crate::app_state::ApplicationState;
use crate::auth::AuthenticatedKey;
use crate::db::DbPool;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::common::{Denomination, ValidatedAddress};
use crate::types::orders::{OrderSide, OrderState, OrdersByTokenParams, OrdersListResponse};
use alloy::primitives::Address;
use rain_orderbook_common::raindex_client::orders::GetOrdersFilters;
use rain_orderbook_common::raindex_client::orders::GetOrdersTokenFilter;
use rocket::serde::json::Json;
use rocket::State;
use tracing::Instrument;

pub(crate) async fn process_get_orders_by_token(
    ds: &dyn OrdersListDataSource,
    address: Address,
    state: Option<OrderState>,
    side: Option<OrderSide>,
    page: Option<u16>,
    page_size: Option<u16>,
    denomination: Denomination,
) -> Result<OrdersListResponse, ApiError> {
    let token_filter = match side {
        Some(OrderSide::Input) => GetOrdersTokenFilter {
            inputs: Some(vec![address]),
            outputs: None,
        },
        Some(OrderSide::Output) => GetOrdersTokenFilter {
            inputs: None,
            outputs: Some(vec![address]),
        },
        None => GetOrdersTokenFilter {
            inputs: Some(vec![address]),
            outputs: Some(vec![address]),
        },
    };

    let active_filter = active_filter_for_state(state);
    let filters = GetOrdersFilters {
        active: active_filter,
        tokens: Some(token_filter),
        has_positive_output_vault_balance: (active_filter == Some(true)).then_some(true),
        ..Default::default()
    };

    let page_num = page.unwrap_or(1);
    let effective_page_size = page_size
        .unwrap_or(DEFAULT_PAGE_SIZE as u16)
        .min(MAX_PAGE_SIZE);
    let (orders, total_count) = ds
        .get_orders_list(filters, Some(page_num), Some(effective_page_size))
        .await?;

    tracing::info!(
        quoted_orders = orders.len(),
        "fetching batched quotes for orders by token"
    );
    let quote_results = get_order_quotes_for_summaries(ds, &orders).await;
    let wrap_ratios = current_wrap_ratios_for_orders(ds, denomination, &orders).await?;

    build_orders_list_response(
        &orders,
        total_count,
        page_num.into(),
        effective_page_size.into(),
        quote_results,
        denomination,
        &wrap_ratios,
    )
}

#[utoipa::path(
    get,
    path = "/v1/orders/token/{address}",
    tag = "Orders",
    security(("basicAuth" = [])),
    params(
        ("address" = String, Path, description = "Token address"),
        OrdersByTokenParams,
    ),
    responses(
        (status = 200, description = "Paginated list of orders for token", body = OrdersListResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 422, description = "Unprocessable entity", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[allow(clippy::too_many_arguments)]
#[get("/token/<address>?<params..>")]
pub async fn get_orders_by_token(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    app_state: &State<ApplicationState>,
    pool: &State<DbPool>,
    span: TracingSpan,
    address: ValidatedAddress,
    params: OrdersByTokenParams,
) -> Result<Json<OrdersListResponse>, ApiError> {
    async move {
        tracing::info!(address = ?address, params = ?params, "request received");
        let addr = address.0;
        let state = params.state;
        let side = params.side;
        let page = params.page;
        let page_size = params.page_size;
        let denomination = params.denomination.unwrap_or_default();
        if !app_state.response_caches.is_enabled() {
            let raindex = shared_raindex.read().await;
            let ds = RaindexOrdersListDataSource {
                client: raindex.client(),
                caches: &app_state.response_caches,
                pool: pool.inner(),
            };
            let response =
                process_get_orders_by_token(&ds, addr, state, side, page, page_size, denomination)
                    .await?;
            return Ok(Json(response));
        }

        let cache_key =
            orders_by_token_cache_key(addr, state, side.as_ref(), page, page_size, denomination);
        let response = app_state
            .response_caches
            .orders_by_token
            .get_or_try_insert(cache_key, || async move {
                let raindex = shared_raindex.read().await;
                let ds = RaindexOrdersListDataSource {
                    client: raindex.client(),
                    caches: &app_state.response_caches,
                    pool: pool.inner(),
                };
                process_get_orders_by_token(&ds, addr, state, side, page, page_size, denomination)
                    .await
            })
            .await
            .map_err(|e| (*e).clone())?;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

fn orders_by_token_cache_key(
    address: Address,
    state: Option<OrderState>,
    side: Option<&OrderSide>,
    page: Option<u16>,
    page_size: Option<u16>,
    denomination: Denomination,
) -> String {
    let state = match state.unwrap_or(OrderState::Active) {
        OrderState::Active => "active",
        OrderState::Inactive => "inactive",
        OrderState::All => "all",
    };
    let side = match side {
        Some(OrderSide::Input) => "input",
        Some(OrderSide::Output) => "output",
        None => "all",
    };
    let page = page.unwrap_or(1);
    let page_size = page_size
        .unwrap_or(DEFAULT_PAGE_SIZE as u16)
        .min(MAX_PAGE_SIZE);
    format!(
        "orders/token/{}/{state}/{side}/{page}/{page_size}/{denomination:?}",
        address.to_string().to_ascii_lowercase()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::order::test_fixtures::{
        mock_order, mock_order_with_shared_vaults, mock_quote,
    };
    use crate::routes::orders::test_fixtures::{
        MockOrdersListDataSource, RecordingOrdersListDataSource,
    };
    use crate::test_helpers::{basic_auth_header, seed_api_key, TestClientBuilder};
    use crate::types::orders::OrderSummaryOrderType;
    use rocket::http::{Header, Status};

    #[rocket::async_test]
    async fn test_process_get_orders_by_token_success() {
        let ds = MockOrdersListDataSource {
            orders: Ok(vec![mock_order()]),
            total_count: 1,
            quotes: Ok(vec![mock_quote("1.5")]),
        };
        let addr: Address = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
            .parse()
            .unwrap();
        let result =
            process_get_orders_by_token(&ds, addr, None, None, None, None, Denomination::Wrapped)
                .await
                .unwrap();

        assert_eq!(result.orders.len(), 1);
        assert_eq!(result.orders[0].input_token.symbol, "USDC");
        assert_eq!(result.orders[0].output_token.symbol, "WETH");
        assert_eq!(result.orders[0].chain_id, 8453);
        assert_eq!(result.orders[0].order_bytes.as_ref(), &[1]);
        assert!(result.orders[0].active);
        assert_eq!(result.orders[0].removed_at, None);
        assert_eq!(result.orders[0].order_type, OrderSummaryOrderType::Strategy);
        assert_eq!(result.orders[0].io_ratio, "1.5");
        assert_eq!(result.orders[0].max_output.as_deref(), Some("1"));
        assert_eq!(result.pagination.total_orders, 1);
        assert_eq!(result.pagination.page, 1);
        assert!(!result.pagination.has_more);
    }

    #[rocket::async_test]
    async fn test_process_get_orders_by_token_empty() {
        let ds = MockOrdersListDataSource {
            orders: Ok(vec![]),
            total_count: 0,
            quotes: Ok(vec![]),
        };
        let addr: Address = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
            .parse()
            .unwrap();
        let result = process_get_orders_by_token(
            &ds,
            addr,
            None,
            Some(OrderSide::Input),
            None,
            None,
            Denomination::Wrapped,
        )
        .await
        .unwrap();

        assert!(result.orders.is_empty());
        assert_eq!(result.pagination.total_orders, 0);
        assert_eq!(result.pagination.total_pages, 0);
    }

    #[rocket::async_test]
    async fn test_process_get_orders_by_token_quote_failure_shows_dash() {
        let ds = MockOrdersListDataSource {
            orders: Ok(vec![mock_order()]),
            total_count: 1,
            quotes: Err(ApiError::Internal("quote error".into())),
        };
        let addr: Address = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
            .parse()
            .unwrap();
        let result =
            process_get_orders_by_token(&ds, addr, None, None, None, None, Denomination::Wrapped)
                .await
                .unwrap();

        assert_eq!(result.orders[0].io_ratio, "-");
        assert_eq!(result.orders[0].max_output, None);
    }

    #[rocket::async_test]
    async fn test_process_get_orders_by_token_query_failure() {
        let ds = MockOrdersListDataSource {
            orders: Err(ApiError::Internal("failed".into())),
            total_count: 0,
            quotes: Ok(vec![]),
        };
        let addr: Address = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
            .parse()
            .unwrap();
        let result =
            process_get_orders_by_token(&ds, addr, None, None, None, None, Denomination::Wrapped)
                .await;
        assert!(matches!(result, Err(ApiError::Internal(_))));
    }

    #[rocket::async_test]
    async fn test_process_get_orders_by_token_shared_vaults() {
        let ds = MockOrdersListDataSource {
            orders: Ok(vec![mock_order_with_shared_vaults()]),
            total_count: 1,
            quotes: Ok(vec![mock_quote("200.0")]),
        };
        let addr: Address = "0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2"
            .parse()
            .unwrap();
        let result =
            process_get_orders_by_token(&ds, addr, None, None, None, None, Denomination::Wrapped)
                .await
                .unwrap();

        assert_eq!(result.orders.len(), 1);
        assert_eq!(result.orders[0].input_token.symbol, "wtMSTR");
        assert_eq!(result.orders[0].output_token.symbol, "wtMSTR");
        assert_eq!(result.orders[0].chain_id, 8453);
    }

    #[rocket::async_test]
    async fn test_process_get_orders_by_token_inactive_state_sets_active_false_filter() {
        let ds = RecordingOrdersListDataSource::default();
        let addr: Address = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
            .parse()
            .unwrap();

        let result = process_get_orders_by_token(
            &ds,
            addr,
            Some(OrderState::Inactive),
            None,
            None,
            None,
            Denomination::Wrapped,
        )
        .await;

        assert!(result.is_ok());
        let filters = ds.filters.lock().expect("lock filters");
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].active, Some(false));
        assert_eq!(filters[0].has_positive_output_vault_balance, None);
    }

    #[rocket::async_test]
    async fn test_process_get_orders_by_token_all_state_omits_active_filter() {
        let ds = RecordingOrdersListDataSource::default();
        let addr: Address = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
            .parse()
            .unwrap();

        let result = process_get_orders_by_token(
            &ds,
            addr,
            Some(OrderState::All),
            None,
            None,
            None,
            Denomination::Wrapped,
        )
        .await;

        assert!(result.is_ok());
        let filters = ds.filters.lock().expect("lock filters");
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].active, None);
        assert_eq!(filters[0].has_positive_output_vault_balance, None);
    }

    #[rocket::async_test]
    async fn test_pagination_math() {
        use super::super::build_pagination;

        let p = build_pagination(250, 1, 100);
        assert_eq!(p.total_orders, 250);
        assert_eq!(p.total_pages, 3);
        assert!(p.has_more);

        let p = build_pagination(250, 3, 100);
        assert_eq!(p.total_pages, 3);
        assert!(!p.has_more);

        let p = build_pagination(0, 1, 100);
        assert_eq!(p.total_pages, 0);
        assert!(!p.has_more);

        let p = build_pagination(100, 1, 100);
        assert_eq!(p.total_pages, 1);
        assert!(!p.has_more);

        let p = build_pagination(101, 1, 100);
        assert_eq!(p.total_pages, 2);
        assert!(p.has_more);
    }

    #[test]
    fn test_orders_by_token_cache_key_normalizes_defaults_and_address_case() {
        let lower: Address = "0x31c2c14134e6e3b7ef9478297f199331133fc2d8"
            .parse()
            .unwrap();
        let mixed: Address = "0x31C2C14134e6E3B7ef9478297F199331133Fc2d8"
            .parse()
            .unwrap();

        assert_eq!(
            orders_by_token_cache_key(lower, None, None, None, None, Denomination::Wrapped),
            orders_by_token_cache_key(
                mixed,
                None,
                None,
                Some(1),
                Some(DEFAULT_PAGE_SIZE as u16),
                Denomination::Wrapped
            )
        );
        assert_ne!(
            orders_by_token_cache_key(
                lower,
                Some(OrderState::Inactive),
                None,
                None,
                None,
                Denomination::Wrapped
            ),
            orders_by_token_cache_key(
                lower,
                Some(OrderState::All),
                None,
                None,
                None,
                Denomination::Wrapped
            )
        );
    }

    #[rocket::async_test]
    async fn test_get_orders_by_token_401_without_auth() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .get("/v1/orders/token/0x833589fcd6edb6e08f4c7c32d4f71b54bda02913")
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_get_orders_by_token_invalid_address_returns_404() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/orders/token/not-an-address")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::UnprocessableEntity);
    }
}
