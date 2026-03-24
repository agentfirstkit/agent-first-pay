#[cfg(feature = "rest")]
use crate::mode::rest::RestInit;
use crate::mode::rpc::RpcInit;
use crate::types::*;
use agent_first_data::OutputFormat;
use clap::{Parser, Subcommand, ValueEnum};
use std::collections::BTreeMap;
use std::io::Write;

// ═══════════════════════════════════════════
// Mode Dispatch Types
// ═══════════════════════════════════════════

pub struct CliError {
    pub message: String,
    pub hint: Option<String>,
}

impl From<String> for CliError {
    fn from(message: String) -> Self {
        Self {
            message,
            hint: None,
        }
    }
}

pub enum Mode {
    Cli(Box<CliRequest>),
    Pipe(PipeInit),
    Interactive(InteractiveInit),
    Rpc(RpcInit),
    #[cfg(feature = "rest")]
    Rest(RestInit),
}

pub struct CliRequest {
    pub input: Input,
    pub output: OutputFormat,
    pub log: Vec<String>,
    pub data_dir: Option<String>,
    pub rpc_endpoint: Option<String>,
    pub rpc_secret: Option<String>,
    pub startup_argv: Vec<String>,
    pub startup_args: serde_json::Value,
    pub startup_requested: bool,
    pub dry_run: bool,
}

pub struct PipeInit {
    pub output: OutputFormat,
    pub log: Vec<String>,
    pub data_dir: Option<String>,
    pub startup_argv: Vec<String>,
    pub startup_args: serde_json::Value,
    pub startup_requested: bool,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InteractiveFrontend {
    Interactive,
    Tui,
}

#[allow(dead_code)]
#[derive(Clone)]
pub struct InteractiveInit {
    pub frontend: InteractiveFrontend,
    pub output: OutputFormat,
    pub log: Vec<String>,
    pub data_dir: Option<String>,
    pub rpc_endpoint: Option<String>,
    pub rpc_secret: Option<String>,
}

// RpcInit is defined in mode::rpc and re-used here

// ═══════════════════════════════════════════
// Memo Helpers
// ═══════════════════════════════════════════

fn parse_memo_kv(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, v)) => {
            if k.is_empty() {
                return Err("memo key must not be empty".into());
            }
            Ok((k.to_string(), v.to_string()))
        }
        None => Ok(("note".to_string(), s.to_string())),
    }
}

fn memo_vec_to_map(v: Vec<(String, String)>) -> Option<BTreeMap<String, String>> {
    if v.is_empty() {
        None
    } else {
        Some(v.into_iter().collect())
    }
}

// ═══════════════════════════════════════════
// Shared Arg Structs
// ═══════════════════════════════════════════

#[derive(clap::Args, Clone)]
struct CommonSendArgs {
    /// Source wallet ID (auto-selected if omitted)
    #[arg(long)]
    wallet: Option<String>,
    /// On-chain memo (sent with the transaction)
    #[arg(long = "onchain-memo")]
    onchain_memo: Option<String>,
    /// Local bookkeeping annotation (repeatable: --local-memo purpose=donation --local-memo note=coffee)
    #[arg(long = "local-memo", value_parser = parse_memo_kv)]
    local_memo: Vec<(String, String)>,
}

#[derive(clap::Args, Clone)]
struct CommonReceiveArgs {
    /// Wallet ID (auto-selected if omitted)
    #[arg(long)]
    wallet: Option<String>,
    /// Wait for payment / matching receive transaction
    #[arg(long)]
    wait: bool,
    /// Timeout in seconds for --wait
    #[arg(long = "wait-timeout-s")]
    wait_timeout_s: Option<u64>,
    /// Poll interval in milliseconds for --wait
    #[arg(long = "wait-poll-interval-ms")]
    wait_poll_interval_ms: Option<u64>,
    /// Write receive QR payload to an SVG file
    #[arg(long = "qr-svg-file", default_value_t = false)]
    qr_svg_file: bool,
}

// ═══════════════════════════════════════════
// Clap Definitions
// ═══════════════════════════════════════════

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum RuntimeMode {
    Cli,
    Pipe,
    Interactive,
    Tui,
    Rpc,
    #[cfg(feature = "rest")]
    Rest,
}

#[derive(Parser)]
#[command(
    name = "afpay",
    bin_name = "afpay",
    version,
    about = "Agent-first cryptocurrency micropayment tool"
)]
pub struct AfpayCli {
    /// Run mode
    #[arg(long, value_enum, default_value_t = RuntimeMode::Cli)]
    mode: RuntimeMode,

    /// Connect to remote RPC daemon (cli mode)
    #[arg(long = "rpc-endpoint")]
    rpc_endpoint: Option<String>,

    /// Listen address for RPC daemon (rpc mode)
    #[arg(long = "rpc-listen", default_value = "0.0.0.0:9400")]
    rpc_listen: String,

    /// RPC encryption secret
    #[arg(long = "rpc-secret")]
    rpc_secret: Option<String>,

    /// Listen address for REST HTTP server (rest mode)
    #[arg(long = "rest-listen", default_value = "0.0.0.0:9401")]
    rest_listen: String,

    /// API key for REST bearer authentication (rest mode)
    #[arg(long = "rest-api-key")]
    rest_api_key: Option<String>,

    /// Wallet and data directory
    #[arg(long = "data-dir")]
    data_dir: Option<String>,

    /// Output format
    #[arg(long, default_value = "json")]
    output: String,

    /// Log filters (comma-separated)
    #[arg(long = "log", value_delimiter = ',')]
    log: Vec<String>,

    /// Preview the command without executing it
    #[arg(long)]
    dry_run: bool,

    #[command(subcommand)]
    command: Option<PayCommand>,
}

#[derive(Subcommand)]
enum PayCommand {
    /// Global (cross-network) operations
    Global {
        #[command(subcommand)]
        action: GlobalCommand,
    },
    /// Cashu operations
    Cashu {
        #[command(subcommand)]
        action: CashuCommand,
    },
    /// Lightning Network operations (NWC, phoenixd, LNbits)
    Ln {
        #[command(subcommand)]
        action: LnCommand,
    },
    /// Solana operations
    Sol {
        #[command(subcommand)]
        action: SolCommand,
    },
    /// EVM chain operations (Base, Arbitrum)
    Evm {
        #[command(subcommand)]
        action: EvmCommand,
    },
    /// Bitcoin on-chain operations
    Btc {
        #[command(subcommand)]
        action: BtcCommand,
    },
    /// List all wallets (cross-network)
    Wallet {
        #[command(subcommand)]
        action: WalletTopAction,
    },
    /// All wallets balance (cross-network)
    Balance {
        /// Wallet ID (omit to show all wallets)
        #[arg(long)]
        wallet: Option<String>,
        /// Filter by network: cashu, ln, sol, evm
        #[arg(long, value_enum)]
        network: Option<CliNetwork>,
        /// Verify cashu proofs against mint (slower but accurate; cashu only)
        #[arg(long = "cashu-check")]
        cashu_check: bool,
    },
    /// History queries
    #[command(name = "history")]
    History {
        #[command(subcommand)]
        action: HistoryAction,
    },
    /// Spend limit list and remove (cross-network)
    Limit {
        #[command(subcommand)]
        action: LimitAction,
    },
}

#[derive(Subcommand)]
enum GlobalCommand {
    /// Global spend limit (USD cents)
    Limit {
        #[command(subcommand)]
        action: GlobalLimitAction,
    },
    /// Global runtime configuration
    Config {
        #[command(subcommand)]
        action: GlobalConfigAction,
    },
}

#[derive(Subcommand)]
enum GlobalConfigAction {
    /// Show current runtime configuration
    Show,
    /// Update runtime configuration
    Set {
        /// Log filters (comma-separated: startup,cashu,ln,sol,wallet,all,off)
        #[arg(long, value_delimiter = ',')]
        log: Option<Vec<String>>,
    },
}

#[derive(Subcommand)]
enum GlobalLimitAction {
    /// Add a global spend limit (USD cents)
    Add {
        /// Time window: e.g. 30m, 1h, 24h, 7d
        #[arg(long)]
        window: String,
        /// Maximum spend in USD cents
        #[arg(long)]
        max_spend: u64,
    },
}

/// Per-wallet configuration for cashu, ln, btc (label only).
#[derive(Subcommand)]
enum SimpleWalletConfigAction {
    /// Show current wallet configuration
    Show,
    /// Update wallet settings
    Set {
        /// New label
        #[arg(long)]
        label: Option<String>,
    },
}

/// Per-wallet configuration for sol (label + rpc-endpoint + token management).
#[derive(Subcommand)]
enum SolWalletConfigAction {
    /// Show current wallet configuration
    Show,
    /// Update wallet settings
    Set {
        /// New label
        #[arg(long)]
        label: Option<String>,
        /// Replace RPC endpoint(s)
        #[arg(long = "rpc-endpoint")]
        rpc_endpoint: Vec<String>,
    },
    /// Register a custom token for balance tracking
    #[command(name = "token-add")]
    TokenAdd {
        /// Token symbol (e.g. dai)
        #[arg(long)]
        symbol: String,
        /// Token contract address
        #[arg(long)]
        address: String,
        /// Token decimals
        #[arg(long, default_value_t = 6)]
        decimals: u8,
    },
    /// Unregister a custom token
    #[command(name = "token-remove")]
    TokenRemove {
        /// Token symbol to remove
        #[arg(long)]
        symbol: String,
    },
}

/// Per-wallet configuration for evm (label + rpc-endpoint + chain-id + token management).
#[derive(Subcommand)]
enum EvmWalletConfigAction {
    /// Show current wallet configuration
    Show,
    /// Update wallet settings
    Set {
        /// New label
        #[arg(long)]
        label: Option<String>,
        /// Replace RPC endpoint(s)
        #[arg(long = "rpc-endpoint")]
        rpc_endpoint: Vec<String>,
        /// EVM chain ID
        #[arg(long = "chain-id")]
        chain_id: Option<u64>,
    },
    /// Register a custom token for balance tracking
    #[command(name = "token-add")]
    TokenAdd {
        /// Token symbol (e.g. dai)
        #[arg(long)]
        symbol: String,
        /// Token contract address
        #[arg(long)]
        address: String,
        /// Token decimals
        #[arg(long, default_value_t = 6)]
        decimals: u8,
    },
    /// Unregister a custom token
    #[command(name = "token-remove")]
    TokenRemove {
        /// Token symbol to remove
        #[arg(long)]
        symbol: String,
    },
}

/// Limit actions for cashu, ln, btc (sats-only networks — no --token flag).
#[derive(Subcommand)]
enum SimpleLimitAction {
    /// Add a network or wallet spend limit
    Add {
        /// Time window: e.g. 30m, 1h, 24h, 7d
        #[arg(long)]
        window: String,
        /// Maximum spend in base units
        #[arg(long)]
        max_spend: u64,
    },
}

/// Limit actions for sol, evm (multi-token networks — has --token flag).
#[derive(Subcommand)]
enum TokenLimitAction {
    /// Add a network or wallet spend limit
    Add {
        /// Token: native, usdc, usdt
        #[arg(long)]
        token: Option<String>,
        /// Time window: e.g. 30m, 1h, 24h, 7d
        #[arg(long)]
        window: String,
        /// Maximum spend in base units
        #[arg(long)]
        max_spend: u64,
    },
}

#[derive(Subcommand)]
enum CashuCommand {
    /// Send P2P cashu token (outputs token string; for Lightning, use send-to-ln)
    #[command(name = "send")]
    Send {
        /// Amount in sats (base units)
        #[arg(long = "amount-sats")]
        amount_sats: u64,
        /// Restrict to wallets on these mint URLs (tried in order)
        #[arg(long = "cashu-mint")]
        mint_url: Vec<String>,
        #[command(flatten)]
        common: CommonSendArgs,
        /// Hidden: catches --to and redirects to send-to-ln
        #[arg(long, hide = true)]
        to: Option<String>,
    },
    /// Receive cashu token
    #[command(name = "receive")]
    Receive {
        /// Cashu token string
        token: String,
        /// Wallet ID (auto-matched from token if omitted)
        #[arg(long)]
        wallet: Option<String>,
    },
    /// Send cashu to a Lightning invoice
    #[command(name = "send-to-ln")]
    SendToLn {
        /// Lightning invoice (bolt11)
        #[arg(long)]
        to: String,
        #[command(flatten)]
        common: CommonSendArgs,
    },
    /// Create Lightning invoice to receive cashu from LN
    #[command(name = "receive-from-ln")]
    ReceiveFromLn {
        /// Amount in sats (base units)
        #[arg(long = "amount-sats")]
        amount_sats: Option<u64>,
        /// On-chain memo (sent with the transaction)
        #[arg(long = "onchain-memo")]
        onchain_memo: Option<String>,
        #[command(flatten)]
        common: CommonReceiveArgs,
    },
    /// Claim minted tokens from a receive-from-ln quote
    #[command(name = "receive-from-ln-claim")]
    ReceiveFromLnClaim {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
        /// Quote ID / payment hash from deposit
        #[arg(long = "ln-quote-id")]
        ln_quote_id: String,
    },
    /// Check cashu balance
    Balance {
        /// Wallet ID (omit to show all cashu wallets)
        #[arg(long)]
        wallet: Option<String>,
        /// Verify proofs against mint (slower but accurate)
        #[arg(long)]
        check: bool,
    },
    /// Wallet management
    Wallet {
        #[command(subcommand)]
        action: CashuWalletAction,
    },
    /// Spend limit for cashu network or a specific cashu wallet
    Limit {
        /// Wallet ID (omit for network-level limit)
        #[arg(long)]
        wallet: Option<String>,
        #[command(subcommand)]
        action: SimpleLimitAction,
    },
    /// Per-wallet configuration
    Config {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
        #[command(subcommand)]
        action: SimpleWalletConfigAction,
    },
}

#[derive(Subcommand)]
enum CashuWalletAction {
    /// Create a new cashu wallet
    Create {
        /// Cashu mint URL
        #[arg(long = "cashu-mint")]
        mint_url: String,
        /// Optional label
        #[arg(long)]
        label: Option<String>,
        /// Existing BIP39 mnemonic secret to restore this wallet
        #[arg(long = "mnemonic-secret")]
        mnemonic_secret: Option<String>,
    },
    /// Close a zero-balance cashu wallet
    Close {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
        /// Dangerously skip balance checks when closing wallet
        #[arg(long = "dangerously-skip-balance-check-and-may-lose-money")]
        dangerously_skip_balance_check_and_may_lose_money: bool,
    },
    /// List cashu wallets
    List,
    /// Dangerously show wallet seed mnemonic (12 BIP39 words)
    #[command(name = "dangerously-show-seed")]
    ShowSeed {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
    },
    /// Restore lost proofs from mint (fixes counter/proof sync issues)
    Restore {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
    },
}

#[derive(Subcommand)]
enum LnCommand {
    /// Wallet management
    Wallet {
        #[command(subcommand)]
        action: LnWalletAction,
    },
    /// Pay a Lightning invoice or BOLT12 offer
    #[command(name = "send")]
    Send {
        /// BOLT11 invoice or BOLT12 offer (lno1…) to pay
        #[arg(long)]
        to: String,
        /// Amount in sats (required for BOLT12 offers, rejected for BOLT11)
        #[arg(long = "amount-sats")]
        amount_sats: Option<u64>,
        #[command(flatten)]
        common: CommonSendArgs,
    },
    /// Create a Lightning invoice (BOLT11) or get a reusable BOLT12 offer
    #[command(name = "receive")]
    Receive {
        /// Amount in sats (omit for BOLT12 offer)
        #[arg(long = "amount-sats")]
        amount_sats: Option<u64>,
        #[command(flatten)]
        common: CommonReceiveArgs,
    },
    /// Check balance
    Balance {
        /// Wallet ID (omit to show all ln wallets)
        #[arg(long)]
        wallet: Option<String>,
    },
    /// Spend limit for ln network or a specific ln wallet
    Limit {
        /// Wallet ID (omit for network-level limit)
        #[arg(long)]
        wallet: Option<String>,
        #[command(subcommand)]
        action: SimpleLimitAction,
    },
    /// Per-wallet configuration
    Config {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
        #[command(subcommand)]
        action: SimpleWalletConfigAction,
    },
}

#[derive(Subcommand)]
enum SolCommand {
    /// Wallet management
    Wallet {
        #[command(subcommand)]
        action: SolWalletAction,
    },
    /// Send SOL or SPL token transfer
    #[command(name = "send")]
    Send {
        /// Recipient Solana address (base58)
        #[arg(long)]
        to: String,
        /// Amount in token base units (lamports for SOL, smallest unit for SPL tokens)
        #[arg(long)]
        amount: u64,
        /// Token: "native" for SOL, "usdc", "usdt", or SPL mint address
        #[arg(long)]
        token: String,
        /// Reference key for order binding (base58-encoded 32 bytes, per strain-payment-method-solana)
        #[arg(long)]
        reference: Option<String>,
        #[command(flatten)]
        common: CommonSendArgs,
    },
    /// Show wallet receive address
    #[command(name = "receive")]
    Receive {
        /// On-chain memo to watch for (used with --wait)
        #[arg(long = "onchain-memo")]
        onchain_memo: Option<String>,
        /// Minimum confirmation depth before considering payment settled (requires --wait)
        #[arg(long = "min-confirmations")]
        min_confirmations: Option<u32>,
        /// Reference key to watch for (base58, used with --wait, per strain-payment-method-solana)
        #[arg(long)]
        reference: Option<String>,
        #[command(flatten)]
        common: CommonReceiveArgs,
    },
    /// Check balance
    Balance {
        /// Wallet ID (omit to show all sol wallets)
        #[arg(long)]
        wallet: Option<String>,
    },
    /// Spend limit for sol network or a specific sol wallet
    Limit {
        /// Wallet ID (omit for network-level limit)
        #[arg(long)]
        wallet: Option<String>,
        #[command(subcommand)]
        action: TokenLimitAction,
    },
    /// Per-wallet configuration
    Config {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
        #[command(subcommand)]
        action: SolWalletConfigAction,
    },
}

#[derive(Subcommand)]
enum SolWalletAction {
    /// Create a new Solana wallet
    Create {
        /// Solana JSON-RPC endpoint (repeat to configure failover order)
        #[arg(long = "sol-rpc-endpoint", required = true)]
        sol_rpc_endpoint: Vec<String>,
        /// Optional label
        #[arg(long)]
        label: Option<String>,
    },
    /// Close a Solana wallet
    Close {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
        /// Dangerously skip balance checks when closing wallet
        #[arg(long = "dangerously-skip-balance-check-and-may-lose-money")]
        dangerously_skip_balance_check_and_may_lose_money: bool,
    },
    /// List Solana wallets
    List,
    /// Dangerously show wallet seed mnemonic (12 BIP39 words)
    #[command(name = "dangerously-show-seed")]
    ShowSeed {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
    },
}

#[derive(Subcommand)]
enum EvmCommand {
    /// Wallet management
    Wallet {
        #[command(subcommand)]
        action: EvmWalletAction,
    },
    /// Send native token or ERC-20 token transfer
    #[command(name = "send")]
    Send {
        /// Recipient address (0x...)
        #[arg(long)]
        to: String,
        /// Amount in token base units (wei for ETH, smallest unit for ERC-20)
        #[arg(long)]
        amount: u64,
        /// Token: "native" for chain native, "usdc" or contract address for ERC-20
        #[arg(long)]
        token: String,
        #[command(flatten)]
        common: CommonSendArgs,
    },
    /// Show wallet receive address
    #[command(name = "receive")]
    Receive {
        /// On-chain memo to watch for (used with --wait)
        #[arg(long = "onchain-memo")]
        onchain_memo: Option<String>,
        /// Minimum confirmation depth before considering payment settled (requires --wait)
        #[arg(long = "min-confirmations")]
        min_confirmations: Option<u32>,
        #[command(flatten)]
        common: CommonReceiveArgs,
    },
    /// Check balance
    Balance {
        /// Wallet ID (omit to show all evm wallets)
        #[arg(long)]
        wallet: Option<String>,
    },
    /// Spend limit for evm network or a specific evm wallet
    Limit {
        /// Wallet ID (omit for network-level limit)
        #[arg(long)]
        wallet: Option<String>,
        #[command(subcommand)]
        action: TokenLimitAction,
    },
    /// Per-wallet configuration
    Config {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
        #[command(subcommand)]
        action: EvmWalletConfigAction,
    },
}

#[derive(Subcommand)]
enum EvmWalletAction {
    /// Create a new EVM chain wallet
    Create {
        /// EVM JSON-RPC endpoint (repeat to configure failover order)
        #[arg(long = "evm-rpc-endpoint", required = true)]
        evm_rpc_endpoint: Vec<String>,
        /// Chain ID (default: 8453 = Base)
        #[arg(long = "chain-id", default_value_t = 8453)]
        chain_id: u64,
        /// Optional label
        #[arg(long)]
        label: Option<String>,
    },
    /// Close an EVM chain wallet
    Close {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
        /// Dangerously skip balance checks when closing wallet
        #[arg(long = "dangerously-skip-balance-check-and-may-lose-money")]
        dangerously_skip_balance_check_and_may_lose_money: bool,
    },
    /// List EVM chain wallets
    List,
    /// Dangerously show wallet seed mnemonic (12 BIP39 words)
    #[command(name = "dangerously-show-seed")]
    ShowSeed {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
    },
}

#[derive(Subcommand)]
enum BtcCommand {
    /// Wallet management
    Wallet {
        #[command(subcommand)]
        action: BtcWalletAction,
    },
    /// Send BTC on-chain
    #[command(name = "send")]
    Send {
        /// Recipient Bitcoin address (bc1.../tb1...)
        #[arg(long)]
        to: String,
        /// Amount in satoshis
        #[arg(long = "amount-sats")]
        amount_sats: u64,
        #[command(flatten)]
        common: CommonSendArgs,
    },
    /// Show wallet receive address
    #[command(name = "receive")]
    Receive {
        /// Max history records scanned per poll when resolving tx id
        #[arg(long = "wait-sync-limit")]
        wait_sync_limit: Option<usize>,
        #[command(flatten)]
        common: CommonReceiveArgs,
    },
    /// Check balance
    Balance {
        /// Wallet ID (omit to show all btc wallets)
        #[arg(long)]
        wallet: Option<String>,
    },
    /// Spend limit for btc network or a specific btc wallet
    Limit {
        /// Wallet ID (omit for network-level limit)
        #[arg(long)]
        wallet: Option<String>,
        #[command(subcommand)]
        action: SimpleLimitAction,
    },
    /// Per-wallet configuration
    Config {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
        #[command(subcommand)]
        action: SimpleWalletConfigAction,
    },
}

#[derive(Subcommand)]
enum BtcWalletAction {
    /// Create a new Bitcoin wallet
    Create {
        /// Bitcoin sub-network: mainnet or signet (default: mainnet)
        #[arg(long = "btc-network", default_value = "mainnet")]
        btc_network: String,
        /// Address type: taproot or segwit (default: taproot)
        #[arg(long = "btc-address-type", default_value = "taproot")]
        btc_address_type: String,
        /// Chain-source backend: esplora (default), core-rpc, electrum
        #[arg(long = "btc-backend", value_enum)]
        btc_backend: Option<CliBtcBackend>,
        /// Custom Esplora API URL
        #[arg(long = "btc-esplora-url")]
        btc_esplora_url: Option<String>,
        /// Bitcoin Core RPC URL (core-rpc backend)
        #[arg(long = "btc-core-url")]
        btc_core_url: Option<String>,
        /// Bitcoin Core RPC auth "user:pass" (core-rpc backend)
        #[arg(long = "btc-core-auth-secret")]
        btc_core_auth_secret: Option<String>,
        /// Electrum server URL (electrum backend)
        #[arg(long = "btc-electrum-url")]
        btc_electrum_url: Option<String>,
        /// Existing BIP39 mnemonic secret to restore wallet
        #[arg(long = "mnemonic-secret")]
        mnemonic_secret: Option<String>,
        /// Optional label
        #[arg(long)]
        label: Option<String>,
    },
    /// Close a Bitcoin wallet
    Close {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
        /// Dangerously skip balance checks when closing wallet
        #[arg(long = "dangerously-skip-balance-check-and-may-lose-money")]
        dangerously_skip_balance_check_and_may_lose_money: bool,
    },
    /// List Bitcoin wallets
    List,
    /// Dangerously show wallet seed mnemonic (12 BIP39 words)
    #[command(name = "dangerously-show-seed")]
    ShowSeed {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
    },
}

#[derive(Subcommand)]
enum LnWalletAction {
    /// Create a new Lightning wallet
    Create {
        /// Backend: nwc, phoenixd, lnbits
        #[arg(long, value_enum)]
        backend: CliLnBackend,
        /// NWC connection URI secret (for nwc backend)
        #[arg(long = "nwc-uri-secret")]
        nwc_uri_secret: Option<String>,
        /// Endpoint URL (for phoenixd, lnbits)
        #[arg(long)]
        endpoint: Option<String>,
        /// Password secret (for phoenixd)
        #[arg(long = "password-secret")]
        password_secret: Option<String>,
        /// Admin API key secret (for lnbits)
        #[arg(long = "admin-key-secret")]
        admin_key_secret: Option<String>,
        /// Optional label
        #[arg(long)]
        label: Option<String>,
    },
    /// Close a Lightning wallet
    Close {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
        /// Dangerously skip balance checks when closing wallet
        #[arg(long = "dangerously-skip-balance-check-and-may-lose-money")]
        dangerously_skip_balance_check_and_may_lose_money: bool,
    },
    /// List Lightning wallets
    List,
    /// Dangerously show wallet seed (for LN this is backend credential, not mnemonic words)
    #[command(name = "dangerously-show-seed")]
    ShowSeed {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum CliLnBackend {
    Nwc,
    Phoenixd,
    Lnbits,
}

impl From<CliLnBackend> for LnWalletBackend {
    fn from(value: CliLnBackend) -> Self {
        match value {
            CliLnBackend::Nwc => LnWalletBackend::Nwc,
            CliLnBackend::Phoenixd => LnWalletBackend::Phoenixd,
            CliLnBackend::Lnbits => LnWalletBackend::Lnbits,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum CliBtcBackend {
    Esplora,
    #[value(name = "core-rpc")]
    CoreRpc,
    Electrum,
}

impl From<CliBtcBackend> for BtcBackend {
    fn from(value: CliBtcBackend) -> Self {
        match value {
            CliBtcBackend::Esplora => BtcBackend::Esplora,
            CliBtcBackend::CoreRpc => BtcBackend::CoreRpc,
            CliBtcBackend::Electrum => BtcBackend::Electrum,
        }
    }
}

#[derive(Subcommand)]
enum WalletTopAction {
    /// List all wallets (cross-network)
    List {
        /// Filter by network: cashu, ln, sol, evm
        #[arg(long, value_enum)]
        network: Option<CliNetwork>,
    },
}

#[derive(Subcommand)]
enum HistoryAction {
    /// List history records from local store
    List {
        /// Filter by wallet ID
        #[arg(long)]
        wallet: Option<String>,
        /// Filter by network: cashu, ln, sol, evm
        #[arg(long, value_enum)]
        network: Option<CliNetwork>,
        /// Filter by exact on-chain memo text
        #[arg(long = "onchain-memo")]
        onchain_memo: Option<String>,
        /// Max results
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Offset
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Only include records created at or after this epoch second
        #[arg(long = "since-epoch-s")]
        since_epoch_s: Option<u64>,
        /// Only include records created before this epoch second
        #[arg(long = "until-epoch-s")]
        until_epoch_s: Option<u64>,
    },
    /// Check history status
    Status {
        /// Transaction ID
        #[arg(long = "transaction-id")]
        transaction_id: String,
    },
    /// Incrementally sync on-chain/backend history into local store
    Update {
        /// Sync a specific wallet (defaults to all wallets in scope)
        #[arg(long)]
        wallet: Option<String>,
        /// Restrict sync to a single network
        #[arg(long, value_enum)]
        network: Option<CliNetwork>,
        /// Max records to scan per wallet during this incremental sync
        #[arg(long, default_value_t = 200)]
        limit: usize,
    },
}

#[derive(Subcommand)]
enum LimitAction {
    /// Remove a spend limit rule by ID
    Remove {
        /// Rule ID (e.g. r_1a2b3c4d)
        #[arg(long)]
        rule_id: String,
    },
    /// List current limit status
    List,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum CliNetwork {
    Ln,
    Sol,
    Evm,
    Cashu,
    Btc,
}

impl From<CliNetwork> for Network {
    fn from(c: CliNetwork) -> Self {
        match c {
            CliNetwork::Ln => Network::Ln,
            CliNetwork::Sol => Network::Sol,
            CliNetwork::Evm => Network::Evm,
            CliNetwork::Cashu => Network::Cashu,
            CliNetwork::Btc => Network::Btc,
        }
    }
}

// ═══════════════════════════════════════════
// Subcommand Parser (reused by interactive mode)
// ═══════════════════════════════════════════

#[derive(Parser)]
#[command(no_binary_name = true, name = "afpay")]
struct SubcommandParser {
    #[command(subcommand)]
    command: PayCommand,
}

/// Parse a subcommand from args (e.g. `["cashu", "send", "--amount-sats", "100"]`).
/// Used by interactive mode to reuse CLI command definitions.
#[cfg(any(feature = "interactive", test))]
pub fn parse_subcommand(args: &[&str], id: &str) -> Result<Input, String> {
    let parsed = SubcommandParser::try_parse_from(args).map_err(|e| e.to_string())?;
    command_to_input(parsed.command, id)
}

/// Render clap help for the given args (e.g. `&["--help"]`, `&["cashu", "--help"]`).
#[cfg(feature = "interactive")]
pub fn subcommand_help(args: &[&str]) -> String {
    match SubcommandParser::try_parse_from(args) {
        Ok(_) => String::new(),
        Err(e) => e.to_string(),
    }
}

/// Describes a single CLI argument extracted from clap definitions.
#[cfg(feature = "interactive")]
#[derive(Debug, Clone)]
pub struct ArgInfo {
    /// Long flag name without `--` prefix (e.g. `"amount-sats"`, `"to"`).
    pub long: String,
    /// Clap help string for this argument.
    pub help: String,
    /// Whether the argument is required.
    pub required: bool,
    /// Whether this is a boolean flag (no value, presence = true).
    pub is_flag: bool,
    /// Positional argument index (None for named `--` args).
    pub positional_index: Option<usize>,
}

/// Return the list of user-facing arguments for a subcommand path.
///
/// Example: `subcommand_args(&["cashu", "send"])` returns the args for `afpay cashu send`.
///
/// Hidden args and internal clap args (`help`, `version`) are excluded.
/// Flattened structs (e.g. `CommonSendArgs`) are inlined automatically by clap.
#[cfg(feature = "interactive")]
pub fn subcommand_args(path: &[&str]) -> Vec<ArgInfo> {
    use clap::CommandFactory;
    let root = SubcommandParser::command();
    let cmd = walk_subcommands(&root, path);
    let Some(cmd) = cmd else {
        return vec![];
    };
    cmd.get_arguments()
        .filter(|a| !a.is_hide_set())
        .filter(|a| {
            let id = a.get_id().as_str();
            id != "help" && id != "version"
        })
        .map(|a| {
            let long = a
                .get_long()
                .map(|s| s.to_string())
                .unwrap_or_else(|| a.get_id().to_string());
            let help = a.get_help().map(|s| s.to_string()).unwrap_or_default();
            let required = a.is_required_set();
            let is_flag = !a.get_action().takes_values();
            let positional_index = a.get_index();
            ArgInfo {
                long,
                help,
                required,
                is_flag,
                positional_index,
            }
        })
        .collect()
}

#[cfg(feature = "interactive")]
fn walk_subcommands<'a>(cmd: &'a clap::Command, path: &[&str]) -> Option<&'a clap::Command> {
    if path.is_empty() {
        return Some(cmd);
    }
    for sub in cmd.get_subcommands() {
        if sub.get_name() == path[0] {
            return walk_subcommands(sub, &path[1..]);
        }
    }
    None
}

// ═══════════════════════════════════════════
// Parsing
// ═══════════════════════════════════════════

pub fn parse_args() -> Result<Mode, CliError> {
    let raw: Vec<String> = std::env::args().collect();
    let startup_requested = raw.iter().any(|a| a == "--log");

    let cli = match AfpayCli::try_parse_from(&raw) {
        Ok(c) => c,
        Err(e) => {
            use clap::error::ErrorKind;
            if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                let _ = writeln!(std::io::stdout(), "{e}");
                std::process::exit(0);
            }
            return Err(e.to_string().into());
        }
    };
    let output = agent_first_data::cli_parse_output(&cli.output).map_err(CliError::from)?;
    let log = agent_first_data::cli_parse_log_filters(&cli.log);
    let startup_args = build_startup_args(&cli);

    match cli.mode {
        RuntimeMode::Pipe => {
            return Ok(Mode::Pipe(PipeInit {
                output,
                log,
                data_dir: cli.data_dir,
                startup_argv: raw.clone(),
                startup_args,
                startup_requested,
            }));
        }
        RuntimeMode::Interactive => {
            let (rpc_endpoint, rpc_secret) =
                resolve_rpc_args(cli.rpc_endpoint, cli.rpc_secret, cli.data_dir.as_deref());
            return Ok(Mode::Interactive(InteractiveInit {
                frontend: InteractiveFrontend::Interactive,
                output,
                log,
                data_dir: cli.data_dir,
                rpc_endpoint,
                rpc_secret,
            }));
        }
        RuntimeMode::Tui => {
            let (rpc_endpoint, rpc_secret) =
                resolve_rpc_args(cli.rpc_endpoint, cli.rpc_secret, cli.data_dir.as_deref());
            return Ok(Mode::Interactive(InteractiveInit {
                frontend: InteractiveFrontend::Tui,
                output,
                log,
                data_dir: cli.data_dir,
                rpc_endpoint,
                rpc_secret,
            }));
        }
        RuntimeMode::Rpc => {
            return Ok(Mode::Rpc(RpcInit {
                listen: cli.rpc_listen,
                rpc_secret: cli.rpc_secret,
                log,
                data_dir: cli.data_dir,
                startup_argv: raw.clone(),
                startup_args: startup_args.clone(),
                startup_requested,
            }));
        }
        #[cfg(feature = "rest")]
        RuntimeMode::Rest => {
            return Ok(Mode::Rest(RestInit {
                listen: cli.rest_listen,
                api_key: cli.rest_api_key,
                log,
                data_dir: cli.data_dir,
                startup_argv: raw.clone(),
                startup_args: startup_args.clone(),
                startup_requested,
            }));
        }
        RuntimeMode::Cli => {}
    }

    let Some(command) = cli.command else {
        return Err("no subcommand provided; run with --help for usage"
            .to_string()
            .into());
    };

    let request_id = format!("cli_{}", std::process::id());
    let input = command_to_input(command, &request_id)?;

    let (rpc_endpoint, rpc_secret) =
        resolve_rpc_args(cli.rpc_endpoint, cli.rpc_secret, cli.data_dir.as_deref());

    Ok(Mode::Cli(Box::new(CliRequest {
        input,
        output,
        log,
        data_dir: cli.data_dir,
        rpc_endpoint,
        rpc_secret,
        startup_argv: raw,
        startup_args,
        startup_requested,
        dry_run: cli.dry_run,
    })))
}

// ═══════════════════════════════════════════
// Validation helpers
// ═══════════════════════════════════════════

fn validate_sol_address(to: &str) -> Result<(), String> {
    if to.starts_with("0x") {
        return Err(format!(
            "invalid Solana address '{to}': looks like an EVM address (0x prefix). \
             Solana addresses are base58-encoded"
        ));
    }
    if !(32..=44).contains(&to.len()) {
        return Err(format!(
            "invalid Solana address '{to}': expected 32-44 base58 characters, got {}",
            to.len()
        ));
    }
    // Quick base58 character check (Bitcoin alphabet)
    if let Some(bad) = to
        .chars()
        .find(|c| !c.is_ascii_alphanumeric() || *c == '0' || *c == 'O' || *c == 'I' || *c == 'l')
    {
        return Err(format!(
            "invalid Solana address '{to}': illegal base58 character '{bad}'"
        ));
    }
    Ok(())
}

fn validate_evm_address(to: &str) -> Result<(), String> {
    if !to.starts_with("0x") {
        return Err(format!("invalid EVM address '{to}': must start with 0x"));
    }
    let hex_part = &to[2..];
    if hex_part.len() != 40 {
        return Err(format!(
            "invalid EVM address '{to}': expected 0x + 40 hex characters, got 0x + {}",
            hex_part.len()
        ));
    }
    if !hex_part.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "invalid EVM address '{to}': contains non-hex characters"
        ));
    }
    Ok(())
}

fn validate_bolt11(to: &str) -> Result<(), String> {
    let lower = to.to_lowercase();
    if !lower.starts_with("lnbc")
        && !lower.starts_with("lntb")
        && !lower.starts_with("lnbcrt")
        && !lower.starts_with("lno1")
    {
        return Err(format!(
            "invalid Lightning invoice/offer '{to}': must start with lnbc, lntb, lnbcrt, or lno1"
        ));
    }
    Ok(())
}

fn validate_token_not_contract(token: &str) -> Result<(), String> {
    if token.starts_with("0x")
        || (token.len() > 40 && token.chars().all(|c| c.is_ascii_alphanumeric()))
    {
        return Err(format!(
            "raw contract address not accepted for --token; register it first: \
             afpay <network> config --wallet <id> token-add --symbol <name> --address {token}"
        ));
    }
    Ok(())
}

fn command_to_input(cmd: PayCommand, id: &str) -> Result<Input, String> {
    match cmd {
        PayCommand::Global { action } => match action {
            GlobalCommand::Limit { action } => match action {
                GlobalLimitAction::Add { window, max_spend } => {
                    let window_s = parse_window(&window)?;
                    Ok(Input::LimitAdd {
                        id: id.to_string(),
                        limit: SpendLimit {
                            rule_id: None,
                            scope: SpendScope::GlobalUsdCents,
                            network: None,
                            wallet: None,
                            window_s,
                            max_spend,
                            token: None,
                        },
                    })
                }
            },
            GlobalCommand::Config { action } => match action {
                GlobalConfigAction::Show => Ok(Input::ConfigShow { id: id.to_string() }),
                GlobalConfigAction::Set { log } => Ok(Input::Config(ConfigPatch {
                    data_dir: None,
                    log,
                    exchange_rate: None,
                    afpay_rpc: None,
                    providers: None,
                })),
            },
        },
        PayCommand::Cashu { action } => cashu_command_to_input(action, id),
        PayCommand::Ln { action } => ln_command_to_input(action, id),
        PayCommand::Sol { action } => sol_command_to_input(action, id),
        PayCommand::Evm { action } => evm_command_to_input(action, id),
        PayCommand::Btc { action } => btc_command_to_input(action, id),
        PayCommand::Wallet { action } => match action {
            WalletTopAction::List { network } => Ok(Input::WalletList {
                id: id.to_string(),
                network: network.map(Into::into),
            }),
        },
        PayCommand::Balance {
            wallet,
            network,
            cashu_check,
        } => Ok(Input::Balance {
            id: id.to_string(),
            wallet: wallet.filter(|s| !s.is_empty()),
            network: network.map(Into::into),
            check: cashu_check,
        }),
        PayCommand::History { action } => match action {
            HistoryAction::List {
                wallet,
                network,
                onchain_memo,
                limit,
                offset,
                since_epoch_s,
                until_epoch_s,
            } => Ok(Input::HistoryList {
                id: id.to_string(),
                wallet,
                network: network.map(Into::into),
                onchain_memo,
                limit: Some(limit),
                offset: Some(offset),
                since_epoch_s,
                until_epoch_s,
            }),
            HistoryAction::Status { transaction_id } => Ok(Input::HistoryStatus {
                id: id.to_string(),
                transaction_id,
            }),
            HistoryAction::Update {
                wallet,
                network,
                limit,
            } => Ok(Input::HistoryUpdate {
                id: id.to_string(),
                wallet,
                network: network.map(Into::into),
                limit: Some(limit),
            }),
        },
        PayCommand::Limit { action } => match action {
            LimitAction::Remove { rule_id } => Ok(Input::LimitRemove {
                id: id.to_string(),
                rule_id,
            }),
            LimitAction::List => Ok(Input::LimitList { id: id.to_string() }),
        },
    }
}

fn simple_config_to_input(
    wallet: String,
    action: SimpleWalletConfigAction,
    id: &str,
) -> Result<Input, String> {
    match action {
        SimpleWalletConfigAction::Show => Ok(Input::WalletConfigShow {
            id: id.to_string(),
            wallet,
        }),
        SimpleWalletConfigAction::Set { label } => Ok(Input::WalletConfigSet {
            id: id.to_string(),
            wallet,
            label,
            rpc_endpoints: vec![],
            chain_id: None,
        }),
    }
}

fn sol_config_to_input(
    wallet: String,
    action: SolWalletConfigAction,
    id: &str,
) -> Result<Input, String> {
    match action {
        SolWalletConfigAction::Show => Ok(Input::WalletConfigShow {
            id: id.to_string(),
            wallet,
        }),
        SolWalletConfigAction::Set {
            label,
            rpc_endpoint,
        } => Ok(Input::WalletConfigSet {
            id: id.to_string(),
            wallet,
            label,
            rpc_endpoints: rpc_endpoint,
            chain_id: None,
        }),
        SolWalletConfigAction::TokenAdd {
            symbol,
            address,
            decimals,
        } => Ok(Input::WalletConfigTokenAdd {
            id: id.to_string(),
            wallet,
            symbol,
            address,
            decimals,
        }),
        SolWalletConfigAction::TokenRemove { symbol } => Ok(Input::WalletConfigTokenRemove {
            id: id.to_string(),
            wallet,
            symbol,
        }),
    }
}

fn evm_config_to_input(
    wallet: String,
    action: EvmWalletConfigAction,
    id: &str,
) -> Result<Input, String> {
    match action {
        EvmWalletConfigAction::Show => Ok(Input::WalletConfigShow {
            id: id.to_string(),
            wallet,
        }),
        EvmWalletConfigAction::Set {
            label,
            rpc_endpoint,
            chain_id,
        } => Ok(Input::WalletConfigSet {
            id: id.to_string(),
            wallet,
            label,
            rpc_endpoints: rpc_endpoint,
            chain_id,
        }),
        EvmWalletConfigAction::TokenAdd {
            symbol,
            address,
            decimals,
        } => Ok(Input::WalletConfigTokenAdd {
            id: id.to_string(),
            wallet,
            symbol,
            address,
            decimals,
        }),
        EvmWalletConfigAction::TokenRemove { symbol } => Ok(Input::WalletConfigTokenRemove {
            id: id.to_string(),
            wallet,
            symbol,
        }),
    }
}

fn simple_limit_to_input(
    network: Network,
    wallet: Option<String>,
    action: SimpleLimitAction,
    id: &str,
) -> Result<Input, String> {
    match action {
        SimpleLimitAction::Add { window, max_spend } => {
            let window_s = parse_window(&window)?;
            let (scope, wallet) = match wallet {
                Some(w) => (SpendScope::Wallet, Some(w)),
                None => (SpendScope::Network, None),
            };
            Ok(Input::LimitAdd {
                id: id.to_string(),
                limit: SpendLimit {
                    rule_id: None,
                    scope,
                    network: Some(network.to_string()),
                    wallet,
                    window_s,
                    max_spend,
                    token: None,
                },
            })
        }
    }
}

fn token_limit_to_input(
    network: Network,
    wallet: Option<String>,
    action: TokenLimitAction,
    id: &str,
) -> Result<Input, String> {
    match action {
        TokenLimitAction::Add {
            token,
            window,
            max_spend,
        } => {
            let window_s = parse_window(&window)?;
            let (scope, wallet) = match wallet {
                Some(w) => (SpendScope::Wallet, Some(w)),
                None => (SpendScope::Network, None),
            };
            Ok(Input::LimitAdd {
                id: id.to_string(),
                limit: SpendLimit {
                    rule_id: None,
                    scope,
                    network: Some(network.to_string()),
                    wallet,
                    window_s,
                    max_spend,
                    token,
                },
            })
        }
    }
}

fn cashu_command_to_input(cmd: CashuCommand, id: &str) -> Result<Input, String> {
    match cmd {
        CashuCommand::Send {
            common,
            amount_sats,
            mint_url,
            to,
        } => {
            if to.is_some() {
                return Err("cashu send generates a P2P cashu token — it does not send to an address. To pay a Lightning invoice, use: cashu send-to-ln --to <bolt11>".to_string());
            }
            Ok(Input::CashuSend {
                id: id.to_string(),
                wallet: common.wallet.filter(|s| !s.is_empty()),
                amount: Amount {
                    value: amount_sats,
                    token: "sats".to_string(),
                },
                onchain_memo: common.onchain_memo,
                local_memo: memo_vec_to_map(common.local_memo),
                mints: if mint_url.is_empty() {
                    None
                } else {
                    Some(mint_url)
                },
            })
        }
        CashuCommand::Receive { wallet, token } => Ok(Input::CashuReceive {
            id: id.to_string(),
            wallet: wallet.filter(|s| !s.is_empty()),
            token,
        }),
        CashuCommand::SendToLn { common, to } => Ok(Input::Send {
            id: id.to_string(),
            wallet: common.wallet.filter(|s| !s.is_empty()),
            network: Some(Network::Cashu),
            to,
            onchain_memo: common.onchain_memo,
            local_memo: memo_vec_to_map(common.local_memo),
            mints: None,
        }),
        CashuCommand::ReceiveFromLn {
            common,
            amount_sats,
            onchain_memo,
        } => {
            let resolved = amount_sats.map(|v| Amount {
                value: v,
                token: "sats".to_string(),
            });
            Ok(Input::Receive {
                id: id.to_string(),
                wallet: common.wallet.filter(|s| !s.is_empty()).unwrap_or_default(),
                network: Some(Network::Cashu),
                amount: resolved,
                onchain_memo,
                wait_until_paid: common.wait,
                wait_timeout_s: common.wait_timeout_s,
                wait_poll_interval_ms: common.wait_poll_interval_ms,
                wait_sync_limit: None,
                write_qr_svg_file: common.qr_svg_file,
                min_confirmations: None,
                reference: None,
            })
        }
        CashuCommand::ReceiveFromLnClaim {
            wallet,
            ln_quote_id,
        } => Ok(Input::ReceiveClaim {
            id: id.to_string(),
            wallet,
            quote_id: ln_quote_id,
        }),
        CashuCommand::Balance { wallet, check } => Ok(Input::Balance {
            id: id.to_string(),
            wallet: wallet.filter(|s| !s.is_empty()),
            network: Some(Network::Cashu),
            check,
        }),
        CashuCommand::Wallet { action } => match action {
            CashuWalletAction::Create {
                mint_url,
                label,
                mnemonic_secret,
            } => Ok(Input::WalletCreate {
                id: id.to_string(),
                network: Network::Cashu,
                label,
                mint_url: Some(mint_url),
                rpc_endpoints: vec![],
                chain_id: None,
                mnemonic_secret,
                btc_esplora_url: None,
                btc_network: None,
                btc_address_type: None,
                btc_backend: None,
                btc_core_url: None,
                btc_core_auth_secret: None,
                btc_electrum_url: None,
            }),
            CashuWalletAction::Close {
                wallet,
                dangerously_skip_balance_check_and_may_lose_money,
            } => Ok(Input::WalletClose {
                id: id.to_string(),
                wallet,
                dangerously_skip_balance_check_and_may_lose_money,
            }),
            CashuWalletAction::List => Ok(Input::WalletList {
                id: id.to_string(),
                network: Some(Network::Cashu),
            }),
            CashuWalletAction::ShowSeed { wallet } => Ok(Input::WalletShowSeed {
                id: id.to_string(),
                wallet,
            }),
            CashuWalletAction::Restore { wallet } => Ok(Input::Restore {
                id: id.to_string(),
                wallet,
            }),
        },
        CashuCommand::Limit { wallet, action } => {
            simple_limit_to_input(Network::Cashu, wallet, action, id)
        }
        CashuCommand::Config { wallet, action } => simple_config_to_input(wallet, action, id),
    }
}

fn ln_command_to_input(cmd: LnCommand, id: &str) -> Result<Input, String> {
    match cmd {
        LnCommand::Wallet { action } => match action {
            LnWalletAction::Create {
                backend,
                nwc_uri_secret,
                endpoint,
                password_secret,
                admin_key_secret,
                label,
            } => {
                let backend_code: LnWalletBackend = backend.into();
                let request = match backend_code {
                    LnWalletBackend::Nwc => LnWalletCreateRequest {
                        backend: backend_code,
                        label,
                        nwc_uri_secret: Some(
                            nwc_uri_secret.ok_or("--nwc-uri-secret is required for nwc backend")?,
                        ),
                        endpoint: None,
                        password_secret: None,
                        admin_key_secret: None,
                    },
                    LnWalletBackend::Phoenixd => LnWalletCreateRequest {
                        backend: backend_code,
                        label,
                        nwc_uri_secret: None,
                        endpoint: Some(
                            endpoint.ok_or("--endpoint is required for phoenixd backend")?,
                        ),
                        password_secret: Some(
                            password_secret
                                .ok_or("--password-secret is required for phoenixd backend")?,
                        ),
                        admin_key_secret: None,
                    },
                    LnWalletBackend::Lnbits => LnWalletCreateRequest {
                        backend: backend_code,
                        label,
                        nwc_uri_secret: None,
                        endpoint: Some(
                            endpoint.ok_or("--endpoint is required for lnbits backend")?,
                        ),
                        password_secret: None,
                        admin_key_secret: Some(
                            admin_key_secret
                                .ok_or("--admin-key-secret is required for lnbits backend")?,
                        ),
                    },
                };

                Ok(Input::LnWalletCreate {
                    id: id.to_string(),
                    request,
                })
            }
            LnWalletAction::Close {
                wallet,
                dangerously_skip_balance_check_and_may_lose_money,
            } => Ok(Input::WalletClose {
                id: id.to_string(),
                wallet,
                dangerously_skip_balance_check_and_may_lose_money,
            }),
            LnWalletAction::List => Ok(Input::WalletList {
                id: id.to_string(),
                network: Some(Network::Ln),
            }),
            LnWalletAction::ShowSeed { wallet } => Ok(Input::WalletShowSeed {
                id: id.to_string(),
                wallet,
            }),
        },
        LnCommand::Send {
            common,
            to,
            amount_sats,
        } => {
            if common.onchain_memo.is_some() {
                return Err(
                    "--onchain-memo is not supported for ln; use --local-memo for bookkeeping"
                        .into(),
                );
            }
            validate_bolt11(&to)?;
            let to = if is_bolt12_offer(&to) {
                let amt = amount_sats
                    .ok_or("--amount-sats is required when sending to a bolt12 offer")?;
                format!("{to}?amount={amt}")
            } else {
                if amount_sats.is_some() {
                    return Err(
                        "--amount-sats is not accepted for bolt11 invoices; the invoice encodes the amount".into(),
                    );
                }
                to
            };
            Ok(Input::Send {
                id: id.to_string(),
                wallet: common.wallet.filter(|s| !s.is_empty()),
                network: Some(Network::Ln),
                to,
                onchain_memo: None,
                local_memo: memo_vec_to_map(common.local_memo),
                mints: None,
            })
        }
        LnCommand::Receive {
            common,
            amount_sats,
        } => Ok(Input::Receive {
            id: id.to_string(),
            wallet: common.wallet.filter(|s| !s.is_empty()).unwrap_or_default(),
            network: Some(Network::Ln),
            amount: amount_sats.map(|v| Amount {
                value: v,
                token: "sats".to_string(),
            }),
            onchain_memo: None,
            wait_until_paid: common.wait,
            wait_timeout_s: common.wait_timeout_s,
            wait_poll_interval_ms: common.wait_poll_interval_ms,
            wait_sync_limit: None,
            write_qr_svg_file: common.qr_svg_file,
            min_confirmations: None,
            reference: None,
        }),
        LnCommand::Balance { wallet } => Ok(Input::Balance {
            id: id.to_string(),
            wallet: wallet.filter(|s| !s.is_empty()),
            network: Some(Network::Ln),
            check: false,
        }),
        LnCommand::Limit { wallet, action } => {
            simple_limit_to_input(Network::Ln, wallet, action, id)
        }
        LnCommand::Config { wallet, action } => simple_config_to_input(wallet, action, id),
    }
}

fn sol_command_to_input(cmd: SolCommand, id: &str) -> Result<Input, String> {
    match cmd {
        SolCommand::Wallet { action } => match action {
            SolWalletAction::Create {
                sol_rpc_endpoint,
                label,
            } => Ok(Input::WalletCreate {
                id: id.to_string(),
                network: Network::Sol,
                label,
                mint_url: None,
                rpc_endpoints: sol_rpc_endpoint,
                chain_id: None,
                mnemonic_secret: None,
                btc_esplora_url: None,
                btc_network: None,
                btc_address_type: None,
                btc_backend: None,
                btc_core_url: None,
                btc_core_auth_secret: None,
                btc_electrum_url: None,
            }),
            SolWalletAction::Close {
                wallet,
                dangerously_skip_balance_check_and_may_lose_money,
            } => Ok(Input::WalletClose {
                id: id.to_string(),
                wallet,
                dangerously_skip_balance_check_and_may_lose_money,
            }),
            SolWalletAction::List => Ok(Input::WalletList {
                id: id.to_string(),
                network: Some(Network::Sol),
            }),
            SolWalletAction::ShowSeed { wallet } => Ok(Input::WalletShowSeed {
                id: id.to_string(),
                wallet,
            }),
        },
        SolCommand::Send {
            common,
            to,
            amount,
            token,
            reference,
        } => {
            validate_sol_address(&to)?;
            validate_token_not_contract(&token)?;
            let mut target = format!("solana:{to}?amount={amount}&token={token}");
            if let Some(ref r) = reference {
                target.push_str(&format!("&reference={r}"));
            }
            Ok(Input::Send {
                id: id.to_string(),
                wallet: common.wallet.filter(|s| !s.is_empty()),
                network: Some(Network::Sol),
                to: target,
                onchain_memo: common.onchain_memo,
                local_memo: memo_vec_to_map(common.local_memo),
                mints: None,
            })
        }
        SolCommand::Receive {
            common,
            onchain_memo,
            min_confirmations,
            reference,
        } => Ok(Input::Receive {
            id: id.to_string(),
            wallet: common.wallet.filter(|s| !s.is_empty()).unwrap_or_default(),
            network: Some(Network::Sol),
            amount: None,
            onchain_memo: onchain_memo.filter(|s| !s.trim().is_empty()),
            wait_until_paid: common.wait,
            wait_timeout_s: common.wait_timeout_s,
            wait_poll_interval_ms: common.wait_poll_interval_ms,
            wait_sync_limit: None,
            write_qr_svg_file: common.qr_svg_file,
            min_confirmations,
            reference,
        }),
        SolCommand::Balance { wallet } => Ok(Input::Balance {
            id: id.to_string(),
            wallet: wallet.filter(|s| !s.is_empty()),
            network: Some(Network::Sol),
            check: false,
        }),
        SolCommand::Limit { wallet, action } => {
            token_limit_to_input(Network::Sol, wallet, action, id)
        }
        SolCommand::Config { wallet, action } => sol_config_to_input(wallet, action, id),
    }
}

fn evm_command_to_input(cmd: EvmCommand, id: &str) -> Result<Input, String> {
    match cmd {
        EvmCommand::Wallet { action } => match action {
            EvmWalletAction::Create {
                evm_rpc_endpoint,
                chain_id,
                label,
            } => Ok(Input::WalletCreate {
                id: id.to_string(),
                network: Network::Evm,
                label,
                mint_url: None,
                rpc_endpoints: evm_rpc_endpoint,
                chain_id: Some(chain_id),
                mnemonic_secret: None,
                btc_esplora_url: None,
                btc_network: None,
                btc_address_type: None,
                btc_backend: None,
                btc_core_url: None,
                btc_core_auth_secret: None,
                btc_electrum_url: None,
            }),
            EvmWalletAction::Close {
                wallet,
                dangerously_skip_balance_check_and_may_lose_money,
            } => Ok(Input::WalletClose {
                id: id.to_string(),
                wallet,
                dangerously_skip_balance_check_and_may_lose_money,
            }),
            EvmWalletAction::List => Ok(Input::WalletList {
                id: id.to_string(),
                network: Some(Network::Evm),
            }),
            EvmWalletAction::ShowSeed { wallet } => Ok(Input::WalletShowSeed {
                id: id.to_string(),
                wallet,
            }),
        },
        EvmCommand::Send {
            common,
            to,
            amount,
            token,
        } => {
            validate_evm_address(&to)?;
            validate_token_not_contract(&token)?;
            let target = format!("ethereum:{to}?amount={amount}&token={token}");
            Ok(Input::Send {
                id: id.to_string(),
                wallet: common.wallet.filter(|s| !s.is_empty()),
                network: Some(Network::Evm),
                to: target,
                onchain_memo: common.onchain_memo,
                local_memo: memo_vec_to_map(common.local_memo),
                mints: None,
            })
        }
        EvmCommand::Receive {
            common,
            onchain_memo,
            min_confirmations,
        } => {
            if common.wait {
                return Err(
                    "evm receive --wait requires --amount; use unified receive command".into(),
                );
            }
            Ok(Input::Receive {
                id: id.to_string(),
                wallet: common.wallet.filter(|s| !s.is_empty()).unwrap_or_default(),
                network: Some(Network::Evm),
                amount: None,
                onchain_memo,
                wait_until_paid: common.wait,
                wait_timeout_s: common.wait_timeout_s,
                wait_poll_interval_ms: common.wait_poll_interval_ms,
                wait_sync_limit: None,
                write_qr_svg_file: false,
                min_confirmations,
                reference: None,
            })
        }
        EvmCommand::Balance { wallet } => Ok(Input::Balance {
            id: id.to_string(),
            wallet: wallet.filter(|s| !s.is_empty()),
            network: Some(Network::Evm),
            check: false,
        }),
        EvmCommand::Limit { wallet, action } => {
            token_limit_to_input(Network::Evm, wallet, action, id)
        }
        EvmCommand::Config { wallet, action } => evm_config_to_input(wallet, action, id),
    }
}

fn btc_command_to_input(cmd: BtcCommand, id: &str) -> Result<Input, String> {
    match cmd {
        BtcCommand::Wallet { action } => match action {
            BtcWalletAction::Create {
                label,
                btc_network,
                btc_address_type,
                btc_esplora_url,
                btc_backend,
                btc_core_url,
                btc_core_auth_secret,
                btc_electrum_url,
                mnemonic_secret,
            } => Ok(Input::WalletCreate {
                id: id.to_string(),
                network: Network::Btc,
                label,
                mint_url: None,
                rpc_endpoints: vec![],
                chain_id: None,
                mnemonic_secret,
                btc_esplora_url,
                btc_network: Some(btc_network),
                btc_address_type: Some(btc_address_type),
                btc_backend: btc_backend.map(Into::into),
                btc_core_url,
                btc_core_auth_secret,
                btc_electrum_url,
            }),
            BtcWalletAction::Close {
                wallet,
                dangerously_skip_balance_check_and_may_lose_money,
            } => Ok(Input::WalletClose {
                id: id.to_string(),
                wallet,
                dangerously_skip_balance_check_and_may_lose_money,
            }),
            BtcWalletAction::List => Ok(Input::WalletList {
                id: id.to_string(),
                network: Some(Network::Btc),
            }),
            BtcWalletAction::ShowSeed { wallet } => Ok(Input::WalletShowSeed {
                id: id.to_string(),
                wallet,
            }),
        },
        BtcCommand::Send {
            common,
            to,
            amount_sats,
        } => {
            let target = format!("bitcoin:{to}?amount={amount_sats}");
            Ok(Input::Send {
                id: id.to_string(),
                wallet: common.wallet.filter(|s| !s.is_empty()),
                network: Some(Network::Btc),
                to: target,
                onchain_memo: common.onchain_memo,
                local_memo: memo_vec_to_map(common.local_memo),
                mints: None,
            })
        }
        BtcCommand::Receive {
            common,
            wait_sync_limit,
        } => Ok(Input::Receive {
            id: id.to_string(),
            wallet: common.wallet.filter(|s| !s.is_empty()).unwrap_or_default(),
            network: Some(Network::Btc),
            amount: None,
            onchain_memo: None,
            wait_until_paid: common.wait,
            wait_timeout_s: common.wait_timeout_s,
            wait_poll_interval_ms: common.wait_poll_interval_ms,
            wait_sync_limit,
            write_qr_svg_file: false,
            min_confirmations: None,
            reference: None,
        }),
        BtcCommand::Balance { wallet } => Ok(Input::Balance {
            id: id.to_string(),
            wallet: wallet.filter(|s| !s.is_empty()),
            network: Some(Network::Btc),
            check: false,
        }),
        BtcCommand::Limit { wallet, action } => {
            simple_limit_to_input(Network::Btc, wallet, action, id)
        }
        BtcCommand::Config { wallet, action } => simple_config_to_input(wallet, action, id),
    }
}

fn parse_window(s: &str) -> Result<u64, String> {
    let (num_str, multiplier) = if let Some(n) = s.strip_suffix('d') {
        (n, 86400u64)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3600u64)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60u64)
    } else {
        return Err(format!(
            "invalid window '{s}': expected suffix m (minutes), h (hours), or d (days)"
        ));
    };
    let num: u64 = num_str
        .parse()
        .map_err(|_| format!("invalid window number '{num_str}'"))?;
    if num == 0 {
        return Err("window cannot be zero".to_string());
    }
    Ok(num.saturating_mul(multiplier))
}

/// Resolve rpc_endpoint/rpc_secret: CLI args take priority, then config.toml.
fn resolve_rpc_args(
    cli_endpoint: Option<String>,
    cli_secret: Option<String>,
    data_dir: Option<&str>,
) -> (Option<String>, Option<String>) {
    if cli_endpoint.is_some() {
        return (cli_endpoint, cli_secret);
    }
    let dir = data_dir
        .map(|s| s.to_string())
        .unwrap_or_else(|| RuntimeConfig::default().data_dir);
    let config = RuntimeConfig::load_from_dir(&dir).unwrap_or_default();
    if config.rpc_endpoint.is_some() {
        return (config.rpc_endpoint, cli_secret.or(config.rpc_secret));
    }
    (None, cli_secret)
}

fn build_startup_args(cli: &AfpayCli) -> serde_json::Value {
    serde_json::json!({
        "mode": format!("{:?}", cli.mode),
        "output": cli.output,
        "data_dir": cli.data_dir,
        "rpc_endpoint": cli.rpc_endpoint,
        "rpc_listen": cli.rpc_listen,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parse_window_minutes() {
        assert_eq!(parse_window("30m").unwrap(), 1800);
    }

    #[test]
    fn parse_tui_runtime_mode() {
        let cli = AfpayCli::try_parse_from(["afpay", "--mode", "tui", "wallet", "list"])
            .expect("tui mode should parse");
        assert_eq!(cli.mode, RuntimeMode::Tui);
    }

    #[test]
    fn parse_window_hours() {
        assert_eq!(parse_window("1h").unwrap(), 3600);
        assert_eq!(parse_window("24h").unwrap(), 86400);
    }

    #[test]
    fn parse_window_days() {
        assert_eq!(parse_window("7d").unwrap(), 604800);
    }

    #[test]
    fn parse_window_rejects_invalid() {
        assert!(parse_window("0h").is_err());
        assert!(parse_window("abc").is_err());
        assert!(parse_window("10s").is_err());
    }

    #[test]
    fn parse_limit_add_network_scope() {
        let input = parse_subcommand(
            &[
                "cashu",
                "limit",
                "add",
                "--window",
                "1h",
                "--max-spend",
                "10000",
            ],
            "t_limit_1",
        )
        .expect("cashu limit add should parse");

        match input {
            Input::LimitAdd { limit, .. } => {
                assert_eq!(limit.scope, SpendScope::Network);
                assert_eq!(limit.network.as_deref(), Some("cashu"));
                assert_eq!(limit.window_s, 3600);
                assert_eq!(limit.max_spend, 10000);
                assert!(limit.token.is_none());
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_limit_add_global_usd_cents_scope() {
        let input = parse_subcommand(
            &[
                "global",
                "limit",
                "add",
                "--window",
                "24h",
                "--max-spend",
                "50000",
            ],
            "t_limit_2",
        )
        .expect("global limit add should parse");

        match input {
            Input::LimitAdd { limit, .. } => {
                assert_eq!(limit.scope, SpendScope::GlobalUsdCents);
                assert_eq!(limit.window_s, 86400);
                assert_eq!(limit.max_spend, 50000);
                assert!(limit.token.is_none());
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_limit_add_network_scope_with_token() {
        let input = parse_subcommand(
            &[
                "evm",
                "limit",
                "add",
                "--token",
                "usdc",
                "--window",
                "24h",
                "--max-spend",
                "100000000",
            ],
            "t_limit_2b",
        )
        .expect("evm limit add with token should parse");

        match input {
            Input::LimitAdd { limit, .. } => {
                assert_eq!(limit.scope, SpendScope::Network);
                assert_eq!(limit.network.as_deref(), Some("evm"));
                assert_eq!(limit.token.as_deref(), Some("usdc"));
                assert_eq!(limit.max_spend, 100000000);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_limit_add_wallet_scope() {
        let input = parse_subcommand(
            &[
                "cashu",
                "limit",
                "--wallet",
                "w_abc",
                "add",
                "--window",
                "30m",
                "--max-spend",
                "5000",
            ],
            "t_limit_4",
        )
        .expect("cashu limit --wallet add should parse");

        match input {
            Input::LimitAdd { limit, .. } => {
                assert_eq!(limit.scope, SpendScope::Wallet);
                assert_eq!(limit.network.as_deref(), Some("cashu"));
                assert_eq!(limit.wallet.as_deref(), Some("w_abc"));
                assert_eq!(limit.window_s, 1800);
                assert_eq!(limit.max_spend, 5000);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_limit_remove() {
        let input = parse_subcommand(&["limit", "remove", "--rule-id", "r_1a2b3c4d"], "t_limit_3")
            .expect("limit remove should parse");

        match input {
            Input::LimitRemove { rule_id, .. } => {
                assert_eq!(rule_id, "r_1a2b3c4d");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_limit_list() {
        let input =
            parse_subcommand(&["limit", "list"], "t_limit_4").expect("limit list should parse");
        assert!(matches!(input, Input::LimitList { .. }));
    }

    #[test]
    fn parse_ln_receive_wallet_optional() {
        let input = parse_subcommand(&["ln", "receive", "--amount-sats", "100"], "t_1")
            .expect("ln receive should parse without --wallet");

        match input {
            Input::Receive { wallet, amount, .. } => {
                assert_eq!(wallet, "");
                assert_eq!(amount.expect("amount").value, 100);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_cashu_receive_from_ln_wallet_optional() {
        let input = parse_subcommand(&["cashu", "receive-from-ln", "--amount-sats", "100"], "t_2")
            .expect("cashu receive-from-ln should parse without --wallet");

        match input {
            Input::Receive {
                wallet,
                network,
                amount,
                ..
            } => {
                assert_eq!(wallet, "");
                assert_eq!(network, Some(Network::Cashu));
                assert_eq!(amount.expect("amount").value, 100);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_cashu_send_mint_url() {
        let input = parse_subcommand(
            &[
                "cashu",
                "send",
                "--amount-sats",
                "100",
                "--cashu-mint",
                "https://mint-a.example",
                "--cashu-mint",
                "https://mint-b.example",
            ],
            "t_cashu_1",
        )
        .expect("cashu send --mint-url should parse");

        match input {
            Input::CashuSend { mints, amount, .. } => {
                assert_eq!(amount.value, 100);
                assert_eq!(
                    mints,
                    Some(vec![
                        "https://mint-a.example".to_string(),
                        "https://mint-b.example".to_string()
                    ])
                );
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_cashu_send_legacy_mint_flag_rejected() {
        let err = parse_subcommand(
            &[
                "cashu",
                "send",
                "--amount-sats",
                "100",
                "--mint",
                "https://mint-a.example",
            ],
            "t_cashu_2",
        )
        .expect_err("legacy --mint should be rejected");

        assert!(err.contains("--mint"));
    }

    #[test]
    fn parse_cashu_send_to_flag_hints_send_to_ln() {
        let err = parse_subcommand(
            &["cashu", "send", "--amount-sats", "100", "--to", "lnbc1..."],
            "t_hint",
        )
        .expect_err("--to on cashu send should be rejected with hint");
        assert!(
            err.contains("send-to-ln"),
            "should suggest send-to-ln: {err}"
        );
    }

    #[test]
    fn parse_cashu_wallet_create_with_mnemonic_secret() {
        let input = parse_subcommand(
            &[
                "cashu",
                "wallet",
                "create",
                "--cashu-mint",
                "https://mint.example",
                "--mnemonic-secret",
                "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
            ],
            "t_cashu_create_1",
        )
        .expect("cashu wallet create --mnemonic-secret should parse");

        match input {
            Input::WalletCreate {
                network,
                mint_url,
                mnemonic_secret,
                ..
            } => {
                assert_eq!(network, Network::Cashu);
                assert_eq!(mint_url.as_deref(), Some("https://mint.example"));
                assert_eq!(
                    mnemonic_secret.as_deref(),
                    Some("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about")
                );
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_sol_wallet_create_sol_rpc_endpoint() {
        let input = parse_subcommand(
            &[
                "sol",
                "wallet",
                "create",
                "--sol-rpc-endpoint",
                "https://api.mainnet-beta.solana.com",
            ],
            "t_sol_create_1",
        )
        .expect("sol wallet create --sol-rpc-endpoint should parse");

        match input {
            Input::WalletCreate {
                network,
                rpc_endpoints,
                mint_url,
                ..
            } => {
                assert_eq!(network, Network::Sol);
                assert!(mint_url.is_none());
                assert_eq!(rpc_endpoints, vec!["https://api.mainnet-beta.solana.com"]);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_sol_wallet_create_multiple_sol_rpc_endpoints() {
        let input = parse_subcommand(
            &[
                "sol",
                "wallet",
                "create",
                "--sol-rpc-endpoint",
                "https://rpc-a.example",
                "--sol-rpc-endpoint",
                "https://rpc-b.example",
            ],
            "t_sol_create_3",
        )
        .expect("sol wallet create with repeated --sol-rpc-endpoint should parse");

        match input {
            Input::WalletCreate {
                network,
                rpc_endpoints,
                mint_url,
                ..
            } => {
                assert_eq!(network, Network::Sol);
                assert!(mint_url.is_none());
                assert_eq!(
                    rpc_endpoints,
                    vec!["https://rpc-a.example", "https://rpc-b.example"]
                );
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_sol_wallet_create_legacy_rpc_endpoint_rejected() {
        let err = parse_subcommand(
            &[
                "sol",
                "wallet",
                "create",
                "--rpc-endpoint",
                "https://api.mainnet-beta.solana.com",
            ],
            "t_sol_create_2",
        )
        .expect_err("legacy --rpc-endpoint should be rejected for sol wallet create");

        assert!(err.contains("--rpc-endpoint"));
    }

    #[test]
    fn parse_sol_receive_qr_svg_file() {
        let input = parse_subcommand(
            &["sol", "receive", "--wallet", "w_12345678", "--qr-svg-file"],
            "t_sol_1",
        )
        .expect("sol receive --qr-svg-file should parse");

        match input {
            Input::Receive {
                wallet,
                network,
                write_qr_svg_file,
                ..
            } => {
                assert_eq!(wallet, "w_12345678");
                assert_eq!(network, Some(Network::Sol));
                assert!(write_qr_svg_file);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_sol_receive_wait_with_onchain_memo() {
        let input = parse_subcommand(
            &[
                "sol",
                "receive",
                "--wallet",
                "w_12345678",
                "--onchain-memo",
                "order:ord_123",
                "--wait",
                "--wait-timeout-s",
                "15",
            ],
            "t_sol_1b",
        )
        .expect("sol receive --onchain-memo --wait should parse");

        match input {
            Input::Receive {
                wallet,
                network,
                onchain_memo,
                wait_until_paid,
                wait_timeout_s,
                ..
            } => {
                assert_eq!(wallet, "w_12345678");
                assert_eq!(network, Some(Network::Sol));
                assert_eq!(onchain_memo.as_deref(), Some("order:ord_123"));
                assert!(wait_until_paid);
                assert_eq!(wait_timeout_s, Some(15));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_history_list_with_onchain_memo_filter() {
        let input = parse_subcommand(
            &[
                "history",
                "list",
                "--wallet",
                "w_12345678",
                "--onchain-memo",
                "order:ord_123",
                "--limit",
                "50",
            ],
            "t_hist_1",
        )
        .expect("history list --onchain-memo should parse");

        match input {
            Input::HistoryList {
                wallet,
                onchain_memo,
                limit,
                ..
            } => {
                assert_eq!(wallet.as_deref(), Some("w_12345678"));
                assert_eq!(onchain_memo.as_deref(), Some("order:ord_123"));
                assert_eq!(limit, Some(50));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_history_update_with_scope() {
        let input = parse_subcommand(
            &[
                "history",
                "update",
                "--wallet",
                "w_12345678",
                "--network",
                "btc",
                "--limit",
                "120",
            ],
            "t_hist_up_1",
        )
        .expect("history update with scope should parse");

        match input {
            Input::HistoryUpdate {
                wallet,
                network,
                limit,
                ..
            } => {
                assert_eq!(wallet.as_deref(), Some("w_12345678"));
                assert_eq!(network, Some(Network::Btc));
                assert_eq!(limit, Some(120));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_history_update_defaults_limit() {
        let input = parse_subcommand(&["history", "update"], "t_hist_up_2")
            .expect("history update should parse");
        match input {
            Input::HistoryUpdate {
                wallet,
                network,
                limit,
                ..
            } => {
                assert_eq!(wallet, None);
                assert_eq!(network, None);
                assert_eq!(limit, Some(200));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_sol_wallet_dangerously_show_seed() {
        let input = parse_subcommand(
            &[
                "sol",
                "wallet",
                "dangerously-show-seed",
                "--wallet",
                "w_sol",
            ],
            "t_sol_2",
        )
        .expect("sol wallet dangerously-show-seed should parse");

        match input {
            Input::WalletShowSeed { wallet, .. } => assert_eq!(wallet, "w_sol"),
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_ln_wallet_dangerously_show_seed() {
        let input = parse_subcommand(
            &["ln", "wallet", "dangerously-show-seed", "--wallet", "w_ln"],
            "t_ln_1",
        )
        .expect("ln wallet dangerously-show-seed should parse");

        match input {
            Input::WalletShowSeed { wallet, .. } => assert_eq!(wallet, "w_ln"),
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_sol_wallet_legacy_show_seed_rejected() {
        let err = parse_subcommand(
            &["sol", "wallet", "show-seed", "--wallet", "w_sol"],
            "t_sol_3",
        )
        .expect_err("legacy sol wallet show-seed should be rejected");
        assert!(err.contains("show-seed"));
    }

    #[test]
    fn parse_ln_send_sets_network_hint() {
        let input = parse_subcommand(
            &[
                "ln",
                "send",
                "--to",
                "lnbc1example",
                "--local-memo",
                "hello",
            ],
            "t_3",
        )
        .expect("ln send should parse");

        match input {
            Input::Send { network, .. } => {
                assert_eq!(network, Some(Network::Ln));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_cashu_send_amount() {
        let input = parse_subcommand(&["cashu", "send", "--amount-sats", "500"], "t_unified_1")
            .expect("cashu send --amount-sats should parse");

        match input {
            Input::CashuSend { amount, .. } => {
                assert_eq!(amount.value, 500);
                assert_eq!(amount.token, "sats");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_sol_send_token_required() {
        let input = parse_subcommand(
            &[
                "sol",
                "send",
                "--to",
                "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU",
                "--amount",
                "1000000",
                "--token",
                "native",
            ],
            "t_unified_2",
        )
        .expect("sol send --amount --token should parse");

        match input {
            Input::Send { to, .. } => {
                assert!(to.contains("amount=1000000"));
                assert!(to.contains("token=native"));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_evm_send_token_required() {
        let input = parse_subcommand(
            &[
                "evm",
                "send",
                "--to",
                "0x1234567890abcdef1234567890abcdef12345678",
                "--amount",
                "1000000000",
                "--token",
                "native",
            ],
            "t_unified_3",
        )
        .expect("evm send --amount --token should parse");

        match input {
            Input::Send { to, .. } => {
                assert!(to.contains("amount=1000000000"));
                assert!(to.contains("token=native"));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_ln_receive_amount() {
        let input = parse_subcommand(&["ln", "receive", "--amount-sats", "1000"], "t_unified_4")
            .expect("ln receive --amount-sats should parse");

        match input {
            Input::Receive { amount, .. } => {
                let a = amount.expect("amount should be set");
                assert_eq!(a.value, 1000);
                assert_eq!(a.token, "sats");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }
    #[test]
    fn parse_cashu_receive_from_ln_claim_hidden_still_works() {
        let input = parse_subcommand(
            &[
                "cashu",
                "receive-from-ln-claim",
                "--wallet",
                "w_abc",
                "--ln-quote-id",
                "ph_456",
            ],
            "t_claim_5",
        )
        .expect("hidden cashu receive-from-ln-claim should still parse");
        match input {
            Input::ReceiveClaim {
                wallet, quote_id, ..
            } => {
                assert_eq!(wallet, "w_abc");
                assert_eq!(quote_id, "ph_456");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_cashu_wallet_restore() {
        let input = parse_subcommand(
            &["cashu", "wallet", "restore", "--wallet", "w_cashu1"],
            "t_wr_1",
        )
        .expect("cashu wallet restore should parse");
        match input {
            Input::Restore { wallet, .. } => {
                assert_eq!(wallet, "w_cashu1");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    // ═══════════════════════════════════════════
    // Top-level balance with --cashu-check
    // ═══════════════════════════════════════════

    #[test]
    fn parse_balance_with_cashu_check() {
        let input = parse_subcommand(&["balance", "--cashu-check"], "t_bal_1")
            .expect("balance --cashu-check should parse");
        match input {
            Input::Balance { check, wallet, .. } => {
                assert!(check);
                assert!(wallet.is_none());
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_balance_with_wallet() {
        let input = parse_subcommand(&["balance", "--wallet", "w_abc"], "t_bal_2")
            .expect("balance --wallet should parse");
        match input {
            Input::Balance { wallet, check, .. } => {
                assert_eq!(wallet.as_deref(), Some("w_abc"));
                assert!(!check);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    // ═══════════════════════════════════════════
    // BOLT12 offer tests
    // ═══════════════════════════════════════════

    #[test]
    fn parse_ln_receive_without_amount_for_bolt12() {
        let input = parse_subcommand(&["ln", "receive"], "t_bolt12_1")
            .expect("ln receive without --amount should parse (bolt12 offer)");
        match input {
            Input::Receive {
                network, amount, ..
            } => {
                assert_eq!(network, Some(Network::Ln));
                assert!(amount.is_none(), "amount should be None for bolt12 offer");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_ln_send_bolt12_requires_amount() {
        let err = parse_subcommand(&["ln", "send", "--to", "lno1abc123"], "t_bolt12_3")
            .expect_err("ln send to bolt12 without --amount-sats should error");
        assert!(
            err.contains("amount-sats"),
            "error should mention amount-sats: {err}"
        );
    }

    #[test]
    fn parse_ln_send_bolt12_with_amount() {
        let input = parse_subcommand(
            &["ln", "send", "--to", "lno1abc123", "--amount-sats", "500"],
            "t_bolt12_4",
        )
        .expect("ln send to bolt12 with --amount-sats should parse");
        match input {
            Input::Send { to, network, .. } => {
                assert_eq!(network, Some(Network::Ln));
                assert!(to.contains("lno1abc123"), "to should contain offer");
                assert!(to.contains("?amount=500"), "to should encode amount");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_ln_send_bolt12_case_insensitive() {
        let input = parse_subcommand(
            &[
                "ln",
                "send",
                "--to",
                "LNO1UPPERCASE",
                "--amount-sats",
                "100",
            ],
            "t_bolt12_5",
        )
        .expect("uppercase LNO1 should be accepted");
        match input {
            Input::Send { to, .. } => {
                assert!(
                    to.contains("?amount=100"),
                    "uppercase offer should get amount appended: {to}"
                );
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_ln_send_bolt11_rejects_amount_sats() {
        let err = parse_subcommand(
            &["ln", "send", "--to", "lnbc1abc", "--amount-sats", "100"],
            "t_bolt12_8",
        )
        .expect_err("ln send to bolt11 with --amount-sats should error");
        assert!(
            err.contains("not accepted"),
            "error should reject amount for bolt11: {err}"
        );
    }
}
