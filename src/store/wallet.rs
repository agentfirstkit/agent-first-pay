use crate::provider::PayError;
use crate::types::Network;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ═══════════════════════════════════════════
// Shared types (always available)
// ═══════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomToken {
    pub symbol: String,
    pub address: String,
    pub decimals: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletMetadata {
    pub id: String,
    pub network: Network,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mint_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sol_rpc_endpoints: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evm_rpc_endpoints: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evm_chain_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub btc_esplora_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub btc_network: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub btc_address_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub btc_core_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub btc_core_auth_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub btc_electrum_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_tokens: Option<Vec<CustomToken>>,
    #[serde(default)]
    pub created_at_epoch_s: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ═══════════════════════════════════════════
// Shared helpers (always available)
// ═══════════════════════════════════════════

pub fn generate_wallet_identifier() -> Result<String, PayError> {
    let mut buf = [0u8; 4];
    getrandom::fill(&mut buf).map_err(|e| PayError::InternalError(format!("rng failed: {e}")))?;
    Ok(format!("w_{}", hex::encode(buf)))
}

pub fn generate_transaction_identifier() -> Result<String, PayError> {
    let mut buf = [0u8; 8];
    getrandom::fill(&mut buf).map_err(|e| PayError::InternalError(format!("rng failed: {e}")))?;
    Ok(format!("tx_{}", hex::encode(buf)))
}

pub fn now_epoch_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn wallet_data_directory_path_for_wallet_metadata(
    data_dir: &str,
    wallet_metadata: &WalletMetadata,
) -> PathBuf {
    provider_root_path_for_wallet_metadata(data_dir, wallet_metadata)
        .join(&wallet_metadata.id)
        .join("wallet-data")
}

pub fn wallet_directory_path(data_dir: &str, wallet_id: &str) -> Result<PathBuf, PayError> {
    find_wallet_dir(data_dir, wallet_id)?
        .ok_or_else(|| PayError::WalletNotFound(format!("wallet {wallet_id} not found")))
}

pub fn wallet_data_directory_path(data_dir: &str, wallet_id: &str) -> Result<PathBuf, PayError> {
    Ok(wallet_directory_path(data_dir, wallet_id)?.join("wallet-data"))
}

pub(crate) fn parse_wallet_metadata(
    raw: &str,
    wallet_id: &str,
) -> Result<WalletMetadata, PayError> {
    serde_json::from_str(raw)
        .map_err(|e| PayError::InternalError(format!("parse wallet {wallet_id}: {e}")))
}

// ═══════════════════════════════════════════
// Path / filesystem helpers (always available)
// ═══════════════════════════════════════════

pub(crate) fn provider_root_path_for_wallet_metadata(
    data_dir: &str,
    wallet_metadata: &WalletMetadata,
) -> PathBuf {
    Path::new(data_dir).join(provider_directory_name_for_wallet_metadata(wallet_metadata))
}

fn provider_directory_name_for_wallet_metadata(wallet_metadata: &WalletMetadata) -> String {
    match wallet_metadata.network {
        Network::Cashu => "wallets-cashu".to_string(),
        Network::Ln => {
            let backend = wallet_metadata
                .backend
                .as_deref()
                .unwrap_or("default")
                .to_ascii_lowercase();
            format!("wallets-ln-{backend}")
        }
        Network::Sol => "wallets-sol".to_string(),
        Network::Evm => "wallets-evm".to_string(),
        Network::Btc => "wallets-btc".to_string(),
    }
}

pub(crate) fn network_from_provider_dir(name: &str) -> Option<Network> {
    if name == "wallets-cashu" {
        Some(Network::Cashu)
    } else if name.starts_with("wallets-ln-") {
        Some(Network::Ln)
    } else if name == "wallets-sol" || name.starts_with("wallets-sol-") {
        Some(Network::Sol)
    } else if name == "wallets-evm" || name.starts_with("wallets-evm-") {
        Some(Network::Evm)
    } else if name == "wallets-btc" || name.starts_with("wallets-btc-") {
        Some(Network::Btc)
    } else {
        None
    }
}

fn provider_dir_matches_network(name: &str, network: Network) -> bool {
    match network {
        Network::Cashu => name == "wallets-cashu",
        Network::Ln => name.starts_with("wallets-ln-"),
        Network::Sol => name == "wallets-sol" || name.starts_with("wallets-sol-"),
        Network::Evm => name == "wallets-evm" || name.starts_with("wallets-evm-"),
        Network::Btc => name == "wallets-btc" || name.starts_with("wallets-btc-"),
    }
}

fn provider_dir_supported(name: &str) -> bool {
    name == "wallets-cashu"
        || name == "wallets-sol"
        || name == "wallets-evm"
        || name == "wallets-btc"
        || name.starts_with("wallets-ln-")
        || name.starts_with("wallets-sol-")
        || name.starts_with("wallets-evm-")
        || name.starts_with("wallets-btc-")
}

pub(crate) fn provider_roots(
    data_dir: &str,
    network: Option<Network>,
) -> Result<Vec<PathBuf>, PayError> {
    let root = Path::new(data_dir);
    if !root.exists() {
        return Ok(vec![]);
    }

    let mut roots = Vec::new();
    for entry in std::fs::read_dir(root)
        .map_err(|e| PayError::InternalError(format!("read data_dir {}: {e}", root.display())))?
    {
        let entry = entry.map_err(|e| PayError::InternalError(format!("read dir entry: {e}")))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !provider_dir_supported(name) {
            continue;
        }
        if let Some(network) = network {
            if !provider_dir_matches_network(name, network) {
                continue;
            }
        }
        roots.push(path);
    }
    roots.sort();
    Ok(roots)
}

pub(crate) fn find_wallet_dir(
    data_dir: &str,
    wallet_id: &str,
) -> Result<Option<PathBuf>, PayError> {
    for root in provider_roots(data_dir, None)? {
        let dir = root.join(wallet_id);
        if dir.is_dir() {
            return Ok(Some(dir));
        }
    }
    Ok(None)
}

// ═══════════════════════════════════════════
// Redb-specific functions
// ═══════════════════════════════════════════

#[cfg(feature = "redb")]
use crate::store::db;
#[cfg(feature = "redb")]
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

#[cfg(feature = "redb")]
const CATALOG_WALLET_BY_ID: TableDefinition<&str, &str> = TableDefinition::new("wallet_by_id");
#[cfg(feature = "redb")]
const CORE_METADATA_KEY_VALUE: TableDefinition<&str, &str> = TableDefinition::new("metadata_kv");
#[cfg(feature = "redb")]
const CORE_WALLET_METADATA_KEY: &str = "wallet_metadata";

#[cfg(feature = "redb")]
pub fn save_wallet_metadata(
    data_dir: &str,
    wallet_metadata: &WalletMetadata,
) -> Result<(), PayError> {
    let provider_root = provider_root_path_for_wallet_metadata(data_dir, wallet_metadata);
    std::fs::create_dir_all(&provider_root).map_err(|e| {
        PayError::InternalError(format!(
            "create provider wallet dir {}: {e}",
            provider_root.display()
        ))
    })?;

    let wallet_dir = provider_root.join(&wallet_metadata.id);
    let wallet_data_dir = wallet_dir.join("wallet-data");
    std::fs::create_dir_all(&wallet_data_dir).map_err(|e| {
        PayError::InternalError(format!(
            "create wallet dir {}: {e}",
            wallet_data_dir.display()
        ))
    })?;

    let wallet_metadata_json = serde_json::to_string(wallet_metadata)
        .map_err(|e| PayError::InternalError(format!("serialize wallet metadata: {e}")))?;

    // catalog.redb: provider-level wallet index
    let catalog_db = open_catalog(&provider_root)?;
    let catalog_txn = catalog_db
        .begin_write()
        .map_err(|e| PayError::InternalError(format!("catalog begin_write: {e}")))?;
    {
        let mut table = catalog_txn
            .open_table(CATALOG_WALLET_BY_ID)
            .map_err(|e| PayError::InternalError(format!("catalog open wallet_by_id: {e}")))?;
        table
            .insert(wallet_metadata.id.as_str(), wallet_metadata_json.as_str())
            .map_err(|e| PayError::InternalError(format!("catalog insert wallet: {e}")))?;
    }
    catalog_txn
        .commit()
        .map_err(|e| PayError::InternalError(format!("catalog commit: {e}")))?;

    // core.redb: per-wallet authoritative metadata
    let core_db = open_core(&wallet_dir.join("core.redb"))?;
    let core_txn = core_db
        .begin_write()
        .map_err(|e| PayError::InternalError(format!("core begin_write: {e}")))?;
    {
        let mut table = core_txn
            .open_table(CORE_METADATA_KEY_VALUE)
            .map_err(|e| PayError::InternalError(format!("core open metadata_kv: {e}")))?;
        table
            .insert(CORE_WALLET_METADATA_KEY, wallet_metadata_json.as_str())
            .map_err(|e| PayError::InternalError(format!("core write wallet metadata: {e}")))?;
    }
    core_txn
        .commit()
        .map_err(|e| PayError::InternalError(format!("core commit wallet metadata: {e}")))?;

    Ok(())
}

#[cfg(feature = "redb")]
pub fn load_wallet_metadata(data_dir: &str, wallet_id: &str) -> Result<WalletMetadata, PayError> {
    // Fast path: provider catalogs
    for provider_root in provider_roots(data_dir, None)? {
        let catalog_path = provider_root.join("catalog.redb");
        if !catalog_path.exists() {
            continue;
        }
        let db = open_catalog(&provider_root)?;
        let read_txn = db
            .begin_read()
            .map_err(|e| PayError::InternalError(format!("catalog begin_read: {e}")))?;
        let Ok(table) = read_txn.open_table(CATALOG_WALLET_BY_ID) else {
            continue;
        };
        if let Some(value) = table
            .get(wallet_id)
            .map_err(|e| PayError::InternalError(format!("catalog read wallet {wallet_id}: {e}")))?
        {
            return parse_wallet_metadata(value.value(), wallet_id);
        }
    }

    // Fallback: wallet core metadata
    if let Some(dir) = find_wallet_dir(data_dir, wallet_id)? {
        let core_path = dir.join("core.redb");
        if core_path.exists() {
            let db = db::open_database(&core_path)?;
            let read_txn = db
                .begin_read()
                .map_err(|e| PayError::InternalError(format!("core begin_read: {e}")))?;
            let Ok(table) = read_txn.open_table(CORE_METADATA_KEY_VALUE) else {
                return Err(PayError::WalletNotFound(format!(
                    "wallet {wallet_id} not found"
                )));
            };
            if let Some(value) = table
                .get(CORE_WALLET_METADATA_KEY)
                .map_err(|e| PayError::InternalError(format!("core read wallet metadata: {e}")))?
            {
                return parse_wallet_metadata(value.value(), wallet_id);
            }
        }
    }

    // Label fallback: if wallet_id doesn't start with "w_", try matching by label
    if !wallet_id.starts_with("w_") {
        for provider_root in provider_roots(data_dir, None)? {
            let catalog_path = provider_root.join("catalog.redb");
            if !catalog_path.exists() {
                continue;
            }
            let db = open_catalog(&provider_root)?;
            let read_txn = db
                .begin_read()
                .map_err(|e| PayError::InternalError(format!("catalog begin_read: {e}")))?;
            let Ok(table) = read_txn.open_table(CATALOG_WALLET_BY_ID) else {
                continue;
            };
            for entry in table
                .iter()
                .map_err(|e| PayError::InternalError(format!("catalog iterate: {e}")))?
            {
                let (key, value) = entry
                    .map_err(|e| PayError::InternalError(format!("catalog read entry: {e}")))?;
                if let Ok(meta) = parse_wallet_metadata(value.value(), key.value()) {
                    if meta.label.as_deref() == Some(wallet_id) {
                        return Ok(meta);
                    }
                }
            }
        }
    }

    Err(PayError::WalletNotFound(format!(
        "wallet {wallet_id} not found"
    )))
}

#[cfg(feature = "redb")]
pub fn list_wallet_metadata(
    data_dir: &str,
    network: Option<Network>,
) -> Result<Vec<WalletMetadata>, PayError> {
    let mut wallets = Vec::new();

    for provider_root in provider_roots(data_dir, network)? {
        let catalog_path = provider_root.join("catalog.redb");
        if !catalog_path.exists() {
            continue;
        }
        let dir_network = provider_root
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(network_from_provider_dir);
        let db = open_catalog(&provider_root)?;
        let read_txn = db
            .begin_read()
            .map_err(|e| PayError::InternalError(format!("catalog begin_read: {e}")))?;
        let Ok(table) = read_txn.open_table(CATALOG_WALLET_BY_ID) else {
            continue;
        };

        for entry in table
            .iter()
            .map_err(|e| PayError::InternalError(format!("catalog iterate wallets: {e}")))?
        {
            let (key, value) = entry
                .map_err(|e| PayError::InternalError(format!("catalog read wallet entry: {e}")))?;
            let wallet_metadata: WalletMetadata = match serde_json::from_str(value.value()) {
                Ok(m) => m,
                Err(e) => {
                    let Some(dn) = dir_network else { continue };
                    WalletMetadata {
                        id: key.value().to_string(),
                        network: dn,
                        label: None,
                        mint_url: None,
                        sol_rpc_endpoints: None,
                        evm_rpc_endpoints: None,
                        evm_chain_id: None,
                        seed_secret: None,
                        backend: None,
                        btc_esplora_url: None,
                        btc_network: None,
                        btc_address_type: None,
                        btc_core_url: None,
                        btc_core_auth_secret: None,
                        btc_electrum_url: None,
                        custom_tokens: None,
                        created_at_epoch_s: 0,
                        error: Some(format!("corrupt metadata: {e}")),
                    }
                }
            };
            if let Some(network) = network {
                if wallet_metadata.network != network {
                    continue;
                }
            }
            wallets.push(wallet_metadata);
        }
    }

    wallets.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(wallets)
}

#[cfg(feature = "redb")]
pub fn delete_wallet_metadata(data_dir: &str, wallet_id: &str) -> Result<(), PayError> {
    let wallet_metadata = load_wallet_metadata(data_dir, wallet_id)?;
    let provider_root = provider_root_path_for_wallet_metadata(data_dir, &wallet_metadata);

    // Remove from provider catalog
    let catalog_path = provider_root.join("catalog.redb");
    if catalog_path.exists() {
        let db = open_catalog(&provider_root)?;
        let write_txn = db
            .begin_write()
            .map_err(|e| PayError::InternalError(format!("catalog begin_write: {e}")))?;
        {
            let mut table = write_txn
                .open_table(CATALOG_WALLET_BY_ID)
                .map_err(|e| PayError::InternalError(format!("catalog open wallet_by_id: {e}")))?;
            let _ = table
                .remove(wallet_id)
                .map_err(|e| PayError::InternalError(format!("catalog remove wallet: {e}")))?;
        }
        write_txn
            .commit()
            .map_err(|e| PayError::InternalError(format!("catalog commit delete: {e}")))?;
    }

    // Remove wallet directory (core.redb + wallet-data/*)
    let wallet_dir = provider_root.join(wallet_id);
    if wallet_dir.exists() {
        std::fs::remove_dir_all(&wallet_dir)
            .map_err(|e| PayError::InternalError(format!("delete wallet dir: {e}")))?;
    }

    Ok(())
}

#[cfg(feature = "redb")]
pub fn wallet_core_database_path(data_dir: &str, wallet_id: &str) -> Result<PathBuf, PayError> {
    Ok(wallet_directory_path(data_dir, wallet_id)?.join("core.redb"))
}

#[cfg(feature = "redb")]
pub fn resolve_wallet_id(data_dir: &str, id_or_label: &str) -> Result<String, PayError> {
    if id_or_label.starts_with("w_") {
        return Ok(id_or_label.to_string());
    }
    // Search by label
    let all = list_wallet_metadata(data_dir, None)?;
    let mut matches: Vec<&WalletMetadata> = all
        .iter()
        .filter(|w| w.label.as_deref() == Some(id_or_label))
        .collect();
    match matches.len() {
        0 => Err(PayError::WalletNotFound(format!(
            "no wallet found with ID or label '{id_or_label}'"
        ))),
        1 => Ok(matches.remove(0).id.clone()),
        n => Err(PayError::InvalidAmount(format!(
            "label '{id_or_label}' matches {n} wallets — use wallet ID instead"
        ))),
    }
}

#[cfg(feature = "redb")]
const CATALOG_VERSION: u64 = 1;
#[cfg(feature = "redb")]
const CORE_VERSION: u64 = 1;

#[cfg(feature = "redb")]
fn open_catalog(provider_root: &Path) -> Result<Database, PayError> {
    let dir_name = provider_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let dir_name_owned = dir_name.to_string();

    db::open_and_migrate(
        &provider_root.join("catalog.redb"),
        CATALOG_VERSION,
        &[
            // v0 → v1: backfill `network` from provider directory name
            &|db: &Database| migrate_catalog_v0_to_v1(db, &dir_name_owned),
        ],
    )
}

#[cfg(feature = "redb")]
fn open_core(path: &Path) -> Result<Database, PayError> {
    db::open_and_migrate(
        path,
        CORE_VERSION,
        &[
            // v0 → v1: no data migration, just stamp version
            &|_db: &Database| Ok(()),
        ],
    )
}

#[cfg(feature = "redb")]
fn migrate_catalog_v0_to_v1(db: &Database, provider_dir_name: &str) -> Result<(), PayError> {
    let network = match network_from_provider_dir(provider_dir_name) {
        Some(n) => n,
        None => return Ok(()), // unknown provider dir — skip
    };
    let network_str = match network {
        Network::Cashu => "cashu",
        Network::Ln => "ln",
        Network::Sol => "sol",
        Network::Evm => "evm",
        Network::Btc => "btc",
    };

    // Collect keys needing update (can't mutate during iteration)
    let read_txn = db
        .begin_read()
        .map_err(|e| PayError::InternalError(format!("catalog migration begin_read: {e}")))?;
    let Ok(table) = read_txn.open_table(CATALOG_WALLET_BY_ID) else {
        return Ok(());
    };
    let mut updates: Vec<(String, String)> = Vec::new();
    for entry in table
        .iter()
        .map_err(|e| PayError::InternalError(format!("catalog migration iterate: {e}")))?
    {
        let (key, value) = entry
            .map_err(|e| PayError::InternalError(format!("catalog migration read entry: {e}")))?;
        let raw = value.value();
        let mut obj: serde_json::Value = serde_json::from_str(raw).map_err(|e| {
            PayError::InternalError(format!("catalog migration parse {}: {e}", key.value()))
        })?;
        if obj.get("network").is_none() {
            obj["network"] = serde_json::Value::String(network_str.to_string());
            let updated = serde_json::to_string(&obj).map_err(|e| {
                PayError::InternalError(format!("catalog migration serialize {}: {e}", key.value()))
            })?;
            updates.push((key.value().to_string(), updated));
        }
    }
    drop(table);
    drop(read_txn);

    if updates.is_empty() {
        return Ok(());
    }

    let write_txn = db
        .begin_write()
        .map_err(|e| PayError::InternalError(format!("catalog migration begin_write: {e}")))?;
    {
        let mut table = write_txn.open_table(CATALOG_WALLET_BY_ID).map_err(|e| {
            PayError::InternalError(format!("catalog migration open wallet_by_id: {e}"))
        })?;
        for (key, value) in &updates {
            table.insert(key.as_str(), value.as_str()).map_err(|e| {
                PayError::InternalError(format!("catalog migration update {key}: {e}"))
            })?;
        }
    }
    write_txn
        .commit()
        .map_err(|e| PayError::InternalError(format!("catalog migration commit: {e}")))?;

    Ok(())
}

// ═══════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_wallet_id_format() {
        let id = generate_wallet_identifier().unwrap();
        assert!(id.starts_with("w_"), "should start with w_: {id}");
        assert_eq!(id.len(), 10, "w_ + 8 hex chars = 10: {id}");
        assert!(id[2..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generate_tx_id_format() {
        let id = generate_transaction_identifier().unwrap();
        assert!(id.starts_with("tx_"), "should start with tx_: {id}");
        assert_eq!(id.len(), 19, "tx_ + 16 hex chars = 19: {id}");
        assert!(id[3..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[cfg(feature = "redb")]
    #[test]
    fn save_and_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();
        let meta = WalletMetadata {
            id: "w_aabbccdd".to_string(),
            network: Network::Cashu,
            label: Some("test wallet".to_string()),
            mint_url: Some("https://mint.example".to_string()),
            sol_rpc_endpoints: None,
            evm_rpc_endpoints: None,
            evm_chain_id: None,
            seed_secret: Some("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".to_string()),
            backend: None,
            btc_esplora_url: None,
            btc_network: None,
            btc_address_type: None,
            btc_core_url: None,
            btc_core_auth_secret: None,
            btc_electrum_url: None,
            custom_tokens: None,
            created_at_epoch_s: 1700000000,
            error: None,
        };
        save_wallet_metadata(dir, &meta).unwrap();
        let loaded = load_wallet_metadata(dir, "w_aabbccdd").unwrap();
        assert_eq!(loaded.id, meta.id);
        assert_eq!(loaded.network, Network::Cashu);
        assert_eq!(loaded.label, meta.label);
        assert_eq!(loaded.mint_url, meta.mint_url);
        assert_eq!(loaded.seed_secret, meta.seed_secret);
        assert_eq!(loaded.created_at_epoch_s, meta.created_at_epoch_s);

        let wallet_data_dir = wallet_data_directory_path(dir, "w_aabbccdd").unwrap();
        assert!(wallet_data_dir.ends_with("wallet-data"));
        assert!(wallet_data_dir.exists());
    }

    #[cfg(feature = "redb")]
    #[test]
    fn load_wallet_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();
        let err = load_wallet_metadata(dir, "w_00000000").unwrap_err();
        assert!(
            matches!(err, PayError::WalletNotFound(_)),
            "expected WalletNotFound, got: {err}"
        );
    }

    #[cfg(feature = "redb")]
    #[test]
    fn list_wallets_filter_by_network() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();

        let cashu = WalletMetadata {
            id: "w_cashu001".to_string(),
            network: Network::Cashu,
            label: None,
            mint_url: None,
            sol_rpc_endpoints: None,
            evm_rpc_endpoints: None,
            evm_chain_id: None,
            seed_secret: None,
            backend: None,
            btc_esplora_url: None,
            btc_network: None,
            btc_address_type: None,
            btc_core_url: None,
            btc_core_auth_secret: None,
            btc_electrum_url: None,
            custom_tokens: None,
            created_at_epoch_s: 1,
            error: None,
        };
        let ln = WalletMetadata {
            id: "w_ln000001".to_string(),
            network: Network::Ln,
            label: None,
            mint_url: None,
            sol_rpc_endpoints: None,
            evm_rpc_endpoints: None,
            evm_chain_id: None,
            seed_secret: None,
            backend: Some("nwc".to_string()),
            btc_esplora_url: None,
            btc_network: None,
            btc_address_type: None,
            btc_core_url: None,
            btc_core_auth_secret: None,
            btc_electrum_url: None,
            custom_tokens: None,
            created_at_epoch_s: 2,
            error: None,
        };
        save_wallet_metadata(dir, &cashu).unwrap();
        save_wallet_metadata(dir, &ln).unwrap();

        let all = list_wallet_metadata(dir, None).unwrap();
        assert_eq!(all.len(), 2);

        let only_cashu = list_wallet_metadata(dir, Some(Network::Cashu)).unwrap();
        assert_eq!(only_cashu.len(), 1);
        assert_eq!(only_cashu[0].id, "w_cashu001");

        let only_ln = list_wallet_metadata(dir, Some(Network::Ln)).unwrap();
        assert_eq!(only_ln.len(), 1);
        assert_eq!(only_ln[0].id, "w_ln000001");
    }

    #[cfg(feature = "redb")]
    #[test]
    fn list_wallets_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();
        let result = list_wallet_metadata(dir, None).unwrap();
        assert!(result.is_empty());
    }

    #[cfg(feature = "redb")]
    #[test]
    fn delete_wallet_removes_wallet_dir_and_catalog_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();
        let meta = WalletMetadata {
            id: "w_del001".to_string(),
            network: Network::Cashu,
            label: None,
            mint_url: Some("https://mint.example".to_string()),
            sol_rpc_endpoints: None,
            evm_rpc_endpoints: None,
            evm_chain_id: None,
            seed_secret: Some("seed".to_string()),
            backend: None,
            btc_esplora_url: None,
            btc_network: None,
            btc_address_type: None,
            btc_core_url: None,
            btc_core_auth_secret: None,
            btc_electrum_url: None,
            custom_tokens: None,
            created_at_epoch_s: 1,
            error: None,
        };
        save_wallet_metadata(dir, &meta).unwrap();
        let wallet_dir = wallet_directory_path(dir, &meta.id).unwrap();
        assert!(wallet_dir.exists());

        delete_wallet_metadata(dir, &meta.id).unwrap();

        assert!(load_wallet_metadata(dir, &meta.id).is_err());
        assert!(!wallet_dir.exists());
    }

    #[cfg(feature = "redb")]
    #[test]
    fn catalog_migration_v0_to_v1_backfills_network() {
        use crate::store::db;

        let tmp = tempfile::tempdir().unwrap();
        let provider_root = tmp.path().join("wallets-cashu");
        std::fs::create_dir_all(&provider_root).unwrap();

        // Create a legacy catalog.redb with an entry missing "network"
        let catalog_path = provider_root.join("catalog.redb");
        {
            let db = db::open_database(&catalog_path).unwrap();
            let w = db.begin_write().unwrap();
            {
                let mut t = w.open_table(CATALOG_WALLET_BY_ID).unwrap();
                // JSON without "network" field — simulates pre-migration data
                let legacy_json = r#"{"id":"w_legacy01","label":"old","mint_url":"https://mint.example","created_at_epoch_s":1700000000}"#;
                t.insert("w_legacy01", legacy_json).unwrap();
            }
            w.commit().unwrap();
            // No _schema table — version 0
        }

        // Open via open_catalog which triggers migration
        let db = open_catalog(&provider_root).unwrap();

        // Verify: entry now has "network": "cashu"
        let r = db.begin_read().unwrap();
        let t = r.open_table(CATALOG_WALLET_BY_ID).unwrap();
        let raw = t.get("w_legacy01").unwrap().unwrap();
        let obj: serde_json::Value = serde_json::from_str(raw.value()).unwrap();
        assert_eq!(obj["network"], "cashu");

        // Verify: schema version is 1
        assert_eq!(db::read_schema_version_pub(&db).unwrap(), 1);
        drop(t);
        drop(r);
        drop(db);

        // Reopen — should not re-migrate, version still 1
        let db2 = open_catalog(&provider_root).unwrap();
        assert_eq!(db::read_schema_version_pub(&db2).unwrap(), 1);
    }

    #[cfg(feature = "redb")]
    #[test]
    fn catalog_migration_preserves_existing_network() {
        use crate::store::db;

        let tmp = tempfile::tempdir().unwrap();
        let provider_root = tmp.path().join("wallets-cashu");
        std::fs::create_dir_all(&provider_root).unwrap();

        let catalog_path = provider_root.join("catalog.redb");
        {
            let db = db::open_database(&catalog_path).unwrap();
            let w = db.begin_write().unwrap();
            {
                let mut t = w.open_table(CATALOG_WALLET_BY_ID).unwrap();
                // JSON with "network" already present
                let json =
                    r#"{"id":"w_has_net","network":"cashu","created_at_epoch_s":1700000000}"#;
                t.insert("w_has_net", json).unwrap();
            }
            w.commit().unwrap();
        }

        let db = open_catalog(&provider_root).unwrap();
        let r = db.begin_read().unwrap();
        let t = r.open_table(CATALOG_WALLET_BY_ID).unwrap();
        let raw = t.get("w_has_net").unwrap().unwrap();
        let obj: serde_json::Value = serde_json::from_str(raw.value()).unwrap();
        assert_eq!(
            obj["network"], "cashu",
            "existing network field should be untouched"
        );
    }
}
