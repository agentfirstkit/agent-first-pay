#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr
)]
//! Integration tests for SolProvider against Solana devnet.
//!
//! These tests hit the real devnet RPC — no funds required for read-only tests.
//! Send tests require devnet SOL in the wallet (use `solana airdrop`).
//!
//! Run read-only tests:
//!   cargo test --features sol -- --ignored sol_live
//!
//! Run send tests (requires funded wallet):
//!   SOL_TEST_MNEMONIC="<12 words>" cargo test --features sol -- --ignored sol_live_send

#![cfg(feature = "sol")]

use agent_first_pay::provider::sol::SolProvider;
use agent_first_pay::provider::{PayError, PayProvider};
use agent_first_pay::store::redb_store::RedbStore;
use agent_first_pay::store::StorageBackend;
use agent_first_pay::types::{Network, WalletCreateRequest};
use std::sync::Arc;

const DEVNET_RPC: &str = "https://api.devnet.solana.com";

#[tokio::test]
#[ignore]
async fn sol_live_create_and_list() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = SolProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let w = provider
        .create_wallet(&WalletCreateRequest {
            label: "devnet-test".to_string(),
            mint_url: None,
            rpc_endpoints: vec![DEVNET_RPC.to_string()],
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
    assert!(w.id.starts_with("w_"), "wallet id format: {}", w.id);
    assert_eq!(w.network, Network::Sol);
    assert!(!w.address.is_empty(), "address should not be empty");

    let list = provider.list_wallets().await.unwrap();
    assert!(
        list.iter().any(|ws| ws.id == w.id),
        "created wallet should appear in list"
    );
}

#[tokio::test]
#[ignore]
async fn sol_live_balance_new_wallet() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = SolProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let w = provider
        .create_wallet(&WalletCreateRequest {
            label: "devnet-bal".to_string(),
            mint_url: None,
            rpc_endpoints: vec![DEVNET_RPC.to_string()],
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

    let bal = provider.balance(&w.id).await.unwrap();
    assert_eq!(bal.confirmed, 0, "new devnet wallet should have 0 lamports");
    assert_eq!(bal.unit, "lamports");
}

#[tokio::test]
#[ignore]
async fn sol_live_balance_all() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = SolProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let w = provider
        .create_wallet(&WalletCreateRequest {
            label: "devnet-all".to_string(),
            mint_url: None,
            rpc_endpoints: vec![DEVNET_RPC.to_string()],
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

    let items = provider.balance_all().await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].wallet.id, w.id);
    assert!(items[0].balance.is_some(), "balance should resolve");
    assert!(items[0].error.is_none(), "no error expected");
}

#[tokio::test]
#[ignore]
async fn sol_live_receive_info() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = SolProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let w = provider
        .create_wallet(&WalletCreateRequest {
            label: "devnet-dep".to_string(),
            mint_url: None,
            rpc_endpoints: vec![DEVNET_RPC.to_string()],
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

    let dep = provider.receive_info(&w.id, None).await.unwrap();
    assert!(
        dep.address.is_some(),
        "receive_info should return an address"
    );
    assert_eq!(dep.address.unwrap(), w.address);
}

#[tokio::test]
#[ignore]
async fn sol_live_wallet_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = SolProvider::new(
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
async fn sol_live_close_empty_wallet() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = SolProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let w = provider
        .create_wallet(&WalletCreateRequest {
            label: "devnet-close".to_string(),
            mint_url: None,
            rpc_endpoints: vec![DEVNET_RPC.to_string()],
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

    // New wallet has 0 balance → close should succeed
    provider.close_wallet(&w.id).await.unwrap();

    let list = provider.list_wallets().await.unwrap();
    assert!(
        !list.iter().any(|ws| ws.id == w.id),
        "closed wallet should not appear in list"
    );
}

#[tokio::test]
#[ignore]
async fn sol_live_history_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = SolProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let w = provider
        .create_wallet(&WalletCreateRequest {
            label: "devnet-hist".to_string(),
            mint_url: None,
            rpc_endpoints: vec![DEVNET_RPC.to_string()],
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

    let history = provider.history_list(&w.id, 10, 0).await.unwrap();
    assert!(history.is_empty(), "new wallet should have no history");
}

// ═══════════════════════════════════════════
// Send tests — require funded wallet
// ═══════════════════════════════════════════
//
// Set SOL_TEST_MNEMONIC to a BIP39 mnemonic with devnet SOL:
//   SOL_TEST_MNEMONIC="word1 word2 ... word12" cargo test --features sol -- --ignored sol_live_send

fn funded_mnemonic() -> Option<String> {
    std::env::var("SOL_TEST_MNEMONIC").ok()
}

/// Known devnet address to send to (system program — will always exist)
const DEVNET_SINK: &str = "11111111111111111111111111111111";

#[tokio::test]
#[ignore]
async fn sol_live_send_native() {
    let mnemonic = match funded_mnemonic() {
        Some(m) => m,
        None => {
            println!("SKIP: SOL_TEST_MNEMONIC not set");
            return;
        }
    };
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = SolProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let w = provider
        .create_wallet(&WalletCreateRequest {
            label: "funded".to_string(),
            mint_url: None,
            rpc_endpoints: vec![DEVNET_RPC.to_string()],
            chain_id: None,
            mnemonic_secret: Some(mnemonic.clone()),
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

    let bal = provider.balance(&w.id).await.unwrap();
    if bal.confirmed < 10_000 {
        println!(
            "SKIP: wallet {} has {} lamports, need >= 10000. Run: solana airdrop 1 {} --url devnet",
            w.id, bal.confirmed, w.address
        );
        return;
    }

    let result = provider
        .send(
            &w.id,
            &format!("solana:{DEVNET_SINK}?amount-lamports=1000"),
            Some("test-native"),
            None,
        )
        .await
        .unwrap();

    assert!(!result.transaction_id.is_empty());
    assert_eq!(result.amount.value, 1000);
    assert_eq!(result.amount.token, "lamports");

    // Check history records the send
    let history = provider.history_list(&w.id, 10, 0).await.unwrap();
    assert!(
        history
            .iter()
            .any(|h| h.transaction_id == result.transaction_id),
        "send should appear in history"
    );
}

#[tokio::test]
#[ignore]
async fn sol_live_send_usdc_token() {
    let mnemonic = match funded_mnemonic() {
        Some(m) => m,
        None => {
            println!("SKIP: SOL_TEST_MNEMONIC not set");
            return;
        }
    };
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = SolProvider::new(
        data_dir,
        Arc::new(StorageBackend::Redb(RedbStore::new(data_dir))),
    );

    let w = provider
        .create_wallet(&WalletCreateRequest {
            label: "funded-tok".to_string(),
            mint_url: None,
            rpc_endpoints: vec![DEVNET_RPC.to_string()],
            chain_id: None,
            mnemonic_secret: Some(mnemonic.clone()),
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

    let bal = provider.balance(&w.id).await.unwrap();
    println!("wallet {} balance: {:?}", w.id, bal);

    // Check USDC token balance
    let usdc_balance = bal.additional.get("usdc_base_units").copied().unwrap_or(0);
    if usdc_balance < 1000 || bal.confirmed < 5_000_000 {
        println!(
            "SKIP: need >= 1000 USDC base units and >= 5000000 lamports (for ATA rent). \
             Got: usdc={usdc_balance}, lamports={}. Address: {}",
            bal.confirmed, w.address
        );
        return;
    }

    // Send 100 USDC base units (= 0.0001 USDC) via token flag
    let result = provider
        .send(
            &w.id,
            &format!("solana:{DEVNET_SINK}?amount-lamports=100&token=usdc"),
            Some("test-usdc"),
            None,
        )
        .await
        .unwrap();

    assert!(!result.transaction_id.is_empty());
    assert_eq!(result.amount.token, "token-units");
}
