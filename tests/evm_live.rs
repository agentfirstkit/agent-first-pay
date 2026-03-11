//! Integration tests for EvmProvider against Base Sepolia testnet.
//!
//! Read-only tests (create, balance, list) need no funds — just a working RPC endpoint.
//! Send tests require testnet ETH in the wallet.
//!
//! Run read-only tests:
//!   EVM_TEST_RPC="https://sepolia.base.org" cargo test --features evm -- --ignored evm_live
//!
//! Run send tests (requires funded wallet):
//!   EVM_TEST_RPC="https://sepolia.base.org" \
//!   EVM_TEST_MNEMONIC="<12 words>" \
//!     cargo test --features evm -- --ignored evm_live_send

#![cfg(feature = "evm")]

use agent_first_pay::provider::evm::EvmProvider;
use agent_first_pay::provider::{PayError, PayProvider};
use agent_first_pay::types::{Network, WalletCreateRequest};

const BASE_SEPOLIA_CHAIN_ID: u64 = 84532;

fn rpc_endpoint() -> String {
    std::env::var("EVM_TEST_RPC").unwrap_or_else(|_| "https://sepolia.base.org".to_string())
}

#[tokio::test]
#[ignore]
async fn evm_live_create_and_list() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = EvmProvider::new(data_dir);

    let w = provider
        .create_wallet(&WalletCreateRequest {
            label: "sepolia-test".to_string(),
            mint_url: None,
            rpc_endpoints: vec![rpc_endpoint()],
            chain_id: Some(BASE_SEPOLIA_CHAIN_ID),
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
    assert_eq!(w.network, Network::Evm);
    assert!(
        w.address.starts_with("0x"),
        "eth address format: {}",
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
async fn evm_live_balance_new_wallet() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = EvmProvider::new(data_dir);

    let w = provider
        .create_wallet(&WalletCreateRequest {
            label: "sepolia-bal".to_string(),
            mint_url: None,
            rpc_endpoints: vec![rpc_endpoint()],
            chain_id: Some(BASE_SEPOLIA_CHAIN_ID),
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
    assert_eq!(bal.confirmed, 0, "new testnet wallet should have 0 gwei");
    assert_eq!(bal.unit, "gwei");
    // Token balances should also be 0 (or absent) for new wallet
    println!("balance additional: {:?}", bal.additional);
}

#[tokio::test]
#[ignore]
async fn evm_live_balance_all() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = EvmProvider::new(data_dir);

    let w = provider
        .create_wallet(&WalletCreateRequest {
            label: "sepolia-all".to_string(),
            mint_url: None,
            rpc_endpoints: vec![rpc_endpoint()],
            chain_id: Some(BASE_SEPOLIA_CHAIN_ID),
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
async fn evm_live_receive_info() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = EvmProvider::new(data_dir);

    let w = provider
        .create_wallet(&WalletCreateRequest {
            label: "sepolia-dep".to_string(),
            mint_url: None,
            rpc_endpoints: vec![rpc_endpoint()],
            chain_id: Some(BASE_SEPOLIA_CHAIN_ID),
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
    assert!(dep.address.is_some());
    assert_eq!(dep.address.unwrap(), w.address);
}

#[tokio::test]
#[ignore]
async fn evm_live_wallet_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = EvmProvider::new(data_dir);

    let err = provider.balance("w_nonexist").await.unwrap_err();
    assert!(
        matches!(err, PayError::WalletNotFound(_)),
        "expected WalletNotFound, got: {err}"
    );
}

#[tokio::test]
#[ignore]
async fn evm_live_close_empty_wallet() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = EvmProvider::new(data_dir);

    let w = provider
        .create_wallet(&WalletCreateRequest {
            label: "sepolia-close".to_string(),
            mint_url: None,
            rpc_endpoints: vec![rpc_endpoint()],
            chain_id: Some(BASE_SEPOLIA_CHAIN_ID),
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

    provider.close_wallet(&w.id).await.unwrap();

    let list = provider.list_wallets().await.unwrap();
    assert!(
        !list.iter().any(|ws| ws.id == w.id),
        "closed wallet should not appear in list"
    );
}

#[tokio::test]
#[ignore]
async fn evm_live_history_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = EvmProvider::new(data_dir);

    let w = provider
        .create_wallet(&WalletCreateRequest {
            label: "sepolia-hist".to_string(),
            mint_url: None,
            rpc_endpoints: vec![rpc_endpoint()],
            chain_id: Some(BASE_SEPOLIA_CHAIN_ID),
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

fn funded_mnemonic() -> Option<String> {
    std::env::var("EVM_TEST_MNEMONIC").ok()
}

/// Burn address for testnet sends
const TESTNET_SINK: &str = "0x000000000000000000000000000000000000dEaD";

#[tokio::test]
#[ignore]
async fn evm_live_send_native() {
    let mnemonic = match funded_mnemonic() {
        Some(m) => m,
        None => {
            println!("SKIP: EVM_TEST_MNEMONIC not set");
            return;
        }
    };
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = EvmProvider::new(data_dir);

    let w = provider
        .create_wallet(&WalletCreateRequest {
            label: "funded".to_string(),
            mint_url: None,
            rpc_endpoints: vec![rpc_endpoint()],
            chain_id: Some(BASE_SEPOLIA_CHAIN_ID),
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
    if bal.confirmed < 100 {
        println!(
            "SKIP: wallet {} has {} gwei, need >= 100. Get testnet ETH at https://www.alchemy.com/faucets/base-sepolia. Address: {}",
            w.id, bal.confirmed, w.address
        );
        return;
    }

    // Send 1 gwei native ETH
    let result = provider
        .send(
            &w.id,
            &format!("ethereum:{TESTNET_SINK}?amount-gwei=1"),
            Some("test-native"),
            None,
        )
        .await
        .unwrap();

    assert!(!result.transaction_id.is_empty());
    assert_eq!(result.amount.token, "gwei");
    println!("tx: {}", result.transaction_id);
}

#[tokio::test]
#[ignore]
async fn evm_live_send_usdc_token() {
    let mnemonic = match funded_mnemonic() {
        Some(m) => m,
        None => {
            println!("SKIP: EVM_TEST_MNEMONIC not set");
            return;
        }
    };
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_str().unwrap();
    let provider = EvmProvider::new(data_dir);

    let w = provider
        .create_wallet(&WalletCreateRequest {
            label: "funded-tok".to_string(),
            mint_url: None,
            rpc_endpoints: vec![rpc_endpoint()],
            chain_id: Some(BASE_SEPOLIA_CHAIN_ID),
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

    let usdc_balance = bal.additional.get("usdc_base_units").copied().unwrap_or(0);
    if usdc_balance < 1000 || bal.confirmed < 100 {
        println!(
            "SKIP: need >= 1000 USDC base units and >= 100 gwei. \
             Got: usdc={usdc_balance}, gwei={}. Address: {}",
            bal.confirmed, w.address
        );
        return;
    }

    // Send 100 USDC base units (= 0.0001 USDC)
    let result = provider
        .send(
            &w.id,
            &format!("ethereum:{TESTNET_SINK}?amount-wei=100&token=usdc"),
            Some("test-usdc"),
            None,
        )
        .await
        .unwrap();

    assert!(!result.transaction_id.is_empty());
    assert_eq!(result.amount.token, "token-units");
    println!("tx: {}", result.transaction_id);
}
