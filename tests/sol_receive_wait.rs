#![cfg(feature = "redb")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use agent_first_pay::handler::{dispatch, App};
use agent_first_pay::provider::{PayError, PayProvider};
use agent_first_pay::store::wallet::{self, WalletMetadata};
use agent_first_pay::store::{create_storage_backend, PayStore};
use agent_first_pay::types::{
    Amount, BalanceInfo, CashuReceiveResult, CashuSendResult, Direction, HistoryRecord,
    HistoryStatusInfo, Input, Network, Output, ReceiveInfo, RuntimeConfig, SendResult, TxStatus,
    WalletBalanceItem, WalletCreateRequest, WalletInfo, WalletSummary,
};
use async_trait::async_trait;
use tokio::sync::mpsc;

struct MockSolWaitProvider {
    wallet_id: String,
    tx_id: String,
    memo: String,
}

impl MockSolWaitProvider {
    fn incoming_record(&self) -> HistoryRecord {
        HistoryRecord {
            transaction_id: self.tx_id.clone(),
            wallet: self.wallet_id.clone(),
            network: Network::Sol,
            direction: Direction::Receive,
            amount: Amount {
                value: 1000,
                token: "lamports".to_string(),
            },
            status: TxStatus::Confirmed,
            onchain_memo: Some(self.memo.clone()),
            local_memo: None,
            remote_addr: None,
            preimage: None,
            created_at_epoch_s: wallet::now_epoch_seconds(),
            confirmed_at_epoch_s: Some(wallet::now_epoch_seconds()),
            fee: None,
            reference_keys: None,
        }
    }
}

#[async_trait]
impl PayProvider for MockSolWaitProvider {
    fn network(&self) -> Network {
        Network::Sol
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
        Ok(BalanceInfo::new(0, 0, "lamports"))
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
            address: Some("8nTKRhLQDcnCaS5s8Z4KZPb1i9ddfbfQDeJpw7g4QxjV".to_string()),
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
        Ok(vec![self.incoming_record()])
    }

    async fn history_status(&self, _transaction_id: &str) -> Result<HistoryStatusInfo, PayError> {
        // Simulate Solana finalized tx where confirmations are unavailable (None).
        Ok(HistoryStatusInfo {
            transaction_id: self.tx_id.clone(),
            status: TxStatus::Confirmed,
            confirmations: None,
            preimage: None,
            item: Some(self.incoming_record()),
        })
    }
}

#[tokio::test]
async fn sol_receive_wait_min_confirmations_accepts_finalized_without_depth_value() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().to_string_lossy().into_owned();
    let config = RuntimeConfig {
        data_dir: data_dir.clone(),
        ..RuntimeConfig::default()
    };
    let store = create_storage_backend(&config).expect("storage backend");

    let wallet_id = "w_sol_wait".to_string();
    store
        .save_wallet_metadata(&WalletMetadata {
            id: wallet_id.clone(),
            network: Network::Sol,
            label: Some("sol-wait".to_string()),
            mint_url: None,
            sol_rpc_endpoints: Some(vec!["https://api.devnet.solana.com".to_string()]),
            evm_rpc_endpoints: None,
            evm_chain_id: None,
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
        Network::Sol,
        Box::new(MockSolWaitProvider {
            wallet_id: wallet_id.clone(),
            tx_id: "sig_wait_test".to_string(),
            memo: "order:abc".to_string(),
        }),
    );

    dispatch(
        &app,
        Input::Receive {
            id: "req_sol_wait".to_string(),
            wallet: wallet_id.clone(),
            network: Some(Network::Sol),
            amount: None,
            onchain_memo: Some("order:abc".to_string()),
            wait_until_paid: true,
            wait_timeout_s: Some(2),
            wait_poll_interval_ms: Some(1),
            wait_sync_limit: None,
            write_qr_svg_file: false,
            min_confirmations: Some(6),
            reference: None,
        },
    )
    .await;

    drop(app);

    let mut saw_history_status = false;
    while let Some(output) = rx.recv().await {
        match output {
            Output::HistoryStatus {
                status,
                confirmations,
                ..
            } => {
                assert_eq!(status, TxStatus::Confirmed);
                assert_eq!(confirmations, Some(6));
                saw_history_status = true;
            }
            Output::Error { error, .. } => {
                panic!("unexpected error output: {error}");
            }
            _ => {}
        }
    }

    assert!(
        saw_history_status,
        "expected history_status output for finalized sol tx"
    );
}
