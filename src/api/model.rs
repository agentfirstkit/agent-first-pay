//! Typed request and result DTOs for the HTTP domain API.
//!
//! These are the single contract source: `api::schema` generates both the
//! OpenAPI components and the standalone JSON Schemas from exactly these
//! types, and `api::server` deserializes every request body/query into them
//! and re-reads every success payload back through them. A result the
//! dispatcher produces that no longer fits its DTO is reported as
//! `api_contract_violation` rather than shipped, so the committed contract
//! cannot silently drift away from what the daemon actually returns.
//!
//! Request DTOs carry `deny_unknown_fields`. A typo in an agent's body is a
//! 400 naming the field, not a silently ignored instruction.

#![allow(
    dead_code,
    reason = "result DTO fields exist to be validated through Serde and rendered into JSON Schema"
)]

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::store::wallet::WalletMetadata;
use crate::types::{
    Amount, BtcBackend, DownstreamLimitNode, HistoryRecord, Input, LnWalletBackend,
    LnWalletCreateRequest, Network, NetworkBalanceSummary, PayPlanOperation, PlanWarning,
    ReceiveInfo, SpendDebit, SpendLimitStatus, TxStatus, WalletBalanceItem, WalletSummary,
};

// ═══════════════════════════════════════════
// Discovery
// ═══════════════════════════════════════════

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
pub struct HealthResult {
    /// Always `afpay`.
    pub service: String,
    /// Crate version of the process answering this request.
    pub version: String,
    /// Version of the JSON domain protocol the payloads below follow.
    pub protocol_version: u32,
    /// Always `ready`; the process answers this route only once it is serving.
    pub status: String,
}

// ═══════════════════════════════════════════
// Error envelope
// ═══════════════════════════════════════════

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApiErrorEnvelope {
    /// Always `error`.
    pub kind: String,
    pub error: ApiErrorBody,
    pub trace: ApiTrace,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
pub struct ApiErrorBody {
    /// Stable machine code; the HTTP status narrows it to a class.
    pub code: String,
    pub message: String,
    /// Whether an identical retry can succeed without operator action.
    pub retryable: bool,
    /// A safe recovery step, when one exists.
    pub hint: Option<String>,
    /// Minimum delay before retrying, on transient failures that name one.
    pub retry_after_ms: Option<u64>,
    /// Typed conflict/recovery detail. Present on `limit_exceeded` and
    /// `accounting_inconsistent`, whose refusals carry ledger state the
    /// caller needs in order to act.
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApiTrace {
    pub duration_ms: u64,
}

// ═══════════════════════════════════════════
// Wallets
// ═══════════════════════════════════════════

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct WalletListQuery {
    /// Restrict the listing to wallets on one network.
    pub network: Option<Network>,
}

impl WalletListQuery {
    pub fn into_input(self, id: String) -> Input {
        Input::WalletList {
            id,
            network: self.network,
        }
    }
}

/// Closed tagged union: the `network` decides which settings object is legal.
/// Every variant rejects unknown fields, so a Solana setting sent to a
/// Bitcoin wallet is refused instead of dropped.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "network", rename_all = "lowercase", deny_unknown_fields)]
pub enum WalletCreateRequest {
    /// An eCash wallet held against one Cashu mint.
    Cashu {
        /// Human label; wallets are addressed by the id afpay assigns.
        label: Option<String>,
        /// Mint this wallet holds proofs against. Subject to
        /// `allowed_mint_urls` when the operator configured an allowlist.
        mint_url: String,
        /// Restore an existing wallet from its BIP39 mnemonic instead of
        /// generating one.
        mnemonic_secret: Option<String>,
    },
    /// A Lightning wallet backed by NWC, phoenixd, or lnbits.
    Ln {
        label: Option<String>,
        backend: LnWalletBackend,
        /// Backend HTTP endpoint (phoenixd, lnbits).
        endpoint_url: Option<String>,
        /// `nostr+walletconnect://…` URI (nwc).
        nwc_uri_secret: Option<String>,
        /// HTTP password (phoenixd).
        password_secret: Option<String>,
        /// Admin key (lnbits).
        admin_key_secret: Option<String>,
    },
    /// A Solana wallet.
    Sol {
        label: Option<String>,
        /// RPC endpoints tried in order.
        #[serde(default)]
        rpc_endpoints: Vec<String>,
        /// Pin the wallet to a cluster: `mainnet-beta`, `devnet`, or
        /// `testnet`. Sends refuse when the active endpoint resolves to a
        /// different cluster.
        cluster: Option<String>,
        mnemonic_secret: Option<String>,
    },
    /// An EVM wallet.
    Evm {
        label: Option<String>,
        #[serde(default)]
        rpc_endpoints: Vec<String>,
        /// Chain the wallet lives on; defaults to 8453 (Base).
        chain_id: Option<u64>,
        mnemonic_secret: Option<String>,
    },
    /// A Bitcoin on-chain wallet.
    Btc {
        label: Option<String>,
        /// Chain-source backend; defaults to `esplora`.
        backend: Option<BtcBackend>,
        /// Esplora API URL (esplora backend).
        esplora_url: Option<String>,
        /// Bitcoin Core RPC URL (core-rpc backend).
        core_url: Option<String>,
        /// Bitcoin Core RPC `user:pass` (core-rpc backend).
        core_auth_secret: Option<String>,
        /// Electrum server URL (electrum backend).
        electrum_url: Option<String>,
        /// `mainnet` or `signet`.
        btc_network: Option<String>,
        /// `taproot` or `segwit`.
        address_type: Option<String>,
        mnemonic_secret: Option<String>,
    },
}

impl WalletCreateRequest {
    pub fn into_input(self, id: String, idempotency_key: String) -> Input {
        let idempotency_key = Some(idempotency_key);
        match self {
            Self::Cashu {
                label,
                mint_url,
                mnemonic_secret,
            } => Input::WalletCreate {
                id,
                network: Network::Cashu,
                label,
                mint_url: Some(mint_url),
                rpc_endpoints: Vec::new(),
                chain_id: None,
                mnemonic_secret,
                btc_esplora_url: None,
                btc_network: None,
                btc_address_type: None,
                btc_backend: None,
                btc_core_url: None,
                btc_core_auth_secret: None,
                btc_electrum_url: None,
                sol_cluster: None,
                idempotency_key,
            },
            Self::Ln {
                label,
                backend,
                endpoint_url,
                nwc_uri_secret,
                password_secret,
                admin_key_secret,
            } => Input::LnWalletCreate {
                id,
                request: LnWalletCreateRequest {
                    backend,
                    label,
                    nwc_uri_secret,
                    endpoint_url,
                    password_secret,
                    admin_key_secret,
                },
                idempotency_key,
            },
            Self::Sol {
                label,
                rpc_endpoints,
                cluster,
                mnemonic_secret,
            } => Input::WalletCreate {
                id,
                network: Network::Sol,
                label,
                mint_url: None,
                rpc_endpoints,
                chain_id: None,
                mnemonic_secret,
                btc_esplora_url: None,
                btc_network: None,
                btc_address_type: None,
                btc_backend: None,
                btc_core_url: None,
                btc_core_auth_secret: None,
                btc_electrum_url: None,
                sol_cluster: cluster,
                idempotency_key,
            },
            Self::Evm {
                label,
                rpc_endpoints,
                chain_id,
                mnemonic_secret,
            } => Input::WalletCreate {
                id,
                network: Network::Evm,
                label,
                mint_url: None,
                rpc_endpoints,
                chain_id,
                mnemonic_secret,
                btc_esplora_url: None,
                btc_network: None,
                btc_address_type: None,
                btc_backend: None,
                btc_core_url: None,
                btc_core_auth_secret: None,
                btc_electrum_url: None,
                sol_cluster: None,
                idempotency_key,
            },
            Self::Btc {
                label,
                backend,
                esplora_url,
                core_url,
                core_auth_secret,
                electrum_url,
                btc_network,
                address_type,
                mnemonic_secret,
            } => Input::WalletCreate {
                id,
                network: Network::Btc,
                label,
                mint_url: None,
                rpc_endpoints: Vec::new(),
                chain_id: None,
                mnemonic_secret,
                btc_esplora_url: esplora_url,
                btc_network,
                btc_address_type: address_type,
                btc_backend: backend,
                btc_core_url: core_url,
                btc_core_auth_secret: core_auth_secret,
                btc_electrum_url: electrum_url,
                sol_cluster: None,
                idempotency_key,
            },
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
pub struct WalletListResult {
    pub wallets: Vec<WalletSummary>,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
pub struct WalletCreatedResult {
    /// Wallet id afpay assigned; address every other route by it.
    pub wallet: String,
    pub network: Network,
    /// Receiving address, or the backend node identity for Lightning.
    pub address: String,
    /// Redacted on this transport. A generated mnemonic is readable only
    /// through the local CLI, never over HTTP.
    pub mnemonic_secret: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
pub struct WalletDetailResult {
    pub wallet: String,
    /// Stored configuration. Secret-suffixed members are redacted.
    pub config: WalletMetadata,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
pub struct WalletClosedResult {
    pub wallet: String,
}

// ═══════════════════════════════════════════
// Balances
// ═══════════════════════════════════════════

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct BalanceListQuery {
    /// One wallet; omit to read every wallet.
    pub wallet: Option<String>,
    /// Restrict to one network.
    pub network: Option<Network>,
    /// Verify balances against the source of truth (Cashu proof check)
    /// instead of trusting local state. Slower and authoritative.
    pub check: bool,
}

impl BalanceListQuery {
    pub fn into_input(self, id: String) -> Input {
        Input::Balance {
            id,
            wallet: self.wallet,
            network: self.network,
            check: self.check,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
pub struct BalanceListResult {
    /// One self-describing entry per wallet; a wallet whose provider failed
    /// carries its own `error` rather than dropping out of the list.
    pub wallets: Vec<WalletBalanceItem>,
    /// Totals grouped by network and unit.
    #[serde(default)]
    pub summary: Vec<NetworkBalanceSummary>,
}

// ═══════════════════════════════════════════
// Receives
// ═══════════════════════════════════════════

/// Hold the request open until the receive is paid. Omit the object to
/// return the address or invoice immediately.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ReceiveWait {
    /// Give up after this many seconds.
    pub timeout_s: Option<u64>,
    /// Delay between polls.
    pub poll_interval_ms: Option<u64>,
    /// Cap on transactions scanned per poll for chain-scanning backends.
    pub sync_limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReceiveCreateRequest {
    /// Wallet that will receive the funds.
    pub wallet: String,
    /// Network of the wallet, when its id is ambiguous.
    pub network: Option<Network>,
    /// Amount to request. Required for Lightning invoices; on-chain
    /// addresses may omit it.
    pub amount: Option<Amount>,
    /// Memo carried on-chain or in the invoice.
    pub onchain_memo: Option<String>,
    /// Confirmations required before the receive counts as paid.
    pub min_confirmations: Option<u32>,
    /// Base58 reference key to bind an order to this receive (Solana).
    pub reference: Option<String>,
    /// Present to wait for payment before responding.
    pub wait: Option<ReceiveWait>,
}

impl ReceiveCreateRequest {
    pub fn into_input(self, id: String, idempotency_key: String) -> Input {
        let wait_until_paid = self.wait.is_some();
        let wait = self.wait.unwrap_or_default();
        Input::Receive {
            id,
            wallet: self.wallet,
            network: self.network,
            amount: self.amount,
            onchain_memo: self.onchain_memo,
            wait_until_paid,
            wait_timeout_s: wait.timeout_s,
            wait_poll_interval_ms: wait.poll_interval_ms,
            wait_sync_limit: wait.sync_limit,
            write_qr_svg_file: false,
            min_confirmations: self.min_confirmations,
            reference: self.reference,
            idempotency_key: Some(idempotency_key),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReceiveClaimRequest {
    /// Wallet the quote was created against.
    pub wallet: String,
}

impl ReceiveClaimRequest {
    pub fn into_input(self, id: String, quote_id: String) -> Input {
        Input::ReceiveClaim {
            id,
            wallet: self.wallet,
            quote_id,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
pub struct ReceiveResult {
    pub wallet: String,
    /// The address, invoice, and/or mint quote id to hand to the payer.
    pub receive_info: ReceiveInfo,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
pub struct ReceiveClaimedResult {
    pub wallet: String,
    pub amount: Amount,
}

// ═══════════════════════════════════════════
// Sends
// ═══════════════════════════════════════════

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendPlanRequest {
    /// Address, BOLT11 invoice, BOLT12 offer, or payment URI.
    pub to: String,
    /// Source wallet; omit to let afpay select one. The plan names the wallet
    /// it actually picked.
    pub wallet: Option<String>,
    /// Network to send on, when the target is ambiguous.
    pub network: Option<Network>,
    /// Amount in the network's base unit. Optional only when `to` already
    /// carries one.
    pub amount: Option<Amount>,
    /// Memo carried with the transaction.
    pub onchain_memo: Option<String>,
    /// Local-only bookkeeping annotation; never sent to the network.
    pub local_memo: Option<BTreeMap<String, String>>,
    /// Restrict Cashu wallet selection to these mints, tried in order.
    pub mints: Option<Vec<String>>,
    /// Pin the EVM chain. A wallet on a different chain refuses with
    /// `forbidden` (`wrong_chain`) at plan time instead of broadcasting.
    pub chain_id: Option<u64>,
}

impl SendPlanRequest {
    pub fn into_input(self, id: String) -> Input {
        Input::SendPlan {
            id,
            wallet: self.wallet,
            network: self.network,
            to: self.to,
            amount: self.amount,
            onchain_memo: self.onchain_memo,
            local_memo: self.local_memo,
            mints: self.mints,
            chain_id: self.chain_id,
        }
    }
}

/// The whole body of a confirm: the id of the plan that was reviewed.
///
/// The payment itself is not repeated here, and cannot be. What executes is
/// read out of the stored plan, so a caller cannot approve one payment and
/// submit another.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PayConfirmRequest {
    /// The `plan_id` returned by the matching plan operation.
    pub plan_id: String,
}

impl PayConfirmRequest {
    pub fn into_input(
        self,
        id: String,
        expect: PayPlanOperation,
        idempotency_key: String,
    ) -> Input {
        Input::PayConfirm {
            id,
            plan_id: self.plan_id,
            expect: Some(expect),
            idempotency_key: Some(idempotency_key),
        }
    }
}

/// A payment resolved down to what it would actually do, and the id that
/// submits it. Nothing has moved.
#[derive(Debug, Deserialize, JsonSchema, Serialize)]
pub struct PayPlanResult {
    /// Submit this to the matching confirm operation. Single-use.
    pub plan_id: String,
    /// `send` or `cashu_send` — which confirm route accepts this plan.
    pub operation: String,
    pub network: Network,
    /// The wallet afpay picked, which may differ from the one requested when
    /// the request left it open.
    pub wallet: String,
    /// The destination exactly as the payment will use it, normalised. Absent
    /// for a Cashu token mint.
    pub to: Option<String>,
    /// What leaves the wallet, in the network's base unit.
    pub amount_native: u64,
    /// What the network is expected to charge on top. An estimate: the chain
    /// charges what it charges, and the reservation is taken against this.
    pub fee_estimate_native: u64,
    /// Unit both amounts are quoted in (`sats`, `lamports`, `gwei`).
    pub fee_unit: String,
    /// The spend-limit budgets confirming this plan would debit.
    #[serde(default)]
    pub spend_debits: Vec<SpendDebit>,
    /// Safety signals that must be considered before confirming the plan.
    #[serde(default)]
    pub warnings: Vec<PlanWarning>,
    /// After this the plan is refused and the payment must be re-planned.
    pub expires_at_epoch_ms: u64,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
pub struct SendResult {
    pub wallet: String,
    /// Network transaction id, or the Cashu melt id.
    pub transaction_id: String,
    pub amount: Amount,
    pub fee: Option<Amount>,
    /// Lightning payment preimage, when the backend returned one.
    pub preimage: Option<String>,
    /// Spend-ledger reservations this payment consumed. Empty when no spend
    /// limit applied.
    #[serde(default)]
    pub reservation_ids: Vec<u64>,
}

// ═══════════════════════════════════════════
// Cashu bearer tokens
// ═══════════════════════════════════════════

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CashuTokenPlanRequest {
    /// Amount to move into the token.
    pub amount: Amount,
    /// Source wallet; omit to let afpay select one. The plan names the wallet
    /// it actually picked.
    pub wallet: Option<String>,
    pub onchain_memo: Option<String>,
    pub local_memo: Option<BTreeMap<String, String>>,
    /// Restrict wallet selection to these mints, tried in order.
    pub mints: Option<Vec<String>>,
}

impl CashuTokenPlanRequest {
    pub fn into_input(self, id: String) -> Input {
        Input::CashuSendPlan {
            id,
            wallet: self.wallet,
            amount: self.amount,
            onchain_memo: self.onchain_memo,
            local_memo: self.local_memo,
            mints: self.mints,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
pub struct CashuTokenResult {
    pub wallet: String,
    pub transaction_id: String,
    pub status: TxStatus,
    pub fee: Option<Amount>,
    /// The bearer token. Whoever holds this string holds the funds.
    pub token: String,
    #[serde(default)]
    pub reservation_ids: Vec<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CashuRedemptionRequest {
    /// The bearer token to redeem.
    pub token: String,
    /// Destination wallet; omit to let afpay select one on the token's mint.
    pub wallet: Option<String>,
}

impl CashuRedemptionRequest {
    pub fn into_input(self, id: String) -> Input {
        Input::CashuReceive {
            id,
            wallet: self.wallet,
            token: self.token,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
pub struct CashuRedemptionResult {
    pub wallet: String,
    pub amount: Amount,
    pub memo: Option<String>,
}

// ═══════════════════════════════════════════
// Transactions
// ═══════════════════════════════════════════

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TransactionListQuery {
    pub wallet: Option<String>,
    pub network: Option<Network>,
    /// Exact on-chain memo to filter by.
    pub onchain_memo: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    /// Include records created at or after this epoch second.
    pub since_epoch_s: Option<u64>,
    /// Include records created before this epoch second.
    pub until_epoch_s: Option<u64>,
}

impl TransactionListQuery {
    pub fn into_input(self, id: String) -> Input {
        Input::HistoryList {
            id,
            wallet: self.wallet,
            network: self.network,
            onchain_memo: self.onchain_memo,
            limit: self.limit,
            offset: self.offset,
            since_epoch_s: self.since_epoch_s,
            until_epoch_s: self.until_epoch_s,
        }
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TransactionSyncRequest {
    pub wallet: Option<String>,
    pub network: Option<Network>,
    /// Cap on records pulled from each provider.
    pub limit: Option<usize>,
}

impl TransactionSyncRequest {
    pub fn into_input(self, id: String) -> Input {
        Input::HistoryUpdate {
            id,
            wallet: self.wallet,
            network: self.network,
            limit: self.limit,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
pub struct TransactionListResult {
    pub items: Vec<HistoryRecord>,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
pub struct TransactionStatusResult {
    pub transaction_id: String,
    pub status: TxStatus,
    pub confirmations: Option<u32>,
    pub preimage: Option<String>,
    /// The stored record, when afpay holds one for this transaction.
    pub item: Option<HistoryRecord>,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
pub struct TransactionSyncResult {
    pub wallets_synced: usize,
    pub records_scanned: usize,
    pub records_added: usize,
    pub records_updated: usize,
}

// ═══════════════════════════════════════════
// Spend limits
// ═══════════════════════════════════════════

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
pub struct SpendLimitListResult {
    /// Every rule this node enforces, with the spend already consumed in the
    /// current window.
    pub limits: Vec<SpendLimitStatus>,
    /// Limits enforced by afpay nodes this one forwards to.
    #[serde(default)]
    pub downstream: Vec<DownstreamLimitNode>,
}
