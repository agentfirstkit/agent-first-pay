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
        }
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

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ConfigPatch {
    #[serde(default)]
    pub data_dir: Option<String>,
    #[serde(default)]
    pub log: Option<Vec<String>>,
    #[serde(default)]
    pub exchange_rate: Option<ExchangeRateConfig>,
    #[serde(default)]
    pub afpay_rpc: Option<std::collections::HashMap<String, AfpayRpcConfig>>,
    #[serde(default)]
    pub providers: Option<std::collections::HashMap<String, String>>,
}
