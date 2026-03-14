use crate::types::*;
use agent_first_data::cli_parse_log_filters;
use std::path::Path;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn enabled_features() -> Vec<&'static str> {
    let features: &[Option<&str>] = &[
        #[cfg(feature = "redb")]
        Some("redb"),
        #[cfg(feature = "postgres")]
        Some("postgres"),
        #[cfg(feature = "cashu")]
        Some("cashu"),
        #[cfg(feature = "ln-nwc")]
        Some("ln-nwc"),
        #[cfg(feature = "ln-phoenixd")]
        Some("ln-phoenixd"),
        #[cfg(feature = "ln-lnbits")]
        Some("ln-lnbits"),
        #[cfg(feature = "sol")]
        Some("sol"),
        #[cfg(feature = "evm")]
        Some("evm"),
        #[cfg(feature = "btc-esplora")]
        Some("btc-esplora"),
        #[cfg(feature = "btc-core")]
        Some("btc-core"),
        #[cfg(feature = "btc-electrum")]
        Some("btc-electrum"),
        #[cfg(feature = "interactive")]
        Some("interactive"),
        #[cfg(feature = "rest")]
        Some("rest"),
    ];
    features.iter().copied().flatten().collect()
}

/// Single source of truth for startup log — always includes env.features.
pub fn build_startup_log(
    argv: Option<Vec<String>>,
    config: Option<&RuntimeConfig>,
    args: serde_json::Value,
) -> Output {
    Output::Log {
        event: "startup".to_string(),
        request_id: None,
        version: Some(VERSION.to_string()),
        argv,
        config: config.map(|c| serde_json::to_value(c).unwrap_or(serde_json::Value::Null)),
        args: Some(args),
        env: Some(serde_json::json!({
            "features": enabled_features(),
        })),
        trace: Trace::from_duration(0),
    }
}

/// Decide whether startup log should be emitted for this process.
/// Startup is emitted when explicit startup logging is requested or any log filter is set.
pub fn should_emit_startup_log(log_filters: &[String], startup_requested: bool) -> bool {
    startup_requested || !log_filters.is_empty()
}

/// Unified startup log builder + gate used by all runtime modes.
pub fn maybe_startup_log(
    log_filters: &[String],
    startup_requested: bool,
    argv: Option<Vec<String>>,
    config: Option<&RuntimeConfig>,
    args: serde_json::Value,
) -> Option<Output> {
    if !should_emit_startup_log(log_filters, startup_requested) {
        return None;
    }
    Some(build_startup_log(argv, config, args))
}

impl RuntimeConfig {
    /// Load config from `{data_dir}/config.toml`. Falls back to defaults if file missing.
    pub fn load_from_dir(data_dir: &str) -> Result<Self, String> {
        let path = Path::new(data_dir).join("config.toml");
        if !path.exists() {
            return Ok(Self {
                data_dir: data_dir.to_string(),
                ..Self::default()
            });
        }
        let contents =
            std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let mut cfg: Self =
            toml::from_str(&contents).map_err(|e| format!("parse {}: {e}", path.display()))?;
        // Ensure data_dir reflects the actual directory (config file may omit it)
        cfg.data_dir = data_dir.to_string();
        Ok(cfg)
    }

    #[allow(dead_code)]
    pub fn apply_update(&mut self, patch: ConfigPatch) {
        if let Some(v) = patch.data_dir {
            self.data_dir = v;
        }
        if let Some(v) = patch.limits {
            self.limits = v;
        }
        if let Some(v) = patch.log {
            self.log = cli_parse_log_filters(&v);
        }
        if let Some(rpc_nodes) = patch.afpay_rpc {
            for (name, cfg) in rpc_nodes {
                self.afpay_rpc.insert(name, cfg);
            }
        }
        if let Some(providers) = patch.providers {
            for (network, rpc_name) in providers {
                self.providers.insert(network, rpc_name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maybe_startup_log_disabled_without_filters_or_request() {
        let out = maybe_startup_log(&[], false, None, None, serde_json::json!({"mode": "test"}));
        assert!(out.is_none());
    }

    #[test]
    fn maybe_startup_log_enabled_with_filters() {
        let filters = vec!["cashu".to_string()];
        let out = maybe_startup_log(
            &filters,
            false,
            None,
            None,
            serde_json::json!({"mode": "test"}),
        );
        assert!(out.is_some());
    }

    #[test]
    fn maybe_startup_log_enabled_with_explicit_request() {
        let out = maybe_startup_log(&[], true, None, None, serde_json::json!({"mode": "test"}));
        assert!(out.is_some());
    }
}
