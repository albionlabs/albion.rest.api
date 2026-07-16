use crate::error::ApiError;
use rain_orderbook_app_settings::yaml::{
    raindex::{RaindexYaml, RaindexYamlValidation},
    YamlParsable,
};
use rain_orderbook_common::raindex_client::RaindexClient;
use rain_orderbook_common::registry::DotrainRegistry;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug)]
pub(crate) struct RaindexProvider {
    client: RaindexClient,
    raindex_yaml: RaindexYaml,
    db_path: Option<PathBuf>,
    additional_rpcs: HashMap<String, Vec<String>>,
}

/// Prepends additional RPC URLs to each network's `rpcs:` list. Keys in
/// `overrides` are network names matching `networks:` in the settings YAML;
/// values are the URLs to inject ahead of the existing entries (so newly added
/// reliable endpoints are tried first by alloy's `FallbackLayer`). Networks
/// with no override or with an empty list are left untouched. Unknown network
/// keys in `overrides` are logged and skipped — they're a likely operator typo
/// rather than a fatal error.
fn prepend_rpcs(yaml: &str, overrides: &HashMap<String, Vec<String>>) -> Result<String, String> {
    if overrides.values().all(Vec::is_empty) {
        return Ok(yaml.to_string());
    }

    let mut doc: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|e| format!("parse settings yaml: {e}"))?;

    let networks = doc
        .as_mapping_mut()
        .and_then(|m| m.get_mut(serde_yaml::Value::String("networks".into())))
        .and_then(|n| n.as_mapping_mut())
        .ok_or_else(|| "settings yaml has no `networks` mapping".to_string())?;

    for (network, extras) in overrides {
        if extras.is_empty() {
            continue;
        }
        let key = serde_yaml::Value::String(network.clone());
        let Some(net_value) = networks.get_mut(&key) else {
            tracing::warn!(
                network = %network,
                "additional_rpcs references unknown network; skipping"
            );
            continue;
        };
        let net_map = net_value
            .as_mapping_mut()
            .ok_or_else(|| format!("network `{network}` is not a mapping"))?;

        let rpcs_key = serde_yaml::Value::String("rpcs".into());
        let mut merged: Vec<serde_yaml::Value> = extras
            .iter()
            .map(|s| serde_yaml::Value::String(s.clone()))
            .collect();

        if let Some(existing) = net_map.get(&rpcs_key) {
            if let Some(seq) = existing.as_sequence() {
                for v in seq {
                    if !merged.iter().any(|m| m == v) {
                        merged.push(v.clone());
                    }
                }
            }
        }

        net_map.insert(rpcs_key, serde_yaml::Value::Sequence(merged));
    }

    serde_yaml::to_string(&doc).map_err(|e| format!("serialize settings yaml: {e}"))
}

/// Applies Albion's additional-RPC injection to a raw registry settings YAML
/// string. Shared by every registry load path (configured URL, DB-persisted
/// URL, private artifact) so a rate-limited public RPC can never freeze sync
/// regardless of registry source.
///
/// NOTE: this intentionally leaves the `metaboards` section intact. Upstream's
/// `/v1/tokens/{address}/proofs` route resolves proof metadata via
/// `RaindexYaml::get_metaboard`, so neutralizing metaboards (an old Albion
/// sync-latency optimization) would break it.
fn rewrite_settings(
    settings: &str,
    additional_rpcs: &HashMap<String, Vec<String>>,
) -> Result<String, RaindexProviderError> {
    if additional_rpcs.values().any(|urls| !urls.is_empty()) {
        let rewritten = prepend_rpcs(settings, additional_rpcs)
            .map_err(RaindexProviderError::SettingsRewrite)?;
        let counts: Vec<(String, usize)> = additional_rpcs
            .iter()
            .map(|(k, v)| (k.clone(), v.len()))
            .collect();
        tracing::info!(?counts, "injected additional RPCs into settings");
        Ok(rewritten)
    } else {
        Ok(settings.to_string())
    }
}

impl RaindexProvider {
    pub(crate) async fn load(
        registry_url: &str,
        db_path: Option<PathBuf>,
        additional_rpcs: HashMap<String, Vec<String>>,
    ) -> Result<Self, RaindexProviderError> {
        let url = registry_url.to_string();
        let db = db_path.clone();
        let extras = additional_rpcs;

        let (tx, rx) = tokio::sync::oneshot::channel();

        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(RaindexProviderError::RegistryLoad(e.to_string())));
                    return;
                }
            };

            let result = runtime.block_on(async {
                let registry = DotrainRegistry::new(url)
                    .await
                    .map_err(|e| RaindexProviderError::RegistryLoad(e.to_string()))?;

                // Rewrite the registry-provided settings before building any
                // client/yaml: inject additional RPCs so alloy's FallbackLayer
                // has more transports to rotate over and we're not pinned to a
                // single rate-limited endpoint.
                let settings = rewrite_settings(&registry.settings(), &extras)?;

                let client = RaindexClient::new(vec![settings.clone()], None, db.clone())
                    .await
                    .map_err(|e| RaindexProviderError::ClientInit(e.to_string()))?;
                let raindex_yaml =
                    RaindexYaml::new(vec![settings], RaindexYamlValidation::default())
                        .map_err(|e| RaindexProviderError::RegistryLoad(e.to_string()))?;

                Ok(RaindexProvider {
                    client,
                    raindex_yaml,
                    db_path: db,
                    additional_rpcs: extras,
                })
            });

            let _ = tx.send(result);
        });

        rx.await.map_err(|_| RaindexProviderError::WorkerPanicked)?
    }

    pub(crate) fn client(&self) -> &RaindexClient {
        &self.client
    }

    pub(crate) fn raindex_yaml(&self) -> &RaindexYaml {
        &self.raindex_yaml
    }

    pub(crate) fn db_path(&self) -> Option<PathBuf> {
        self.db_path.clone()
    }

    pub(crate) fn additional_rpcs(&self) -> HashMap<String, Vec<String>> {
        self.additional_rpcs.clone()
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RaindexProviderError {
    #[error("failed to load registry: {0}")]
    RegistryLoad(String),
    #[error("failed to create raindex client: {0}")]
    ClientInit(String),
    #[error("failed to rewrite settings yaml: {0}")]
    SettingsRewrite(String),
    #[error("worker thread panicked")]
    WorkerPanicked,
}

impl From<RaindexProviderError> for ApiError {
    fn from(e: RaindexProviderError) -> Self {
        tracing::error!(error = %e.safe_summary(), "raindex client provider error");
        match e {
            RaindexProviderError::RegistryLoad(_) => {
                ApiError::Internal("registry configuration error".into())
            }
            RaindexProviderError::ClientInit(_) => {
                ApiError::Internal("failed to initialize orderbook client".into())
            }
            RaindexProviderError::SettingsRewrite(_) => {
                ApiError::Internal("failed to apply settings overrides".into())
            }
            RaindexProviderError::WorkerPanicked => {
                ApiError::Internal("failed to initialize client runtime".into())
            }
        }
    }
}

impl RaindexProviderError {
    pub(crate) fn safe_summary(&self) -> &'static str {
        match self {
            RaindexProviderError::RegistryLoad(_) => "registry load failed",
            RaindexProviderError::ClientInit(_) => "raindex client initialization failed",
            RaindexProviderError::SettingsRewrite(_) => "settings rewrite failed",
            RaindexProviderError::WorkerPanicked => "worker thread panicked",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rocket::async_test]
    async fn test_load_fails_with_unreachable_url() {
        let result =
            RaindexProvider::load("http://127.0.0.1:1/registry.txt", None, HashMap::new()).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RaindexProviderError::RegistryLoad(_)
        ));
    }

    #[rocket::async_test]
    async fn test_load_fails_with_invalid_format() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let body = "this is not a valid registry file format";
        let response = format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes()).await;
        });

        let result =
            RaindexProvider::load(&format!("http://{addr}/registry.txt"), None, HashMap::new())
                .await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RaindexProviderError::RegistryLoad(_)
        ));
    }

    #[rocket::async_test]
    async fn test_load_succeeds_with_valid_registry() {
        crate::test_helpers::mock_raindex_config().await;
    }

    #[test]
    fn test_error_maps_to_api_error() {
        let err = RaindexProviderError::RegistryLoad("test".into());
        let api_err: ApiError = err.into();
        assert!(
            matches!(api_err, ApiError::Internal(msg) if msg == "registry configuration error")
        );

        let err = RaindexProviderError::ClientInit("test".into());
        let api_err: ApiError = err.into();
        assert!(
            matches!(api_err, ApiError::Internal(msg) if msg == "failed to initialize orderbook client")
        );

        let err = RaindexProviderError::SettingsRewrite("bad yaml".into());
        let api_err: ApiError = err.into();
        assert!(
            matches!(api_err, ApiError::Internal(msg) if msg == "failed to apply settings overrides")
        );
    }

    fn parse_rpcs(yaml: &str, network: &str) -> Vec<String> {
        let doc: serde_yaml::Value = serde_yaml::from_str(yaml).expect("parse yaml");
        doc["networks"][network]["rpcs"]
            .as_sequence()
            .expect("rpcs sequence")
            .iter()
            .map(|v| v.as_str().expect("rpc string").to_string())
            .collect()
    }

    #[test]
    fn prepend_rpcs_inserts_extras_before_existing() {
        let yaml = "\
networks:
  base:
    rpcs:
      - https://base-rpc.publicnode.com
    chain-id: 8453
";
        let mut overrides = HashMap::new();
        overrides.insert(
            "base".into(),
            vec![
                "https://base-mainnet.g.alchemy.com/v2/k".into(),
                "https://mainnet.base.org".into(),
            ],
        );
        let out = prepend_rpcs(yaml, &overrides).unwrap();
        assert_eq!(
            parse_rpcs(&out, "base"),
            vec![
                "https://base-mainnet.g.alchemy.com/v2/k",
                "https://mainnet.base.org",
                "https://base-rpc.publicnode.com",
            ]
        );
    }

    #[test]
    fn prepend_rpcs_dedupes_duplicates() {
        let yaml = "\
networks:
  base:
    rpcs:
      - https://mainnet.base.org
    chain-id: 8453
";
        let mut overrides = HashMap::new();
        overrides.insert(
            "base".into(),
            vec![
                "https://mainnet.base.org".into(),
                "https://x.example".into(),
            ],
        );
        let out = prepend_rpcs(yaml, &overrides).unwrap();
        let rpcs = parse_rpcs(&out, "base");
        assert_eq!(rpcs.len(), 2);
        assert_eq!(rpcs[0], "https://mainnet.base.org");
        assert_eq!(rpcs[1], "https://x.example");
    }

    #[test]
    fn prepend_rpcs_unknown_network_skipped() {
        let yaml = "\
networks:
  base:
    rpcs:
      - https://mainnet.base.org
    chain-id: 8453
";
        let mut overrides = HashMap::new();
        overrides.insert("ethereum".into(), vec!["https://eth.example".into()]);
        let out = prepend_rpcs(yaml, &overrides).unwrap();
        assert_eq!(parse_rpcs(&out, "base"), vec!["https://mainnet.base.org"]);
        assert!(!out.contains("https://eth.example"));
    }

    #[test]
    fn prepend_rpcs_creates_rpcs_when_absent() {
        let yaml = "\
networks:
  base:
    chain-id: 8453
";
        let mut overrides = HashMap::new();
        overrides.insert("base".into(), vec!["https://x.example".into()]);
        let out = prepend_rpcs(yaml, &overrides).unwrap();
        assert_eq!(parse_rpcs(&out, "base"), vec!["https://x.example"]);
    }

    #[test]
    fn prepend_rpcs_empty_overrides_passthrough() {
        let yaml = "\
networks:
  base:
    rpcs:
      - https://mainnet.base.org
    chain-id: 8453
";
        let out = prepend_rpcs(yaml, &HashMap::new()).unwrap();
        assert_eq!(out, yaml);

        let mut empty_for_known = HashMap::new();
        empty_for_known.insert("base".into(), Vec::new());
        let out = prepend_rpcs(yaml, &empty_for_known).unwrap();
        assert_eq!(out, yaml);
    }

    #[test]
    fn prepend_rpcs_rejects_invalid_yaml() {
        let mut overrides = HashMap::new();
        overrides.insert("base".into(), vec!["https://x.example".into()]);
        assert!(prepend_rpcs(": :", &overrides).is_err());
    }

    #[test]
    fn prepend_rpcs_errors_when_networks_missing() {
        let mut overrides = HashMap::new();
        overrides.insert("base".into(), vec!["https://x.example".into()]);
        assert!(prepend_rpcs("version: 4\n", &overrides).is_err());
    }

    #[test]
    fn rewrite_settings_injects_rpcs_and_preserves_metaboards() {
        let yaml = "\
version: 4
networks:
  base:
    rpcs:
      - https://mainnet.base.org
    chain-id: 8453
metaboards:
  base: https://api.goldsky.com/metaboard
";
        let mut overrides = HashMap::new();
        overrides.insert("base".into(), vec!["https://drpc.example/base".into()]);
        let out = rewrite_settings(yaml, &overrides).unwrap();
        // Metaboards must survive — token proofs depend on them.
        assert!(out.contains("api.goldsky.com/metaboard"));
        assert_eq!(
            parse_rpcs(&out, "base"),
            vec!["https://drpc.example/base", "https://mainnet.base.org"]
        );
    }

    #[test]
    fn rewrite_settings_passthrough_when_no_overrides() {
        let yaml = "version: 4\nnetworks:\n  base:\n    chain-id: 8453\n";
        let out = rewrite_settings(yaml, &HashMap::new()).unwrap();
        assert_eq!(out, yaml);
    }
}
