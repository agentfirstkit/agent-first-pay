use crate::provider::PayError;
use crate::store::db;
use crate::store::wallet;
use crate::types::{HistoryRecord, TxStatus};
use redb::{ReadableDatabase, ReadableTable, TableDefinition};

const TRANSACTION_METADATA_COUNTER: TableDefinition<&str, u64> =
    TableDefinition::new("transaction_metadata_counter");
const TRANSACTION_RECORD_BY_SEQUENCE: TableDefinition<u64, &str> =
    TableDefinition::new("transaction_log_by_sequence");
const TRANSACTION_SEQUENCE_BY_TRANSACTION_ID: TableDefinition<&str, u64> =
    TableDefinition::new("transaction_log_sequence_by_transaction_id");
const NEXT_TRANSACTION_SEQUENCE_KEY: &str = "next_transaction_sequence";

pub fn append_transaction_record(data_dir: &str, record: &HistoryRecord) -> Result<(), PayError> {
    let core_path = wallet::wallet_core_database_path(data_dir, &record.wallet)?;
    let db = db::open_database(&core_path)?;
    let serialized_record = serde_json::to_string(record)
        .map_err(|e| PayError::InternalError(format!("serialize transaction record: {e}")))?;

    let write_txn = db
        .begin_write()
        .map_err(|e| PayError::InternalError(format!("transaction_log_store begin_write: {e}")))?;
    {
        let mut counter = write_txn
            .open_table(TRANSACTION_METADATA_COUNTER)
            .map_err(|e| {
                PayError::InternalError(format!("transaction_log_store open tx_meta_counter: {e}"))
            })?;
        let next_sequence = counter
            .get(NEXT_TRANSACTION_SEQUENCE_KEY)
            .map_err(|e| {
                PayError::InternalError(format!("transaction_log_store read counter: {e}"))
            })?
            .map(|v| v.value())
            .unwrap_or(0)
            .saturating_add(1);
        counter
            .insert(NEXT_TRANSACTION_SEQUENCE_KEY, next_sequence)
            .map_err(|e| {
                PayError::InternalError(format!("transaction_log_store update counter: {e}"))
            })?;

        let mut records_by_sequence = write_txn
            .open_table(TRANSACTION_RECORD_BY_SEQUENCE)
            .map_err(|e| {
                PayError::InternalError(format!(
                    "transaction_log_store open by-sequence table: {e}"
                ))
            })?;
        records_by_sequence
            .insert(next_sequence, serialized_record.as_str())
            .map_err(|e| {
                PayError::InternalError(format!("transaction_log_store append record: {e}"))
            })?;

        let mut sequence_by_transaction_id = write_txn
            .open_table(TRANSACTION_SEQUENCE_BY_TRANSACTION_ID)
            .map_err(|e| {
                PayError::InternalError(format!("transaction_log_store open by-id table: {e}"))
            })?;
        sequence_by_transaction_id
            .insert(record.transaction_id.as_str(), next_sequence)
            .map_err(|e| {
                PayError::InternalError(format!("transaction_log_store update tx-id index: {e}"))
            })?;
    }
    write_txn.commit().map_err(|e| {
        PayError::InternalError(format!("transaction_log_store commit append: {e}"))
    })?;

    Ok(())
}

pub fn load_wallet_transaction_records(
    data_dir: &str,
    wallet_id: &str,
) -> Result<Vec<HistoryRecord>, PayError> {
    let core_path = match wallet::wallet_core_database_path(data_dir, wallet_id) {
        Ok(p) => p,
        Err(PayError::WalletNotFound(_)) => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    if !core_path.exists() {
        return Ok(vec![]);
    }

    let db = db::open_database(&core_path)?;
    let read_txn = db
        .begin_read()
        .map_err(|e| PayError::InternalError(format!("transaction_log_store begin_read: {e}")))?;
    let Ok(table) = read_txn.open_table(TRANSACTION_RECORD_BY_SEQUENCE) else {
        return Ok(vec![]);
    };

    table
        .iter()
        .map_err(|e| {
            PayError::InternalError(format!("transaction_log_store iterate by-sequence: {e}"))
        })?
        .map(|entry| {
            let (_k, v) = entry.map_err(|e| {
                PayError::InternalError(format!("transaction_log_store read entry: {e}"))
            })?;
            serde_json::from_str::<HistoryRecord>(v.value()).map_err(|e| {
                PayError::InternalError(format!("transaction_log_store parse record: {e}"))
            })
        })
        .collect()
}

pub fn find_transaction_record_by_id(
    data_dir: &str,
    transaction_id: &str,
) -> Result<Option<HistoryRecord>, PayError> {
    let wallets = wallet::list_wallet_metadata(data_dir, None)?;
    for wallet_metadata in &wallets {
        if let Some(record) =
            find_transaction_record_in_wallet(data_dir, &wallet_metadata.id, transaction_id)?
        {
            return Ok(Some(record));
        }
    }
    Ok(None)
}

fn find_transaction_record_in_wallet(
    data_dir: &str,
    wallet_id: &str,
    transaction_id: &str,
) -> Result<Option<HistoryRecord>, PayError> {
    let core_path = match wallet::wallet_core_database_path(data_dir, wallet_id) {
        Ok(p) => p,
        Err(PayError::WalletNotFound(_)) => return Ok(None),
        Err(e) => return Err(e),
    };
    if !core_path.exists() {
        return Ok(None);
    }

    let db = db::open_database(&core_path)?;
    let read_txn = db
        .begin_read()
        .map_err(|e| PayError::InternalError(format!("transaction_log_store begin_read: {e}")))?;

    let Ok(sequence_by_transaction_id) =
        read_txn.open_table(TRANSACTION_SEQUENCE_BY_TRANSACTION_ID)
    else {
        return Ok(None);
    };
    let Some(sequence_guard) = sequence_by_transaction_id
        .get(transaction_id)
        .map_err(|e| {
            PayError::InternalError(format!("transaction_log_store read tx-id index: {e}"))
        })?
    else {
        return Ok(None);
    };
    let sequence = sequence_guard.value();

    let Ok(records_by_sequence) = read_txn.open_table(TRANSACTION_RECORD_BY_SEQUENCE) else {
        return Ok(None);
    };
    let Some(serialized_record) = records_by_sequence.get(sequence).map_err(|e| {
        PayError::InternalError(format!("transaction_log_store read by-sequence: {e}"))
    })?
    else {
        return Ok(None);
    };

    let record: HistoryRecord = serde_json::from_str(serialized_record.value()).map_err(|e| {
        PayError::InternalError(format!(
            "transaction_log_store parse record by sequence: {e}"
        ))
    })?;
    Ok(Some(record))
}

pub fn update_transaction_record_memo(
    data_dir: &str,
    transaction_id: &str,
    local_memo: Option<&std::collections::BTreeMap<String, String>>,
) -> Result<(), PayError> {
    let wallets = wallet::list_wallet_metadata(data_dir, None)?;
    for wallet_metadata in &wallets {
        let core_path = match wallet::wallet_core_database_path(data_dir, &wallet_metadata.id) {
            Ok(p) => p,
            Err(PayError::WalletNotFound(_)) => continue,
            Err(e) => return Err(e),
        };
        if !core_path.exists() {
            continue;
        }
        let db = db::open_database(&core_path)?;

        // Check if this wallet has the transaction
        let sequence = {
            let read_txn = db.begin_read().map_err(|e| {
                PayError::InternalError(format!("transaction_log_store begin_read: {e}"))
            })?;
            let Ok(idx_table) = read_txn.open_table(TRANSACTION_SEQUENCE_BY_TRANSACTION_ID) else {
                continue;
            };
            match idx_table.get(transaction_id) {
                Ok(Some(guard)) => guard.value(),
                _ => continue,
            }
        };

        // Read, update, and write back
        let write_txn = db.begin_write().map_err(|e| {
            PayError::InternalError(format!("transaction_log_store begin_write: {e}"))
        })?;
        {
            let mut records = write_txn
                .open_table(TRANSACTION_RECORD_BY_SEQUENCE)
                .map_err(|e| {
                    PayError::InternalError(format!(
                        "transaction_log_store open by-sequence table: {e}"
                    ))
                })?;
            let updated = {
                let serialized = records.get(sequence).map_err(|e| {
                    PayError::InternalError(format!("transaction_log_store read by-sequence: {e}"))
                })?;
                let Some(serialized) = serialized else {
                    return Err(PayError::InternalError(format!(
                        "transaction {transaction_id} sequence {sequence} missing"
                    )));
                };
                let mut record: HistoryRecord =
                    serde_json::from_str(serialized.value()).map_err(|e| {
                        PayError::InternalError(format!("transaction_log_store parse record: {e}"))
                    })?;
                record.local_memo = local_memo.cloned();
                serde_json::to_string(&record).map_err(|e| {
                    PayError::InternalError(format!("serialize updated record: {e}"))
                })?
            };
            records.insert(sequence, updated.as_str()).map_err(|e| {
                PayError::InternalError(format!("transaction_log_store update record: {e}"))
            })?;
        }
        write_txn.commit().map_err(|e| {
            PayError::InternalError(format!("transaction_log_store commit update: {e}"))
        })?;
        return Ok(());
    }
    Err(PayError::WalletNotFound(format!(
        "transaction {transaction_id} not found"
    )))
}

pub fn update_transaction_record_fee(
    data_dir: &str,
    transaction_id: &str,
    fee_value: u64,
    fee_unit: &str,
) -> Result<(), PayError> {
    let wallets = wallet::list_wallet_metadata(data_dir, None)?;
    for wallet_metadata in &wallets {
        let core_path = match wallet::wallet_core_database_path(data_dir, &wallet_metadata.id) {
            Ok(p) => p,
            Err(PayError::WalletNotFound(_)) => continue,
            Err(e) => return Err(e),
        };
        if !core_path.exists() {
            continue;
        }
        let db = db::open_database(&core_path)?;

        let sequence = {
            let read_txn = db.begin_read().map_err(|e| {
                PayError::InternalError(format!("transaction_log_store begin_read: {e}"))
            })?;
            let Ok(idx_table) = read_txn.open_table(TRANSACTION_SEQUENCE_BY_TRANSACTION_ID) else {
                continue;
            };
            match idx_table.get(transaction_id) {
                Ok(Some(guard)) => guard.value(),
                _ => continue,
            }
        };

        let write_txn = db.begin_write().map_err(|e| {
            PayError::InternalError(format!("transaction_log_store begin_write: {e}"))
        })?;
        {
            let mut records = write_txn
                .open_table(TRANSACTION_RECORD_BY_SEQUENCE)
                .map_err(|e| {
                    PayError::InternalError(format!(
                        "transaction_log_store open by-sequence table: {e}"
                    ))
                })?;
            let updated = {
                let serialized = records.get(sequence).map_err(|e| {
                    PayError::InternalError(format!("transaction_log_store read by-sequence: {e}"))
                })?;
                let Some(serialized) = serialized else {
                    return Err(PayError::InternalError(format!(
                        "transaction {transaction_id} sequence {sequence} missing"
                    )));
                };
                let mut record: HistoryRecord =
                    serde_json::from_str(serialized.value()).map_err(|e| {
                        PayError::InternalError(format!("transaction_log_store parse record: {e}"))
                    })?;
                record.fee = Some(crate::types::Amount {
                    value: fee_value,
                    token: fee_unit.to_string(),
                });
                serde_json::to_string(&record).map_err(|e| {
                    PayError::InternalError(format!("serialize updated record: {e}"))
                })?
            };
            records.insert(sequence, updated.as_str()).map_err(|e| {
                PayError::InternalError(format!("transaction_log_store update record: {e}"))
            })?;
        }
        write_txn.commit().map_err(|e| {
            PayError::InternalError(format!("transaction_log_store commit update: {e}"))
        })?;
        return Ok(());
    }
    Err(PayError::WalletNotFound(format!(
        "transaction {transaction_id} not found"
    )))
}

pub fn update_transaction_record_status(
    data_dir: &str,
    transaction_id: &str,
    status: TxStatus,
    confirmed_at_epoch_s: Option<u64>,
) -> Result<(), PayError> {
    let wallets = wallet::list_wallet_metadata(data_dir, None)?;
    for wallet_metadata in &wallets {
        let core_path = match wallet::wallet_core_database_path(data_dir, &wallet_metadata.id) {
            Ok(p) => p,
            Err(PayError::WalletNotFound(_)) => continue,
            Err(e) => return Err(e),
        };
        if !core_path.exists() {
            continue;
        }
        let db = db::open_database(&core_path)?;

        let sequence = {
            let read_txn = db.begin_read().map_err(|e| {
                PayError::InternalError(format!("transaction_log_store begin_read: {e}"))
            })?;
            let Ok(idx_table) = read_txn.open_table(TRANSACTION_SEQUENCE_BY_TRANSACTION_ID) else {
                continue;
            };
            match idx_table.get(transaction_id) {
                Ok(Some(guard)) => guard.value(),
                _ => continue,
            }
        };

        let write_txn = db.begin_write().map_err(|e| {
            PayError::InternalError(format!("transaction_log_store begin_write: {e}"))
        })?;
        {
            let mut records = write_txn
                .open_table(TRANSACTION_RECORD_BY_SEQUENCE)
                .map_err(|e| {
                    PayError::InternalError(format!(
                        "transaction_log_store open by-sequence table: {e}"
                    ))
                })?;
            let updated = {
                let serialized = records.get(sequence).map_err(|e| {
                    PayError::InternalError(format!("transaction_log_store read by-sequence: {e}"))
                })?;
                let Some(serialized) = serialized else {
                    return Err(PayError::InternalError(format!(
                        "transaction {transaction_id} sequence {sequence} missing"
                    )));
                };
                let mut record: HistoryRecord =
                    serde_json::from_str(serialized.value()).map_err(|e| {
                        PayError::InternalError(format!("transaction_log_store parse record: {e}"))
                    })?;
                record.status = status;
                record.confirmed_at_epoch_s = confirmed_at_epoch_s;
                serde_json::to_string(&record).map_err(|e| {
                    PayError::InternalError(format!("serialize updated record: {e}"))
                })?
            };
            records.insert(sequence, updated.as_str()).map_err(|e| {
                PayError::InternalError(format!("transaction_log_store update record: {e}"))
            })?;
        }
        write_txn.commit().map_err(|e| {
            PayError::InternalError(format!("transaction_log_store commit update: {e}"))
        })?;
        return Ok(());
    }
    Err(PayError::WalletNotFound(format!(
        "transaction {transaction_id} not found"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::wallet::{self, WalletMetadata};
    use crate::types::*;

    fn make_tx(transaction_id: &str, wallet: &str, value: u64) -> HistoryRecord {
        HistoryRecord {
            transaction_id: transaction_id.to_string(),
            wallet: wallet.to_string(),
            network: Network::Cashu,
            direction: Direction::Send,
            amount: Amount {
                value,
                token: "sats".to_string(),
            },
            status: TxStatus::Confirmed,
            onchain_memo: Some(format!("memo-{transaction_id}")),
            local_memo: None,
            remote_addr: None,
            preimage: None,
            created_at_epoch_s: 1700000000,
            confirmed_at_epoch_s: Some(1700000001),
            fee: None,
        }
    }

    fn ensure_wallet(dir: &str, wallet_id: &str, network: Network) {
        let meta = WalletMetadata {
            id: wallet_id.to_string(),
            network,
            label: None,
            mint_url: Some("https://mint.example".to_string()),
            sol_rpc_endpoints: None,
            evm_rpc_endpoints: None,
            evm_chain_id: None,
            seed_secret: Some("seed".to_string()),
            backend: if network == Network::Ln {
                Some("nwc".to_string())
            } else {
                None
            },
            btc_esplora_url: None,
            btc_network: None,
            btc_address_type: None,
            btc_core_url: None,
            btc_core_auth_secret: None,
            btc_electrum_url: None,
            custom_tokens: None,
            created_at_epoch_s: 1700000000,
            error: None,
        };
        wallet::save_wallet_metadata(dir, &meta).unwrap();
    }

    #[test]
    fn append_and_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();

        ensure_wallet(dir, "w_aabb", Network::Cashu);

        let tx1 = make_tx("tx_001", "w_aabb", 100);
        let tx2 = make_tx("tx_002", "w_aabb", 200);
        append_transaction_record(dir, &tx1).unwrap();
        append_transaction_record(dir, &tx2).unwrap();

        let loaded = load_wallet_transaction_records(dir, "w_aabb").unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].transaction_id, "tx_001");
        assert_eq!(loaded[0].amount.value, 100);
        assert_eq!(loaded[1].transaction_id, "tx_002");
        assert_eq!(loaded[1].amount.value, 200);
    }

    #[test]
    fn load_all_empty_wallet() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();
        let result = load_wallet_transaction_records(dir, "w_nonexist").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn find_tx_found() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();

        ensure_wallet(dir, "w_aabb", Network::Cashu);

        let tx = make_tx("tx_findme", "w_aabb", 42);
        append_transaction_record(dir, &tx).unwrap();

        let found = find_transaction_record_by_id(dir, "tx_findme").unwrap();
        assert!(found.is_some());
        let rec = found.unwrap();
        assert_eq!(rec.transaction_id, "tx_findme");
        assert_eq!(rec.amount.value, 42);
    }

    #[test]
    fn find_tx_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();

        ensure_wallet(dir, "w_aabb", Network::Cashu);

        let tx = make_tx("tx_exists", "w_aabb", 10);
        append_transaction_record(dir, &tx).unwrap();

        let found = find_transaction_record_by_id(dir, "tx_ghost").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn find_tx_across_wallets() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();

        ensure_wallet(dir, "w_first", Network::Cashu);
        ensure_wallet(dir, "w_second", Network::Ln);

        let tx1 = make_tx("tx_w1", "w_first", 10);
        let tx2 = make_tx("tx_w2", "w_second", 20);
        append_transaction_record(dir, &tx1).unwrap();
        append_transaction_record(dir, &tx2).unwrap();

        // find tx in second wallet's log file
        let found = find_transaction_record_by_id(dir, "tx_w2").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().wallet, "w_second");

        // find tx in first wallet's log file
        let found = find_transaction_record_by_id(dir, "tx_w1").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().wallet, "w_first");
    }

    #[test]
    fn update_tx_status_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();

        ensure_wallet(dir, "w_aabb", Network::Cashu);

        let mut tx = make_tx("tx_status", "w_aabb", 123);
        tx.status = TxStatus::Pending;
        tx.confirmed_at_epoch_s = None;
        append_transaction_record(dir, &tx).unwrap();

        update_transaction_record_status(dir, "tx_status", TxStatus::Confirmed, Some(1700001234))
            .unwrap();

        let updated = find_transaction_record_by_id(dir, "tx_status")
            .unwrap()
            .expect("record should exist");
        assert_eq!(updated.status, TxStatus::Confirmed);
        assert_eq!(updated.confirmed_at_epoch_s, Some(1700001234));
    }

    #[test]
    fn update_tx_status_missing_record_returns_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();

        ensure_wallet(dir, "w_aabb", Network::Cashu);

        let err = update_transaction_record_status(dir, "tx_missing", TxStatus::Confirmed, Some(1))
            .unwrap_err();
        assert!(
            matches!(err, PayError::WalletNotFound(_)),
            "expected WalletNotFound, got: {err}"
        );
    }
}
