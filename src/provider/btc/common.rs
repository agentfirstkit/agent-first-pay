use crate::provider::PayError;
use crate::store::wallet::{self, WalletMetadata};
use crate::types::*;
use bdk_wallet::bitcoin::bip32::Xpriv;
use bdk_wallet::bitcoin::Network as BtcNetwork;
use bdk_wallet::chain::Merge;
use bdk_wallet::keys::bip39::Mnemonic;
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
        .map_err(|e| PayError::InternalError(format!("invalid mnemonic: {e}")))?;
    let seed = mnemonic.to_seed("");
    let xprv = Xpriv::new_master(btc_network, &seed)
        .map_err(|e| PayError::InternalError(format!("derive master key: {e}")))?;

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
        .ok_or_else(|| PayError::InternalError("wallet has no seed_secret".to_string()))?;

    let btc_net = btc_network_for_meta(meta);
    let addr_type = meta.btc_address_type.as_deref().unwrap_or("taproot");
    let (external, internal) = descriptors_from_mnemonic(seed_secret, btc_net, addr_type)?;

    let changeset_path = wallet::wallet_data_directory_path_for_wallet_metadata(data_dir, meta)
        .join("bdk_changeset.json");

    if changeset_path.exists() {
        if let Ok(raw) = std::fs::read_to_string(&changeset_path) {
            if let Ok(changeset) = serde_json::from_str::<bdk_wallet::ChangeSet>(&raw) {
                if let Ok(Some(wallet)) = Wallet::load()
                    .descriptor(KeychainKind::External, Some(external.clone()))
                    .descriptor(KeychainKind::Internal, Some(internal.clone()))
                    .extract_keys()
                    .load_wallet_no_persist(changeset)
                {
                    return Ok(wallet);
                }
            }
        }
    }

    Wallet::create(external, internal)
        .network(btc_net)
        .create_wallet_no_persist()
        .map_err(|e| PayError::InternalError(format!("create bdk wallet: {e}")))
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
            .map_err(|e| PayError::InternalError(format!("read changeset: {e}")))?;
        serde_json::from_str::<bdk_wallet::ChangeSet>(&raw).unwrap_or_default()
    } else {
        bdk_wallet::ChangeSet::default()
    };

    full_changeset.merge(wallet.take_staged().unwrap_or_default());

    let json = serde_json::to_string(&full_changeset)
        .map_err(|e| PayError::InternalError(format!("serialize changeset: {e}")))?;
    std::fs::write(&changeset_path, json)
        .map_err(|e| PayError::InternalError(format!("write changeset: {e}")))?;
    Ok(())
}

pub(crate) fn wallet_address(meta: &WalletMetadata) -> Result<String, PayError> {
    let seed_secret = meta
        .seed_secret
        .as_deref()
        .ok_or_else(|| PayError::InternalError("wallet has no seed_secret".to_string()))?;
    let btc_net = btc_network_for_meta(meta);
    let addr_type = meta.btc_address_type.as_deref().unwrap_or("taproot");
    let (external, internal) = descriptors_from_mnemonic(seed_secret, btc_net, addr_type)?;
    let wallet = Wallet::create(external, internal)
        .network(btc_net)
        .create_wallet_no_persist()
        .map_err(|e| PayError::InternalError(format!("derive address: {e}")))?;
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
                amount_sats =
                    Some(val.parse::<u64>().map_err(|e| {
                        PayError::InvalidAmount(format!("invalid amount in URI: {e}"))
                    })?);
            }
        }
    }

    let amount_sats = amount_sats.ok_or_else(|| {
        PayError::InvalidAmount(
            "amount is required; use bitcoin:<addr>?amount=<sats> or <addr>?amount=<sats>"
                .to_string(),
        )
    })?;

    Ok(BtcTransferTarget {
        address: addr_str.to_string(),
        amount_sats,
    })
}
