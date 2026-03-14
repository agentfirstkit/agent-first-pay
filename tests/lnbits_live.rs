//! Integration tests for LnProvider (lnbits backend) against a local LNbits FakeWallet instance.
//!
//! Prerequisites — start LNbits with FakeWallet in Docker:
//!
//!   docker run -d -p 5001:5000 --name lnbits-test \
//!     -e LNBITS_BACKEND_WALLET_CLASS=FakeWallet \
//!     -e FAKE_WALLET_SECRET=testing \
//!     lnbits/lnbits:0.12.8
//!
//! Then create a user/wallet and grab the admin key:
//!
//!   curl -s http://localhost:5001/api/v1/account -X POST \
//!     -H 'Content-Type: application/json' \
//!     -d '{"username":"test","password":"test"}' | jq -r '.id'
//!
//! Run with:
//!   LNBITS_ENDPOINT=http://localhost:5001 \
//!   LNBITS_ADMIN_KEY=<admin_key> \
//!     cargo test --features ln-lnbits -- --ignored lnbits_live

#![cfg(feature = "ln-lnbits")]

use agent_first_pay::provider::ln::LnProvider;
use agent_first_pay::provider::{PayError, PayProvider};
use agent_first_pay::store::redb_store::RedbStore;
use agent_first_pay::store::StorageBackend;
use agent_first_pay::types::{Amount, LnWalletBackend, LnWalletCreateRequest, Network};
use std::sync::Arc;

fn endpoint() -> String {
    std::env::var("LNBITS_ENDPOINT").unwrap_or_else(|_| "http://localhost:5001".to_string())
}

fn admin_key() -> String {
    std::env::var("LNBITS_ADMIN_KEY")
        .expect("set LNBITS_ADMIN_KEY to a valid LNbits wallet admin key")
}

fn sats(value: u64) -> Amount {
    Amount {
        value,
        token: "sats".to_string(),
    }
}

/// Helper: create a LnProvider and an lnbits wallet in a temp dir.
async fn setup() -> (tempfile::TempDir, LnProvider, String) {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = LnProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let ep = endpoint();
    let key = admin_key();
    let info = provider
        .create_ln_wallet(LnWalletCreateRequest {
            backend: LnWalletBackend::Lnbits,
            label: Some(format!("test-{}", std::process::id())),
            nwc_uri_secret: None,
            endpoint: Some(ep),
            password_secret: None,
            admin_key_secret: Some(key),
        })
        .await
        .unwrap();
    assert!(info.id.starts_with("w_"), "wallet id: {}", info.id);
    assert_eq!(info.network, Network::Ln);

    (tmp, provider, info.id)
}

#[tokio::test]
#[ignore]
async fn lnbits_live_create_and_list() {
    let (_tmp, provider, wallet_id) = setup().await;

    let list = provider.list_wallets().await.unwrap();
    assert!(
        list.iter().any(|w| w.id == wallet_id),
        "created wallet should appear in list"
    );
    assert!(
        list.iter().all(|w| w.network == Network::Ln),
        "all wallets should be ln"
    );
}

#[tokio::test]
#[ignore]
async fn lnbits_live_balance() {
    let (_tmp, provider, wallet_id) = setup().await;

    let bal = provider.balance(&wallet_id).await.unwrap();
    assert!(
        bal.confirmed < 100_000_000,
        "balance should be reasonable: {} sats",
        bal.confirmed
    );
}

#[tokio::test]
#[ignore]
async fn lnbits_live_balance_all() {
    let (_tmp, provider, wallet_id) = setup().await;

    let items = provider.balance_all().await.unwrap();
    assert!(
        items.iter().any(|w| w.wallet.id == wallet_id),
        "wallet should appear in balance_all"
    );
}

#[tokio::test]
#[ignore]
async fn lnbits_live_receive_info_creates_invoice() {
    let (_tmp, provider, wallet_id) = setup().await;

    let dep = provider
        .receive_info(&wallet_id, Some(sats(1000)))
        .await
        .unwrap();
    let invoice = dep.invoice.as_deref().unwrap_or("");
    let payment_hash = dep.quote_id.as_deref().unwrap_or("");

    assert!(
        invoice.starts_with("lntb") || invoice.starts_with("lnbc"),
        "should be a bolt11 invoice: {}",
        &invoice[..invoice.len().min(30)]
    );
    assert!(!payment_hash.is_empty(), "payment_hash should not be empty");
}

#[tokio::test]
#[ignore]
async fn lnbits_live_receive_claim_unpaid() {
    let (_tmp, provider, wallet_id) = setup().await;

    // Create an invoice then immediately check — FakeWallet may auto-settle,
    // so we accept either NetworkError (unpaid) or success (auto-settled).
    let dep = provider
        .receive_info(&wallet_id, Some(sats(1000)))
        .await
        .unwrap();
    let payment_hash = dep.quote_id.as_deref().unwrap_or("");

    match provider.receive_claim(&wallet_id, payment_hash).await {
        Err(PayError::NetworkError(_)) => {
            // Expected: invoice not yet paid
        }
        Ok(amount) => {
            // FakeWallet auto-settled the invoice
            assert!(amount > 0, "auto-settled amount should be positive");
        }
        Err(other) => {
            panic!("unexpected error: {other}");
        }
    }
}

#[tokio::test]
#[ignore]
async fn lnbits_live_transactions() {
    let (_tmp, provider, wallet_id) = setup().await;

    let txs = provider.history_list(&wallet_id, 20, 0).await.unwrap();
    // Just verify it returns without error; list may be empty on fresh wallet
    assert!(txs.len() <= 20, "should respect limit");
}

#[tokio::test]
#[ignore]
async fn lnbits_live_send_invalid_invoice_fails() {
    let (_tmp, provider, wallet_id) = setup().await;

    let err = provider
        .send(&wallet_id, "fake-destination", None, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, PayError::InvalidAmount(_)),
        "invalid invoice should fail validation, got: {err}"
    );
}

#[tokio::test]
#[ignore]
async fn lnbits_live_send_quote_invalid_invoice_fails() {
    let (_tmp, provider, wallet_id) = setup().await;

    let err = provider
        .send_quote(&wallet_id, "fake-destination", None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, PayError::InvalidAmount(_)),
        "invalid invoice should fail quote parsing, got: {err}"
    );
}

#[tokio::test]
#[ignore]
async fn lnbits_live_wallet_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = LnProvider::new(
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
async fn lnbits_live_receive_info_requires_amount() {
    let (_tmp, provider, wallet_id) = setup().await;

    let err = provider.receive_info(&wallet_id, None).await.unwrap_err();
    assert!(
        matches!(err, PayError::InvalidAmount(_)),
        "deposit without amount should fail, got: {err}"
    );
}

#[tokio::test]
#[ignore]
async fn lnbits_live_history_sync() {
    let (_tmp, provider, wallet_id) = setup().await;

    let stats = provider.history_sync(&wallet_id, 50).await.unwrap();
    // On a fresh wallet, we expect 0 records scanned and 0 added.
    // Just verify the call succeeds and returns sane values.
    assert!(
        stats.records_added <= stats.records_scanned,
        "added ({}) should not exceed scanned ({})",
        stats.records_added,
        stats.records_scanned
    );
}
