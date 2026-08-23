#![cfg_attr(not(any(feature = "redb", feature = "postgres")), allow(dead_code))]

pub mod tokens;

use crate::provider::PayError;
#[cfg(feature = "exchange-rate")]
use crate::types::ExchangeRateSourceType;
use crate::types::{ExchangeRateConfig, SpendLimit, SpendLimitStatus, SpendScope};
#[cfg(feature = "redb")]
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use tokio::sync::Mutex;

#[cfg(feature = "redb")]
use crate::store::db;
#[cfg(feature = "redb")]
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
#[cfg(feature = "redb")]
use std::path::{Path, PathBuf};

#[cfg(feature = "redb")]
const META_COUNTER: TableDefinition<&str, u64> = TableDefinition::new("meta_counter");
#[cfg(feature = "redb")]
const RULE_BY_ID: TableDefinition<&str, &str> = TableDefinition::new("rule_by_id_v3");
#[cfg(feature = "redb")]
const RESERVATION_BY_ID: TableDefinition<u64, &str> = TableDefinition::new("reservation_by_id");
#[cfg(feature = "redb")]
const RESERVATION_ID_BY_OP_ID: TableDefinition<&str, u64> =
    TableDefinition::new("reservation_id_by_op_id");
#[cfg(feature = "redb")]
const SPEND_EVENT_BY_ID: TableDefinition<u64, &str> = TableDefinition::new("spend_event_by_id");
#[cfg(feature = "redb")]
const FX_QUOTE_BY_PAIR: TableDefinition<&str, &str> = TableDefinition::new("quote_by_pair");
#[cfg(feature = "redb")]
const IDEMPOTENCY_BY_KEY: TableDefinition<&str, &str> =
    TableDefinition::new("idempotency_by_key_v1");
#[cfg(feature = "redb")]
const NEXT_RESERVATION_ID_KEY: &str = "next_reservation_id";
#[cfg(feature = "redb")]
const NEXT_EVENT_ID_KEY: &str = "next_event_id";
#[cfg(feature = "redb")]
const SPEND_VERSION: u64 = 1;

/// Idempotency records live for 24h. Long enough to cover a slow BTC settlement
/// or a multi-hop LN retry; short enough that the key→hash table never balloons.
pub(crate) const IDEMPOTENCY_TTL_MS: u64 = 24 * 60 * 60 * 1_000;

/// Max length of an agent-supplied idempotency key. Mirrors common payment APIs
/// (Stripe, PayPal) and bounds the storage cost per record.
pub const IDEMPOTENCY_KEY_MAX_LEN: usize = 128;
#[cfg(feature = "redb")]
const FX_CACHE_VERSION: u64 = 1;

#[derive(Debug, Clone, Hash)]
pub struct SpendContext {
    pub network: String,
    pub wallet: Option<String>,
    pub amount_native: u64,
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum ReservationStatus {
    Pending,
    Confirmed,
    Cancelled,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SpendReservation {
    reservation_id: u64,
    op_id: String,
    network: String,
    wallet: Option<String>,
    #[serde(default)]
    token: Option<String>,
    amount_native: u64,
    amount_usd_cents: Option<u64>,
    status: ReservationStatus,
    created_at_epoch_ms: u64,
    expires_at_epoch_ms: u64,
    finalized_at_epoch_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_hash: Option<String>,
    /// Free-form operator note set by `force_confirm` / `force_cancel`. Surfaced
    /// in audit logs and reservation queries so operators can later see why a
    /// state transition happened outside the normal Pending→Confirmed flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reconcile_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum IdempotencyState {
    Pending,
    Final,
}

/// Replay payload persisted by `idempotency_finalize`. Stores only the
/// "effect data" of the terminal output — fields the agent could not have
/// guessed (transaction_id, fees, preimage) — and never the wrapping `id`
/// field or trace timing. The handler reconstructs a full `Output` from
/// this payload + the current request's id at replay time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IdempotentReplayPayload {
    Sent {
        wallet: String,
        transaction_id: String,
        amount: crate::types::Amount,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fee: Option<crate::types::Amount>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preimage: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reservation_ids: Vec<u64>,
    },
    CashuSent {
        wallet: String,
        transaction_id: String,
        status: crate::types::TxStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fee: Option<crate::types::Amount>,
        token: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reservation_ids: Vec<u64>,
    },
    AccountingInconsistent {
        transaction_id: String,
        reservation_ids: Vec<u64>,
        confirm_errors: Vec<String>,
        hint: String,
    },
    /// A wallet that already exists. The mnemonic is deliberately absent: a
    /// replay record is not a place to keep key material for 24 hours, and the
    /// seed of a wallet that exists is readable on the machine that holds it
    /// (`local_wallet_show_seed`). A replayed create therefore reports the
    /// wallet without re-emitting its seed.
    WalletCreated {
        wallet: String,
        network: crate::types::Network,
        address: String,
    },
    /// A receive that was already placed: the same address, invoice, or mint
    /// quote the first call handed out. Replaying this is the point — a payer
    /// may already be holding it.
    ReceiveInfo {
        wallet: String,
        receive_info: crate::types::ReceiveInfo,
    },
    /// A receive that was placed and then waited until it settled.
    ReceiveClaimed {
        wallet: String,
        amount: crate::types::Amount,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IdempotencyRecord {
    input_hash: String,
    state: IdempotencyState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    payload: Option<IdempotentReplayPayload>,
    created_at_epoch_ms: u64,
    expires_at_epoch_ms: u64,
}

/// Outcome of `SpendLedger::idempotency_claim` — drives how the handler
/// short-circuits the Send / CashuSend flow.
#[derive(Debug)]
pub enum IdempotencyLookup {
    /// Fresh slot claimed. Caller proceeds with the real operation and must
    /// finalize or clear the slot on its terminal output.
    Fresh,
    /// A request with this key + matching hash is already in flight elsewhere.
    /// Caller should return `idempotency_in_progress` to the agent.
    InProgress,
    /// A previous request with this key used a DIFFERENT body. Caller must
    /// return `idempotency_conflict` and refuse to execute.
    Conflict,
    /// The key was used before by an identical request that ran to completion.
    /// Caller replays the stored payload as a fresh Output.
    Replay(IdempotentReplayPayload),
}

/// Outcome of `force_confirm` / `force_cancel`.
#[derive(Debug)]
pub enum ReconcileOutcome {
    /// Reservation existed and was successfully moved to its new state.
    Reconciled {
        previous_status: &'static str,
        new_status: &'static str,
    },
    /// No reservation with that id exists.
    NotFound,
    /// Reservation exists but is already in a terminal state that cannot be
    /// flipped (e.g. asking to confirm a Cancelled one, or vice versa). The
    /// current status is returned so the caller can explain it to the agent.
    AlreadyTerminal { current_status: &'static str },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SpendEvent {
    event_id: u64,
    reservation_id: u64,
    op_id: String,
    network: String,
    wallet: Option<String>,
    #[serde(default)]
    token: Option<String>,
    amount_native: u64,
    amount_usd_cents: Option<u64>,
    created_at_epoch_ms: u64,
    confirmed_at_epoch_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExchangeRateQuote {
    pair: String,
    source: String,
    price: f64,
    fetched_at_epoch_ms: u64,
    expires_at_epoch_ms: u64,
}

// ═══════════════════════════════════════════
// SpendBackend
// ═══════════════════════════════════════════

#[allow(dead_code)] // None variant used when neither redb nor postgres features are enabled
enum SpendBackend {
    #[cfg(feature = "redb")]
    Redb {
        data_dir: String,
    },
    #[cfg(feature = "postgres")]
    Postgres {
        pool: sqlx::PgPool,
    },
    None,
}

// ═══════════════════════════════════════════
// SpendLedger
// ═══════════════════════════════════════════

pub struct SpendLedger {
    backend: SpendBackend,
    exchange_rate: Option<ExchangeRateConfig>,
    mu: Mutex<()>,
    /// Set to true when a cached FX quote's age exceeds 80% of its TTL.
    fx_stale_warned: std::sync::atomic::AtomicBool,
}

impl SpendLedger {
    pub fn new(data_dir: &str, exchange_rate: Option<ExchangeRateConfig>) -> Self {
        #[cfg(feature = "redb")]
        let backend = SpendBackend::Redb {
            data_dir: data_dir.to_string(),
        };
        #[cfg(not(feature = "redb"))]
        let backend = {
            let _ = data_dir;
            SpendBackend::None
        };
        Self {
            backend,
            exchange_rate,
            mu: Mutex::new(()),
            fx_stale_warned: std::sync::atomic::AtomicBool::new(false),
        }
    }

    #[cfg(feature = "postgres")]
    pub fn new_postgres(pool: sqlx::PgPool, exchange_rate: Option<ExchangeRateConfig>) -> Self {
        Self {
            backend: SpendBackend::Postgres { pool },
            exchange_rate,
            mu: Mutex::new(()),
            fx_stale_warned: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Returns true (once) if a stale FX quote was used since last check.
    pub fn take_fx_stale_warning(&self) -> bool {
        self.fx_stale_warned
            .swap(false, std::sync::atomic::Ordering::Relaxed)
    }

    /// Add a single spend limit rule. Generates and assigns a rule_id, returns it.
    pub async fn add_limit(&self, limit: &mut SpendLimit) -> Result<String, PayError> {
        normalize_limit(limit);
        validate_limit(limit, self.exchange_rate.as_ref())?;

        let _guard = self.mu.lock().await;

        match &self.backend {
            #[cfg(feature = "redb")]
            SpendBackend::Redb { .. } => self.add_limit_redb(limit),
            #[cfg(feature = "postgres")]
            SpendBackend::Postgres { .. } => self.add_limit_postgres(limit).await,
            SpendBackend::None => Err(PayError::not_implemented(
                "no storage backend for spend limits".to_string(),
            )),
        }
    }

    /// Remove a spend limit rule by its rule_id.
    pub async fn remove_limit(&self, _rule_id: &str) -> Result<(), PayError> {
        let _guard = self.mu.lock().await;

        match &self.backend {
            #[cfg(feature = "redb")]
            SpendBackend::Redb { .. } => self.remove_limit_redb(_rule_id),
            #[cfg(feature = "postgres")]
            SpendBackend::Postgres { .. } => self.remove_limit_postgres(_rule_id).await,
            SpendBackend::None => Err(PayError::not_implemented(
                "no storage backend for spend limits".to_string(),
            )),
        }
    }

    /// Replace all spend limits (used by config patch / pipe mode).
    pub async fn set_limits(&self, limits: &[SpendLimit]) -> Result<(), PayError> {
        let mut limits = limits.to_vec();
        for limit in &mut limits {
            normalize_limit(limit);
            validate_limit(limit, self.exchange_rate.as_ref())?;
        }

        let _guard = self.mu.lock().await;

        match &self.backend {
            #[cfg(feature = "redb")]
            SpendBackend::Redb { .. } => self.set_limits_redb(&limits),
            #[cfg(feature = "postgres")]
            SpendBackend::Postgres { .. } => self.set_limits_postgres(&limits).await,
            SpendBackend::None => Err(PayError::not_implemented(
                "no storage backend for spend limits".to_string(),
            )),
        }
    }

    /// Compute current status for all limits.
    pub async fn get_status(&self) -> Result<Vec<SpendLimitStatus>, PayError> {
        let _guard = self.mu.lock().await;

        match &self.backend {
            #[cfg(feature = "redb")]
            SpendBackend::Redb { .. } => self.get_status_redb(),
            #[cfg(feature = "postgres")]
            SpendBackend::Postgres { .. } => self.get_status_postgres().await,
            SpendBackend::None => Ok(Vec::new()),
        }
    }

    /// Reserve spend against all matching limits, returns reservation id.
    pub async fn reserve(&self, op_id: &str, ctx: &SpendContext) -> Result<u64, PayError> {
        if op_id.trim().is_empty() {
            return Err(PayError::invalid_amount(
                "op_id cannot be empty".to_string(),
            ));
        }
        if ctx.network.trim().is_empty() {
            return Err(PayError::invalid_amount(
                "network cannot be empty for spend check".to_string(),
            ));
        }
        let request_hash = spend_request_hash(op_id, ctx);

        let _guard = self.mu.lock().await;

        match &self.backend {
            #[cfg(feature = "redb")]
            SpendBackend::Redb { .. } => self.reserve_redb(op_id, ctx, &request_hash).await,
            #[cfg(feature = "postgres")]
            SpendBackend::Postgres { .. } => self.reserve_postgres(op_id, ctx, &request_hash).await,
            SpendBackend::None => Err(PayError::not_implemented(
                "no storage backend for spend limits".to_string(),
            )),
        }
    }

    pub async fn confirm(&self, _reservation_id: u64) -> Result<(), PayError> {
        let _guard = self.mu.lock().await;

        match &self.backend {
            #[cfg(feature = "redb")]
            SpendBackend::Redb { .. } => self.confirm_redb(_reservation_id),
            #[cfg(feature = "postgres")]
            SpendBackend::Postgres { .. } => self.confirm_postgres(_reservation_id).await,
            SpendBackend::None => Err(PayError::not_implemented(
                "no storage backend for spend limits".to_string(),
            )),
        }
    }

    pub async fn cancel(&self, _reservation_id: u64) -> Result<(), PayError> {
        let _guard = self.mu.lock().await;

        match &self.backend {
            #[cfg(feature = "redb")]
            SpendBackend::Redb { .. } => self.cancel_redb(_reservation_id),
            #[cfg(feature = "postgres")]
            SpendBackend::Postgres { .. } => self.cancel_postgres(_reservation_id).await,
            SpendBackend::None => Ok(()),
        }
    }

    /// Atomic key/hash claim: insert a `Pending` slot if the key is unused,
    /// otherwise classify the existing record (Replay / Conflict / InProgress).
    /// Expired records are swept here so the caller never sees them.
    pub async fn idempotency_claim(
        &self,
        key: &str,
        hash: &str,
    ) -> Result<IdempotencyLookup, PayError> {
        if key.is_empty() {
            return Err(PayError::invalid_amount(
                "idempotency_key cannot be empty".to_string(),
            ));
        }
        if key.len() > IDEMPOTENCY_KEY_MAX_LEN {
            return Err(PayError::invalid_amount(format!(
                "idempotency_key length {} exceeds max {IDEMPOTENCY_KEY_MAX_LEN}",
                key.len()
            )));
        }

        let _guard = self.mu.lock().await;
        match &self.backend {
            #[cfg(feature = "redb")]
            SpendBackend::Redb { .. } => self.idempotency_claim_redb(key, hash),
            #[cfg(feature = "postgres")]
            SpendBackend::Postgres { .. } => self.idempotency_claim_postgres(key, hash).await,
            SpendBackend::None => Err(PayError::not_implemented(
                "no storage backend for idempotency".to_string(),
            )),
        }
    }

    /// Promote a claimed Pending slot to Final and stash its replay payload.
    /// Idempotent: if the slot is already Final with the same hash, return Ok
    /// (the in-flight retry won the race and this caller is the late one).
    /// Returns Err if the slot was finalized by a different hash — that means
    /// two callers raced with different bodies AND both squeezed past the
    /// claim window, which shouldn't happen and indicates a bug to surface.
    pub async fn idempotency_finalize(
        &self,
        key: &str,
        hash: &str,
        payload: IdempotentReplayPayload,
    ) -> Result<(), PayError> {
        let _guard = self.mu.lock().await;
        match &self.backend {
            #[cfg(feature = "redb")]
            SpendBackend::Redb { .. } => self.idempotency_finalize_redb(key, hash, payload),
            #[cfg(feature = "postgres")]
            SpendBackend::Postgres { .. } => {
                self.idempotency_finalize_postgres(key, hash, payload).await
            }
            SpendBackend::None => Err(PayError::not_implemented(
                "no storage backend for idempotency".to_string(),
            )),
        }
    }

    /// Release a Pending claim so the same key can be retried fresh (e.g. the
    /// underlying send failed before broadcast). No-op if the slot is already
    /// Final or has a different hash.
    pub async fn idempotency_clear(&self, key: &str, hash: &str) -> Result<(), PayError> {
        let _guard = self.mu.lock().await;
        match &self.backend {
            #[cfg(feature = "redb")]
            SpendBackend::Redb { .. } => self.idempotency_clear_redb(key, hash),
            #[cfg(feature = "postgres")]
            SpendBackend::Postgres { .. } => self.idempotency_clear_postgres(key, hash).await,
            SpendBackend::None => Ok(()),
        }
    }

    /// Force a reservation to Confirmed regardless of Pending/Expired state.
    /// Writes a `SpendEvent` so subsequent limit checks count the spend. The
    /// reason is stored on the reservation for audit.
    pub async fn force_confirm(
        &self,
        reservation_id: u64,
        reason: &str,
    ) -> Result<ReconcileOutcome, PayError> {
        let _guard = self.mu.lock().await;
        match &self.backend {
            #[cfg(feature = "redb")]
            SpendBackend::Redb { .. } => self.force_confirm_redb(reservation_id, reason),
            #[cfg(feature = "postgres")]
            SpendBackend::Postgres { .. } => {
                self.force_confirm_postgres(reservation_id, reason).await
            }
            SpendBackend::None => Err(PayError::not_implemented(
                "no storage backend for spend limits".to_string(),
            )),
        }
    }

    /// Force a reservation to Cancelled regardless of Pending/Expired state.
    /// Does NOT write a SpendEvent — the money never moved. Reason is stored
    /// for audit.
    pub async fn force_cancel(
        &self,
        reservation_id: u64,
        reason: &str,
    ) -> Result<ReconcileOutcome, PayError> {
        let _guard = self.mu.lock().await;
        match &self.backend {
            #[cfg(feature = "redb")]
            SpendBackend::Redb { .. } => self.force_cancel_redb(reservation_id, reason),
            #[cfg(feature = "postgres")]
            SpendBackend::Postgres { .. } => {
                self.force_cancel_postgres(reservation_id, reason).await
            }
            SpendBackend::None => Err(PayError::not_implemented(
                "no storage backend for spend limits".to_string(),
            )),
        }
    }
}

// ═══════════════════════════════════════════
// Redb backend implementation
// ═══════════════════════════════════════════

#[cfg(feature = "redb")]
impl SpendLedger {
    fn spend_db_path(&self) -> PathBuf {
        match &self.backend {
            SpendBackend::Redb { data_dir } => Path::new(data_dir).join("spend").join("spend.redb"),
            #[allow(unreachable_patterns)]
            _ => PathBuf::new(),
        }
    }

    fn exchange_rate_db_path(&self) -> PathBuf {
        match &self.backend {
            SpendBackend::Redb { data_dir } => Path::new(data_dir)
                .join("spend")
                .join("exchange-rate-cache.redb"),
            #[allow(unreachable_patterns)]
            _ => PathBuf::new(),
        }
    }

    fn open_spend_db(&self) -> Result<Database, PayError> {
        db::open_and_migrate(
            &self.spend_db_path(),
            SPEND_VERSION,
            &[
                // v0 → v1: no data migration, just stamp version
                &|_db: &Database| Ok(()),
            ],
        )
    }

    fn open_exchange_rate_db(&self) -> Result<Database, PayError> {
        db::open_and_migrate(
            &self.exchange_rate_db_path(),
            FX_CACHE_VERSION,
            &[
                // v0 → v1: no data migration, just stamp version
                &|_db: &Database| Ok(()),
            ],
        )
    }

    fn add_limit_redb(&self, limit: &mut SpendLimit) -> Result<String, PayError> {
        let db = self.open_spend_db()?;
        let rule_id = generate_rule_identifier()?;
        limit.rule_id = Some(rule_id.clone());
        let encoded = encode(limit)?;
        let write_txn = db
            .begin_write()
            .map_err(|e| PayError::internal_error(format!("spend begin_write: {e}")))?;
        {
            let mut rule_table = write_txn
                .open_table(RULE_BY_ID)
                .map_err(|e| PayError::internal_error(format!("spend open rule table: {e}")))?;
            rule_table
                .insert(rule_id.as_str(), encoded.as_str())
                .map_err(|e| PayError::internal_error(format!("spend insert rule: {e}")))?;
        }
        write_txn
            .commit()
            .map_err(|e| PayError::internal_error(format!("spend commit add_limit: {e}")))?;
        Ok(rule_id)
    }

    fn remove_limit_redb(&self, rule_id: &str) -> Result<(), PayError> {
        let db = self.open_spend_db()?;
        let write_txn = db
            .begin_write()
            .map_err(|e| PayError::internal_error(format!("spend begin_write: {e}")))?;
        {
            let mut rule_table = write_txn
                .open_table(RULE_BY_ID)
                .map_err(|e| PayError::internal_error(format!("spend open rule table: {e}")))?;
            let existed = rule_table
                .remove(rule_id)
                .map_err(|e| PayError::internal_error(format!("spend remove rule: {e}")))?;
            if existed.is_none() {
                return Err(PayError::invalid_amount(format!(
                    "rule_id '{rule_id}' not found"
                )));
            }
        }
        write_txn
            .commit()
            .map_err(|e| PayError::internal_error(format!("spend commit remove_limit: {e}")))
    }

    fn set_limits_redb(&self, limits: &[SpendLimit]) -> Result<(), PayError> {
        let db = self.open_spend_db()?;
        let write_txn = db
            .begin_write()
            .map_err(|e| PayError::internal_error(format!("spend begin_write: {e}")))?;
        {
            let mut rule_table = write_txn
                .open_table(RULE_BY_ID)
                .map_err(|e| PayError::internal_error(format!("spend open rule table: {e}")))?;
            // Clear existing rules
            let existing_ids = rule_table
                .iter()
                .map_err(|e| PayError::internal_error(format!("spend iterate rules: {e}")))?
                .map(|entry| {
                    entry
                        .map(|(k, _)| k.value().to_string())
                        .map_err(|e| PayError::internal_error(format!("spend read rule key: {e}")))
                })
                .collect::<Result<Vec<_>, _>>()?;
            for rid in existing_ids {
                rule_table
                    .remove(rid.as_str())
                    .map_err(|e| PayError::internal_error(format!("spend remove rule: {e}")))?;
            }

            // Insert new rules with generated IDs
            for limit in limits {
                let mut rule = limit.clone();
                let rid = generate_rule_identifier()?;
                rule.rule_id = Some(rid.clone());
                let encoded = encode(&rule)?;
                rule_table
                    .insert(rid.as_str(), encoded.as_str())
                    .map_err(|e| PayError::internal_error(format!("spend insert rule: {e}")))?;
            }
        }
        write_txn
            .commit()
            .map_err(|e| PayError::internal_error(format!("spend commit set_limits: {e}")))
    }

    fn get_status_redb(&self) -> Result<Vec<SpendLimitStatus>, PayError> {
        let db = self.open_spend_db()?;
        let read_txn = db
            .begin_read()
            .map_err(|e| PayError::internal_error(format!("spend begin_read: {e}")))?;
        let rules = load_rules(&read_txn)?;
        let reservations = load_reservations(&read_txn)?;
        let now = now_epoch_ms();
        let mut out = Vec::with_capacity(rules.len());
        for rule in rules {
            let use_usd = rule.scope == SpendScope::GlobalUsdCents;
            let (spent, oldest_ts) = spent_in_window(&rule, &reservations, now, use_usd)?;
            let remaining = rule.max_spend.saturating_sub(spent);
            let window_ms = rule.window_s.saturating_mul(1000);
            let window_reset_s = oldest_ts
                .map(|oldest| (oldest.saturating_add(window_ms)).saturating_sub(now) / 1000)
                .unwrap_or(0);
            out.push(SpendLimitStatus {
                rule_id: rule.rule_id.clone().unwrap_or_default(),
                scope: rule.scope,
                network: rule.network.clone(),
                wallet: rule.wallet.clone(),
                window_s: rule.window_s,
                max_spend: rule.max_spend,
                spent,
                remaining,
                token: rule.token.clone(),
                window_reset_s,
            });
        }
        Ok(out)
    }

    async fn reserve_redb(
        &self,
        op_id: &str,
        ctx: &SpendContext,
        request_hash: &str,
    ) -> Result<u64, PayError> {
        let now = now_epoch_ms();
        let db = self.open_spend_db()?;

        let read_txn = db
            .begin_read()
            .map_err(|e| PayError::internal_error(format!("spend begin_read: {e}")))?;
        let rules = load_rules(&read_txn)?;

        if rules.iter().any(|r| {
            r.scope == SpendScope::Wallet
                && r.network.as_deref() == Some(ctx.network.as_str())
                && ctx.wallet.is_none()
        }) {
            return Err(PayError::invalid_amount(
                "wallet-scoped limits require an explicit wallet".to_string(),
            ));
        }

        // GlobalUsdCents scope needs USD conversion
        let needs_usd = rules.iter().any(|r| r.scope == SpendScope::GlobalUsdCents);
        let amount_usd_cents = if needs_usd {
            Some(
                self.amount_to_usd_cents(&ctx.network, ctx.token.as_deref(), ctx.amount_native)
                    .await?,
            )
        } else {
            None
        };

        let write_txn = db
            .begin_write()
            .map_err(|e| PayError::internal_error(format!("spend begin_write: {e}")))?;

        let mut encoded_blobs: Vec<String> = Vec::new();
        let reservation_id = {
            let mut reservation_index =
                write_txn.open_table(RESERVATION_ID_BY_OP_ID).map_err(|e| {
                    PayError::internal_error(format!("spend open reservation op index: {e}"))
                })?;
            if let Some(existing) = reservation_index
                .get(op_id)
                .map_err(|e| PayError::internal_error(format!("spend read op index: {e}")))?
            {
                let existing_id = existing.value();
                let reservation_table = write_txn.open_table(RESERVATION_BY_ID).map_err(|e| {
                    PayError::internal_error(format!("spend open reservation table: {e}"))
                })?;
                let status = reservation_table
                    .get(existing_id)
                    .map_err(|e| PayError::internal_error(format!("spend read reservation: {e}")))?
                    .map(|value| decode::<SpendReservation>(value.value()))
                    .transpose()?
                    .map(|reservation| reservation.status)
                    .unwrap_or(ReservationStatus::Pending);
                return Err(duplicate_reservation_error(op_id, existing_id, &status));
            }

            let mut reservation_table = write_txn.open_table(RESERVATION_BY_ID).map_err(|e| {
                PayError::internal_error(format!("spend open reservation table: {e}"))
            })?;

            expire_pending(&mut reservation_table, now)?;

            let reservations = reservation_table
                .iter()
                .map_err(|e| PayError::internal_error(format!("spend iterate reservations: {e}")))?
                .map(|entry| {
                    let (_k, v) = entry.map_err(|e| {
                        PayError::internal_error(format!("spend read reservation: {e}"))
                    })?;
                    decode::<SpendReservation>(v.value())
                        .map_err(|e| prepend_err("spend decode reservation", e))
                })
                .collect::<Result<Vec<_>, _>>()?;

            for rule in rules.iter() {
                if !rule_matches_context(
                    rule,
                    &ctx.network,
                    ctx.wallet.as_deref(),
                    ctx.token.as_deref(),
                ) {
                    continue;
                }

                let use_usd = rule.scope == SpendScope::GlobalUsdCents;
                let candidate_amount =
                    amount_for_rule(rule, ctx.amount_native, amount_usd_cents, use_usd)?;
                let (spent, oldest_ts) = spent_in_window(rule, &reservations, now, use_usd)?;
                if spent.saturating_add(candidate_amount) > rule.max_spend {
                    let window_ms = rule.window_s.saturating_mul(1000);
                    let remaining_s = oldest_ts
                        .map(|oldest| (oldest.saturating_add(window_ms)).saturating_sub(now) / 1000)
                        .unwrap_or(0);

                    return Err(PayError::LimitExceeded {
                        rule_id: rule.rule_id.clone().unwrap_or_default(),
                        scope: rule.scope,
                        scope_key: scope_key(rule),
                        spent,
                        max_spend: rule.max_spend,
                        token: rule.token.clone(),
                        remaining_s,
                        origin: None,
                        hint: None,
                    });
                }
            }

            let reservation_id = next_counter(&write_txn, NEXT_RESERVATION_ID_KEY)?;
            let reservation = SpendReservation {
                reservation_id,
                op_id: op_id.to_string(),
                network: ctx.network.clone(),
                wallet: ctx.wallet.clone(),
                token: ctx.token.clone(),
                amount_native: ctx.amount_native,
                amount_usd_cents,
                status: ReservationStatus::Pending,
                created_at_epoch_ms: now,
                expires_at_epoch_ms: now
                    .saturating_add(reservation_ttl_ms_for_network(&ctx.network)),
                finalized_at_epoch_ms: None,
                request_hash: Some(request_hash.to_string()),
                reconcile_reason: None,
            };
            encoded_blobs.push(encode(&reservation)?);
            let encoded = encoded_blobs
                .last()
                .ok_or_else(|| PayError::internal_error("missing reservation blob".to_string()))?;
            reservation_table
                .insert(reservation_id, encoded.as_str())
                .map_err(|e| PayError::internal_error(format!("spend insert reservation: {e}")))?;
            reservation_index
                .insert(op_id, reservation_id)
                .map_err(|e| PayError::internal_error(format!("spend insert op index: {e}")))?;
            reservation_id
        };

        write_txn
            .commit()
            .map_err(|e| PayError::internal_error(format!("spend commit reserve: {e}")))?;
        Ok(reservation_id)
    }

    fn confirm_redb(&self, reservation_id: u64) -> Result<(), PayError> {
        let db = self.open_spend_db()?;
        let now = now_epoch_ms();

        let write_txn = db
            .begin_write()
            .map_err(|e| PayError::internal_error(format!("spend begin_write: {e}")))?;

        let mut encoded_blobs: Vec<String> = Vec::new();
        {
            let mut reservation_table = write_txn.open_table(RESERVATION_BY_ID).map_err(|e| {
                PayError::internal_error(format!("spend open reservation table: {e}"))
            })?;
            let Some(existing_bytes) = reservation_table
                .get(reservation_id)
                .map_err(|e| PayError::internal_error(format!("spend read reservation: {e}")))?
                .map(|g| g.value().to_string())
            else {
                return Err(PayError::internal_error(format!(
                    "reservation {reservation_id} not found"
                )));
            };

            let mut reservation: SpendReservation = decode(&existing_bytes)?;
            if !matches!(reservation.status, ReservationStatus::Pending) {
                return Ok(());
            }

            reservation.status = ReservationStatus::Confirmed;
            reservation.finalized_at_epoch_ms = Some(now);
            encoded_blobs.push(encode(&reservation)?);
            let encoded = encoded_blobs
                .last()
                .ok_or_else(|| PayError::internal_error("missing reservation blob".to_string()))?;
            reservation_table
                .insert(reservation_id, encoded.as_str())
                .map_err(|e| PayError::internal_error(format!("spend update reservation: {e}")))?;

            let mut events = write_txn
                .open_table(SPEND_EVENT_BY_ID)
                .map_err(|e| PayError::internal_error(format!("spend open event table: {e}")))?;
            let event_id = next_counter(&write_txn, NEXT_EVENT_ID_KEY)?;
            let event = SpendEvent {
                event_id,
                reservation_id,
                op_id: reservation.op_id,
                network: reservation.network,
                wallet: reservation.wallet,
                token: reservation.token,
                amount_native: reservation.amount_native,
                amount_usd_cents: reservation.amount_usd_cents,
                created_at_epoch_ms: reservation.created_at_epoch_ms,
                confirmed_at_epoch_ms: now,
            };
            encoded_blobs.push(encode(&event)?);
            let encoded_event = encoded_blobs
                .last()
                .ok_or_else(|| PayError::internal_error("missing event blob".to_string()))?;
            events
                .insert(event_id, encoded_event.as_str())
                .map_err(|e| PayError::internal_error(format!("spend insert event: {e}")))?;
        }

        write_txn
            .commit()
            .map_err(|e| PayError::internal_error(format!("spend commit confirm: {e}")))
    }

    fn cancel_redb(&self, reservation_id: u64) -> Result<(), PayError> {
        let db = self.open_spend_db()?;
        let now = now_epoch_ms();

        let write_txn = db
            .begin_write()
            .map_err(|e| PayError::internal_error(format!("spend begin_write: {e}")))?;

        let mut encoded_blobs: Vec<String> = Vec::new();
        {
            let mut reservation_table = write_txn.open_table(RESERVATION_BY_ID).map_err(|e| {
                PayError::internal_error(format!("spend open reservation table: {e}"))
            })?;
            let existing = reservation_table
                .get(reservation_id)
                .map_err(|e| PayError::internal_error(format!("spend read reservation: {e}")))?;
            let existing_bytes = existing.map(|g| g.value().to_string());
            if let Some(existing_bytes) = existing_bytes {
                let mut reservation: SpendReservation = decode(&existing_bytes)?;
                if matches!(reservation.status, ReservationStatus::Pending) {
                    reservation.status = ReservationStatus::Cancelled;
                    reservation.finalized_at_epoch_ms = Some(now);
                    encoded_blobs.push(encode(&reservation)?);
                    let encoded = encoded_blobs.last().ok_or_else(|| {
                        PayError::internal_error("missing reservation blob".to_string())
                    })?;
                    reservation_table
                        .insert(reservation_id, encoded.as_str())
                        .map_err(|e| {
                            PayError::internal_error(format!("spend update reservation: {e}"))
                        })?;
                }
            }
        }

        write_txn
            .commit()
            .map_err(|e| PayError::internal_error(format!("spend commit cancel: {e}")))
    }

    // ─── idempotency ───────────────────────────

    fn idempotency_claim_redb(&self, key: &str, hash: &str) -> Result<IdempotencyLookup, PayError> {
        let db = self.open_spend_db()?;
        let now = now_epoch_ms();
        let write_txn = db
            .begin_write()
            .map_err(|e| PayError::internal_error(format!("idem begin_write: {e}")))?;

        let outcome = {
            let mut table = write_txn
                .open_table(IDEMPOTENCY_BY_KEY)
                .map_err(|e| PayError::internal_error(format!("idem open table: {e}")))?;

            // Sweep expired records lazily, so the table doesn't grow forever
            // and so an expired Pending slot doesn't masquerade as InProgress.
            sweep_expired_idempotency_redb(&mut table, now)?;

            let existing = table
                .get(key)
                .map_err(|e| PayError::internal_error(format!("idem read: {e}")))?
                .map(|g| g.value().to_string());

            if let Some(bytes) = existing {
                let record: IdempotencyRecord = decode(&bytes)?;
                if record.input_hash != hash {
                    IdempotencyLookup::Conflict
                } else {
                    match record.state {
                        IdempotencyState::Pending => IdempotencyLookup::InProgress,
                        IdempotencyState::Final => {
                            let payload = record.payload.ok_or_else(|| {
                                PayError::internal_error(
                                    "idempotency record final without payload".to_string(),
                                )
                            })?;
                            IdempotencyLookup::Replay(payload)
                        }
                    }
                }
            } else {
                let record = IdempotencyRecord {
                    input_hash: hash.to_string(),
                    state: IdempotencyState::Pending,
                    payload: None,
                    created_at_epoch_ms: now,
                    expires_at_epoch_ms: now.saturating_add(IDEMPOTENCY_TTL_MS),
                };
                let encoded = encode(&record)?;
                table
                    .insert(key, encoded.as_str())
                    .map_err(|e| PayError::internal_error(format!("idem insert pending: {e}")))?;
                IdempotencyLookup::Fresh
            }
        };

        write_txn
            .commit()
            .map_err(|e| PayError::internal_error(format!("idem commit claim: {e}")))?;
        Ok(outcome)
    }

    fn idempotency_finalize_redb(
        &self,
        key: &str,
        hash: &str,
        payload: IdempotentReplayPayload,
    ) -> Result<(), PayError> {
        let db = self.open_spend_db()?;
        let now = now_epoch_ms();
        let write_txn = db
            .begin_write()
            .map_err(|e| PayError::internal_error(format!("idem begin_write: {e}")))?;

        {
            let mut table = write_txn
                .open_table(IDEMPOTENCY_BY_KEY)
                .map_err(|e| PayError::internal_error(format!("idem open table: {e}")))?;

            let existing = table
                .get(key)
                .map_err(|e| PayError::internal_error(format!("idem read: {e}")))?
                .map(|g| g.value().to_string());

            let record = match existing {
                Some(bytes) => {
                    let mut existing_rec: IdempotencyRecord = decode(&bytes)?;
                    if existing_rec.input_hash != hash {
                        return Err(PayError::internal_error(
                            "idempotency_finalize: input_hash drift between claim and finalize"
                                .to_string(),
                        ));
                    }
                    if existing_rec.state == IdempotencyState::Final {
                        return Ok(());
                    }
                    existing_rec.state = IdempotencyState::Final;
                    existing_rec.payload = Some(payload);
                    existing_rec.expires_at_epoch_ms = now.saturating_add(IDEMPOTENCY_TTL_MS);
                    existing_rec
                }
                None => IdempotencyRecord {
                    input_hash: hash.to_string(),
                    state: IdempotencyState::Final,
                    payload: Some(payload),
                    created_at_epoch_ms: now,
                    expires_at_epoch_ms: now.saturating_add(IDEMPOTENCY_TTL_MS),
                },
            };

            let encoded = encode(&record)?;
            table
                .insert(key, encoded.as_str())
                .map_err(|e| PayError::internal_error(format!("idem insert final: {e}")))?;
        }

        write_txn
            .commit()
            .map_err(|e| PayError::internal_error(format!("idem commit finalize: {e}")))
    }

    fn idempotency_clear_redb(&self, key: &str, hash: &str) -> Result<(), PayError> {
        let db = self.open_spend_db()?;
        let write_txn = db
            .begin_write()
            .map_err(|e| PayError::internal_error(format!("idem begin_write: {e}")))?;

        {
            let mut table = write_txn
                .open_table(IDEMPOTENCY_BY_KEY)
                .map_err(|e| PayError::internal_error(format!("idem open table: {e}")))?;

            let existing = table
                .get(key)
                .map_err(|e| PayError::internal_error(format!("idem read: {e}")))?
                .map(|g| g.value().to_string());
            if let Some(bytes) = existing {
                let record: IdempotencyRecord = decode(&bytes)?;
                if record.input_hash == hash && record.state == IdempotencyState::Pending {
                    table.remove(key).map_err(|e| {
                        PayError::internal_error(format!("idem remove pending: {e}"))
                    })?;
                }
            }
        }

        write_txn
            .commit()
            .map_err(|e| PayError::internal_error(format!("idem commit clear: {e}")))
    }

    // ─── reconcile ─────────────────────────────

    fn force_confirm_redb(
        &self,
        reservation_id: u64,
        reason: &str,
    ) -> Result<ReconcileOutcome, PayError> {
        let db = self.open_spend_db()?;
        let now = now_epoch_ms();
        let write_txn = db
            .begin_write()
            .map_err(|e| PayError::internal_error(format!("reconcile begin_write: {e}")))?;

        let mut encoded_blobs: Vec<String> = Vec::new();
        let outcome = {
            let mut reservation_table = write_txn.open_table(RESERVATION_BY_ID).map_err(|e| {
                PayError::internal_error(format!("reconcile open reservation table: {e}"))
            })?;
            let Some(existing_bytes) = reservation_table
                .get(reservation_id)
                .map_err(|e| PayError::internal_error(format!("reconcile read reservation: {e}")))?
                .map(|g| g.value().to_string())
            else {
                return Ok(ReconcileOutcome::NotFound);
            };

            let mut reservation: SpendReservation = decode(&existing_bytes)?;
            let previous = reservation_status_label(&reservation.status);
            match reservation.status {
                ReservationStatus::Confirmed => {
                    return Ok(ReconcileOutcome::AlreadyTerminal {
                        current_status: previous,
                    });
                }
                ReservationStatus::Cancelled => {
                    return Ok(ReconcileOutcome::AlreadyTerminal {
                        current_status: previous,
                    });
                }
                ReservationStatus::Pending | ReservationStatus::Expired => {}
            }

            reservation.status = ReservationStatus::Confirmed;
            reservation.finalized_at_epoch_ms = Some(now);
            reservation.reconcile_reason = Some(reason.to_string());
            encoded_blobs.push(encode(&reservation)?);
            let encoded = encoded_blobs
                .last()
                .ok_or_else(|| PayError::internal_error("missing reservation blob".to_string()))?;
            reservation_table
                .insert(reservation_id, encoded.as_str())
                .map_err(|e| {
                    PayError::internal_error(format!("reconcile update reservation: {e}"))
                })?;

            let mut events = write_txn.open_table(SPEND_EVENT_BY_ID).map_err(|e| {
                PayError::internal_error(format!("reconcile open event table: {e}"))
            })?;
            let event_id = next_counter(&write_txn, NEXT_EVENT_ID_KEY)?;
            let event = SpendEvent {
                event_id,
                reservation_id,
                op_id: reservation.op_id.clone(),
                network: reservation.network.clone(),
                wallet: reservation.wallet.clone(),
                token: reservation.token.clone(),
                amount_native: reservation.amount_native,
                amount_usd_cents: reservation.amount_usd_cents,
                created_at_epoch_ms: reservation.created_at_epoch_ms,
                confirmed_at_epoch_ms: now,
            };
            encoded_blobs.push(encode(&event)?);
            let encoded_event = encoded_blobs
                .last()
                .ok_or_else(|| PayError::internal_error("missing event blob".to_string()))?;
            events
                .insert(event_id, encoded_event.as_str())
                .map_err(|e| PayError::internal_error(format!("reconcile insert event: {e}")))?;

            ReconcileOutcome::Reconciled {
                previous_status: previous,
                new_status: "confirmed",
            }
        };

        write_txn
            .commit()
            .map_err(|e| PayError::internal_error(format!("reconcile commit confirm: {e}")))?;
        Ok(outcome)
    }

    fn force_cancel_redb(
        &self,
        reservation_id: u64,
        reason: &str,
    ) -> Result<ReconcileOutcome, PayError> {
        let db = self.open_spend_db()?;
        let now = now_epoch_ms();
        let write_txn = db
            .begin_write()
            .map_err(|e| PayError::internal_error(format!("reconcile begin_write: {e}")))?;

        let mut encoded_blobs: Vec<String> = Vec::new();
        let outcome = {
            let mut reservation_table = write_txn.open_table(RESERVATION_BY_ID).map_err(|e| {
                PayError::internal_error(format!("reconcile open reservation table: {e}"))
            })?;
            let Some(existing_bytes) = reservation_table
                .get(reservation_id)
                .map_err(|e| PayError::internal_error(format!("reconcile read reservation: {e}")))?
                .map(|g| g.value().to_string())
            else {
                return Ok(ReconcileOutcome::NotFound);
            };
            let mut reservation: SpendReservation = decode(&existing_bytes)?;
            let previous = reservation_status_label(&reservation.status);
            match reservation.status {
                ReservationStatus::Confirmed => {
                    return Ok(ReconcileOutcome::AlreadyTerminal {
                        current_status: previous,
                    });
                }
                ReservationStatus::Cancelled => {
                    return Ok(ReconcileOutcome::AlreadyTerminal {
                        current_status: previous,
                    });
                }
                ReservationStatus::Pending | ReservationStatus::Expired => {}
            }
            reservation.status = ReservationStatus::Cancelled;
            reservation.finalized_at_epoch_ms = Some(now);
            reservation.reconcile_reason = Some(reason.to_string());
            encoded_blobs.push(encode(&reservation)?);
            let encoded = encoded_blobs
                .last()
                .ok_or_else(|| PayError::internal_error("missing reservation blob".to_string()))?;
            reservation_table
                .insert(reservation_id, encoded.as_str())
                .map_err(|e| {
                    PayError::internal_error(format!("reconcile update reservation: {e}"))
                })?;
            ReconcileOutcome::Reconciled {
                previous_status: previous,
                new_status: "cancelled",
            }
        };

        write_txn
            .commit()
            .map_err(|e| PayError::internal_error(format!("reconcile commit cancel: {e}")))?;
        Ok(outcome)
    }
}

// ═══════════════════════════════════════════
// Postgres backend implementation
// ═══════════════════════════════════════════

#[cfg(feature = "postgres")]
impl SpendLedger {
    fn pg_pool(&self) -> Result<&sqlx::PgPool, PayError> {
        match &self.backend {
            SpendBackend::Postgres { pool } => Ok(pool),
            _ => Err(PayError::internal_error(
                "expected postgres spend backend".to_string(),
            )),
        }
    }

    async fn add_limit_postgres(&self, limit: &mut SpendLimit) -> Result<String, PayError> {
        let pool = self.pg_pool()?;
        let rule_id = generate_rule_identifier()?;
        limit.rule_id = Some(rule_id.clone());
        let rule_json = serde_json::to_value(limit)
            .map_err(|e| PayError::internal_error(format!("serialize spend rule: {e}")))?;

        sqlx::query("INSERT INTO spend_rules (rule_id, rule) VALUES ($1, $2)")
            .bind(&rule_id)
            .bind(&rule_json)
            .execute(pool)
            .await
            .map_err(|e| PayError::internal_error(format!("pg insert spend rule: {e}")))?;

        Ok(rule_id)
    }

    async fn remove_limit_postgres(&self, rule_id: &str) -> Result<(), PayError> {
        let pool = self.pg_pool()?;
        let result = sqlx::query("DELETE FROM spend_rules WHERE rule_id = $1")
            .bind(rule_id)
            .execute(pool)
            .await
            .map_err(|e| PayError::internal_error(format!("pg delete spend rule: {e}")))?;

        if result.rows_affected() == 0 {
            return Err(PayError::invalid_amount(format!(
                "rule_id '{rule_id}' not found"
            )));
        }
        Ok(())
    }

    async fn set_limits_postgres(&self, limits: &[SpendLimit]) -> Result<(), PayError> {
        let pool = self.pg_pool()?;
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| PayError::internal_error(format!("pg begin tx: {e}")))?;

        sqlx::query("DELETE FROM spend_rules")
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::internal_error(format!("pg clear spend rules: {e}")))?;

        for limit in limits {
            let mut rule = limit.clone();
            let rid = generate_rule_identifier()?;
            rule.rule_id = Some(rid.clone());
            let rule_json = serde_json::to_value(&rule)
                .map_err(|e| PayError::internal_error(format!("serialize spend rule: {e}")))?;
            sqlx::query("INSERT INTO spend_rules (rule_id, rule) VALUES ($1, $2)")
                .bind(&rid)
                .bind(&rule_json)
                .execute(&mut *tx)
                .await
                .map_err(|e| PayError::internal_error(format!("pg insert spend rule: {e}")))?;
        }

        tx.commit()
            .await
            .map_err(|e| PayError::internal_error(format!("pg commit set_limits: {e}")))
    }

    async fn get_status_postgres(&self) -> Result<Vec<SpendLimitStatus>, PayError> {
        let pool = self.pg_pool()?;
        let rules = pg_load_rules(pool).await?;
        let reservations = pg_load_reservations(pool).await?;
        let now = now_epoch_ms();

        let mut out = Vec::with_capacity(rules.len());
        for rule in rules {
            let use_usd = rule.scope == SpendScope::GlobalUsdCents;
            let (spent, oldest_ts) = spent_in_window(&rule, &reservations, now, use_usd)?;
            let remaining = rule.max_spend.saturating_sub(spent);
            let window_ms = rule.window_s.saturating_mul(1000);
            let window_reset_s = oldest_ts
                .map(|oldest| (oldest.saturating_add(window_ms)).saturating_sub(now) / 1000)
                .unwrap_or(0);
            out.push(SpendLimitStatus {
                rule_id: rule.rule_id.clone().unwrap_or_default(),
                scope: rule.scope,
                network: rule.network.clone(),
                wallet: rule.wallet.clone(),
                window_s: rule.window_s,
                max_spend: rule.max_spend,
                spent,
                remaining,
                token: rule.token.clone(),
                window_reset_s,
            });
        }
        Ok(out)
    }

    async fn reserve_postgres(
        &self,
        op_id: &str,
        ctx: &SpendContext,
        request_hash: &str,
    ) -> Result<u64, PayError> {
        use crate::store::postgres_store::SPEND_ADVISORY_LOCK_KEY;

        let pool = self.pg_pool()?;
        let now = now_epoch_ms();

        // Pre-flight: load rules outside the transaction for USD conversion
        let rules = pg_load_rules(pool).await?;
        if rules.iter().any(|r| {
            r.scope == SpendScope::Wallet
                && r.network.as_deref() == Some(ctx.network.as_str())
                && ctx.wallet.is_none()
        }) {
            return Err(PayError::invalid_amount(
                "wallet-scoped limits require an explicit wallet".to_string(),
            ));
        }

        let needs_usd = rules.iter().any(|r| r.scope == SpendScope::GlobalUsdCents);
        let amount_usd_cents = if needs_usd {
            Some(
                self.amount_to_usd_cents(&ctx.network, ctx.token.as_deref(), ctx.amount_native)
                    .await?,
            )
        } else {
            None
        };

        // Begin serializable transaction with advisory lock
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| PayError::internal_error(format!("pg begin tx: {e}")))?;

        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(SPEND_ADVISORY_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::internal_error(format!("pg advisory lock: {e}")))?;

        // Check for existing reservation with same op_id (idempotency)
        let existing: Option<(i64, serde_json::Value)> = sqlx::query_as(
            "SELECT reservation_id, reservation FROM spend_reservations WHERE op_id = $1",
        )
        .bind(op_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| PayError::internal_error(format!("pg check op_id: {e}")))?;

        if let Some((rid, reservation_json)) = existing {
            let status = serde_json::from_value::<SpendReservation>(reservation_json)
                .map(|reservation| reservation.status)
                .unwrap_or(ReservationStatus::Pending);
            return Err(duplicate_reservation_error(op_id, rid as u64, &status));
        }

        // Expire pending reservations
        pg_expire_pending(&mut tx, now).await?;

        // Load all reservations within the lock
        let reservations = pg_load_reservations_tx(&mut tx).await?;

        // Re-load rules within the lock (could have changed)
        let rules = pg_load_rules_tx(&mut tx).await?;

        // Check limits
        for rule in rules.iter() {
            if !rule_matches_context(
                rule,
                &ctx.network,
                ctx.wallet.as_deref(),
                ctx.token.as_deref(),
            ) {
                continue;
            }

            let use_usd = rule.scope == SpendScope::GlobalUsdCents;
            let candidate_amount =
                amount_for_rule(rule, ctx.amount_native, amount_usd_cents, use_usd)?;
            let (spent, oldest_ts) = spent_in_window(rule, &reservations, now, use_usd)?;
            if spent.saturating_add(candidate_amount) > rule.max_spend {
                let window_ms = rule.window_s.saturating_mul(1000);
                let remaining_s = oldest_ts
                    .map(|oldest| (oldest.saturating_add(window_ms)).saturating_sub(now) / 1000)
                    .unwrap_or(0);

                return Err(PayError::LimitExceeded {
                    rule_id: rule.rule_id.clone().unwrap_or_default(),
                    scope: rule.scope,
                    scope_key: scope_key(rule),
                    spent,
                    max_spend: rule.max_spend,
                    token: rule.token.clone(),
                    remaining_s,
                    origin: None,
                    hint: None,
                });
            }
        }

        // Insert reservation
        let reservation = SpendReservation {
            reservation_id: 0, // will be assigned by BIGSERIAL
            op_id: op_id.to_string(),
            network: ctx.network.clone(),
            wallet: ctx.wallet.clone(),
            token: ctx.token.clone(),
            amount_native: ctx.amount_native,
            amount_usd_cents,
            status: ReservationStatus::Pending,
            created_at_epoch_ms: now,
            expires_at_epoch_ms: now.saturating_add(reservation_ttl_ms_for_network(&ctx.network)),
            finalized_at_epoch_ms: None,
            request_hash: Some(request_hash.to_string()),
            reconcile_reason: None,
        };
        let reservation_json = serde_json::to_value(&reservation)
            .map_err(|e| PayError::internal_error(format!("serialize reservation: {e}")))?;

        let row: (i64,) = sqlx::query_as(
            "INSERT INTO spend_reservations (op_id, reservation) \
             VALUES ($1, $2) RETURNING reservation_id",
        )
        .bind(op_id)
        .bind(&reservation_json)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| PayError::internal_error(format!("pg insert reservation: {e}")))?;

        let reservation_id = row.0 as u64;

        // Update the reservation JSON with the assigned ID
        let mut updated_json = reservation_json;
        updated_json["reservation_id"] = serde_json::json!(reservation_id);
        sqlx::query("UPDATE spend_reservations SET reservation = $1 WHERE reservation_id = $2")
            .bind(&updated_json)
            .bind(row.0)
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::internal_error(format!("pg update reservation id: {e}")))?;

        tx.commit()
            .await
            .map_err(|e| PayError::internal_error(format!("pg commit reserve: {e}")))?;

        Ok(reservation_id)
    }

    async fn confirm_postgres(&self, reservation_id: u64) -> Result<(), PayError> {
        use crate::store::postgres_store::SPEND_ADVISORY_LOCK_KEY;

        let pool = self.pg_pool()?;
        let now = now_epoch_ms();
        let rid = reservation_id as i64;

        // Run read + update + event insert under the same `pg_advisory_xact_lock`
        // that `reserve` holds. Without this, two daemons sharing the same PG
        // instance could race a confirm against another confirm/cancel and one
        // update would silently win. The `FOR UPDATE` row lock is belt-and-braces
        // for cases where confirms touch DIFFERENT reservation_ids concurrently
        // (advisory lock is per-key — a single key serializes everything).
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| PayError::internal_error(format!("pg begin tx: {e}")))?;

        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(SPEND_ADVISORY_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::internal_error(format!("pg advisory lock: {e}")))?;

        let row: Option<(serde_json::Value,)> = sqlx::query_as(
            "SELECT reservation FROM spend_reservations WHERE reservation_id = $1 FOR UPDATE",
        )
        .bind(rid)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| PayError::internal_error(format!("pg read reservation: {e}")))?;

        let Some((res_json,)) = row else {
            return Err(PayError::internal_error(format!(
                "reservation {reservation_id} not found"
            )));
        };

        let mut reservation: SpendReservation = serde_json::from_value(res_json)
            .map_err(|e| PayError::internal_error(format!("pg parse reservation: {e}")))?;

        if !matches!(reservation.status, ReservationStatus::Pending) {
            // Already finalized (confirmed or cancelled) — idempotent no-op.
            return Ok(());
        }

        reservation.status = ReservationStatus::Confirmed;
        reservation.finalized_at_epoch_ms = Some(now);
        let updated_json = serde_json::to_value(&reservation)
            .map_err(|e| PayError::internal_error(format!("serialize reservation: {e}")))?;

        let event = SpendEvent {
            event_id: 0, // assigned by BIGSERIAL
            reservation_id,
            op_id: reservation.op_id,
            network: reservation.network,
            wallet: reservation.wallet,
            token: reservation.token,
            amount_native: reservation.amount_native,
            amount_usd_cents: reservation.amount_usd_cents,
            created_at_epoch_ms: reservation.created_at_epoch_ms,
            confirmed_at_epoch_ms: now,
        };
        let event_json = serde_json::to_value(&event)
            .map_err(|e| PayError::internal_error(format!("serialize spend event: {e}")))?;

        sqlx::query("UPDATE spend_reservations SET reservation = $1 WHERE reservation_id = $2")
            .bind(&updated_json)
            .bind(rid)
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::internal_error(format!("pg update reservation: {e}")))?;

        sqlx::query("INSERT INTO spend_events (reservation_id, event) VALUES ($1, $2)")
            .bind(rid)
            .bind(&event_json)
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::internal_error(format!("pg insert spend event: {e}")))?;

        tx.commit()
            .await
            .map_err(|e| PayError::internal_error(format!("pg commit confirm: {e}")))
    }

    async fn cancel_postgres(&self, reservation_id: u64) -> Result<(), PayError> {
        use crate::store::postgres_store::SPEND_ADVISORY_LOCK_KEY;

        let pool = self.pg_pool()?;
        let now = now_epoch_ms();
        let rid = reservation_id as i64;

        // Same advisory-lock + FOR UPDATE pattern as `confirm_postgres`. A bare
        // pool query (as the previous implementation used) raced against confirm
        // and other cancels, letting two daemons silently overwrite each other.
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| PayError::internal_error(format!("pg begin tx: {e}")))?;

        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(SPEND_ADVISORY_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::internal_error(format!("pg advisory lock: {e}")))?;

        let row: Option<(serde_json::Value,)> = sqlx::query_as(
            "SELECT reservation FROM spend_reservations WHERE reservation_id = $1 FOR UPDATE",
        )
        .bind(rid)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| PayError::internal_error(format!("pg read reservation: {e}")))?;

        if let Some((res_json,)) = row {
            let mut reservation: SpendReservation = serde_json::from_value(res_json)
                .map_err(|e| PayError::internal_error(format!("pg parse reservation: {e}")))?;

            if matches!(reservation.status, ReservationStatus::Pending) {
                reservation.status = ReservationStatus::Cancelled;
                reservation.finalized_at_epoch_ms = Some(now);
                let updated_json = serde_json::to_value(&reservation)
                    .map_err(|e| PayError::internal_error(format!("serialize reservation: {e}")))?;

                sqlx::query(
                    "UPDATE spend_reservations SET reservation = $1 WHERE reservation_id = $2",
                )
                .bind(&updated_json)
                .bind(rid)
                .execute(&mut *tx)
                .await
                .map_err(|e| PayError::internal_error(format!("pg update reservation: {e}")))?;
            }
        }

        tx.commit()
            .await
            .map_err(|e| PayError::internal_error(format!("pg commit cancel: {e}")))?;

        Ok(())
    }

    // ─── idempotency (postgres) ────────────────

    async fn idempotency_claim_postgres(
        &self,
        key: &str,
        hash: &str,
    ) -> Result<IdempotencyLookup, PayError> {
        use crate::store::postgres_store::SPEND_ADVISORY_LOCK_KEY;

        let pool = self.pg_pool()?;
        let now = now_epoch_ms();
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| PayError::internal_error(format!("pg begin tx: {e}")))?;

        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(SPEND_ADVISORY_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::internal_error(format!("pg advisory lock: {e}")))?;

        sqlx::query("DELETE FROM afpay_idempotency WHERE expires_at_ms <= $1")
            .bind(now as i64)
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::internal_error(format!("pg idem sweep: {e}")))?;

        let row: Option<(String, String, Option<serde_json::Value>)> = sqlx::query_as(
            "SELECT state, input_hash, payload_json FROM afpay_idempotency \
             WHERE key = $1 FOR UPDATE",
        )
        .bind(key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| PayError::internal_error(format!("pg idem read: {e}")))?;

        let outcome = if let Some((state, existing_hash, payload_json)) = row {
            if existing_hash != hash {
                IdempotencyLookup::Conflict
            } else if state == "pending" {
                IdempotencyLookup::InProgress
            } else {
                let value = payload_json.ok_or_else(|| {
                    PayError::internal_error("pg idem record final without payload".to_string())
                })?;
                let payload: IdempotentReplayPayload = serde_json::from_value(value)
                    .map_err(|e| PayError::internal_error(format!("pg idem parse payload: {e}")))?;
                IdempotencyLookup::Replay(payload)
            }
        } else {
            sqlx::query(
                "INSERT INTO afpay_idempotency \
                 (key, input_hash, state, payload_json, created_at_ms, expires_at_ms) \
                 VALUES ($1, $2, 'pending', NULL, $3, $4)",
            )
            .bind(key)
            .bind(hash)
            .bind(now as i64)
            .bind(now.saturating_add(IDEMPOTENCY_TTL_MS) as i64)
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::internal_error(format!("pg idem insert pending: {e}")))?;
            IdempotencyLookup::Fresh
        };

        tx.commit()
            .await
            .map_err(|e| PayError::internal_error(format!("pg idem commit claim: {e}")))?;
        Ok(outcome)
    }

    async fn idempotency_finalize_postgres(
        &self,
        key: &str,
        hash: &str,
        payload: IdempotentReplayPayload,
    ) -> Result<(), PayError> {
        use crate::store::postgres_store::SPEND_ADVISORY_LOCK_KEY;

        let pool = self.pg_pool()?;
        let now = now_epoch_ms();
        let payload_json = serde_json::to_value(&payload)
            .map_err(|e| PayError::internal_error(format!("serialize idem payload: {e}")))?;
        let expires_at = now.saturating_add(IDEMPOTENCY_TTL_MS) as i64;

        let mut tx = pool
            .begin()
            .await
            .map_err(|e| PayError::internal_error(format!("pg begin tx: {e}")))?;

        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(SPEND_ADVISORY_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::internal_error(format!("pg advisory lock: {e}")))?;

        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT state, input_hash FROM afpay_idempotency WHERE key = $1 FOR UPDATE",
        )
        .bind(key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| PayError::internal_error(format!("pg idem read finalize: {e}")))?;

        match row {
            Some((state, existing_hash)) => {
                if existing_hash != hash {
                    return Err(PayError::internal_error(
                        "idempotency_finalize: input_hash drift between claim and finalize"
                            .to_string(),
                    ));
                }
                if state == "final" {
                    tx.commit().await.map_err(|e| {
                        PayError::internal_error(format!("pg idem commit finalize: {e}"))
                    })?;
                    return Ok(());
                }
                sqlx::query(
                    "UPDATE afpay_idempotency \
                     SET state = 'final', payload_json = $1, expires_at_ms = $2 \
                     WHERE key = $3",
                )
                .bind(&payload_json)
                .bind(expires_at)
                .bind(key)
                .execute(&mut *tx)
                .await
                .map_err(|e| PayError::internal_error(format!("pg idem promote final: {e}")))?;
            }
            None => {
                sqlx::query(
                    "INSERT INTO afpay_idempotency \
                     (key, input_hash, state, payload_json, created_at_ms, expires_at_ms) \
                     VALUES ($1, $2, 'final', $3, $4, $5)",
                )
                .bind(key)
                .bind(hash)
                .bind(&payload_json)
                .bind(now as i64)
                .bind(expires_at)
                .execute(&mut *tx)
                .await
                .map_err(|e| PayError::internal_error(format!("pg idem insert final: {e}")))?;
            }
        }

        tx.commit()
            .await
            .map_err(|e| PayError::internal_error(format!("pg idem commit finalize: {e}")))
    }

    async fn idempotency_clear_postgres(&self, key: &str, hash: &str) -> Result<(), PayError> {
        let pool = self.pg_pool()?;
        sqlx::query(
            "DELETE FROM afpay_idempotency \
             WHERE key = $1 AND input_hash = $2 AND state = 'pending'",
        )
        .bind(key)
        .bind(hash)
        .execute(pool)
        .await
        .map_err(|e| PayError::internal_error(format!("pg idem clear: {e}")))?;
        Ok(())
    }

    // ─── reconcile (postgres) ──────────────────

    async fn force_confirm_postgres(
        &self,
        reservation_id: u64,
        reason: &str,
    ) -> Result<ReconcileOutcome, PayError> {
        use crate::store::postgres_store::SPEND_ADVISORY_LOCK_KEY;

        let pool = self.pg_pool()?;
        let now = now_epoch_ms();
        let rid = reservation_id as i64;
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| PayError::internal_error(format!("pg begin tx: {e}")))?;

        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(SPEND_ADVISORY_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::internal_error(format!("pg advisory lock: {e}")))?;

        let row: Option<(serde_json::Value,)> = sqlx::query_as(
            "SELECT reservation FROM spend_reservations WHERE reservation_id = $1 FOR UPDATE",
        )
        .bind(rid)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| PayError::internal_error(format!("pg read reservation: {e}")))?;

        let Some((res_json,)) = row else {
            return Ok(ReconcileOutcome::NotFound);
        };
        let mut reservation: SpendReservation = serde_json::from_value(res_json)
            .map_err(|e| PayError::internal_error(format!("pg parse reservation: {e}")))?;
        let previous = reservation_status_label(&reservation.status);
        match reservation.status {
            ReservationStatus::Confirmed | ReservationStatus::Cancelled => {
                return Ok(ReconcileOutcome::AlreadyTerminal {
                    current_status: previous,
                });
            }
            ReservationStatus::Pending | ReservationStatus::Expired => {}
        }

        reservation.status = ReservationStatus::Confirmed;
        reservation.finalized_at_epoch_ms = Some(now);
        reservation.reconcile_reason = Some(reason.to_string());
        let updated_json = serde_json::to_value(&reservation)
            .map_err(|e| PayError::internal_error(format!("serialize reservation: {e}")))?;
        let event = SpendEvent {
            event_id: 0,
            reservation_id,
            op_id: reservation.op_id.clone(),
            network: reservation.network.clone(),
            wallet: reservation.wallet.clone(),
            token: reservation.token.clone(),
            amount_native: reservation.amount_native,
            amount_usd_cents: reservation.amount_usd_cents,
            created_at_epoch_ms: reservation.created_at_epoch_ms,
            confirmed_at_epoch_ms: now,
        };
        let event_json = serde_json::to_value(&event)
            .map_err(|e| PayError::internal_error(format!("serialize spend event: {e}")))?;

        sqlx::query("UPDATE spend_reservations SET reservation = $1 WHERE reservation_id = $2")
            .bind(&updated_json)
            .bind(rid)
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::internal_error(format!("pg update reservation: {e}")))?;
        sqlx::query("INSERT INTO spend_events (reservation_id, event) VALUES ($1, $2)")
            .bind(rid)
            .bind(&event_json)
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::internal_error(format!("pg insert spend event: {e}")))?;

        tx.commit()
            .await
            .map_err(|e| PayError::internal_error(format!("pg commit reconcile confirm: {e}")))?;

        Ok(ReconcileOutcome::Reconciled {
            previous_status: previous,
            new_status: "confirmed",
        })
    }

    async fn force_cancel_postgres(
        &self,
        reservation_id: u64,
        reason: &str,
    ) -> Result<ReconcileOutcome, PayError> {
        use crate::store::postgres_store::SPEND_ADVISORY_LOCK_KEY;

        let pool = self.pg_pool()?;
        let now = now_epoch_ms();
        let rid = reservation_id as i64;
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| PayError::internal_error(format!("pg begin tx: {e}")))?;

        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(SPEND_ADVISORY_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::internal_error(format!("pg advisory lock: {e}")))?;

        let row: Option<(serde_json::Value,)> = sqlx::query_as(
            "SELECT reservation FROM spend_reservations WHERE reservation_id = $1 FOR UPDATE",
        )
        .bind(rid)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| PayError::internal_error(format!("pg read reservation: {e}")))?;
        let Some((res_json,)) = row else {
            return Ok(ReconcileOutcome::NotFound);
        };
        let mut reservation: SpendReservation = serde_json::from_value(res_json)
            .map_err(|e| PayError::internal_error(format!("pg parse reservation: {e}")))?;
        let previous = reservation_status_label(&reservation.status);
        match reservation.status {
            ReservationStatus::Confirmed | ReservationStatus::Cancelled => {
                return Ok(ReconcileOutcome::AlreadyTerminal {
                    current_status: previous,
                });
            }
            ReservationStatus::Pending | ReservationStatus::Expired => {}
        }
        reservation.status = ReservationStatus::Cancelled;
        reservation.finalized_at_epoch_ms = Some(now);
        reservation.reconcile_reason = Some(reason.to_string());
        let updated_json = serde_json::to_value(&reservation)
            .map_err(|e| PayError::internal_error(format!("serialize reservation: {e}")))?;
        sqlx::query("UPDATE spend_reservations SET reservation = $1 WHERE reservation_id = $2")
            .bind(&updated_json)
            .bind(rid)
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::internal_error(format!("pg update reservation: {e}")))?;

        tx.commit()
            .await
            .map_err(|e| PayError::internal_error(format!("pg commit reconcile cancel: {e}")))?;

        Ok(ReconcileOutcome::Reconciled {
            previous_status: previous,
            new_status: "cancelled",
        })
    }
}

#[cfg(feature = "postgres")]
async fn pg_load_rules(pool: &sqlx::PgPool) -> Result<Vec<SpendLimit>, PayError> {
    let rows: Vec<(serde_json::Value,)> =
        sqlx::query_as("SELECT rule FROM spend_rules ORDER BY rule_id")
            .fetch_all(pool)
            .await
            .map_err(|e| PayError::internal_error(format!("pg load spend rules: {e}")))?;
    rows.into_iter()
        .map(|(v,)| {
            serde_json::from_value(v)
                .map_err(|e| PayError::internal_error(format!("pg parse spend rule: {e}")))
        })
        .collect()
}

#[cfg(feature = "postgres")]
async fn pg_load_rules_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<Vec<SpendLimit>, PayError> {
    let rows: Vec<(serde_json::Value,)> =
        sqlx::query_as("SELECT rule FROM spend_rules ORDER BY rule_id")
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| PayError::internal_error(format!("pg load spend rules: {e}")))?;
    rows.into_iter()
        .map(|(v,)| {
            serde_json::from_value(v)
                .map_err(|e| PayError::internal_error(format!("pg parse spend rule: {e}")))
        })
        .collect()
}

#[cfg(feature = "postgres")]
async fn pg_load_reservations(pool: &sqlx::PgPool) -> Result<Vec<SpendReservation>, PayError> {
    let rows: Vec<(serde_json::Value,)> =
        sqlx::query_as("SELECT reservation FROM spend_reservations ORDER BY reservation_id")
            .fetch_all(pool)
            .await
            .map_err(|e| PayError::internal_error(format!("pg load reservations: {e}")))?;
    rows.into_iter()
        .map(|(v,)| {
            serde_json::from_value(v)
                .map_err(|e| PayError::internal_error(format!("pg parse reservation: {e}")))
        })
        .collect()
}

#[cfg(feature = "postgres")]
async fn pg_load_reservations_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<Vec<SpendReservation>, PayError> {
    let rows: Vec<(serde_json::Value,)> =
        sqlx::query_as("SELECT reservation FROM spend_reservations ORDER BY reservation_id")
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| PayError::internal_error(format!("pg load reservations: {e}")))?;
    rows.into_iter()
        .map(|(v,)| {
            serde_json::from_value(v)
                .map_err(|e| PayError::internal_error(format!("pg parse reservation: {e}")))
        })
        .collect()
}

#[cfg(feature = "postgres")]
async fn pg_expire_pending(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    now_ms: u64,
) -> Result<(), PayError> {
    // Load pending reservations and expire those past their deadline
    let rows: Vec<(i64, serde_json::Value)> =
        sqlx::query_as("SELECT reservation_id, reservation FROM spend_reservations")
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| {
                PayError::internal_error(format!("pg load reservations for expire: {e}"))
            })?;

    for (rid, res_json) in rows {
        let mut reservation: SpendReservation = serde_json::from_value(res_json)
            .map_err(|e| PayError::internal_error(format!("pg parse reservation: {e}")))?;
        if matches!(reservation.status, ReservationStatus::Pending)
            && reservation.expires_at_epoch_ms <= now_ms
        {
            reservation.status = ReservationStatus::Expired;
            reservation.finalized_at_epoch_ms = Some(now_ms);
            let updated = serde_json::to_value(&reservation)
                .map_err(|e| PayError::internal_error(format!("serialize reservation: {e}")))?;
            sqlx::query("UPDATE spend_reservations SET reservation = $1 WHERE reservation_id = $2")
                .bind(&updated)
                .bind(rid)
                .execute(&mut **tx)
                .await
                .map_err(|e| PayError::internal_error(format!("pg expire reservation: {e}")))?;
        }
    }
    Ok(())
}

// ═══════════════════════════════════════════
// Exchange rate (shared, delegates to backend for caching)
// ═══════════════════════════════════════════

impl SpendLedger {
    async fn amount_to_usd_cents(
        &self,
        network: &str,
        token: Option<&str>,
        amount_native: u64,
    ) -> Result<u64, PayError> {
        let (symbol, divisor) = token_asset(network, token).ok_or_else(|| {
            PayError::invalid_amount(format!(
                "network '{network}' token '{token:?}' is unsupported for global-usd-cents limits"
            ))
        })?;

        let quote = if symbol == "USD" {
            let now = now_epoch_ms();
            ExchangeRateQuote {
                pair: "USD/USD".to_string(),
                source: "identity".to_string(),
                price: 1.0,
                fetched_at_epoch_ms: now,
                expires_at_epoch_ms: now.saturating_add(86_400_000),
            }
        } else {
            self.get_or_fetch_quote(symbol, "USD").await?
        };

        // Block if the quote has fully expired (fetch must have failed silently
        // in a prior call, or the clock jumped).
        let now = now_epoch_ms();
        if quote.expires_at_epoch_ms > 0 && now > quote.expires_at_epoch_ms {
            return Err(PayError::network_error(
                "exchange-rate quote expired — cannot convert to USD; check exchange_rate sources"
                    .to_string(),
            ));
        }

        // Flag if cached quote age exceeds 80% of its TTL (set on every occurrence
        // so callers can surface the warning per-request).
        let ttl_ms = quote
            .expires_at_epoch_ms
            .saturating_sub(quote.fetched_at_epoch_ms);
        let age_ms = now.saturating_sub(quote.fetched_at_epoch_ms);
        if ttl_ms > 0 && age_ms > ttl_ms * 4 / 5 {
            self.fx_stale_warned
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }

        let usd = (amount_native as f64 / divisor) * quote.price;
        if !usd.is_finite() || usd < 0f64 {
            return Err(PayError::internal_error(
                "invalid exchange-rate conversion result".to_string(),
            ));
        }
        // Rounded up, never to nearest.
        //
        // This number is charged against a spending limit, and rounding a
        // payment down is the ledger saying it cost less than it did. Worse, it
        // can say a payment cost nothing at all: a USD-priced token with six
        // decimals, sent 4,999 base units at a time, is 0.4999 cents, which
        // `round()` made zero — so the same send repeated any number of times
        // never touched the global cap. Anything worth more than nothing costs
        // at least one cent here.
        //
        // The bias is deliberate and only ever against the spender. It does not
        // make this a fixed-point ledger — `f64` still decides the value — but
        // it removes the direction of error that lets a limit be walked past.
        Ok((usd * 100f64).ceil() as u64)
    }

    async fn get_or_fetch_quote(
        &self,
        base: &str,
        quote: &str,
    ) -> Result<ExchangeRateQuote, PayError> {
        let pair = format!(
            "{}/{}",
            base.to_ascii_uppercase(),
            quote.to_ascii_uppercase()
        );
        let now = now_epoch_ms();

        // Try cache — redb
        #[cfg(feature = "redb")]
        if let SpendBackend::Redb { .. } = &self.backend {
            let fx_db = self.open_exchange_rate_db()?;
            let read_txn = fx_db
                .begin_read()
                .map_err(|e| PayError::internal_error(format!("fx begin_read: {e}")))?;
            if let Ok(table) = read_txn.open_table(FX_QUOTE_BY_PAIR)
                && let Some(entry) = table
                    .get(pair.as_str())
                    .map_err(|e| PayError::internal_error(format!("fx read quote: {e}")))?
            {
                let cached: ExchangeRateQuote = decode(entry.value())?;
                if cached.expires_at_epoch_ms > now {
                    return Ok(cached);
                }
            }
        }

        // Try cache — postgres
        #[cfg(feature = "postgres")]
        if let SpendBackend::Postgres { pool } = &self.backend {
            let row: Option<(serde_json::Value,)> =
                sqlx::query_as("SELECT quote FROM exchange_rate_cache WHERE pair = $1")
                    .bind(&pair)
                    .fetch_optional(pool)
                    .await
                    .map_err(|e| PayError::internal_error(format!("pg fx read cache: {e}")))?;
            if let Some((quote_json,)) = row {
                let cached: ExchangeRateQuote = serde_json::from_value(quote_json)
                    .map_err(|e| PayError::internal_error(format!("pg fx parse cache: {e}")))?;
                if cached.expires_at_epoch_ms > now {
                    return Ok(cached);
                }
            }
        }

        let (fetched_price, source_name) = self.fetch_exchange_rate_http(base, quote).await?;
        let ttl_s = self
            .exchange_rate
            .as_ref()
            .map(|cfg| cfg.ttl_s)
            .unwrap_or(300)
            .max(1);
        let new_quote = ExchangeRateQuote {
            pair: pair.clone(),
            source: source_name,
            price: fetched_price,
            fetched_at_epoch_ms: now,
            expires_at_epoch_ms: now.saturating_add(ttl_s.saturating_mul(1000)),
        };

        // Write cache — redb
        #[cfg(feature = "redb")]
        if let SpendBackend::Redb { .. } = &self.backend {
            let fx_db = self.open_exchange_rate_db()?;
            let write_txn = fx_db
                .begin_write()
                .map_err(|e| PayError::internal_error(format!("fx begin_write: {e}")))?;
            let mut encoded_blobs: Vec<String> = Vec::new();
            {
                let mut table = write_txn
                    .open_table(FX_QUOTE_BY_PAIR)
                    .map_err(|e| PayError::internal_error(format!("fx open quote table: {e}")))?;
                encoded_blobs.push(encode(&new_quote)?);
                let encoded = encoded_blobs
                    .last()
                    .ok_or_else(|| PayError::internal_error("missing quote blob".to_string()))?;
                table
                    .insert(pair.as_str(), encoded.as_str())
                    .map_err(|e| PayError::internal_error(format!("fx insert quote: {e}")))?;
            }
            write_txn
                .commit()
                .map_err(|e| PayError::internal_error(format!("fx commit write: {e}")))?;
        }

        // Write cache — postgres
        #[cfg(feature = "postgres")]
        if let SpendBackend::Postgres { pool } = &self.backend {
            let quote_json = serde_json::to_value(&new_quote)
                .map_err(|e| PayError::internal_error(format!("serialize fx quote: {e}")))?;
            let _ = sqlx::query(
                "INSERT INTO exchange_rate_cache (pair, quote) VALUES ($1, $2) \
                 ON CONFLICT (pair) DO UPDATE SET quote = $2",
            )
            .bind(&pair)
            .bind(&quote_json)
            .execute(pool)
            .await;
        }

        Ok(new_quote)
    }

    #[cfg(feature = "exchange-rate")]
    async fn fetch_exchange_rate_http(
        &self,
        base: &str,
        quote_currency: &str,
    ) -> Result<(f64, String), PayError> {
        let cfg = self.exchange_rate.as_ref().cloned().unwrap_or_default();

        if cfg.sources.is_empty() {
            return Err(PayError::invalid_amount(
                "exchange_rate.sources is empty — no exchange-rate API configured".to_string(),
            ));
        }

        let client = reqwest::Client::new();
        let mut last_err = String::new();

        for source in &cfg.sources {
            match fetch_from_source(&client, source, base, quote_currency).await {
                Ok(price) => return Ok((price, source.endpoint.clone())),
                Err(e) => {
                    last_err =
                        format!("{} ({}): {e}", source.endpoint, source.source_type.as_str());
                }
            }
        }

        Err(PayError::network_error(format!(
            "all exchange-rate sources failed; last: {last_err}"
        )))
    }

    #[cfg(not(feature = "exchange-rate"))]
    async fn fetch_exchange_rate_http(
        &self,
        _base: &str,
        _quote_currency: &str,
    ) -> Result<(f64, String), PayError> {
        Err(PayError::not_implemented(
            "exchange-rate HTTP support is not built in this feature set".to_string(),
        ))
    }
}

// ═══════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════

/// Reservation TTL by network. Picked to bracket the typical settlement
/// window: long enough that a successful payment confirms within it, short
/// enough that a stuck reservation does not silently lock spend headroom
/// for hours. BTC is the outlier (10+ min confirms), Cashu/LN are nearly
/// instant.
pub(crate) fn reservation_ttl_ms_for_network(network: &str) -> u64 {
    match network.to_ascii_lowercase().as_str() {
        "cashu" => 60_000,        // 60s
        "ln" => 90_000,           // 90s
        "sol" => 120_000,         // 120s
        "evm" => 180_000,         // 180s
        "btc" => 30 * 60 * 1_000, // 30 minutes
        _ => 5 * 60 * 1_000,      // 5 minutes — pre-existing default
    }
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn spend_request_hash(op_id: &str, ctx: &SpendContext) -> String {
    let mut hasher = DefaultHasher::new();
    op_id.hash(&mut hasher);
    ctx.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn reservation_status_label(status: &ReservationStatus) -> &'static str {
    match status {
        ReservationStatus::Pending => "pending",
        ReservationStatus::Confirmed => "confirmed",
        ReservationStatus::Cancelled => "cancelled",
        ReservationStatus::Expired => "expired",
    }
}

fn duplicate_reservation_error(
    op_id: &str,
    reservation_id: u64,
    status: &ReservationStatus,
) -> PayError {
    PayError::invalid_amount(format!(
        "duplicate spend operation id '{op_id}' already has reservation {reservation_id} ({status}); refusing to re-execute payment",
        status = reservation_status_label(status)
    ))
}

fn normalize_limit(rule: &mut SpendLimit) {
    rule.network = rule
        .network
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    rule.wallet = rule
        .wallet
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    rule.token = rule
        .token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| canonical_spend_token(rule.network.as_deref().unwrap_or(""), value));

    if matches!(rule.scope, SpendScope::Network | SpendScope::Wallet)
        && matches!(rule.network.as_deref(), Some("sol" | "evm"))
        && rule.token.is_none()
    {
        rule.token = Some("native".to_string());
    }
}

fn canonical_spend_token(network: &str, token: &str) -> String {
    let token = token.trim().to_ascii_lowercase();
    match (network, token.as_str()) {
        ("sol", "sol" | "native" | "lamports") => "native".to_string(),
        ("evm", "eth" | "native" | "wei") => "native".to_string(),
        _ => token,
    }
}

fn token_asset(network: &str, token: Option<&str>) -> Option<(&'static str, f64)> {
    let network = network.to_ascii_lowercase();
    match token.map(|t| canonical_spend_token(&network, t)).as_deref() {
        Some("native") => {
            if network == "sol" {
                Some(("SOL", 1e9))
            } else if network == "evm" {
                Some(("ETH", 1e18))
            } else if network.starts_with("ln") || network == "cashu" || network == "btc" {
                Some(("BTC", 1e8))
            } else {
                None
            }
        }
        Some("btc" | "sat" | "sats") => Some(("BTC", 1e8)),
        Some("sol") => Some(("SOL", 1e9)),
        Some("eth") => Some(("ETH", 1e18)),
        Some("usdc" | "usdt") => Some(("USD", 1e6)),
        Some(_) => None,
        None => {
            if network.starts_with("ln") || network == "cashu" || network == "btc" {
                Some(("BTC", 1e8))
            } else {
                None
            }
        }
    }
}

#[cfg(feature = "exchange-rate")]
fn extract_price_generic(value: &serde_json::Value) -> Option<f64> {
    value
        .get("price")
        .and_then(|v| v.as_f64())
        .or_else(|| value.get("rate").and_then(|v| v.as_f64()))
        .or_else(|| value.get("usd_per_base").and_then(|v| v.as_f64()))
        .or_else(|| {
            value
                .get("data")
                .and_then(|d| d.get("price"))
                .and_then(|v| v.as_f64())
        })
}

#[cfg(feature = "exchange-rate")]
impl ExchangeRateSourceType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::CoinGecko => "coingecko",
            Self::Kraken => "kraken",
        }
    }
}

#[cfg(feature = "exchange-rate")]
fn coingecko_coin_id(symbol: &str) -> Option<&'static str> {
    match symbol.to_ascii_uppercase().as_str() {
        "BTC" => Some("bitcoin"),
        "SOL" => Some("solana"),
        "ETH" => Some("ethereum"),
        _ => None,
    }
}

#[cfg(feature = "exchange-rate")]
fn kraken_pair(symbol: &str) -> Option<&'static str> {
    match symbol.to_ascii_uppercase().as_str() {
        "BTC" => Some("XBTUSD"),
        "SOL" => Some("SOLUSD"),
        "ETH" => Some("ETHUSD"),
        _ => None,
    }
}

#[cfg(feature = "exchange-rate")]
async fn fetch_from_source(
    client: &reqwest::Client,
    source: &crate::types::ExchangeRateSource,
    base: &str,
    quote_currency: &str,
) -> Result<f64, String> {
    type PriceExtractor = Box<dyn Fn(&serde_json::Value) -> Option<f64> + Send>;
    let (url, extract_fn): (String, PriceExtractor) = match source.source_type {
        ExchangeRateSourceType::Kraken => {
            let pair = kraken_pair(base)
                .ok_or_else(|| format!("kraken: unsupported base asset '{base}'"))?;
            let url = format!("{}/0/public/Ticker?pair={pair}", source.endpoint);
            let pair_owned = pair.to_string();
            (
                url,
                Box::new(move |v: &serde_json::Value| {
                    let result = v.get("result")?;
                    let ticker = result
                        .get(&pair_owned)
                        .or_else(|| result.as_object().and_then(|m| m.values().next()))?;
                    let price_str = ticker.get("c")?.as_array()?.first()?.as_str()?;
                    price_str.parse::<f64>().ok()
                }),
            )
        }
        ExchangeRateSourceType::CoinGecko => {
            let coin_id = coingecko_coin_id(base)
                .ok_or_else(|| format!("coingecko: unsupported base asset '{base}'"))?;
            let vs = quote_currency.to_ascii_lowercase();
            let url = format!(
                "{}/simple/price?ids={coin_id}&vs_currencies={vs}",
                source.endpoint
            );
            let coin_id_owned = coin_id.to_string();
            let vs_owned = vs.clone();
            (
                url,
                Box::new(move |v: &serde_json::Value| {
                    v.get(&coin_id_owned)?.get(&vs_owned)?.as_f64()
                }),
            )
        }
        ExchangeRateSourceType::Generic => {
            let sep = if source.endpoint.contains('?') {
                '&'
            } else {
                '?'
            };
            let url = format!(
                "{}{sep}base={}&quote={}",
                source.endpoint,
                base.to_ascii_uppercase(),
                quote_currency.to_ascii_uppercase()
            );
            (url, Box::new(extract_price_generic))
        }
    };

    let mut req = client.get(&url);
    if let Some(key) = &source.api_key_secret {
        req = req.header("Authorization", format!("Bearer {key}"));
        req = req.header("X-Api-Key", key);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("status {}", resp.status()));
    }

    let value: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("parse failed: {e}"))?;

    extract_fn(&value).ok_or_else(|| "could not extract price from response".to_string())
}

#[cfg(feature = "redb")]
fn encode<T: Serialize>(value: &T) -> Result<String, PayError> {
    serde_json::to_string(value)
        .map_err(|e| PayError::internal_error(format!("spend encode failed: {e}")))
}

#[cfg(feature = "redb")]
fn decode<T: DeserializeOwned>(encoded: &str) -> Result<T, PayError> {
    serde_json::from_str(encoded).map_err(|e| {
        let preview_len = encoded.len().min(48);
        let preview = &encoded[..preview_len];
        PayError::internal_error(format!(
            "spend decode failed (len={}, preview={}): {e}",
            encoded.len(),
            preview
        ))
    })
}

#[cfg(feature = "redb")]
fn prepend_err(prefix: &str, err: PayError) -> PayError {
    match err {
        PayError::InternalError { message, hint } => PayError::InternalError {
            message: format!("{prefix}: {message}"),
            hint,
        },
        other => other,
    }
}

fn generate_rule_identifier() -> Result<String, PayError> {
    let mut buf = [0u8; 4];
    getrandom::fill(&mut buf).map_err(|e| PayError::internal_error(format!("rng failed: {e}")))?;
    Ok(format!("r_{}", hex::encode(buf)))
}

fn validate_limit(
    rule: &SpendLimit,
    exchange_rate: Option<&ExchangeRateConfig>,
) -> Result<(), PayError> {
    if rule.window_s == 0 {
        return Err(PayError::invalid_amount(
            "limit rule has zero window_s".to_string(),
        ));
    }
    if rule.max_spend == 0 {
        return Err(PayError::invalid_amount(
            "limit rule has zero max_spend".to_string(),
        ));
    }

    match rule.scope {
        SpendScope::GlobalUsdCents => {
            if rule.network.is_some() || rule.wallet.is_some() {
                return Err(PayError::invalid_amount(
                    "scope=global-usd-cents cannot set network/wallet".to_string(),
                ));
            }
            if rule.token.is_some() {
                return Err(PayError::invalid_amount(
                    "scope=global-usd-cents cannot set token".to_string(),
                ));
            }
        }
        SpendScope::Network => {
            if rule.network.as_deref().unwrap_or("").trim().is_empty() {
                return Err(PayError::invalid_amount(
                    "scope=network requires network".to_string(),
                ));
            }
            if rule.wallet.is_some() {
                return Err(PayError::invalid_amount(
                    "scope=network cannot set wallet".to_string(),
                ));
            }
        }
        SpendScope::Wallet => {
            if rule.network.as_deref().unwrap_or("").trim().is_empty() {
                return Err(PayError::invalid_amount(
                    "scope=wallet requires network".to_string(),
                ));
            }
            if rule.wallet.as_deref().unwrap_or("").trim().is_empty() {
                return Err(PayError::invalid_amount(
                    "scope=wallet requires wallet".to_string(),
                ));
            }
        }
    }

    if rule.scope == SpendScope::GlobalUsdCents && exchange_rate.is_none() {
        return Err(PayError::invalid_amount(
            "scope=global-usd-cents requires config.exchange_rate".to_string(),
        ));
    }
    Ok(())
}

#[cfg(feature = "redb")]
fn load_rules(read_txn: &redb::ReadTransaction) -> Result<Vec<SpendLimit>, PayError> {
    let Ok(rule_table) = read_txn.open_table(RULE_BY_ID) else {
        return Ok(vec![]);
    };
    rule_table
        .iter()
        .map_err(|e| PayError::internal_error(format!("spend iterate rules: {e}")))?
        .map(|entry| {
            let (_k, v) = entry
                .map_err(|e| PayError::internal_error(format!("spend read rule entry: {e}")))?;
            decode::<SpendLimit>(v.value()).map_err(|e| prepend_err("spend decode rule", e))
        })
        .collect()
}

#[cfg(feature = "redb")]
fn load_reservations(read_txn: &redb::ReadTransaction) -> Result<Vec<SpendReservation>, PayError> {
    let Ok(table) = read_txn.open_table(RESERVATION_BY_ID) else {
        return Ok(vec![]);
    };
    table
        .iter()
        .map_err(|e| PayError::internal_error(format!("spend iterate reservations: {e}")))?
        .map(|entry| {
            let (_k, v) = entry
                .map_err(|e| PayError::internal_error(format!("spend read reservation: {e}")))?;
            decode::<SpendReservation>(v.value())
                .map_err(|e| prepend_err("spend decode reservation", e))
        })
        .collect()
}

#[cfg(feature = "redb")]
fn expire_pending(_table: &mut redb::Table<u64, &str>, _now_ms: u64) -> Result<(), PayError> {
    Ok(())
}

/// Drop idempotency records whose `expires_at_epoch_ms` has passed. Called at
/// the top of every `idempotency_claim` so the table size is bounded by the
/// 24h TTL window and an expired Pending slot can never block a retry.
#[cfg(feature = "redb")]
fn sweep_expired_idempotency_redb(
    table: &mut redb::Table<&str, &str>,
    now_ms: u64,
) -> Result<(), PayError> {
    let mut stale: Vec<String> = Vec::new();
    {
        let iter = table
            .iter()
            .map_err(|e| PayError::internal_error(format!("idem iterate: {e}")))?;
        for entry in iter {
            let (k, v) =
                entry.map_err(|e| PayError::internal_error(format!("idem iter entry: {e}")))?;
            let record: IdempotencyRecord = decode(v.value())?;
            if record.expires_at_epoch_ms <= now_ms {
                stale.push(k.value().to_string());
            }
        }
    }
    for key in stale {
        table
            .remove(key.as_str())
            .map_err(|e| PayError::internal_error(format!("idem sweep remove: {e}")))?;
    }
    Ok(())
}

fn amount_for_rule(
    _rule: &SpendLimit,
    amount_native: u64,
    amount_usd_cents: Option<u64>,
    use_usd: bool,
) -> Result<u64, PayError> {
    if use_usd {
        amount_usd_cents.ok_or_else(|| {
            PayError::internal_error("missing USD amount for non-native unit rule".to_string())
        })
    } else {
        Ok(amount_native)
    }
}

fn reservation_active_for_window(r: &SpendReservation, now_ms: u64) -> bool {
    match r.status {
        ReservationStatus::Confirmed => true,
        ReservationStatus::Pending => r.expires_at_epoch_ms > now_ms,
        ReservationStatus::Cancelled | ReservationStatus::Expired => false,
    }
}

fn rule_matches_context(
    rule: &SpendLimit,
    network: &str,
    wallet: Option<&str>,
    token: Option<&str>,
) -> bool {
    if let Some(rule_token) = &rule.token {
        let normalized_rule_token = canonical_spend_token(network, rule_token);
        match token.map(|ctx_token| canonical_spend_token(network, ctx_token)) {
            Some(ctx_token) if ctx_token == normalized_rule_token => {}
            _ => return false,
        }
    }
    match rule.scope {
        SpendScope::GlobalUsdCents => true,
        SpendScope::Network => rule.network.as_deref() == Some(network),
        SpendScope::Wallet => {
            rule.network.as_deref() == Some(network) && rule.wallet.as_deref() == wallet
        }
    }
}

fn scope_key(rule: &SpendLimit) -> String {
    match rule.scope {
        SpendScope::GlobalUsdCents => "global-usd-cents".to_string(),
        SpendScope::Network => rule.network.clone().unwrap_or_default(),
        SpendScope::Wallet => format!(
            "{}/{}",
            rule.network.clone().unwrap_or_default(),
            rule.wallet.clone().unwrap_or_default()
        ),
    }
}

fn spent_in_window(
    rule: &SpendLimit,
    reservations: &[SpendReservation],
    now_ms: u64,
    use_usd: bool,
) -> Result<(u64, Option<u64>), PayError> {
    let window_ms = rule.window_s.saturating_mul(1000);
    let cutoff = now_ms.saturating_sub(window_ms);

    let mut spent = 0u64;
    let mut oldest: Option<u64> = None;

    for r in reservations {
        if !reservation_active_for_window(r, now_ms) {
            continue;
        }
        if r.created_at_epoch_ms < cutoff {
            continue;
        }
        if !rule_matches_context(rule, &r.network, r.wallet.as_deref(), r.token.as_deref()) {
            continue;
        }

        let amount = if use_usd {
            r.amount_usd_cents.ok_or_else(|| {
                PayError::internal_error("reservation missing USD amount".to_string())
            })?
        } else {
            r.amount_native
        };
        spent = spent.saturating_add(amount);
        oldest = Some(oldest.map_or(r.created_at_epoch_ms, |v| v.min(r.created_at_epoch_ms)));
    }

    Ok((spent, oldest))
}

#[cfg(feature = "redb")]
fn next_counter(write_txn: &redb::WriteTransaction, key: &str) -> Result<u64, PayError> {
    let mut meta = write_txn
        .open_table(META_COUNTER)
        .map_err(|e| PayError::internal_error(format!("spend open meta table: {e}")))?;
    let current = match meta
        .get(key)
        .map_err(|e| PayError::internal_error(format!("spend read counter {key}: {e}")))?
    {
        Some(v) => v.value(),
        None => 0,
    };
    let next = current.saturating_add(1);
    meta.insert(key, next)
        .map_err(|e| PayError::internal_error(format!("spend write counter {key}: {e}")))?;
    Ok(next)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn a_payment_worth_something_never_costs_the_limit_nothing() {
        // A USD-priced six-decimal token: 4,999 base units is 0.4999 cents.
        // Rounding to nearest made that zero, so the same send repeated any
        // number of times never touched the global USD cap. Rounding up costs
        // a cent, which makes splitting strictly worse for the spender rather
        // than free.
        let tmp = tempfile::tempdir().unwrap();
        let ledger = SpendLedger::new(tmp.path().to_str().unwrap(), None);

        let sub_cent = ledger
            .amount_to_usd_cents("evm", Some("usdc"), 4_999)
            .await
            .expect("a USD-priced token converts without a quote");
        assert_eq!(sub_cent, 1, "a payment worth 0.4999 cents must cost a cent");

        // Zero really is zero, and a whole cent is still one cent.
        assert_eq!(
            ledger
                .amount_to_usd_cents("evm", Some("usdc"), 0)
                .await
                .expect("zero converts"),
            0
        );
        assert_eq!(
            ledger
                .amount_to_usd_cents("evm", Some("usdc"), 10_000)
                .await
                .expect("one cent converts"),
            1
        );

        // And splitting a payment can only ever cost more, never less.
        let whole = ledger
            .amount_to_usd_cents("evm", Some("usdc"), 100_000)
            .await
            .expect("converts");
        let mut split = 0u64;
        for _ in 0..10 {
            split += ledger
                .amount_to_usd_cents("evm", Some("usdc"), 10_000)
                .await
                .expect("converts");
        }
        assert!(
            split >= whole,
            "splitting must not undercount: {split} < {whole}"
        );
    }

    #[test]
    fn ttl_per_network_matches_spec() {
        // Spec from the agent-hardening audit:
        // Cashu 60s, LN 90s, SOL 120s, EVM 180s, BTC 30 min, fallback 5 min.
        assert_eq!(reservation_ttl_ms_for_network("cashu"), 60_000);
        assert_eq!(reservation_ttl_ms_for_network("ln"), 90_000);
        assert_eq!(reservation_ttl_ms_for_network("sol"), 120_000);
        assert_eq!(reservation_ttl_ms_for_network("evm"), 180_000);
        assert_eq!(reservation_ttl_ms_for_network("btc"), 30 * 60 * 1_000);
        assert_eq!(reservation_ttl_ms_for_network("unknown"), 5 * 60 * 1_000);
        // Case-insensitive matching so callers don't have to normalize.
        assert_eq!(reservation_ttl_ms_for_network("BTC"), 30 * 60 * 1_000);
    }

    fn make_limit(scope: SpendScope, network: Option<&str>, wallet: Option<&str>) -> SpendLimit {
        SpendLimit {
            rule_id: None,
            scope,
            network: network.map(|s| s.to_string()),
            wallet: wallet.map(|s| s.to_string()),
            window_s: 3600,
            max_spend: 1000,
            token: None,
        }
    }

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn provider_limit_reserve_and_confirm() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = SpendLedger::new(tmp.path().to_str().unwrap(), None);

        ledger
            .set_limits(&[make_limit(SpendScope::Network, Some("cashu"), None)])
            .await
            .unwrap();

        let ctx = SpendContext {
            network: "cashu".to_string(),
            wallet: Some("w_01".to_string()),
            amount_native: 400,
            token: None,
        };
        let r1 = ledger.reserve("op_1", &ctx).await.unwrap();
        ledger.confirm(r1).await.unwrap();

        let r2 = ledger.reserve("op_2", &ctx).await.unwrap();
        let err = ledger.reserve("op_3", &ctx).await.unwrap_err();
        assert!(matches!(err, PayError::LimitExceeded { .. }));

        ledger.cancel(r2).await.unwrap();
    }

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn duplicate_operation_id_is_rejected_after_confirm() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = SpendLedger::new(tmp.path().to_str().unwrap(), None);

        ledger
            .set_limits(&[make_limit(SpendScope::Network, Some("cashu"), None)])
            .await
            .unwrap();

        let ctx = SpendContext {
            network: "cashu".to_string(),
            wallet: Some("w_01".to_string()),
            amount_native: 100,
            token: None,
        };
        let rid = ledger.reserve("op_duplicate", &ctx).await.unwrap();
        ledger.confirm(rid).await.unwrap();

        let err = ledger.reserve("op_duplicate", &ctx).await.unwrap_err();
        assert!(matches!(err, PayError::InvalidAmount { .. }));
        assert!(err.to_string().contains("refusing to re-execute"));
    }

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn wallet_scope_requires_wallet_context() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = SpendLedger::new(tmp.path().to_str().unwrap(), None);

        ledger
            .set_limits(&[make_limit(SpendScope::Wallet, Some("cashu"), Some("w_abc"))])
            .await
            .unwrap();

        let ctx = SpendContext {
            network: "cashu".to_string(),
            wallet: None,
            amount_native: 1,
            token: None,
        };
        let err = ledger.reserve("op_1", &ctx).await.unwrap_err();
        assert!(matches!(err, PayError::InvalidAmount { .. }));
    }

    #[tokio::test]
    async fn global_usd_cents_scope_requires_exchange_rate_config() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = SpendLedger::new(tmp.path().to_str().unwrap(), None);

        let err = ledger
            .set_limits(&[SpendLimit {
                rule_id: None,
                scope: SpendScope::GlobalUsdCents,
                network: None,
                wallet: None,
                window_s: 3600,
                max_spend: 100,
                token: None,
            }])
            .await
            .unwrap_err();

        assert!(matches!(err, PayError::InvalidAmount { .. }));
    }

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn network_scope_native_token_ok_without_exchange_rate() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = SpendLedger::new(tmp.path().to_str().unwrap(), None);

        ledger
            .set_limits(&[SpendLimit {
                rule_id: None,
                scope: SpendScope::Network,
                network: Some("cashu".to_string()),
                wallet: None,
                window_s: 3600,
                max_spend: 100,
                token: None,
            }])
            .await
            .expect("network scope should not require exchange_rate");
    }

    #[test]
    fn native_sol_and_evm_assets_can_be_priced_for_global_usd_limits() {
        assert_eq!(token_asset("sol", Some("native")), Some(("SOL", 1e9)));
        assert_eq!(token_asset("sol", Some("lamports")), Some(("SOL", 1e9)));
        assert_eq!(token_asset("evm", Some("native")), Some(("ETH", 1e18)));
        assert_eq!(token_asset("evm", Some("wei")), Some(("ETH", 1e18)));
    }

    #[test]
    fn sol_and_evm_limits_without_token_normalize_to_native() {
        let mut sol_limit = make_limit(SpendScope::Network, Some("SOL"), None);
        normalize_limit(&mut sol_limit);
        assert_eq!(sol_limit.network.as_deref(), Some("sol"));
        assert_eq!(sol_limit.token.as_deref(), Some("native"));

        let mut evm_limit = make_limit(SpendScope::Wallet, Some("evm"), Some("w_evm"));
        normalize_limit(&mut evm_limit);
        assert_eq!(evm_limit.token.as_deref(), Some("native"));
    }

    // ─── idempotency ────────────────────────────────────────────────────

    fn dummy_payload() -> IdempotentReplayPayload {
        IdempotentReplayPayload::Sent {
            wallet: "w_test".into(),
            transaction_id: "tx_abc".into(),
            amount: crate::types::Amount {
                value: 42,
                token: "sats".into(),
            },
            fee: None,
            preimage: None,
            reservation_ids: vec![],
        }
    }

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn idempotency_claim_fresh_then_replay_after_finalize() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = SpendLedger::new(tmp.path().to_str().unwrap(), None);

        let r = ledger.idempotency_claim("k1", "hash_a").await.unwrap();
        assert!(matches!(r, IdempotencyLookup::Fresh));

        // Same key+hash while Pending → InProgress.
        let r = ledger.idempotency_claim("k1", "hash_a").await.unwrap();
        assert!(matches!(r, IdempotencyLookup::InProgress));

        ledger
            .idempotency_finalize("k1", "hash_a", dummy_payload())
            .await
            .unwrap();

        // After finalize → Replay returned with the stored payload.
        let r = ledger.idempotency_claim("k1", "hash_a").await.unwrap();
        match r {
            IdempotencyLookup::Replay(IdempotentReplayPayload::Sent { transaction_id, .. }) => {
                assert_eq!(transaction_id, "tx_abc")
            }
            other => panic!("expected Replay::Sent, got {other:?}"),
        }
    }

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn idempotency_claim_conflict_when_hash_differs() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = SpendLedger::new(tmp.path().to_str().unwrap(), None);

        assert!(matches!(
            ledger.idempotency_claim("k2", "hash_a").await.unwrap(),
            IdempotencyLookup::Fresh
        ));
        let r = ledger.idempotency_claim("k2", "hash_b").await.unwrap();
        assert!(matches!(r, IdempotencyLookup::Conflict));
    }

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn idempotency_clear_releases_pending_for_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = SpendLedger::new(tmp.path().to_str().unwrap(), None);

        assert!(matches!(
            ledger.idempotency_claim("k3", "hash_a").await.unwrap(),
            IdempotencyLookup::Fresh
        ));
        ledger.idempotency_clear("k3", "hash_a").await.unwrap();
        // Cleared → next claim is Fresh again.
        assert!(matches!(
            ledger.idempotency_claim("k3", "hash_a").await.unwrap(),
            IdempotencyLookup::Fresh
        ));
    }

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn idempotency_clear_refuses_to_drop_final_records() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = SpendLedger::new(tmp.path().to_str().unwrap(), None);

        assert!(matches!(
            ledger.idempotency_claim("k4", "hash_a").await.unwrap(),
            IdempotencyLookup::Fresh
        ));
        ledger
            .idempotency_finalize("k4", "hash_a", dummy_payload())
            .await
            .unwrap();
        // Clear is a no-op on Final entries (only Pending can be released).
        ledger.idempotency_clear("k4", "hash_a").await.unwrap();
        let r = ledger.idempotency_claim("k4", "hash_a").await.unwrap();
        assert!(matches!(r, IdempotencyLookup::Replay(_)));
    }

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn idempotency_finalize_with_wrong_hash_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = SpendLedger::new(tmp.path().to_str().unwrap(), None);

        ledger.idempotency_claim("k5", "hash_a").await.unwrap();
        let err = ledger
            .idempotency_finalize("k5", "hash_b", dummy_payload())
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("idempotency_finalize"));
    }

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn idempotency_key_length_is_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = SpendLedger::new(tmp.path().to_str().unwrap(), None);
        let too_long: String = "a".repeat(IDEMPOTENCY_KEY_MAX_LEN + 1);
        let err = ledger.idempotency_claim(&too_long, "x").await.unwrap_err();
        assert!(matches!(err, PayError::InvalidAmount { .. }));
    }

    // ─── reconcile ──────────────────────────────────────────────────────

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn force_confirm_promotes_pending_and_writes_event() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = SpendLedger::new(tmp.path().to_str().unwrap(), None);

        ledger
            .set_limits(&[make_limit(SpendScope::Network, Some("cashu"), None)])
            .await
            .unwrap();
        let ctx = SpendContext {
            network: "cashu".to_string(),
            wallet: Some("w_01".to_string()),
            amount_native: 100,
            token: None,
        };
        let rid = ledger.reserve("op_x", &ctx).await.unwrap();

        let outcome = ledger.force_confirm(rid, "manual fix").await.unwrap();
        match outcome {
            ReconcileOutcome::Reconciled {
                previous_status,
                new_status,
            } => {
                assert_eq!(previous_status, "pending");
                assert_eq!(new_status, "confirmed");
            }
            other => panic!("expected Reconciled, got {other:?}"),
        }

        // After reconcile, status reflects the spend: 100/1000 confirmed.
        let status = ledger.get_status().await.unwrap();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].spent, 100);
    }

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn force_confirm_refuses_terminal_state() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = SpendLedger::new(tmp.path().to_str().unwrap(), None);

        ledger
            .set_limits(&[make_limit(SpendScope::Network, Some("cashu"), None)])
            .await
            .unwrap();
        let ctx = SpendContext {
            network: "cashu".to_string(),
            wallet: Some("w_01".to_string()),
            amount_native: 50,
            token: None,
        };
        let rid = ledger.reserve("op_y", &ctx).await.unwrap();
        ledger.confirm(rid).await.unwrap();

        let outcome = ledger.force_confirm(rid, "redo").await.unwrap();
        assert!(matches!(
            outcome,
            ReconcileOutcome::AlreadyTerminal {
                current_status: "confirmed"
            }
        ));
    }

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn force_cancel_releases_pending_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = SpendLedger::new(tmp.path().to_str().unwrap(), None);

        ledger
            .set_limits(&[make_limit(SpendScope::Network, Some("cashu"), None)])
            .await
            .unwrap();
        let ctx = SpendContext {
            network: "cashu".to_string(),
            wallet: Some("w_01".to_string()),
            amount_native: 700,
            token: None,
        };
        let rid = ledger.reserve("op_z", &ctx).await.unwrap();
        let outcome = ledger.force_cancel(rid, "never sent").await.unwrap();
        assert!(matches!(
            outcome,
            ReconcileOutcome::Reconciled {
                previous_status: "pending",
                new_status: "cancelled"
            }
        ));
        let status = ledger.get_status().await.unwrap();
        assert_eq!(status[0].spent, 0);
    }

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn force_confirm_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = SpendLedger::new(tmp.path().to_str().unwrap(), None);
        let outcome = ledger.force_confirm(9999, "nothing").await.unwrap();
        assert!(matches!(outcome, ReconcileOutcome::NotFound));
    }

    #[test]
    fn token_limit_does_not_match_native_gas_debit() {
        let mut sol_usdc = make_limit(SpendScope::Network, Some("sol"), None);
        sol_usdc.token = Some("usdc".to_string());
        normalize_limit(&mut sol_usdc);
        assert!(rule_matches_context(&sol_usdc, "sol", None, Some("usdc")));
        assert!(!rule_matches_context(
            &sol_usdc,
            "sol",
            None,
            Some("native")
        ));

        let mut evm_usdc = make_limit(SpendScope::Network, Some("evm"), None);
        evm_usdc.token = Some("usdc".to_string());
        normalize_limit(&mut evm_usdc);
        assert!(rule_matches_context(&evm_usdc, "evm", None, Some("usdc")));
        assert!(!rule_matches_context(
            &evm_usdc,
            "evm",
            None,
            Some("native")
        ));
    }
}
