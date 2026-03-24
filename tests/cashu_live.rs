#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Integration tests for CashuProvider against testnut.cashu.space (FakeWallet mint).
//!
//! All Lightning invoices are auto-paid by the FakeWallet mint, so no real funds needed.
//!
//! Run with:
//!   cargo test --test cashu_live -- --ignored                        # redb (default)
//!   cargo test --test cashu_live -- --ignored cashu_live_redb        # redb only
//!   cargo test --test cashu_live -- --ignored cashu_live_cdk_redb    # cdk-redb store

#![cfg(feature = "cashu")]

use agent_first_pay::provider::cashu::CashuProvider;
use agent_first_pay::provider::{PayError, PayProvider};
use agent_first_pay::store::{self, PayStore};
use agent_first_pay::types::{Amount, Direction, Network, RuntimeConfig, WalletCreateRequest};
use std::sync::Arc;

const MINT_URL: &str = "https://testnut.cashu.space";

fn sats(value: u64) -> Amount {
    Amount {
        value,
        token: "sats".to_string(),
    }
}

#[cfg(feature = "redb")]
fn make_redb_provider(data_dir: &str) -> CashuProvider {
    let store = store::create_storage_backend(&RuntimeConfig {
        data_dir: data_dir.to_string(),
        ..RuntimeConfig::default()
    })
    .expect("redb store");
    CashuProvider::new(data_dir, None, Arc::new(store))
}

// ═══════════════════════════════════════════
// Full flow: create → deposit → send → receive → history
// ═══════════════════════════════════════════

#[cfg(feature = "redb")]
#[tokio::test]
#[ignore]
async fn cashu_live_redb() {
    let tmp = tempfile::tempdir().unwrap();
    let provider = make_redb_provider(tmp.path().to_str().unwrap());
    cashu_full_flow(&provider).await;
}

/// Shared full-flow test body.
async fn cashu_full_flow(provider: &CashuProvider) {
    // ── 1. create wallet ──
    let w1 = provider
        .create_wallet(&WalletCreateRequest {
            label: "test-wallet".to_string(),
            mint_url: Some(MINT_URL.to_string()),
            rpc_endpoints: vec![],
            chain_id: None,
            mnemonic_secret: None,
            btc_esplora_url: None,
            btc_network: None,
            btc_address_type: None,
            btc_backend: None,
            btc_core_url: None,
            btc_core_auth_secret: None,
            btc_electrum_url: None,
        })
        .await
        .unwrap();
    assert!(w1.id.starts_with("w_"), "wallet id format: {}", w1.id);
    assert_eq!(w1.network, Network::Cashu);
    assert!(
        w1.address.contains("testnut.cashu.space"),
        "address should be mint url: {}",
        w1.address
    );

    // ── 2. list wallets ──
    let list = provider.list_wallets().await.unwrap();
    assert!(
        list.iter().any(|w| w.id == w1.id),
        "created wallet should appear in list"
    );

    // ── 3. initial balance = 0 ──
    let bal = provider.balance(&w1.id).await.unwrap();
    assert_eq!(bal.confirmed, 0);
    assert_eq!(bal.pending, 0);

    // ── 4. receive_info (get invoice + quote_id) ──
    let deposit_amount: u64 = 64;
    let dep = provider
        .receive_info(&w1.id, Some(sats(deposit_amount)))
        .await
        .unwrap();
    let invoice = dep.invoice.as_deref().unwrap();
    let quote_id = dep.quote_id.as_deref().unwrap();
    assert!(!invoice.is_empty(), "invoice should not be empty");
    assert!(!quote_id.is_empty(), "quote_id should not be empty");

    // ── 5. receive_claim (FakeWallet auto-pays) ──
    let claimed = provider.receive_claim(&w1.id, quote_id).await.unwrap();
    assert_eq!(
        claimed, deposit_amount,
        "claimed amount should match deposit"
    );

    // receive_claim should be persisted as a receive history entry
    let claim_history = provider.history_list(&w1.id, 100, 0).await.unwrap();
    assert!(
        claim_history.iter().any(|h| {
            h.transaction_id == quote_id
                && h.direction == Direction::Receive
                && h.amount.value == deposit_amount
        }),
        "receive_claim should append receive history by quote_id"
    );

    // ── 6. balance after claim ──
    let bal = provider.balance(&w1.id).await.unwrap();
    assert_eq!(
        bal.confirmed, deposit_amount,
        "balance should reflect claimed amount"
    );

    // ── 7. cashu_send (P2P cashu token) ──
    let send_amount: u64 = 16;
    let send_result = provider
        .cashu_send(&w1.id, sats(send_amount), Some("p2p test"), None)
        .await
        .unwrap();
    assert!(
        send_result.transaction_id.starts_with("tx_"),
        "transaction_id format: {}",
        send_result.transaction_id
    );
    let token_str = &send_result.token;
    assert!(
        token_str.starts_with("cashu"),
        "token should start with cashu prefix: {}",
        &token_str[..token_str.len().min(40)]
    );

    let send_fee = send_result.fee.as_ref().map(|f| f.value).unwrap_or(0);

    // balance should decrease
    let bal = provider.balance(&w1.id).await.unwrap();
    assert_eq!(
        bal.confirmed,
        deposit_amount - send_amount - send_fee,
        "balance should decrease by send amount plus fee"
    );

    // ── 8. history list ──
    let txs = provider.history_list(&w1.id, 100, 0).await.unwrap();
    assert!(
        txs.iter()
            .any(|t| t.transaction_id == send_result.transaction_id),
        "send record should appear in history"
    );

    // ── 9. history status ──
    let status = provider
        .history_status(&send_result.transaction_id)
        .await
        .unwrap();
    assert_eq!(status.transaction_id, send_result.transaction_id);

    // ── 10. cashu_receive (token from step 7 into a second wallet) ──
    let recv = provider.cashu_receive("", token_str).await.unwrap();
    assert_eq!(
        recv.amount.value, send_amount,
        "received amount should match sent amount"
    );
    assert!(!recv.wallet.is_empty(), "receive should resolve a wallet");
}

// ═══════════════════════════════════════════
// CDK-redb localstore: verify the redb database file is created on testnet
// ═══════════════════════════════════════════

#[cfg(feature = "redb")]
#[tokio::test]
#[ignore]
async fn cashu_live_cdk_redb() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let store = store::create_storage_backend(&RuntimeConfig {
        data_dir: data_dir.to_string(),
        ..RuntimeConfig::default()
    })
    .expect("redb store");
    let store = Arc::new(store);
    let provider = CashuProvider::new(data_dir, None, store.clone());

    let w = provider
        .create_wallet(&WalletCreateRequest {
            label: "cdk-redb-test".to_string(),
            mint_url: Some(MINT_URL.to_string()),
            rpc_endpoints: vec![],
            chain_id: None,
            mnemonic_secret: None,
            btc_esplora_url: None,
            btc_network: None,
            btc_address_type: None,
            btc_backend: None,
            btc_core_url: None,
            btc_core_auth_secret: None,
            btc_electrum_url: None,
        })
        .await
        .unwrap();

    // balance() triggers CDK wallet creation with cdk-redb
    let bal = provider.balance(&w.id).await.unwrap();
    assert_eq!(bal.confirmed, 0);

    // Verify the cdk-wallet.redb file was created
    let meta = store.load_wallet_metadata(&w.id).unwrap();
    let db_dir = store.wallet_data_directory_path_for_meta(&meta);
    let redb_path = db_dir.join("cdk-wallet.redb");
    assert!(
        redb_path.exists(),
        "cdk-wallet.redb should be created at {redb_path:?}"
    );
}

// ═══════════════════════════════════════════
// Error paths
// ═══════════════════════════════════════════

#[cfg(feature = "redb")]
#[tokio::test]
#[ignore]
async fn balance_wallet_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let provider = make_redb_provider(tmp.path().to_str().unwrap());

    let err = provider.balance("w_nonexist").await.unwrap_err();
    assert!(
        matches!(err, PayError::WalletNotFound(_)),
        "expected WalletNotFound, got: {err}"
    );
}

#[cfg(feature = "redb")]
#[tokio::test]
#[ignore]
async fn receive_claim_wallet_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let provider = make_redb_provider(tmp.path().to_str().unwrap());

    let err = provider
        .receive_claim("w_nonexist", "fake_quote")
        .await
        .unwrap_err();
    assert!(
        matches!(err, PayError::WalletNotFound(_)),
        "expected WalletNotFound, got: {err}"
    );
}

#[cfg(feature = "redb")]
#[tokio::test]
#[ignore]
async fn cashu_send_insufficient_balance() {
    let tmp = tempfile::tempdir().unwrap();
    let provider = make_redb_provider(tmp.path().to_str().unwrap());

    let w = provider
        .create_wallet(&WalletCreateRequest {
            label: "empty".to_string(),
            mint_url: Some(MINT_URL.to_string()),
            rpc_endpoints: vec![],
            chain_id: None,
            mnemonic_secret: None,
            btc_esplora_url: None,
            btc_network: None,
            btc_address_type: None,
            btc_backend: None,
            btc_core_url: None,
            btc_core_auth_secret: None,
            btc_electrum_url: None,
        })
        .await
        .unwrap();

    // cashu_send from a wallet with 0 balance
    let err = provider
        .cashu_send(&w.id, sats(100), None, None)
        .await
        .unwrap_err();
    // CDK returns a NetworkError (from prepare_send) when there are insufficient funds
    assert!(
        matches!(err, PayError::NetworkError(_)),
        "expected error for insufficient balance, got: {err}"
    );
}

// ═══════════════════════════════════════════
// CDK-postgres: verify it attempts postgres connection
// ═══════════════════════════════════════════

#[cfg(all(feature = "redb", feature = "postgres"))]
#[tokio::test]
#[ignore]
async fn cashu_live_cdk_postgres_selection() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let store = store::create_storage_backend(&RuntimeConfig {
        data_dir: data_dir.to_string(),
        ..RuntimeConfig::default()
    })
    .expect("redb store");
    let store = Arc::new(store);

    // Provider configured with postgres_url → should use cdk-postgres for CDK wallet
    let provider = CashuProvider::new(
        data_dir,
        Some("postgres://localhost:5432/nonexistent_cdk_test".to_string()),
        store,
    );

    let w = provider
        .create_wallet(&WalletCreateRequest {
            label: "pg-cdk".to_string(),
            mint_url: Some(MINT_URL.to_string()),
            rpc_endpoints: vec![],
            chain_id: None,
            mnemonic_secret: None,
            btc_esplora_url: None,
            btc_network: None,
            btc_address_type: None,
            btc_backend: None,
            btc_core_url: None,
            btc_core_auth_secret: None,
            btc_electrum_url: None,
        })
        .await
        .unwrap();

    // balance() triggers get_or_create_cdk_wallet → cdk-postgres → connection error
    let err = provider.balance(&w.id).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("cdk postgres"),
        "expected cdk postgres error, got: {msg}"
    );
}
