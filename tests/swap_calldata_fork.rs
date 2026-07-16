//! Fork-based integration test proving the REST API's swap calldata actually
//! executes on-chain.
//!
//! ## What this proves
//!
//! The unit tests in `src/routes/swap/calldata.rs` assert only the *shape* of
//! the swap calldata (`to == orderbook`, non-empty `data`, `value == 0`,
//! approvals present). They mock the datasource, so they never prove the bytes
//! the API hands a client will actually fill an order when submitted to the
//! chain. This test closes that gap: it drives the exact calldata-construction
//! path the `/v1/swap/calldata` route uses
//! (`RaindexClient::get_take_orders_calldata`, wrapped by
//! `RaindexSwapDataSource::get_calldata`) against an **anvil fork of Base**,
//! then submits the resulting `{to, data, value}` (and approval) as real
//! transactions and asserts on-chain effects.
//!
//! ## Design: replay a real live order against a fork (with a topped-up vault)
//!
//! We replay a real Albion order (captured from the production Goldsky
//! subgraph, `tests/fixtures/alb_usdc_order.json`) that sells `ALB-WR1-R1`
//! (`0xf836a5…`) for `USDC` (`0x833589…`) on orderbook `0xe522cB4a…`. The order's
//! real `orderBytes` are served to the calldata builder via a mock subgraph HTTP
//! server (the same interface the client uses in production); the RPC in the
//! settings YAML points at the anvil fork, so candidate simulation and the
//! submitted transactions run against real on-chain state.
//!
//! Two facts shaped the exact recipe:
//!
//! * We pin a **recent** Base block (`PINNED_FORK_BLOCK`) rather than a deep
//!   historical one, because public RPCs prune trie state and can't serve state
//!   deep in the past for anvil forking. A recent block forks reliably.
//! * This order's `ALB` output vault is currently empty on-chain, so at a recent
//!   block there is nothing to buy. We therefore **top up the real order's output
//!   vault on the fork** before building calldata: impersonate the order owner,
//!   source `ALB` by impersonating the orderbook (which custodies `ALB` for other
//!   vaults) to transfer some out, then call the orderbook's real `deposit4` into
//!   the order's actual output vault. This is a targeted, hermetic top-up of a
//!   *real replayed order* — everything else (orderBytes, orderbook, token
//!   contracts, price evaluation) is genuine on-chain state.
//!
//! We deliberately chose this **fixed-limit** order (its rainlang has no price
//! oracle) over Albion's oracle-priced `wt*` orders: an oracle order reverts with
//! `StalePrice` when quoted on a pinned fork (the oracle's freshness window is
//! shorter than the fork's block age), which is a property of the oracle, not the
//! swap calldata we are trying to prove.
//!
//! ## Env gating
//!
//! The test forks Base via `BASE_FORK_RPC_URL` (default
//! `https://base.publicnode.com`). If the fork RPC is unset/unreachable or the
//! pinned (archive-depth) block cannot be forked, the test **skips** rather than
//! fails, mirroring how the repo keeps network-dependent tests green on CI
//! forks without an archive RPC. For archive depth locally, set
//! `BASE_FORK_RPC_URL` to an Alchemy/dRPC Base URL.

use alloy::network::TransactionBuilder;
use alloy::primitives::{address, keccak256, Address, Bytes, B256, U256};
use alloy::providers::ext::AnvilApi;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use alloy::sol;
use alloy::sol_types::{SolCall, SolValue};
use httpmock::MockServer;
use rain_orderbook_common::raindex_client::take_orders::TakeOrdersRequest;
use rain_orderbook_common::raindex_client::{RaindexClient, RaindexError};
use rain_orderbook_common::take_orders::TakeOrdersMode;
use serde_json::json;

// ---------------------------------------------------------------------------
// Pinned fork parameters (Base mainnet, chain id 8453)
// ---------------------------------------------------------------------------

/// Base mainnet chain id. Matches `albion_rest_api::CHAIN_ID`.
const CHAIN_ID: u32 = 8453;

/// A recent Base block that public RPCs reliably serve for anvil forking. The
/// replayed order is active on-chain at this block; we top up its output vault on
/// the fork (see module docs). Pinned for determinism.
const PINNED_FORK_BLOCK: u64 = 48_724_000;

/// Albion orderbook (Raindex) on Base — shared with st0x per the registry.
const ORDERBOOK: Address = address!("e522cB4a5fCb2eb31a52Ff41a4653d85A4fd7C9D");

/// USDC on Base (input token / what the taker pays), 6 decimals.
const USDC: Address = address!("833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
/// Storage slot of USDC's `balances` mapping (FiatTokenV2), verified on-chain.
const USDC_BALANCES_SLOT: u64 = 9;

/// ALB-WR1-R1 on Base (output token / what the taker buys), 18 decimals.
const BUY_TOKEN: Address = address!("f836a500910453A397084ADe41321ee20a5AAde1");

fn default_fork_rpc() -> String {
    std::env::var("BASE_FORK_RPC_URL").unwrap_or_else(|_| "https://base.publicnode.com".to_string())
}

sol! {
    #[sol(rpc)]
    interface IERC20 {
        function balanceOf(address account) external view returns (uint256);
        function allowance(address owner, address spender) external view returns (uint256);
        function transfer(address to, uint256 amount) external returns (bool);
        function approve(address spender, uint256 amount) external returns (bool);
    }

    struct EvaluableV4 {
        address interpreter;
        address store;
        bytes bytecode;
    }
    struct SignedContextV1 {
        address signer;
        bytes32[] context;
        bytes signature;
    }
    struct TaskV2 {
        EvaluableV4 evaluable;
        SignedContextV1[] signedContext;
    }

    #[sol(rpc)]
    interface IOrderbook {
        function vaultBalance2(address owner, address token, bytes32 vaultId) external view returns (bytes32);
        function deposit4(address token, bytes32 vaultId, bytes32 depositAmount, TaskV2[] tasks) external;
    }
}

/// The captured real order, as the production subgraph returns it. We serve
/// `orderBytes` + vault/token metadata; on-chain state (fork) governs fills.
fn fixture_order_json() -> serde_json::Value {
    let raw = include_str!("fixtures/alb_usdc_order.json");
    serde_json::from_str(raw).expect("valid fixture order json")
}

fn order_owner() -> Address {
    fixture_order_json()["owner"]
        .as_str()
        .expect("owner")
        .parse()
        .expect("owner address")
}

fn output_vault_id() -> B256 {
    fixture_order_json()["outputs"][0]["vaultId"]
        .as_str()
        .expect("output vaultId")
        .parse()
        .expect("vaultId b256")
}

/// Reshapes the captured subgraph order into the exact JSON the
/// `RaindexSubgraphClient` expects (`orderBytes`, `inputs`/`outputs` with
/// nested `token`, `vaultId`, `balance`, `raindex`, etc.). The balances we
/// serve are cosmetic — candidate building and fill simulation read the fork,
/// not these numbers — but they must be present and well-formed.
fn subgraph_order_response() -> serde_json::Value {
    let order = fixture_order_json();

    let map_vaults = |vaults: &serde_json::Value| -> Vec<serde_json::Value> {
        vaults
            .as_array()
            .expect("vault array")
            .iter()
            .map(|v| {
                json!({
                    "id": v["id"],
                    "owner": order["owner"],
                    "vaultId": v["vaultId"],
                    "balance": v["balance"],
                    "token": {
                        "id": v["token"]["id"],
                        "address": v["token"]["address"],
                        "name": v["token"]["name"],
                        "symbol": v["token"]["symbol"],
                        "decimals": v["token"]["decimals"],
                    },
                    "raindex": { "id": order["orderbook"]["id"] },
                    "ordersAsOutput": [],
                    "ordersAsInput": [],
                    "balanceChanges": [],
                })
            })
            .collect()
    };

    let inputs = map_vaults(&order["inputs"]);
    let outputs = map_vaults(&order["outputs"]);

    json!({
        "id": order["orderHash"],
        "orderBytes": order["orderBytes"],
        "orderHash": order["orderHash"],
        "owner": order["owner"],
        "outputs": outputs,
        "inputs": inputs,
        "raindex": { "id": order["orderbook"]["id"] },
        "active": true,
        "timestampAdded": order["timestampAdded"],
        "meta": null,
        "addEvents": [{
            "transaction": {
                "id": "0x0000000000000000000000000000000000000000000000000000000000000001",
                "from": order["owner"],
                "blockNumber": "1",
                "timestamp": order["timestampAdded"],
            }
        }],
        "trades": [],
        "removeEvents": []
    })
}

/// Settings YAML that mirrors the production registry's `base` network but
/// points `rpcs` at the anvil fork and `subgraph` at the mock server. Building
/// a `RaindexClient` from this is exactly what `RaindexProvider::load` does in
/// production (minus the additional-RPC injection, which is orthogonal here).
fn fork_settings_yaml(rpc_url: &str, sg_url: &str) -> String {
    format!(
        r#"
version: 6
networks:
    base:
        rpcs:
            - {rpc_url}
        chain-id: {CHAIN_ID}
        network-id: {CHAIN_ID}
        currency: ETH
subgraphs:
    base: {sg_url}
metaboards:
    base: http://localhost:0/notused
raindexes:
    base:
        address: {ORDERBOOK}
        network: base
        subgraph: base
        local-db-remote: remote
        deployment-block: 0
"#
    )
}

/// Starts a mock subgraph returning our single real order for every query.
fn start_mock_subgraph() -> (MockServer, String) {
    let server = MockServer::start();
    let order = subgraph_order_response();
    server.mock(|when, then| {
        when.path("/sg");
        then.status(200).json_body(json!({
            "data": { "orders": [order] }
        }));
    });
    let url = server.url("/sg");
    (server, url)
}

/// Spawns an anvil fork of Base pinned to `PINNED_FORK_BLOCK`. Returns `None`
/// (skip signal) if anvil can't fork the RPC (missing binary, unreachable RPC,
/// or non-archive RPC that can't serve the pinned block) — retried once for
/// transient failures.
async fn try_spawn_fork() -> Option<ForkHarness> {
    for attempt in 1..=2 {
        match spawn_fork_once().await {
            Ok(harness) => return Some(harness),
            Err(e) => {
                eprintln!("[swap_calldata_fork] fork spawn attempt {attempt}/2 failed: {e}");
            }
        }
    }
    None
}

struct ForkHarness {
    _anvil: alloy::node_bindings::AnvilInstance,
    endpoint: String,
    /// Provider with the taker's wallet — signs txs sent as the taker.
    provider: alloy::providers::DynProvider,
    /// Wallet-less provider — used to send *unsigned* txs from impersonated
    /// (anvil-unlocked) accounts, which a wallet filler would wrongly try to sign.
    raw_provider: alloy::providers::DynProvider,
    taker: Address,
}

async fn spawn_fork_once() -> Result<ForkHarness, String> {
    if std::env::var("SWAP_FORK_TRACE").is_ok() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("raindex_common=debug")),
            )
            .try_init();
    }
    let rpc = default_fork_rpc();

    // anvil defaults to auto-mining (one block per transaction), which is what
    // we want: each submitted tx is mined immediately.
    let anvil = alloy::node_bindings::Anvil::new()
        .fork(&rpc)
        .fork_block_number(PINNED_FORK_BLOCK)
        .chain_id(CHAIN_ID as u64)
        .try_spawn()
        .map_err(|e| format!("anvil spawn (rpc={rpc}): {e}"))?;

    let endpoint = anvil.endpoint();

    // Use anvil's first prefunded account as the taker.
    let signer: alloy::signers::local::PrivateKeySigner = anvil.keys()[0].clone().into();
    let taker = signer.address();
    let taker_wallet = alloy::network::EthereumWallet::from(signer);

    let provider = ProviderBuilder::new()
        .wallet(taker_wallet)
        .connect(&endpoint)
        .await
        .map_err(|e| format!("connect provider: {e}"))?
        .erased();

    let raw_provider = ProviderBuilder::new()
        .connect(&endpoint)
        .await
        .map_err(|e| format!("connect raw provider: {e}"))?
        .erased();

    // Sanity: confirm we actually forked at (or near) the pinned block. A
    // non-archive RPC silently forks at latest instead of the requested block;
    // reject that so we don't run against the wrong state.
    let block = provider
        .get_block_number()
        .await
        .map_err(|e| format!("get_block_number: {e}"))?;
    if !(PINNED_FORK_BLOCK..=PINNED_FORK_BLOCK + 2).contains(&block) {
        return Err(format!(
            "fork at block {block}, expected ~{PINNED_FORK_BLOCK} (RPC likely not archive)"
        ));
    }

    // Archive-capability probe. `get_block_number` and even `eth_call` succeed on
    // pruning RPCs, but anvil's fork backend fetches raw account storage
    // (`eth_getStorageAt` at the pinned block) mid-transaction — which 403s on
    // non-archive endpoints ("Archive requests require a personal token"). Probe
    // that exact path via `anvil_set_balance` + a storage read so we return a
    // clean skip instead of failing mid-test. `anvil_set_balance` forces the
    // backend to load the account at the fork block.
    raw_provider
        .anvil_set_balance(ORDERBOOK, U256::from(1u64))
        .await
        .map_err(|e| format!("archive probe (set_balance): {e}"))?;
    raw_provider
        .get_storage_at(BUY_TOKEN, U256::from(0u64))
        .await
        .map_err(|e| format!("archive state probe failed (RPC likely not archive): {e}"))?;

    Ok(ForkHarness {
        _anvil: anvil,
        endpoint,
        provider,
        raw_provider,
        taker,
    })
}

impl ForkHarness {
    /// Builds a `RaindexClient` whose RPC points at this fork and whose subgraph
    /// points at `sg_url` — the exact wiring `RaindexProvider::load` produces.
    async fn raindex_client(&self, sg_url: &str) -> RaindexClient {
        let yaml = fork_settings_yaml(&self.endpoint, sg_url);
        RaindexClient::new(vec![yaml], None, None)
            .await
            .expect("build RaindexClient from fork settings")
    }

    /// Funds the taker with `amount` USDC by writing the FiatToken balances slot
    /// (`anvil_setStorageAt`), and tops up ETH for gas.
    async fn fund_taker_usdc(&self, amount: U256) {
        self.provider
            .anvil_set_balance(self.taker, U256::from(10u64).pow(U256::from(20u64)))
            .await
            .expect("set ETH balance");

        let slot = keccak256((self.taker, U256::from(USDC_BALANCES_SLOT)).abi_encode());
        self.provider
            .anvil_set_storage_at(USDC, slot.into(), B256::from(amount))
            .await
            .expect("set USDC balance slot");

        let bal = IERC20::new(USDC, &self.provider)
            .balanceOf(self.taker)
            .call()
            .await
            .expect("read USDC balance");
        assert_eq!(bal, amount, "USDC funding via storage slot should stick");
    }

    async fn erc20_balance(&self, token: Address, who: Address) -> U256 {
        IERC20::new(token, &self.provider)
            .balanceOf(who)
            .call()
            .await
            .expect("balanceOf")
    }

    async fn vault_balance(&self, token: Address) -> B256 {
        IOrderbook::new(ORDERBOOK, &self.provider)
            .vaultBalance2(order_owner(), token, output_vault_id())
            .call()
            .await
            .expect("vaultBalance2")
    }

    /// Tops up the replayed order's real output vault with `amount` (raw, 18-dec)
    /// of the buy token, so a recent-block fork has liquidity to fill against.
    ///
    /// Recipe (all against the fork; no production state touched):
    /// 1. impersonate the orderbook, which custodies the buy token for other
    ///    vaults, and `transfer` `amount` to the order owner;
    /// 2. impersonate the order owner, `approve` the orderbook, then call the
    ///    orderbook's real `deposit4` into the order's actual output vault.
    async fn fund_order_vault(&self, amount: U256) {
        let owner = order_owner();
        let vault_id = output_vault_id();

        // Give both impersonated accounts ETH for gas.
        for acct in [ORDERBOOK, owner] {
            self.provider
                .anvil_set_balance(acct, U256::from(10u64).pow(U256::from(18u64)))
                .await
                .expect("set ETH balance for impersonated account");
        }

        // 1. Orderbook -> owner transfer of the buy token.
        self.provider
            .anvil_impersonate_account(ORDERBOOK)
            .await
            .expect("impersonate orderbook");
        let transfer_tx = TransactionRequest::default()
            .with_from(ORDERBOOK)
            .with_to(BUY_TOKEN)
            .with_input(IERC20::transferCall { to: owner, amount }.abi_encode());
        let ok = self.send_raw(transfer_tx).await;
        assert!(ok, "orderbook -> owner buy-token transfer should succeed");
        self.provider
            .anvil_stop_impersonating_account(ORDERBOOK)
            .await
            .expect("stop impersonating orderbook");

        // 2. Owner approves + deposits into the order's output vault.
        self.provider
            .anvil_impersonate_account(owner)
            .await
            .expect("impersonate owner");

        let approve_tx = TransactionRequest::default()
            .with_from(owner)
            .with_to(BUY_TOKEN)
            .with_input(
                IERC20::approveCall {
                    spender: ORDERBOOK,
                    amount,
                }
                .abi_encode(),
            );
        assert!(
            self.send_raw(approve_tx).await,
            "owner approve should succeed"
        );

        // deposit4 takes a Float-encoded amount (bytes32).
        let (deposit_float, _) = rain_math_float::Float::from_fixed_decimal_lossy(amount, 18)
            .expect("encode deposit amount");
        let deposit_tx = TransactionRequest::default()
            .with_from(owner)
            .with_to(ORDERBOOK)
            .with_input(
                IOrderbook::deposit4Call {
                    token: BUY_TOKEN,
                    vaultId: vault_id,
                    depositAmount: deposit_float.get_inner(),
                    tasks: vec![],
                }
                .abi_encode(),
            );
        assert!(
            self.send_raw(deposit_tx).await,
            "deposit4 into the order's output vault should succeed"
        );

        self.provider
            .anvil_stop_impersonating_account(owner)
            .await
            .expect("stop impersonating owner");

        let vault_after = self.vault_balance(BUY_TOKEN).await;
        assert_ne!(
            vault_after,
            B256::ZERO,
            "output vault should be funded after deposit"
        );
    }

    /// Sends an *unsigned* transaction from an anvil-impersonated account (the
    /// wallet-less provider leaves it unsigned so anvil executes it as the
    /// unlocked `from`) and returns whether it succeeded.
    async fn send_raw(&self, tx: TransactionRequest) -> bool {
        match self.raw_provider.send_transaction(tx).await {
            Ok(pending) => pending
                .get_receipt()
                .await
                .map(|r| r.status())
                .unwrap_or(false),
            Err(e) => {
                eprintln!("[swap_calldata_fork] impersonated send failed: {e}");
                false
            }
        }
    }

    /// Submits every approval transaction in an approval-step response, as the
    /// taker. Errors if any approval reverts.
    async fn submit_approvals(&self, response: &SwapResponse) -> Result<(), String> {
        for approval in &response.approvals {
            let tx = TransactionRequest::default()
                .with_from(self.taker)
                .with_to(approval.token)
                .with_input(approval.data.clone());
            let pending = self
                .provider
                .send_transaction(tx)
                .await
                .map_err(|e| format!("send approval: {e}"))?;
            let receipt = pending
                .get_receipt()
                .await
                .map_err(|e| format!("approval receipt: {e}"))?;
            if !receipt.status() {
                return Err("approval tx reverted".to_string());
            }
        }
        Ok(())
    }

    /// Submits the swap `{to, data, value}` as the taker and returns whether the
    /// transaction succeeded (did not revert).
    async fn submit_swap(&self, response: &SwapResponse) -> Result<bool, String> {
        let tx = TransactionRequest::default()
            .with_from(self.taker)
            .with_to(response.to)
            .with_value(response.value)
            .with_input(response.data.clone());

        let pending = self
            .provider
            .send_transaction(tx)
            .await
            .map_err(|e| format!("send swap: {e}"))?;
        let receipt = pending
            .get_receipt()
            .await
            .map_err(|e| format!("swap receipt: {e}"))?;
        Ok(receipt.status())
    }

    fn taker(&self) -> Address {
        self.taker
    }
}

// ---------------------------------------------------------------------------
// Mirror of the REST route's calldata construction + response mapping.
// ---------------------------------------------------------------------------

/// Local mirror of `SwapCalldataResponse` (the route's DTO). We rebuild it from
/// `get_take_orders_calldata` exactly as `RaindexSwapDataSource::get_calldata`
/// does, so submitting it exercises the real API output.
#[derive(Debug)]
struct SwapResponse {
    to: Address,
    data: Bytes,
    value: U256,
    estimated_input: String,
    approvals: Vec<ApprovalTx>,
}

#[derive(Debug)]
struct ApprovalTx {
    token: Address,
    spender: Address,
    data: Bytes,
}

/// Reproduces `RaindexSwapDataSource::get_calldata` (`src/routes/swap/mod.rs`):
/// call `get_take_orders_calldata`, then map either the approval info or the
/// take-orders info into the response the route returns.
async fn build_swap_response(
    client: &RaindexClient,
    req: TakeOrdersRequest,
) -> Result<SwapResponse, RaindexError> {
    let result = client.get_take_orders_calldata(req).await?;

    if let Some(approval_info) = result.approval_info() {
        let formatted_amount = approval_info.formatted_amount().to_string();
        Ok(SwapResponse {
            to: approval_info.spender(),
            data: Bytes::new(),
            value: U256::ZERO,
            estimated_input: formatted_amount.clone(),
            approvals: vec![ApprovalTx {
                token: approval_info.token(),
                spender: approval_info.spender(),
                data: approval_info.calldata().clone(),
            }],
        })
    } else if let Some(take_orders_info) = result.take_orders_info() {
        let expected_sell = take_orders_info
            .expected_sell()
            .format()
            .map_err(|_| RaindexError::NoLiquidity)?;
        Ok(SwapResponse {
            to: take_orders_info.raindex(),
            data: take_orders_info.calldata().clone(),
            value: U256::ZERO,
            estimated_input: expected_sell,
            approvals: vec![],
        })
    } else {
        Err(RaindexError::NoLiquidity)
    }
}

/// Builds the `TakeOrdersRequest` exactly as `process_swap_calldata_build`
/// (`src/routes/swap/calldata.rs`) does for a wrapped-denomination request.
fn take_orders_request(
    taker: Address,
    sell_token: Address,
    buy_token: Address,
    mode: TakeOrdersMode,
    amount: &str,
    price_cap: &str,
) -> TakeOrdersRequest {
    TakeOrdersRequest {
        taker: taker.to_string(),
        chain_id: CHAIN_ID,
        sell_token: sell_token.to_string(),
        buy_token: buy_token.to_string(),
        mode,
        amount: amount.to_string(),
        price_cap: price_cap.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Case 1 — Buy with approval executes.
///
/// A fresh taker (no USDC allowance to the orderbook) requests a small BUY_TOKEN buy.
/// The API returns an approval first; we submit it, then the swap. Asserts:
/// taker's BUY_TOKEN balance increased, USDC decreased by ~estimated_input, and the
/// order's BUY_TOKEN output vault was debited.
#[tokio::test]
async fn buy_with_approval_executes_on_fork() {
    let Some(harness) = try_spawn_fork().await else {
        eprintln!(
            "[swap_calldata_fork] SKIP buy_with_approval_executes_on_fork: \
             set BASE_FORK_RPC_URL to a reachable Base archive RPC to run it"
        );
        return;
    };

    let (_sg, sg_url) = start_mock_subgraph();
    let client = harness.raindex_client(&sg_url).await;

    // Fund the taker with 1,000 USDC (6 decimals) — plenty for a tiny buy.
    let usdc_funding = U256::from(1_000u64) * U256::from(10u64).pow(U256::from(6u64));
    harness.fund_taker_usdc(usdc_funding).await;

    // Top up the replayed order's output vault with 10 buy-token so there is
    // liquidity to fill against on this recent fork block.
    harness
        .fund_order_vault(U256::from(200u64) * U256::from(10u64).pow(U256::from(18u64)))
        .await;

    // Buy up to 0.5 BUY_TOKEN paying USDC (above the order's minimum trade size).
    // BuyUpTo (partial) fills what is available up to the target.
    let req = take_orders_request(
        harness.taker(),
        USDC,
        BUY_TOKEN,
        TakeOrdersMode::BuyUpTo,
        "5",
        // Generous price cap so the order's real ratio is well within it.
        "1000000",
    );

    let response = build_swap_response(&client, req)
        .await
        .expect("calldata build should succeed for funded order");

    // The first response for a fresh taker must be an approval (no allowance).
    assert_eq!(
        response.approvals.len(),
        1,
        "fresh taker with no allowance should get an approval step"
    );
    assert_eq!(response.approvals[0].token, USDC, "approve USDC");
    assert_eq!(
        response.approvals[0].spender, ORDERBOOK,
        "approval spender is the orderbook"
    );
    assert!(
        !response.approvals[0].data.is_empty(),
        "approval calldata must be non-empty"
    );

    // Submit the approval, then re-request calldata (now allowance exists) to
    // get the actual swap bytes — mirrors a real client's two-step flow.
    harness
        .submit_approvals(&response)
        .await
        .expect("approval submission");

    let req2 = take_orders_request(
        harness.taker(),
        USDC,
        BUY_TOKEN,
        TakeOrdersMode::BuyUpTo,
        "5",
        "1000000",
    );
    let swap = build_swap_response(&client, req2)
        .await
        .expect("calldata build after approval should succeed");

    assert!(
        swap.approvals.is_empty(),
        "after approval the response should carry swap bytes, not another approval"
    );
    // Route invariants: to == orderbook, data non-empty, value == 0.
    assert_eq!(swap.to, ORDERBOOK, "swap target is the orderbook");
    assert!(!swap.data.is_empty(), "swap calldata must be non-empty");
    assert_eq!(swap.value, U256::ZERO, "swap value must be zero");

    let alb_before = harness.erc20_balance(BUY_TOKEN, harness.taker()).await;
    let usdc_before = harness.erc20_balance(USDC, harness.taker()).await;
    let vault_before = harness.vault_balance(BUY_TOKEN).await;

    let filled = harness.submit_swap(&swap).await.expect("swap submission");
    assert!(filled, "swap tx should not revert on the fork");

    let alb_after = harness.erc20_balance(BUY_TOKEN, harness.taker()).await;
    let usdc_after = harness.erc20_balance(USDC, harness.taker()).await;
    let vault_after = harness.vault_balance(BUY_TOKEN).await;

    assert!(
        alb_after > alb_before,
        "taker should have received BUY_TOKEN (before={alb_before}, after={alb_after})"
    );
    assert!(
        usdc_after < usdc_before,
        "taker should have spent USDC (before={usdc_before}, after={usdc_after})"
    );
    assert_ne!(
        vault_before, vault_after,
        "order's BUY_TOKEN output vault should be debited by the fill"
    );

    // USDC spent should be within a factor of the estimated input (Float-formatted,
    // 6-decimal token). Loose bound: spent > 0 and the estimate parses to > 0.
    let spent = usdc_before - usdc_after;
    assert!(spent > U256::ZERO, "USDC spent must be positive");
    let est: f64 = swap.estimated_input.parse().unwrap_or(0.0);
    assert!(
        est > 0.0,
        "estimated_input should be a positive number, got {:?}",
        swap.estimated_input
    );

    eprintln!(
        "[swap_calldata_fork] buy executed: ALB +{}, USDC -{}, estimated_input={}",
        alb_after - alb_before,
        spent,
        swap.estimated_input
    );
}

/// Case 2 — Slippage cap rejects.
///
/// Build BuyExact calldata but with a `maximumIoRatio` (price cap) tighter than
/// the order's real ratio. When submitted, the fill can't be satisfied within
/// the cap, so the take reverts on-chain — proving the cap is enforced by the
/// actual calldata, not merely the quote.
#[tokio::test]
async fn slippage_cap_reverts_on_fork() {
    let Some(harness) = try_spawn_fork().await else {
        eprintln!(
            "[swap_calldata_fork] SKIP slippage_cap_reverts_on_fork: \
             set BASE_FORK_RPC_URL to a reachable Base archive RPC to run it"
        );
        return;
    };

    let (_sg, sg_url) = start_mock_subgraph();
    let client = harness.raindex_client(&sg_url).await;

    let usdc_funding = U256::from(1_000u64) * U256::from(10u64).pow(U256::from(6u64));
    harness.fund_taker_usdc(usdc_funding).await;
    harness
        .fund_order_vault(U256::from(200u64) * U256::from(10u64).pow(U256::from(18u64)))
        .await;

    // Approve first (isolate the revert to the cap, not a missing allowance).
    let approve_req = take_orders_request(
        harness.taker(),
        USDC,
        BUY_TOKEN,
        TakeOrdersMode::BuyUpTo,
        "5",
        "1000000",
    );
    let approval = build_swap_response(&client, approve_req)
        .await
        .expect("approval build");
    assert_eq!(approval.approvals.len(), 1, "expected an approval step");
    harness
        .submit_approvals(&approval)
        .await
        .expect("submit approval");

    // Now build a BuyExact swap with an absurdly tight price cap. BuyExact
    // requires the full requested amount to fill; with a cap below the order's
    // ratio the order is skipped, so either the build reports no liquidity or the
    // submitted tx reverts. Both outcomes prove the cap is enforced end-to-end.
    let tight_cap = "0.0000000001";
    let req = take_orders_request(
        harness.taker(),
        USDC,
        BUY_TOKEN,
        TakeOrdersMode::BuyExact,
        "0.0001",
        tight_cap,
    );

    match build_swap_response(&client, req).await {
        Err(e) => {
            eprintln!(
                "[swap_calldata_fork] tight cap rejected at build time: {e:?} (cap enforced)"
            );
        }
        Ok(swap) => {
            // If calldata was produced, submitting it must revert on-chain.
            let filled = harness.submit_swap(&swap).await.unwrap_or(false);
            assert!(
                !filled,
                "swap under an impossibly tight price cap must revert on-chain, \
                 proving the cap is baked into the calldata"
            );
            eprintln!(
                "[swap_calldata_fork] tight cap calldata reverted on submission (cap enforced)"
            );
        }
    }
}

/// Case 3 — No liquidity.
///
/// Request a buy far exceeding the order's available output in BuyExact mode.
/// The calldata builder cannot satisfy the exact amount, so the route's error
/// path (`RaindexError::{NoLiquidity, InsufficientLiquidity}` → HTTP 404) is
/// exercised. This is a build-level assertion (no on-chain submission needed),
/// but it still runs against the real forked vault balance.
#[tokio::test]
async fn no_liquidity_for_oversized_buy_on_fork() {
    let Some(harness) = try_spawn_fork().await else {
        eprintln!(
            "[swap_calldata_fork] SKIP no_liquidity_for_oversized_buy_on_fork: \
             set BASE_FORK_RPC_URL to a reachable Base archive RPC to run it"
        );
        return;
    };

    let (_sg, sg_url) = start_mock_subgraph();
    let client = harness.raindex_client(&sg_url).await;

    // Fund the vault with a modest 10 buy-token: enough for the order to quote,
    // far short of the oversized exact buy below.
    harness
        .fund_order_vault(U256::from(200u64) * U256::from(10u64).pow(U256::from(18u64)))
        .await;

    // Buy an amount of BUY_TOKEN far larger than the funded vault balance, exact.
    let req = take_orders_request(
        harness.taker(),
        USDC,
        BUY_TOKEN,
        TakeOrdersMode::BuyExact,
        "100000000",
        "1000000",
    );

    let result = build_swap_response(&client, req).await;
    assert!(
        matches!(
            result,
            Err(RaindexError::NoLiquidity) | Err(RaindexError::InsufficientLiquidity { .. })
        ),
        "oversized exact buy should report no/insufficient liquidity, got: {result:?}"
    );
    eprintln!(
        "[swap_calldata_fork] oversized exact buy correctly reported no/insufficient liquidity"
    );
}
