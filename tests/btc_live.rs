#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr
)]
//! Integration tests for BtcProvider against Bitcoin Signet.
//!
//! These tests hit the real Signet Esplora API — no funds required for read-only tests.
//! Send tests require signet BTC in the wallet (use a signet faucet).
//!
//! Run read-only tests:
//!   cargo test --features btc -- --ignored btc_live
//!
//! Run send tests (requires funded wallet):
//!   BTC_TEST_MNEMONIC="<12 words>" cargo test --features btc -- --ignored btc_live_send

#![cfg(feature = "btc-esplora")]

use agent_first_pay::provider::btc::BtcProvider;
use agent_first_pay::provider::{PayError, PayProvider};
use agent_first_pay::store::redb_store::RedbStore;
use agent_first_pay::store::StorageBackend;
use agent_first_pay::types::{Network, TxStatus, WalletCreateRequest};
use std::process::Command;
use std::sync::Arc;

fn signet_request(label: &str) -> WalletCreateRequest {
    WalletCreateRequest {
        label: label.to_string(),
        mint_url: None,
        rpc_endpoints: vec![],
        chain_id: None,
        mnemonic_secret: None,
        btc_esplora_url: None,
        btc_network: Some("signet".to_string()),
        btc_address_type: Some("taproot".to_string()),
        btc_backend: None,
        btc_core_url: None,
        btc_core_auth_secret: None,
        btc_electrum_url: None,
    }
}

fn signet_request_segwit(label: &str) -> WalletCreateRequest {
    WalletCreateRequest {
        btc_address_type: Some("segwit".to_string()),
        ..signet_request(label)
    }
}

fn signet_request_with_mnemonic(label: &str, mnemonic: String) -> WalletCreateRequest {
    WalletCreateRequest {
        mnemonic_secret: Some(mnemonic),
        ..signet_request(label)
    }
}

// ═══════════════════════════════════════════
// Read-only tests (no funds needed)
// ═══════════════════════════════════════════

#[tokio::test]
#[ignore]
async fn btc_live_create_taproot_and_list() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = BtcProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let w = provider
        .create_wallet(&signet_request("signet-taproot"))
        .await
        .unwrap();
    assert!(w.id.starts_with("w_"), "wallet id format: {}", w.id);
    assert_eq!(w.network, Network::Btc);
    assert!(
        w.address.starts_with("tb1p"),
        "taproot signet address should start with tb1p: {}",
        w.address
    );

    let list = provider.list_wallets().await.unwrap();
    assert!(
        list.iter().any(|ws| ws.id == w.id),
        "created wallet should appear in list"
    );
}

#[tokio::test]
#[ignore]
async fn btc_live_create_segwit_address() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = BtcProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let w = provider
        .create_wallet(&signet_request_segwit("signet-segwit"))
        .await
        .unwrap();
    assert_eq!(w.network, Network::Btc);
    assert!(
        w.address.starts_with("tb1q"),
        "segwit signet address should start with tb1q: {}",
        w.address
    );
}

#[tokio::test]
#[ignore]
async fn btc_live_balance_new_wallet() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = BtcProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let w = provider
        .create_wallet(&signet_request("signet-bal"))
        .await
        .unwrap();

    let bal = provider.balance(&w.id).await.unwrap();
    assert_eq!(bal.confirmed, 0, "new signet wallet should have 0 sats");
    assert_eq!(bal.unit, "sats");
}

#[tokio::test]
#[ignore]
async fn btc_live_balance_all() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = BtcProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let w = provider
        .create_wallet(&signet_request("signet-all"))
        .await
        .unwrap();

    let items = provider.balance_all().await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].wallet.id, w.id);
    assert!(items[0].balance.is_some(), "balance should resolve");
    assert!(items[0].error.is_none(), "no error expected");
}

#[tokio::test]
#[ignore]
async fn btc_live_receive_info() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = BtcProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let w = provider
        .create_wallet(&signet_request("signet-recv"))
        .await
        .unwrap();

    let info = provider.receive_info(&w.id, None).await.unwrap();
    assert!(
        info.address.is_some(),
        "receive_info should return an address"
    );
    assert_eq!(info.address.unwrap(), w.address);
}

#[tokio::test]
#[ignore]
async fn btc_live_wallet_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = BtcProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let err = provider.balance("w_nonexist").await.unwrap_err();
    assert!(
        matches!(err, PayError::WalletNotFound(_)),
        "expected WalletNotFound, got: {err}"
    );
}

#[tokio::test]
#[ignore]
async fn btc_live_close_empty_wallet() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = BtcProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let w = provider
        .create_wallet(&signet_request("signet-close"))
        .await
        .unwrap();

    provider.close_wallet(&w.id).await.unwrap();

    let list = provider.list_wallets().await.unwrap();
    assert!(
        !list.iter().any(|ws| ws.id == w.id),
        "closed wallet should not appear in list"
    );
}

#[tokio::test]
#[ignore]
async fn btc_live_history_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = BtcProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let w = provider
        .create_wallet(&signet_request("signet-hist"))
        .await
        .unwrap();

    let history = provider.history_list(&w.id, 10, 0).await.unwrap();
    assert!(history.is_empty(), "new wallet should have no history");
}

#[tokio::test]
#[ignore]
async fn btc_live_mnemonic_restore_same_address() {
    let tmp1 = tempfile::tempdir().unwrap();
    let dir1 = tmp1.path().to_str().unwrap();
    let provider1 = BtcProvider::new(dir1, Arc::new(StorageBackend::Redb(RedbStore::new(dir1))));

    let w1 = provider1
        .create_wallet(&signet_request("signet-orig"))
        .await
        .unwrap();

    // Read back seed
    let meta = agent_first_pay::store::wallet::load_wallet_metadata(dir1, &w1.id).unwrap();
    let mnemonic = meta.seed_secret.unwrap();

    // Restore in a fresh data dir
    let tmp2 = tempfile::tempdir().unwrap();
    let dir2 = tmp2.path().to_str().unwrap();
    let provider2 = BtcProvider::new(dir2, Arc::new(StorageBackend::Redb(RedbStore::new(dir2))));

    let w2 = provider2
        .create_wallet(&signet_request_with_mnemonic("signet-restore", mnemonic))
        .await
        .unwrap();

    assert_eq!(
        w1.address, w2.address,
        "restored wallet should have same address"
    );
}

#[tokio::test]
#[ignore]
async fn btc_live_multiple_wallets() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = BtcProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let w1 = provider
        .create_wallet(&signet_request("wallet-1"))
        .await
        .unwrap();
    let w2 = provider
        .create_wallet(&signet_request_segwit("wallet-2"))
        .await
        .unwrap();

    assert_ne!(w1.id, w2.id);
    assert_ne!(w1.address, w2.address);

    let list = provider.list_wallets().await.unwrap();
    assert_eq!(list.len(), 2);

    let items = provider.balance_all().await.unwrap();
    assert_eq!(items.len(), 2);
}

#[tokio::test]
#[ignore]
async fn btc_live_send_quote_supported() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = BtcProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let w = provider
        .create_wallet(&signet_request("signet-send-quote"))
        .await
        .unwrap();

    let err = provider
        .send_quote(&w.id, &format!("bitcoin:{SIGNET_SINK}?amount=1000"), None)
        .await
        .unwrap_err();
    assert!(
        !matches!(err, PayError::NotImplemented(_)),
        "send_quote should be implemented, got: {err}"
    );
}

// ═══════════════════════════════════════════
// Send tests — require funded signet wallet
// ═══════════════════════════════════════════
//
// Set BTC_TEST_MNEMONIC to a BIP39 mnemonic with signet BTC:
//   BTC_TEST_MNEMONIC="word1 word2 ... word12" \
//     cargo test --features btc -- --ignored btc_live_send

fn funded_mnemonic() -> Option<String> {
    std::env::var("BTC_TEST_MNEMONIC").ok()
}

/// Signet faucet return address (or any valid signet address).
const SIGNET_SINK: &str = "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx";

#[tokio::test]
#[ignore]
async fn btc_live_send_signet() {
    let mnemonic = match funded_mnemonic() {
        Some(m) => m,
        None => {
            println!("SKIP: BTC_TEST_MNEMONIC not set");
            return;
        }
    };
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = BtcProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let w = provider
        .create_wallet(&signet_request_with_mnemonic("funded", mnemonic))
        .await
        .unwrap();

    let bal = provider.balance(&w.id).await.unwrap();
    if bal.confirmed < 2000 {
        println!(
            "SKIP: wallet {} has {} sats, need >= 2000. Use signet faucet to send to {}",
            w.id, bal.confirmed, w.address
        );
        return;
    }

    let result = provider
        .send(
            &w.id,
            &format!("bitcoin:{SIGNET_SINK}?amount=1000"),
            None,
            None,
        )
        .await
        .unwrap();

    assert!(!result.transaction_id.is_empty());
    assert_eq!(result.amount.value, 1000);
    assert_eq!(result.amount.token, "sats");

    // Check history records the send
    let history = provider.history_list(&w.id, 10, 0).await.unwrap();
    assert!(
        history
            .iter()
            .any(|h| h.transaction_id == result.transaction_id),
        "send should appear in history"
    );

    // history_status should query chain confirmation state for btc tx.
    let status = provider
        .history_status(&result.transaction_id)
        .await
        .unwrap();
    assert!(
        matches!(status.status, TxStatus::Pending | TxStatus::Confirmed),
        "unexpected status: {:?}",
        status.status
    );
    assert!(
        status.confirmations.is_some(),
        "btc history_status should return confirmation count"
    );
    if status.status == TxStatus::Confirmed {
        assert!(
            status
                .item
                .as_ref()
                .and_then(|i| i.confirmed_at_epoch_s)
                .is_some(),
            "confirmed btc tx should backfill confirmed_at_epoch_s"
        );
    }
}

#[test]
#[ignore]
fn btc_live_receive_wait_uses_btc_branch() {
    let exe = env!("CARGO_BIN_EXE_afpay");
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();

    let create = Command::new(exe)
        .args([
            "--data-dir",
            data_dir,
            "wallet",
            "create",
            "--network",
            "btc",
            "--btc-network",
            "signet",
            "--label",
            "wait-branch",
        ])
        .output()
        .expect("run afpay wallet create");
    assert!(create.status.success(), "wallet create failed");

    let stdout = String::from_utf8(create.stdout).expect("wallet create stdout utf8");
    let line = stdout
        .lines()
        .last()
        .expect("wallet create stdout should contain json line");
    let parsed: serde_json::Value = serde_json::from_str(line).expect("parse wallet create json");
    let wallet = parsed["wallet"]
        .as_str()
        .expect("wallet field in create output");

    let wait = Command::new(exe)
        .args([
            "--data-dir",
            data_dir,
            "btc",
            "receive",
            "--wallet",
            wallet,
            "--wait",
            "--wait-timeout-s",
            "1",
            "--wait-poll-interval-ms",
            "200",
        ])
        .output()
        .expect("run afpay btc receive --wait");

    let wait_stdout = String::from_utf8(wait.stdout).expect("wait stdout utf8");
    assert!(
        !wait_stdout.contains("deposit response missing quote_id/payment_hash"),
        "btc wait should not route into quote_id flow: {wait_stdout}"
    );
}
