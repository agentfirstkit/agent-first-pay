#![cfg(feature = "redb")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use agent_first_pay::handler::{dispatch, App};
use agent_first_pay::provider::{HistorySyncStats, PayError, PayProvider};
use agent_first_pay::store::wallet::{self, WalletMetadata};
use agent_first_pay::store::{create_storage_backend, PayStore};
use agent_first_pay::types::{
    Amount, BalanceInfo, CashuReceiveResult, CashuSendResult, Direction, HistoryRecord,
    HistoryStatusInfo, Input, Network, Output, ReceiveInfo, RuntimeConfig, SendResult, TxStatus,
    WalletBalanceItem, WalletCreateRequest, WalletInfo, WalletSummary,
};
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

struct LocalOnlyHistoryProvider;

#[async_trait]
impl PayProvider for LocalOnlyHistoryProvider {
    fn network(&self) -> Network {
        Network::Evm
    }

    async fn create_wallet(&self, _request: &WalletCreateRequest) -> Result<WalletInfo, PayError> {
        Err(PayError::NotImplemented("unused in test".to_string()))
    }

    async fn close_wallet(&self, _wallet: &str) -> Result<(), PayError> {
        Err(PayError::NotImplemented("unused in test".to_string()))
    }

    async fn list_wallets(&self) -> Result<Vec<WalletSummary>, PayError> {
        Ok(vec![])
    }

    async fn balance(&self, _wallet: &str) -> Result<BalanceInfo, PayError> {
        Ok(BalanceInfo::new(0, 0, "gwei"))
    }

    async fn balance_all(&self) -> Result<Vec<WalletBalanceItem>, PayError> {
        Ok(vec![])
    }

    async fn receive_info(
        &self,
        _wallet: &str,
        _amount: Option<Amount>,
    ) -> Result<ReceiveInfo, PayError> {
        Ok(ReceiveInfo {
            address: Some("0x000000000000000000000000000000000000dEaD".to_string()),
            invoice: None,
            quote_id: None,
        })
    }

    async fn receive_claim(&self, _wallet: &str, _quote_id: &str) -> Result<u64, PayError> {
        Err(PayError::NotImplemented("unused in test".to_string()))
    }

    async fn cashu_send(
        &self,
        _wallet: &str,
        _amount: Amount,
        _onchain_memo: Option<&str>,
        _mints: Option<&[String]>,
    ) -> Result<CashuSendResult, PayError> {
        Err(PayError::NotImplemented("unused in test".to_string()))
    }

    async fn cashu_receive(
        &self,
        _wallet: &str,
        _token: &str,
    ) -> Result<CashuReceiveResult, PayError> {
        Err(PayError::NotImplemented("unused in test".to_string()))
    }

    async fn send(
        &self,
        _wallet: &str,
        _to: &str,
        _onchain_memo: Option<&str>,
        _mints: Option<&[String]>,
    ) -> Result<SendResult, PayError> {
        Err(PayError::NotImplemented("unused in test".to_string()))
    }

    async fn history_list(
        &self,
        _wallet: &str,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<HistoryRecord>, PayError> {
        panic!("history_list should not be called for Input::HistoryList")
    }

    async fn history_status(&self, _transaction_id: &str) -> Result<HistoryStatusInfo, PayError> {
        Err(PayError::NotImplemented("unused in test".to_string()))
    }
}

struct SyncStatsProvider {
    wallet_id: String,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl PayProvider for SyncStatsProvider {
    fn network(&self) -> Network {
        Network::Evm
    }

    async fn create_wallet(&self, _request: &WalletCreateRequest) -> Result<WalletInfo, PayError> {
        Err(PayError::NotImplemented("unused in test".to_string()))
    }

    async fn close_wallet(&self, _wallet: &str) -> Result<(), PayError> {
        Err(PayError::NotImplemented("unused in test".to_string()))
    }

    async fn list_wallets(&self) -> Result<Vec<WalletSummary>, PayError> {
        Ok(vec![])
    }

    async fn balance(&self, _wallet: &str) -> Result<BalanceInfo, PayError> {
        Ok(BalanceInfo::new(0, 0, "gwei"))
    }

    async fn balance_all(&self) -> Result<Vec<WalletBalanceItem>, PayError> {
        Ok(vec![])
    }

    async fn receive_info(
        &self,
        _wallet: &str,
        _amount: Option<Amount>,
    ) -> Result<ReceiveInfo, PayError> {
        Ok(ReceiveInfo {
            address: Some("0x000000000000000000000000000000000000dEaD".to_string()),
            invoice: None,
            quote_id: None,
        })
    }

    async fn receive_claim(&self, _wallet: &str, _quote_id: &str) -> Result<u64, PayError> {
        Err(PayError::NotImplemented("unused in test".to_string()))
    }

    async fn cashu_send(
        &self,
        _wallet: &str,
        _amount: Amount,
        _onchain_memo: Option<&str>,
        _mints: Option<&[String]>,
    ) -> Result<CashuSendResult, PayError> {
        Err(PayError::NotImplemented("unused in test".to_string()))
    }

    async fn cashu_receive(
        &self,
        _wallet: &str,
        _token: &str,
    ) -> Result<CashuReceiveResult, PayError> {
        Err(PayError::NotImplemented("unused in test".to_string()))
    }

    async fn send(
        &self,
        _wallet: &str,
        _to: &str,
        _onchain_memo: Option<&str>,
        _mints: Option<&[String]>,
    ) -> Result<SendResult, PayError> {
        Err(PayError::NotImplemented("unused in test".to_string()))
    }

    async fn history_list(
        &self,
        _wallet: &str,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<HistoryRecord>, PayError> {
        panic!("history_list should not be called by history_update")
    }

    async fn history_status(&self, _transaction_id: &str) -> Result<HistoryStatusInfo, PayError> {
        Err(PayError::NotImplemented("unused in test".to_string()))
    }

    async fn history_sync(&self, wallet: &str, limit: usize) -> Result<HistorySyncStats, PayError> {
        assert_eq!(wallet, self.wallet_id);
        assert_eq!(limit, 50);
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(HistorySyncStats {
            records_scanned: 11,
            records_added: 4,
            records_updated: 3,
        })
    }
}

fn seed_phrase() -> String {
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
        .to_string()
}

fn setup_wallet(
    store: &agent_first_pay::store::StorageBackend,
    wallet_id: &str,
) -> Result<(), Box<PayError>> {
    store
        .save_wallet_metadata(&WalletMetadata {
            id: wallet_id.to_string(),
            network: Network::Evm,
            label: Some("evm-test".to_string()),
            mint_url: None,
            sol_rpc_endpoints: None,
            evm_rpc_endpoints: Some(vec!["https://rpc.example".to_string()]),
            evm_chain_id: Some(8453),
            seed_secret: Some(seed_phrase()),
            backend: None,
            btc_esplora_url: None,
            btc_network: None,
            btc_address_type: None,
            btc_core_url: None,
            btc_core_auth_secret: None,
            btc_electrum_url: None,
            custom_tokens: None,
            created_at_epoch_s: wallet::now_epoch_seconds(),
            error: None,
        })
        .map_err(Box::new)
}

#[tokio::test]
async fn history_list_reads_only_local_store() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().to_string_lossy().into_owned();
    let config = RuntimeConfig {
        data_dir,
        ..RuntimeConfig::default()
    };
    let store = create_storage_backend(&config).expect("storage backend");

    let wallet_id = "w_hist_local";
    setup_wallet(&store, wallet_id).expect("save wallet");
    store
        .append_transaction_record(&HistoryRecord {
            transaction_id: "tx_local_1".to_string(),
            wallet: wallet_id.to_string(),
            network: Network::Evm,
            direction: Direction::Receive,
            amount: Amount {
                value: 100,
                token: "gwei".to_string(),
            },
            status: TxStatus::Confirmed,
            onchain_memo: Some("memo-1".to_string()),
            local_memo: None,
            remote_addr: None,
            preimage: None,
            created_at_epoch_s: wallet::now_epoch_seconds(),
            confirmed_at_epoch_s: Some(wallet::now_epoch_seconds()),
            fee: None,
            reference_keys: None,
        })
        .expect("append transaction");

    let (tx, mut rx) = mpsc::channel::<Output>(16);
    let mut app = App::new(config, tx, Some(true), Some(store));
    app.providers
        .insert(Network::Evm, Box::new(LocalOnlyHistoryProvider));

    dispatch(
        &app,
        Input::HistoryList {
            id: "req_hist_local".to_string(),
            wallet: Some(wallet_id.to_string()),
            network: Some(Network::Evm),
            onchain_memo: None,
            limit: Some(10),
            offset: Some(0),
            since_epoch_s: None,
            until_epoch_s: None,
        },
    )
    .await;

    drop(app);

    let mut saw_history = false;
    while let Some(output) = rx.recv().await {
        if let Output::History { items, .. } = output {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].transaction_id, "tx_local_1");
            saw_history = true;
        }
    }

    assert!(saw_history, "expected history output");
}

#[tokio::test]
async fn history_update_calls_provider_sync_and_returns_stats() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().to_string_lossy().into_owned();
    let config = RuntimeConfig {
        data_dir,
        ..RuntimeConfig::default()
    };
    let store = create_storage_backend(&config).expect("storage backend");

    let wallet_id = "w_hist_sync";
    setup_wallet(&store, wallet_id).expect("save wallet");

    let calls = Arc::new(AtomicUsize::new(0));
    let (tx, mut rx) = mpsc::channel::<Output>(16);
    let mut app = App::new(config, tx, Some(true), Some(store));
    app.providers.insert(
        Network::Evm,
        Box::new(SyncStatsProvider {
            wallet_id: wallet_id.to_string(),
            calls: calls.clone(),
        }),
    );

    dispatch(
        &app,
        Input::HistoryUpdate {
            id: "req_hist_sync".to_string(),
            wallet: Some(wallet_id.to_string()),
            network: Some(Network::Evm),
            limit: Some(50),
        },
    )
    .await;

    drop(app);

    let mut saw_history_updated = false;
    while let Some(output) = rx.recv().await {
        if let Output::HistoryUpdated {
            wallets_synced,
            records_scanned,
            records_added,
            records_updated,
            ..
        } = output
        {
            assert_eq!(wallets_synced, 1);
            assert_eq!(records_scanned, 11);
            assert_eq!(records_added, 4);
            assert_eq!(records_updated, 3);
            saw_history_updated = true;
        }
    }

    assert!(saw_history_updated, "expected history_updated output");
    assert_eq!(calls.load(Ordering::Relaxed), 1, "history_sync call count");
}
