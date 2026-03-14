#[cfg(feature = "redb")]
pub mod lock;
pub mod wallet;

#[cfg(feature = "redb")]
pub mod db;
#[cfg(feature = "redb")]
pub mod redb_store;
#[cfg(feature = "redb")]
pub mod transaction;

#[cfg(feature = "postgres")]
pub mod postgres_store;

use crate::provider::PayError;
use crate::types::{HistoryRecord, Network};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use wallet::WalletMetadata;

#[derive(Debug, Clone, Serialize)]
pub struct MigrationLog {
    pub database: String,
    pub from_version: u64,
    pub to_version: u64,
}

/// Trait abstracting wallet + transaction storage operations.
#[allow(dead_code)]
pub trait PayStore: Send + Sync {
    // Wallet
    fn save_wallet_metadata(&self, meta: &WalletMetadata) -> Result<(), PayError>;
    fn load_wallet_metadata(&self, wallet_id: &str) -> Result<WalletMetadata, PayError>;
    fn list_wallet_metadata(
        &self,
        network: Option<Network>,
    ) -> Result<Vec<WalletMetadata>, PayError>;
    fn delete_wallet_metadata(&self, wallet_id: &str) -> Result<(), PayError>;
    fn wallet_directory_path(&self, wallet_id: &str) -> Result<PathBuf, PayError>;
    fn wallet_data_directory_path(&self, wallet_id: &str) -> Result<PathBuf, PayError>;
    fn wallet_data_directory_path_for_meta(&self, meta: &WalletMetadata) -> PathBuf;
    fn resolve_wallet_id(&self, id_or_label: &str) -> Result<String, PayError>;

    // Transaction
    fn append_transaction_record(&self, record: &HistoryRecord) -> Result<(), PayError>;
    fn load_wallet_transaction_records(
        &self,
        wallet_id: &str,
    ) -> Result<Vec<HistoryRecord>, PayError>;
    fn find_transaction_record_by_id(&self, tx_id: &str)
        -> Result<Option<HistoryRecord>, PayError>;
    fn update_transaction_record_memo(
        &self,
        tx_id: &str,
        memo: Option<&BTreeMap<String, String>>,
    ) -> Result<(), PayError>;
    fn update_transaction_record_fee(
        &self,
        tx_id: &str,
        fee_value: u64,
        fee_unit: &str,
    ) -> Result<(), PayError>;
    fn update_transaction_record_status(
        &self,
        tx_id: &str,
        status: crate::types::TxStatus,
        confirmed_at_epoch_s: Option<u64>,
    ) -> Result<(), PayError>;

    // Migration log
    fn drain_migration_log(&self) -> Vec<MigrationLog>;
}

/// Storage backend enum dispatching to the active variant.
#[derive(Clone)]
pub enum StorageBackend {
    #[cfg(feature = "redb")]
    Redb(redb_store::RedbStore),
    #[cfg(feature = "postgres")]
    Postgres(postgres_store::PostgresStore),
    /// Uninhabited variant ensuring the enum is valid when no backend features
    /// are enabled. Cannot be constructed at runtime.
    #[doc(hidden)]
    _None(std::convert::Infallible),
}

/// Dispatch a method call to the active storage backend variant.
macro_rules! dispatch_storage {
    ($self:expr, $method:ident $(, $arg:expr)*) => {
        match $self {
            #[cfg(feature = "redb")]
            Self::Redb(s) => s.$method($($arg),*),
            #[cfg(feature = "postgres")]
            Self::Postgres(s) => s.$method($($arg),*),
            Self::_None(n) => match *n {},
        }
    }
}

impl PayStore for StorageBackend {
    fn save_wallet_metadata(&self, meta: &WalletMetadata) -> Result<(), PayError> {
        dispatch_storage!(self, save_wallet_metadata, meta)
    }

    fn load_wallet_metadata(&self, wallet_id: &str) -> Result<WalletMetadata, PayError> {
        dispatch_storage!(self, load_wallet_metadata, wallet_id)
    }

    fn list_wallet_metadata(
        &self,
        network: Option<Network>,
    ) -> Result<Vec<WalletMetadata>, PayError> {
        dispatch_storage!(self, list_wallet_metadata, network)
    }

    fn delete_wallet_metadata(&self, wallet_id: &str) -> Result<(), PayError> {
        dispatch_storage!(self, delete_wallet_metadata, wallet_id)
    }

    fn wallet_directory_path(&self, wallet_id: &str) -> Result<PathBuf, PayError> {
        dispatch_storage!(self, wallet_directory_path, wallet_id)
    }

    fn wallet_data_directory_path(&self, wallet_id: &str) -> Result<PathBuf, PayError> {
        dispatch_storage!(self, wallet_data_directory_path, wallet_id)
    }

    fn wallet_data_directory_path_for_meta(&self, meta: &WalletMetadata) -> PathBuf {
        dispatch_storage!(self, wallet_data_directory_path_for_meta, meta)
    }

    fn resolve_wallet_id(&self, id_or_label: &str) -> Result<String, PayError> {
        dispatch_storage!(self, resolve_wallet_id, id_or_label)
    }

    fn append_transaction_record(&self, record: &HistoryRecord) -> Result<(), PayError> {
        dispatch_storage!(self, append_transaction_record, record)
    }

    fn load_wallet_transaction_records(
        &self,
        wallet_id: &str,
    ) -> Result<Vec<HistoryRecord>, PayError> {
        dispatch_storage!(self, load_wallet_transaction_records, wallet_id)
    }

    fn find_transaction_record_by_id(
        &self,
        tx_id: &str,
    ) -> Result<Option<HistoryRecord>, PayError> {
        dispatch_storage!(self, find_transaction_record_by_id, tx_id)
    }

    fn update_transaction_record_memo(
        &self,
        tx_id: &str,
        memo: Option<&BTreeMap<String, String>>,
    ) -> Result<(), PayError> {
        dispatch_storage!(self, update_transaction_record_memo, tx_id, memo)
    }

    fn update_transaction_record_fee(
        &self,
        tx_id: &str,
        fee_value: u64,
        fee_unit: &str,
    ) -> Result<(), PayError> {
        dispatch_storage!(
            self,
            update_transaction_record_fee,
            tx_id,
            fee_value,
            fee_unit
        )
    }

    fn update_transaction_record_status(
        &self,
        tx_id: &str,
        status: crate::types::TxStatus,
        confirmed_at_epoch_s: Option<u64>,
    ) -> Result<(), PayError> {
        dispatch_storage!(
            self,
            update_transaction_record_status,
            tx_id,
            status,
            confirmed_at_epoch_s
        )
    }

    fn drain_migration_log(&self) -> Vec<MigrationLog> {
        dispatch_storage!(self, drain_migration_log)
    }
}

/// Create a storage backend based on config and enabled features.
/// Returns None if no storage backend is available (frontend-only mode).
/// For postgres, performs async connection via `block_in_place`.
pub fn create_storage_backend(config: &crate::types::RuntimeConfig) -> Option<StorageBackend> {
    let requested = config.storage_backend.as_deref().unwrap_or("redb");

    match requested {
        #[cfg(feature = "redb")]
        "redb" => Some(StorageBackend::Redb(redb_store::RedbStore::new(
            &config.data_dir,
        ))),
        #[cfg(feature = "postgres")]
        "postgres" => {
            let url = config.postgres_url_secret.as_deref()?;
            let data_dir = config.data_dir.clone();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    match postgres_store::PostgresStore::connect(url, &data_dir).await {
                        Ok(store) => Some(StorageBackend::Postgres(store)),
                        Err(_) => None,
                    }
                })
            })
        }
        _ => None,
    }
}

/// Create a postgres storage backend asynchronously.
#[cfg(feature = "postgres")]
#[allow(dead_code)]
pub async fn create_postgres_backend(
    config: &crate::types::RuntimeConfig,
) -> Result<StorageBackend, String> {
    let url = config.postgres_url_secret.as_deref().ok_or_else(|| {
        "postgres_url_secret is required when storage_backend = postgres".to_string()
    })?;
    let store = postgres_store::PostgresStore::connect(url, &config.data_dir)
        .await
        .map_err(|e| format!("postgres connection failed: {e}"))?;
    Ok(StorageBackend::Postgres(store))
}
