use agent_first_data::document::{
    KeyedList, Value as AfValue, coerce_values_toward, get_path, set_path,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct RuntimeConfig {
    #[serde(default)]
    pub data_dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_secret: Option<String>,
    #[serde(default)]
    pub log: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exchange_rate: Option<ExchangeRateConfig>,
    /// Named afpay RPC nodes (e.g. `[afpay_rpc.wallet-server]`).
    #[serde(default)]
    pub afpay_rpc: std::collections::HashMap<String, AfpayRpcConfig>,
    /// Network → afpay_rpc node name (omit = local provider).
    #[serde(default)]
    pub providers: std::collections::HashMap<String, String>,
    /// Storage backend: "redb" (default) or "postgres".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_backend: Option<String>,
    /// PostgreSQL connection URL (used when storage_backend = "postgres").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postgres_url_secret: Option<String>,
    /// Rate limiting for REST/RPC endpoints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimitConfig>,
    /// Whitelist of allowed Cashu mint URLs. Empty = no restriction (current
    /// default to preserve existing behaviour). When non-empty, wallet_create
    /// rejects any mint_url not on this list. Entries are matched by
    /// `url_allowed`: exact scheme+host+port, with path-boundary prefix scoping.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_mint_urls: Vec<String>,
    /// Whitelist of allowed BTC Esplora URLs. Empty = no restriction. Entries
    /// are matched by `url_allowed`: exact scheme+host+port, with path-boundary
    /// prefix scoping. `https://mempool.space/api` covers `/api/v1/...` but not
    /// `https://mempool.space/apidocs/...` or a different host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_esplora_urls: Vec<String>,
    /// Whitelist of allowed Solana RPC endpoints. Empty = no restriction.
    /// Entries are matched by `url_allowed`. Applies to both `wallet_create`
    /// and `wallet_config_set`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_sol_rpc_endpoints: Vec<String>,
    /// Whitelist of allowed EVM RPC endpoints. Empty = no restriction.
    /// Entries are matched by `url_allowed`. Applies to both `wallet_create`
    /// and `wallet_config_set`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_evm_rpc_endpoints: Vec<String>,
    /// Whitelist of allowed Bitcoin Core RPC URLs. Empty = no restriction.
    /// Matched by `url_allowed`. Without this an agent can point a BTC
    /// wallet at an attacker-controlled `bitcoind`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_btc_core_urls: Vec<String>,
    /// Whitelist of allowed BTC Electrum server URLs. Empty = no restriction.
    /// Matched by `url_allowed`. Without this an agent-supplied Electrum
    /// server can lie about UTXOs and broadcast to anywhere.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_btc_electrum_urls: Vec<String>,
    /// Whitelist of allowed Lightning backend endpoints (NWC / phoenixd /
    /// lnbits). Empty = no restriction. Matched by `url_allowed`. Without
    /// this `LnWalletCreate.request.endpoint` flows into the wallet
    /// unchecked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_ln_endpoints: Vec<String>,
}

const KEYED_LISTS: &[KeyedList] = &[];

impl RuntimeConfig {
    /// Read one config key. Fields ending with `_secret` are masked as `"***"`.
    /// Omit key (pass `""`) to return the full serialized config.
    pub fn get_key(&self, key: &str) -> Result<serde_json::Value, String> {
        if key.is_empty() {
            return serde_json::to_value(self).map_err(|e| e.to_string());
        }
        // Mask secret fields before they leave the process.
        if key
            .split('.')
            .next_back()
            .is_some_and(|seg| seg.ends_with("_secret"))
        {
            return Ok(serde_json::json!("***"));
        }
        let root = self.as_af_value()?;
        let val =
            get_path(&root, key, KEYED_LISTS).map_err(|e| format!("config key {key}: {e}"))?;
        Ok(serde_json::Value::from(val))
    }

    /// Set one config key and persist to `{data_dir}/config.toml`.
    pub fn set_key(&mut self, key: &str, values: &[String]) -> Result<(), String> {
        if values.is_empty() {
            return Err("config set requires at least one value".to_string());
        }
        if key == "data_dir" {
            return Err("data_dir is set at startup, not persisted in config".to_string());
        }
        if !self.apply_domain_override(key, values)? {
            let mut root = self.as_af_value()?;
            let existing = get_path(&root, key, KEYED_LISTS).ok();
            let coerced = coerce_values_toward(values, existing.as_ref())
                .map_err(|e| format!("config key {key}: {e}"))?;
            set_path(&mut root, key, &coerced, KEYED_LISTS)
                .map_err(|e| format!("config key {key}: {e}"))?;
            let json = serde_json::Value::from(root);
            *self = serde_json::from_value(json).map_err(|e| {
                let msg = e.to_string();
                let hint = msg.split(" at line ").next().unwrap_or(&msg);
                format!("config key {key}: {hint}")
            })?;
        }
        self.save()
    }

    /// Persist the current config to `{data_dir}/config.toml`.
    pub fn save(&self) -> Result<(), String> {
        let path = std::path::Path::new(&self.data_dir).join("config.toml");
        // Serialize without data_dir (it is derived at load time, not stored).
        let mut value = serde_json::to_value(self).map_err(|e| e.to_string())?;
        if let Some(obj) = value.as_object_mut() {
            obj.remove("data_dir");
        }
        let toml_str = toml::to_string_pretty(&value).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&self.data_dir).map_err(|e| format!("create data dir: {e}"))?;
        std::fs::write(&path, toml_str).map_err(|e| format!("write {}: {e}", path.display()))
    }

    fn apply_domain_override(&mut self, key: &str, values: &[String]) -> Result<bool, String> {
        match key {
            "log" => {
                let joined = values.join(",");
                let entries: Vec<&str> = joined.split(',').collect();
                self.log = agent_first_data::cli_parse_log_filters(&entries)
                    .as_slice()
                    .to_vec();
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn as_af_value(&self) -> Result<AfValue, String> {
        let json = serde_json::to_value(self).map_err(|e| e.to_string())?;
        Ok(AfValue::from(json))
    }
}

impl std::fmt::Debug for RuntimeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeConfig")
            .field("data_dir", &self.data_dir)
            .field("rpc_endpoint", &self.rpc_endpoint)
            .field("rpc_secret", &self.rpc_secret.as_ref().map(|_| "***"))
            .field("log", &self.log)
            .field("exchange_rate", &self.exchange_rate)
            .field("afpay_rpc", &self.afpay_rpc)
            .field("providers", &self.providers)
            .field("storage_backend", &self.storage_backend)
            .field(
                "postgres_url_secret",
                &self.postgres_url_secret.as_ref().map(|_| "***"),
            )
            .field("rate_limit", &self.rate_limit)
            .field("allowed_mint_urls", &self.allowed_mint_urls)
            .field("allowed_esplora_urls", &self.allowed_esplora_urls)
            .field("allowed_sol_rpc_endpoints", &self.allowed_sol_rpc_endpoints)
            .field("allowed_evm_rpc_endpoints", &self.allowed_evm_rpc_endpoints)
            .finish()
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            rpc_endpoint: None,
            rpc_secret: None,
            log: vec![],
            exchange_rate: None,
            afpay_rpc: std::collections::HashMap::new(),
            providers: std::collections::HashMap::new(),
            storage_backend: None,
            postgres_url_secret: None,
            rate_limit: None,
            allowed_mint_urls: Vec::new(),
            allowed_esplora_urls: Vec::new(),
            allowed_sol_rpc_endpoints: Vec::new(),
            allowed_evm_rpc_endpoints: Vec::new(),
            allowed_btc_core_urls: Vec::new(),
            allowed_btc_electrum_urls: Vec::new(),
            allowed_ln_endpoints: Vec::new(),
        }
    }
}

/// Reject `url` if it is not covered by `allowlist`. Empty `allowlist` means
/// "no restriction" (the operator did not opt in).
///
/// A candidate URL matches an allowlist entry when:
/// 1. Both parse as `scheme://host[:port][path]` (no userinfo, no query, no fragment).
/// 2. Schemes and hosts are equal (case-insensitive on both).
/// 3. Effective ports are equal — explicit port if set, else the scheme default
///    (`https` → 443, `http` → 80).
/// 4. The candidate path lies under the allowlist entry's path:
///    - empty / `/` matches any path,
///    - trailing `/` is an explicit prefix scope,
///    - otherwise exact match or a `/`-boundary match (so `/api` covers `/api`
///      and `/api/v1` but NOT `/api2`).
///
/// Allowlist entries with userinfo, query, or fragment are rejected. The
/// previous bare `starts_with` check let `https://mint.example` match
/// `https://mint.example.evil/...`; this matcher closes that bypass.
pub fn url_allowed(url: &str, allowlist: &[String]) -> bool {
    if allowlist.is_empty() {
        return true;
    }
    let Some(candidate) = ParsedUrl::parse(url, /* allow_query_fragment */ true) else {
        return false;
    };
    allowlist.iter().any(|entry| {
        let Some(allowed) = ParsedUrl::parse(entry, /* allow_query_fragment */ false) else {
            return false;
        };
        candidate.same_origin_as(&allowed) && path_under(&allowed.path, &candidate.path)
    })
}

#[derive(Debug)]
struct ParsedUrl {
    scheme: String, // lowercased
    host: String,   // lowercased (no IDN normalization)
    port: Option<u16>,
    path: String, // starts with '/' or empty
}

impl ParsedUrl {
    fn parse(s: &str, allow_query_fragment: bool) -> Option<Self> {
        let (scheme, rest) = s.split_once("://")?;
        if scheme.is_empty() {
            return None;
        }
        let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(auth_end);
        if authority.is_empty() || authority.contains('@') {
            return None;
        }
        let (host, port) = if authority.starts_with('[') {
            let bracket_end = authority.find(']')?;
            let host_part = &authority[..=bracket_end];
            let after = &authority[bracket_end + 1..];
            let port = if after.is_empty() {
                None
            } else {
                let p = after.strip_prefix(':')?;
                Some(p.parse::<u16>().ok()?)
            };
            (host_part, port)
        } else if let Some(idx) = authority.rfind(':') {
            let h = &authority[..idx];
            let p_str = &authority[idx + 1..];
            let p: u16 = p_str.parse().ok()?;
            (h, Some(p))
        } else {
            (authority, None)
        };
        if host.is_empty() {
            return None;
        }
        let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
        let (path, query_frag) = tail.split_at(path_end);
        if !allow_query_fragment && !query_frag.is_empty() {
            return None;
        }
        Some(Self {
            scheme: scheme.to_ascii_lowercase(),
            host: host.to_ascii_lowercase(),
            port,
            path: path.to_string(),
        })
    }

    fn effective_port(&self) -> Option<u16> {
        self.port.or(match self.scheme.as_str() {
            "https" => Some(443),
            "http" => Some(80),
            _ => None,
        })
    }

    fn same_origin_as(&self, other: &ParsedUrl) -> bool {
        self.scheme == other.scheme
            && self.host == other.host
            && self.effective_port() == other.effective_port()
    }
}

fn path_under(allowed: &str, candidate: &str) -> bool {
    let allowed_norm = if allowed.is_empty() { "/" } else { allowed };
    let candidate_norm = if candidate.is_empty() { "/" } else { candidate };
    if allowed_norm == "/" {
        return true;
    }
    if candidate_norm == allowed_norm {
        return true;
    }
    if let Some(stripped) = allowed_norm.strip_suffix('/') {
        // Explicit prefix scope. Trailing-slash entry includes its own dir.
        if candidate_norm == stripped {
            return true;
        }
        return candidate_norm.starts_with(allowed_norm);
    }
    // Path-boundary match: candidate starts with allowed AND next char is '/'.
    candidate_norm
        .strip_prefix(allowed_norm)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// Active operator allowlist policy. `from_config` reads sizes off
/// `RuntimeConfig` so REST/RPC startup can print a one-line banner and
/// `require_for_public_listen` can refuse to bind when `--public-listen` is
/// set but the operator has not opted into any allowlist.
pub struct AllowlistPolicy {
    pub mints: usize,
    pub esplora: usize,
    pub sol_rpc: usize,
    pub evm_rpc: usize,
    pub btc_core: usize,
    pub btc_electrum: usize,
    pub ln_endpoints: usize,
}

impl AllowlistPolicy {
    pub fn from_config(cfg: &RuntimeConfig) -> Self {
        Self {
            mints: cfg.allowed_mint_urls.len(),
            esplora: cfg.allowed_esplora_urls.len(),
            sol_rpc: cfg.allowed_sol_rpc_endpoints.len(),
            evm_rpc: cfg.allowed_evm_rpc_endpoints.len(),
            btc_core: cfg.allowed_btc_core_urls.len(),
            btc_electrum: cfg.allowed_btc_electrum_urls.len(),
            ln_endpoints: cfg.allowed_ln_endpoints.len(),
        }
    }

    pub fn any_set(&self) -> bool {
        self.mints
            + self.esplora
            + self.sol_rpc
            + self.evm_rpc
            + self.btc_core
            + self.btc_electrum
            + self.ln_endpoints
            > 0
    }

    /// Returns `Err` with a human-readable message when `--public-listen`
    /// is set but the operator has not opted into ANY allowlist. Empty
    /// allowlists are acceptable on localhost (laptop mode) but not when the
    /// daemon is bound to a public address: any reachable agent could then
    /// point a new wallet at an attacker-controlled mint/esplora/RPC node.
    pub fn require_for_public_listen(&self) -> Result<(), String> {
        if self.any_set() {
            return Ok(());
        }
        Err(
            "--public-listen requires a non-empty operator allowlist (allowed_mint_urls, allowed_esplora_urls, allowed_sol_rpc_endpoints, allowed_evm_rpc_endpoints, allowed_btc_core_urls, allowed_btc_electrum_urls, or allowed_ln_endpoints in runtime config)".to_string(),
        )
    }

    /// One-line banner suitable for daemon startup. Operators see at a glance
    /// what categories are restricted and that the policy is fail-closed.
    pub fn banner(&self) -> String {
        format!(
            "allowlist: mints={} esplora={} sol_rpc={} evm_rpc={} btc_core={} btc_electrum={} ln={} (fail-closed: non-empty list blocks anything off-list)",
            self.mints,
            self.esplora,
            self.sol_rpc,
            self.evm_rpc,
            self.btc_core,
            self.btc_electrum,
            self.ln_endpoints,
        )
    }
}

fn default_data_dir() -> String {
    // AFPAY_HOME takes priority, then ~/.afpay
    if let Some(val) = std::env::var_os("AFPAY_HOME") {
        return std::path::PathBuf::from(val).to_string_lossy().into_owned();
    }
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = std::path::PathBuf::from(home);
        p.push(".afpay");
        p.to_string_lossy().into_owned()
    } else {
        ".afpay".to_string()
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct AfpayRpcConfig {
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_secret: Option<String>,
}

impl std::fmt::Debug for AfpayRpcConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AfpayRpcConfig")
            .field("endpoint", &self.endpoint)
            .field(
                "endpoint_secret",
                &self.endpoint_secret.as_ref().map(|_| "***"),
            )
            .finish()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ExchangeRateConfig {
    #[serde(default = "default_exchange_rate_ttl_s")]
    pub ttl_s: u64,
    #[serde(default = "default_exchange_rate_sources")]
    pub sources: Vec<ExchangeRateSource>,
}

impl Default for ExchangeRateConfig {
    fn default() -> Self {
        Self {
            ttl_s: default_exchange_rate_ttl_s(),
            sources: default_exchange_rate_sources(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ExchangeRateSource {
    #[serde(rename = "type")]
    pub source_type: ExchangeRateSourceType,
    pub endpoint: String,
    #[serde(default, alias = "api_key", skip_serializing_if = "Option::is_none")]
    pub api_key_secret: Option<String>,
}

impl std::fmt::Debug for ExchangeRateSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExchangeRateSource")
            .field("source_type", &self.source_type)
            .field("endpoint", &self.endpoint)
            .field(
                "api_key_secret",
                &self.api_key_secret.as_ref().map(|_| "***"),
            )
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExchangeRateSourceType {
    Generic,
    CoinGecko,
    Kraken,
}

/// Rate limiting configuration for REST/RPC endpoints.
///
/// ```toml
/// [rate_limit]
/// requests_per_second = 20
/// max_concurrent = 50
/// ```
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RateLimitConfig {
    /// Maximum requests per second (token-bucket refill rate). 0 = unlimited.
    #[serde(default = "default_rate_limit_rps")]
    pub requests_per_second: u32,
    /// Maximum concurrent in-flight requests. 0 = unlimited.
    #[serde(default = "default_rate_limit_concurrent")]
    pub max_concurrent: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests_per_second: default_rate_limit_rps(),
            max_concurrent: default_rate_limit_concurrent(),
        }
    }
}

fn default_rate_limit_rps() -> u32 {
    20
}

fn default_rate_limit_concurrent() -> u32 {
    50
}

fn default_exchange_rate_ttl_s() -> u64 {
    300
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn empty_allowlist_permits_everything() {
        assert!(url_allowed("https://anything.example", &[]));
        assert!(url_allowed("http://127.0.0.1:3000", &[]));
        assert!(url_allowed("file:///etc/passwd", &[]));
    }

    #[test]
    fn non_empty_allowlist_blocks_unknown_urls() {
        let allow = vec!["https://mempool.space/api".to_string()];
        assert!(url_allowed("https://mempool.space/api", &allow));
        assert!(url_allowed("https://mempool.space/api/v1/blocks", &allow));
        assert!(!url_allowed("https://attacker.example/api", &allow));
        assert!(!url_allowed("http://mempool.space/api", &allow)); // wrong scheme
    }

    #[test]
    fn allowlist_supports_multiple_entries() {
        let allow = vec![
            "https://mint-a.example".to_string(),
            "https://mint-b.example".to_string(),
        ];
        assert!(url_allowed("https://mint-a.example/v1", &allow));
        assert!(url_allowed("https://mint-b.example/v1", &allow));
        assert!(!url_allowed("https://mint-c.example/v1", &allow));
    }

    #[test]
    fn rejects_host_prefix_bypass() {
        let allow = vec!["https://mint.example".to_string()];
        assert!(!url_allowed("https://mint.example.evil/path", &allow));
        assert!(!url_allowed("https://mint.examplecorp.com/", &allow));
        assert!(!url_allowed("https://evil.mint.example.evil/path", &allow));
    }

    #[test]
    fn scheme_and_host_case_insensitive() {
        let allow = vec!["https://Mint.Example".to_string()];
        assert!(url_allowed("HTTPS://MINT.EXAMPLE/path", &allow));
        assert!(url_allowed("https://mint.example/v1", &allow));
    }

    #[test]
    fn localhost_and_127_not_equivalent() {
        let allow = vec!["http://localhost:9400".to_string()];
        assert!(url_allowed("http://localhost:9400/rpc", &allow));
        assert!(!url_allowed("http://127.0.0.1:9400/rpc", &allow));
        let allow = vec!["http://127.0.0.1:9400".to_string()];
        assert!(!url_allowed("http://localhost:9400/rpc", &allow));
    }

    #[test]
    fn explicit_default_port_matches_implicit() {
        let allow = vec!["https://mint.example".to_string()];
        assert!(url_allowed("https://mint.example:443/v1", &allow));
        let allow = vec!["https://mint.example:443".to_string()];
        assert!(url_allowed("https://mint.example/v1", &allow));
    }

    #[test]
    fn non_default_port_blocks() {
        let allow = vec!["https://mint.example".to_string()];
        assert!(!url_allowed("https://mint.example:8443/v1", &allow));
        let allow = vec!["https://mint.example:8443".to_string()];
        assert!(url_allowed("https://mint.example:8443/v1", &allow));
        assert!(!url_allowed("https://mint.example/v1", &allow));
    }

    #[test]
    fn path_boundary_match() {
        let allow = vec!["https://mempool.space/api".to_string()];
        assert!(url_allowed("https://mempool.space/api", &allow));
        assert!(url_allowed("https://mempool.space/api/v1/blocks", &allow));
        // Sibling path that just shares a textual prefix must NOT match.
        assert!(!url_allowed("https://mempool.space/api2/v1", &allow));
        assert!(!url_allowed("https://mempool.space/api-private/", &allow));
    }

    #[test]
    fn trailing_slash_is_explicit_prefix() {
        let allow = vec!["https://mempool.space/api/".to_string()];
        // The dir itself, and anything under it.
        assert!(url_allowed("https://mempool.space/api/", &allow));
        assert!(url_allowed("https://mempool.space/api/v1", &allow));
        assert!(url_allowed("https://mempool.space/api", &allow));
        // Different sibling does not match.
        assert!(!url_allowed("https://mempool.space/apiv2/", &allow));
    }

    #[test]
    fn entries_with_userinfo_or_query_are_rejected() {
        let bad = vec![
            "https://user:pw@mint.example".to_string(),
            "https://mint.example?x=1".to_string(),
            "https://mint.example#frag".to_string(),
        ];
        // Every entry parses to None → no match → reject.
        assert!(!url_allowed("https://mint.example/v1", &bad));
    }

    #[test]
    fn allowlist_policy_banner_lists_all_categories() {
        let cfg = RuntimeConfig {
            allowed_mint_urls: vec!["https://mint.example".to_string()],
            allowed_esplora_urls: vec![],
            allowed_sol_rpc_endpoints: vec!["https://sol.example".to_string()],
            allowed_evm_rpc_endpoints: vec!["https://evm.example".to_string()],
            ..RuntimeConfig::default()
        };
        let p = AllowlistPolicy::from_config(&cfg);
        let banner = p.banner();
        assert!(banner.contains("mints=1"));
        assert!(banner.contains("esplora=0"));
        assert!(banner.contains("sol_rpc=1"));
        assert!(banner.contains("evm_rpc=1"));
        assert!(banner.contains("fail-closed"));
        assert!(p.any_set());
    }

    #[test]
    fn allowlist_policy_require_for_public_listen_rejects_empty() {
        let cfg = RuntimeConfig::default();
        let p = AllowlistPolicy::from_config(&cfg);
        assert!(!p.any_set());
        let err = p
            .require_for_public_listen()
            .expect_err("empty policy must fail public-listen");
        assert!(err.contains("--public-listen"));
        assert!(err.contains("allowed_mint_urls"));
    }

    #[test]
    fn allowlist_policy_require_for_public_listen_allows_any_set() {
        let cfg = RuntimeConfig {
            allowed_mint_urls: vec!["https://mint.example".to_string()],
            ..RuntimeConfig::default()
        };
        assert!(
            AllowlistPolicy::from_config(&cfg)
                .require_for_public_listen()
                .is_ok()
        );
    }

    #[test]
    fn malformed_candidate_url_rejected() {
        let allow = vec!["https://mint.example".to_string()];
        assert!(!url_allowed("not a url", &allow));
        assert!(!url_allowed("://nohost", &allow));
        assert!(!url_allowed("https://", &allow));
    }
}

fn default_exchange_rate_sources() -> Vec<ExchangeRateSource> {
    vec![
        ExchangeRateSource {
            source_type: ExchangeRateSourceType::Kraken,
            endpoint: "https://api.kraken.com".to_string(),
            api_key_secret: None,
        },
        ExchangeRateSource {
            source_type: ExchangeRateSourceType::CoinGecko,
            endpoint: "https://api.coingecko.com/api/v3".to_string(),
            api_key_secret: None,
        },
    ]
}
