#![cfg_attr(
    not(any(
        feature = "cashu",
        feature = "ln-nwc",
        feature = "ln-phoenixd",
        feature = "ln-lnbits",
        feature = "sol",
        feature = "evm",
        feature = "btc-esplora",
        feature = "btc-core",
        feature = "btc-electrum"
    )),
    allow(dead_code)
)]

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

#[derive(Clone, Serialize, Deserialize)]
pub struct WalletMetadata {
    pub id: String,
    pub network: Network,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mint_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sol_rpc_endpoints: Option<Vec<String>>,
    /// Solana cluster the wallet was created on: `"mainnet-beta"`, `"devnet"`,
    /// or `"testnet"`. Pre-cluster-tagging wallets serialize without this
    /// field; on load they remain `None` and skip the send-time cluster
    /// check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sol_cluster: Option<String>,
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

impl std::fmt::Debug for WalletMetadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalletMetadata")
            .field("id", &self.id)
            .field("network", &self.network)
            .field("label", &self.label)
            .field("mint_url", &self.mint_url)
            .field("sol_rpc_endpoints", &self.sol_rpc_endpoints)
            .field("sol_cluster", &self.sol_cluster)
            .field("evm_rpc_endpoints", &self.evm_rpc_endpoints)
            .field("evm_chain_id", &self.evm_chain_id)
            .field("seed_secret", &self.seed_secret.as_ref().map(|_| "***"))
            .field("backend", &self.backend)
            .field("btc_esplora_url", &self.btc_esplora_url)
            .field("btc_network", &self.btc_network)
            .field("btc_address_type", &self.btc_address_type)
            .field("btc_core_url", &self.btc_core_url)
            .field(
                "btc_core_auth_secret",
                &self.btc_core_auth_secret.as_ref().map(|_| "***"),
            )
            .field("btc_electrum_url", &self.btc_electrum_url)
            .field("custom_tokens", &self.custom_tokens)
            .field("created_at_epoch_s", &self.created_at_epoch_s)
            .field("error", &self.error)
            .finish()
    }
}

// ═══════════════════════════════════════════
// Shared helpers (always available)
// ═══════════════════════════════════════════

const WALLETS_DIR: &str = "wallets";

/// Generate a random `w_<8-hex>` wallet identifier. No longer used in production
/// (providers derive stable ids from mnemonic + network — see
/// `derive_wallet_identifier`) but retained for tests that need an arbitrary id.
#[allow(dead_code)]
pub fn generate_wallet_identifier() -> Result<String, PayError> {
    let mut buf = [0u8; 4];
    getrandom::fill(&mut buf).map_err(|e| PayError::internal_error(format!("rng failed: {e}")))?;
    Ok(format!("w_{}", hex::encode(buf)))
}

/// Derive a stable wallet identifier from the mnemonic plus any data that
/// distinguishes wallets sharing the same seed (mint URL for Cashu, chain id
/// for EVM, sub-network/address-type for BTC). The same inputs produce the
/// same id forever, so a re-issued create_wallet request finds the existing
/// wallet via the load_wallet_metadata idempotency check instead of producing
/// a sibling with a fresh random id.
///
/// Returns the same 4-byte `w_<8-hex>` shape as `generate_wallet_identifier` so
/// downstream code that pattern-matches on the prefix (e.g. label-vs-id resolution
/// in `resolve_wallet_id`) keeps working unchanged.
pub fn derive_wallet_identifier(distinguishers: &[&[u8]]) -> String {
    let mut hasher = blake3::Hasher::new();
    // Domain-separate from any other use of blake3 in the codebase. Bumping
    // this string would migrate every wallet to a fresh id.
    hasher.update(b"afpay/wallet-id/v1");
    for piece in distinguishers {
        // Length-prefix each piece so concatenation is unambiguous (e.g.
        // ["ab","c"] vs ["a","bc"] hash to different ids).
        let len = (piece.len() as u64).to_le_bytes();
        hasher.update(&len);
        hasher.update(piece);
    }
    let hash = hasher.finalize();
    format!("w_{}", hex::encode(&hash.as_bytes()[..4]))
}

pub fn generate_transaction_identifier() -> Result<String, PayError> {
    let mut buf = [0u8; 8];
    getrandom::fill(&mut buf).map_err(|e| PayError::internal_error(format!("rng failed: {e}")))?;
    Ok(format!("tx_{}", hex::encode(buf)))
}

pub fn generate_request_identifier() -> Result<String, PayError> {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).map_err(|e| PayError::internal_error(format!("rng failed: {e}")))?;
    Ok(format!("req_{}", hex::encode(buf)))
}

pub fn now_epoch_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Root path for all wallets: `{data_dir}/wallets/`
fn wallets_root(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join(WALLETS_DIR)
}

pub fn wallet_data_directory_path_for_wallet_metadata(
    data_dir: &str,
    wallet_metadata: &WalletMetadata,
) -> PathBuf {
    wallets_root(data_dir)
        .join(&wallet_metadata.id)
        .join("wallet-data")
}

pub fn wallet_directory_path(data_dir: &str, wallet_id: &str) -> Result<PathBuf, PayError> {
    let dir = wallets_root(data_dir).join(wallet_id);
    if dir.is_dir() {
        Ok(dir)
    } else {
        Err(PayError::wallet_not_found(format!(
            "wallet {wallet_id} not found"
        )))
    }
}

pub fn wallet_data_directory_path(data_dir: &str, wallet_id: &str) -> Result<PathBuf, PayError> {
    Ok(wallet_directory_path(data_dir, wallet_id)?.join("wallet-data"))
}

#[cfg(feature = "redb")]
pub(crate) fn parse_wallet_metadata(
    raw: &str,
    wallet_id: &str,
) -> Result<WalletMetadata, PayError> {
    serde_json::from_str(raw)
        .map_err(|e| PayError::internal_error(format!("parse wallet {wallet_id}: {e}")))
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
    let root = wallets_root(data_dir);
    std::fs::create_dir_all(&root).map_err(|e| {
        PayError::internal_error(format!("create wallets dir {}: {e}", root.display()))
    })?;
    set_private_dir_permissions(&root)?;

    let wallet_dir = root.join(&wallet_metadata.id);
    let wallet_data_dir = wallet_dir.join("wallet-data");
    std::fs::create_dir_all(&wallet_data_dir).map_err(|e| {
        PayError::internal_error(format!(
            "create wallet dir {}: {e}",
            wallet_data_dir.display()
        ))
    })?;
    set_private_dir_permissions(&wallet_dir)?;
    set_private_dir_permissions(&wallet_data_dir)?;

    let wallet_metadata_json = serde_json::to_string(wallet_metadata)
        .map_err(|e| PayError::internal_error(format!("serialize wallet metadata: {e}")))?;

    // catalog.redb: unified wallet index
    let catalog_db = open_catalog(&root)?;
    let catalog_txn = catalog_db
        .begin_write()
        .map_err(|e| PayError::internal_error(format!("catalog begin_write: {e}")))?;
    {
        let mut table = catalog_txn
            .open_table(CATALOG_WALLET_BY_ID)
            .map_err(|e| PayError::internal_error(format!("catalog open wallet_by_id: {e}")))?;
        table
            .insert(wallet_metadata.id.as_str(), wallet_metadata_json.as_str())
            .map_err(|e| PayError::internal_error(format!("catalog insert wallet: {e}")))?;
    }
    catalog_txn
        .commit()
        .map_err(|e| PayError::internal_error(format!("catalog commit: {e}")))?;

    // core.redb: per-wallet authoritative metadata
    let core_db = open_core(&wallet_dir.join("core.redb"))?;
    let core_txn = core_db
        .begin_write()
        .map_err(|e| PayError::internal_error(format!("core begin_write: {e}")))?;
    {
        let mut table = core_txn
            .open_table(CORE_METADATA_KEY_VALUE)
            .map_err(|e| PayError::internal_error(format!("core open metadata_kv: {e}")))?;
        table
            .insert(CORE_WALLET_METADATA_KEY, wallet_metadata_json.as_str())
            .map_err(|e| PayError::internal_error(format!("core write wallet metadata: {e}")))?;
    }
    core_txn
        .commit()
        .map_err(|e| PayError::internal_error(format!("core commit wallet metadata: {e}")))?;

    Ok(())
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> Result<(), PayError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| PayError::internal_error(format!("chmod 700 {}: {e}", path.display())))
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> Result<(), PayError> {
    Ok(())
}

#[cfg(feature = "redb")]
pub fn load_wallet_metadata(data_dir: &str, wallet_id: &str) -> Result<WalletMetadata, PayError> {
    let root = wallets_root(data_dir);

    // Fast path: catalog
    let catalog_path = root.join("catalog.redb");
    if catalog_path.exists() {
        let db = open_catalog(&root)?;
        let read_txn = db
            .begin_read()
            .map_err(|e| PayError::internal_error(format!("catalog begin_read: {e}")))?;
        if let Ok(table) = read_txn.open_table(CATALOG_WALLET_BY_ID)
            && let Some(value) = table.get(wallet_id).map_err(|e| {
                PayError::internal_error(format!("catalog read wallet {wallet_id}: {e}"))
            })?
        {
            return parse_wallet_metadata(value.value(), wallet_id);
        }
    }

    // Fallback: wallet core metadata
    let wallet_dir = root.join(wallet_id);
    if wallet_dir.is_dir() {
        let core_path = wallet_dir.join("core.redb");
        if core_path.exists() {
            let db = db::open_database(&core_path)?;
            let read_txn = db
                .begin_read()
                .map_err(|e| PayError::internal_error(format!("core begin_read: {e}")))?;
            let Ok(table) = read_txn.open_table(CORE_METADATA_KEY_VALUE) else {
                return Err(PayError::wallet_not_found(format!(
                    "wallet {wallet_id} not found"
                )));
            };
            if let Some(value) = table
                .get(CORE_WALLET_METADATA_KEY)
                .map_err(|e| PayError::internal_error(format!("core read wallet metadata: {e}")))?
            {
                return parse_wallet_metadata(value.value(), wallet_id);
            }
        }
    }

    // Label fallback: if wallet_id doesn't start with "w_", try matching by label
    if !wallet_id.starts_with("w_") {
        let catalog_path = root.join("catalog.redb");
        if catalog_path.exists() {
            let db = open_catalog(&root)?;
            let read_txn = db
                .begin_read()
                .map_err(|e| PayError::internal_error(format!("catalog begin_read: {e}")))?;
            if let Ok(table) = read_txn.open_table(CATALOG_WALLET_BY_ID) {
                for entry in table
                    .iter()
                    .map_err(|e| PayError::internal_error(format!("catalog iterate: {e}")))?
                {
                    let (key, value) = entry.map_err(|e| {
                        PayError::internal_error(format!("catalog read entry: {e}"))
                    })?;
                    if let Ok(meta) = parse_wallet_metadata(value.value(), key.value())
                        && meta.label.as_deref() == Some(wallet_id)
                    {
                        return Ok(meta);
                    }
                }
            }
        }
    }

    Err(PayError::wallet_not_found(format!(
        "wallet {wallet_id} not found"
    )))
}

#[cfg(feature = "redb")]
pub fn list_wallet_metadata(
    data_dir: &str,
    network: Option<Network>,
) -> Result<Vec<WalletMetadata>, PayError> {
    let root = wallets_root(data_dir);
    let catalog_path = root.join("catalog.redb");
    if !catalog_path.exists() {
        return Ok(vec![]);
    }

    let db = open_catalog(&root)?;
    let read_txn = db
        .begin_read()
        .map_err(|e| PayError::internal_error(format!("catalog begin_read: {e}")))?;
    let Ok(table) = read_txn.open_table(CATALOG_WALLET_BY_ID) else {
        return Ok(vec![]);
    };

    let mut wallets = Vec::new();
    for entry in table
        .iter()
        .map_err(|e| PayError::internal_error(format!("catalog iterate wallets: {e}")))?
    {
        let (key, value) = entry
            .map_err(|e| PayError::internal_error(format!("catalog read wallet entry: {e}")))?;
        let wallet_metadata: WalletMetadata = match serde_json::from_str(value.value()) {
            Ok(m) => m,
            Err(e) => WalletMetadata {
                id: key.value().to_string(),
                network: Network::Cashu, // placeholder for corrupt entry
                label: None,
                mint_url: None,
                sol_rpc_endpoints: None,
                evm_rpc_endpoints: None,
                evm_chain_id: None,
                sol_cluster: None,
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
            },
        };
        if let Some(network) = network
            && wallet_metadata.network != network
        {
            continue;
        }
        wallets.push(wallet_metadata);
    }

    wallets.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(wallets)
}

#[cfg(feature = "redb")]
pub fn delete_wallet_metadata(data_dir: &str, wallet_id: &str) -> Result<(), PayError> {
    let root = wallets_root(data_dir);

    // Remove from catalog
    let catalog_path = root.join("catalog.redb");
    if catalog_path.exists() {
        let db = open_catalog(&root)?;
        let write_txn = db
            .begin_write()
            .map_err(|e| PayError::internal_error(format!("catalog begin_write: {e}")))?;
        {
            let mut table = write_txn
                .open_table(CATALOG_WALLET_BY_ID)
                .map_err(|e| PayError::internal_error(format!("catalog open wallet_by_id: {e}")))?;
            let _ = table
                .remove(wallet_id)
                .map_err(|e| PayError::internal_error(format!("catalog remove wallet: {e}")))?;
        }
        write_txn
            .commit()
            .map_err(|e| PayError::internal_error(format!("catalog commit delete: {e}")))?;
    }

    // Remove wallet directory (core.redb + wallet-data/*)
    let wallet_dir = root.join(wallet_id);
    if wallet_dir.exists() {
        std::fs::remove_dir_all(&wallet_dir)
            .map_err(|e| PayError::internal_error(format!("delete wallet dir: {e}")))?;
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
        0 => Err(PayError::wallet_not_found(format!(
            "no wallet found with ID or label '{id_or_label}'"
        ))),
        1 => Ok(matches.remove(0).id.clone()),
        n => Err(PayError::invalid_amount(format!(
            "label '{id_or_label}' matches {n} wallets — use wallet ID instead"
        ))),
    }
}

#[cfg(feature = "redb")]
const CATALOG_VERSION: u64 = 1;
#[cfg(feature = "redb")]
const CORE_VERSION: u64 = 1;

#[cfg(feature = "redb")]
fn open_catalog(wallets_dir: &Path) -> Result<Database, PayError> {
    db::open_and_migrate(
        &wallets_dir.join("catalog.redb"),
        CATALOG_VERSION,
        &[
            // v0 → v1: stamp version (no data migration needed)
            &|_db: &Database| Ok(()),
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

// ═══════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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
    fn derive_wallet_id_format_matches_random_shape() {
        let id = derive_wallet_identifier(&[b"sol", b"abandon ability ..."]);
        assert!(id.starts_with("w_"), "should start with w_: {id}");
        assert_eq!(id.len(), 10, "w_ + 8 hex chars = 10: {id}");
        assert!(id[2..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn derive_wallet_id_is_deterministic_and_input_sensitive() {
        // Same inputs always yield the same id.
        let id_a = derive_wallet_identifier(&[b"sol", b"abandon abandon"]);
        let id_b = derive_wallet_identifier(&[b"sol", b"abandon abandon"]);
        assert_eq!(id_a, id_b);

        // Different mnemonic → different id.
        let id_diff_mnemonic = derive_wallet_identifier(&[b"sol", b"about above ..."]);
        assert_ne!(id_a, id_diff_mnemonic);

        // Different network with same mnemonic → different id (no cross-net collision).
        let id_diff_net = derive_wallet_identifier(&[b"evm", b"abandon abandon"]);
        assert_ne!(id_a, id_diff_net);

        // Length-prefixing prevents concatenation ambiguity.
        let id_concat_ambiguity_a = derive_wallet_identifier(&[b"ab", b"c"]);
        let id_concat_ambiguity_b = derive_wallet_identifier(&[b"a", b"bc"]);
        assert_ne!(id_concat_ambiguity_a, id_concat_ambiguity_b);
    }

    #[test]
    fn generate_tx_id_format() {
        let id = generate_transaction_identifier().unwrap();
        assert!(id.starts_with("tx_"), "should start with tx_: {id}");
        assert_eq!(id.len(), 19, "tx_ + 16 hex chars = 19: {id}");
        assert!(id[3..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generate_request_id_format() {
        let id = generate_request_identifier().unwrap();
        assert!(id.starts_with("req_"), "should start with req_: {id}");
        assert_eq!(id.len(), 36, "req_ + 32 hex chars = 36: {id}");
        assert!(id[4..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn wallet_metadata_debug_redacts_secrets() {
        let meta = WalletMetadata {
            id: "w_aabbccdd".to_string(),
            network: Network::Btc,
            label: Some("btc".to_string()),
            mint_url: None,
            sol_rpc_endpoints: None,
            evm_rpc_endpoints: None,
            evm_chain_id: None,
            sol_cluster: None,
            seed_secret: Some("seed-secret-value".to_string()),
            backend: Some("core-rpc".to_string()),
            btc_esplora_url: None,
            btc_network: Some("signet".to_string()),
            btc_address_type: Some("taproot".to_string()),
            btc_core_url: Some("http://127.0.0.1:8332".to_string()),
            btc_core_auth_secret: Some("core-auth-secret-value".to_string()),
            btc_electrum_url: None,
            custom_tokens: None,
            created_at_epoch_s: 1,
            error: None,
        };
        let rendered = format!("{meta:?}");
        assert!(!rendered.contains("seed-secret-value"));
        assert!(!rendered.contains("core-auth-secret-value"));
        assert!(rendered.contains("***"));
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
            sol_cluster: None,
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
            matches!(err, PayError::WalletNotFound { .. }),
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
            sol_cluster: None,
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
            sol_cluster: None,
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
            sol_cluster: None,
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
}
