use crate::provider::PayError;
use crate::store::wallet::{self, WalletMetadata};
use crate::store::{MigrationLog, PayStore};
use crate::types::{HistoryRecord, Network};
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::path::PathBuf;

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS wallets (
    id TEXT PRIMARY KEY,
    network TEXT NOT NULL,
    metadata JSONB NOT NULL,
    created_at_epoch_s BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_wallets_network ON wallets(network);

CREATE TABLE IF NOT EXISTS transactions (
    sequence BIGSERIAL PRIMARY KEY,
    transaction_id TEXT NOT NULL UNIQUE,
    wallet TEXT NOT NULL,
    record JSONB NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_transactions_wallet ON transactions(wallet);

CREATE TABLE IF NOT EXISTS spend_rules (
    rule_id TEXT PRIMARY KEY,
    rule JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS spend_reservations (
    reservation_id BIGSERIAL PRIMARY KEY,
    op_id TEXT NOT NULL UNIQUE,
    reservation JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS spend_events (
    event_id BIGSERIAL PRIMARY KEY,
    reservation_id BIGINT NOT NULL,
    event JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS exchange_rate_cache (
    pair TEXT PRIMARY KEY,
    quote JSONB NOT NULL
);
"#;

/// Advisory lock key for spend operations (prevents concurrent spend check-then-write).
/// Hex of "afpay\0" = 0x616670617900.
pub const SPEND_ADVISORY_LOCK_KEY: i64 = 0x0061_6670_6179;

/// PostgreSQL-backed storage.
#[derive(Clone)]
pub struct PostgresStore {
    pool: PgPool,
    data_dir: String,
}

impl PostgresStore {
    /// Get a reference to the connection pool (used by spend module).
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn connect(database_url: &str, data_dir: &str) -> Result<Self, PayError> {
        let pool = PgPool::connect(database_url)
            .await
            .map_err(|e| PayError::InternalError(format!("postgres connect: {e}")))?;

        sqlx::raw_sql(SCHEMA_SQL)
            .execute(&pool)
            .await
            .map_err(|e| PayError::InternalError(format!("postgres schema init: {e}")))?;

        Ok(Self {
            pool,
            data_dir: data_dir.to_string(),
        })
    }
}

impl PayStore for PostgresStore {
    fn save_wallet_metadata(&self, meta: &WalletMetadata) -> Result<(), PayError> {
        let pool = self.pool.clone();
        let meta_json = serde_json::to_value(meta)
            .map_err(|e| PayError::InternalError(format!("serialize wallet metadata: {e}")))?;
        let network_str = meta.network.to_string();
        let id = meta.id.clone();
        let created = meta.created_at_epoch_s as i64;

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                sqlx::query(
                    "INSERT INTO wallets (id, network, metadata, created_at_epoch_s) \
                     VALUES ($1, $2, $3, $4) \
                     ON CONFLICT (id) DO UPDATE SET metadata = $3",
                )
                .bind(&id)
                .bind(&network_str)
                .bind(&meta_json)
                .bind(created)
                .execute(&pool)
                .await
                .map_err(|e| PayError::InternalError(format!("postgres save wallet: {e}")))?;
                Ok(())
            })
        })
    }

    fn load_wallet_metadata(&self, wallet_id: &str) -> Result<WalletMetadata, PayError> {
        let pool = self.pool.clone();
        let wallet_id = wallet_id.to_string();

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let row: Option<(serde_json::Value,)> =
                    sqlx::query_as("SELECT metadata FROM wallets WHERE id = $1")
                        .bind(&wallet_id)
                        .fetch_optional(&pool)
                        .await
                        .map_err(|e| {
                            PayError::InternalError(format!("postgres load wallet: {e}"))
                        })?;

                match row {
                    Some((meta_json,)) => serde_json::from_value(meta_json).map_err(|e| {
                        PayError::InternalError(format!("postgres parse wallet metadata: {e}"))
                    }),
                    None => {
                        // Label fallback
                        if !wallet_id.starts_with("w_") {
                            let row: Option<(serde_json::Value,)> = sqlx::query_as(
                                "SELECT metadata FROM wallets WHERE metadata->>'label' = $1",
                            )
                            .bind(&wallet_id)
                            .fetch_optional(&pool)
                            .await
                            .map_err(|e| {
                                PayError::InternalError(format!(
                                    "postgres load wallet by label: {e}"
                                ))
                            })?;
                            if let Some((meta_json,)) = row {
                                return serde_json::from_value(meta_json).map_err(|e| {
                                    PayError::InternalError(format!(
                                        "postgres parse wallet metadata: {e}"
                                    ))
                                });
                            }
                        }
                        Err(PayError::WalletNotFound(format!(
                            "wallet {wallet_id} not found"
                        )))
                    }
                }
            })
        })
    }

    fn list_wallet_metadata(
        &self,
        network: Option<Network>,
    ) -> Result<Vec<WalletMetadata>, PayError> {
        let pool = self.pool.clone();

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let rows: Vec<(serde_json::Value,)> = match network {
                    Some(n) => {
                        sqlx::query_as(
                            "SELECT metadata FROM wallets WHERE network = $1 ORDER BY id",
                        )
                        .bind(n.to_string())
                        .fetch_all(&pool)
                        .await
                    }
                    None => {
                        sqlx::query_as("SELECT metadata FROM wallets ORDER BY id")
                            .fetch_all(&pool)
                            .await
                    }
                }
                .map_err(|e| PayError::InternalError(format!("postgres list wallets: {e}")))?;

                rows.into_iter()
                    .map(|(meta_json,)| {
                        serde_json::from_value(meta_json).map_err(|e| {
                            PayError::InternalError(format!("postgres parse wallet metadata: {e}"))
                        })
                    })
                    .collect()
            })
        })
    }

    fn delete_wallet_metadata(&self, wallet_id: &str) -> Result<(), PayError> {
        let pool = self.pool.clone();
        let wallet_id = wallet_id.to_string();

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let result = sqlx::query("DELETE FROM wallets WHERE id = $1")
                    .bind(&wallet_id)
                    .execute(&pool)
                    .await
                    .map_err(|e| PayError::InternalError(format!("postgres delete wallet: {e}")))?;

                if result.rows_affected() == 0 {
                    return Err(PayError::WalletNotFound(format!(
                        "wallet {wallet_id} not found"
                    )));
                }
                Ok(())
            })
        })
    }

    fn wallet_directory_path(&self, wallet_id: &str) -> Result<PathBuf, PayError> {
        wallet::wallet_directory_path(&self.data_dir, wallet_id)
    }

    fn wallet_data_directory_path(&self, wallet_id: &str) -> Result<PathBuf, PayError> {
        wallet::wallet_data_directory_path(&self.data_dir, wallet_id)
    }

    fn wallet_data_directory_path_for_meta(&self, meta: &WalletMetadata) -> PathBuf {
        wallet::wallet_data_directory_path_for_wallet_metadata(&self.data_dir, meta)
    }

    fn resolve_wallet_id(&self, id_or_label: &str) -> Result<String, PayError> {
        if id_or_label.starts_with("w_") {
            return Ok(id_or_label.to_string());
        }
        let all = self.list_wallet_metadata(None)?;
        let mut matches: Vec<&WalletMetadata> = all
            .iter()
            .filter(|w| w.label.as_deref() == Some(id_or_label))
            .collect();
        match matches.len() {
            0 => Err(PayError::WalletNotFound(format!(
                "no wallet found with ID or label '{id_or_label}'"
            ))),
            1 => Ok(matches.remove(0).id.clone()),
            n => Err(PayError::InvalidAmount(format!(
                "label '{id_or_label}' matches {n} wallets — use wallet ID instead"
            ))),
        }
    }

    fn append_transaction_record(&self, record: &HistoryRecord) -> Result<(), PayError> {
        let pool = self.pool.clone();
        let record_json = serde_json::to_value(record)
            .map_err(|e| PayError::InternalError(format!("serialize transaction record: {e}")))?;
        let tx_id = record.transaction_id.clone();
        let wallet = record.wallet.clone();

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                sqlx::query(
                    "INSERT INTO transactions (transaction_id, wallet, record) \
                     VALUES ($1, $2, $3) \
                     ON CONFLICT (transaction_id) DO NOTHING",
                )
                .bind(&tx_id)
                .bind(&wallet)
                .bind(&record_json)
                .execute(&pool)
                .await
                .map_err(|e| {
                    PayError::InternalError(format!("postgres append transaction: {e}"))
                })?;
                Ok(())
            })
        })
    }

    fn load_wallet_transaction_records(
        &self,
        wallet_id: &str,
    ) -> Result<Vec<HistoryRecord>, PayError> {
        let pool = self.pool.clone();
        let wallet_id = wallet_id.to_string();

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let rows: Vec<(serde_json::Value,)> = sqlx::query_as(
                    "SELECT record FROM transactions WHERE wallet = $1 ORDER BY sequence",
                )
                .bind(&wallet_id)
                .fetch_all(&pool)
                .await
                .map_err(|e| PayError::InternalError(format!("postgres load transactions: {e}")))?;

                rows.into_iter()
                    .map(|(record_json,)| {
                        serde_json::from_value(record_json).map_err(|e| {
                            PayError::InternalError(format!(
                                "postgres parse transaction record: {e}"
                            ))
                        })
                    })
                    .collect()
            })
        })
    }

    fn find_transaction_record_by_id(
        &self,
        tx_id: &str,
    ) -> Result<Option<HistoryRecord>, PayError> {
        let pool = self.pool.clone();
        let tx_id = tx_id.to_string();

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let row: Option<(serde_json::Value,)> =
                    sqlx::query_as("SELECT record FROM transactions WHERE transaction_id = $1")
                        .bind(&tx_id)
                        .fetch_optional(&pool)
                        .await
                        .map_err(|e| {
                            PayError::InternalError(format!("postgres find transaction: {e}"))
                        })?;

                match row {
                    Some((record_json,)) => {
                        let record: HistoryRecord =
                            serde_json::from_value(record_json).map_err(|e| {
                                PayError::InternalError(format!(
                                    "postgres parse transaction record: {e}"
                                ))
                            })?;
                        Ok(Some(record))
                    }
                    None => Ok(None),
                }
            })
        })
    }

    fn update_transaction_record_memo(
        &self,
        tx_id: &str,
        memo: Option<&BTreeMap<String, String>>,
    ) -> Result<(), PayError> {
        let pool = self.pool.clone();
        let tx_id = tx_id.to_string();
        let memo_json = serde_json::to_value(memo)
            .map_err(|e| PayError::InternalError(format!("serialize memo: {e}")))?;

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                // Read existing record, update memo, write back
                let row: Option<(serde_json::Value,)> =
                    sqlx::query_as("SELECT record FROM transactions WHERE transaction_id = $1")
                        .bind(&tx_id)
                        .fetch_optional(&pool)
                        .await
                        .map_err(|e| {
                            PayError::InternalError(format!("postgres read transaction: {e}"))
                        })?;

                let Some((record_json,)) = row else {
                    return Err(PayError::WalletNotFound(format!(
                        "transaction {tx_id} not found"
                    )));
                };

                let mut record: HistoryRecord = serde_json::from_value(record_json)
                    .map_err(|e| PayError::InternalError(format!("postgres parse record: {e}")))?;
                record.local_memo = serde_json::from_value(memo_json)
                    .map_err(|e| PayError::InternalError(format!("postgres parse memo: {e}")))?;
                let updated_json = serde_json::to_value(&record).map_err(|e| {
                    PayError::InternalError(format!("serialize updated record: {e}"))
                })?;

                sqlx::query("UPDATE transactions SET record = $1 WHERE transaction_id = $2")
                    .bind(&updated_json)
                    .bind(&tx_id)
                    .execute(&pool)
                    .await
                    .map_err(|e| {
                        PayError::InternalError(format!("postgres update transaction memo: {e}"))
                    })?;
                Ok(())
            })
        })
    }

    fn update_transaction_record_fee(
        &self,
        tx_id: &str,
        fee_value: u64,
        fee_unit: &str,
    ) -> Result<(), PayError> {
        let pool = self.pool.clone();
        let tx_id = tx_id.to_string();
        let fee_unit = fee_unit.to_string();

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let row: Option<(serde_json::Value,)> =
                    sqlx::query_as("SELECT record FROM transactions WHERE transaction_id = $1")
                        .bind(&tx_id)
                        .fetch_optional(&pool)
                        .await
                        .map_err(|e| {
                            PayError::InternalError(format!("postgres read transaction: {e}"))
                        })?;

                let Some((record_json,)) = row else {
                    return Err(PayError::WalletNotFound(format!(
                        "transaction {tx_id} not found"
                    )));
                };

                let mut record: HistoryRecord = serde_json::from_value(record_json)
                    .map_err(|e| PayError::InternalError(format!("postgres parse record: {e}")))?;
                record.fee = Some(crate::types::Amount {
                    value: fee_value,
                    token: fee_unit,
                });
                let updated_json = serde_json::to_value(&record).map_err(|e| {
                    PayError::InternalError(format!("serialize updated record: {e}"))
                })?;

                sqlx::query("UPDATE transactions SET record = $1 WHERE transaction_id = $2")
                    .bind(&updated_json)
                    .bind(&tx_id)
                    .execute(&pool)
                    .await
                    .map_err(|e| {
                        PayError::InternalError(format!("postgres update transaction fee: {e}"))
                    })?;
                Ok(())
            })
        })
    }

    fn drain_migration_log(&self) -> Vec<MigrationLog> {
        Vec::new()
    }
}
