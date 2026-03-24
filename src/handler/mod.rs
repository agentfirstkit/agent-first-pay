#[macro_use]
mod helpers;
mod history;
mod limit;
mod pay;
mod wallet;

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
use crate::provider::{PayError, PayProvider, StubProvider};
use crate::spend::SpendLedger;
use crate::store::StorageBackend;
use crate::types::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;

use helpers::*;

pub struct App {
    pub config: RwLock<RuntimeConfig>,
    pub providers: HashMap<Network, Box<dyn PayProvider>>,
    pub writer: mpsc::Sender<Output>,
    pub in_flight: Mutex<HashMap<String, JoinHandle<()>>>,
    pub requests_total: AtomicU64,
    pub start_time: Instant,
    /// True if any provider uses local data (needs data-dir lock for writes).
    #[cfg(feature = "redb")]
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
                        if let Some(s) = &store {
                            providers.insert(
                                *network,
                                Box::new(LnProvider::new(&config.data_dir, s.clone())),
                            );
                        } else {
                            providers.insert(*network, Box::new(StubProvider::new(*network)));
                        }
                    }
                    #[cfg(feature = "sol")]
                    Network::Sol => {
                        if let Some(s) = &store {
                            providers.insert(
                                *network,
                                Box::new(SolProvider::new(&config.data_dir, s.clone())),
                            );
                        } else {
                            providers.insert(*network, Box::new(StubProvider::new(*network)));
                        }
                    }
                    #[cfg(feature = "evm")]
                    Network::Evm => {
                        if let Some(s) = &store {
                            providers.insert(
                                *network,
                                Box::new(EvmProvider::new(&config.data_dir, s.clone())),
                            );
                        } else {
                            providers.insert(*network, Box::new(StubProvider::new(*network)));
                        }
                    }
                    #[cfg(any(
                        feature = "btc-esplora",
                        feature = "btc-core",
                        feature = "btc-electrum"
                    ))]
                    Network::Btc => {
                        if let Some(s) = &store {
                            providers.insert(
                                *network,
                                Box::new(BtcProvider::new(&config.data_dir, s.clone())),
                            );
                        } else {
                            providers.insert(*network, Box::new(StubProvider::new(*network)));
                        }
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
            #[cfg(feature = "redb")]
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

    match &input {
        // Wallet operations
        Input::WalletCreate { .. }
        | Input::LnWalletCreate { .. }
        | Input::WalletClose { .. }
        | Input::WalletList { .. }
        | Input::Balance { .. }
        | Input::Restore { .. }
        | Input::WalletShowSeed { .. }
        | Input::WalletConfigShow { .. }
        | Input::WalletConfigSet { .. }
        | Input::WalletConfigTokenAdd { .. }
        | Input::WalletConfigTokenRemove { .. } => {
            wallet::dispatch_wallet(app, input).await;
            emit_migration_log(app).await;
            return;
        }

        // Pay / send / receive operations
        Input::Receive { .. }
        | Input::ReceiveClaim { .. }
        | Input::CashuSend { .. }
        | Input::CashuReceive { .. }
        | Input::Send { .. } => {
            pay::dispatch_pay(app, input).await;
            emit_migration_log(app).await;
            return;
        }

        // History operations
        Input::HistoryList { .. } | Input::HistoryStatus { .. } | Input::HistoryUpdate { .. } => {
            history::dispatch_history(app, input).await;
            emit_migration_log(app).await;
            return;
        }

        // Limit operations
        Input::LimitAdd { .. }
        | Input::LimitRemove { .. }
        | Input::LimitList { .. }
        | Input::LimitSet { .. } => {
            limit::dispatch_limit(app, input).await;
            emit_migration_log(app).await;
            return;
        }

        // Inline handlers (small enough to keep in mod.rs)
        Input::Config(_) | Input::ConfigShow { .. } | Input::Version | Input::Close => {}
    }

    // Inline handlers for Config, ConfigShow, Version, Close
    match input {
        Input::ConfigShow { .. } => {
            let cfg = app.config.read().await;
            let _ = app.writer.send(Output::Config(cfg.clone())).await;
        }
        Input::Config(patch) => {
            let start = Instant::now();
            let ConfigPatch {
                data_dir,
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
                    "runtime config only supports 'log'; unsupported fields: {}",
                    unsupported.join(", ")
                ));
                emit_error(&app.writer, None, &err, start).await;
                return;
            }

            let mut cfg = app.config.write().await;
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

        _ => unreachable!(),
    }

    emit_migration_log(app).await;
}
