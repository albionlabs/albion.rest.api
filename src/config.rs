use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Deserialize)]
pub struct Config {
    pub log_dir: String,
    pub database_url: String,
    pub database_max_connections: u32,
    pub usage_log_max_concurrency: usize,
    pub response_cache_max_entries: u64,
    pub response_cache_ttl_seconds: u64,
    pub registry_url: String,
    pub private_registry_path: String,
    pub allow_registry_fallback: bool,
    pub rate_limit_global_rpm: u64,
    pub rate_limit_per_key_rpm: u64,
    pub docs_dir: String,
    pub local_db_path: String,
    /// Threshold for `/sync/status`. A chain is reported as `fresh` (HTTP 200)
    /// only if its last_synced block timestamp is within this many seconds of
    /// wall-clock now. Default 300s = 5 minutes, comfortably above the
    /// sync interval configured upstream.
    #[serde(default = "default_sync_freshness_threshold_seconds")]
    pub sync_freshness_threshold_seconds: u64,
    /// Network key -> additional RPC URL templates. Templates may reference env
    /// vars via `${VAR}`; URLs whose vars are unset are dropped at startup with a
    /// warning. Configured here so the registry's `settings.yaml` can stay free
    /// of secrets and per-deployment endpoints.
    #[serde(default)]
    pub additional_rpcs: HashMap<String, Vec<String>>,
}

fn default_sync_freshness_threshold_seconds() -> u64 {
    300
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let contents =
            std::fs::read_to_string(path).map_err(|e| format!("failed to read config: {e}"))?;
        toml::from_str(&contents).map_err(|e| format!("failed to parse config: {e}"))
    }
}

/// Resolves `${VAR}` placeholders against the process environment. URLs whose
/// vars are missing or empty are dropped (with a `tracing::warn`) so a missing
/// secret in dev never takes the production path down — the resulting list may
/// be empty and is the caller's responsibility to handle.
pub fn resolve_rpc_overrides(raw: &HashMap<String, Vec<String>>) -> HashMap<String, Vec<String>> {
    raw.iter()
        .map(|(network, urls)| {
            let resolved: Vec<String> = urls
                .iter()
                .filter_map(|template| match substitute_env(template) {
                    Ok(url) => Some(url),
                    Err(missing) => {
                        tracing::warn!(
                            network = %network,
                            template = %template,
                            missing_var = %missing,
                            "skipping additional RPC: env var not set"
                        );
                        None
                    }
                })
                .collect();
            (network.clone(), resolved)
        })
        .collect()
}

/// Replaces every `${VAR}` occurrence with `std::env::var(VAR)`. Returns the
/// first missing var name on failure so the caller can log it. Unbalanced `${`
/// is left as-is (no error) — operators can verify the rendered URL via logs.
fn substitute_env(template: &str) -> Result<String, String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return Ok(out);
        };
        let var = &after[..end];
        match std::env::var(var) {
            Ok(value) if !value.is_empty() => out.push_str(&value),
            _ => return Err(var.to_string()),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_env_replaces_var() {
        std::env::set_var("ALBION_TEST_RPC_KEY", "abc123");
        assert_eq!(
            substitute_env("https://x.com/v2/${ALBION_TEST_RPC_KEY}").unwrap(),
            "https://x.com/v2/abc123"
        );
        std::env::remove_var("ALBION_TEST_RPC_KEY");
    }

    #[test]
    fn substitute_env_returns_missing_var() {
        std::env::remove_var("ALBION_TEST_MISSING");
        assert_eq!(
            substitute_env("https://x.com/${ALBION_TEST_MISSING}").unwrap_err(),
            "ALBION_TEST_MISSING"
        );
    }

    #[test]
    fn substitute_env_treats_empty_as_missing() {
        std::env::set_var("ALBION_TEST_EMPTY", "");
        assert_eq!(
            substitute_env("https://x.com/${ALBION_TEST_EMPTY}").unwrap_err(),
            "ALBION_TEST_EMPTY"
        );
        std::env::remove_var("ALBION_TEST_EMPTY");
    }

    #[test]
    fn substitute_env_passthrough_when_no_placeholder() {
        assert_eq!(
            substitute_env("https://mainnet.base.org").unwrap(),
            "https://mainnet.base.org"
        );
    }

    #[test]
    fn substitute_env_unbalanced_brace_left_intact() {
        assert_eq!(
            substitute_env("https://x.com/${UNCLOSED").unwrap(),
            "https://x.com/${UNCLOSED"
        );
    }

    #[test]
    fn resolve_drops_urls_with_missing_vars_keeps_others() {
        std::env::set_var("ALBION_TEST_PRESENT", "k");
        std::env::remove_var("ALBION_TEST_ABSENT");
        let mut raw = HashMap::new();
        raw.insert(
            "base".to_string(),
            vec![
                "https://mainnet.base.org".to_string(),
                "https://a.example/${ALBION_TEST_PRESENT}".to_string(),
                "https://b.example/${ALBION_TEST_ABSENT}".to_string(),
            ],
        );
        let resolved = resolve_rpc_overrides(&raw);
        let urls = resolved.get("base").unwrap();
        assert_eq!(
            urls,
            &vec![
                "https://mainnet.base.org".to_string(),
                "https://a.example/k".to_string(),
            ]
        );
        std::env::remove_var("ALBION_TEST_PRESENT");
    }
}
