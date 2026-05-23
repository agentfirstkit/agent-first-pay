use super::limits::SpendDebit;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    Ln,
    Sol,
    Evm,
    Cashu,
    Btc,
}

impl std::fmt::Display for Network {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ln => write!(f, "ln"),
            Self::Sol => write!(f, "sol"),
            Self::Evm => write!(f, "evm"),
            Self::Cashu => write!(f, "cashu"),
            Self::Btc => write!(f, "btc"),
        }
    }
}

impl std::str::FromStr for Network {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ln" => Ok(Self::Ln),
            "sol" => Ok(Self::Sol),
            "evm" => Ok(Self::Evm),
            "cashu" => Ok(Self::Cashu),
            "btc" => Ok(Self::Btc),
            _ => Err(format!(
                "unknown network '{s}'; expected: cashu, ln, sol, evm, btc"
            )),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WalletCreateRequest {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mint_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rpc_endpoints: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mnemonic_secret: Option<String>,
    /// Esplora API URL for BTC (btc only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub btc_esplora_url: Option<String>,
    /// BTC sub-network: "mainnet" or "signet" (btc only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub btc_network: Option<String>,
    /// BTC address type: "taproot" or "segwit" (btc only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub btc_address_type: Option<String>,
    /// BTC chain-source backend: esplora (default), core-rpc, electrum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub btc_backend: Option<BtcBackend>,
    /// Bitcoin Core RPC URL (btc core-rpc backend only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub btc_core_url: Option<String>,
    /// Bitcoin Core RPC auth "user:pass" (btc core-rpc backend only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub btc_core_auth_secret: Option<String>,
    /// Electrum server URL (btc electrum backend only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub btc_electrum_url: Option<String>,
}

impl std::fmt::Debug for WalletCreateRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalletCreateRequest")
            .field("label", &self.label)
            .field("mint_url", &self.mint_url)
            .field("rpc_endpoints", &self.rpc_endpoints)
            .field("chain_id", &self.chain_id)
            .field(
                "mnemonic_secret",
                &self.mnemonic_secret.as_ref().map(|_| "***"),
            )
            .field("btc_esplora_url", &self.btc_esplora_url)
            .field("btc_network", &self.btc_network)
            .field("btc_address_type", &self.btc_address_type)
            .field("btc_backend", &self.btc_backend)
            .field("btc_core_url", &self.btc_core_url)
            .field(
                "btc_core_auth_secret",
                &self.btc_core_auth_secret.as_ref().map(|_| "***"),
            )
            .field("btc_electrum_url", &self.btc_electrum_url)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Send,
    Receive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TxStatus {
    Pending,
    Confirmed,
    Failed,
}

// ═══════════════════════════════════════════
// Value Types
// ═══════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Amount {
    pub value: u64,
    pub token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LnWalletBackend {
    Nwc,
    Phoenixd,
    Lnbits,
}

impl LnWalletBackend {
    #[cfg_attr(
        not(any(feature = "ln-nwc", feature = "ln-phoenixd", feature = "ln-lnbits")),
        allow(dead_code)
    )]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nwc => "nwc",
            Self::Phoenixd => "phoenixd",
            Self::Lnbits => "lnbits",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BtcBackend {
    Esplora,
    CoreRpc,
    Electrum,
}

impl BtcBackend {
    #[cfg_attr(
        not(any(
            feature = "btc-esplora",
            feature = "btc-core",
            feature = "btc-electrum"
        )),
        allow(dead_code)
    )]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Esplora => "esplora",
            Self::CoreRpc => "core-rpc",
            Self::Electrum => "electrum",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct LnWalletCreateRequest {
    pub backend: LnWalletBackend,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nwc_uri_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_key_secret: Option<String>,
}

impl std::fmt::Debug for LnWalletCreateRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LnWalletCreateRequest")
            .field("backend", &self.backend)
            .field("label", &self.label)
            .field(
                "nwc_uri_secret",
                &self.nwc_uri_secret.as_ref().map(|_| "***"),
            )
            .field("endpoint", &self.endpoint)
            .field(
                "password_secret",
                &self.password_secret.as_ref().map(|_| "***"),
            )
            .field(
                "admin_key_secret",
                &self.admin_key_secret.as_ref().map(|_| "***"),
            )
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletInfo {
    pub id: String,
    pub network: Network,
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mnemonic: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletSummary {
    pub id: String,
    pub network: Network,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mint_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_endpoints: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<u64>,
    pub created_at_epoch_s: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceInfo {
    pub confirmed: u64,
    pub pending: u64,
    /// Native unit name: "sats", "lamports", "gwei", "token-units".
    pub unit: String,
    /// Provider-specific extra balance categories.
    /// Example: `fee_credit_sats` for phoenixd.
    #[serde(default, flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub additional: BTreeMap<String, u64>,
}

impl BalanceInfo {
    #[allow(dead_code)]
    pub fn new(confirmed: u64, pending: u64, unit: impl Into<String>) -> Self {
        Self {
            confirmed,
            pending,
            unit: unit.into(),
            additional: BTreeMap::new(),
        }
    }

    #[cfg_attr(not(feature = "ln-phoenixd"), allow(dead_code))]
    pub fn with_additional(mut self, key: impl Into<String>, value: u64) -> Self {
        self.additional.insert(key.into(), value);
        self
    }

    #[cfg_attr(
        not(any(
            feature = "ln-nwc",
            feature = "ln-phoenixd",
            feature = "ln-lnbits",
            feature = "sol",
            feature = "evm"
        )),
        allow(dead_code)
    )]
    pub fn non_zero_components(&self) -> Vec<(String, u64)> {
        let mut components = Vec::new();
        if self.confirmed > 0 {
            components.push((format!("confirmed_{}", self.unit), self.confirmed));
        }
        if self.pending > 0 {
            components.push((format!("pending_{}", self.unit), self.pending));
        }
        for (key, value) in &self.additional {
            if *value > 0 {
                components.push((key.clone(), *value));
            }
        }
        components
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletBalanceItem {
    #[serde(flatten)]
    pub wallet: WalletSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance: Option<BalanceInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Per-network balance summary aggregated from individual wallets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkBalanceSummary {
    pub network: Network,
    pub wallet_count: usize,
    pub confirmed: u64,
    pub pending: u64,
    pub unit: String,
    pub errors: usize,
}

impl NetworkBalanceSummary {
    /// Build summaries grouped by (network, unit) from a list of wallet balances.
    pub fn from_wallets(wallets: &[WalletBalanceItem]) -> Vec<Self> {
        use std::collections::BTreeMap;
        let mut groups: BTreeMap<(String, String), Self> = BTreeMap::new();
        for item in wallets {
            let network = item.wallet.network;
            let (unit, confirmed, pending) = match &item.balance {
                Some(b) => (b.unit.clone(), b.confirmed, b.pending),
                None => ("unknown".to_string(), 0, 0),
            };
            let has_error = item.error.is_some() || item.balance.is_none();
            let key = (network.to_string(), unit.clone());
            let entry = groups.entry(key).or_insert(Self {
                network,
                wallet_count: 0,
                confirmed: 0,
                pending: 0,
                unit,
                errors: 0,
            });
            entry.wallet_count += 1;
            entry.confirmed += confirmed;
            entry.pending += pending;
            if has_error {
                entry.errors += 1;
            }
        }
        groups.into_values().collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiveInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invoice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quote_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryRecord {
    pub transaction_id: String,
    pub wallet: String,
    pub network: Network,
    pub direction: Direction,
    pub amount: Amount,
    pub status: TxStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onchain_memo: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_local_memo"
    )]
    pub local_memo: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_addr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preimage: Option<String>,
    pub created_at_epoch_s: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmed_at_epoch_s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fee: Option<Amount>,
    /// Reference keys found in the transaction (sol only, per strain-payment-method-solana).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_keys: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CashuSendResult {
    pub wallet: String,
    pub transaction_id: String,
    pub status: TxStatus,
    pub fee: Option<Amount>,
    pub token: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CashuReceiveResult {
    pub wallet: String,
    pub amount: Amount,
    pub memo: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RestoreResult {
    pub wallet: String,
    pub unspent: u64,
    pub spent: u64,
    pub pending: u64,
    pub unit: String,
}

#[cfg(feature = "interactive")]
#[derive(Debug, Clone, Serialize)]
pub struct CashuSendQuoteInfo {
    pub wallet: String,
    pub amount_native: u64,
    pub fee_native: u64,
    pub fee_unit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendQuoteInfo {
    pub wallet: String,
    pub amount_native: u64,
    pub fee_estimate_native: u64,
    pub fee_unit: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spend_debits: Vec<SpendDebit>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SendResult {
    pub wallet: String,
    pub transaction_id: String,
    pub amount: Amount,
    pub fee: Option<Amount>,
    pub preimage: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryStatusInfo {
    pub transaction_id: String,
    pub status: TxStatus,
    pub confirmations: Option<u32>,
    pub preimage: Option<String>,
    pub item: Option<HistoryRecord>,
}

/// Deserializes `local_memo` with backward compatibility.
/// Accepts: null → None, "string" → Some({"note": "string"}), {object} → Some(object).
pub(crate) fn deserialize_local_memo<'de, D>(
    d: D,
) -> Result<Option<BTreeMap<String, String>>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de;

    struct LocalMemoVisitor;

    impl<'de> de::Visitor<'de> for LocalMemoVisitor {
        type Value = Option<BTreeMap<String, String>>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("null, a string, or a map of string→string")
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            let mut m = BTreeMap::new();
            m.insert("note".to_string(), v.to_string());
            Ok(Some(m))
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
            let mut m = BTreeMap::new();
            m.insert("note".to_string(), v);
            Ok(Some(m))
        }

        fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut m = BTreeMap::new();
            while let Some((k, v)) = map.next_entry::<String, String>()? {
                m.insert(k, v);
            }
            Ok(Some(m))
        }

        fn visit_some<D2: Deserializer<'de>>(self, d: D2) -> Result<Self::Value, D2::Error> {
            d.deserialize_any(Self)
        }
    }

    d.deserialize_option(LocalMemoVisitor)
}
