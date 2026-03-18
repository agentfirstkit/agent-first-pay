#![cfg(feature = "redb")]

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
use tokio::sync::mpsc;

struct MockEvmWaitProvider {
    wallet_id: String,
    tx_id: String,
    emit_history: bool,
    chain_memo: Option<String>,
    balance_calls: AtomicUsize,
    sync_calls: AtomicUsize,
}

impl MockEvmWaitProvider {
    fn for_history(wallet_id: String) -> Self {
        Self {
            wallet_id,
            tx_id: "0xevm_wait_chain_txid_001".to_string(),
            emit_history: true,
            chain_memo: None,
            balance_calls: AtomicUsize::new(0),
            sync_calls: AtomicUsize::new(0),
        }
    }

    fn for_history_with_memo(wallet_id: String, memo: &str) -> Self {
        Self {
            wallet_id,
            tx_id: "0xevm_wait_chain_txid_001".to_string(),
            emit_history: true,
            chain_memo: Some(memo.to_string()),
            balance_calls: AtomicUsize::new(0),
            sync_calls: AtomicUsize::new(0),
        }
    }

    fn incoming_record(&self) -> HistoryRecord {
        HistoryRecord {
            transaction_id: self.tx_id.clone(),
            wallet: self.wallet_id.clone(),
            network: Network::Evm,
            direction: Direction::Receive,
            amount: Amount {
                value: 100,
                token: "gwei".to_string(),
            },
            status: TxStatus::Pending,
            onchain_memo: None,
            local_memo: None,
            remote_addr: None,
            preimage: None,
            created_at_epoch_s: wallet::now_epoch_seconds(),
            confirmed_at_epoch_s: None,
            fee: None,
        }
    }
}

#[async_trait]
impl PayProvider for MockEvmWaitProvider {
    fn network(&self) -> Network {
        Network::Evm
    }

    async fn create_wallet(&self, _request: &WalletCreateRequest) -> Result<WalletInfo, PayError> {
        Err(PayError::NotImplemented(
            "create_wallet not used in this test".to_string(),
        ))
    }

    async fn close_wallet(&self, _wallet: &str) -> Result<(), PayError> {
        Err(PayError::NotImplemented(
            "close_wallet not used in this test".to_string(),
        ))
    }

    async fn list_wallets(&self) -> Result<Vec<WalletSummary>, PayError> {
        Ok(vec![])
    }

    async fn balance(&self, _wallet: &str) -> Result<BalanceInfo, PayError> {
        if !self.emit_history {
            return Ok(BalanceInfo::new(0, 0, "gwei"));
        }
        let call_idx = self.balance_calls.fetch_add(1, Ordering::SeqCst);
        if call_idx == 0 {
            Ok(BalanceInfo::new(0, 0, "gwei"))
        } else {
            Ok(BalanceInfo::new(100, 0, "gwei"))
        }
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
        Err(PayError::NotImplemented(
            "receive_claim not used in this test".to_string(),
        ))
    }

    async fn cashu_send(
        &self,
        _wallet: &str,
        _amount: Amount,
        _onchain_memo: Option<&str>,
        _mints: Option<&[String]>,
    ) -> Result<CashuSendResult, PayError> {
        Err(PayError::NotImplemented(
            "cashu_send not used in this test".to_string(),
        ))
    }

    async fn cashu_receive(
        &self,
        _wallet: &str,
        _token: &str,
    ) -> Result<CashuReceiveResult, PayError> {
        Err(PayError::NotImplemented(
            "cashu_receive not used in this test".to_string(),
        ))
    }

    async fn send(
        &self,
        _wallet: &str,
        _to: &str,
        _onchain_memo: Option<&str>,
        _mints: Option<&[String]>,
    ) -> Result<SendResult, PayError> {
        Err(PayError::NotImplemented(
            "send not used in this test".to_string(),
        ))
    }

    async fn history_list(
        &self,
        _wallet: &str,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<HistoryRecord>, PayError> {
        if !self.emit_history {
            return Ok(vec![]);
        }
        if self.sync_calls.load(Ordering::SeqCst) == 0 {
            Ok(vec![])
        } else {
            Ok(vec![self.incoming_record()])
        }
    }

    async fn history_status(&self, transaction_id: &str) -> Result<HistoryStatusInfo, PayError> {
        if transaction_id != self.tx_id {
            return Err(PayError::WalletNotFound(format!(
                "transaction {transaction_id} not found"
            )));
        }
        Ok(HistoryStatusInfo {
            transaction_id: self.tx_id.clone(),
            status: TxStatus::Pending,
            confirmations: Some(0),
            preimage: None,
            item: Some(self.incoming_record()),
        })
    }

    async fn history_sync(
        &self,
        _wallet: &str,
        _limit: usize,
    ) -> Result<HistorySyncStats, PayError> {
        self.sync_calls.fetch_add(1, Ordering::SeqCst);
        Ok(HistorySyncStats::default())
    }

    async fn history_onchain_memo(
        &self,
        _wallet: &str,
        transaction_id: &str,
    ) -> Result<Option<String>, PayError> {
        if transaction_id != self.tx_id {
            return Ok(None);
        }
        Ok(self.chain_memo.clone())
    }
}

#[tokio::test]
async fn evm_receive_wait_matches_onchain_memo() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().to_string_lossy().into_owned();
    let config = RuntimeConfig {
        data_dir: data_dir.clone(),
        ..RuntimeConfig::default()
    };
    let store = create_storage_backend(&config).expect("storage backend");

    let wallet_id = "w_evm_wait".to_string();
    store
        .save_wallet_metadata(&WalletMetadata {
            id: wallet_id.clone(),
            network: Network::Evm,
            label: Some("evm-wait".to_string()),
            mint_url: None,
            sol_rpc_endpoints: None,
            evm_rpc_endpoints: Some(vec!["https://rpc.example".to_string()]),
            evm_chain_id: Some(8453),
            seed_secret: Some(
                "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
                    .to_string(),
            ),
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
        .expect("save wallet metadata");

    let (tx, mut rx) = mpsc::channel::<Output>(16);
    let mut app = App::new(config, tx, Some(true), Some(store));
    app.providers.insert(
        Network::Evm,
        Box::new(MockEvmWaitProvider::for_history_with_memo(
            wallet_id.clone(),
            "order:abc",
        )),
    );

    dispatch(
        &app,
        Input::Receive {
            id: "req_evm_wait".to_string(),
            wallet: wallet_id.clone(),
            network: Some(Network::Evm),
            amount: Some(Amount {
                value: 100,
                token: "native".to_string(),
            }),
            onchain_memo: Some("order:abc".to_string()),
            wait_until_paid: true,
            wait_timeout_s: Some(2),
            wait_poll_interval_ms: Some(1),
            wait_sync_limit: None,
            write_qr_svg_file: false,
            min_confirmations: None,
        },
    )
    .await;

    drop(app);

    let mut saw_receive_info = false;
    let mut saw_history_status = false;
    while let Some(output) = rx.recv().await {
        match output {
            Output::ReceiveInfo { wallet, .. } => {
                assert_eq!(wallet, wallet_id);
                saw_receive_info = true;
            }
            Output::HistoryStatus { transaction_id, .. } => {
                assert_eq!(transaction_id, "0xevm_wait_chain_txid_001");
                saw_history_status = true;
            }
            Output::Error { error, .. } => {
                panic!("unexpected error output: {error}");
            }
            _ => {}
        }
    }

    assert!(saw_receive_info, "expected receive_info output");
    assert!(
        saw_history_status,
        "expected history_status output for onchain memo match"
    );
}

#[tokio::test]
async fn evm_receive_wait_emits_chain_transaction_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().to_string_lossy().into_owned();
    let config = RuntimeConfig {
        data_dir: data_dir.clone(),
        ..RuntimeConfig::default()
    };
    let store = create_storage_backend(&config).expect("storage backend");

    let wallet_id = "w_evm_wait_txid".to_string();
    store
        .save_wallet_metadata(&WalletMetadata {
            id: wallet_id.clone(),
            network: Network::Evm,
            label: Some("evm-wait-txid".to_string()),
            mint_url: None,
            sol_rpc_endpoints: None,
            evm_rpc_endpoints: Some(vec!["https://rpc.example".to_string()]),
            evm_chain_id: Some(8453),
            seed_secret: Some(
                "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
                    .to_string(),
            ),
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
        .expect("save wallet metadata");

    let (tx, mut rx) = mpsc::channel::<Output>(32);
    let mut app = App::new(config, tx, Some(true), Some(store));
    app.providers.insert(
        Network::Evm,
        Box::new(MockEvmWaitProvider::for_history(wallet_id.clone())),
    );

    dispatch(
        &app,
        Input::Receive {
            id: "req_evm_wait_txid".to_string(),
            wallet: wallet_id.clone(),
            network: Some(Network::Evm),
            amount: Some(Amount {
                value: 100,
                token: "native".to_string(),
            }),
            onchain_memo: None,
            wait_until_paid: true,
            wait_timeout_s: Some(2),
            wait_poll_interval_ms: Some(1),
            wait_sync_limit: None,
            write_qr_svg_file: false,
            min_confirmations: None,
        },
    )
    .await;

    drop(app);

    let mut saw_receive_info = false;
    let mut history_tx_id: Option<String> = None;
    while let Some(output) = rx.recv().await {
        match output {
            Output::ReceiveInfo { wallet, .. } => {
                assert_eq!(wallet, wallet_id);
                saw_receive_info = true;
            }
            Output::HistoryStatus {
                transaction_id,
                status,
                confirmations,
                item,
                ..
            } => {
                history_tx_id = Some(transaction_id.clone());
                assert_eq!(status, TxStatus::Pending);
                assert_eq!(confirmations, Some(0));
                let history_item = item.expect("history_status should include item");
                assert_eq!(history_item.transaction_id, transaction_id);
                assert_eq!(history_item.wallet, wallet_id);
                assert_eq!(history_item.network, Network::Evm);
                assert_eq!(history_item.direction, Direction::Receive);
                assert_eq!(history_item.amount.value, 100);
                assert_eq!(history_item.amount.token, "gwei");
            }
            Output::Error { error, .. } => {
                panic!("unexpected error output: {error}");
            }
            _ => {}
        }
    }

    assert!(saw_receive_info, "expected receive_info output");
    let tx_id = history_tx_id.expect("expected history_status output");
    assert_eq!(tx_id, "0xevm_wait_chain_txid_001");
    assert!(
        !tx_id.starts_with("evm_recv_"),
        "evm wait should emit on-chain tx hash, got synthetic id {tx_id}"
    );
}
