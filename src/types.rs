use crate::store::wallet::WalletMetadata;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;

// ═══════════════════════════════════════════
// Core Enums
// ═══════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
pub struct Amount {
    pub value: u64,
    pub token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum LnWalletBackend {
    Nwc,
    Phoenixd,
    Lnbits,
}

impl LnWalletBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nwc => "nwc",
            Self::Phoenixd => "phoenixd",
            Self::Lnbits => "lnbits",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum BtcBackend {
    Esplora,
    CoreRpc,
    Electrum,
}

impl BtcBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Esplora => "esplora",
            Self::CoreRpc => "core-rpc",
            Self::Electrum => "electrum",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SpendScope {
    #[serde(alias = "all")]
    GlobalUsdCents,
    Network,
    Wallet,
}

fn default_spend_scope_network() -> SpendScope {
    SpendScope::Network
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
pub struct SpendLimit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    #[serde(default = "default_spend_scope_network")]
    pub scope: SpendScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet: Option<String>,
    pub window_s: u64,
    pub max_spend: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpendLimitStatus {
    pub rule_id: String,
    pub scope: SpendScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet: Option<String>,
    pub window_s: u64,
    pub max_spend: u64,
    pub spent: u64,
    pub remaining: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub window_reset_s: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownstreamLimitNode {
    pub name: String,
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limits: Vec<SpendLimitStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub downstream: Vec<DownstreamLimitNode>,
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
    pub fn new(confirmed: u64, pending: u64, unit: impl Into<String>) -> Self {
        Self {
            confirmed,
            pending,
            unit: unit.into(),
            additional: BTreeMap::new(),
        }
    }

    pub fn with_additional(mut self, key: impl Into<String>, value: u64) -> Self {
        self.additional.insert(key.into(), value);
        self
    }

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
}

#[derive(Debug, Clone, Serialize)]
pub struct RestoreResult {
    pub wallet: String,
    pub unspent: u64,
    pub spent: u64,
    pub pending: u64,
    pub unit: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CashuSendQuoteInfo {
    pub wallet: String,
    pub amount_native: u64,
    pub fee_native: u64,
    pub fee_unit: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SendQuoteInfo {
    pub wallet: String,
    pub amount_native: u64,
    pub fee_estimate_native: u64,
    pub fee_unit: String,
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

// ═══════════════════════════════════════════
// Trace Types
// ═══════════════════════════════════════════

#[derive(Debug, Serialize, Clone)]
pub struct Trace {
    pub duration_ms: u64,
}

impl Trace {
    pub fn from_duration(duration_ms: u64) -> Self {
        Self { duration_ms }
    }
}

#[derive(Debug, Serialize)]
pub struct PongTrace {
    pub uptime_s: u64,
    pub requests_total: u64,
    pub in_flight: usize,
}

#[derive(Debug, Serialize)]
pub struct CloseTrace {
    pub uptime_s: u64,
    pub requests_total: u64,
}

// ═══════════════════════════════════════════
// Input (Requests)
// ═══════════════════════════════════════════

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "code")]
pub enum Input {
    #[serde(rename = "wallet_create")]
    WalletCreate {
        id: String,
        network: Network,
        #[serde(default)]
        label: Option<String>,
        /// Cashu mint URL (cashu only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mint_url: Option<String>,
        /// RPC endpoints for sol/evm providers.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rpc_endpoints: Vec<String>,
        /// EVM chain ID (evm only, default 8453 = Base).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chain_id: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mnemonic_secret: Option<String>,
        /// Esplora API URL (btc only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        btc_esplora_url: Option<String>,
        /// BTC sub-network: "mainnet" | "signet" (btc only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        btc_network: Option<String>,
        /// BTC address type: "taproot" | "segwit" (btc only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        btc_address_type: Option<String>,
        /// BTC chain-source backend (btc only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        btc_backend: Option<BtcBackend>,
        /// Bitcoin Core RPC URL (btc core-rpc only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        btc_core_url: Option<String>,
        /// Bitcoin Core RPC auth (btc core-rpc only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        btc_core_auth_secret: Option<String>,
        /// Electrum server URL (btc electrum only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        btc_electrum_url: Option<String>,
    },
    #[serde(rename = "ln_wallet_create")]
    LnWalletCreate {
        id: String,
        #[serde(flatten)]
        request: LnWalletCreateRequest,
    },
    #[serde(rename = "wallet_close")]
    WalletClose {
        id: String,
        wallet: String,
        #[serde(default)]
        dangerously_skip_balance_check_and_may_lose_money: bool,
    },
    #[serde(rename = "wallet_list")]
    WalletList {
        id: String,
        #[serde(default)]
        network: Option<Network>,
    },
    #[serde(rename = "balance")]
    Balance {
        id: String,
        #[serde(default)]
        wallet: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network: Option<Network>,
        #[serde(default)]
        check: bool,
    },
    #[serde(rename = "receive")]
    Receive {
        id: String,
        wallet: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network: Option<Network>,
        #[serde(default)]
        amount: Option<Amount>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        onchain_memo: Option<String>,
        #[serde(default)]
        wait_until_paid: bool,
        #[serde(default)]
        wait_timeout_s: Option<u64>,
        #[serde(default)]
        wait_poll_interval_ms: Option<u64>,
        #[serde(default)]
        write_qr_svg_file: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_confirmations: Option<u32>,
    },
    #[serde(rename = "receive_claim")]
    ReceiveClaim {
        id: String,
        wallet: String,
        quote_id: String,
    },

    #[serde(rename = "cashu_send")]
    CashuSend {
        id: String,
        #[serde(default)]
        wallet: Option<String>,
        amount: Amount,
        #[serde(default)]
        onchain_memo: Option<String>,
        #[serde(default, deserialize_with = "deserialize_local_memo")]
        local_memo: Option<BTreeMap<String, String>>,
        /// Restrict to wallets on these mints (tried in order).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mints: Option<Vec<String>>,
    },
    #[serde(rename = "cashu_receive")]
    CashuReceive {
        id: String,
        #[serde(default)]
        wallet: Option<String>,
        token: String,
    },
    #[serde(rename = "send")]
    Send {
        id: String,
        #[serde(default)]
        wallet: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network: Option<Network>,
        to: String,
        #[serde(default)]
        onchain_memo: Option<String>,
        #[serde(default, deserialize_with = "deserialize_local_memo")]
        local_memo: Option<BTreeMap<String, String>>,
        /// Restrict to wallets on these mints (cashu only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mints: Option<Vec<String>>,
    },

    #[serde(rename = "restore")]
    Restore { id: String, wallet: String },
    #[serde(rename = "local_wallet_show_seed")]
    WalletShowSeed { id: String, wallet: String },

    #[serde(rename = "history")]
    HistoryList {
        id: String,
        #[serde(default)]
        wallet: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network: Option<Network>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        onchain_memo: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        offset: Option<usize>,
        /// Only include records created at or after this epoch second.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since_epoch_s: Option<u64>,
        /// Only include records created before this epoch second.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until_epoch_s: Option<u64>,
    },
    #[serde(rename = "history_status")]
    HistoryStatus { id: String, transaction_id: String },
    #[serde(rename = "history_update")]
    HistoryUpdate {
        id: String,
        #[serde(default)]
        wallet: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network: Option<Network>,
        #[serde(default)]
        limit: Option<usize>,
    },

    #[serde(rename = "limit_add")]
    LimitAdd { id: String, limit: SpendLimit },
    #[serde(rename = "limit_remove")]
    LimitRemove { id: String, rule_id: String },
    #[serde(rename = "limit_list")]
    LimitList { id: String },
    #[serde(rename = "limit_set")]
    LimitSet { id: String, limits: Vec<SpendLimit> },

    #[serde(rename = "wallet_config_show")]
    WalletConfigShow { id: String, wallet: String },
    #[serde(rename = "wallet_config_set")]
    WalletConfigSet {
        id: String,
        wallet: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rpc_endpoints: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chain_id: Option<u64>,
    },
    #[serde(rename = "wallet_config_token_add")]
    WalletConfigTokenAdd {
        id: String,
        wallet: String,
        symbol: String,
        address: String,
        decimals: u8,
    },
    #[serde(rename = "wallet_config_token_remove")]
    WalletConfigTokenRemove {
        id: String,
        wallet: String,
        symbol: String,
    },

    #[serde(rename = "config")]
    Config(ConfigPatch),
    #[serde(rename = "version")]
    Version,
    #[serde(rename = "close")]
    Close,
}

impl Input {
    /// Returns true if this input must only be handled locally (never via RPC).
    pub fn is_local_only(&self) -> bool {
        matches!(
            self,
            Input::WalletShowSeed { .. }
                | Input::WalletClose {
                    dangerously_skip_balance_check_and_may_lose_money: true,
                    ..
                }
                | Input::LimitAdd { .. }
                | Input::LimitRemove { .. }
                | Input::LimitSet { .. }
                | Input::WalletConfigSet { .. }
                | Input::WalletConfigTokenAdd { .. }
                | Input::WalletConfigTokenRemove { .. }
                | Input::Restore { .. }
                | Input::Config(_)
        )
    }
}

// ═══════════════════════════════════════════
// Output (Responses)
// ═══════════════════════════════════════════

#[derive(Debug, Serialize)]
#[serde(tag = "code")]
pub enum Output {
    #[serde(rename = "wallet_created")]
    WalletCreated {
        id: String,
        wallet: String,
        network: Network,
        address: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        mnemonic: Option<String>,
        trace: Trace,
    },
    #[serde(rename = "wallet_closed")]
    WalletClosed {
        id: String,
        wallet: String,
        trace: Trace,
    },
    #[serde(rename = "wallet_list")]
    WalletList {
        id: String,
        wallets: Vec<WalletSummary>,
        trace: Trace,
    },
    #[serde(rename = "wallet_balances")]
    WalletBalances {
        id: String,
        wallets: Vec<WalletBalanceItem>,
        trace: Trace,
    },
    #[serde(rename = "receive_info")]
    ReceiveInfo {
        id: String,
        wallet: String,
        receive_info: ReceiveInfo,
        trace: Trace,
    },
    #[serde(rename = "receive_claimed")]
    ReceiveClaimed {
        id: String,
        wallet: String,
        amount: Amount,
        trace: Trace,
    },

    #[serde(rename = "cashu_sent")]
    CashuSent {
        id: String,
        wallet: String,
        transaction_id: String,
        status: TxStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        fee: Option<Amount>,
        token: String,
        trace: Trace,
    },

    #[serde(rename = "history")]
    History {
        id: String,
        items: Vec<HistoryRecord>,
        trace: Trace,
    },
    #[serde(rename = "history_status")]
    HistoryStatus {
        id: String,
        transaction_id: String,
        status: TxStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        confirmations: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        preimage: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        item: Option<HistoryRecord>,
        trace: Trace,
    },
    #[serde(rename = "history_updated")]
    HistoryUpdated {
        id: String,
        wallets_synced: usize,
        records_scanned: usize,
        records_added: usize,
        records_updated: usize,
        trace: Trace,
    },

    #[serde(rename = "limit_added")]
    LimitAdded {
        id: String,
        rule_id: String,
        trace: Trace,
    },
    #[serde(rename = "limit_removed")]
    LimitRemoved {
        id: String,
        rule_id: String,
        trace: Trace,
    },
    #[serde(rename = "limit_status")]
    LimitStatus {
        id: String,
        limits: Vec<SpendLimitStatus>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        downstream: Vec<DownstreamLimitNode>,
        trace: Trace,
    },
    #[serde(rename = "limit_exceeded")]
    #[allow(dead_code)]
    LimitExceeded {
        id: String,
        rule_id: String,
        scope: SpendScope,
        scope_key: String,
        spent: u64,
        max_spend: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        token: Option<String>,
        remaining_s: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        origin: Option<String>,
        trace: Trace,
    },

    #[serde(rename = "cashu_received")]
    CashuReceived {
        id: String,
        wallet: String,
        amount: Amount,
        trace: Trace,
    },
    #[serde(rename = "restored")]
    Restored {
        id: String,
        wallet: String,
        unspent: u64,
        spent: u64,
        pending: u64,
        unit: String,
        trace: Trace,
    },
    #[serde(rename = "wallet_seed")]
    WalletSeed {
        id: String,
        wallet: String,
        mnemonic_secret: String,
        trace: Trace,
    },

    #[serde(rename = "sent")]
    Sent {
        id: String,
        wallet: String,
        transaction_id: String,
        amount: Amount,
        #[serde(skip_serializing_if = "Option::is_none")]
        fee: Option<Amount>,
        #[serde(skip_serializing_if = "Option::is_none")]
        preimage: Option<String>,
        trace: Trace,
    },

    #[serde(rename = "wallet_config")]
    WalletConfig {
        id: String,
        wallet: String,
        config: WalletMetadata,
        trace: Trace,
    },
    #[serde(rename = "wallet_config_updated")]
    WalletConfigUpdated {
        id: String,
        wallet: String,
        trace: Trace,
    },
    #[serde(rename = "wallet_config_token_added")]
    WalletConfigTokenAdded {
        id: String,
        wallet: String,
        symbol: String,
        address: String,
        decimals: u8,
        trace: Trace,
    },
    #[serde(rename = "wallet_config_token_removed")]
    WalletConfigTokenRemoved {
        id: String,
        wallet: String,
        symbol: String,
        trace: Trace,
    },

    #[serde(rename = "error")]
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        error_code: String,
        error: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        hint: Option<String>,
        retryable: bool,
        trace: Trace,
    },

    #[serde(rename = "dry_run")]
    DryRun {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        command: String,
        params: serde_json::Value,
        trace: Trace,
    },

    #[serde(rename = "config")]
    Config(RuntimeConfig),
    #[serde(rename = "version")]
    Version { version: String, trace: PongTrace },
    #[serde(rename = "close")]
    Close { message: String, trace: CloseTrace },
    #[serde(rename = "log")]
    Log {
        event: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        argv: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        config: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        args: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        env: Option<serde_json::Value>,
        trace: Trace,
    },
}

// ═══════════════════════════════════════════
// Config Types
// ═══════════════════════════════════════════

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RuntimeConfig {
    #[serde(default)]
    pub data_dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_secret: Option<String>,
    #[serde(default)]
    pub limits: Vec<SpendLimit>,
    #[serde(default)]
    pub log: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exchange_rate: Option<ExchangeRateConfig>,
    /// Named afpay RPC nodes (e.g. `[afpay_rpc.wallet-server]`).
    #[serde(default)]
    pub afpay_rpc: std::collections::HashMap<String, AfpayRpcConfig>,
    /// Network → afpay_rpc node name (omit = local provider).
    #[serde(default)]
    pub providers: std::collections::HashMap<String, String>,
    /// Storage backend: "redb" (default) or "postgres".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_backend: Option<String>,
    /// PostgreSQL connection URL (used when storage_backend = "postgres").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postgres_url_secret: Option<String>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            rpc_endpoint: None,
            rpc_secret: None,
            limits: vec![],
            log: vec![],
            exchange_rate: None,
            afpay_rpc: std::collections::HashMap::new(),
            providers: std::collections::HashMap::new(),
            storage_backend: None,
            postgres_url_secret: None,
        }
    }
}

fn default_data_dir() -> String {
    // AFPAY_HOME takes priority, then ~/.afpay
    if let Some(val) = std::env::var_os("AFPAY_HOME") {
        return std::path::PathBuf::from(val).to_string_lossy().into_owned();
    }
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = std::path::PathBuf::from(home);
        p.push(".afpay");
        p.to_string_lossy().into_owned()
    } else {
        ".afpay".to_string()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AfpayRpcConfig {
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_secret: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ExchangeRateConfig {
    #[serde(default = "default_exchange_rate_ttl_s")]
    pub ttl_s: u64,
    #[serde(default = "default_exchange_rate_sources")]
    pub sources: Vec<ExchangeRateSource>,
}

impl Default for ExchangeRateConfig {
    fn default() -> Self {
        Self {
            ttl_s: default_exchange_rate_ttl_s(),
            sources: default_exchange_rate_sources(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ExchangeRateSource {
    #[serde(rename = "type")]
    pub source_type: ExchangeRateSourceType,
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExchangeRateSourceType {
    Generic,
    CoinGecko,
    Kraken,
}

fn default_exchange_rate_ttl_s() -> u64 {
    300
}

fn default_exchange_rate_sources() -> Vec<ExchangeRateSource> {
    vec![
        ExchangeRateSource {
            source_type: ExchangeRateSourceType::Kraken,
            endpoint: "https://api.kraken.com".to_string(),
            api_key: None,
        },
        ExchangeRateSource {
            source_type: ExchangeRateSourceType::CoinGecko,
            endpoint: "https://api.coingecko.com/api/v3".to_string(),
            api_key: None,
        },
    ]
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ConfigPatch {
    #[serde(default)]
    pub data_dir: Option<String>,
    #[serde(default)]
    pub limits: Option<Vec<SpendLimit>>,
    #[serde(default)]
    pub log: Option<Vec<String>>,
    #[serde(default)]
    pub exchange_rate: Option<ExchangeRateConfig>,
    #[serde(default)]
    pub afpay_rpc: Option<std::collections::HashMap<String, AfpayRpcConfig>>,
    #[serde(default)]
    pub providers: Option<std::collections::HashMap<String, String>>,
}

/// Deserializes `local_memo` with backward compatibility.
/// Accepts: null → None, "string" → Some({"note": "string"}), {object} → Some(object).
fn deserialize_local_memo<'de, D>(d: D) -> Result<Option<BTreeMap<String, String>>, D::Error>
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_only_checks() {
        // Already local-only
        assert!(Input::WalletShowSeed {
            id: "t".into(),
            wallet: "w".into(),
        }
        .is_local_only());

        assert!(Input::WalletClose {
            id: "t".into(),
            wallet: "w".into(),
            dangerously_skip_balance_check_and_may_lose_money: true,
        }
        .is_local_only());

        assert!(!Input::WalletClose {
            id: "t".into(),
            wallet: "w".into(),
            dangerously_skip_balance_check_and_may_lose_money: false,
        }
        .is_local_only());

        // Limit write ops
        assert!(Input::LimitAdd {
            id: "t".into(),
            limit: SpendLimit {
                rule_id: None,
                scope: SpendScope::GlobalUsdCents,
                network: None,
                wallet: None,
                window_s: 3600,
                max_spend: 1000,
                token: None,
            },
        }
        .is_local_only());

        assert!(Input::LimitRemove {
            id: "t".into(),
            rule_id: "r_1".into(),
        }
        .is_local_only());

        assert!(Input::LimitSet {
            id: "t".into(),
            limits: vec![],
        }
        .is_local_only());

        // Limit read is NOT local-only
        assert!(!Input::LimitList { id: "t".into() }.is_local_only());

        // Wallet config write ops
        assert!(Input::WalletConfigSet {
            id: "t".into(),
            wallet: "w".into(),
            label: None,
            rpc_endpoints: vec![],
            chain_id: None,
        }
        .is_local_only());

        assert!(Input::WalletConfigTokenAdd {
            id: "t".into(),
            wallet: "w".into(),
            symbol: "dai".into(),
            address: "0x".into(),
            decimals: 18,
        }
        .is_local_only());

        assert!(Input::WalletConfigTokenRemove {
            id: "t".into(),
            wallet: "w".into(),
            symbol: "dai".into(),
        }
        .is_local_only());

        // Wallet config read is NOT local-only
        assert!(!Input::WalletConfigShow {
            id: "t".into(),
            wallet: "w".into(),
        }
        .is_local_only());

        // Restore (seed over RPC)
        assert!(Input::Restore {
            id: "t".into(),
            wallet: "w".into(),
        }
        .is_local_only());
    }

    #[test]
    fn wallet_seed_output_uses_mnemonic_secret_field() {
        let out = Output::WalletSeed {
            id: "t_1".to_string(),
            wallet: "w_1".to_string(),
            mnemonic_secret: "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".to_string(),
            trace: Trace::from_duration(0),
        };
        let value = serde_json::to_value(out).expect("serialize wallet_seed output");
        assert_eq!(
            value.get("mnemonic_secret").and_then(|v| v.as_str()),
            Some(
                "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
            )
        );
        assert!(value.get("mnemonic").is_none());
    }

    #[test]
    fn history_list_parses_time_range_fields() {
        let json = r#"{
            "code": "history",
            "id": "t_1",
            "wallet": "w_1",
            "limit": 10,
            "offset": 0,
            "since_epoch_s": 1700000000,
            "until_epoch_s": 1700100000
        }"#;
        let input: Input = serde_json::from_str(json).expect("parse history_list with time range");
        match input {
            Input::HistoryList {
                since_epoch_s,
                until_epoch_s,
                ..
            } => {
                assert_eq!(since_epoch_s, Some(1_700_000_000));
                assert_eq!(until_epoch_s, Some(1_700_100_000));
            }
            other => panic!("expected HistoryList, got {other:?}"),
        }
    }

    #[test]
    fn history_list_time_range_fields_default_to_none() {
        let json = r#"{
            "code": "history",
            "id": "t_1",
            "limit": 10,
            "offset": 0
        }"#;
        let input: Input =
            serde_json::from_str(json).expect("parse history_list without time range");
        match input {
            Input::HistoryList {
                since_epoch_s,
                until_epoch_s,
                ..
            } => {
                assert_eq!(since_epoch_s, None);
                assert_eq!(until_epoch_s, None);
            }
            other => panic!("expected HistoryList, got {other:?}"),
        }
    }

    #[test]
    fn history_update_parses_sync_fields() {
        let json = r#"{
            "code": "history_update",
            "id": "t_2",
            "wallet": "w_1",
            "network": "sol",
            "limit": 150
        }"#;
        let input: Input = serde_json::from_str(json).expect("parse history_update");
        match input {
            Input::HistoryUpdate {
                wallet,
                network,
                limit,
                ..
            } => {
                assert_eq!(wallet.as_deref(), Some("w_1"));
                assert_eq!(network, Some(Network::Sol));
                assert_eq!(limit, Some(150));
            }
            other => panic!("expected HistoryUpdate, got {other:?}"),
        }
    }

    #[test]
    fn history_update_fields_default_to_none() {
        let json = r#"{
            "code": "history_update",
            "id": "t_3"
        }"#;
        let input: Input = serde_json::from_str(json).expect("parse history_update defaults");
        match input {
            Input::HistoryUpdate {
                wallet,
                network,
                limit,
                ..
            } => {
                assert_eq!(wallet, None);
                assert_eq!(network, None);
                assert_eq!(limit, None);
            }
            other => panic!("expected HistoryUpdate, got {other:?}"),
        }
    }
}
