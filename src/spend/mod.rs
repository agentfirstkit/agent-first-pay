#![cfg_attr(not(any(feature = "redb", feature = "postgres")), allow(dead_code))]

pub mod tokens;

use crate::provider::PayError;
#[cfg(feature = "exchange-rate")]
use crate::types::ExchangeRateSourceType;
use crate::types::{ExchangeRateConfig, SpendLimit, SpendLimitStatus, SpendScope};
#[cfg(feature = "redb")]
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
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
const NEXT_RESERVATION_ID_KEY: &str = "next_reservation_id";
#[cfg(feature = "redb")]
const NEXT_EVENT_ID_KEY: &str = "next_event_id";
#[cfg(feature = "redb")]
const SPEND_VERSION: u64 = 1;
#[cfg(feature = "redb")]
const FX_CACHE_VERSION: u64 = 1;

#[derive(Debug, Clone)]
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
        validate_limit(limit, self.exchange_rate.as_ref())?;

        let _guard = self.mu.lock().await;

        match &self.backend {
            #[cfg(feature = "redb")]
            SpendBackend::Redb { .. } => self.add_limit_redb(limit),
            #[cfg(feature = "postgres")]
            SpendBackend::Postgres { .. } => self.add_limit_postgres(limit).await,
            SpendBackend::None => Err(PayError::NotImplemented(
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
            SpendBackend::None => Err(PayError::NotImplemented(
                "no storage backend for spend limits".to_string(),
            )),
        }
    }

    /// Replace all spend limits (used by config patch / pipe mode).
    pub async fn set_limits(&self, limits: &[SpendLimit]) -> Result<(), PayError> {
        for limit in limits {
            validate_limit(limit, self.exchange_rate.as_ref())?;
        }

        let _guard = self.mu.lock().await;

        match &self.backend {
            #[cfg(feature = "redb")]
            SpendBackend::Redb { .. } => self.set_limits_redb(limits),
            #[cfg(feature = "postgres")]
            SpendBackend::Postgres { .. } => self.set_limits_postgres(limits).await,
            SpendBackend::None => Err(PayError::NotImplemented(
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
            return Err(PayError::InvalidAmount("op_id cannot be empty".to_string()));
        }
        if ctx.network.trim().is_empty() {
            return Err(PayError::InvalidAmount(
                "network cannot be empty for spend check".to_string(),
            ));
        }

        let _guard = self.mu.lock().await;

        match &self.backend {
            #[cfg(feature = "redb")]
            SpendBackend::Redb { .. } => self.reserve_redb(op_id, ctx).await,
            #[cfg(feature = "postgres")]
            SpendBackend::Postgres { .. } => self.reserve_postgres(op_id, ctx).await,
            SpendBackend::None => Err(PayError::NotImplemented(
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
            SpendBackend::None => Err(PayError::NotImplemented(
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
            .map_err(|e| PayError::InternalError(format!("spend begin_write: {e}")))?;
        {
            let mut rule_table = write_txn
                .open_table(RULE_BY_ID)
                .map_err(|e| PayError::InternalError(format!("spend open rule table: {e}")))?;
            rule_table
                .insert(rule_id.as_str(), encoded.as_str())
                .map_err(|e| PayError::InternalError(format!("spend insert rule: {e}")))?;
        }
        write_txn
            .commit()
            .map_err(|e| PayError::InternalError(format!("spend commit add_limit: {e}")))?;
        Ok(rule_id)
    }

    fn remove_limit_redb(&self, rule_id: &str) -> Result<(), PayError> {
        let db = self.open_spend_db()?;
        let write_txn = db
            .begin_write()
            .map_err(|e| PayError::InternalError(format!("spend begin_write: {e}")))?;
        {
            let mut rule_table = write_txn
                .open_table(RULE_BY_ID)
                .map_err(|e| PayError::InternalError(format!("spend open rule table: {e}")))?;
            let existed = rule_table
                .remove(rule_id)
                .map_err(|e| PayError::InternalError(format!("spend remove rule: {e}")))?;
            if existed.is_none() {
                return Err(PayError::InvalidAmount(format!(
                    "rule_id '{rule_id}' not found"
                )));
            }
        }
        write_txn
            .commit()
            .map_err(|e| PayError::InternalError(format!("spend commit remove_limit: {e}")))
    }

    fn set_limits_redb(&self, limits: &[SpendLimit]) -> Result<(), PayError> {
        let db = self.open_spend_db()?;
        let write_txn = db
            .begin_write()
            .map_err(|e| PayError::InternalError(format!("spend begin_write: {e}")))?;
        {
            let mut rule_table = write_txn
                .open_table(RULE_BY_ID)
                .map_err(|e| PayError::InternalError(format!("spend open rule table: {e}")))?;
            // Clear existing rules
            let existing_ids = rule_table
                .iter()
                .map_err(|e| PayError::InternalError(format!("spend iterate rules: {e}")))?
                .map(|entry| {
                    entry
                        .map(|(k, _)| k.value().to_string())
                        .map_err(|e| PayError::InternalError(format!("spend read rule key: {e}")))
                })
                .collect::<Result<Vec<_>, _>>()?;
            for rid in existing_ids {
                rule_table
                    .remove(rid.as_str())
                    .map_err(|e| PayError::InternalError(format!("spend remove rule: {e}")))?;
            }

            // Insert new rules with generated IDs
            for limit in limits {
                let mut rule = limit.clone();
                let rid = generate_rule_identifier()?;
                rule.rule_id = Some(rid.clone());
                let encoded = encode(&rule)?;
                rule_table
                    .insert(rid.as_str(), encoded.as_str())
                    .map_err(|e| PayError::InternalError(format!("spend insert rule: {e}")))?;
            }
        }
        write_txn
            .commit()
            .map_err(|e| PayError::InternalError(format!("spend commit set_limits: {e}")))
    }

    fn get_status_redb(&self) -> Result<Vec<SpendLimitStatus>, PayError> {
        let db = self.open_spend_db()?;
        let read_txn = db
            .begin_read()
            .map_err(|e| PayError::InternalError(format!("spend begin_read: {e}")))?;
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

    async fn reserve_redb(&self, op_id: &str, ctx: &SpendContext) -> Result<u64, PayError> {
        let now = now_epoch_ms();
        let db = self.open_spend_db()?;

        let read_txn = db
            .begin_read()
            .map_err(|e| PayError::InternalError(format!("spend begin_read: {e}")))?;
        let rules = load_rules(&read_txn)?;

        if rules.iter().any(|r| {
            r.scope == SpendScope::Wallet
                && r.network.as_deref() == Some(ctx.network.as_str())
                && ctx.wallet.is_none()
        }) {
            return Err(PayError::InvalidAmount(
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
            .map_err(|e| PayError::InternalError(format!("spend begin_write: {e}")))?;

        let mut encoded_blobs: Vec<String> = Vec::new();
        let reservation_id = {
            let mut reservation_index =
                write_txn.open_table(RESERVATION_ID_BY_OP_ID).map_err(|e| {
                    PayError::InternalError(format!("spend open reservation op index: {e}"))
                })?;
            if let Some(existing) = reservation_index
                .get(op_id)
                .map_err(|e| PayError::InternalError(format!("spend read op index: {e}")))?
            {
                let existing_id = existing.value();
                return Ok(existing_id);
            }

            let mut reservation_table = write_txn.open_table(RESERVATION_BY_ID).map_err(|e| {
                PayError::InternalError(format!("spend open reservation table: {e}"))
            })?;

            expire_pending(&mut reservation_table, now)?;

            let reservations = reservation_table
                .iter()
                .map_err(|e| PayError::InternalError(format!("spend iterate reservations: {e}")))?
                .map(|entry| {
                    let (_k, v) = entry.map_err(|e| {
                        PayError::InternalError(format!("spend read reservation: {e}"))
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
                expires_at_epoch_ms: now.saturating_add(300_000),
                finalized_at_epoch_ms: None,
            };
            encoded_blobs.push(encode(&reservation)?);
            let encoded = encoded_blobs
                .last()
                .ok_or_else(|| PayError::InternalError("missing reservation blob".to_string()))?;
            reservation_table
                .insert(reservation_id, encoded.as_str())
                .map_err(|e| PayError::InternalError(format!("spend insert reservation: {e}")))?;
            reservation_index
                .insert(op_id, reservation_id)
                .map_err(|e| PayError::InternalError(format!("spend insert op index: {e}")))?;
            reservation_id
        };

        write_txn
            .commit()
            .map_err(|e| PayError::InternalError(format!("spend commit reserve: {e}")))?;
        Ok(reservation_id)
    }

    fn confirm_redb(&self, reservation_id: u64) -> Result<(), PayError> {
        let db = self.open_spend_db()?;
        let now = now_epoch_ms();

        let write_txn = db
            .begin_write()
            .map_err(|e| PayError::InternalError(format!("spend begin_write: {e}")))?;

        let mut encoded_blobs: Vec<String> = Vec::new();
        {
            let mut reservation_table = write_txn.open_table(RESERVATION_BY_ID).map_err(|e| {
                PayError::InternalError(format!("spend open reservation table: {e}"))
            })?;
            let Some(existing_bytes) = reservation_table
                .get(reservation_id)
                .map_err(|e| PayError::InternalError(format!("spend read reservation: {e}")))?
                .map(|g| g.value().to_string())
            else {
                return Err(PayError::InternalError(format!(
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
                .ok_or_else(|| PayError::InternalError("missing reservation blob".to_string()))?;
            reservation_table
                .insert(reservation_id, encoded.as_str())
                .map_err(|e| PayError::InternalError(format!("spend update reservation: {e}")))?;

            let mut events = write_txn
                .open_table(SPEND_EVENT_BY_ID)
                .map_err(|e| PayError::InternalError(format!("spend open event table: {e}")))?;
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
                .ok_or_else(|| PayError::InternalError("missing event blob".to_string()))?;
            events
                .insert(event_id, encoded_event.as_str())
                .map_err(|e| PayError::InternalError(format!("spend insert event: {e}")))?;
        }

        write_txn
            .commit()
            .map_err(|e| PayError::InternalError(format!("spend commit confirm: {e}")))
    }

    fn cancel_redb(&self, reservation_id: u64) -> Result<(), PayError> {
        let db = self.open_spend_db()?;
        let now = now_epoch_ms();

        let write_txn = db
            .begin_write()
            .map_err(|e| PayError::InternalError(format!("spend begin_write: {e}")))?;

        let mut encoded_blobs: Vec<String> = Vec::new();
        {
            let mut reservation_table = write_txn.open_table(RESERVATION_BY_ID).map_err(|e| {
                PayError::InternalError(format!("spend open reservation table: {e}"))
            })?;
            let existing = reservation_table
                .get(reservation_id)
                .map_err(|e| PayError::InternalError(format!("spend read reservation: {e}")))?;
            let existing_bytes = existing.map(|g| g.value().to_string());
            if let Some(existing_bytes) = existing_bytes {
                let mut reservation: SpendReservation = decode(&existing_bytes)?;
                if matches!(reservation.status, ReservationStatus::Pending) {
                    reservation.status = ReservationStatus::Cancelled;
                    reservation.finalized_at_epoch_ms = Some(now);
                    encoded_blobs.push(encode(&reservation)?);
                    let encoded = encoded_blobs.last().ok_or_else(|| {
                        PayError::InternalError("missing reservation blob".to_string())
                    })?;
                    reservation_table
                        .insert(reservation_id, encoded.as_str())
                        .map_err(|e| {
                            PayError::InternalError(format!("spend update reservation: {e}"))
                        })?;
                }
            }
        }

        write_txn
            .commit()
            .map_err(|e| PayError::InternalError(format!("spend commit cancel: {e}")))
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
            _ => Err(PayError::InternalError(
                "expected postgres spend backend".to_string(),
            )),
        }
    }

    async fn add_limit_postgres(&self, limit: &mut SpendLimit) -> Result<String, PayError> {
        let pool = self.pg_pool()?;
        let rule_id = generate_rule_identifier()?;
        limit.rule_id = Some(rule_id.clone());
        let rule_json = serde_json::to_value(limit)
            .map_err(|e| PayError::InternalError(format!("serialize spend rule: {e}")))?;

        sqlx::query("INSERT INTO spend_rules (rule_id, rule) VALUES ($1, $2)")
            .bind(&rule_id)
            .bind(&rule_json)
            .execute(pool)
            .await
            .map_err(|e| PayError::InternalError(format!("pg insert spend rule: {e}")))?;

        Ok(rule_id)
    }

    async fn remove_limit_postgres(&self, rule_id: &str) -> Result<(), PayError> {
        let pool = self.pg_pool()?;
        let result = sqlx::query("DELETE FROM spend_rules WHERE rule_id = $1")
            .bind(rule_id)
            .execute(pool)
            .await
            .map_err(|e| PayError::InternalError(format!("pg delete spend rule: {e}")))?;

        if result.rows_affected() == 0 {
            return Err(PayError::InvalidAmount(format!(
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
            .map_err(|e| PayError::InternalError(format!("pg begin tx: {e}")))?;

        sqlx::query("DELETE FROM spend_rules")
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::InternalError(format!("pg clear spend rules: {e}")))?;

        for limit in limits {
            let mut rule = limit.clone();
            let rid = generate_rule_identifier()?;
            rule.rule_id = Some(rid.clone());
            let rule_json = serde_json::to_value(&rule)
                .map_err(|e| PayError::InternalError(format!("serialize spend rule: {e}")))?;
            sqlx::query("INSERT INTO spend_rules (rule_id, rule) VALUES ($1, $2)")
                .bind(&rid)
                .bind(&rule_json)
                .execute(&mut *tx)
                .await
                .map_err(|e| PayError::InternalError(format!("pg insert spend rule: {e}")))?;
        }

        tx.commit()
            .await
            .map_err(|e| PayError::InternalError(format!("pg commit set_limits: {e}")))
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

    async fn reserve_postgres(&self, op_id: &str, ctx: &SpendContext) -> Result<u64, PayError> {
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
            return Err(PayError::InvalidAmount(
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
            .map_err(|e| PayError::InternalError(format!("pg begin tx: {e}")))?;

        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(SPEND_ADVISORY_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::InternalError(format!("pg advisory lock: {e}")))?;

        // Check for existing reservation with same op_id (idempotency)
        let existing: Option<(i64,)> =
            sqlx::query_as("SELECT reservation_id FROM spend_reservations WHERE op_id = $1")
                .bind(op_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| PayError::InternalError(format!("pg check op_id: {e}")))?;

        if let Some((rid,)) = existing {
            return Ok(rid as u64);
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
            expires_at_epoch_ms: now.saturating_add(300_000),
            finalized_at_epoch_ms: None,
        };
        let reservation_json = serde_json::to_value(&reservation)
            .map_err(|e| PayError::InternalError(format!("serialize reservation: {e}")))?;

        let row: (i64,) = sqlx::query_as(
            "INSERT INTO spend_reservations (op_id, reservation) \
             VALUES ($1, $2) RETURNING reservation_id",
        )
        .bind(op_id)
        .bind(&reservation_json)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| PayError::InternalError(format!("pg insert reservation: {e}")))?;

        let reservation_id = row.0 as u64;

        // Update the reservation JSON with the assigned ID
        let mut updated_json = reservation_json;
        updated_json["reservation_id"] = serde_json::json!(reservation_id);
        sqlx::query("UPDATE spend_reservations SET reservation = $1 WHERE reservation_id = $2")
            .bind(&updated_json)
            .bind(row.0)
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::InternalError(format!("pg update reservation id: {e}")))?;

        tx.commit()
            .await
            .map_err(|e| PayError::InternalError(format!("pg commit reserve: {e}")))?;

        Ok(reservation_id)
    }

    async fn confirm_postgres(&self, reservation_id: u64) -> Result<(), PayError> {
        let pool = self.pg_pool()?;
        let now = now_epoch_ms();
        let rid = reservation_id as i64;

        let row: Option<(serde_json::Value,)> =
            sqlx::query_as("SELECT reservation FROM spend_reservations WHERE reservation_id = $1")
                .bind(rid)
                .fetch_optional(pool)
                .await
                .map_err(|e| PayError::InternalError(format!("pg read reservation: {e}")))?;

        let Some((res_json,)) = row else {
            return Err(PayError::InternalError(format!(
                "reservation {reservation_id} not found"
            )));
        };

        let mut reservation: SpendReservation = serde_json::from_value(res_json)
            .map_err(|e| PayError::InternalError(format!("pg parse reservation: {e}")))?;

        if !matches!(reservation.status, ReservationStatus::Pending) {
            return Ok(());
        }

        reservation.status = ReservationStatus::Confirmed;
        reservation.finalized_at_epoch_ms = Some(now);
        let updated_json = serde_json::to_value(&reservation)
            .map_err(|e| PayError::InternalError(format!("serialize reservation: {e}")))?;

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
            .map_err(|e| PayError::InternalError(format!("serialize spend event: {e}")))?;

        let mut tx = pool
            .begin()
            .await
            .map_err(|e| PayError::InternalError(format!("pg begin tx: {e}")))?;

        sqlx::query("UPDATE spend_reservations SET reservation = $1 WHERE reservation_id = $2")
            .bind(&updated_json)
            .bind(rid)
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::InternalError(format!("pg update reservation: {e}")))?;

        sqlx::query("INSERT INTO spend_events (reservation_id, event) VALUES ($1, $2)")
            .bind(rid)
            .bind(&event_json)
            .execute(&mut *tx)
            .await
            .map_err(|e| PayError::InternalError(format!("pg insert spend event: {e}")))?;

        tx.commit()
            .await
            .map_err(|e| PayError::InternalError(format!("pg commit confirm: {e}")))
    }

    async fn cancel_postgres(&self, reservation_id: u64) -> Result<(), PayError> {
        let pool = self.pg_pool()?;
        let now = now_epoch_ms();
        let rid = reservation_id as i64;

        let row: Option<(serde_json::Value,)> =
            sqlx::query_as("SELECT reservation FROM spend_reservations WHERE reservation_id = $1")
                .bind(rid)
                .fetch_optional(pool)
                .await
                .map_err(|e| PayError::InternalError(format!("pg read reservation: {e}")))?;

        if let Some((res_json,)) = row {
            let mut reservation: SpendReservation = serde_json::from_value(res_json)
                .map_err(|e| PayError::InternalError(format!("pg parse reservation: {e}")))?;

            if matches!(reservation.status, ReservationStatus::Pending) {
                reservation.status = ReservationStatus::Cancelled;
                reservation.finalized_at_epoch_ms = Some(now);
                let updated_json = serde_json::to_value(&reservation)
                    .map_err(|e| PayError::InternalError(format!("serialize reservation: {e}")))?;

                sqlx::query(
                    "UPDATE spend_reservations SET reservation = $1 WHERE reservation_id = $2",
                )
                .bind(&updated_json)
                .bind(rid)
                .execute(pool)
                .await
                .map_err(|e| PayError::InternalError(format!("pg update reservation: {e}")))?;
            }
        }

        Ok(())
    }
}

#[cfg(feature = "postgres")]
async fn pg_load_rules(pool: &sqlx::PgPool) -> Result<Vec<SpendLimit>, PayError> {
    let rows: Vec<(serde_json::Value,)> =
        sqlx::query_as("SELECT rule FROM spend_rules ORDER BY rule_id")
            .fetch_all(pool)
            .await
            .map_err(|e| PayError::InternalError(format!("pg load spend rules: {e}")))?;
    rows.into_iter()
        .map(|(v,)| {
            serde_json::from_value(v)
                .map_err(|e| PayError::InternalError(format!("pg parse spend rule: {e}")))
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
            .map_err(|e| PayError::InternalError(format!("pg load spend rules: {e}")))?;
    rows.into_iter()
        .map(|(v,)| {
            serde_json::from_value(v)
                .map_err(|e| PayError::InternalError(format!("pg parse spend rule: {e}")))
        })
        .collect()
}

#[cfg(feature = "postgres")]
async fn pg_load_reservations(pool: &sqlx::PgPool) -> Result<Vec<SpendReservation>, PayError> {
    let rows: Vec<(serde_json::Value,)> =
        sqlx::query_as("SELECT reservation FROM spend_reservations ORDER BY reservation_id")
            .fetch_all(pool)
            .await
            .map_err(|e| PayError::InternalError(format!("pg load reservations: {e}")))?;
    rows.into_iter()
        .map(|(v,)| {
            serde_json::from_value(v)
                .map_err(|e| PayError::InternalError(format!("pg parse reservation: {e}")))
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
            .map_err(|e| PayError::InternalError(format!("pg load reservations: {e}")))?;
    rows.into_iter()
        .map(|(v,)| {
            serde_json::from_value(v)
                .map_err(|e| PayError::InternalError(format!("pg parse reservation: {e}")))
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
                PayError::InternalError(format!("pg load reservations for expire: {e}"))
            })?;

    for (rid, res_json) in rows {
        let mut reservation: SpendReservation = serde_json::from_value(res_json)
            .map_err(|e| PayError::InternalError(format!("pg parse reservation: {e}")))?;
        if matches!(reservation.status, ReservationStatus::Pending)
            && reservation.expires_at_epoch_ms <= now_ms
        {
            reservation.status = ReservationStatus::Expired;
            reservation.finalized_at_epoch_ms = Some(now_ms);
            let updated = serde_json::to_value(&reservation)
                .map_err(|e| PayError::InternalError(format!("serialize reservation: {e}")))?;
            sqlx::query("UPDATE spend_reservations SET reservation = $1 WHERE reservation_id = $2")
                .bind(&updated)
                .bind(rid)
                .execute(&mut **tx)
                .await
                .map_err(|e| PayError::InternalError(format!("pg expire reservation: {e}")))?;
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
            PayError::InvalidAmount(format!(
                "network '{network}' token '{token:?}' is unsupported for global-usd-cents limits"
            ))
        })?;

        let quote = self.get_or_fetch_quote(symbol, "USD").await?;

        // Block if the quote has fully expired (fetch must have failed silently
        // in a prior call, or the clock jumped).
        let now = now_epoch_ms();
        if quote.expires_at_epoch_ms > 0 && now > quote.expires_at_epoch_ms {
            return Err(PayError::NetworkError(
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
            return Err(PayError::InternalError(
                "invalid exchange-rate conversion result".to_string(),
            ));
        }
        Ok((usd * 100f64).round() as u64)
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
                .map_err(|e| PayError::InternalError(format!("fx begin_read: {e}")))?;
            if let Ok(table) = read_txn.open_table(FX_QUOTE_BY_PAIR) {
                if let Some(entry) = table
                    .get(pair.as_str())
                    .map_err(|e| PayError::InternalError(format!("fx read quote: {e}")))?
                {
                    let cached: ExchangeRateQuote = decode(entry.value())?;
                    if cached.expires_at_epoch_ms > now {
                        return Ok(cached);
                    }
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
                    .map_err(|e| PayError::InternalError(format!("pg fx read cache: {e}")))?;
            if let Some((quote_json,)) = row {
                let cached: ExchangeRateQuote = serde_json::from_value(quote_json)
                    .map_err(|e| PayError::InternalError(format!("pg fx parse cache: {e}")))?;
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
                .map_err(|e| PayError::InternalError(format!("fx begin_write: {e}")))?;
            let mut encoded_blobs: Vec<String> = Vec::new();
            {
                let mut table = write_txn
                    .open_table(FX_QUOTE_BY_PAIR)
                    .map_err(|e| PayError::InternalError(format!("fx open quote table: {e}")))?;
                encoded_blobs.push(encode(&new_quote)?);
                let encoded = encoded_blobs
                    .last()
                    .ok_or_else(|| PayError::InternalError("missing quote blob".to_string()))?;
                table
                    .insert(pair.as_str(), encoded.as_str())
                    .map_err(|e| PayError::InternalError(format!("fx insert quote: {e}")))?;
            }
            write_txn
                .commit()
                .map_err(|e| PayError::InternalError(format!("fx commit write: {e}")))?;
        }

        // Write cache — postgres
        #[cfg(feature = "postgres")]
        if let SpendBackend::Postgres { pool } = &self.backend {
            let quote_json = serde_json::to_value(&new_quote)
                .map_err(|e| PayError::InternalError(format!("serialize fx quote: {e}")))?;
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
            return Err(PayError::InvalidAmount(
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

        Err(PayError::NetworkError(format!(
            "all exchange-rate sources failed; last: {last_err}"
        )))
    }

    #[cfg(not(feature = "exchange-rate"))]
    async fn fetch_exchange_rate_http(
        &self,
        _base: &str,
        _quote_currency: &str,
    ) -> Result<(f64, String), PayError> {
        Err(PayError::NotImplemented(
            "exchange-rate HTTP support is not built in this feature set".to_string(),
        ))
    }
}

// ═══════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn token_asset(network: &str, token: Option<&str>) -> Option<(&'static str, f64)> {
    match token.map(|t| t.to_ascii_lowercase()).as_deref() {
        Some("sol") => Some(("SOL", 1e9)),
        Some("eth") => Some(("ETH", 1e18)),
        Some("usdc" | "usdt") => Some(("USD", 1e6)),
        Some(_) => None,
        None => {
            let p = network.to_ascii_lowercase();
            if p.starts_with("ln") || p == "cashu" || p == "btc" {
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
    if let Some(key) = &source.api_key {
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
        .map_err(|e| PayError::InternalError(format!("spend encode failed: {e}")))
}

#[cfg(feature = "redb")]
fn decode<T: DeserializeOwned>(encoded: &str) -> Result<T, PayError> {
    serde_json::from_str(encoded).map_err(|e| {
        let preview_len = encoded.len().min(48);
        let preview = &encoded[..preview_len];
        PayError::InternalError(format!(
            "spend decode failed (len={}, preview={}): {e}",
            encoded.len(),
            preview
        ))
    })
}

#[cfg(feature = "redb")]
fn prepend_err(prefix: &str, err: PayError) -> PayError {
    match err {
        PayError::InternalError(msg) => PayError::InternalError(format!("{prefix}: {msg}")),
        other => other,
    }
}

fn generate_rule_identifier() -> Result<String, PayError> {
    let mut buf = [0u8; 4];
    getrandom::fill(&mut buf).map_err(|e| PayError::InternalError(format!("rng failed: {e}")))?;
    Ok(format!("r_{}", hex::encode(buf)))
}

fn validate_limit(
    rule: &SpendLimit,
    exchange_rate: Option<&ExchangeRateConfig>,
) -> Result<(), PayError> {
    if rule.window_s == 0 {
        return Err(PayError::InvalidAmount(
            "limit rule has zero window_s".to_string(),
        ));
    }
    if rule.max_spend == 0 {
        return Err(PayError::InvalidAmount(
            "limit rule has zero max_spend".to_string(),
        ));
    }

    match rule.scope {
        SpendScope::GlobalUsdCents => {
            if rule.network.is_some() || rule.wallet.is_some() {
                return Err(PayError::InvalidAmount(
                    "scope=global-usd-cents cannot set network/wallet".to_string(),
                ));
            }
            if rule.token.is_some() {
                return Err(PayError::InvalidAmount(
                    "scope=global-usd-cents cannot set token".to_string(),
                ));
            }
        }
        SpendScope::Network => {
            if rule.network.as_deref().unwrap_or("").trim().is_empty() {
                return Err(PayError::InvalidAmount(
                    "scope=network requires network".to_string(),
                ));
            }
            if rule.wallet.is_some() {
                return Err(PayError::InvalidAmount(
                    "scope=network cannot set wallet".to_string(),
                ));
            }
        }
        SpendScope::Wallet => {
            if rule.network.as_deref().unwrap_or("").trim().is_empty() {
                return Err(PayError::InvalidAmount(
                    "scope=wallet requires network".to_string(),
                ));
            }
            if rule.wallet.as_deref().unwrap_or("").trim().is_empty() {
                return Err(PayError::InvalidAmount(
                    "scope=wallet requires wallet".to_string(),
                ));
            }
        }
    }

    if rule.scope == SpendScope::GlobalUsdCents && exchange_rate.is_none() {
        return Err(PayError::InvalidAmount(
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
        .map_err(|e| PayError::InternalError(format!("spend iterate rules: {e}")))?
        .map(|entry| {
            let (_k, v) = entry
                .map_err(|e| PayError::InternalError(format!("spend read rule entry: {e}")))?;
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
        .map_err(|e| PayError::InternalError(format!("spend iterate reservations: {e}")))?
        .map(|entry| {
            let (_k, v) = entry
                .map_err(|e| PayError::InternalError(format!("spend read reservation: {e}")))?;
            decode::<SpendReservation>(v.value())
                .map_err(|e| prepend_err("spend decode reservation", e))
        })
        .collect()
}

#[cfg(feature = "redb")]
fn expire_pending(_table: &mut redb::Table<u64, &str>, _now_ms: u64) -> Result<(), PayError> {
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
            PayError::InternalError("missing USD amount for non-native unit rule".to_string())
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
        match token {
            Some(ctx_token) if ctx_token.eq_ignore_ascii_case(rule_token) => {}
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
                PayError::InternalError("reservation missing USD amount".to_string())
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
        .map_err(|e| PayError::InternalError(format!("spend open meta table: {e}")))?;
    let current = match meta
        .get(key)
        .map_err(|e| PayError::InternalError(format!("spend read counter {key}: {e}")))?
    {
        Some(v) => v.value(),
        None => 0,
    };
    let next = current.saturating_add(1);
    meta.insert(key, next)
        .map_err(|e| PayError::InternalError(format!("spend write counter {key}: {e}")))?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(matches!(err, PayError::InvalidAmount(_)));
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

        assert!(matches!(err, PayError::InvalidAmount(_)));
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
}
