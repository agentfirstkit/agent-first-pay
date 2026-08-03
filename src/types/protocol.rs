use super::config::RuntimeConfig;
use super::domain::*;
use super::limits::*;
use crate::store::wallet::WalletMetadata;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::domain::deserialize_local_memo;

pub const JSON_PROTOCOL_VERSION: u32 = 1;

/// Wire-level wrapper for every Input. Carries cross-cutting flags (dry_run,
/// future: request_metadata) so handlers don't have to thread them through
/// each variant. JSON layout: `{"dry_run": true, "code": "send", ...input fields}`.
/// Plain Input JSON (no dry_run field) still deserializes via #[serde(default)].
/// Note: idempotency_key lives on the individual Send/CashuSend variants (not
/// here) because it only applies to value-moving operations.
#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    /// When true, every handler validates the request and emits Output::DryRun
    /// instead of executing. Works across cli/pipe/rpc/rest modes uniformly.
    #[serde(default)]
    pub dry_run: bool,
    #[serde(flatten)]
    pub input: Input,
}

impl Request {
    pub fn from_input(input: Input) -> Self {
        Self {
            dry_run: false,
            input,
        }
    }
}

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
        /// Solana cluster tag: `"mainnet-beta"`, `"devnet"`, or `"testnet"`.
        /// When set, the daemon stores it on the wallet and refuses sends if
        /// the active RPC endpoint heuristically belongs to a different
        /// cluster. Omit on non-sol wallets.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sol_cluster: Option<String>,
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
        wait_sync_limit: Option<usize>,
        #[serde(default)]
        write_qr_svg_file: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_confirmations: Option<u32>,
        /// Reference key to watch for (base58, sol only, per strain-payment-method-solana).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reference: Option<String>,
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
        /// Agent-supplied opaque key (≤128 chars). Two CashuSend calls with the
        /// same key replay the first terminal output instead of re-minting.
        /// Mismatched body returns `idempotency_conflict`. Persisted for 24h.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        idempotency_key: Option<String>,
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
        /// Amount in the network's base unit (sats / lamports / wei). Required for
        /// EVM/SOL/BTC. Optional for Lightning when `to` is an amount-bearing
        /// invoice. Cashu sends go through CashuSend.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        amount: Option<Amount>,
        #[serde(default)]
        onchain_memo: Option<String>,
        #[serde(default, deserialize_with = "deserialize_local_memo")]
        local_memo: Option<BTreeMap<String, String>>,
        /// Restrict to wallets on these mints (cashu only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mints: Option<Vec<String>>,
        /// EVM-only: if set, the daemon verifies that the wallet's chain_id
        /// matches this value before broadcasting. Mismatch returns
        /// `wrong_chain` so an agent that thinks it is talking to Base does
        /// not accidentally broadcast on Arbitrum / Optimism / Mainnet.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chain_id: Option<u64>,
        /// Agent-supplied opaque key (≤128 chars). Two Send calls with the
        /// same key replay the first terminal output (Sent / AccountingInconsistent)
        /// instead of re-broadcasting. Mismatched body returns
        /// `idempotency_conflict`. Persisted for 24h.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        idempotency_key: Option<String>,
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

    #[serde(rename = "reconcile_reservation")]
    ReconcileReservation {
        id: String,
        reservation_id: u64,
        action: ReconcileAction,
        /// Operator-supplied free-form explanation (1..=512 chars). Stored on
        /// the reservation for audit; emitted in the daemon log.
        reason: String,
    },

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

    /// Get a config key (omit key to show all).
    #[serde(rename = "config_get")]
    ConfigGet { id: String, key: Option<String> },
    /// Set a config key.
    #[serde(rename = "config_set")]
    ConfigSet {
        id: String,
        key: String,
        #[serde(default)]
        values: Vec<String>,
    },
    #[serde(rename = "version")]
    Version,
    #[serde(rename = "close")]
    Close,
    /// Return the self-describing wire schema. Available in every mode so an
    /// agent can discover the operation set, field shapes, error codes, and
    /// local-only flags without scraping `--help` or the source tree.
    #[serde(rename = "schema")]
    Schema,
}

/// Operator action when calling `Input::ReconcileReservation` to repair a spend
/// reservation the daemon cannot resolve on its own. `Confirm` is for the case
/// where the network operation actually succeeded but the daemon never observed
/// (or could not record) the confirmation — the operator vouches that the money
/// did leave and the ledger should reflect it. `Cancel` is the converse: the
/// operator knows the payment never happened and wants to release the budget.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileAction {
    Confirm,
    Cancel,
}

impl ReconcileAction {
    pub fn as_str(self) -> &'static str {
        match self {
            ReconcileAction::Confirm => "confirm",
            ReconcileAction::Cancel => "cancel",
        }
    }
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
                | Input::ReconcileReservation { .. }
                | Input::WalletConfigSet { .. }
                | Input::WalletConfigTokenAdd { .. }
                | Input::WalletConfigTokenRemove { .. }
                | Input::Restore { .. }
                | Input::ConfigGet { .. }
                | Input::ConfigSet { .. }
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
        mnemonic_secret: Option<String>,
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
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        summary: Vec<NetworkBalanceSummary>,
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
        /// Spend-ledger reservation ids the daemon successfully confirmed for
        /// this payment. An agent can use these to drive future reconciliation
        /// or cancel flows. Empty when no spend limits were enforced.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reservation_ids: Vec<u64>,
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
    #[serde(rename = "reconciled")]
    Reconciled {
        id: String,
        reservation_id: u64,
        action: ReconcileAction,
        /// One of `pending` / `expired` / `confirmed` / `cancelled` — the status
        /// the reservation held before this reconcile call. Lets an audit log
        /// reconstruct what the operator was looking at when they acted.
        previous_status: String,
        new_status: String,
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

    /// Emitted when a payment was broadcast to the network successfully but the
    /// local spend-ledger failed to record its confirmation (e.g. storage I/O
    /// error mid-flight). The funds left the wallet; the ledger does NOT reflect
    /// them, so future limit checks under-count the spend. An operator must
    /// reconcile manually. Agents should treat this as a hard failure and never
    /// retry the original request — the side-effect already happened.
    #[serde(rename = "accounting_inconsistent")]
    AccountingInconsistent {
        id: String,
        /// The network transaction that succeeded.
        transaction_id: String,
        /// IDs of the ledger reservations that could not be confirmed.
        reservation_ids: Vec<u64>,
        /// Concatenated error messages from confirm attempts (last error per id).
        confirm_errors: Vec<String>,
        hint: String,
        trace: Trace,
    },

    #[serde(rename = "cashu_received")]
    CashuReceived {
        id: String,
        wallet: String,
        amount: Amount,
        #[serde(skip_serializing_if = "Option::is_none")]
        memo: Option<String>,
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
        /// Spend-ledger reservation ids the daemon successfully confirmed for
        /// this payment. An agent can use these to drive future reconciliation
        /// or cancel flows. Empty when no spend limits were enforced.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reservation_ids: Vec<u64>,
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

    #[serde(rename = "data_backed_up")]
    #[cfg_attr(not(feature = "backup"), allow(dead_code))]
    DataBackedUp {
        data_dir: String,
        path: String,
        created_rfc3339: String,
        trace: Trace,
    },
    #[serde(rename = "data_restored")]
    #[cfg_attr(not(feature = "backup"), allow(dead_code))]
    DataRestored {
        data_dir: String,
        path: String,
        trace: Trace,
    },

    #[serde(rename = "network_data_backed_up")]
    #[cfg_attr(not(feature = "backup"), allow(dead_code))]
    NetworkDataBackedUp {
        network: String,
        data_dir: String,
        path: String,
        created_rfc3339: String,
        trace: Trace,
    },
    #[serde(rename = "network_data_restored")]
    #[cfg_attr(not(feature = "backup"), allow(dead_code))]
    NetworkDataRestored {
        network: String,
        data_dir: String,
        path: String,
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
        /// Suggested minimum delay before the agent retries, in milliseconds.
        /// Set on `busy` / `rate_limited` / `temporary_network_error` so an
        /// agent can pace retries without re-implementing daemon backoff.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_ms: Option<u64>,
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
    /// Single config key result from config_get.
    #[serde(rename = "config_value")]
    ConfigValue {
        key: String,
        value: serde_json::Value,
        trace: Trace,
    },
    #[serde(rename = "version")]
    Version {
        version: String,
        protocol_version: u32,
        trace: PongTrace,
    },
    #[serde(rename = "close")]
    Close { message: String, trace: CloseTrace },
    #[serde(rename = "schema")]
    Schema {
        schema: serde_json::Value,
        trace: Trace,
    },
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

/// Returns true if the string looks like a BOLT12 offer (`lno1…`),
/// optionally with a `?amount=<sats>` suffix. Case-insensitive.
#[allow(dead_code)]
pub fn is_bolt12_offer(s: &str) -> bool {
    s.len() >= 4 && s[..4].eq_ignore_ascii_case("lno1")
}

/// Split a BOLT12 offer string into the raw offer and an optional amount-sats.
/// Accepts `lno1...` or `lno1...?amount=1000`. Case-insensitive prefix detection.
#[allow(dead_code)]
pub fn parse_bolt12_offer_parts(s: &str) -> (String, Option<u64>) {
    if let Some(idx) = s.find("?amount=") {
        let offer = s[..idx].to_string();
        let amt = s[idx + 8..].parse::<u64>().ok();
        (offer, amt)
    } else {
        (s.to_string(), None)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::types::*;

    #[test]
    fn bolt12_offer_detection() {
        assert!(is_bolt12_offer("lno1qgsqvgjwcf6qqz9"));
        assert!(is_bolt12_offer("lno1qgsqvgjwcf6qqz9?amount=1000"));
        assert!(is_bolt12_offer("LNO1QGSQVGJWCF6QQZ9"));
        assert!(is_bolt12_offer("Lno1MixedCase"));
        assert!(!is_bolt12_offer("lnbc1qgsqvgjwcf6qqz9"));
        assert!(!is_bolt12_offer("lno"));
        assert!(!is_bolt12_offer(""));
    }

    #[test]
    fn bolt12_offer_parts_parsing() {
        let (offer, amt) = parse_bolt12_offer_parts("lno1abc123");
        assert_eq!(offer, "lno1abc123");
        assert_eq!(amt, None);

        let (offer, amt) = parse_bolt12_offer_parts("lno1abc123?amount=500");
        assert_eq!(offer, "lno1abc123");
        assert_eq!(amt, Some(500));

        let (offer, amt) = parse_bolt12_offer_parts("LNO1ABC?amount=42");
        assert_eq!(offer, "LNO1ABC");
        assert_eq!(amt, Some(42));
    }

    #[test]
    fn local_only_checks() {
        // Already local-only
        assert!(
            Input::WalletShowSeed {
                id: "t".into(),
                wallet: "w".into(),
            }
            .is_local_only()
        );

        assert!(
            Input::WalletClose {
                id: "t".into(),
                wallet: "w".into(),
                dangerously_skip_balance_check_and_may_lose_money: true,
            }
            .is_local_only()
        );

        assert!(
            !Input::WalletClose {
                id: "t".into(),
                wallet: "w".into(),
                dangerously_skip_balance_check_and_may_lose_money: false,
            }
            .is_local_only()
        );

        // Limit write ops
        assert!(
            Input::LimitAdd {
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
            .is_local_only()
        );

        assert!(
            Input::LimitRemove {
                id: "t".into(),
                rule_id: "r_1".into(),
            }
            .is_local_only()
        );

        assert!(
            Input::LimitSet {
                id: "t".into(),
                limits: vec![],
            }
            .is_local_only()
        );

        // Limit read is NOT local-only
        assert!(!Input::LimitList { id: "t".into() }.is_local_only());

        // Wallet config write ops
        assert!(
            Input::WalletConfigSet {
                id: "t".into(),
                wallet: "w".into(),
                label: None,
                rpc_endpoints: vec![],
                chain_id: None,
            }
            .is_local_only()
        );

        assert!(
            Input::WalletConfigTokenAdd {
                id: "t".into(),
                wallet: "w".into(),
                symbol: "dai".into(),
                address: "0x".into(),
                decimals: 18,
            }
            .is_local_only()
        );

        assert!(
            Input::WalletConfigTokenRemove {
                id: "t".into(),
                wallet: "w".into(),
                symbol: "dai".into(),
            }
            .is_local_only()
        );

        // Wallet config read is NOT local-only
        assert!(
            !Input::WalletConfigShow {
                id: "t".into(),
                wallet: "w".into(),
            }
            .is_local_only()
        );

        // Restore (seed over RPC)
        assert!(
            Input::Restore {
                id: "t".into(),
                wallet: "w".into(),
            }
            .is_local_only()
        );

        // Reconcile reservation — operator action, never via RPC
        assert!(
            Input::ReconcileReservation {
                id: "t".into(),
                reservation_id: 1,
                action: ReconcileAction::Confirm,
                reason: "manual override".into(),
            }
            .is_local_only()
        );
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
    fn version_output_includes_json_protocol_version() {
        let out = Output::Version {
            version: "0.1.0".to_string(),
            protocol_version: JSON_PROTOCOL_VERSION,
            trace: PongTrace {
                uptime_s: 1,
                requests_total: 2,
                in_flight: 0,
            },
        };
        let value = serde_json::to_value(out).expect("serialize version output");
        assert_eq!(
            value.get("protocol_version").and_then(|v| v.as_u64()),
            Some(JSON_PROTOCOL_VERSION as u64)
        );
    }

    #[test]
    fn debug_output_redacts_config_secrets() {
        let mut afpay_rpc = std::collections::HashMap::new();
        afpay_rpc.insert(
            "wallet-server".to_string(),
            AfpayRpcConfig {
                endpoint: "http://127.0.0.1:9400".to_string(),
                endpoint_secret: Some("downstream-secret-value".to_string()),
            },
        );
        let config = RuntimeConfig {
            rpc_secret: Some("rpc-secret-value".to_string()),
            postgres_url_secret: Some("postgres-secret-value".to_string()),
            exchange_rate: Some(ExchangeRateConfig {
                ttl_s: 60,
                sources: vec![ExchangeRateSource {
                    source_type: ExchangeRateSourceType::Generic,
                    endpoint: "https://rates.example".to_string(),
                    api_key_secret: Some("exchange-secret-value".to_string()),
                }],
            }),
            afpay_rpc,
            ..RuntimeConfig::default()
        };
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("rpc-secret-value"));
        assert!(!rendered.contains("postgres-secret-value"));
        assert!(!rendered.contains("downstream-secret-value"));
        assert!(!rendered.contains("exchange-secret-value"));
        assert!(rendered.contains("***"));
    }

    #[test]
    fn debug_output_redacts_wallet_request_secrets() {
        let wallet_request = WalletCreateRequest {
            label: "default".to_string(),
            mint_url: None,
            rpc_endpoints: vec![],
            chain_id: None,
            mnemonic_secret: Some("wallet-seed-secret".to_string()),
            btc_esplora_url: None,
            btc_network: None,
            btc_address_type: None,
            btc_backend: None,
            btc_core_url: None,
            btc_core_auth_secret: Some("btc-core-secret".to_string()),
            btc_electrum_url: None,
            sol_cluster: None,
        };
        let ln_request = LnWalletCreateRequest {
            backend: LnWalletBackend::Nwc,
            label: Some("ln".to_string()),
            nwc_uri_secret: Some("nwc-uri-secret".to_string()),
            endpoint_url: None,
            password_secret: Some("password-secret".to_string()),
            admin_key_secret: Some("admin-secret".to_string()),
        };
        let rendered = format!("{wallet_request:?} {ln_request:?}");
        assert!(!rendered.contains("wallet-seed-secret"));
        assert!(!rendered.contains("btc-core-secret"));
        assert!(!rendered.contains("nwc-uri-secret"));
        assert!(!rendered.contains("password-secret"));
        assert!(!rendered.contains("admin-secret"));
        assert!(rendered.contains("***"));
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

    #[test]
    fn request_accepts_plain_input_json_without_dry_run_field() {
        let json = r#"{"code":"version"}"#;
        let request: Request = serde_json::from_str(json).expect("plain input parses as Request");
        assert!(!request.dry_run);
        assert!(matches!(request.input, Input::Version));
    }

    #[test]
    fn request_parses_dry_run_alongside_flattened_input() {
        let json = r#"{
            "dry_run": true,
            "code": "send",
            "id": "t",
            "to": "0xabc",
            "amount": {"value": 1000, "token": "eth"}
        }"#;
        let request: Request = serde_json::from_str(json).expect("dry_run request parses");
        assert!(request.dry_run);
        match request.input {
            Input::Send { amount, to, .. } => {
                assert_eq!(to, "0xabc");
                let amount = amount.expect("amount field deserialized");
                assert_eq!(amount.value, 1000);
                assert_eq!(amount.token, "eth");
            }
            other => panic!("expected Send, got {other:?}"),
        }
    }

    #[test]
    fn send_chain_id_parses_and_defaults_to_none() {
        let with_chain = r#"{"code":"send","id":"t","to":"0xabc","chain_id":42161}"#;
        let input: Input = serde_json::from_str(with_chain).expect("with chain_id parses");
        match input {
            Input::Send { chain_id, .. } => assert_eq!(chain_id, Some(42161)),
            other => panic!("expected Send, got {other:?}"),
        }
        let without = r#"{"code":"send","id":"t","to":"0xabc"}"#;
        let input: Input = serde_json::from_str(without).expect("without chain_id parses");
        match input {
            Input::Send { chain_id, .. } => assert_eq!(chain_id, None),
            other => panic!("expected Send, got {other:?}"),
        }
    }

    #[test]
    fn send_amount_field_is_optional() {
        let json = r#"{"code":"send","id":"t","to":"bitcoin:bc1q?amount=100"}"#;
        let input: Input = serde_json::from_str(json).expect("send without amount parses");
        match input {
            Input::Send { amount, .. } => assert!(amount.is_none()),
            other => panic!("expected Send, got {other:?}"),
        }
    }
}
