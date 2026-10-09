use crate::provider::PayError;
use crate::store::wallet::{self, WalletMetadata};
use crate::types::*;
use bdk_wallet::bitcoin::Network as BtcNetwork;
use bdk_wallet::bitcoin::bip32::Xpriv;
use bdk_wallet::chain::Merge;
use bdk_wallet::descriptor::IntoWalletDescriptor;
use bdk_wallet::keys::bip39::Mnemonic;
use bdk_wallet::signer::SignersContainer;
use bdk_wallet::{KeychainKind, Wallet};

pub(crate) fn btc_network_for_meta(meta: &WalletMetadata) -> BtcNetwork {
    match meta.btc_network.as_deref() {
        Some("signet") => BtcNetwork::Signet,
        _ => BtcNetwork::Bitcoin,
    }
}

pub(crate) fn descriptors_from_mnemonic(
    mnemonic_str: &str,
    btc_network: BtcNetwork,
    address_type: &str,
) -> Result<(String, String), PayError> {
    let mnemonic = Mnemonic::parse(mnemonic_str)
        .map_err(|e| PayError::internal_error(format!("invalid mnemonic: {e}")))?;
    let seed = mnemonic.to_seed("");
    let xprv = Xpriv::new_master(btc_network, &seed)
        .map_err(|e| PayError::internal_error(format!("derive master key: {e}")))?;

    let coin_type = match btc_network {
        BtcNetwork::Bitcoin => 0,
        _ => 1,
    };

    let (external, internal) = match address_type {
        "segwit" => (
            format!("wpkh({xprv}/84'/{coin_type}'/0'/0/*)"),
            format!("wpkh({xprv}/84'/{coin_type}'/0'/1/*)"),
        ),
        _ => (
            format!("tr({xprv}/86'/{coin_type}'/0'/0/*)"),
            format!("tr({xprv}/86'/{coin_type}'/0'/1/*)"),
        ),
    };
    Ok((external, internal))
}

pub(crate) fn open_bdk_wallet_with_dir(
    data_dir: &str,
    meta: &WalletMetadata,
) -> Result<Wallet, PayError> {
    let seed_secret = meta
        .seed_secret
        .as_deref()
        .ok_or_else(|| PayError::internal_error("wallet has no seed_secret".to_string()))?;

    let btc_net = btc_network_for_meta(meta);
    let addr_type = meta.btc_address_type.as_deref().unwrap_or("taproot");
    let (external, internal) = descriptors_from_mnemonic(seed_secret, btc_net, addr_type)?;

    let changeset_path = wallet::wallet_data_directory_path_for_wallet_metadata(data_dir, meta)
        .join("bdk_changeset.json");

    if changeset_path.exists() {
        let raw = std::fs::read_to_string(&changeset_path).map_err(|error| {
            PayError::internal_error(format!(
                "read BDK changeset: {error}; use restore to rebuild the wallet"
            ))
        })?;
        let changeset = serde_json::from_str::<bdk_wallet::ChangeSet>(&raw).map_err(|error| {
            PayError::internal_error(format!(
                "invalid BDK changeset: {error}; use restore to rebuild the wallet"
            ))
        })?;
        return Wallet::load()
            .descriptor(KeychainKind::External, Some(external))
            .descriptor(KeychainKind::Internal, Some(internal))
            .load_wallet_no_persist(changeset)
            .map_err(|error| {
                PayError::internal_error(format!(
                    "load BDK changeset: {error}; use restore to rebuild the wallet"
                ))
            })?
            .ok_or_else(|| {
                PayError::internal_error(
                    "BDK changeset has no wallet; use restore to rebuild the wallet".to_owned(),
                )
            });
    }

    Wallet::create(external, internal)
        .network(btc_net)
        .create_wallet_no_persist()
        .map_err(|e| PayError::internal_error(format!("create bdk wallet: {e}")))
}

/// The signers for a wallet's two keychains.
///
/// bdk_wallet 3.2 phased the `Wallet` out as a key store: a loaded wallet holds only
/// public descriptors, and signing takes the signers from its caller. They are derived
/// here from the same mnemonic the wallet was built from, so the private keys exist in
/// exactly one place — the descriptors this module writes — and for exactly the one
/// call that needs them.
pub(crate) fn signers_for_wallet(
    meta: &WalletMetadata,
    wallet: &Wallet,
) -> Result<[SignersContainer; 2], PayError> {
    let seed_secret = meta
        .seed_secret
        .as_deref()
        .ok_or_else(|| PayError::internal_error("wallet has no seed_secret".to_string()))?;

    let btc_net = btc_network_for_meta(meta);
    let addr_type = meta.btc_address_type.as_deref().unwrap_or("taproot");
    let (external, internal) = descriptors_from_mnemonic(seed_secret, btc_net, addr_type)?;
    let secp = wallet.secp_ctx();

    let signers_for = |keychain: KeychainKind, descriptor: String| {
        let (_, keymap) = descriptor
            .into_wallet_descriptor(secp, btc_net.into())
            .map_err(|e| PayError::internal_error(format!("parse signing descriptor: {e}")))?;
        Ok(SignersContainer::build(
            keymap,
            wallet.public_descriptor(keychain),
            secp,
        ))
    };

    Ok([
        signers_for(KeychainKind::External, external)?,
        signers_for(KeychainKind::Internal, internal)?,
    ])
}

pub(crate) fn persist_changeset(
    data_dir: &str,
    meta: &WalletMetadata,
    wallet: &mut Wallet,
) -> Result<(), PayError> {
    let staged = wallet.staged();
    if staged.is_none() {
        return Ok(());
    }
    let changeset_path = wallet::wallet_data_directory_path_for_wallet_metadata(data_dir, meta)
        .join("bdk_changeset.json");

    let mut full_changeset = if changeset_path.exists() {
        let raw = std::fs::read_to_string(&changeset_path)
            .map_err(|e| PayError::internal_error(format!("read changeset: {e}")))?;
        serde_json::from_str::<bdk_wallet::ChangeSet>(&raw).map_err(|error| {
            PayError::internal_error(format!(
                "invalid BDK changeset: {error}; use restore to rebuild the wallet"
            ))
        })?
    } else {
        bdk_wallet::ChangeSet::default()
    };

    full_changeset.merge(staged.cloned().unwrap_or_default());

    let json = serde_json::to_string(&full_changeset)
        .map_err(|e| PayError::internal_error(format!("serialize changeset: {e}")))?;
    crate::store::plan::write_private_file(&changeset_path, json.as_bytes())?;
    // Keep staged changes available when publication fails.
    let _ = wallet.take_staged();
    Ok(())
}

pub(crate) fn wallet_address(meta: &WalletMetadata) -> Result<String, PayError> {
    let seed_secret = meta
        .seed_secret
        .as_deref()
        .ok_or_else(|| PayError::internal_error("wallet has no seed_secret".to_string()))?;
    let btc_net = btc_network_for_meta(meta);
    let addr_type = meta.btc_address_type.as_deref().unwrap_or("taproot");
    let (external, internal) = descriptors_from_mnemonic(seed_secret, btc_net, addr_type)?;
    let wallet = Wallet::create(external, internal)
        .network(btc_net)
        .create_wallet_no_persist()
        .map_err(|e| PayError::internal_error(format!("derive address: {e}")))?;
    let addr_info = wallet.peek_address(KeychainKind::External, 0);
    Ok(addr_info.address.to_string())
}

pub(crate) fn btc_wallet_summary(meta: WalletMetadata, address: String) -> WalletSummary {
    WalletSummary {
        id: meta.id,
        network: Network::Btc,
        label: meta.label,
        address,
        backend: meta.backend.clone(),
        mint_url: None,
        rpc_endpoints: None,
        chain_id: None,
        created_at_epoch_s: meta.created_at_epoch_s,
    }
}

#[derive(Debug)]
pub(crate) struct BtcTransferTarget {
    pub address: String,
    pub amount_sats: u64,
}

pub(crate) fn parse_transfer_target(to: &str) -> Result<BtcTransferTarget, PayError> {
    let stripped = to.strip_prefix("bitcoin:").unwrap_or(to);

    let (addr_str, query) = if let Some(idx) = stripped.find('?') {
        (&stripped[..idx], Some(&stripped[idx + 1..]))
    } else {
        (stripped, None)
    };

    let mut amount_sats: Option<u64> = None;
    if let Some(q) = query {
        for pair in q.split('&') {
            if let Some(val) = pair.strip_prefix("amount=") {
                amount_sats = Some(val.parse::<u64>().map_err(|e| {
                    PayError::invalid_amount(format!("invalid amount in URI: {e}"))
                })?);
            }
        }
    }

    let amount_sats = amount_sats.ok_or_else(|| {
        PayError::invalid_amount(
            "amount is required; use bitcoin:<addr>?amount=<sats> or <addr>?amount=<sats>"
                .to_string(),
        )
    })?;

    Ok(BtcTransferTarget {
        address: addr_str.to_string(),
        amount_sats,
    })
}

#[cfg(all(test, feature = "redb", feature = "btc-esplora"))]
mod tests {
    use super::*;
    use crate::provider::{PayProvider, btc::BtcProvider};
    use crate::store::{PayStore, StorageBackend, redb_store::RedbStore};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[allow(clippy::unwrap_used)]
    async fn fixture() -> (tempfile::TempDir, BtcProvider, WalletMetadata) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_str().unwrap();
        let store = Arc::new(StorageBackend::Redb(RedbStore::new(path)));
        let provider = BtcProvider::new(path, store.clone());
        let request: WalletCreateRequest = serde_json::from_value(serde_json::json!({
            "label":"reviewer", "btc_network":"signet", "btc_esplora_url":"http://127.0.0.1:0"
        }))
        .unwrap();
        let wallet = provider.create_wallet(&request).await.unwrap();
        let meta = store.load_wallet_metadata(&wallet.id).unwrap();
        (directory, provider, meta)
    }

    #[tokio::test]
    async fn btc_sync_failure_before_signing_is_known_not_submitted() {
        let (_directory, provider, meta) = fixture().await;
        let address = wallet_address(&meta).unwrap();
        let error = provider
            .send(
                &meta.id,
                &format!("bitcoin:{address}?amount=1000"),
                None,
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, PayError::NetworkNotSubmitted { .. }));
        assert_eq!(error.error_code(), "network_error");
        assert!(error.retryable());
        assert!(error.submission_transaction_id().is_none());
    }

    #[tokio::test]
    async fn btc_changeset_corruption_refuses_balance_and_preserves_the_file_and_staging() {
        let (directory, provider, meta) = fixture().await;
        let path = directory.path().to_str().unwrap();
        let changeset_path = wallet::wallet_data_directory_path_for_wallet_metadata(path, &meta)
            .join("bdk_changeset.json");
        let original = std::fs::read(&changeset_path).unwrap();
        let mut wallet = open_bdk_wallet_with_dir(path, &meta).unwrap();
        wallet.reveal_next_address(KeychainKind::External);
        let truncated = b"{\"descriptor\":";
        std::fs::write(&changeset_path, truncated).unwrap();
        let error = provider.balance(&meta.id).await.unwrap_err();
        assert_eq!(error.error_code(), "internal_error");
        assert!(error.to_string().contains("restore"));
        assert!(persist_changeset(path, &meta, &mut wallet).is_err());
        assert!(wallet.staged().is_some());
        assert_eq!(std::fs::read(&changeset_path).unwrap(), truncated);
        std::fs::write(&changeset_path, original).unwrap();
        persist_changeset(path, &meta, &mut wallet).unwrap();
        assert!(wallet.staged().is_none());
        assert!(open_bdk_wallet_with_dir(path, &meta).is_ok());
    }

    #[tokio::test]
    async fn btc_changeset_atomic_replacement_never_exposes_partial_json() {
        let (directory, _provider, meta) = fixture().await;
        let path = directory.path().to_str().unwrap();
        let changeset_path = wallet::wallet_data_directory_path_for_wallet_metadata(path, &meta)
            .join("bdk_changeset.json");
        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = stop.clone();
        let (ready, observed) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut reads = 0;
            loop {
                let bytes = std::fs::read(&changeset_path).unwrap();
                serde_json::from_slice::<bdk_wallet::ChangeSet>(&bytes).unwrap();
                reads += 1;
                if reads == 1 {
                    ready.send(()).unwrap();
                }
                if reader_stop.load(Ordering::SeqCst) {
                    break;
                }
            }
            reads
        });
        observed.recv().unwrap();
        let mut wallet = open_bdk_wallet_with_dir(path, &meta).unwrap();
        for _ in 0..32 {
            wallet.reveal_next_address(KeychainKind::External);
            persist_changeset(path, &meta, &mut wallet).unwrap();
        }
        stop.store(true, Ordering::SeqCst);
        assert!(reader.join().unwrap() > 1);
        assert!(open_bdk_wallet_with_dir(path, &meta).is_ok());
    }
}
