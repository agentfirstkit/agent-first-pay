use crate::provider::PayError;
use crate::store::db;
use crate::store::transaction;
use crate::store::wallet;
use crate::store::{MigrationLog, PayStore};
use crate::types::{HistoryRecord, Network};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Redb-backed storage. Stores wallet metadata and transaction history
/// in per-wallet redb files under `data_dir`.
#[derive(Clone)]
pub struct RedbStore {
    data_dir: String,
}

impl RedbStore {
    pub fn new(data_dir: &str) -> Self {
        Self {
            data_dir: data_dir.to_string(),
        }
    }

    #[allow(dead_code)]
    pub fn data_dir(&self) -> &str {
        &self.data_dir
    }
}

impl PayStore for RedbStore {
    fn save_wallet_metadata(&self, meta: &wallet::WalletMetadata) -> Result<(), PayError> {
        wallet::save_wallet_metadata(&self.data_dir, meta)
    }

    fn load_wallet_metadata(&self, wallet_id: &str) -> Result<wallet::WalletMetadata, PayError> {
        wallet::load_wallet_metadata(&self.data_dir, wallet_id)
    }

    fn list_wallet_metadata(
        &self,
        network: Option<Network>,
    ) -> Result<Vec<wallet::WalletMetadata>, PayError> {
        wallet::list_wallet_metadata(&self.data_dir, network)
    }

    fn delete_wallet_metadata(&self, wallet_id: &str) -> Result<(), PayError> {
        wallet::delete_wallet_metadata(&self.data_dir, wallet_id)
    }

    fn wallet_directory_path(&self, wallet_id: &str) -> Result<PathBuf, PayError> {
        wallet::wallet_directory_path(&self.data_dir, wallet_id)
    }

    fn wallet_data_directory_path(&self, wallet_id: &str) -> Result<PathBuf, PayError> {
        wallet::wallet_data_directory_path(&self.data_dir, wallet_id)
    }

    fn wallet_data_directory_path_for_meta(&self, meta: &wallet::WalletMetadata) -> PathBuf {
        wallet::wallet_data_directory_path_for_wallet_metadata(&self.data_dir, meta)
    }

    fn resolve_wallet_id(&self, id_or_label: &str) -> Result<String, PayError> {
        wallet::resolve_wallet_id(&self.data_dir, id_or_label)
    }

    fn append_transaction_record(&self, record: &HistoryRecord) -> Result<(), PayError> {
        transaction::append_transaction_record(&self.data_dir, record)
    }

    fn load_wallet_transaction_records(
        &self,
        wallet_id: &str,
    ) -> Result<Vec<HistoryRecord>, PayError> {
        transaction::load_wallet_transaction_records(&self.data_dir, wallet_id)
    }

    fn find_transaction_record_by_id(
        &self,
        tx_id: &str,
    ) -> Result<Option<HistoryRecord>, PayError> {
        transaction::find_transaction_record_by_id(&self.data_dir, tx_id)
    }

    fn update_transaction_record_memo(
        &self,
        tx_id: &str,
        memo: Option<&BTreeMap<String, String>>,
    ) -> Result<(), PayError> {
        transaction::update_transaction_record_memo(&self.data_dir, tx_id, memo)
    }

    fn update_transaction_record_fee(
        &self,
        tx_id: &str,
        fee_value: u64,
        fee_unit: &str,
    ) -> Result<(), PayError> {
        transaction::update_transaction_record_fee(&self.data_dir, tx_id, fee_value, fee_unit)
    }

    fn drain_migration_log(&self) -> Vec<MigrationLog> {
        db::drain_migration_log()
    }
}
