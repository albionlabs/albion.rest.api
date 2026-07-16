use alloy::primitives::{Address, U256};
use base64::Engine;
use rain_math_float::Float;
use rain_orderbook_bindings::IRaindexV6::{EvaluableV4, OrderV4, IOV2};
use rain_orderbook_common::raindex_client::orders::RaindexOrder;
use rain_orderbook_common::take_orders::TakeOrderCandidate;
use rocket::local::asynchronous::Client;
use serde_json::json;

pub(crate) async fn client() -> Client {
    TestClientBuilder::new().build().await
}

pub(crate) struct TestClientBuilder {
    rate_limiter: crate::fairings::RateLimiter,
    raindex_registry_url: Option<String>,
    raindex_config: Option<crate::raindex::RaindexProvider>,
    private_registry_path: Option<std::path::PathBuf>,
    database_url: Option<String>,
    sync_status_fetcher: Option<crate::sync_status::SyncStatusFetcher>,
}

impl TestClientBuilder {
    pub(crate) fn new() -> Self {
        Self {
            rate_limiter: crate::fairings::RateLimiter::new(10000, 10000),
            raindex_registry_url: None,
            raindex_config: None,
            private_registry_path: None,
            database_url: None,
            sync_status_fetcher: None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn rate_limiter(mut self, rate_limiter: crate::fairings::RateLimiter) -> Self {
        self.rate_limiter = rate_limiter;
        self
    }

    #[allow(dead_code)]
    pub(crate) fn raindex_config(mut self, config: crate::raindex::RaindexProvider) -> Self {
        self.raindex_config = Some(config);
        self
    }

    #[allow(dead_code)]
    pub(crate) fn sync_status_fetcher(
        mut self,
        fetcher: crate::sync_status::SyncStatusFetcher,
    ) -> Self {
        self.sync_status_fetcher = Some(fetcher);
        self
    }

    pub(crate) fn private_registry_path(mut self, path: std::path::PathBuf) -> Self {
        self.private_registry_path = Some(path);
        self
    }

    pub(crate) fn database_url(mut self, url: String) -> Self {
        self.database_url = Some(url);
        self
    }

    pub(crate) async fn build(self) -> Client {
        let id = uuid::Uuid::new_v4();
        let database_url = self
            .database_url
            .unwrap_or_else(|| format!("sqlite:file:{id}?mode=memory&cache=shared"));
        let pool = crate::db::init(&database_url, 5)
            .await
            .expect("database init");

        let private_registry_path = self.private_registry_path.unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "st0x-test-private-registry-{}.data",
                uuid::Uuid::new_v4()
            ))
        });

        let raindex_config = match self.raindex_config {
            Some(config) => config,
            None => {
                let registry_url = match std::fs::read_to_string(&private_registry_path) {
                    Ok(artifact) if !artifact.is_empty() => artifact,
                    _ => match self.raindex_registry_url {
                        Some(url) => url,
                        None => mock_raindex_registry_url().await,
                    },
                };
                crate::raindex::RaindexProvider::load(
                    &registry_url,
                    None,
                    std::collections::HashMap::new(),
                )
                .await
                .expect("mock raindex config from registry url")
            }
        };

        let shared_raindex = tokio::sync::RwLock::new(raindex_config);
        let artifact_store =
            crate::registry_artifact::RegistryArtifactStore::new(private_registry_path);
        let response_caches =
            crate::cache::RouteResponseCaches::new(100, std::time::Duration::from_secs(10));
        let app_state = crate::app_state::ApplicationState::new(artifact_store, response_caches);
        let docs_dir = std::env::temp_dir().to_string_lossy().into_owned();
        let rocket = crate::rocket(
            pool,
            self.rate_limiter,
            shared_raindex,
            app_state,
            docs_dir,
            2,
            self.sync_status_fetcher,
        )
        .expect("valid rocket instance");

        Client::tracked(rocket).await.expect("valid client")
    }
}

pub(crate) async fn mock_raindex_config() -> crate::raindex::RaindexProvider {
    let registry_url = mock_raindex_registry_url().await;
    crate::raindex::RaindexProvider::load(&registry_url, None, std::collections::HashMap::new())
        .await
        .expect("mock raindex config")
}

pub(crate) async fn mock_raindex_registry_url() -> String {
    let settings = r#"version: 6
networks:
  base:
    rpcs:
      - https://mainnet.base.org
    chain-id: 8453
    currency: ETH
subgraphs:
  base: https://api.goldsky.com/api/public/project_clv14x04y9kzi01saerx7bxpg/subgraphs/ob4-base/0.9/gn
raindexes:
  base:
    address: 0xd2938e7c9fe3597f78832ce780feb61945c377d7
    network: base
    subgraph: base
    deployment-block: 0
deployers:
  base:
    address: 0xC1A14cE2fd58A3A2f99deCb8eDd866204eE07f8D
    network: base
tokens:
  token1:
    address: 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913
    network: base
"#;
    mock_raindex_registry_url_with_settings(settings).await
}

pub(crate) async fn mock_raindex_registry_url_with_settings(settings: &str) -> String {
    mock_raindex_registry_url_with_settings_and_tokens(settings, "{}").await
}

pub(crate) fn mock_raindex_registry_artifact() -> String {
    let settings = r#"version: 6
networks:
  base:
    rpcs:
      - https://mainnet.base.org
    chain-id: 8453
    currency: ETH
subgraphs:
  base: https://api.goldsky.com/api/public/project_clv14x04y9kzi01saerx7bxpg/subgraphs/ob4-base/0.9/gn
raindexes:
  base:
    address: 0xd2938e7c9fe3597f78832ce780feb61945c377d7
    network: base
    subgraph: base
    deployment-block: 0
deployers:
  base:
    address: 0xC1A14cE2fd58A3A2f99deCb8eDd866204eE07f8D
    network: base
tokens:
  token1:
    address: 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913
    network: base
"#;
    mock_raindex_registry_artifact_with_settings(settings)
}

pub(crate) fn mock_raindex_registry_artifact_with_settings(settings: &str) -> String {
    let settings_uri = format!(
        "data:application/yaml;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(settings)
    );
    format!(
        "data:text/plain;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(settings_uri)
    )
}

pub(crate) async fn mock_raindex_registry_url_with_settings_and_tokens(
    settings: &str,
    remote_tokens: &str,
) -> String {
    let settings = settings.to_string();
    let remote_tokens = remote_tokens.to_string();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock registry server");
    let addr = listener.local_addr().expect("mock registry server address");

    let registry_body = format!("http://{addr}/settings.yaml");
    let settings_body = settings.replace("__TOKENS_URL__", &format!("http://{addr}/tokens.json"));

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };

            let registry_body = registry_body.clone();
            let settings_body = settings_body.clone();
            let remote_tokens = remote_tokens.clone();

            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let n = tokio::io::AsyncReadExt::read(&mut socket, &mut buf)
                    .await
                    .unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);

                let body = if request.contains("/settings.yaml") {
                    &settings_body
                } else if request.contains("/tokens.json") {
                    &remote_tokens
                } else {
                    &registry_body
                };

                let response = format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes()).await;
            });
        }
    });

    format!("http://{addr}/registry.txt")
}

pub(crate) async fn seed_api_key(client: &Client) -> (String, String) {
    let key_id = uuid::Uuid::new_v4().to_string();
    let secret = uuid::Uuid::new_v4().to_string();
    let hash = crate::auth::hash_secret(&secret).expect("hash secret");

    let pool = client
        .rocket()
        .state::<crate::db::DbPool>()
        .expect("pool in state");
    sqlx::query("INSERT INTO api_keys (key_id, secret_hash, label, owner) VALUES (?, ?, ?, ?)")
        .bind(&key_id)
        .bind(&hash)
        .bind("test-key")
        .bind("test-owner")
        .execute(pool)
        .await
        .expect("insert api key");

    (key_id, secret)
}

pub(crate) async fn seed_admin_key(client: &Client) -> (String, String) {
    let key_id = uuid::Uuid::new_v4().to_string();
    let secret = uuid::Uuid::new_v4().to_string();
    let hash = crate::auth::hash_secret(&secret).expect("hash secret");

    let pool = client
        .rocket()
        .state::<crate::db::DbPool>()
        .expect("pool in state");
    sqlx::query(
        "INSERT INTO api_keys (key_id, secret_hash, label, owner, is_admin) VALUES (?, ?, ?, ?, 1)",
    )
    .bind(&key_id)
    .bind(&hash)
    .bind("admin-key")
    .bind("admin-owner")
    .execute(pool)
    .await
    .expect("insert admin api key");

    (key_id, secret)
}

pub(crate) fn basic_auth_header(key_id: &str, secret: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{key_id}:{secret}"));
    format!("Basic {encoded}")
}

fn stub_raindex_client() -> serde_json::Value {
    json!({
        "raindex_yaml": {
            "documents": ["version: 6\nnetworks:\n  base:\n    rpcs:\n      - https://mainnet.base.org\n    chain-id: 8453\n    currency: ETH\nsubgraphs:\n  base: https://example.com/sg\nraindexes:\n  base:\n    address: 0xd2938e7c9fe3597f78832ce780feb61945c377d7\n    network: base\n    subgraph: base\n    deployment-block: 0\ndeployers:\n  base:\n    address: 0xC1A14cE2fd58A3A2f99deCb8eDd866204eE07f8D\n    network: base\n"],
            "profile": "strict"
        }
    })
}

pub(crate) fn order_json() -> serde_json::Value {
    let rc = stub_raindex_client();
    json!({
        "raindexClient": rc,
        "chainId": 8453,
        "id": "0x0000000000000000000000000000000000000000000000000000000000000001",
        "orderBytes": "0x01",
        "orderHash": "0x000000000000000000000000000000000000000000000000000000000000abcd",
        "owner": "0x0000000000000000000000000000000000000001",
        "raindex": "0xd2938e7c9fe3597f78832ce780feb61945c377d7",
        "active": true,
        "timestampAdded": "0x000000000000000000000000000000000000000000000000000000006553f100",
        "meta": null,
        "parsedMeta": [],
        "rainlang": null,
        "transaction": {
            "id": "0x0000000000000000000000000000000000000000000000000000000000000099",
            "from": "0x0000000000000000000000000000000000000001",
            "blockNumber": "0x0000000000000000000000000000000000000000000000000000000000000001",
            "timestamp": "0x000000000000000000000000000000000000000000000000000000006553f100"
        },
        "tradesCount": 0,
        "inputs": [{
            "raindexClient": rc,
            "chainId": 8453,
            "vaultType": "input",
            "id": "0x01",
            "owner": "0x0000000000000000000000000000000000000001",
            "vaultId": "0x0000000000000000000000000000000000000000000000000000000000000001",
            "balance": "0x0000000000000000000000000000000000000000000000000000000000000001",
            "formattedBalance": "1.000000",
            "token": {
                "chainId": 8453,
                "id": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                "address": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                "name": "USD Coin",
                "symbol": "USDC",
                "decimals": 6
            },
            "raindex": "0xd2938e7c9fe3597f78832ce780feb61945c377d7",
            "ordersAsInputs": [],
            "ordersAsOutputs": []
        }],
        "outputs": [{
            "raindexClient": rc,
            "chainId": 8453,
            "vaultType": "output",
            "id": "0x02",
            "owner": "0x0000000000000000000000000000000000000001",
            "vaultId": "0x0000000000000000000000000000000000000000000000000000000000000002",
            "balance": "0xffffffff00000000000000000000000000000000000000000000000000000005",
            "formattedBalance": "0.500000000000000000",
            "token": {
                "chainId": 8453,
                "id": "0x4200000000000000000000000000000000000006",
                "address": "0x4200000000000000000000000000000000000006",
                "name": "Wrapped Ether",
                "symbol": "WETH",
                "decimals": 18
            },
            "raindex": "0xd2938e7c9fe3597f78832ce780feb61945c377d7",
            "ordersAsInputs": [],
            "ordersAsOutputs": []
        }]
    })
}

pub(crate) fn mock_order() -> RaindexOrder {
    serde_json::from_value(order_json()).expect("deserialize mock RaindexOrder")
}

pub(crate) fn mock_candidate(max_output: &str, ratio: &str) -> TakeOrderCandidate {
    let token_a = Address::from([4u8; 20]);
    let token_b = Address::from([5u8; 20]);
    TakeOrderCandidate {
        raindex: Address::from([0xAAu8; 20]),
        order: OrderV4 {
            owner: Address::from([1u8; 20]),
            nonce: U256::from(1).into(),
            evaluable: EvaluableV4 {
                interpreter: Address::from([2u8; 20]),
                store: Address::from([3u8; 20]),
                bytecode: alloy::primitives::Bytes::from(vec![0x01, 0x02]),
            },
            validInputs: vec![IOV2 {
                token: token_a,
                vaultId: U256::from(100).into(),
            }],
            validOutputs: vec![IOV2 {
                token: token_b,
                vaultId: U256::from(200).into(),
            }],
        },
        input_io_index: 0,
        output_io_index: 0,
        max_output: Float::parse(max_output.to_string()).unwrap(),
        ratio: Float::parse(ratio.to_string()).unwrap(),
        signed_context: vec![],
    }
}
