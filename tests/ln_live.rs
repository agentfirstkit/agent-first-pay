#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Integration tests for LnProvider (phoenixd backend) against a local phoenixd testnet instance.
//!
//! Prerequisites:
//!   phoenixd --chain=testnet --agree-to-terms-of-service
//!
//! Run with:
//!   PHOENIXD_PASSWORD=$(grep http-password ~/.phoenix/phoenix.conf | head -1 | cut -d= -f2) \
//!     cargo test --features ln-phoenixd -- --ignored ln_live

#![cfg(feature = "ln-phoenixd")]

use agent_first_pay::provider::ln::LnProvider;
use agent_first_pay::provider::{PayError, PayProvider};
use agent_first_pay::store::redb_store::RedbStore;
use agent_first_pay::store::StorageBackend;
use agent_first_pay::types::{Amount, LnWalletBackend, LnWalletCreateRequest, Network};
use std::sync::Arc;

fn endpoint() -> String {
    std::env::var("PHOENIXD_ENDPOINT").unwrap_or_else(|_| "http://localhost:9740".to_string())
}

fn password() -> String {
    std::env::var("PHOENIXD_PASSWORD").unwrap_or_else(|_| {
        // Try reading from phoenix.conf
        let home = std::env::var("HOME").unwrap_or_default();
        let conf = std::path::Path::new(&home).join(".phoenix/phoenix.conf");
        if let Ok(contents) = std::fs::read_to_string(conf) {
            for line in contents.lines() {
                if let Some(rest) = line.strip_prefix("http-password=") {
                    return rest.to_string();
                }
            }
        }
        panic!("set PHOENIXD_PASSWORD or ensure ~/.phoenix/phoenix.conf exists");
    })
}

fn sats(value: u64) -> Amount {
    Amount {
        value,
        token: "sats".to_string(),
    }
}

/// Helper: create a LnProvider and a phoenixd wallet in a temp dir.
async fn setup() -> (tempfile::TempDir, LnProvider, String) {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = LnProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let ep = endpoint();
    let pw = password();
    let info = provider
        .create_ln_wallet(LnWalletCreateRequest {
            backend: LnWalletBackend::Phoenixd,
            label: Some(format!("test-{}", std::process::id())),
            nwc_uri_secret: None,
            endpoint: Some(ep),
            password_secret: Some(pw),
            admin_key_secret: None,
        })
        .await
        .unwrap();
    assert!(info.id.starts_with("w_"), "wallet id: {}", info.id);
    assert_eq!(info.network, Network::Ln);

    (tmp, provider, info.id)
}

#[tokio::test]
#[ignore]
async fn ln_live_create_and_list() {
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
async fn ln_live_balance() {
    let (_tmp, provider, wallet_id) = setup().await;

    let bal = provider.balance(&wallet_id).await.unwrap();
    // Fresh phoenixd testnet node — balance is whatever it has (likely 0)
    // Just verify no error and fields are sane
    assert!(
        bal.confirmed < 100_000_000,
        "balance should be reasonable: {} sats",
        bal.confirmed
    );
}

#[tokio::test]
#[ignore]
async fn ln_live_balance_all() {
    let (_tmp, provider, wallet_id) = setup().await;

    let items = provider.balance_all().await.unwrap();
    assert!(
        items.iter().any(|w| w.wallet.id == wallet_id),
        "wallet should appear in balance_all"
    );
}

#[tokio::test]
#[ignore]
async fn ln_live_receive_info_creates_invoice() {
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
async fn ln_live_receive_claim_unpaid() {
    let (_tmp, provider, wallet_id) = setup().await;

    // Create an invoice then immediately check — should be unpaid
    let dep = provider
        .receive_info(&wallet_id, Some(sats(1000)))
        .await
        .unwrap();
    let payment_hash = dep.quote_id.as_deref().unwrap_or("");

    let err = provider
        .receive_claim(&wallet_id, payment_hash)
        .await
        .unwrap_err();
    // Should be a NetworkError since invoice is not yet paid
    assert!(
        matches!(err, PayError::NetworkError(_)),
        "expected NetworkError for unpaid invoice, got: {err}"
    );
}

#[tokio::test]
#[ignore]
async fn ln_live_transactions() {
    let (_tmp, provider, wallet_id) = setup().await;

    let txs = provider.history_list(&wallet_id, 20, 0).await.unwrap();
    // Just verify it returns without error; list may be empty on fresh node
    assert!(txs.len() <= 20, "should respect limit");
}

#[tokio::test]
#[ignore]
async fn ln_live_send_invalid_invoice_fails() {
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
async fn ln_live_send_quote_invalid_invoice_fails() {
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
async fn ln_live_wallet_not_found() {
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
async fn ln_live_receive_bolt12_offer() {
    let (_tmp, provider, wallet_id) = setup().await;

    // With BOLT12 support, receive without amount returns a persistent offer
    let info = provider.receive_info(&wallet_id, None).await.unwrap();
    let offer = info.address.expect("should return an offer address");
    assert!(
        offer.starts_with("lno1"),
        "offer should start with lno1, got: {}",
        &offer[..offer.len().min(30)]
    );
    assert!(
        info.invoice.is_none(),
        "bolt12 should not return a bolt11 invoice"
    );
    assert!(
        info.quote_id.is_none(),
        "bolt12 should not return a quote_id"
    );
}

#[tokio::test]
#[ignore]
async fn ln_live_send_quote_rejects_bolt12() {
    let (_tmp, provider, wallet_id) = setup().await;

    // send_quote should reject BOLT12 offers (not parseable as bolt11)
    let err = provider
        .send_quote(&wallet_id, "lno1qgsqvgjwrdkmcakuay0rz", None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, PayError::InvalidAmount(_)),
        "bolt12 should fail send_quote, got: {err}"
    );
}
