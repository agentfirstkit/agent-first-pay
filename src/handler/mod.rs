#[macro_use]
mod helpers;
mod history;
mod idempotency;
mod limit;
mod pay;
mod receive_watch;
mod reconcile;
pub mod schema;
mod spend_guard;
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
#[cfg(feature = "federation")]
use crate::provider::remote::RemoteProvider;
#[cfg(feature = "sol")]
use crate::provider::sol::SolProvider;
use crate::provider::{PayError, PayProvider, StubProvider};
use crate::spend::SpendLedger;
use crate::store::StorageBackend;
use crate::types::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::{Mutex, RwLock, mpsc};
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
    /// Daemon modes: always true. CLI/pipe with all remote: false. CLI/pipe with any local: true.
    pub enforce_limits: bool,
    pub spend_ledger: SpendLedger,
    /// Storage backend for wallet metadata and transaction history.
    /// None when running in frontend-only mode (no local DB, only afpay peers).
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
            if let Some(peer_name) = config.providers.get(&key) {
                // Look up the named peer
                if let Some(peer) = config.peers.get(peer_name) {
                    #[cfg(feature = "federation")]
                    {
                        let api_key = peer.api_key_secret.as_deref().unwrap_or("");
                        providers.insert(
                            *network,
                            Box::new(RemoteProvider::new(&peer.url, api_key, *network)),
                        );
                    }
                    #[cfg(not(feature = "federation"))]
                    {
                        let _ = peer;
                        providers.insert(*network, Box::new(StubProvider::new(*network)));
                    }
                } else {
                    // Unknown peer name — insert stub so errors surface at runtime
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
/// Pings every configured afpay peer (deduplicated) and validates provider mappings.
pub async fn startup_provider_validation_errors(config: &RuntimeConfig) -> Vec<Output> {
    let mut errors = Vec::new();

    // Validate that all provider values reference known peer names
    for (network, peer_name) in &config.providers {
        if !config.peers.contains_key(peer_name) {
            errors.push(Output::Error {
                id: None,
                error_code: "invalid_config".to_string(),
                error: format!("providers.{network} references unknown peer '{peer_name}'"),
                hint: Some(format!(
                    "add [peers.{peer_name}] with url and api_key_secret to config.toml"
                )),
                retryable: false,
                retry_after_ms: None,
                trace: Trace::from_duration(0),
            });
        }
    }
    if !errors.is_empty() {
        return errors;
    }

    // Ask each unique peer who it is, once. This is the identity check that
    // makes a mismatched or absent peer fail here — loudly, naming the node —
    // instead of somewhere deep inside a payment.
    #[cfg(feature = "federation")]
    {
        let mut pinged: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (peer_name, peer) in &config.peers {
            if !pinged.insert(peer.url.clone()) {
                continue;
            }
            // Find any network that maps to this peer (for the RemoteProvider constructor)
            let network = config
                .providers
                .iter()
                .find(|(_, name)| *name == peer_name)
                .and_then(|(k, _)| k.parse::<Network>().ok())
                .unwrap_or(Network::Cashu);
            let api_key = peer.api_key_secret.as_deref().unwrap_or("");
            let provider = RemoteProvider::new(&peer.url, api_key, network);
            if let Err(err) = provider.ping().await {
                errors.push(Output::Error {
                    id: None,
                    error_code: "provider_unreachable".to_string(),
                    error: format!("peers.{peer_name} ({}): {err}", peer.url),
                    hint: err.hint().or_else(|| {
                        Some("check the peer url and that its HTTP API is running".to_string())
                    }),
                    retryable: err.retryable(),
                    retry_after_ms: None,
                    trace: Trace::from_duration(0),
                });
            }
        }
    }
    errors
}

pub async fn dispatch(app: &App, request: Request) {
    let Request { dry_run, input } = request;

    // Dry-run short-circuit: emit Output::DryRun for every Input variant
    // without acquiring locks, opening providers, or hitting external services.
    // Works uniformly across cli/pipe/rest so an agent can validate any
    // request without side-effects regardless of transport.
    if dry_run {
        let params = serde_json::to_value(&input).unwrap_or(serde_json::Value::Null);
        let command = params
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let _ = app
            .writer
            .send(Output::DryRun {
                id: extract_id(&input),
                command,
                params,
                trace: Trace::from_duration(0),
            })
            .await;
        return;
    }

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
    if let Some(store) = &app.store
        && let Err(e) = resolve_wallet_labels(&mut input, store.as_ref())
    {
        let id = extract_id(&input);
        emit_error(&app.writer, id, &e, Instant::now()).await;
        return;
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
        | Input::CashuSendPlan { .. }
        | Input::CashuReceive { .. }
        | Input::SendPlan { .. }
        | Input::PayConfirm { .. } => {
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

        // Reservation reconcile (operator action; local-only)
        Input::ReconcileReservation { .. } => {
            reconcile::dispatch_reconcile(app, input).await;
            emit_migration_log(app).await;
            return;
        }

        // Inline handlers (small enough to keep in mod.rs)
        Input::ConfigGet { .. }
        | Input::ConfigSet { .. }
        | Input::Version
        | Input::Schema
        | Input::Close => {}
    }

    // Inline handlers for ConfigGet, ConfigSet, Version, Close
    match input {
        Input::ConfigGet { key, .. } => {
            let start = Instant::now();
            let cfg = app.config.read().await;
            match key {
                None => {
                    let _ = app.writer.send(Output::Config(cfg.clone())).await;
                }
                Some(k) => match cfg.get_key(&k) {
                    Ok(value) => {
                        let _ = app
                            .writer
                            .send(Output::ConfigValue {
                                key: k,
                                value,
                                trace: Trace::from_duration(start.elapsed().as_millis() as u64),
                            })
                            .await;
                    }
                    Err(e) => {
                        emit_error(&app.writer, None, &PayError::invalid_request(e), start).await;
                    }
                },
            }
        }
        Input::ConfigSet { key, values, .. } => {
            let start = Instant::now();
            let mut cfg = app.config.write().await;
            match cfg.set_key(&key, &values) {
                Ok(()) => {
                    let _ = app.writer.send(Output::Config(cfg.clone())).await;
                }
                Err(e) => {
                    emit_error(&app.writer, None, &PayError::invalid_request(e), start).await;
                }
            }
        }

        Input::Version => {
            let _ = app
                .writer
                .send(Output::Version {
                    version: crate::config::VERSION.to_string(),
                    protocol_version: JSON_PROTOCOL_VERSION,
                    trace: PongTrace {
                        uptime_s: app.start_time.elapsed().as_secs(),
                        requests_total: app.requests_total.load(Ordering::Relaxed),
                        in_flight: app.in_flight.lock().await.len(),
                    },
                })
                .await;
        }

        Input::Schema => {
            let start = Instant::now();
            let _ = app
                .writer
                .send(Output::Schema {
                    schema: schema::wire_protocol_schema(),
                    trace: Trace::from_duration(start.elapsed().as_millis() as u64),
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
