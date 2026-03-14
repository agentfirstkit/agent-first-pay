use crate::provider::{PayError, PayProvider};
use crate::store::wallet;
use crate::store::{PayStore, StorageBackend};
use crate::types::*;
use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::mpsc;

use super::App;

/// Try each provider until one succeeds. Skips NotImplemented, stops on first real result.
macro_rules! try_provider {
    ($providers:expr, |$p:ident| $call:expr) => {{
        let mut _result: Option<Result<_, PayError>> = None;
        for _prov in $providers.values() {
            let $p = _prov.as_ref();
            match $call.await {
                Ok(v) => {
                    _result = Some(Ok(v));
                    break;
                }
                Err(PayError::NotImplemented(_)) => continue,
                Err(e) => {
                    _result = Some(Err(e));
                    break;
                }
            }
        }
        match _result {
            Some(r) => r,
            None => Err(PayError::NotImplemented("network not enabled".to_string())),
        }
    }};
}

/// Collect results from all providers, skipping NotImplemented.
macro_rules! collect_all {
    ($providers:expr, |$p:ident| $call:expr) => {{
        let mut _all = Vec::new();
        let mut _err: Option<PayError> = None;
        for _prov in $providers.values() {
            let $p = _prov.as_ref();
            match $call.await {
                Ok(mut items) => _all.append(&mut items),
                Err(PayError::NotImplemented(_)) => {}
                Err(e) => {
                    _err = Some(e);
                    break;
                }
            }
        }
        match _err {
            Some(e) => Err(e),
            None => Ok(_all),
        }
    }};
}

pub(crate) fn get_provider(
    providers: &HashMap<Network, Box<dyn PayProvider>>,
    network: Network,
) -> Option<&dyn PayProvider> {
    providers.get(&network).map(|p| p.as_ref())
}

pub(crate) fn looks_like_bip39_mnemonic(secret: &str) -> bool {
    let words = secret.split_whitespace().count();
    words == 12 || words == 24
}

pub(crate) fn evm_receive_token_matches(expected: &str, observed: &str) -> bool {
    let expected = expected.trim().to_ascii_lowercase();
    let observed = observed.trim().to_ascii_lowercase();
    if expected == "native" {
        return observed == "native" || observed == "gwei" || observed == "wei";
    }
    if observed == expected {
        return true;
    }
    if let Some(stripped) = observed.strip_suffix("_base_units") {
        return stripped == expected;
    }
    false
}

pub(crate) async fn emit_error(
    writer: &mpsc::Sender<Output>,
    id: Option<String>,
    err: &PayError,
    start: Instant,
) {
    emit_error_hint(writer, id, err, start, None).await;
}

/// Like [`emit_error`] but with an optional hint override.
/// When `hint_override` is `Some`, it takes precedence over `PayError::hint()`.
pub(crate) async fn emit_error_hint(
    writer: &mpsc::Sender<Output>,
    id: Option<String>,
    err: &PayError,
    start: Instant,
    hint_override: Option<&str>,
) {
    let _ = writer
        .send(Output::Error {
            id,
            error_code: err.error_code().to_string(),
            error: err.to_string(),
            hint: hint_override.map(|h| h.to_string()).or_else(|| err.hint()),
            retryable: err.retryable(),
            trace: trace_from(start),
        })
        .await;
}

pub(crate) fn extract_id(input: &Input) -> Option<String> {
    match input {
        Input::WalletCreate { id, .. }
        | Input::LnWalletCreate { id, .. }
        | Input::WalletClose { id, .. }
        | Input::WalletList { id, .. }
        | Input::Balance { id, .. }
        | Input::Receive { id, .. }
        | Input::ReceiveClaim { id, .. }
        | Input::CashuSend { id, .. }
        | Input::CashuReceive { id, .. }
        | Input::Send { id, .. }
        | Input::Restore { id, .. }
        | Input::WalletShowSeed { id, .. }
        | Input::HistoryList { id, .. }
        | Input::HistoryStatus { id, .. }
        | Input::HistoryUpdate { id, .. }
        | Input::LimitAdd { id, .. }
        | Input::LimitRemove { id, .. }
        | Input::LimitList { id, .. }
        | Input::LimitSet { id, .. }
        | Input::WalletConfigShow { id, .. }
        | Input::WalletConfigSet { id, .. }
        | Input::WalletConfigTokenAdd { id, .. }
        | Input::WalletConfigTokenRemove { id, .. } => Some(id.clone()),
        Input::Config(_) | Input::Version | Input::Close => None,
    }
}

pub(crate) fn trace_from(start: Instant) -> Trace {
    Trace::from_duration(start.elapsed().as_millis() as u64)
}

/// Query limits from each unique downstream afpay_rpc node.
pub(crate) async fn query_downstream_limits(config: &RuntimeConfig) -> Vec<DownstreamLimitNode> {
    let mut result = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (name, rpc_cfg) in &config.afpay_rpc {
        if !seen.insert(rpc_cfg.endpoint.clone()) {
            continue;
        }
        let secret = rpc_cfg.endpoint_secret.as_deref().unwrap_or("");
        let limit_input = Input::LimitList {
            id: format!("downstream_{name}"),
        };
        let outputs =
            crate::provider::remote::rpc_call(&rpc_cfg.endpoint, secret, &limit_input).await;
        let mut node = DownstreamLimitNode {
            name: name.clone(),
            endpoint: rpc_cfg.endpoint.clone(),
            limits: vec![],
            error: None,
            downstream: vec![],
        };
        for value in &outputs {
            if value.get("code").and_then(|v| v.as_str()) == Some("error") {
                node.error = value
                    .get("error")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }
            if value.get("code").and_then(|v| v.as_str()) == Some("limit_status") {
                if let Some(limits) = value.get("limits") {
                    node.limits = serde_json::from_value(limits.clone()).unwrap_or_default();
                }
                if let Some(ds) = value.get("downstream") {
                    node.downstream = serde_json::from_value(ds.clone()).unwrap_or_default();
                }
            }
        }
        result.push(node);
    }
    result
}

/// Extract `token=<value>` from a transfer target URI query string.
pub(crate) fn extract_token_from_target(to: &str) -> Option<String> {
    let query = to.split('?').nth(1)?;
    for part in query.split('&') {
        if let Some(val) = part.strip_prefix("token=") {
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

pub(crate) fn wallet_provider_key(meta: &wallet::WalletMetadata) -> String {
    match meta.network {
        Network::Ln => meta
            .backend
            .as_deref()
            .map(|b| format!("ln-{}", b.to_ascii_lowercase()))
            .unwrap_or_else(|| "ln".to_string()),
        _ => meta.network.to_string(),
    }
}

pub(crate) fn wallet_summary_from_meta(
    meta: &wallet::WalletMetadata,
    wallet_id: &str,
) -> WalletSummary {
    let (address, backend) = match meta.network {
        Network::Cashu => (meta.mint_url.clone().unwrap_or_default(), None),
        Network::Ln => {
            let b = meta
                .backend
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            (format!("ln:{b}"), Some(b))
        }
        _ => (wallet_id.to_string(), None),
    };
    WalletSummary {
        id: meta.id.clone(),
        network: meta.network,
        label: meta.label.clone(),
        address,
        backend,
        mint_url: meta.mint_url.clone(),
        rpc_endpoints: meta
            .sol_rpc_endpoints
            .clone()
            .or(meta.evm_rpc_endpoints.clone()),
        chain_id: meta.evm_chain_id,
        created_at_epoch_s: meta.created_at_epoch_s,
    }
}

pub(crate) async fn resolve_wallet_summary(app: &App, wallet_id: &str) -> WalletSummary {
    if let Ok(meta) = require_store(app).and_then(|s| s.load_wallet_metadata(wallet_id)) {
        return wallet_summary_from_meta(&meta, wallet_id);
    }
    if let Ok(wallets) = collect_all!(&app.providers, |p| p.list_wallets()) {
        if let Some(summary) = wallets.into_iter().find(|w| w.id == wallet_id) {
            return summary;
        }
    }
    WalletSummary {
        id: wallet_id.to_string(),
        network: Network::Ln,
        label: None,
        address: String::new(),
        backend: None,
        mint_url: None,
        rpc_endpoints: None,
        chain_id: None,
        created_at_epoch_s: 0,
    }
}

pub(crate) fn log_enabled(log: &[String], event: &str) -> bool {
    if log.is_empty() {
        return false;
    }
    let ev = event.to_ascii_lowercase();
    log.iter()
        .any(|f| f == "*" || f == "all" || ev.starts_with(f.as_str()))
}

pub(crate) async fn emit_migration_log(app: &App) {
    let entries = app
        .store
        .as_ref()
        .map(|s| s.drain_migration_log())
        .unwrap_or_default();
    if entries.is_empty() {
        return;
    }
    for entry in entries {
        emit_log(
            app,
            "schema_migration",
            None,
            serde_json::json!({
                "database": entry.database,
                "from_version": entry.from_version,
                "to_version": entry.to_version,
            }),
        )
        .await;
    }
}

pub(crate) async fn emit_log(
    app: &App,
    event: &str,
    request_id: Option<String>,
    args: serde_json::Value,
) {
    let log = app.config.read().await.log.clone();
    if !log_enabled(&log, event) {
        return;
    }
    let _ = app
        .writer
        .send(Output::Log {
            event: event.to_string(),
            request_id,
            version: None,
            argv: None,
            config: None,
            args: Some(args),
            env: None,
            trace: Trace::from_duration(0),
        })
        .await;
}

/// Get a reference to the storage backend, or return NotImplemented.
pub(crate) fn require_store(app: &App) -> Result<&StorageBackend, PayError> {
    app.store
        .as_deref()
        .ok_or_else(|| PayError::NotImplemented("no storage backend available".to_string()))
}

/// Acquire the data-directory lock for a write operation.
/// Returns the lock guard (dropped after operation) or emits an error.
#[cfg(feature = "redb")]
pub(crate) async fn acquire_write_lock(
    app: &App,
) -> Result<crate::store::lock::DataLock, PayError> {
    let data_dir = app.config.read().await.data_dir.clone();
    let lock = tokio::task::spawn_blocking(move || crate::store::lock::acquire(&data_dir, None))
        .await
        .map_err(|e| PayError::InternalError(format!("lock task: {e}")))?
        .map_err(PayError::InternalError)?;
    Ok(lock)
}

#[cfg(feature = "redb")]
pub(crate) fn needs_write_lock(input: &Input) -> bool {
    matches!(
        input,
        Input::WalletCreate { .. }
            | Input::LnWalletCreate { .. }
            | Input::WalletClose { .. }
            | Input::Receive { .. }
            | Input::ReceiveClaim { .. }
            | Input::CashuSend { .. }
            | Input::CashuReceive { .. }
            | Input::Send { .. }
            | Input::Restore { .. }
            | Input::LimitAdd { .. }
            | Input::LimitRemove { .. }
            | Input::LimitSet { .. }
            | Input::HistoryUpdate { .. }
            | Input::WalletConfigSet { .. }
            | Input::WalletConfigTokenAdd { .. }
            | Input::WalletConfigTokenRemove { .. }
    )
}

/// Resolve wallet labels to wallet IDs in-place.
/// If a wallet field does not start with "w_", treat it as a label and look it up.
pub(crate) fn resolve_wallet_labels(
    input: &mut Input,
    store: &dyn PayStore,
) -> Result<(), PayError> {
    fn resolve(store: &dyn PayStore, w: &mut String) -> Result<(), PayError> {
        if !w.starts_with("w_") {
            *w = store.resolve_wallet_id(w)?;
        }
        Ok(())
    }
    fn resolve_opt(store: &dyn PayStore, w: &mut Option<String>) -> Result<(), PayError> {
        if let Some(val) = w.as_mut() {
            if !val.starts_with("w_") {
                *val = store.resolve_wallet_id(val)?;
            }
        }
        Ok(())
    }
    match input {
        Input::WalletClose { wallet, .. } => resolve(store, wallet),
        Input::Balance { wallet, .. } => resolve_opt(store, wallet),
        Input::Receive { wallet, .. } => resolve(store, wallet),
        Input::ReceiveClaim { wallet, .. } => resolve(store, wallet),
        Input::CashuSend { wallet, .. } => resolve_opt(store, wallet),
        Input::CashuReceive { wallet, .. } => resolve_opt(store, wallet),
        Input::Send { wallet, .. } => resolve_opt(store, wallet),
        Input::Restore { wallet, .. } => resolve(store, wallet),
        Input::WalletShowSeed { wallet, .. } => resolve(store, wallet),
        Input::HistoryList { wallet, .. } | Input::HistoryUpdate { wallet, .. } => {
            resolve_opt(store, wallet)
        }
        Input::WalletConfigShow { wallet, .. } => resolve(store, wallet),
        Input::WalletConfigSet { wallet, .. } => resolve(store, wallet),
        Input::WalletConfigTokenAdd { wallet, .. } => resolve(store, wallet),
        Input::WalletConfigTokenRemove { wallet, .. } => resolve(store, wallet),
        Input::LimitAdd { limit, .. } => resolve_opt(store, &mut limit.wallet),
        Input::LimitSet { limits, .. } => {
            for limit in limits.iter_mut() {
                resolve_opt(store, &mut limit.wallet)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
