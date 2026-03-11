#![cfg(feature = "redb")]

use agent_first_pay::handler::{dispatch, App};
use agent_first_pay::provider::{PayError, PayProvider};
use agent_first_pay::store::wallet::{self, WalletMetadata};
use agent_first_pay::store::{create_storage_backend, PayStore};
use agent_first_pay::types::{
    Amount, BalanceInfo, CashuReceiveResult, CashuSendResult, HistoryRecord, HistoryStatusInfo,
    Input, Network, Output, ReceiveInfo, RuntimeConfig, SendResult, WalletBalanceItem,
    WalletCreateRequest, WalletInfo, WalletSummary,
};
use async_trait::async_trait;
use tokio::sync::mpsc;

struct MockEvmWaitProvider;

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
        Ok(vec![])
    }

    async fn history_status(&self, _transaction_id: &str) -> Result<HistoryStatusInfo, PayError> {
        Err(PayError::NotImplemented(
            "history_status not used in this test".to_string(),
        ))
    }
}

#[tokio::test]
async fn evm_receive_wait_rejects_onchain_memo_matching() {
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
    app.providers
        .insert(Network::Evm, Box::new(MockEvmWaitProvider));

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
            write_qr_svg_file: false,
            min_confirmations: None,
        },
    )
    .await;

    drop(app);

    let mut saw_receive_info = false;
    let mut saw_expected_error = false;
    while let Some(output) = rx.recv().await {
        match output {
            Output::ReceiveInfo { wallet, .. } => {
                assert_eq!(wallet, wallet_id);
                saw_receive_info = true;
            }
            Output::Error { error, .. } => {
                if error.contains("does not support --onchain-memo matching") {
                    saw_expected_error = true;
                }
            }
            Output::HistoryStatus { .. } => {
                panic!("evm receive wait with onchain memo should not emit history status");
            }
            _ => {}
        }
    }

    assert!(
        saw_receive_info,
        "expected receive_info output before error"
    );
    assert!(
        saw_expected_error,
        "expected unsupported onchain-memo error"
    );
}
