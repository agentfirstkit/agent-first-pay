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
use tokio::sync::mpsc;

struct MockBtcWaitProvider {
    wallet_id: String,
    tx_id: String,
    balance_calls: AtomicUsize,
    sync_calls: AtomicUsize,
}

impl MockBtcWaitProvider {
    fn new(wallet_id: String) -> Self {
        Self {
            wallet_id,
            tx_id: "btc_wait_chain_txid_001".to_string(),
            balance_calls: AtomicUsize::new(0),
            sync_calls: AtomicUsize::new(0),
        }
    }

    fn incoming_record(&self) -> HistoryRecord {
        HistoryRecord {
            transaction_id: self.tx_id.clone(),
            wallet: self.wallet_id.clone(),
            network: Network::Btc,
            direction: Direction::Receive,
            amount: Amount {
                value: 500,
                token: "sats".to_string(),
            },
            status: TxStatus::Pending,
            onchain_memo: Some(self.tx_id.clone()),
            local_memo: None,
            remote_addr: None,
            preimage: None,
            created_at_epoch_s: wallet::now_epoch_seconds(),
            confirmed_at_epoch_s: None,
            fee: None,
            reference_keys: None,
        }
    }
}

#[async_trait]
impl PayProvider for MockBtcWaitProvider {
    fn network(&self) -> Network {
        Network::Btc
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
        let call_idx = self.balance_calls.fetch_add(1, Ordering::SeqCst);
        if call_idx == 0 {
            Ok(BalanceInfo::new(0, 0, "sats"))
        } else {
            Ok(BalanceInfo::new(0, 500, "sats"))
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
            address: Some("tb1ptestreceivewait".to_string()),
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
}

#[tokio::test]
async fn btc_receive_wait_routes_to_btc_polling_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_string_lossy().into_owned();
    let config = RuntimeConfig {
        data_dir: data_dir.clone(),
        ..RuntimeConfig::default()
    };
    let store = create_storage_backend(&config).expect("storage backend should be available");

    let wallet_id = "w_btc_wait".to_string();
    let wallet_meta = WalletMetadata {
        id: wallet_id.clone(),
        network: Network::Btc,
        label: Some("btc-wait".to_string()),
        mint_url: None,
        sol_rpc_endpoints: None,
        evm_rpc_endpoints: None,
        evm_chain_id: None,
        seed_secret: Some("seed".to_string()),
        backend: Some("esplora".to_string()),
        btc_esplora_url: Some("https://example.invalid".to_string()),
        btc_network: Some("signet".to_string()),
        btc_address_type: Some("taproot".to_string()),
        btc_core_url: None,
        btc_core_auth_secret: None,
        btc_electrum_url: None,
        custom_tokens: None,
        created_at_epoch_s: wallet::now_epoch_seconds(),
        error: None,
    };
    store.save_wallet_metadata(&wallet_meta).unwrap();

    let (tx, mut rx) = mpsc::channel::<Output>(32);
    let mut app = App::new(config, tx, Some(true), Some(store.clone()));
    app.providers.insert(
        Network::Btc,
        Box::new(MockBtcWaitProvider::new(wallet_id.clone())),
    );

    dispatch(
        &app,
        Input::Receive {
            id: "req_btc_wait".to_string(),
            wallet: wallet_id.clone(),
            network: Some(Network::Btc),
            amount: Some(Amount {
                value: 500,
                token: "sats".to_string(),
            }),
            onchain_memo: None,
            wait_until_paid: true,
            wait_timeout_s: Some(2),
            wait_poll_interval_ms: Some(1),
            wait_sync_limit: None,
            write_qr_svg_file: false,
            min_confirmations: None,
            reference: None,
        },
    )
    .await;

    drop(app);

    let mut saw_receive_info = false;
    let mut history_tx_id: Option<String> = None;
    while let Some(out) = rx.recv().await {
        match out {
            Output::ReceiveInfo {
                wallet,
                receive_info,
                ..
            } => {
                saw_receive_info = true;
                assert_eq!(wallet, wallet_id);
                assert!(receive_info.quote_id.is_none());
            }
            Output::HistoryStatus {
                transaction_id,
                status,
                confirmations,
                item,
                ..
            } => {
                history_tx_id = Some(transaction_id);
                assert_eq!(status, TxStatus::Pending);
                assert_eq!(confirmations, Some(0));
                let history_item = item.expect("history_status should include item");
                assert_eq!(history_item.wallet, wallet_id);
                assert_eq!(history_item.network, Network::Btc);
                assert_eq!(history_item.direction, Direction::Receive);
                assert_eq!(history_item.amount.value, 500);
                assert_eq!(history_item.amount.token, "sats");
            }
            Output::Error { error, .. } => {
                assert!(
                    !error.contains("deposit response missing quote_id/payment_hash"),
                    "btc wait should not go through quote_id flow: {error}"
                );
            }
            _ => {}
        }
    }

    assert!(saw_receive_info, "expected receive_info output");
    let tx_id = history_tx_id.expect("expected history_status output");
    assert_eq!(tx_id, "btc_wait_chain_txid_001");
    assert!(
        !tx_id.starts_with("btc_recv_"),
        "btc wait should emit on-chain txid, got synthetic id {tx_id}"
    );
}
