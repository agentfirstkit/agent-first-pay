#[cfg(any(
    feature = "btc-esplora",
    feature = "btc-core",
    feature = "btc-electrum"
))]
use crate::provider::btc::BtcProvider;
#[cfg(feature = "cashu")]
use crate::provider::cashu::CashuProvider;
#[cfg(feature = "evm")]
use crate::provider::evm::EvmProvider;
#[cfg(any(feature = "ln-nwc", feature = "ln-phoenixd", feature = "ln-lnbits"))]
use crate::provider::ln::LnProvider;
use crate::provider::remote::RemoteProvider;
#[cfg(feature = "sol")]
use crate::provider::sol::SolProvider;
use crate::provider::{HistorySyncStats, PayError, PayProvider, StubProvider};
use crate::spend::{SpendContext, SpendLedger};
#[cfg(feature = "redb")]
use crate::store::lock;
use crate::store::wallet;
use crate::store::{PayStore, StorageBackend};
use crate::types::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::sleep;

const DEFAULT_WAIT_TIMEOUT_SECS: u64 = 300;
const DEFAULT_WAIT_POLL_INTERVAL_MS: u64 = 1000;

pub struct App {
    pub config: RwLock<RuntimeConfig>,
    pub providers: HashMap<Network, Box<dyn PayProvider>>,
    pub writer: mpsc::Sender<Output>,
    pub in_flight: Mutex<HashMap<String, JoinHandle<()>>>,
    pub requests_total: AtomicU64,
    pub start_time: Instant,
    /// True if any provider uses local data (needs data-dir lock for writes).
    pub has_local_providers: bool,
    /// Whether this node enforces spend limits.
    /// RPC mode: always true. CLI/pipe with all remote: false. CLI/pipe with any local: true.
    pub enforce_limits: bool,
    pub spend_ledger: SpendLedger,
    /// Storage backend for wallet metadata and transaction history.
    /// None when running in frontend-only mode (no local DB, only remote RPC).
    pub store: Option<Arc<StorageBackend>>,
}

impl App {
    /// Create a new App. If `enforce_limits_override` is Some, use that value;
    /// otherwise auto-detect: enforce if any provider writes locally.
    pub fn new(
        config: RuntimeConfig,
        writer: mpsc::Sender<Output>,
        enforce_limits_override: Option<bool>,
        store: Option<StorageBackend>,
    ) -> Self {
        let store = store.map(Arc::new);
        let mut providers: HashMap<Network, Box<dyn PayProvider>> = HashMap::new();

        for network in &[
            Network::Ln,
            Network::Sol,
            Network::Evm,
            Network::Cashu,
            Network::Btc,
        ] {
            let key = network.to_string();
            if let Some(rpc_name) = config.providers.get(&key) {
                // Look up the afpay_rpc node by name
                if let Some(rpc_cfg) = config.afpay_rpc.get(rpc_name) {
                    let secret = rpc_cfg.endpoint_secret.as_deref().unwrap_or("");
                    providers.insert(
                        *network,
                        Box::new(RemoteProvider::new(&rpc_cfg.endpoint, secret, *network)),
                    );
                } else {
                    // Unknown afpay_rpc name — insert stub so errors surface at runtime
                    providers.insert(*network, Box::new(StubProvider::new(*network)));
                }
            } else {
                #[allow(unreachable_patterns)]
                match network {
                    #[cfg(feature = "cashu")]
                    Network::Cashu => {
                        if let Some(s) = &store {
                            let pg_url = config
                                .postgres_url_secret
                                .clone()
                                .filter(|_| config.storage_backend.as_deref() == Some("postgres"));
                            providers.insert(
                                *network,
                                Box::new(CashuProvider::new(&config.data_dir, pg_url, s.clone())),
                            );
                        } else {
                            providers.insert(*network, Box::new(StubProvider::new(*network)));
                        }
                    }
                    #[cfg(any(feature = "ln-nwc", feature = "ln-phoenixd", feature = "ln-lnbits"))]
                    Network::Ln => {
                        providers.insert(*network, Box::new(LnProvider::new(&config.data_dir)));
                    }
                    #[cfg(feature = "sol")]
                    Network::Sol => {
                        providers.insert(*network, Box::new(SolProvider::new(&config.data_dir)));
                    }
                    #[cfg(feature = "evm")]
                    Network::Evm => {
                        providers.insert(*network, Box::new(EvmProvider::new(&config.data_dir)));
                    }
                    #[cfg(any(
                        feature = "btc-esplora",
                        feature = "btc-core",
                        feature = "btc-electrum"
                    ))]
                    Network::Btc => {
                        providers.insert(*network, Box::new(BtcProvider::new(&config.data_dir)));
                    }
                    _ => {
                        providers.insert(*network, Box::new(StubProvider::new(*network)));
                    }
                }
            }
        }

        let has_local = providers.values().any(|p| p.writes_locally());
        let spend_ledger = match store.as_deref() {
            #[cfg(feature = "postgres")]
            Some(StorageBackend::Postgres(pg)) => {
                SpendLedger::new_postgres(pg.pool().clone(), config.exchange_rate.clone())
            }
            _ => SpendLedger::new(&config.data_dir, config.exchange_rate.clone()),
        };
        Self {
            config: RwLock::new(config),
            providers,
            writer,
            in_flight: Mutex::new(HashMap::new()),
            requests_total: AtomicU64::new(0),
            start_time: Instant::now(),
            has_local_providers: has_local,
            enforce_limits: enforce_limits_override.unwrap_or(has_local),
            spend_ledger,
            store,
        }
    }
}

/// Unified startup validation for long-lived modes.
/// Pings all configured remote afpay_rpc nodes (deduplicated) and validates provider mappings.
pub async fn startup_provider_validation_errors(config: &RuntimeConfig) -> Vec<Output> {
    let mut errors = Vec::new();

    // Validate that all provider values reference known afpay_rpc names
    for (network, rpc_name) in &config.providers {
        if !config.afpay_rpc.contains_key(rpc_name) {
            errors.push(Output::Error {
                id: None,
                error_code: "invalid_config".to_string(),
                error: format!(
                    "providers.{network} references unknown afpay_rpc node '{rpc_name}'"
                ),
                hint: Some(format!(
                    "add [afpay_rpc.{rpc_name}] with endpoint and endpoint_secret to config.toml"
                )),
                retryable: false,
                trace: Trace::from_duration(0),
            });
        }
    }
    if !errors.is_empty() {
        return errors;
    }

    // Ping each unique afpay_rpc endpoint once
    let mut pinged: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (rpc_name, rpc_cfg) in &config.afpay_rpc {
        if !pinged.insert(rpc_cfg.endpoint.clone()) {
            continue;
        }
        // Find any network that maps to this rpc_name (for the RemoteProvider constructor)
        let network = config
            .providers
            .iter()
            .find(|(_, name)| *name == rpc_name)
            .and_then(|(k, _)| k.parse::<Network>().ok())
            .unwrap_or(Network::Cashu);
        let secret = rpc_cfg.endpoint_secret.as_deref().unwrap_or("");
        let provider = RemoteProvider::new(&rpc_cfg.endpoint, secret, network);
        if let Err(err) = provider.ping().await {
            errors.push(Output::Error {
                id: None,
                error_code: "provider_unreachable".to_string(),
                error: format!("afpay_rpc.{rpc_name} ({}): {err}", rpc_cfg.endpoint),
                hint: Some("check endpoint address and that the daemon is running".to_string()),
                retryable: true,
                trace: Trace::from_duration(0),
            });
        }
    }
    errors
}

/// Get a reference to the storage backend, or return NotImplemented.
fn require_store(app: &App) -> Result<&StorageBackend, PayError> {
    app.store
        .as_deref()
        .ok_or_else(|| PayError::NotImplemented("no storage backend available".to_string()))
}

/// Acquire the data-directory lock for a write operation.
/// Returns the lock guard (dropped after operation) or emits an error.
#[cfg(feature = "redb")]
async fn acquire_write_lock(app: &App) -> Result<lock::DataLock, PayError> {
    let data_dir = app.config.read().await.data_dir.clone();
    let lock = tokio::task::spawn_blocking(move || lock::acquire(&data_dir, None))
        .await
        .map_err(|e| PayError::InternalError(format!("lock task: {e}")))?
        .map_err(PayError::InternalError)?;
    Ok(lock)
}

fn needs_write_lock(input: &Input) -> bool {
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

/// Resolve wallet labels to wallet IDs in-place.
/// If a wallet field does not start with "w_", treat it as a label and look it up.
fn resolve_wallet_labels(input: &mut Input, store: &dyn PayStore) -> Result<(), PayError> {
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

pub async fn dispatch(app: &App, input: Input) {
    // Acquire per-operation file lock for redb write operations.
    // Postgres handles its own concurrency; no file lock needed.
    #[cfg(feature = "redb")]
    let _lock = if app.has_local_providers
        && needs_write_lock(&input)
        && matches!(app.store.as_deref(), Some(StorageBackend::Redb(..)) | None)
    {
        match acquire_write_lock(app).await {
            Ok(guard) => Some(guard),
            Err(e) => {
                let id = extract_id(&input);
                emit_error(&app.writer, id, &e, Instant::now()).await;
                return;
            }
        }
    } else {
        None
    };

    // Resolve wallet labels → wallet IDs before dispatch
    let mut input = input;
    if let Some(store) = &app.store {
        if let Err(e) = resolve_wallet_labels(&mut input, store.as_ref()) {
            let id = extract_id(&input);
            emit_error(&app.writer, id, &e, Instant::now()).await;
            return;
        }
    }

    match input {
        Input::WalletCreate {
            id,
            network,
            label,
            mint_url,
            rpc_endpoints,
            chain_id,
            mnemonic_secret,
            btc_esplora_url,
            btc_network,
            btc_address_type,
            btc_backend,
            btc_core_url,
            btc_core_auth_secret,
            btc_electrum_url,
        } => {
            let start = Instant::now();
            let mut log_args = serde_json::json!({
                "operation": "wallet_create",
                "network": network.to_string(),
                "label": label.as_deref().unwrap_or("default"),
            });
            if let Some(object) = log_args.as_object_mut() {
                if !rpc_endpoints.is_empty() {
                    object.insert(
                        "rpc_endpoints".to_string(),
                        serde_json::json!(rpc_endpoints),
                    );
                }
                if let Some(cid) = chain_id {
                    object.insert("chain_id".to_string(), serde_json::json!(cid));
                }
                if let Some(url) = mint_url.as_deref() {
                    object.insert("mint_url".to_string(), serde_json::json!(url));
                }
                object.insert(
                    "use_recovery_mnemonic".to_string(),
                    serde_json::json!(mnemonic_secret.is_some()),
                );
            }
            emit_log(app, "wallet", Some(id.clone()), log_args).await;
            let request = WalletCreateRequest {
                label: label.unwrap_or_else(|| "default".to_string()),
                mint_url,
                rpc_endpoints,
                chain_id,
                mnemonic_secret,
                btc_esplora_url,
                btc_network,
                btc_address_type,
                btc_backend,
                btc_core_url,
                btc_core_auth_secret,
                btc_electrum_url,
            };
            match get_provider(&app.providers, network) {
                Some(p) => match p.create_wallet(&request).await {
                    Ok(info) => {
                        // Sync wallet metadata to store (for non-redb backends).
                        #[cfg(feature = "redb")]
                        if let Some(store) = app.store.as_ref() {
                            if let Ok(meta) = wallet::load_wallet_metadata(
                                &app.config.read().await.data_dir,
                                &info.id,
                            ) {
                                let _ = store.save_wallet_metadata(&meta);
                            }
                        }
                        let _ = app
                            .writer
                            .send(Output::WalletCreated {
                                id,
                                wallet: info.id,
                                network: info.network,
                                address: info.address,
                                mnemonic: None,
                                trace: trace_from(start),
                            })
                            .await;
                    }
                    Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                },
                None => {
                    emit_error(
                        &app.writer,
                        Some(id),
                        &PayError::NotImplemented(format!("no provider for {network}")),
                        start,
                    )
                    .await;
                }
            }
        }

        Input::LnWalletCreate { id, request } => {
            let start = Instant::now();
            let mut log_args =
                serde_json::to_value(&request).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(object) = log_args.as_object_mut() {
                object.insert(
                    "operation".to_string(),
                    serde_json::json!("ln_wallet_create"),
                );
                object.insert("network".to_string(), serde_json::json!("ln"));
            }
            emit_log(app, "wallet", Some(id.clone()), log_args).await;

            match get_provider(&app.providers, Network::Ln) {
                Some(p) => match p.create_ln_wallet(request).await {
                    Ok(info) => {
                        #[cfg(feature = "redb")]
                        if let Some(store) = app.store.as_ref() {
                            if let Ok(meta) = wallet::load_wallet_metadata(
                                &app.config.read().await.data_dir,
                                &info.id,
                            ) {
                                let _ = store.save_wallet_metadata(&meta);
                            }
                        }
                        let _ = app
                            .writer
                            .send(Output::WalletCreated {
                                id,
                                wallet: info.id,
                                network: info.network,
                                address: info.address,
                                mnemonic: None,
                                trace: trace_from(start),
                            })
                            .await;
                    }
                    Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                },
                None => {
                    emit_error(
                        &app.writer,
                        Some(id),
                        &PayError::NotImplemented("no provider for ln".to_string()),
                        start,
                    )
                    .await;
                }
            }
        }

        Input::WalletClose {
            id,
            wallet,
            dangerously_skip_balance_check_and_may_lose_money,
        } => {
            let start = Instant::now();
            emit_log(
                app,
                "wallet",
                Some(id.clone()),
                serde_json::json!({
                    "operation": "wallet_close",
                    "wallet": &wallet,
                    "dangerously_skip_balance_check_and_may_lose_money": dangerously_skip_balance_check_and_may_lose_money,
                }),
            )
            .await;
            let close_result = if dangerously_skip_balance_check_and_may_lose_money {
                match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet)) {
                    Ok(_) => require_store(app).and_then(|s| s.delete_wallet_metadata(&wallet)).map(|_| ()),
                    Err(PayError::WalletNotFound(_)) => Err(PayError::WalletNotFound(format!(
                        "wallet {wallet} not found locally; dangerous skip balance check only supports local wallets"
                    ))),
                    Err(error) => Err(error),
                }
            } else {
                match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet)) {
                    Ok(meta) => match get_provider(&app.providers, meta.network) {
                        Some(provider) => provider.close_wallet(&wallet).await,
                        None => Err(PayError::NotImplemented(format!(
                            "no provider for {}",
                            meta.network
                        ))),
                    },
                    Err(PayError::WalletNotFound(_)) => {
                        // Fallback for remote-only deployments where wallets may not be stored locally.
                        try_provider!(&app.providers, |p| p.close_wallet(&wallet))
                    }
                    Err(error) => Err(error),
                }
            };

            match close_result {
                Ok(()) => {
                    let _ = app
                        .writer
                        .send(Output::WalletClosed {
                            id,
                            wallet,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::WalletList { id, network } => {
            let start = Instant::now();
            emit_log(
                app,
                "wallet",
                Some(id.clone()),
                serde_json::json!({
                    "operation": "wallet_list",
                    "network": network.map(|c| c.to_string()).unwrap_or_else(|| "all".to_string()),
                }),
            )
            .await;
            if let Some(network) = network {
                match get_provider(&app.providers, network) {
                    Some(p) => match p.list_wallets().await {
                        Ok(wallets) => {
                            let _ = app
                                .writer
                                .send(Output::WalletList {
                                    id,
                                    wallets,
                                    trace: trace_from(start),
                                })
                                .await;
                        }
                        Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                    },
                    None => {
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::NotImplemented(format!("no provider for {network}")),
                            start,
                        )
                        .await;
                    }
                }
            } else {
                let wallets = collect_all!(&app.providers, |p| p.list_wallets());
                match wallets {
                    Ok(all) => {
                        let _ = app
                            .writer
                            .send(Output::WalletList {
                                id,
                                wallets: all,
                                trace: trace_from(start),
                            })
                            .await;
                    }
                    Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                }
            }
        }

        Input::Balance {
            id,
            wallet,
            network,
            check,
        } => {
            let start = Instant::now();
            emit_log(
                app,
                "wallet",
                Some(id.clone()),
                serde_json::json!({
                    "operation": "balance", "wallet": wallet.as_deref().unwrap_or("all"), "check": check,
                }),
            )
            .await;
            if let Some(wallet_id) = wallet {
                let meta_opt = require_store(app)
                    .and_then(|s| s.load_wallet_metadata(&wallet_id))
                    .ok();
                let result = if let Some(ref meta) = meta_opt {
                    match get_provider(&app.providers, meta.network) {
                        Some(provider) => {
                            if check {
                                match provider.check_balance(&wallet_id).await {
                                    Err(PayError::NotImplemented(_)) => {
                                        provider.balance(&wallet_id).await
                                    }
                                    other => other,
                                }
                            } else {
                                provider.balance(&wallet_id).await
                            }
                        }
                        None => Err(PayError::NotImplemented(format!(
                            "no provider for {}",
                            meta.network
                        ))),
                    }
                } else {
                    // Remote-only fallback: wallet metadata may not exist locally.
                    if check {
                        try_provider!(&app.providers, |p| async {
                            match p.check_balance(&wallet_id).await {
                                Err(PayError::NotImplemented(_)) => p.balance(&wallet_id).await,
                                other => other,
                            }
                        })
                    } else {
                        try_provider!(&app.providers, |p| p.balance(&wallet_id))
                    }
                };
                match result {
                    Ok(balance) => {
                        let summary = if let Some(meta) = meta_opt {
                            wallet_summary_from_meta(&meta, &wallet_id)
                        } else {
                            resolve_wallet_summary(app, &wallet_id).await
                        };
                        let _ = app
                            .writer
                            .send(Output::WalletBalances {
                                id,
                                wallets: vec![WalletBalanceItem {
                                    wallet: summary,
                                    balance: Some(balance),
                                    error: None,
                                }],
                                trace: trace_from(start),
                            })
                            .await;
                    }
                    Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                }
            } else {
                match collect_all!(&app.providers, |p| p.balance_all()) {
                    Ok(wallets) => {
                        let filtered = if let Some(network) = network {
                            wallets
                                .into_iter()
                                .filter(|w| w.wallet.network == network)
                                .collect()
                        } else {
                            wallets
                        };
                        let _ = app
                            .writer
                            .send(Output::WalletBalances {
                                id,
                                wallets: filtered,
                                trace: trace_from(start),
                            })
                            .await;
                    }
                    Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                }
            }
        }

        Input::Receive {
            id,
            wallet,
            network,
            amount,
            onchain_memo,
            wait_until_paid,
            wait_timeout_s,
            wait_poll_interval_ms,
            write_qr_svg_file: _,
            min_confirmations,
        } => {
            let start = Instant::now();
            let wait_requested =
                wait_until_paid || wait_timeout_s.is_some() || wait_poll_interval_ms.is_some();
            emit_log(
                app,
                "wallet",
                Some(id.clone()),
                serde_json::json!({
                    "operation": "receive",
                    "wallet": &wallet,
                    "network": network.map(|c| c.to_string()).unwrap_or_else(|| "auto".to_string()),
                    "amount": amount.as_ref().map(|a| a.value),
                    "onchain_memo": onchain_memo.as_deref().unwrap_or(""),
                    "wait_until_paid": wait_requested,
                    "wait_timeout_s": wait_timeout_s,
                    "wait_poll_interval_ms": wait_poll_interval_ms,
                }),
            )
            .await;

            let (target_network, wallet_for_call) = if wallet.trim().is_empty() {
                let wallets = match require_store(app).and_then(|s| s.list_wallet_metadata(network))
                {
                    Ok(v) => v,
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                };
                match wallets.len() {
                    0 => {
                        let msg = match network {
                            Some(network) => format!("no {network} wallet found"),
                            None => "no wallet found".to_string(),
                        };
                        emit_error(&app.writer, Some(id), &PayError::WalletNotFound(msg), start)
                            .await;
                        return;
                    }
                    1 => (wallets[0].network, wallets[0].id.clone()),
                    _ => {
                        let msg = match network {
                            Some(network) => {
                                format!("multiple {network} wallets found; pass --wallet")
                            }
                            None => "multiple wallets found; pass --wallet".to_string(),
                        };
                        emit_error(&app.writer, Some(id), &PayError::InvalidAmount(msg), start)
                            .await;
                        return;
                    }
                }
            } else {
                let meta = match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet)) {
                    Ok(m) => m,
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                };
                if let Some(expected) = network {
                    if meta.network != expected {
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::InvalidAmount(format!(
                                "wallet {wallet} is {}, not {expected}",
                                meta.network
                            )),
                            start,
                        )
                        .await;
                        return;
                    }
                }
                (meta.network, wallet.clone())
            };

            let Some(provider) = get_provider(&app.providers, target_network) else {
                emit_error(
                    &app.writer,
                    Some(id),
                    &PayError::NotImplemented(format!("no provider for {target_network}")),
                    start,
                )
                .await;
                return;
            };

            match provider
                .receive_info(&wallet_for_call, amount.clone())
                .await
            {
                Ok(receive_info) => {
                    let quote_id = receive_info.quote_id.clone();
                    let _ = app
                        .writer
                        .send(Output::ReceiveInfo {
                            id: id.clone(),
                            wallet: wallet_for_call.clone(),
                            receive_info,
                            trace: trace_from(start),
                        })
                        .await;

                    if !wait_requested {
                        return;
                    }

                    let timeout_secs = wait_timeout_s.unwrap_or(DEFAULT_WAIT_TIMEOUT_SECS);
                    if timeout_secs == 0 {
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::InvalidAmount("wait_timeout_s must be >= 1".to_string()),
                            start,
                        )
                        .await;
                        return;
                    }
                    let poll_interval_ms =
                        wait_poll_interval_ms.unwrap_or(DEFAULT_WAIT_POLL_INTERVAL_MS);
                    if poll_interval_ms == 0 {
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::InvalidAmount(
                                "wait_poll_interval_ms must be >= 1".to_string(),
                            ),
                            start,
                        )
                        .await;
                        return;
                    }

                    if target_network == Network::Sol {
                        let memo_to_watch = onchain_memo
                            .as_deref()
                            .map(str::trim)
                            .filter(|text| !text.is_empty())
                            .map(str::to_owned);
                        let amount_to_watch = amount.as_ref().map(|a| a.value);

                        if memo_to_watch.is_none() && amount_to_watch.is_none() {
                            emit_error_hint(
                                &app.writer,
                                Some(id),
                                &PayError::InvalidAmount(
                                    "sol receive --wait requires a match condition".to_string(),
                                ),
                                start,
                                Some("pass --onchain-memo or --amount"),
                            )
                            .await;
                            return;
                        }

                        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
                        loop {
                            match provider.history_list(&wallet_for_call, 200, 0).await {
                                Ok(items) => {
                                    let matched = items.into_iter().find(|item| {
                                        if item.direction != Direction::Receive {
                                            return false;
                                        }
                                        if let Some(ref m) = memo_to_watch {
                                            item.onchain_memo.as_deref() == Some(m.as_str())
                                        } else if let Some(expected) = amount_to_watch {
                                            item.amount.value == expected
                                        } else {
                                            false
                                        }
                                    });
                                    if let Some(item) = matched {
                                        // Check confirmation depth if requested
                                        if let Some(min_conf) = min_confirmations {
                                            match provider
                                                .history_status(&item.transaction_id)
                                                .await
                                            {
                                                Ok(status_info) => {
                                                    let confs = status_info
                                                        .confirmations
                                                        .unwrap_or_else(|| {
                                                            if status_info.status
                                                                == TxStatus::Confirmed
                                                            {
                                                                min_conf
                                                            } else {
                                                                0
                                                            }
                                                        });
                                                    if confs < min_conf {
                                                        // Not enough confirmations yet, keep polling
                                                        if Instant::now() >= deadline {
                                                            let criteria = if let Some(ref m) =
                                                                memo_to_watch
                                                            {
                                                                format!("memo '{m}'")
                                                            } else {
                                                                format!(
                                                                    "amount {}",
                                                                    amount_to_watch.unwrap_or(0)
                                                                )
                                                            };
                                                            emit_error(
                                                                &app.writer,
                                                                Some(id),
                                                                &PayError::NetworkError(format!(
                                                                    "wait timeout after {timeout_secs}s: sol transaction {tx} matching {criteria} has {confs}/{min_conf} confirmations",
                                                                    tx = item.transaction_id,
                                                                )),
                                                                start,
                                                            )
                                                            .await;
                                                            break;
                                                        }
                                                        sleep(Duration::from_millis(
                                                            poll_interval_ms,
                                                        ))
                                                        .await;
                                                        continue;
                                                    }
                                                    // Enough confirmations — emit with confirmation count
                                                    let transaction_id =
                                                        item.transaction_id.clone();
                                                    let _ = app
                                                        .writer
                                                        .send(Output::HistoryStatus {
                                                            id,
                                                            transaction_id,
                                                            status: item.status,
                                                            confirmations: Some(confs),
                                                            preimage: item.preimage.clone(),
                                                            item: Some(item),
                                                            trace: trace_from(start),
                                                        })
                                                        .await;
                                                    break;
                                                }
                                                Err(e) if e.retryable() => {
                                                    sleep(Duration::from_millis(poll_interval_ms))
                                                        .await;
                                                    continue;
                                                }
                                                Err(e) => {
                                                    emit_error(&app.writer, Some(id), &e, start)
                                                        .await;
                                                    break;
                                                }
                                            }
                                        } else {
                                            let transaction_id = item.transaction_id.clone();
                                            let _ = app
                                                .writer
                                                .send(Output::HistoryStatus {
                                                    id,
                                                    transaction_id,
                                                    status: item.status,
                                                    confirmations: None,
                                                    preimage: item.preimage.clone(),
                                                    item: Some(item),
                                                    trace: trace_from(start),
                                                })
                                                .await;
                                            break;
                                        }
                                    }
                                    if Instant::now() >= deadline {
                                        let criteria = if let Some(ref m) = memo_to_watch {
                                            format!("memo '{m}'")
                                        } else {
                                            format!("amount {}", amount_to_watch.unwrap_or(0))
                                        };
                                        emit_error(
                                            &app.writer,
                                            Some(id),
                                            &PayError::NetworkError(format!(
                                                "wait timeout after {timeout_secs}s: no incoming sol transaction matching {criteria}"
                                            )),
                                            start,
                                        )
                                        .await;
                                        break;
                                    }
                                    sleep(Duration::from_millis(poll_interval_ms)).await;
                                }
                                Err(e) if e.retryable() => {
                                    if Instant::now() >= deadline {
                                        let criteria = if let Some(ref m) = memo_to_watch {
                                            format!("memo '{m}'")
                                        } else {
                                            format!("amount {}", amount_to_watch.unwrap_or(0))
                                        };
                                        emit_error(
                                            &app.writer,
                                            Some(id),
                                            &PayError::NetworkError(format!(
                                                "wait timeout after {timeout_secs}s: no incoming sol transaction matching {criteria}"
                                            )),
                                            start,
                                        )
                                        .await;
                                        break;
                                    }
                                    sleep(Duration::from_millis(poll_interval_ms)).await;
                                }
                                Err(e) => {
                                    emit_error(&app.writer, Some(id), &e, start).await;
                                    break;
                                }
                            }
                        }
                        return;
                    }

                    // EVM: poll balance deltas for incoming deposits.
                    if target_network == Network::Evm {
                        if onchain_memo
                            .as_deref()
                            .map(str::trim)
                            .filter(|text| !text.is_empty())
                            .is_some()
                        {
                            emit_error_hint(
                                &app.writer,
                                Some(id),
                                &PayError::InvalidAmount(
                                    "evm receive --wait does not support --onchain-memo matching"
                                        .to_string(),
                                ),
                                start,
                                Some("pass --amount to match incoming transfers"),
                            )
                            .await;
                            return;
                        }
                        let amount_to_watch = amount.as_ref().map(|a| a.value);

                        if amount_to_watch.is_none() {
                            emit_error_hint(
                                &app.writer,
                                Some(id),
                                &PayError::InvalidAmount(
                                    "evm receive --wait requires --amount".to_string(),
                                ),
                                start,
                                Some("pass --amount"),
                            )
                            .await;
                            return;
                        }

                        // Snapshot current balance before polling
                        let initial_balance = match provider.balance(&wallet_for_call).await {
                            Ok(b) => b,
                            Err(e) => {
                                emit_error(&app.writer, Some(id), &e, start).await;
                                return;
                            }
                        };

                        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
                        loop {
                            sleep(Duration::from_millis(poll_interval_ms)).await;
                            if Instant::now() >= deadline {
                                let criteria = format!("amount {}", amount_to_watch.unwrap_or(0));
                                emit_error(
                                    &app.writer,
                                    Some(id),
                                    &PayError::NetworkError(format!(
                                        "wait timeout after {timeout_secs}s: no incoming evm deposit matching {criteria}"
                                    )),
                                    start,
                                )
                                .await;
                                break;
                            }
                            match provider.balance(&wallet_for_call).await {
                                Ok(current) => {
                                    // Check native balance increase
                                    let native_increase =
                                        current.confirmed.saturating_sub(initial_balance.confirmed);
                                    // Check token balance increases
                                    let mut token_increase: Option<(String, u64)> = None;
                                    for (key, &cur_val) in &current.additional {
                                        let init_val = initial_balance
                                            .additional
                                            .get(key)
                                            .copied()
                                            .unwrap_or(0);
                                        if cur_val > init_val {
                                            token_increase =
                                                Some((key.clone(), cur_val - init_val));
                                            break;
                                        }
                                    }

                                    if native_increase > 0 || token_increase.is_some() {
                                        let (amount_value, amount_unit) =
                                            if let Some((token_key, delta)) = token_increase {
                                                (delta, token_key)
                                            } else {
                                                (native_increase, current.unit.clone())
                                            };

                                        // Match expected amount exactly.
                                        if let Some(expected) = amount_to_watch {
                                            if amount_value != expected {
                                                continue;
                                            }
                                        }

                                        // If min_confirmations requested, find the on-chain tx and wait for depth
                                        if let Some(min_conf) = min_confirmations {
                                            // Find the most recent incoming transaction via history_list
                                            let chain_tx_id = match provider
                                                .history_list(&wallet_for_call, 20, 0)
                                                .await
                                            {
                                                Ok(items) => items
                                                    .into_iter()
                                                    .find(|i| {
                                                        i.direction == Direction::Receive
                                                            && i.amount.value == amount_value
                                                    })
                                                    .map(|i| i.transaction_id),
                                                Err(_) => None,
                                            };
                                            if let Some(ref ctxid) = chain_tx_id {
                                                // Poll history_status until enough confirmations or timeout
                                                loop {
                                                    match provider.history_status(ctxid).await {
                                                        Ok(status_info) => {
                                                            let confs = status_info
                                                                .confirmations
                                                                .unwrap_or(0);
                                                            if confs >= min_conf {
                                                                let record = HistoryRecord {
                                                                    transaction_id: ctxid.clone(),
                                                                    wallet: wallet_for_call.clone(),
                                                                    network: Network::Evm,
                                                                    direction: Direction::Receive,
                                                                    amount: Amount {
                                                                        value: amount_value,
                                                                        token: amount_unit.clone(),
                                                                    },
                                                                    status: TxStatus::Confirmed,
                                                                    onchain_memo: None,
                                                                    local_memo: None,
                                                                    remote_addr: None,
                                                                    preimage: None,
                                                                    created_at_epoch_s:
                                                                        wallet::now_epoch_seconds(),
                                                                    confirmed_at_epoch_s: Some(
                                                                        wallet::now_epoch_seconds(),
                                                                    ),
                                                                    fee: None,
                                                                };
                                                                if let Some(s) = &app.store {
                                                                    let _ = s
                                                                        .append_transaction_record(
                                                                            &record,
                                                                        );
                                                                }
                                                                let _ = app
                                                                    .writer
                                                                    .send(Output::HistoryStatus {
                                                                        id,
                                                                        transaction_id: ctxid
                                                                            .clone(),
                                                                        status: TxStatus::Confirmed,
                                                                        confirmations: Some(confs),
                                                                        preimage: None,
                                                                        item: Some(record),
                                                                        trace: trace_from(start),
                                                                    })
                                                                    .await;
                                                                break;
                                                            }
                                                            if Instant::now() >= deadline {
                                                                let criteria = format!(
                                                                    "amount {}",
                                                                    amount_to_watch.unwrap_or(0)
                                                                );
                                                                emit_error(
                                                                    &app.writer,
                                                                    Some(id),
                                                                    &PayError::NetworkError(format!(
                                                                        "wait timeout after {timeout_secs}s: evm transaction {ctxid} matching {criteria} has {confs}/{min_conf} confirmations",
                                                                    )),
                                                                    start,
                                                                )
                                                                .await;
                                                                break;
                                                            }
                                                            sleep(Duration::from_millis(
                                                                poll_interval_ms,
                                                            ))
                                                            .await;
                                                        }
                                                        Err(e) if e.retryable() => {
                                                            if Instant::now() >= deadline {
                                                                emit_error(
                                                                    &app.writer,
                                                                    Some(id),
                                                                    &e,
                                                                    start,
                                                                )
                                                                .await;
                                                                break;
                                                            }
                                                            sleep(Duration::from_millis(
                                                                poll_interval_ms,
                                                            ))
                                                            .await;
                                                        }
                                                        Err(e) => {
                                                            emit_error(
                                                                &app.writer,
                                                                Some(id),
                                                                &e,
                                                                start,
                                                            )
                                                            .await;
                                                            break;
                                                        }
                                                    }
                                                }
                                            } else {
                                                // Could not find on-chain tx; fall back to recording without confirmations
                                                let tx_id = format!(
                                                    "evm_recv_{}",
                                                    wallet::now_epoch_seconds()
                                                );
                                                let record = HistoryRecord {
                                                    transaction_id: tx_id.clone(),
                                                    wallet: wallet_for_call.clone(),
                                                    network: Network::Evm,
                                                    direction: Direction::Receive,
                                                    amount: Amount {
                                                        value: amount_value,
                                                        token: amount_unit,
                                                    },
                                                    status: TxStatus::Confirmed,
                                                    onchain_memo: None,
                                                    local_memo: None,
                                                    remote_addr: None,
                                                    preimage: None,
                                                    created_at_epoch_s: wallet::now_epoch_seconds(),
                                                    confirmed_at_epoch_s: Some(
                                                        wallet::now_epoch_seconds(),
                                                    ),
                                                    fee: None,
                                                };
                                                if let Some(s) = &app.store {
                                                    let _ = s.append_transaction_record(&record);
                                                }
                                                let _ = app
                                                    .writer
                                                    .send(Output::HistoryStatus {
                                                        id,
                                                        transaction_id: tx_id,
                                                        status: TxStatus::Confirmed,
                                                        confirmations: None,
                                                        preimage: None,
                                                        item: Some(record),
                                                        trace: trace_from(start),
                                                    })
                                                    .await;
                                            }
                                        } else {
                                            let tx_id =
                                                format!("evm_recv_{}", wallet::now_epoch_seconds());
                                            let record = HistoryRecord {
                                                transaction_id: tx_id.clone(),
                                                wallet: wallet_for_call.clone(),
                                                network: Network::Evm,
                                                direction: Direction::Receive,
                                                amount: Amount {
                                                    value: amount_value,
                                                    token: amount_unit,
                                                },
                                                status: TxStatus::Confirmed,
                                                onchain_memo: None,
                                                local_memo: None,
                                                remote_addr: None,
                                                preimage: None,
                                                created_at_epoch_s: wallet::now_epoch_seconds(),
                                                confirmed_at_epoch_s: Some(
                                                    wallet::now_epoch_seconds(),
                                                ),
                                                fee: None,
                                            };
                                            if let Some(s) = &app.store {
                                                let _ = s.append_transaction_record(&record);
                                            }
                                            let _ = app
                                                .writer
                                                .send(Output::HistoryStatus {
                                                    id,
                                                    transaction_id: tx_id,
                                                    status: TxStatus::Confirmed,
                                                    confirmations: None,
                                                    preimage: None,
                                                    item: Some(record),
                                                    trace: trace_from(start),
                                                })
                                                .await;
                                        }
                                        break;
                                    }
                                }
                                Err(e) if e.retryable() => {
                                    // transient error, keep polling
                                }
                                Err(e) => {
                                    emit_error(&app.writer, Some(id), &e, start).await;
                                    break;
                                }
                            }
                        }
                        return;
                    }

                    // BTC: poll wallet balance deltas for incoming deposits.
                    if target_network == Network::Btc {
                        let amount_to_watch = amount.as_ref().map(|a| a.value).filter(|v| *v > 0);
                        let initial_balance = match provider.balance(&wallet_for_call).await {
                            Ok(b) => b,
                            Err(e) => {
                                emit_error(&app.writer, Some(id), &e, start).await;
                                return;
                            }
                        };

                        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
                        loop {
                            sleep(Duration::from_millis(poll_interval_ms)).await;
                            if Instant::now() >= deadline {
                                let criteria = if let Some(expected) = amount_to_watch {
                                    format!("amount {expected}")
                                } else {
                                    "any incoming amount".to_string()
                                };
                                emit_error(
                                    &app.writer,
                                    Some(id),
                                    &PayError::NetworkError(format!(
                                        "wait timeout after {timeout_secs}s: no incoming btc transaction matching {criteria}"
                                    )),
                                    start,
                                )
                                .await;
                                break;
                            }

                            match provider.balance(&wallet_for_call).await {
                                Ok(current) => {
                                    let confirmed_delta =
                                        current.confirmed.saturating_sub(initial_balance.confirmed);
                                    let pending_delta =
                                        current.pending.saturating_sub(initial_balance.pending);
                                    let observed_delta =
                                        confirmed_delta.saturating_add(pending_delta);
                                    if observed_delta == 0 {
                                        continue;
                                    }
                                    if let Some(expected) = amount_to_watch {
                                        if observed_delta != expected {
                                            continue;
                                        }
                                    }

                                    let status = if confirmed_delta > 0 {
                                        TxStatus::Confirmed
                                    } else {
                                        TxStatus::Pending
                                    };
                                    let now = wallet::now_epoch_seconds();
                                    let tx_id = format!("btc_recv_{now}");
                                    let record = HistoryRecord {
                                        transaction_id: tx_id.clone(),
                                        wallet: wallet_for_call.clone(),
                                        network: Network::Btc,
                                        direction: Direction::Receive,
                                        amount: Amount {
                                            value: amount_to_watch.unwrap_or(observed_delta),
                                            token: current.unit.clone(),
                                        },
                                        status,
                                        onchain_memo: onchain_memo.clone(),
                                        local_memo: None,
                                        remote_addr: None,
                                        preimage: None,
                                        created_at_epoch_s: now,
                                        confirmed_at_epoch_s: if status == TxStatus::Confirmed {
                                            Some(now)
                                        } else {
                                            None
                                        },
                                        fee: None,
                                    };
                                    if let Some(s) = &app.store {
                                        if s.find_transaction_record_by_id(&tx_id)
                                            .ok()
                                            .flatten()
                                            .is_none()
                                        {
                                            let _ = s.append_transaction_record(&record);
                                        }
                                    }
                                    let _ = app
                                        .writer
                                        .send(Output::HistoryStatus {
                                            id,
                                            transaction_id: tx_id,
                                            status,
                                            confirmations: Some(if status == TxStatus::Confirmed {
                                                1
                                            } else {
                                                0
                                            }),
                                            preimage: None,
                                            item: Some(record),
                                            trace: trace_from(start),
                                        })
                                        .await;
                                    break;
                                }
                                Err(e) if e.retryable() => {
                                    // Transient network error, keep polling until timeout.
                                }
                                Err(e) => {
                                    emit_error(&app.writer, Some(id), &e, start).await;
                                    break;
                                }
                            }
                        }
                        return;
                    }

                    let Some(quote_id) = quote_id else {
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::InternalError(
                                "deposit response missing quote_id/payment_hash".to_string(),
                            ),
                            start,
                        )
                        .await;
                        return;
                    };

                    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
                    loop {
                        match provider.receive_claim(&wallet_for_call, &quote_id).await {
                            Ok(claimed) => {
                                let _ = app
                                    .writer
                                    .send(Output::ReceiveClaimed {
                                        id,
                                        wallet: wallet_for_call.clone(),
                                        amount: Amount {
                                            value: claimed,
                                            token: "sats".to_string(),
                                        },
                                        trace: trace_from(start),
                                    })
                                    .await;
                                break;
                            }
                            Err(e) if e.retryable() => {
                                if Instant::now() >= deadline {
                                    emit_error(
                                        &app.writer,
                                        Some(id),
                                        &PayError::NetworkError(format!(
                                            "wait-until-paid timeout after {timeout_secs}s"
                                        )),
                                        start,
                                    )
                                    .await;
                                    break;
                                }
                                sleep(Duration::from_millis(poll_interval_ms)).await;
                            }
                            Err(e) => {
                                emit_error(&app.writer, Some(id), &e, start).await;
                                break;
                            }
                        }
                    }
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::ReceiveClaim {
            id,
            wallet,
            quote_id,
        } => {
            let start = Instant::now();
            emit_log(
                app,
                "wallet",
                Some(id.clone()),
                serde_json::json!({
                    "operation": "receive_claim", "wallet": &wallet, "quote_id": &quote_id,
                }),
            )
            .await;
            let meta = match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet)) {
                Ok(m) => m,
                Err(e) => {
                    emit_error(&app.writer, Some(id), &e, start).await;
                    return;
                }
            };
            let Some(provider) = get_provider(&app.providers, meta.network) else {
                emit_error(
                    &app.writer,
                    Some(id),
                    &PayError::NotImplemented(format!("no provider for {}", meta.network)),
                    start,
                )
                .await;
                return;
            };

            match provider.receive_claim(&wallet, &quote_id).await {
                Ok(claimed) => {
                    let _ = app
                        .writer
                        .send(Output::ReceiveClaimed {
                            id,
                            wallet,
                            amount: Amount {
                                value: claimed,
                                token: "sats".to_string(),
                            },
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::CashuSend {
            id,
            wallet,
            amount,
            onchain_memo,
            local_memo,
            mints,
        } => {
            let start = Instant::now();
            emit_log(
                app,
                "pay",
                Some(id.clone()),
                serde_json::json!({
                    "operation": "cashu_send", "wallet": wallet.as_deref().unwrap_or("auto"),
                    "amount": amount.value, "onchain_memo": onchain_memo.as_deref().unwrap_or(""),
                    "mints": mints.as_deref().unwrap_or(&[]),
                }),
            )
            .await;

            let wallet_str = wallet.unwrap_or_default();
            let mints_ref = mints.as_deref();
            let Some(provider) = get_provider(&app.providers, Network::Cashu) else {
                emit_error(
                    &app.writer,
                    Some(id),
                    &PayError::NotImplemented("no provider for cashu".to_string()),
                    start,
                )
                .await;
                return;
            };

            let mut reservation_id: Option<u64> = None;
            if app.enforce_limits {
                let spend_ctx = SpendContext {
                    network: "cashu".to_string(),
                    wallet: if wallet_str.is_empty() {
                        None
                    } else {
                        Some(wallet_str.clone())
                    },
                    amount_native: amount.value,
                    token: None,
                };
                match app
                    .spend_ledger
                    .reserve(&format!("cashu_send:{id}"), &spend_ctx)
                    .await
                {
                    Ok(rid) => reservation_id = Some(rid),
                    Err(e) => {
                        if let PayError::LimitExceeded {
                            rule_id,
                            scope,
                            scope_key,
                            spent,
                            max_spend,
                            token,
                            remaining_s,
                            origin,
                        } = &e
                        {
                            let _ = app
                                .writer
                                .send(Output::LimitExceeded {
                                    id,
                                    rule_id: rule_id.clone(),
                                    scope: *scope,
                                    scope_key: scope_key.clone(),
                                    spent: *spent,
                                    max_spend: *max_spend,
                                    token: token.clone(),
                                    remaining_s: *remaining_s,
                                    origin: origin.clone(),
                                    trace: trace_from(start),
                                })
                                .await;
                        } else {
                            emit_error(&app.writer, Some(id), &e, start).await;
                        }
                        return;
                    }
                }
            }

            match provider
                .cashu_send(
                    &wallet_str,
                    amount.clone(),
                    onchain_memo.as_deref(),
                    mints_ref,
                )
                .await
            {
                Ok(r) => {
                    if let Some(rid) = reservation_id {
                        let _ = app.spend_ledger.confirm(rid).await;
                    }
                    if local_memo.is_some() {
                        if let Some(s) = &app.store {
                            let _ = s.update_transaction_record_memo(
                                &r.transaction_id,
                                local_memo.as_ref(),
                            );
                        }
                    }
                    let _ = app
                        .writer
                        .send(Output::CashuSent {
                            id,
                            wallet: r.wallet,
                            transaction_id: r.transaction_id,
                            status: r.status,
                            fee: r.fee,
                            token: r.token,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => {
                    if let Some(rid) = reservation_id {
                        let _ = app.spend_ledger.cancel(rid).await;
                    }
                    emit_error(&app.writer, Some(id), &e, start).await
                }
            }
        }

        Input::CashuReceive { id, wallet, token } => {
            let start = Instant::now();
            let token_preview = if token.len() > 20 {
                format!("{}...", &token[..20])
            } else {
                token.clone()
            };
            emit_log(
                app,
                "pay",
                Some(id.clone()),
                serde_json::json!({
                    "operation": "cashu_receive", "wallet": wallet.as_deref().unwrap_or("auto"), "token": token_preview,
                }),
            )
            .await;
            let wallet_str = wallet.unwrap_or_default();
            let Some(provider) = get_provider(&app.providers, Network::Cashu) else {
                emit_error(
                    &app.writer,
                    Some(id),
                    &PayError::NotImplemented("no provider for cashu".to_string()),
                    start,
                )
                .await;
                return;
            };
            match provider.cashu_receive(&wallet_str, &token).await {
                Ok(r) => {
                    let _ = app
                        .writer
                        .send(Output::CashuReceived {
                            id,
                            wallet: r.wallet,
                            amount: r.amount,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::Send {
            id,
            wallet,
            network,
            to,
            onchain_memo,
            local_memo,
            mints,
        } => {
            let start = Instant::now();
            let operation_name = "send";
            let to_preview = if to.len() > 20 {
                format!("{}...", &to[..20])
            } else {
                to.clone()
            };
            emit_log(
                app,
                "pay",
                Some(id.clone()),
                serde_json::json!({
                    "operation": operation_name, "wallet": wallet.as_deref().unwrap_or("auto"),
                    "network": network.map(|c| c.to_string()).unwrap_or_else(|| "auto".to_string()),
                    "to": to_preview, "onchain_memo": onchain_memo.as_deref().unwrap_or(""),
                }),
            )
            .await;

            let (target_network, wallet_for_call) = if let Some(w) =
                wallet.filter(|w| !w.is_empty())
            {
                let meta = match require_store(app).and_then(|s| s.load_wallet_metadata(&w)) {
                    Ok(m) => m,
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                };
                if let Some(expected) = network {
                    if meta.network != expected {
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::InvalidAmount(format!(
                                "wallet {w} is {}, not {expected}",
                                meta.network
                            )),
                            start,
                        )
                        .await;
                        return;
                    }
                }
                (meta.network, w)
            } else {
                let wallets = match require_store(app).and_then(|s| s.list_wallet_metadata(network))
                {
                    Ok(v) => v,
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                };

                // For cashu with --cashu-mint or multiple wallets: filter by mint,
                // then select wallet with smallest sufficient balance.
                let is_cashu = matches!(network, Some(Network::Cashu));
                let filtered: Vec<_> = if is_cashu {
                    if let Some(ref mint_list) = mints {
                        wallets
                            .into_iter()
                            .filter(|w| {
                                w.mint_url.as_deref().is_some_and(|u| {
                                    let nu = u.trim().trim_end_matches('/');
                                    mint_list
                                        .iter()
                                        .any(|m| m.trim().trim_end_matches('/') == nu)
                                })
                            })
                            .collect()
                    } else {
                        wallets
                    }
                } else {
                    wallets
                };

                match filtered.len() {
                    0 => {
                        let msg = if mints.is_some() {
                            "no cashu wallet found matching --cashu-mint".to_string()
                        } else {
                            match network {
                                Some(network) => format!("no {network} wallet found"),
                                None => "no wallet found".to_string(),
                            }
                        };
                        emit_error(&app.writer, Some(id), &PayError::WalletNotFound(msg), start)
                            .await;
                        return;
                    }
                    1 => (filtered[0].network, filtered[0].id.clone()),
                    _ if is_cashu => {
                        // Multiple cashu wallets: select by smallest sufficient balance.
                        // Pass empty wallet to provider — it will use select_wallet_by_balance.
                        (Network::Cashu, String::new())
                    }
                    _ => {
                        let msg = match network {
                            Some(network) => {
                                format!("multiple {network} wallets found; pass --wallet")
                            }
                            None => "multiple wallets found; pass --wallet".to_string(),
                        };
                        emit_error(&app.writer, Some(id), &PayError::InvalidAmount(msg), start)
                            .await;
                        return;
                    }
                }
            };

            let Some(provider) = get_provider(&app.providers, target_network) else {
                emit_error(
                    &app.writer,
                    Some(id),
                    &PayError::NotImplemented(format!("no provider for {target_network}")),
                    start,
                )
                .await;
                return;
            };

            let mut reservation_id: Option<u64> = None;
            if app.enforce_limits {
                let quote = match provider
                    .send_quote(&wallet_for_call, &to, mints.as_deref())
                    .await
                {
                    Ok(q) => q,
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                };
                let spend_amount = quote.amount_native + quote.fee_estimate_native;
                let provider_key = require_store(app)
                    .and_then(|s| s.load_wallet_metadata(&quote.wallet))
                    .ok()
                    .map(|meta| wallet_provider_key(&meta))
                    .unwrap_or_else(|| target_network.to_string());
                let spend_ctx = SpendContext {
                    network: provider_key,
                    wallet: Some(quote.wallet.clone()),
                    amount_native: spend_amount,
                    token: extract_token_from_target(&to),
                };
                match app
                    .spend_ledger
                    .reserve(&format!("send:{id}"), &spend_ctx)
                    .await
                {
                    Ok(rid) => reservation_id = Some(rid),
                    Err(e) => {
                        if let PayError::LimitExceeded {
                            rule_id,
                            scope,
                            scope_key,
                            spent,
                            max_spend,
                            token,
                            remaining_s,
                            origin,
                        } = &e
                        {
                            let _ = app
                                .writer
                                .send(Output::LimitExceeded {
                                    id,
                                    rule_id: rule_id.clone(),
                                    scope: *scope,
                                    scope_key: scope_key.clone(),
                                    spent: *spent,
                                    max_spend: *max_spend,
                                    token: token.clone(),
                                    remaining_s: *remaining_s,
                                    origin: origin.clone(),
                                    trace: trace_from(start),
                                })
                                .await;
                        } else {
                            emit_error(&app.writer, Some(id), &e, start).await;
                        }
                        return;
                    }
                }
            }
            match provider
                .send(
                    &wallet_for_call,
                    &to,
                    onchain_memo.as_deref(),
                    mints.as_deref(),
                )
                .await
            {
                Ok(r) => {
                    if let Some(rid) = reservation_id {
                        let _ = app.spend_ledger.confirm(rid).await;
                    }
                    if local_memo.is_some() {
                        if let Some(s) = &app.store {
                            let _ = s.update_transaction_record_memo(
                                &r.transaction_id,
                                local_memo.as_ref(),
                            );
                        }
                    }
                    let _ = app
                        .writer
                        .send(Output::Sent {
                            id,
                            wallet: r.wallet,
                            transaction_id: r.transaction_id,
                            amount: r.amount,
                            fee: r.fee,
                            preimage: r.preimage,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => {
                    if let Some(rid) = reservation_id {
                        let _ = app.spend_ledger.cancel(rid).await;
                    }
                    emit_error(&app.writer, Some(id), &e, start).await
                }
            }
        }

        Input::Restore { id, wallet } => {
            let start = Instant::now();
            emit_log(
                app,
                "wallet",
                Some(id.clone()),
                serde_json::json!({
                    "operation": "restore", "wallet": &wallet,
                }),
            )
            .await;
            match try_provider!(&app.providers, |p| p.restore(&wallet)) {
                Ok(r) => {
                    let _ = app
                        .writer
                        .send(Output::Restored {
                            id,
                            wallet: r.wallet,
                            unspent: r.unspent,
                            spent: r.spent,
                            pending: r.pending,
                            unit: r.unit,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::WalletShowSeed { id, wallet } => {
            let start = Instant::now();
            emit_log(
                app,
                "wallet",
                Some(id.clone()),
                serde_json::json!({
                    "operation": "wallet_show_seed", "wallet": &wallet,
                }),
            )
            .await;
            match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet)) {
                Ok(meta) => match meta.network {
                    Network::Cashu => match meta.seed_secret {
                        Some(mnemonic) => {
                            let _ = app
                                .writer
                                .send(Output::WalletSeed {
                                    id,
                                    wallet,
                                    mnemonic_secret: mnemonic,
                                    trace: trace_from(start),
                                })
                                .await;
                        }
                        None => {
                            emit_error(
                                &app.writer,
                                Some(id),
                                &PayError::InternalError("wallet has no seed".to_string()),
                                start,
                            )
                            .await;
                        }
                    },
                    Network::Sol => match meta.seed_secret {
                        Some(secret) => {
                            if looks_like_bip39_mnemonic(&secret) {
                                let _ = app
                                    .writer
                                    .send(Output::WalletSeed {
                                        id,
                                        wallet,
                                        mnemonic_secret: secret,
                                        trace: trace_from(start),
                                    })
                                    .await;
                            } else {
                                emit_error(
                                        &app.writer,
                                        Some(id),
                                        &PayError::InvalidAmount(
                                            "this sol wallet was created before mnemonic support; create a new sol wallet to get 12-word backup".to_string(),
                                        ),
                                        start,
                                    )
                                    .await;
                            }
                        }
                        None => {
                            emit_error(
                                &app.writer,
                                Some(id),
                                &PayError::InternalError("wallet has no seed".to_string()),
                                start,
                            )
                            .await;
                        }
                    },
                    Network::Ln => {
                        emit_error(
                                &app.writer,
                                Some(id),
                                &PayError::InvalidAmount(
                                    "ln wallets do not have mnemonic words; they store backend credentials (nwc-uri/password/admin-key)".to_string(),
                                ),
                                start,
                            )
                            .await;
                    }
                    Network::Evm | Network::Btc => match meta.seed_secret {
                        Some(secret) => {
                            if looks_like_bip39_mnemonic(&secret) {
                                let _ = app
                                    .writer
                                    .send(Output::WalletSeed {
                                        id,
                                        wallet,
                                        mnemonic_secret: secret,
                                        trace: trace_from(start),
                                    })
                                    .await;
                            } else {
                                emit_error(
                                        &app.writer,
                                        Some(id),
                                        &PayError::InvalidAmount(
                                            "this wallet was created before mnemonic support; create a new wallet to get 12-word backup".to_string(),
                                        ),
                                        start,
                                    )
                                    .await;
                            }
                        }
                        None => {
                            emit_error(
                                &app.writer,
                                Some(id),
                                &PayError::InternalError("wallet has no seed".to_string()),
                                start,
                            )
                            .await;
                        }
                    },
                },
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::HistoryList {
            id,
            wallet,
            network,
            onchain_memo,
            limit,
            offset,
            since_epoch_s,
            until_epoch_s,
        } => {
            let start = Instant::now();
            let lim = limit.unwrap_or(20);
            let off = offset.unwrap_or(0);
            let memo_filter = onchain_memo
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            let store = match require_store(app) {
                Ok(store) => store,
                Err(e) => {
                    emit_error(&app.writer, Some(id), &e, start).await;
                    return;
                }
            };

            let mut all_txs = Vec::new();
            if let Some(wallet_id) = wallet {
                let meta = match store.load_wallet_metadata(&wallet_id) {
                    Ok(meta) => meta,
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                };
                if let Some(expected_network) = network {
                    if meta.network != expected_network {
                        let _ = app
                            .writer
                            .send(Output::History {
                                id,
                                items: Vec::new(),
                                trace: trace_from(start),
                            })
                            .await;
                        return;
                    }
                }
                match store.load_wallet_transaction_records(&wallet_id) {
                    Ok(mut records) => all_txs.append(&mut records),
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                }
            } else {
                let wallets = match store.list_wallet_metadata(network) {
                    Ok(wallets) => wallets,
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                };
                for wallet_meta in wallets {
                    match store.load_wallet_transaction_records(&wallet_meta.id) {
                        Ok(mut records) => all_txs.append(&mut records),
                        Err(e) => {
                            emit_error(&app.writer, Some(id.clone()), &e, start).await;
                            return;
                        }
                    }
                }
            }

            if let Some(expected_network) = network {
                all_txs.retain(|item| item.network == expected_network);
            }
            if let Some(since) = since_epoch_s {
                all_txs.retain(|item| item.created_at_epoch_s >= since);
            }
            if let Some(until) = until_epoch_s {
                all_txs.retain(|item| item.created_at_epoch_s < until);
            }
            if let Some(filter) = memo_filter.as_deref() {
                all_txs.retain(|item| item.onchain_memo.as_deref() == Some(filter));
            }
            all_txs.sort_by(|a, b| b.created_at_epoch_s.cmp(&a.created_at_epoch_s));
            let start_idx = all_txs.len().min(off);
            let end_idx = all_txs.len().min(off.saturating_add(lim));
            let items = all_txs[start_idx..end_idx].to_vec();
            let _ = app
                .writer
                .send(Output::History {
                    id,
                    items,
                    trace: trace_from(start),
                })
                .await;
        }

        Input::HistoryStatus { id, transaction_id } => {
            let start = Instant::now();
            let mut routed: Option<Result<HistoryStatusInfo, PayError>> = None;
            for provider in app.providers.values() {
                match provider.history_status(&transaction_id).await {
                    Ok(info) => {
                        routed = Some(Ok(info));
                        break;
                    }
                    Err(PayError::NotImplemented(_)) | Err(PayError::WalletNotFound(_)) => {}
                    Err(err) => {
                        routed = Some(Err(err));
                        break;
                    }
                }
            }
            match routed.unwrap_or_else(|| {
                Err(PayError::WalletNotFound(format!(
                    "transaction {transaction_id} not found"
                )))
            }) {
                Ok(info) => {
                    let _ = app
                        .writer
                        .send(Output::HistoryStatus {
                            id,
                            transaction_id: info.transaction_id,
                            status: info.status,
                            confirmations: info.confirmations,
                            preimage: info.preimage,
                            item: info.item,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::HistoryUpdate {
            id,
            wallet,
            network,
            limit,
        } => {
            let start = Instant::now();
            if let Err(e) = require_store(app) {
                emit_error(&app.writer, Some(id), &e, start).await;
                return;
            }

            let sync_limit = limit.unwrap_or(200).clamp(1, 5000);
            let mut totals = HistorySyncStats::default();
            let mut wallets_synced = 0usize;

            if let Some(wallet_id) = wallet {
                let sync_result = if let Some(expected_network) = network {
                    match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet_id)) {
                        Ok(meta) if meta.network != expected_network => {
                            Err(PayError::InvalidAmount(format!(
                                "wallet {wallet_id} belongs to {}, not {expected_network}",
                                meta.network
                            )))
                        }
                        Ok(_) => match get_provider(&app.providers, expected_network) {
                            Some(provider) => provider.history_sync(&wallet_id, sync_limit).await,
                            None => Err(PayError::NotImplemented(format!(
                                "network {expected_network} not enabled"
                            ))),
                        },
                        Err(e) => Err(e),
                    }
                } else {
                    match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet_id)) {
                        Ok(meta) => match get_provider(&app.providers, meta.network) {
                            Some(provider) => provider.history_sync(&wallet_id, sync_limit).await,
                            None => Err(PayError::NotImplemented(format!(
                                "network {} not enabled",
                                meta.network
                            ))),
                        },
                        Err(e) => Err(e),
                    }
                };

                match sync_result {
                    Ok(stats) => {
                        wallets_synced = 1;
                        totals.records_scanned =
                            totals.records_scanned.saturating_add(stats.records_scanned);
                        totals.records_added =
                            totals.records_added.saturating_add(stats.records_added);
                        totals.records_updated =
                            totals.records_updated.saturating_add(stats.records_updated);
                    }
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                }
            } else {
                let target_networks: Vec<Network> = if let Some(single) = network {
                    vec![single]
                } else {
                    vec![
                        Network::Cashu,
                        Network::Ln,
                        Network::Sol,
                        Network::Evm,
                        Network::Btc,
                    ]
                };

                let wallets = match require_store(app).and_then(|s| s.list_wallet_metadata(None)) {
                    Ok(wallets) => wallets,
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                };

                for network_key in target_networks {
                    let Some(provider) = get_provider(&app.providers, network_key) else {
                        if network.is_some() {
                            emit_error(
                                &app.writer,
                                Some(id),
                                &PayError::NotImplemented(format!(
                                    "network {network_key} not enabled"
                                )),
                                start,
                            )
                            .await;
                            return;
                        }
                        continue;
                    };
                    for wallet_meta in &wallets {
                        if wallet_meta.network != network_key {
                            continue;
                        }
                        match provider.history_sync(&wallet_meta.id, sync_limit).await {
                            Ok(stats) => {
                                wallets_synced = wallets_synced.saturating_add(1);
                                totals.records_scanned =
                                    totals.records_scanned.saturating_add(stats.records_scanned);
                                totals.records_added =
                                    totals.records_added.saturating_add(stats.records_added);
                                totals.records_updated =
                                    totals.records_updated.saturating_add(stats.records_updated);
                            }
                            Err(PayError::NotImplemented(_)) | Err(PayError::WalletNotFound(_)) => {
                            }
                            Err(e) => {
                                emit_error(&app.writer, Some(id), &e, start).await;
                                return;
                            }
                        }
                    }
                }
            }

            let _ = app
                .writer
                .send(Output::HistoryUpdated {
                    id,
                    wallets_synced,
                    records_scanned: totals.records_scanned,
                    records_added: totals.records_added,
                    records_updated: totals.records_updated,
                    trace: trace_from(start),
                })
                .await;
        }

        Input::LimitAdd { id, mut limit } => {
            let start = Instant::now();
            if !app.enforce_limits {
                emit_error(
                    &app.writer,
                    Some(id),
                    &PayError::NotImplemented(
                        "limit_add is unavailable when limits are not enforced locally; configure limits on the RPC daemon"
                            .to_string(),
                    ),
                    start,
                )
                .await;
                return;
            }

            // Auto-fill provider for wallet-scope rules that don't have one
            if limit.scope == SpendScope::Wallet && limit.network.is_none() {
                if let Some(wallet_id) = limit.wallet.as_deref() {
                    match require_store(app).and_then(|s| s.load_wallet_metadata(wallet_id)) {
                        Ok(meta) => {
                            limit.network = Some(meta.network.to_string());
                        }
                        Err(e) => {
                            emit_error(&app.writer, Some(id), &e, start).await;
                            return;
                        }
                    }
                }
            }

            match app.spend_ledger.add_limit(&mut limit).await {
                Ok(rule_id) => {
                    let _ = app
                        .writer
                        .send(Output::LimitAdded {
                            id,
                            rule_id,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::LimitRemove { id, rule_id } => {
            let start = Instant::now();
            if !app.enforce_limits {
                emit_error(
                    &app.writer,
                    Some(id),
                    &PayError::NotImplemented(
                        "limit_remove is unavailable when limits are not enforced locally; configure limits on the RPC daemon"
                            .to_string(),
                    ),
                    start,
                )
                .await;
                return;
            }

            match app.spend_ledger.remove_limit(&rule_id).await {
                Ok(()) => {
                    let _ = app
                        .writer
                        .send(Output::LimitRemoved {
                            id,
                            rule_id,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::LimitList { id } => {
            let start = Instant::now();
            let local_limits = if app.enforce_limits {
                match app.spend_ledger.get_status().await {
                    Ok(status) => status,
                    Err(e) => {
                        emit_error(&app.writer, Some(id), &e, start).await;
                        return;
                    }
                }
            } else {
                vec![]
            };

            // Query downstream afpay_rpc nodes
            let config = app.config.read().await.clone();
            let downstream = query_downstream_limits(&config).await;

            let _ = app
                .writer
                .send(Output::LimitStatus {
                    id,
                    limits: local_limits,
                    downstream,
                    trace: trace_from(start),
                })
                .await;
        }

        Input::LimitSet { id, mut limits } => {
            let start = Instant::now();
            if !app.enforce_limits {
                emit_error(
                    &app.writer,
                    Some(id),
                    &PayError::NotImplemented(
                        "limit_set is unavailable when limits are not enforced locally; configure limits on the RPC daemon"
                            .to_string(),
                    ),
                    start,
                )
                .await;
                return;
            }

            // Auto-fill provider for wallet-scope rules that don't have one
            for rule in &mut limits {
                if rule.scope == SpendScope::Wallet && rule.network.is_none() {
                    if let Some(wallet_id) = rule.wallet.as_deref() {
                        match require_store(app).and_then(|s| s.load_wallet_metadata(wallet_id)) {
                            Ok(meta) => {
                                rule.network = Some(meta.network.to_string());
                            }
                            Err(e) => {
                                emit_error(&app.writer, Some(id), &e, start).await;
                                return;
                            }
                        }
                    }
                }
            }

            match app.spend_ledger.set_limits(&limits).await {
                Ok(()) => match app.spend_ledger.get_status().await {
                    Ok(status) => {
                        let _ = app
                            .writer
                            .send(Output::LimitStatus {
                                id,
                                limits: status,
                                downstream: vec![],
                                trace: trace_from(start),
                            })
                            .await;
                    }
                    Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                },
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::WalletConfigShow { id, wallet } => {
            let start = Instant::now();
            match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet)) {
                Ok(meta) => {
                    let resolved_wallet = meta.id.clone();
                    let _ = app
                        .writer
                        .send(Output::WalletConfig {
                            id,
                            wallet: resolved_wallet,
                            config: meta,
                            trace: trace_from(start),
                        })
                        .await;
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::WalletConfigSet {
            id,
            wallet,
            label,
            rpc_endpoints,
            chain_id,
        } => {
            let start = Instant::now();
            match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet)) {
                Ok(mut meta) => {
                    let resolved_wallet = meta.id.clone();
                    let mut changed = false;

                    if let Some(new_label) = label {
                        let trimmed = new_label.trim();
                        meta.label = if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed.to_string())
                        };
                        changed = true;
                    }

                    if !rpc_endpoints.is_empty() {
                        match meta.network {
                            Network::Sol => {
                                meta.sol_rpc_endpoints = Some(rpc_endpoints);
                                changed = true;
                            }
                            Network::Evm => {
                                meta.evm_rpc_endpoints = Some(rpc_endpoints);
                                changed = true;
                            }
                            _ => {
                                emit_error(
                                    &app.writer,
                                    Some(id),
                                    &PayError::InvalidAmount(format!(
                                        "rpc-endpoint not supported for {} wallets",
                                        meta.network
                                    )),
                                    start,
                                )
                                .await;
                                return;
                            }
                        }
                    }

                    if let Some(cid) = chain_id {
                        if meta.network != Network::Evm {
                            emit_error(
                                &app.writer,
                                Some(id),
                                &PayError::InvalidAmount(
                                    "chain-id is only supported for evm wallets".to_string(),
                                ),
                                start,
                            )
                            .await;
                            return;
                        }
                        meta.evm_chain_id = Some(cid);
                        changed = true;
                    }

                    if !changed {
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::InvalidAmount(
                                "no configuration changes specified".to_string(),
                            ),
                            start,
                        )
                        .await;
                        return;
                    }

                    match require_store(app).and_then(|s| s.save_wallet_metadata(&meta)) {
                        Ok(()) => {
                            let _ = app
                                .writer
                                .send(Output::WalletConfigUpdated {
                                    id,
                                    wallet: resolved_wallet,
                                    trace: trace_from(start),
                                })
                                .await;
                        }
                        Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                    }
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::WalletConfigTokenAdd {
            id,
            wallet,
            symbol,
            address,
            decimals,
        } => {
            let start = Instant::now();
            match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet)) {
                Ok(mut meta) => {
                    let resolved_wallet = meta.id.clone();
                    if !matches!(meta.network, Network::Evm | Network::Sol) {
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::InvalidAmount(format!(
                                "custom tokens not supported for {} wallets",
                                meta.network
                            )),
                            start,
                        )
                        .await;
                        return;
                    }

                    let lower_symbol = symbol.to_ascii_lowercase();
                    let tokens = meta.custom_tokens.get_or_insert_with(Vec::new);
                    if tokens
                        .iter()
                        .any(|t| t.symbol.to_ascii_lowercase() == lower_symbol)
                    {
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::InvalidAmount(format!(
                                "custom token '{lower_symbol}' already registered"
                            )),
                            start,
                        )
                        .await;
                        return;
                    }

                    tokens.push(wallet::CustomToken {
                        symbol: lower_symbol.clone(),
                        address: address.clone(),
                        decimals,
                    });

                    match require_store(app).and_then(|s| s.save_wallet_metadata(&meta)) {
                        Ok(()) => {
                            let _ = app
                                .writer
                                .send(Output::WalletConfigTokenAdded {
                                    id,
                                    wallet: resolved_wallet,
                                    symbol: lower_symbol,
                                    address,
                                    decimals,
                                    trace: trace_from(start),
                                })
                                .await;
                        }
                        Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                    }
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::WalletConfigTokenRemove { id, wallet, symbol } => {
            let start = Instant::now();
            match require_store(app).and_then(|s| s.load_wallet_metadata(&wallet)) {
                Ok(mut meta) => {
                    let resolved_wallet = meta.id.clone();
                    let lower_symbol = symbol.to_ascii_lowercase();
                    let tokens = meta.custom_tokens.get_or_insert_with(Vec::new);
                    let before_len = tokens.len();
                    tokens.retain(|t| t.symbol.to_ascii_lowercase() != lower_symbol);
                    if tokens.len() == before_len {
                        emit_error(
                            &app.writer,
                            Some(id),
                            &PayError::InvalidAmount(format!(
                                "custom token '{lower_symbol}' not found"
                            )),
                            start,
                        )
                        .await;
                        return;
                    }
                    if tokens.is_empty() {
                        meta.custom_tokens = None;
                    }

                    match require_store(app).and_then(|s| s.save_wallet_metadata(&meta)) {
                        Ok(()) => {
                            let _ = app
                                .writer
                                .send(Output::WalletConfigTokenRemoved {
                                    id,
                                    wallet: resolved_wallet,
                                    symbol: lower_symbol,
                                    trace: trace_from(start),
                                })
                                .await;
                        }
                        Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
                    }
                }
                Err(e) => emit_error(&app.writer, Some(id), &e, start).await,
            }
        }

        Input::Config(patch) => {
            let start = Instant::now();
            let ConfigPatch {
                data_dir,
                limits,
                log,
                exchange_rate,
                afpay_rpc,
                providers,
            } = patch;

            let mut unsupported = Vec::new();
            if data_dir.is_some() {
                unsupported.push("data_dir");
            }
            if afpay_rpc.is_some() {
                unsupported.push("afpay_rpc");
            }
            if providers.is_some() {
                unsupported.push("providers");
            }
            if exchange_rate.is_some() {
                unsupported.push("exchange_rate");
            }
            if !unsupported.is_empty() {
                let err = PayError::NotImplemented(format!(
                    "runtime config only supports 'log' and 'limits'; unsupported fields: {}",
                    unsupported.join(", ")
                ));
                emit_error(&app.writer, None, &err, start).await;
                return;
            }

            if let Some(ref v) = limits {
                if !app.enforce_limits {
                    let err = PayError::NotImplemented(
                        "config.limits is unavailable when limits are not enforced locally; configure limits on the RPC daemon"
                            .to_string(),
                    );
                    emit_error(&app.writer, None, &err, start).await;
                    return;
                }
                if let Err(e) = app.spend_ledger.set_limits(v).await {
                    emit_error(&app.writer, None, &e, start).await;
                    return;
                }
            }

            let mut cfg = app.config.write().await;
            if let Some(v) = limits {
                cfg.limits = v;
            }
            if let Some(v) = log {
                cfg.log = agent_first_data::cli_parse_log_filters(&v);
            }
            let _ = app.writer.send(Output::Config(cfg.clone())).await;
        }

        Input::Version => {
            let _ = app
                .writer
                .send(Output::Version {
                    version: crate::config::VERSION.to_string(),
                    trace: PongTrace {
                        uptime_s: app.start_time.elapsed().as_secs(),
                        requests_total: app.requests_total.load(Ordering::Relaxed),
                        in_flight: app.in_flight.lock().await.len(),
                    },
                })
                .await;
        }

        Input::Close => {
            // Handled in main loop
        }
    }

    emit_migration_log(app).await;
}

// ═══════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════

fn get_provider(
    providers: &HashMap<Network, Box<dyn PayProvider>>,
    network: Network,
) -> Option<&dyn PayProvider> {
    providers.get(&network).map(|p| p.as_ref())
}

fn looks_like_bip39_mnemonic(secret: &str) -> bool {
    let words = secret.split_whitespace().count();
    words == 12 || words == 24
}

async fn emit_error(
    writer: &mpsc::Sender<Output>,
    id: Option<String>,
    err: &PayError,
    start: Instant,
) {
    emit_error_hint(writer, id, err, start, None).await;
}

/// Like [`emit_error`] but with an optional hint override.
/// When `hint_override` is `Some`, it takes precedence over `PayError::hint()`.
async fn emit_error_hint(
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

fn extract_id(input: &Input) -> Option<String> {
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

fn trace_from(start: Instant) -> Trace {
    Trace::from_duration(start.elapsed().as_millis() as u64)
}

/// Query limits from each unique downstream afpay_rpc node.
async fn query_downstream_limits(config: &RuntimeConfig) -> Vec<DownstreamLimitNode> {
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
fn extract_token_from_target(to: &str) -> Option<String> {
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

fn wallet_provider_key(meta: &wallet::WalletMetadata) -> String {
    match meta.network {
        Network::Ln => meta
            .backend
            .as_deref()
            .map(|b| format!("ln-{}", b.to_ascii_lowercase()))
            .unwrap_or_else(|| "ln".to_string()),
        _ => meta.network.to_string(),
    }
}

fn wallet_summary_from_meta(meta: &wallet::WalletMetadata, wallet_id: &str) -> WalletSummary {
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

async fn resolve_wallet_summary(app: &App, wallet_id: &str) -> WalletSummary {
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

fn log_enabled(log: &[String], event: &str) -> bool {
    if log.is_empty() {
        return false;
    }
    let ev = event.to_ascii_lowercase();
    log.iter()
        .any(|f| f == "*" || f == "all" || ev.starts_with(f.as_str()))
}

async fn emit_migration_log(app: &App) {
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

async fn emit_log(app: &App, event: &str, request_id: Option<String>, args: serde_json::Value) {
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
