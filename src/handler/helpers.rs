use crate::provider::{PayError, PayProvider};
use crate::store::wallet;
use crate::store::{PayStore, StorageBackend};
use crate::types::*;
use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::mpsc;

use super::App;

/// Try each provider until one succeeds. Skips NotImplemented.
/// Collect results from all providers, skipping NotImplemented.
macro_rules! collect_all {
    ($providers:expr, |$p:ident| $call:expr) => {{
        let mut _all = Vec::new();
        let mut _err: Option<PayError> = None;
        for _prov in $providers.values() {
            let $p = _prov.as_ref();
            match $call.await {
                Ok(mut items) => _all.append(&mut items),
                Err(PayError::NotImplemented { .. }) => {}
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

/// Reject `url` if it is not covered by the operator allowlist for `kind`.
/// The hint points the caller at the exact RuntimeConfig key to widen.
pub(crate) fn validate_url_in_allowlist(
    url: &str,
    allowlist: &[String],
    kind: &str,
    config_key: &str,
) -> Result<(), PayError> {
    if url_allowed(url, allowlist) {
        return Ok(());
    }
    Err(PayError::Forbidden {
        message: format!("{kind} {url} is not in the operator allowlist"),
        hint: Some(format!(
            "add to runtime config `{config_key}` or use a {kind} that is already allowed"
        )),
    })
}

/// Heuristic detection of a Solana cluster from an RPC endpoint URL. Matches
/// the official cluster hostnames; returns `None` for unknown hosts (e.g.
/// self-hosted RPC, Quicknode, Triton). The send-time cluster check uses
/// `None` as "skip — no opinion".
pub(crate) fn sol_cluster_from_endpoint(endpoint: &str) -> Option<&'static str> {
    let lower = endpoint.to_ascii_lowercase();
    // Order matters: "mainnet-beta" before bare "mainnet" if we ever add it.
    if lower.contains("devnet.solana.com") {
        Some("devnet")
    } else if lower.contains("testnet.solana.com") {
        Some("testnet")
    } else if lower.contains("mainnet-beta.solana.com") || lower.contains("api.mainnet-beta") {
        Some("mainnet-beta")
    } else {
        None
    }
}

/// Validate every RPC endpoint URL against the operator allowlist for the
/// given network. Empty allowlists keep the existing "anything goes" behaviour.
/// Returns `PayError::Forbidden` on the first URL that fails so the caller can
/// point the agent at the offending entry.
pub(crate) fn validate_rpc_endpoints(
    cfg: &RuntimeConfig,
    network: Network,
    endpoints: &[String],
) -> Result<(), PayError> {
    let (allowlist, config_key) = match network {
        Network::Sol => (
            cfg.allowed_sol_rpc_endpoints.as_slice(),
            "allowed_sol_rpc_endpoints",
        ),
        Network::Evm => (
            cfg.allowed_evm_rpc_endpoints.as_slice(),
            "allowed_evm_rpc_endpoints",
        ),
        // Other networks do not (currently) accept agent-supplied RPC endpoints
        // through this code path; there is nothing to validate.
        _ => return Ok(()),
    };
    for url in endpoints {
        validate_url_in_allowlist(url, allowlist, "rpc endpoint", config_key)?;
    }
    Ok(())
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
            retry_after_ms: None,
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
        | Input::ReconcileReservation { id, .. }
        | Input::WalletConfigShow { id, .. }
        | Input::WalletConfigSet { id, .. }
        | Input::WalletConfigTokenAdd { id, .. }
        | Input::WalletConfigTokenRemove { id, .. } => Some(id.clone()),
        Input::ConfigGet { .. }
        | Input::ConfigSet { .. }
        | Input::Version
        | Input::Schema
        | Input::Close => None,
    }
}

pub(crate) fn trace_from(start: Instant) -> Trace {
    Trace::from_duration(start.elapsed().as_millis() as u64)
}

/// Stable hex blake3 hash of the "what is this payment moving" subset of a
/// Send/CashuSend. Two Send requests with the same idempotency_key are only
/// considered equivalent if this hash matches — that way a daemon refuses to
/// replay when the agent forgot to bump the key after editing the body, and
/// agents who send the exact same body twice safely replay the first response.
///
/// Excludes: `id` (request correlation, varies per call), `idempotency_key`
/// (it IS the key), `dry_run` (validation-only doesn't affect identity).
pub(crate) fn canonical_send_hash(input: &Input) -> Option<String> {
    let value = match input {
        Input::Send {
            wallet,
            network,
            to,
            amount,
            onchain_memo,
            local_memo,
            mints,
            chain_id,
            ..
        } => serde_json::json!({
            "kind": "send",
            "wallet": wallet,
            "network": network,
            "to": to,
            "amount": amount,
            "onchain_memo": onchain_memo,
            "local_memo": local_memo,
            "mints": mints,
            "chain_id": chain_id,
        }),
        Input::CashuSend {
            wallet,
            amount,
            onchain_memo,
            local_memo,
            mints,
            ..
        } => serde_json::json!({
            "kind": "cashu_send",
            "wallet": wallet,
            "amount": amount,
            "onchain_memo": onchain_memo,
            "local_memo": local_memo,
            "mints": mints,
        }),
        _ => return None,
    };
    let bytes = serde_json::to_vec(&value).ok()?;
    Some(hex::encode(blake3::hash(&bytes).as_bytes()))
}

/// Query limits from each unique downstream afpay_rpc node.
#[cfg(feature = "rpc")]
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

/// Stub when rpc feature is disabled.
#[cfg(not(feature = "rpc"))]
pub(crate) async fn query_downstream_limits(_config: &RuntimeConfig) -> Vec<DownstreamLimitNode> {
    Vec::new()
}

/// Extract `token=<value>` from a transfer target URI query string.
pub(crate) fn extract_token_from_target(to: &str) -> Option<String> {
    let query = to.split('?').nth(1)?;
    for part in query.split('&') {
        if let Some(val) = part.strip_prefix("token=")
            && !val.is_empty()
        {
            return Some(val.to_string());
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

pub(crate) async fn resolve_wallet_for_provider(
    app: &App,
    wallet: Option<&str>,
    network: Option<Network>,
) -> Result<(Network, String), PayError> {
    let wallet = wallet.map(str::trim).filter(|value| !value.is_empty());

    if let Some(wallet_id) = wallet {
        if let Some(store) = app.store.as_deref()
            && let Ok(meta) = store.load_wallet_metadata(wallet_id)
        {
            if let Some(expected) = network
                && meta.network != expected
            {
                return Err(PayError::invalid_amount(format!(
                    "wallet {wallet_id} is {}, not {expected}",
                    meta.network
                )));
            }
            return Ok((meta.network, meta.id));
        }

        if let Some(expected) = network {
            // Remote/coordinator mode: the wallet may exist only on the downstream node.
            return Ok((expected, wallet_id.to_string()));
        }

        let matches = provider_wallet_candidates(app, None, Some(wallet_id)).await?;
        return select_wallet_candidate(matches, network, Some(wallet_id));
    }

    let local = local_wallet_candidates(app, network)?;
    if !local.is_empty() {
        return select_wallet_candidate(local, network, None);
    }

    let remote = provider_wallet_candidates(app, network, None).await?;
    select_wallet_candidate(remote, network, None)
}

fn local_wallet_candidates(
    app: &App,
    network: Option<Network>,
) -> Result<Vec<(Network, String)>, PayError> {
    let Some(store) = app.store.as_deref() else {
        return Ok(Vec::new());
    };
    let wallets = store.list_wallet_metadata(network)?;
    Ok(wallets
        .into_iter()
        .map(|meta| (meta.network, meta.id))
        .collect())
}

async fn provider_wallet_candidates(
    app: &App,
    network: Option<Network>,
    wallet_or_label: Option<&str>,
) -> Result<Vec<(Network, String)>, PayError> {
    let mut candidates = Vec::new();
    let mut first_error: Option<PayError> = None;

    for (network_key, provider) in &app.providers {
        if let Some(expected) = network
            && *network_key != expected
        {
            continue;
        }
        match provider.list_wallets().await {
            Ok(wallets) => {
                for wallet in wallets {
                    if let Some(needle) = wallet_or_label
                        && wallet.id != needle
                        && wallet.label.as_deref() != Some(needle)
                    {
                        continue;
                    }
                    candidates.push((wallet.network, wallet.id));
                }
            }
            Err(PayError::NotImplemented { .. }) | Err(PayError::WalletNotFound { .. }) => {}
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
    }

    if candidates.is_empty()
        && let Some(e) = first_error
    {
        return Err(e);
    }
    Ok(candidates)
}

fn select_wallet_candidate(
    candidates: Vec<(Network, String)>,
    network: Option<Network>,
    requested_wallet: Option<&str>,
) -> Result<(Network, String), PayError> {
    match candidates.len() {
        0 => {
            let msg = match (network, requested_wallet) {
                (_, Some(wallet)) => format!("wallet {wallet} not found"),
                (Some(network), None) => format!("no {network} wallet found"),
                (None, None) => "no wallet found".to_string(),
            };
            Err(PayError::wallet_not_found(msg))
        }
        1 => Ok(candidates[0].clone()),
        n => {
            let msg = match (network, requested_wallet) {
                (_, Some(wallet)) => {
                    format!("wallet '{wallet}' matches {n} wallets; pass wallet ID and --network")
                }
                (Some(network), None) => format!("multiple {network} wallets found; pass --wallet"),
                (None, None) => "multiple wallets found; pass --wallet".to_string(),
            };
            Err(PayError::invalid_amount(msg))
        }
    }
}

pub(crate) fn log_enabled(log: &agent_first_data::LogFilters, event: &str) -> bool {
    log.enabled(event)
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
    let log_filters = agent_first_data::LogFilters::new(log);
    if !log_enabled(&log_filters, event) {
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
        .ok_or_else(|| PayError::not_implemented("no storage backend available".to_string()))
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
        .map_err(|e| PayError::internal_error(format!("lock task: {e}")))?
        .map_err(PayError::internal_error)?;
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
        if let Some(val) = w.as_mut()
            && !val.starts_with("w_")
        {
            *val = store.resolve_wallet_id(val)?;
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn cfg_with(sol: Vec<String>, evm: Vec<String>) -> RuntimeConfig {
        RuntimeConfig {
            allowed_sol_rpc_endpoints: sol,
            allowed_evm_rpc_endpoints: evm,
            ..RuntimeConfig::default()
        }
    }

    #[test]
    fn validate_rpc_endpoints_empty_allowlist_permits_anything() {
        let cfg = RuntimeConfig::default();
        let result = validate_rpc_endpoints(
            &cfg,
            Network::Sol,
            &["https://api.devnet.solana.com".to_string()],
        );
        assert!(result.is_ok());
    }

    #[test]
    fn validate_rpc_endpoints_sol_blocks_unlisted() {
        let cfg = cfg_with(
            vec!["https://api.mainnet-beta.solana.com".to_string()],
            vec![],
        );
        let err = validate_rpc_endpoints(
            &cfg,
            Network::Sol,
            &["https://attacker.example/rpc".to_string()],
        )
        .expect_err("attacker endpoint must be rejected");
        assert_eq!(err.error_code(), "forbidden");
        assert!(format!("{err}").contains("allowlist"));
    }

    #[test]
    fn validate_rpc_endpoints_sol_permits_listed() {
        let cfg = cfg_with(
            vec!["https://api.mainnet-beta.solana.com".to_string()],
            vec![],
        );
        let result = validate_rpc_endpoints(
            &cfg,
            Network::Sol,
            &["https://api.mainnet-beta.solana.com/v1".to_string()],
        );
        assert!(result.is_ok());
    }

    #[test]
    fn validate_rpc_endpoints_evm_uses_evm_allowlist() {
        let cfg = cfg_with(
            vec!["https://api.mainnet-beta.solana.com".to_string()],
            vec!["https://mainnet.base.org".to_string()],
        );
        // EVM endpoint that matches sol allowlist must still fail for evm.
        let err = validate_rpc_endpoints(
            &cfg,
            Network::Evm,
            &["https://api.mainnet-beta.solana.com/rpc".to_string()],
        )
        .expect_err("evm uses evm allowlist, not sol");
        assert_eq!(err.error_code(), "forbidden");
        // Listed evm endpoint passes.
        assert!(
            validate_rpc_endpoints(
                &cfg,
                Network::Evm,
                &["https://mainnet.base.org".to_string()],
            )
            .is_ok()
        );
    }

    #[test]
    fn validate_rpc_endpoints_rejects_first_bad_url() {
        let cfg = cfg_with(vec!["https://good.example".to_string()], vec![]);
        let err = validate_rpc_endpoints(
            &cfg,
            Network::Sol,
            &[
                "https://good.example/rpc".to_string(),
                "https://bad.example/rpc".to_string(),
            ],
        )
        .expect_err("any bad url should fail");
        assert!(format!("{err}").contains("bad.example"));
    }

    #[test]
    fn sol_cluster_from_endpoint_matches_official_hostnames() {
        assert_eq!(
            sol_cluster_from_endpoint("https://api.mainnet-beta.solana.com"),
            Some("mainnet-beta"),
        );
        assert_eq!(
            sol_cluster_from_endpoint("https://api.devnet.solana.com"),
            Some("devnet"),
        );
        assert_eq!(
            sol_cluster_from_endpoint("https://api.testnet.solana.com"),
            Some("testnet"),
        );
        assert_eq!(
            sol_cluster_from_endpoint("HTTPS://API.MAINNET-BETA.SOLANA.COM/v1"),
            Some("mainnet-beta"),
        );
    }

    #[test]
    fn sol_cluster_from_endpoint_unknown_hosts_yield_none() {
        assert_eq!(
            sol_cluster_from_endpoint("https://rpc.quicknode.com/abc"),
            None,
        );
        assert_eq!(sol_cluster_from_endpoint("http://localhost:8899"), None,);
    }

    #[test]
    fn validate_rpc_endpoints_other_networks_skip_check() {
        // BTC/LN/Cashu have no RPC-endpoint allowlist; validator should no-op.
        let cfg = cfg_with(vec!["https://only-sol.example".to_string()], vec![]);
        assert!(
            validate_rpc_endpoints(
                &cfg,
                Network::Btc,
                &["https://anything.example/api".to_string()],
            )
            .is_ok()
        );
        assert!(
            validate_rpc_endpoints(&cfg, Network::Ln, &["https://anything.example".to_string()],)
                .is_ok()
        );
        assert!(
            validate_rpc_endpoints(
                &cfg,
                Network::Cashu,
                &["https://anything.example".to_string()],
            )
            .is_ok()
        );
    }
}
