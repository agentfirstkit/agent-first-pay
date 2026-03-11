#[cfg(feature = "rest")]
use crate::rest::RestInit;
use crate::rpc::RpcInit;
use crate::types::*;
use agent_first_data::OutputFormat;
use clap::{Parser, Subcommand, ValueEnum};
use std::collections::BTreeMap;

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
    #[cfg(feature = "mcp")]
    Mcp(PipeInit),
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
#[derive(Clone)]
pub struct InteractiveInit {
    pub output: OutputFormat,
    pub log: Vec<String>,
    pub data_dir: Option<String>,
    pub rpc_endpoint: Option<String>,
    pub rpc_secret: Option<String>,
}

// RpcInit is defined in rpc.rs and re-used here

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
// Clap Definitions
// ═══════════════════════════════════════════

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum RuntimeMode {
    Cli,
    Pipe,
    #[cfg(feature = "mcp")]
    Mcp,
    Interactive,
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
struct AfpayCli {
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
    /// Cashu operations
    #[command(hide = true)]
    Cashu {
        #[command(subcommand)]
        action: CashuCommand,
    },
    /// Lightning Network operations (NWC, phoenixd, LNbits)
    #[command(hide = true)]
    Ln {
        #[command(subcommand)]
        action: LnCommand,
    },
    /// Solana operations
    #[command(hide = true)]
    Sol {
        #[command(subcommand)]
        action: SolCommand,
    },
    /// EVM chain operations (Base, Arbitrum)
    #[command(hide = true)]
    Evm {
        #[command(subcommand)]
        action: EvmCommand,
    },
    /// Bitcoin on-chain operations
    #[command(hide = true)]
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
    /// Spend limit management
    Limit {
        #[command(subcommand)]
        action: LimitAction,
    },
    /// Send payment (unified, network-aware)
    Send {
        /// Target network (inferred from --wallet if omitted)
        #[arg(long, value_enum)]
        network: Option<CliNetwork>,
        /// Recipient address (cashu: optional, ln/sol/evm: required)
        #[arg(long)]
        to: Option<String>,
        /// Amount in base units (cashu/sol/evm: required, ln: rejected)
        #[arg(long)]
        amount: Option<u64>,
        /// Token: "native" for chain native, or symbol/address (sol/evm: required, cashu/ln: rejected)
        #[arg(long)]
        token: Option<String>,
        /// Source wallet ID (auto-selected if omitted)
        #[arg(long)]
        wallet: Option<String>,
        /// On-chain memo (sent with the transaction)
        #[arg(long = "onchain-memo")]
        onchain_memo: Option<String>,
        /// Local bookkeeping annotation (repeatable: --local-memo purpose=donation --local-memo note=coffee)
        #[arg(long = "local-memo", value_parser = parse_memo_kv)]
        local_memo: Vec<(String, String)>,
        /// Restrict to wallets on these mint URLs (cashu only)
        #[arg(long = "cashu-mint")]
        cashu_mint: Vec<String>,
    },
    /// Receive payment (unified, network-aware)
    Receive {
        /// Target network (inferred from --wallet if omitted)
        #[arg(long, value_enum)]
        network: Option<CliNetwork>,
        /// Cashu token string to receive (cashu only)
        #[arg(long = "cashu-token")]
        cashu_token: Option<String>,
        /// Amount in base units
        #[arg(long)]
        amount: Option<u64>,
        /// Token: "native" for chain native, or symbol/address (sol/evm: optional filter, cashu/ln: rejected)
        #[arg(long)]
        token: Option<String>,
        /// Wallet ID (auto-selected if omitted)
        #[arg(long)]
        wallet: Option<String>,
        /// On-chain memo to watch for (used with --wait)
        #[arg(long = "onchain-memo")]
        onchain_memo: Option<String>,
        /// Wait for a matching receive transaction
        #[arg(long)]
        wait: bool,
        /// Timeout in seconds for --wait
        #[arg(long = "wait-timeout-s")]
        wait_timeout_s: Option<u64>,
        /// Poll interval in milliseconds for --wait
        #[arg(long = "wait-poll-interval-ms")]
        wait_poll_interval_ms: Option<u64>,
        /// Write receive QR payload to an SVG file
        #[arg(long = "qr-svg-file")]
        qr_svg_file: bool,
        /// Resume claiming from a previous deposit quote (cashu/ln only)
        #[arg(long = "ln-quote-id")]
        ln_quote_id: Option<String>,
        /// Minimum confirmation depth before considering payment settled (sol/evm only, requires --wait)
        #[arg(long = "min-confirmations")]
        min_confirmations: Option<u32>,
    },
}

#[derive(Subcommand)]
enum CashuCommand {
    /// Send P2P cashu token (outputs token string; for Lightning, use send-to-ln)
    #[command(name = "send", hide = true)]
    Send {
        /// Source wallet ID (auto-selected if omitted)
        #[arg(long)]
        wallet: Option<String>,
        /// Amount in sats (base units)
        #[arg(long)]
        amount: u64,
        /// On-chain memo (sent with the transaction)
        #[arg(long = "onchain-memo")]
        onchain_memo: Option<String>,
        /// Local bookkeeping annotation (repeatable: --local-memo purpose=donation --local-memo note=coffee)
        #[arg(long = "local-memo", value_parser = parse_memo_kv)]
        local_memo: Vec<(String, String)>,
        /// Restrict to wallets on these mint URLs (tried in order)
        #[arg(long = "cashu-mint")]
        mint_url: Vec<String>,
        /// Hidden: catches --to and redirects to send-to-ln
        #[arg(long, hide = true)]
        to: Option<String>,
    },
    /// Receive cashu token
    #[command(name = "receive", hide = true)]
    Receive {
        /// Wallet ID (auto-matched from token if omitted)
        #[arg(long)]
        wallet: Option<String>,
        /// Cashu token string
        token: String,
    },
    /// Send cashu to a Lightning invoice
    #[command(name = "send-to-ln", hide = true)]
    SendToLn {
        /// Source wallet ID (auto-selected if omitted)
        #[arg(long)]
        wallet: Option<String>,
        /// Lightning invoice (bolt11)
        #[arg(long)]
        to: String,
        /// On-chain memo
        #[arg(long = "onchain-memo")]
        onchain_memo: Option<String>,
        /// Local bookkeeping annotation (repeatable: --local-memo key=value)
        #[arg(long = "local-memo", value_parser = parse_memo_kv)]
        local_memo: Vec<(String, String)>,
    },
    /// Create Lightning invoice to receive cashu from LN
    #[command(name = "receive-from-ln", hide = true)]
    ReceiveFromLn {
        /// Wallet ID (omit to auto-select when exactly one cashu wallet exists)
        #[arg(long)]
        wallet: Option<String>,
        /// Amount in sats (base units)
        #[arg(long)]
        amount: Option<u64>,
        /// Wait until invoice is paid and auto-claim
        #[arg(long = "wait-until-paid")]
        wait_until_paid: bool,
        /// Timeout in seconds for wait-until-paid
        #[arg(long = "wait-timeout-s")]
        wait_timeout_s: Option<u64>,
        /// Poll interval in milliseconds for wait-until-paid
        #[arg(long = "wait-poll-interval-ms")]
        wait_poll_interval_ms: Option<u64>,
        /// Write invoice QR payload to an SVG file (interactive mode)
        #[arg(long = "qr-svg-file", default_value_t = false)]
        qr_svg_file: bool,
    },
    /// Claim minted tokens from a receive-from-ln quote
    #[command(name = "receive-from-ln-claim", hide = true)]
    ReceiveFromLnClaim {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
        /// Quote ID / payment hash from deposit
        #[arg(long = "ln-quote-id")]
        ln_quote_id: String,
    },
    /// Check cashu balance
    #[command(hide = true)]
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
    /// Restore lost proofs from mint (fixes counter/proof sync issues)
    Restore {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
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
}

#[derive(Subcommand)]
enum LnCommand {
    /// Wallet management
    Wallet {
        #[command(subcommand)]
        action: LnWalletAction,
    },
    /// Pay a Lightning invoice
    #[command(name = "send", hide = true)]
    Send {
        /// Source wallet ID (auto-selected if omitted)
        #[arg(long)]
        wallet: Option<String>,
        /// BOLT11 invoice to pay
        #[arg(long)]
        to: String,
        /// On-chain memo (LN description)
        #[arg(long = "onchain-memo")]
        onchain_memo: Option<String>,
        /// Local bookkeeping annotation (repeatable: --local-memo key=value)
        #[arg(long = "local-memo", value_parser = parse_memo_kv)]
        local_memo: Vec<(String, String)>,
    },
    /// Create a Lightning invoice to receive payment
    #[command(name = "receive", hide = true)]
    Receive {
        /// Wallet ID (omit to auto-select when exactly one LN wallet exists)
        #[arg(long)]
        wallet: Option<String>,
        /// Amount in sats (base units)
        #[arg(long)]
        amount: u64,
        /// Wait until invoice is paid and auto-claim
        #[arg(long = "wait-until-paid")]
        wait_until_paid: bool,
        /// Timeout in seconds for wait-until-paid
        #[arg(long = "wait-timeout-s")]
        wait_timeout_s: Option<u64>,
        /// Poll interval in milliseconds for wait-until-paid
        #[arg(long = "wait-poll-interval-ms")]
        wait_poll_interval_ms: Option<u64>,
        /// Write invoice QR payload to an SVG file (interactive mode)
        #[arg(long = "qr-svg-file", default_value_t = false)]
        qr_svg_file: bool,
    },
    /// Check invoice/payment status by transaction_id/payment_hash
    #[command(name = "invoice", hide = true)]
    Invoice {
        /// Transaction ID / payment hash
        #[arg(long = "transaction-id")]
        transaction_id: String,
    },
    /// Check balance
    #[command(hide = true)]
    Balance {
        /// Wallet ID (omit to show all ln wallets)
        #[arg(long)]
        wallet: Option<String>,
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
    #[command(name = "send", hide = true)]
    Send {
        /// Source wallet ID (auto-selected if omitted)
        #[arg(long)]
        wallet: Option<String>,
        /// Recipient Solana address (base58)
        #[arg(long)]
        to: String,
        /// Amount in token base units (lamports for SOL, smallest unit for SPL tokens)
        #[arg(long)]
        amount: u64,
        /// Token: "native" for SOL, "usdc", "usdt", or SPL mint address
        #[arg(long)]
        token: String,
        /// On-chain memo (SOL memo instruction)
        #[arg(long = "onchain-memo")]
        onchain_memo: Option<String>,
        /// Local bookkeeping annotation (repeatable: --local-memo key=value)
        #[arg(long = "local-memo", value_parser = parse_memo_kv)]
        local_memo: Vec<(String, String)>,
    },
    /// Show wallet receive address
    #[command(name = "receive", hide = true)]
    Receive {
        /// Wallet ID (omit to auto-select when exactly one sol wallet exists)
        #[arg(long)]
        wallet: Option<String>,
        /// On-chain memo to watch for (used with --wait)
        #[arg(long = "onchain-memo")]
        onchain_memo: Option<String>,
        /// Wait for a matching receive transaction to appear
        #[arg(long)]
        wait: bool,
        /// Timeout in seconds for --wait
        #[arg(long = "wait-timeout-s")]
        wait_timeout_s: Option<u64>,
        /// Poll interval in milliseconds for --wait
        #[arg(long = "wait-poll-interval-ms")]
        wait_poll_interval_ms: Option<u64>,
        /// Write receive address QR payload to an SVG file (interactive mode)
        #[arg(long = "qr-svg-file", default_value_t = false)]
        qr_svg_file: bool,
        /// Minimum confirmation depth before considering payment settled (requires --wait)
        #[arg(long = "min-confirmations")]
        min_confirmations: Option<u32>,
    },
    /// Check balance
    #[command(hide = true)]
    Balance {
        /// Wallet ID (omit to show all sol wallets)
        #[arg(long)]
        wallet: Option<String>,
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
    #[command(name = "send", hide = true)]
    Send {
        /// Source wallet ID (auto-selected if omitted)
        #[arg(long)]
        wallet: Option<String>,
        /// Recipient address (0x...)
        #[arg(long)]
        to: String,
        /// Amount in token base units (wei for ETH, smallest unit for ERC-20)
        #[arg(long)]
        amount: u64,
        /// Token: "native" for chain native, "usdc" or contract address for ERC-20
        #[arg(long)]
        token: String,
        /// On-chain memo
        #[arg(long = "onchain-memo")]
        onchain_memo: Option<String>,
        /// Local bookkeeping annotation (repeatable: --local-memo key=value)
        #[arg(long = "local-memo", value_parser = parse_memo_kv)]
        local_memo: Vec<(String, String)>,
    },
    /// Show wallet receive address
    #[command(name = "receive", hide = true)]
    Receive {
        /// Wallet ID (omit to auto-select when exactly one evm wallet exists)
        #[arg(long)]
        wallet: Option<String>,
        /// On-chain memo to watch for (used with --wait)
        #[arg(long = "onchain-memo")]
        onchain_memo: Option<String>,
        /// Wait for a matching receive transaction to appear
        #[arg(long)]
        wait: bool,
        /// Timeout in seconds for --wait
        #[arg(long = "wait-timeout-s")]
        wait_timeout_s: Option<u64>,
        /// Poll interval in milliseconds for --wait
        #[arg(long = "wait-poll-interval-ms")]
        wait_poll_interval_ms: Option<u64>,
        /// Minimum confirmation depth before considering payment settled (requires --wait)
        #[arg(long = "min-confirmations")]
        min_confirmations: Option<u32>,
    },
    /// Check balance
    #[command(hide = true)]
    Balance {
        /// Wallet ID (omit to show all evm wallets)
        #[arg(long)]
        wallet: Option<String>,
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
    #[command(name = "send", hide = true)]
    Send {
        /// Source wallet ID (auto-selected if omitted)
        #[arg(long)]
        wallet: Option<String>,
        /// Recipient Bitcoin address (bc1.../tb1...)
        #[arg(long)]
        to: String,
        /// Amount in satoshis
        #[arg(long)]
        amount: u64,
        /// On-chain memo (OP_RETURN is not supported; stored locally)
        #[arg(long = "onchain-memo")]
        onchain_memo: Option<String>,
        /// Local bookkeeping annotation (repeatable: --local-memo key=value)
        #[arg(long = "local-memo", value_parser = parse_memo_kv)]
        local_memo: Vec<(String, String)>,
    },
    /// Show wallet receive address
    #[command(name = "receive", hide = true)]
    Receive {
        /// Wallet ID (omit to auto-select when exactly one btc wallet exists)
        #[arg(long)]
        wallet: Option<String>,
        /// Wait for a matching receive transaction to appear
        #[arg(long)]
        wait: bool,
        /// Timeout in seconds for --wait
        #[arg(long = "wait-timeout-s")]
        wait_timeout_s: Option<u64>,
        /// Poll interval in milliseconds for --wait
        #[arg(long = "wait-poll-interval-ms")]
        wait_poll_interval_ms: Option<u64>,
    },
    /// Check balance
    #[command(hide = true)]
    Balance {
        /// Wallet ID (omit to show all btc wallets)
        #[arg(long)]
        wallet: Option<String>,
    },
}

#[derive(Subcommand)]
enum BtcWalletAction {
    /// Create a new Bitcoin wallet
    Create {
        /// Optional label
        #[arg(long)]
        label: Option<String>,
        /// Bitcoin sub-network: mainnet or signet (default: mainnet)
        #[arg(long = "btc-network", default_value = "mainnet")]
        btc_network: String,
        /// Address type: taproot or segwit (default: taproot)
        #[arg(long = "btc-address-type", default_value = "taproot")]
        btc_address_type: String,
        /// Custom Esplora API URL
        #[arg(long = "btc-esplora-url")]
        btc_esplora_url: Option<String>,
        /// Chain-source backend: esplora (default), core-rpc, electrum
        #[arg(long = "btc-backend", value_enum)]
        btc_backend: Option<CliBtcBackend>,
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

#[derive(clap::Args)]
struct WalletCreateArgs {
    /// Network
    #[arg(long, value_enum)]
    network: CliNetwork,
    /// Optional label
    #[arg(long)]
    label: Option<String>,
    /// Cashu mint URL (cashu only)
    #[arg(long = "cashu-mint")]
    cashu_mint: Option<String>,
    /// Existing BIP39 mnemonic secret to restore wallet (cashu/sol only)
    #[arg(long = "mnemonic-secret")]
    mnemonic_secret: Option<String>,
    /// Solana JSON-RPC endpoint (sol only, repeat for failover)
    #[arg(long = "sol-rpc-endpoint")]
    sol_rpc_endpoint: Vec<String>,
    /// EVM JSON-RPC endpoint (evm only, repeat for failover)
    #[arg(long = "evm-rpc-endpoint")]
    evm_rpc_endpoint: Vec<String>,
    /// EVM chain ID (evm only, default: 8453 = Base)
    #[arg(long = "chain-id")]
    chain_id: Option<u64>,
    /// Bitcoin sub-network: mainnet or signet (btc only)
    #[arg(long = "btc-network")]
    btc_network: Option<String>,
    /// Bitcoin address type: taproot or segwit (btc only)
    #[arg(long = "btc-address-type")]
    btc_address_type: Option<String>,
    /// Custom Esplora API URL (btc only)
    #[arg(long = "btc-esplora-url")]
    btc_esplora_url: Option<String>,
    /// BTC chain-source backend: esplora, core-rpc, electrum (btc only)
    #[arg(long = "btc-backend", value_enum)]
    btc_backend: Option<CliBtcBackend>,
    /// Bitcoin Core RPC URL (btc core-rpc only)
    #[arg(long = "btc-core-url")]
    btc_core_url: Option<String>,
    /// Bitcoin Core RPC auth "user:pass" (btc core-rpc only)
    #[arg(long = "btc-core-auth-secret")]
    btc_core_auth_secret: Option<String>,
    /// Electrum server URL (btc electrum only)
    #[arg(long = "btc-electrum-url")]
    btc_electrum_url: Option<String>,
    /// LN backend: nwc, phoenixd, lnbits (ln only)
    #[arg(long, value_enum)]
    backend: Option<CliLnBackend>,
    /// NWC connection URI secret (ln nwc backend)
    #[arg(long = "nwc-uri-secret")]
    nwc_uri_secret: Option<String>,
    /// Endpoint URL (ln phoenixd/lnbits backend)
    #[arg(long)]
    endpoint: Option<String>,
    /// Password secret (ln phoenixd backend)
    #[arg(long = "password-secret")]
    password_secret: Option<String>,
    /// Admin API key secret (ln lnbits backend)
    #[arg(long = "admin-key-secret")]
    admin_key_secret: Option<String>,
}

#[derive(Subcommand)]
enum WalletTopAction {
    /// Create a new wallet
    Create(Box<WalletCreateArgs>),
    /// List all wallets (cross-network)
    List {
        /// Filter by network: cashu, ln, sol, evm
        #[arg(long, value_enum)]
        network: Option<CliNetwork>,
    },
    /// Close a zero-balance wallet (auto-detect network by wallet ID)
    Close {
        /// Wallet ID
        wallet: String,
        /// Dangerously skip balance checks when closing wallet
        #[arg(long = "dangerously-skip-balance-check-and-may-lose-money")]
        dangerously_skip_balance_check_and_may_lose_money: bool,
    },
    /// Dangerously show wallet seed mnemonic / credential
    #[command(name = "dangerously-show-seed")]
    DangerouslyShowSeed {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
    },
    /// Restore lost cashu proofs from mint
    #[command(name = "cashu-restore")]
    CashuRestore {
        /// Wallet ID
        #[arg(long)]
        wallet: String,
    },
    /// Manage per-wallet configuration
    Config {
        #[command(subcommand)]
        action: WalletConfigAction,
    },
}

#[derive(Subcommand)]
enum WalletConfigAction {
    /// Show current wallet configuration
    Show {
        /// Wallet ID or label
        #[arg(long)]
        wallet: String,
    },
    /// Update wallet settings
    Set {
        /// Wallet ID or label
        #[arg(long)]
        wallet: String,
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
    TokenAdd {
        /// Wallet ID or label
        #[arg(long)]
        wallet: String,
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
    TokenRemove {
        /// Wallet ID or label
        #[arg(long)]
        wallet: String,
        /// Token symbol to remove
        #[arg(long)]
        symbol: String,
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
    /// Add a spend limit rule
    Add {
        /// Scope: global-usd-cents, network, or wallet
        #[arg(long, value_enum)]
        scope: CliSpendScope,
        /// Network name: cashu, ln, sol, evm (required for network/wallet scope)
        #[arg(long)]
        network: Option<String>,
        /// Wallet ID (required for wallet scope)
        #[arg(long)]
        wallet: Option<String>,
        /// Token: native, usdc, usdt (for network/wallet scoped limits on sol/evm)
        #[arg(long)]
        token: Option<String>,
        /// Time window: e.g. 30m, 1h, 24h, 7d
        #[arg(long)]
        window: String,
        /// Maximum spend in base units (or USD cents for global-usd-cents scope)
        #[arg(long)]
        max_spend: u64,
    },
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
enum CliSpendScope {
    #[value(name = "global-usd-cents")]
    GlobalUsdCents,
    Network,
    Wallet,
}

impl From<CliSpendScope> for SpendScope {
    fn from(c: CliSpendScope) -> Self {
        match c {
            CliSpendScope::GlobalUsdCents => SpendScope::GlobalUsdCents,
            CliSpendScope::Network => SpendScope::Network,
            CliSpendScope::Wallet => SpendScope::Wallet,
        }
    }
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
pub fn parse_subcommand(args: &[&str], id: &str) -> Result<Input, String> {
    let parsed = SubcommandParser::try_parse_from(args).map_err(|e| e.to_string())?;
    command_to_input(parsed.command, id)
}

/// Render clap help for the given args (e.g. `&["--help"]`, `&["cashu", "--help"]`).
pub fn subcommand_help(args: &[&str]) -> String {
    match SubcommandParser::try_parse_from(args) {
        Ok(_) => String::new(),
        Err(e) => e.to_string(),
    }
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
                println!("{e}");
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
        #[cfg(feature = "mcp")]
        RuntimeMode::Mcp => {
            if output != OutputFormat::Json {
                return Err(CliError {
                    message: "--output is not supported in MCP mode (MCP uses JSONRPC transport)"
                        .to_string(),
                    hint: Some("remove --output or use --mode pipe instead".to_string()),
                });
            }
            return Ok(Mode::Mcp(PipeInit {
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
    if !lower.starts_with("lnbc") && !lower.starts_with("lntb") && !lower.starts_with("lnbcrt") {
        return Err(format!(
            "invalid Lightning invoice '{to}': must start with lnbc, lntb, or lnbcrt"
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
             afpay wallet config token-add --wallet <id> --symbol <name> --address {token}"
        ));
    }
    Ok(())
}

// ═══════════════════════════════════════════
// Unified send / receive dispatchers
// ═══════════════════════════════════════════

#[allow(clippy::too_many_arguments)]
fn unified_send_to_input(
    network: Option<CliNetwork>,
    to: Option<String>,
    amount: Option<u64>,
    token: Option<String>,
    wallet: Option<String>,
    onchain_memo: Option<String>,
    local_memo: Vec<(String, String)>,
    mint_url: Vec<String>,
    id: &str,
) -> Result<Input, String> {
    let local_memo = memo_vec_to_map(local_memo);
    let wallet = wallet.filter(|s| !s.is_empty());
    let network = match network {
        Some(n) => n,
        None => {
            if wallet.is_none() {
                return Err("--network is required when --wallet is not specified".into());
            }
            // No network specified but wallet given — build a generic Pay/Send
            // and let handler infer network from wallet metadata.
            if let Some(to) = to {
                return Ok(Input::Send {
                    id: id.to_string(),
                    wallet,
                    network: None,
                    to,
                    onchain_memo,
                    local_memo,
                    mints: None,
                });
            }
            // No --to: assume cashu-style P2P send
            let amount = amount.ok_or("--amount is required for send without --to")?;
            return Ok(Input::CashuSend {
                id: id.to_string(),
                wallet,
                amount: Amount {
                    value: amount,
                    token: "sats".to_string(),
                },
                onchain_memo,
                local_memo,
                mints: if mint_url.is_empty() {
                    None
                } else {
                    Some(mint_url)
                },
            });
        }
    };
    match network {
        CliNetwork::Cashu => {
            if token.is_some() {
                return Err("--token is not supported for cashu; cashu operates in sats".into());
            }
            if onchain_memo.is_some() {
                return Err(
                    "--onchain-memo is not supported for cashu; use --local-memo for bookkeeping"
                        .into(),
                );
            }
            let amount = amount.ok_or("--amount is required for cashu send")?;
            if let Some(to) = to {
                // If --to is provided, treat as send-to-ln (bolt11)
                validate_bolt11(&to)?;
                Ok(Input::Send {
                    id: id.to_string(),
                    wallet,
                    network: Some(Network::Cashu),
                    to,
                    onchain_memo: None,
                    local_memo,
                    mints: if mint_url.is_empty() {
                        None
                    } else {
                        Some(mint_url)
                    },
                })
            } else {
                Ok(Input::CashuSend {
                    id: id.to_string(),
                    wallet,
                    amount: Amount {
                        value: amount,
                        token: "sats".to_string(),
                    },
                    onchain_memo: None,
                    local_memo,
                    mints: if mint_url.is_empty() {
                        None
                    } else {
                        Some(mint_url)
                    },
                })
            }
        }
        CliNetwork::Ln => {
            if token.is_some() {
                return Err("--token is not supported for ln; Lightning uses sats".into());
            }
            if onchain_memo.is_some() {
                return Err(
                    "--onchain-memo is not supported for ln; use --local-memo for bookkeeping"
                        .into(),
                );
            }
            if !mint_url.is_empty() {
                return Err("--cashu-mint is only supported for cashu".into());
            }
            if amount.is_some() {
                return Err(
                    "--amount is not accepted for ln send; the invoice encodes the amount".into(),
                );
            }
            let to = to.ok_or("--to is required for ln send (bolt11 invoice)")?;
            validate_bolt11(&to)?;
            Ok(Input::Send {
                id: id.to_string(),
                wallet,
                network: Some(Network::Ln),
                to,
                onchain_memo: None,
                local_memo,
                mints: None,
            })
        }
        CliNetwork::Sol => {
            if !mint_url.is_empty() {
                return Err("--cashu-mint is only supported for cashu".into());
            }
            let token = token.ok_or("--token is required for sol send (e.g. native, usdc)")?;
            validate_token_not_contract(&token)?;
            let amount = amount.ok_or("--amount is required for sol send")?;
            let to = to.ok_or("--to is required for sol send (Solana address)")?;
            validate_sol_address(&to)?;
            let target = format!("solana:{to}?amount={amount}&token={token}");
            Ok(Input::Send {
                id: id.to_string(),
                wallet,
                network: Some(Network::Sol),
                to: target,
                onchain_memo,
                local_memo,
                mints: None,
            })
        }
        CliNetwork::Evm => {
            if !mint_url.is_empty() {
                return Err("--cashu-mint is only supported for cashu".into());
            }
            let token = token.ok_or("--token is required for evm send (e.g. native, usdc)")?;
            validate_token_not_contract(&token)?;
            let amount = amount.ok_or("--amount is required for evm send")?;
            let to = to.ok_or("--to is required for evm send (0x address)")?;
            validate_evm_address(&to)?;
            let target = format!("ethereum:{to}?amount={amount}&token={token}");
            Ok(Input::Send {
                id: id.to_string(),
                wallet,
                network: Some(Network::Evm),
                to: target,
                onchain_memo,
                local_memo,
                mints: None,
            })
        }
        CliNetwork::Btc => {
            if !mint_url.is_empty() {
                return Err("--cashu-mint is only supported for cashu".into());
            }
            if token.is_some() {
                return Err("--token is not supported for btc; Bitcoin operates in sats".into());
            }
            let amount = amount.ok_or("--amount is required for btc send (in satoshis)")?;
            let to = to.ok_or("--to is required for btc send (Bitcoin address)")?;
            let target = format!("bitcoin:{to}?amount={amount}");
            Ok(Input::Send {
                id: id.to_string(),
                wallet,
                network: Some(Network::Btc),
                to: target,
                onchain_memo,
                local_memo,
                mints: None,
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn unified_receive_to_input(
    network: Option<CliNetwork>,
    cashu_token: Option<String>,
    amount: Option<u64>,
    token: Option<String>,
    wallet: Option<String>,
    onchain_memo: Option<String>,
    wait: bool,
    wait_timeout_s: Option<u64>,
    wait_poll_interval_ms: Option<u64>,
    qr_svg_file: bool,
    ln_quote_id: Option<String>,
    min_confirmations: Option<u32>,
    id: &str,
) -> Result<Input, String> {
    // --ln-quote-id: resume claiming from a previous deposit (cashu/ln only)
    if let Some(quote_id) = ln_quote_id {
        if let Some(ref n) = network {
            if !matches!(n, CliNetwork::Cashu | CliNetwork::Ln) {
                return Err("--ln-quote-id is only supported for cashu and ln networks".into());
            }
        }
        let wallet = wallet
            .filter(|s| !s.is_empty())
            .ok_or("--wallet is required with --ln-quote-id")?;
        return Ok(Input::ReceiveClaim {
            id: id.to_string(),
            wallet,
            quote_id,
        });
    }

    // Validate --wait-timeout-s / --wait-poll-interval-ms require --wait
    if !wait && (wait_timeout_s.is_some() || wait_poll_interval_ms.is_some()) {
        return Err("--wait-timeout-s and --wait-poll-interval-ms require --wait".into());
    }

    // Validate --min-confirmations requires --wait and sol/evm network
    if min_confirmations.is_some() {
        if !wait {
            return Err("--min-confirmations requires --wait".into());
        }
        if let Some(ref n) = network {
            if !matches!(n, CliNetwork::Sol | CliNetwork::Evm) {
                return Err(
                    "--min-confirmations is only supported for sol and evm networks".into(),
                );
            }
        }
    }

    let wallet = wallet.filter(|s| !s.is_empty());
    let network = match network {
        Some(n) => n,
        None => {
            if wallet.is_none() && cashu_token.is_none() {
                return Err("--network is required when --wallet is not specified".into());
            }
            // --cashu-token: cashu token receive (network-independent)
            if let Some(cashu_token) = cashu_token {
                return Ok(Input::CashuReceive {
                    id: id.to_string(),
                    wallet,
                    token: cashu_token,
                });
            }
            // wallet given, no network — let handler infer from wallet metadata
            return Ok(Input::Receive {
                id: id.to_string(),
                wallet: wallet.unwrap_or_default(),
                network: None,
                amount: amount.map(|v| Amount {
                    value: v,
                    token: "sats".to_string(),
                }),
                onchain_memo,
                wait_until_paid: wait,
                wait_timeout_s,
                wait_poll_interval_ms,
                write_qr_svg_file: qr_svg_file,
                min_confirmations,
            });
        }
    };
    match network {
        CliNetwork::Cashu => {
            if token.is_some() {
                return Err("--token is not supported for cashu receive".into());
            }
            if onchain_memo.is_some() {
                return Err(
                    "--onchain-memo is not supported for cashu; use --local-memo for bookkeeping"
                        .into(),
                );
            }
            if let Some(cashu_token) = cashu_token {
                // Redeem a cashu token
                if amount.is_some() {
                    return Err("--amount is not accepted when receiving a cashu token (amount is encoded in the token)".into());
                }
                Ok(Input::CashuReceive {
                    id: id.to_string(),
                    wallet,
                    token: cashu_token,
                })
            } else {
                // Create a receive-from-LN invoice
                Ok(Input::Receive {
                    id: id.to_string(),
                    wallet: wallet.unwrap_or_default(),
                    network: Some(Network::Cashu),
                    amount: amount.map(|v| Amount {
                        value: v,
                        token: "sats".to_string(),
                    }),
                    onchain_memo: None,
                    wait_until_paid: wait,
                    wait_timeout_s,
                    wait_poll_interval_ms,
                    write_qr_svg_file: qr_svg_file,
                    min_confirmations: None,
                })
            }
        }
        CliNetwork::Ln => {
            if token.is_some() {
                return Err("--token is not supported for ln receive".into());
            }
            if onchain_memo.is_some() {
                return Err(
                    "--onchain-memo is not supported for ln; use --local-memo for bookkeeping"
                        .into(),
                );
            }
            if cashu_token.is_some() {
                return Err("--cashu-token is only supported for cashu receive".into());
            }
            let amount = amount.ok_or("--amount is required for ln receive")?;
            Ok(Input::Receive {
                id: id.to_string(),
                wallet: wallet.unwrap_or_default(),
                network: Some(Network::Ln),
                amount: Some(Amount {
                    value: amount,
                    token: "sats".to_string(),
                }),
                onchain_memo: None,
                wait_until_paid: wait,
                wait_timeout_s,
                wait_poll_interval_ms,
                write_qr_svg_file: qr_svg_file,
                min_confirmations: None,
            })
        }
        CliNetwork::Sol => {
            if cashu_token.is_some() {
                return Err("--cashu-token is only supported for cashu receive".into());
            }
            if let Some(ref t) = token {
                validate_token_not_contract(t)?;
            }
            if wait && onchain_memo.is_none() && amount.is_none() {
                return Err(
                    "--wait requires --onchain-memo or --amount to match transactions".into(),
                );
            }
            Ok(Input::Receive {
                id: id.to_string(),
                wallet: wallet.unwrap_or_default(),
                network: Some(Network::Sol),
                amount: amount.map(|v| Amount {
                    value: v,
                    token: token.clone().unwrap_or_else(|| "native".to_string()),
                }),
                onchain_memo: onchain_memo.filter(|s| !s.trim().is_empty()),
                wait_until_paid: wait,
                wait_timeout_s,
                wait_poll_interval_ms,
                write_qr_svg_file: qr_svg_file,
                min_confirmations,
            })
        }
        CliNetwork::Evm => {
            if cashu_token.is_some() {
                return Err("--cashu-token is only supported for cashu receive".into());
            }
            if let Some(ref t) = token {
                validate_token_not_contract(t)?;
            }
            if wait
                && onchain_memo
                    .as_deref()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .is_some()
            {
                return Err(
                    "--onchain-memo is not yet supported for evm receive --wait; use --amount"
                        .into(),
                );
            }
            if wait && amount.is_none() {
                return Err("--wait for evm receive requires --amount".into());
            }
            Ok(Input::Receive {
                id: id.to_string(),
                wallet: wallet.unwrap_or_default(),
                network: Some(Network::Evm),
                amount: amount.map(|v| Amount {
                    value: v,
                    token: token.clone().unwrap_or_else(|| "native".to_string()),
                }),
                onchain_memo,
                wait_until_paid: wait,
                wait_timeout_s,
                wait_poll_interval_ms,
                write_qr_svg_file: false,
                min_confirmations,
            })
        }
        CliNetwork::Btc => {
            if cashu_token.is_some() {
                return Err("--cashu-token is only supported for cashu receive".into());
            }
            if token.is_some() {
                return Err("--token is not supported for btc receive".into());
            }
            Ok(Input::Receive {
                id: id.to_string(),
                wallet: wallet.unwrap_or_default(),
                network: Some(Network::Btc),
                amount: amount.map(|v| Amount {
                    value: v,
                    token: "sats".to_string(),
                }),
                onchain_memo: None,
                wait_until_paid: wait,
                wait_timeout_s,
                wait_poll_interval_ms,
                write_qr_svg_file: false,
                min_confirmations: None,
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn unified_wallet_create(
    network: CliNetwork,
    label: Option<String>,
    cashu_mint: Option<String>,
    mnemonic_secret: Option<String>,
    sol_rpc_endpoint: Vec<String>,
    evm_rpc_endpoint: Vec<String>,
    chain_id: Option<u64>,
    backend: Option<CliLnBackend>,
    nwc_uri_secret: Option<String>,
    endpoint: Option<String>,
    password_secret: Option<String>,
    admin_key_secret: Option<String>,
    btc_network: Option<String>,
    btc_address_type: Option<String>,
    btc_esplora_url: Option<String>,
    btc_backend: Option<CliBtcBackend>,
    btc_core_url: Option<String>,
    btc_core_auth_secret: Option<String>,
    btc_electrum_url: Option<String>,
    id: &str,
) -> Result<Input, String> {
    match network {
        CliNetwork::Cashu => {
            let mint_url = cashu_mint.ok_or("--cashu-mint is required for cashu wallet create")?;
            Ok(Input::WalletCreate {
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
            })
        }
        CliNetwork::Sol => {
            if sol_rpc_endpoint.is_empty() {
                return Err("--sol-rpc-endpoint is required for sol wallet create".into());
            }
            Ok(Input::WalletCreate {
                id: id.to_string(),
                network: Network::Sol,
                label,
                mint_url: None,
                rpc_endpoints: sol_rpc_endpoint,
                chain_id: None,
                mnemonic_secret,
                btc_esplora_url: None,
                btc_network: None,
                btc_address_type: None,
                btc_backend: None,
                btc_core_url: None,
                btc_core_auth_secret: None,
                btc_electrum_url: None,
            })
        }
        CliNetwork::Evm => {
            if evm_rpc_endpoint.is_empty() {
                return Err("--evm-rpc-endpoint is required for evm wallet create".into());
            }
            Ok(Input::WalletCreate {
                id: id.to_string(),
                network: Network::Evm,
                label,
                mint_url: None,
                rpc_endpoints: evm_rpc_endpoint,
                chain_id: Some(chain_id.unwrap_or(8453)),
                mnemonic_secret: None,
                btc_esplora_url: None,
                btc_network: None,
                btc_address_type: None,
                btc_backend: None,
                btc_core_url: None,
                btc_core_auth_secret: None,
                btc_electrum_url: None,
            })
        }
        CliNetwork::Ln => {
            let backend = backend
                .ok_or("--backend is required for ln wallet create (nwc, phoenixd, lnbits)")?;
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
                    endpoint: Some(endpoint.ok_or("--endpoint is required for phoenixd backend")?),
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
                    endpoint: Some(endpoint.ok_or("--endpoint is required for lnbits backend")?),
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
        CliNetwork::Btc => Ok(Input::WalletCreate {
            id: id.to_string(),
            network: Network::Btc,
            label,
            mint_url: None,
            rpc_endpoints: vec![],
            chain_id: None,
            mnemonic_secret,
            btc_esplora_url,
            btc_network: Some(btc_network.unwrap_or_else(|| "mainnet".to_string())),
            btc_address_type: Some(btc_address_type.unwrap_or_else(|| "taproot".to_string())),
            btc_backend: btc_backend.map(Into::into),
            btc_core_url,
            btc_core_auth_secret,
            btc_electrum_url,
        }),
    }
}

fn command_to_input(cmd: PayCommand, id: &str) -> Result<Input, String> {
    match cmd {
        PayCommand::Send {
            network,
            to,
            amount,
            token,
            wallet,
            onchain_memo,
            local_memo,
            cashu_mint,
        } => unified_send_to_input(
            network,
            to,
            amount,
            token,
            wallet,
            onchain_memo,
            local_memo,
            cashu_mint,
            id,
        ),
        PayCommand::Receive {
            network,
            cashu_token,
            amount,
            token,
            wallet,
            onchain_memo,
            wait,
            wait_timeout_s,
            wait_poll_interval_ms,
            qr_svg_file,
            ln_quote_id,
            min_confirmations,
        } => unified_receive_to_input(
            network,
            cashu_token,
            amount,
            token,
            wallet,
            onchain_memo,
            wait,
            wait_timeout_s,
            wait_poll_interval_ms,
            qr_svg_file,
            ln_quote_id,
            min_confirmations,
            id,
        ),
        PayCommand::Cashu { action } => cashu_command_to_input(action, id),
        PayCommand::Ln { action } => ln_command_to_input(action, id),
        PayCommand::Sol { action } => sol_command_to_input(action, id),
        PayCommand::Evm { action } => evm_command_to_input(action, id),
        PayCommand::Btc { action } => btc_command_to_input(action, id),
        PayCommand::Wallet { action } => match action {
            WalletTopAction::Create(args) => unified_wallet_create(
                args.network,
                args.label,
                args.cashu_mint,
                args.mnemonic_secret,
                args.sol_rpc_endpoint,
                args.evm_rpc_endpoint,
                args.chain_id,
                args.backend,
                args.nwc_uri_secret,
                args.endpoint,
                args.password_secret,
                args.admin_key_secret,
                args.btc_network,
                args.btc_address_type,
                args.btc_esplora_url,
                args.btc_backend,
                args.btc_core_url,
                args.btc_core_auth_secret,
                args.btc_electrum_url,
                id,
            ),
            WalletTopAction::List { network } => Ok(Input::WalletList {
                id: id.to_string(),
                network: network.map(Into::into),
            }),
            WalletTopAction::Close {
                wallet,
                dangerously_skip_balance_check_and_may_lose_money,
            } => Ok(Input::WalletClose {
                id: id.to_string(),
                wallet,
                dangerously_skip_balance_check_and_may_lose_money,
            }),
            WalletTopAction::DangerouslyShowSeed { wallet } => Ok(Input::WalletShowSeed {
                id: id.to_string(),
                wallet,
            }),
            WalletTopAction::CashuRestore { wallet } => Ok(Input::Restore {
                id: id.to_string(),
                wallet,
            }),
            WalletTopAction::Config { action } => match action {
                WalletConfigAction::Show { wallet } => Ok(Input::WalletConfigShow {
                    id: id.to_string(),
                    wallet,
                }),
                WalletConfigAction::Set {
                    wallet,
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
                WalletConfigAction::TokenAdd {
                    wallet,
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
                WalletConfigAction::TokenRemove { wallet, symbol } => {
                    Ok(Input::WalletConfigTokenRemove {
                        id: id.to_string(),
                        wallet,
                        symbol,
                    })
                }
            },
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
            LimitAction::Add {
                scope,
                network,
                wallet,
                token,
                window,
                max_spend,
            } => {
                let window_s = parse_window(&window)?;
                Ok(Input::LimitAdd {
                    id: id.to_string(),
                    limit: SpendLimit {
                        rule_id: None,
                        scope: scope.into(),
                        network,
                        wallet,
                        window_s,
                        max_spend,
                        token,
                    },
                })
            }
            LimitAction::Remove { rule_id } => Ok(Input::LimitRemove {
                id: id.to_string(),
                rule_id,
            }),
            LimitAction::List => Ok(Input::LimitList { id: id.to_string() }),
        },
    }
}

fn cashu_command_to_input(cmd: CashuCommand, id: &str) -> Result<Input, String> {
    match cmd {
        CashuCommand::Send {
            wallet,
            amount,
            onchain_memo,
            local_memo,
            mint_url,
            to,
        } => {
            if to.is_some() {
                return Err("cashu send generates a P2P cashu token — it does not send to an address. To pay a Lightning invoice, use: cashu send-to-ln --to <bolt11>".to_string());
            }
            if onchain_memo.is_some() {
                return Err(
                    "--onchain-memo is not supported for cashu; use --local-memo for bookkeeping"
                        .into(),
                );
            }
            Ok(Input::CashuSend {
                id: id.to_string(),
                wallet: wallet.filter(|s| !s.is_empty()),
                amount: Amount {
                    value: amount,
                    token: "sats".to_string(),
                },
                onchain_memo: None,
                local_memo: memo_vec_to_map(local_memo),
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
        CashuCommand::SendToLn {
            wallet,
            to,
            onchain_memo,
            local_memo,
        } => {
            if onchain_memo.is_some() {
                return Err(
                    "--onchain-memo is not supported for cashu; use --local-memo for bookkeeping"
                        .into(),
                );
            }
            Ok(Input::Send {
                id: id.to_string(),
                wallet: wallet.filter(|s| !s.is_empty()),
                network: Some(Network::Cashu),
                to,
                onchain_memo: None,
                local_memo: memo_vec_to_map(local_memo),
                mints: None,
            })
        }
        CashuCommand::ReceiveFromLn {
            wallet,
            amount,
            wait_until_paid,
            wait_timeout_s,
            wait_poll_interval_ms,
            qr_svg_file,
        } => {
            let resolved = amount.map(|v| Amount {
                value: v,
                token: "sats".to_string(),
            });
            Ok(Input::Receive {
                id: id.to_string(),
                wallet: wallet.filter(|s| !s.is_empty()).unwrap_or_default(),
                network: Some(Network::Cashu),
                amount: resolved,
                onchain_memo: None,
                wait_until_paid,
                wait_timeout_s,
                wait_poll_interval_ms,
                write_qr_svg_file: qr_svg_file,
                min_confirmations: None,
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
        },
        CashuCommand::Restore { wallet } => Ok(Input::Restore {
            id: id.to_string(),
            wallet,
        }),
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
            wallet,
            to,
            onchain_memo,
            local_memo,
        } => {
            if onchain_memo.is_some() {
                return Err(
                    "--onchain-memo is not supported for ln; use --local-memo for bookkeeping"
                        .into(),
                );
            }
            validate_bolt11(&to)?;
            Ok(Input::Send {
                id: id.to_string(),
                wallet: wallet.filter(|s| !s.is_empty()),
                network: Some(Network::Ln),
                to,
                onchain_memo: None,
                local_memo: memo_vec_to_map(local_memo),
                mints: None,
            })
        }
        LnCommand::Receive {
            wallet,
            amount,
            wait_until_paid,
            wait_timeout_s,
            wait_poll_interval_ms,
            qr_svg_file,
        } => Ok(Input::Receive {
            id: id.to_string(),
            wallet: wallet.filter(|s| !s.is_empty()).unwrap_or_default(),
            network: Some(Network::Ln),
            amount: Some(Amount {
                value: amount,
                token: "sats".to_string(),
            }),
            onchain_memo: None,
            wait_until_paid,
            wait_timeout_s,
            wait_poll_interval_ms,
            write_qr_svg_file: qr_svg_file,
            min_confirmations: None,
        }),
        LnCommand::Invoice { transaction_id } => Ok(Input::HistoryStatus {
            id: id.to_string(),
            transaction_id,
        }),
        LnCommand::Balance { wallet } => Ok(Input::Balance {
            id: id.to_string(),
            wallet: wallet.filter(|s| !s.is_empty()),
            network: Some(Network::Ln),
            check: false,
        }),
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
            wallet,
            to,
            amount,
            token,
            onchain_memo,
            local_memo,
        } => {
            validate_sol_address(&to)?;
            validate_token_not_contract(&token)?;
            let target = format!("solana:{to}?amount={amount}&token={token}");
            Ok(Input::Send {
                id: id.to_string(),
                wallet: wallet.filter(|s| !s.is_empty()),
                network: Some(Network::Sol),
                to: target,
                onchain_memo,
                local_memo: memo_vec_to_map(local_memo),
                mints: None,
            })
        }
        SolCommand::Receive {
            wallet,
            onchain_memo,
            wait,
            wait_timeout_s,
            wait_poll_interval_ms,
            qr_svg_file,
            min_confirmations,
        } => Ok(Input::Receive {
            id: id.to_string(),
            wallet: wallet.filter(|s| !s.is_empty()).unwrap_or_default(),
            network: Some(Network::Sol),
            amount: None,
            onchain_memo: onchain_memo.filter(|s| !s.trim().is_empty()),
            wait_until_paid: wait,
            wait_timeout_s,
            wait_poll_interval_ms,
            write_qr_svg_file: qr_svg_file,
            min_confirmations,
        }),
        SolCommand::Balance { wallet } => Ok(Input::Balance {
            id: id.to_string(),
            wallet: wallet.filter(|s| !s.is_empty()),
            network: Some(Network::Sol),
            check: false,
        }),
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
            wallet,
            to,
            amount,
            token,
            onchain_memo,
            local_memo,
        } => {
            validate_evm_address(&to)?;
            validate_token_not_contract(&token)?;
            let target = format!("ethereum:{to}?amount={amount}&token={token}");
            Ok(Input::Send {
                id: id.to_string(),
                wallet: wallet.filter(|s| !s.is_empty()),
                network: Some(Network::Evm),
                to: target,
                onchain_memo,
                local_memo: memo_vec_to_map(local_memo),
                mints: None,
            })
        }
        EvmCommand::Receive {
            wallet,
            onchain_memo,
            wait,
            wait_timeout_s,
            wait_poll_interval_ms,
            min_confirmations,
        } => {
            if wait
                && onchain_memo
                    .as_deref()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .is_some()
            {
                return Err(
                    "--onchain-memo is not yet supported for evm receive --wait; use --amount"
                        .into(),
                );
            }
            if wait {
                return Err(
                    "evm receive --wait requires --amount; use unified receive command".into(),
                );
            }
            Ok(Input::Receive {
                id: id.to_string(),
                wallet: wallet.filter(|s| !s.is_empty()).unwrap_or_default(),
                network: Some(Network::Evm),
                amount: None,
                onchain_memo,
                wait_until_paid: wait,
                wait_timeout_s,
                wait_poll_interval_ms,
                write_qr_svg_file: false,
                min_confirmations,
            })
        }
        EvmCommand::Balance { wallet } => Ok(Input::Balance {
            id: id.to_string(),
            wallet: wallet.filter(|s| !s.is_empty()),
            network: Some(Network::Evm),
            check: false,
        }),
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
            wallet,
            to,
            amount,
            onchain_memo,
            local_memo,
        } => {
            let target = format!("bitcoin:{to}?amount={amount}");
            Ok(Input::Send {
                id: id.to_string(),
                wallet: wallet.filter(|s| !s.is_empty()),
                network: Some(Network::Btc),
                to: target,
                onchain_memo,
                local_memo: memo_vec_to_map(local_memo),
                mints: None,
            })
        }
        BtcCommand::Receive {
            wallet,
            wait,
            wait_timeout_s,
            wait_poll_interval_ms,
        } => Ok(Input::Receive {
            id: id.to_string(),
            wallet: wallet.filter(|s| !s.is_empty()).unwrap_or_default(),
            network: Some(Network::Btc),
            amount: None,
            onchain_memo: None,
            wait_until_paid: wait,
            wait_timeout_s,
            wait_poll_interval_ms,
            write_qr_svg_file: false,
            min_confirmations: None,
        }),
        BtcCommand::Balance { wallet } => Ok(Input::Balance {
            id: id.to_string(),
            wallet: wallet.filter(|s| !s.is_empty()),
            network: Some(Network::Btc),
            check: false,
        }),
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
mod tests {
    use super::*;

    #[test]
    fn parse_window_minutes() {
        assert_eq!(parse_window("30m").unwrap(), 1800);
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
                "limit",
                "add",
                "--scope",
                "network",
                "--network",
                "cashu",
                "--window",
                "1h",
                "--max-spend",
                "10000",
            ],
            "t_limit_1",
        )
        .expect("limit add should parse");

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
                "limit",
                "add",
                "--scope",
                "global-usd-cents",
                "--window",
                "24h",
                "--max-spend",
                "50000",
            ],
            "t_limit_2",
        )
        .expect("limit add global-usd-cents scope should parse");

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
                "limit",
                "add",
                "--scope",
                "network",
                "--network",
                "evm",
                "--token",
                "usdc",
                "--window",
                "24h",
                "--max-spend",
                "100000000",
            ],
            "t_limit_2b",
        )
        .expect("limit add network scope with token should parse");

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
        let input = parse_subcommand(&["ln", "receive", "--amount", "100"], "t_1")
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
        let input = parse_subcommand(&["cashu", "receive-from-ln", "--amount", "100"], "t_2")
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
                "--amount",
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
                "--amount",
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
            &["cashu", "send", "--amount", "100", "--to", "lnbc1..."],
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
    fn parse_ln_invoice_maps_to_tx_status() {
        let input = parse_subcommand(&["ln", "invoice", "--transaction-id", "abc123"], "t_33")
            .expect("ln invoice should parse");

        match input {
            Input::HistoryStatus { transaction_id, .. } => {
                assert_eq!(transaction_id, "abc123");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_wallet_close_top_level() {
        let input = parse_subcommand(&["wallet", "close", "w_1a2b3c4d"], "t_4")
            .expect("wallet close should parse");

        match input {
            Input::WalletClose {
                wallet,
                dangerously_skip_balance_check_and_may_lose_money,
                ..
            } => {
                assert_eq!(wallet, "w_1a2b3c4d");
                assert!(!dangerously_skip_balance_check_and_may_lose_money);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_wallet_close_dangerous_skip_flag_top_level() {
        let input = parse_subcommand(
            &[
                "wallet",
                "close",
                "w_1a2b3c4d",
                "--dangerously-skip-balance-check-and-may-lose-money",
            ],
            "t_5",
        )
        .expect("wallet close dangerous skip flag should parse");

        match input {
            Input::WalletClose {
                wallet,
                dangerously_skip_balance_check_and_may_lose_money,
                ..
            } => {
                assert_eq!(wallet, "w_1a2b3c4d");
                assert!(dangerously_skip_balance_check_and_may_lose_money);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_cashu_send_amount() {
        let input = parse_subcommand(&["cashu", "send", "--amount", "500"], "t_unified_1")
            .expect("cashu send --amount should parse");

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
        let input = parse_subcommand(&["ln", "receive", "--amount", "1000"], "t_unified_4")
            .expect("ln receive --amount should parse");

        match input {
            Input::Receive { amount, .. } => {
                let a = amount.expect("amount should be set");
                assert_eq!(a.value, 1000);
                assert_eq!(a.token, "sats");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    // ═══════════════════════════════════════════
    // Unified send tests
    // ═══════════════════════════════════════════

    #[test]
    fn parse_unified_send_cashu() {
        let input = parse_subcommand(&["send", "--network", "cashu", "--amount", "500"], "t_us_1")
            .expect("unified cashu send should parse");
        match input {
            Input::CashuSend { amount, .. } => {
                assert_eq!(amount.value, 500);
                assert_eq!(amount.token, "sats");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_unified_send_ln() {
        let input = parse_subcommand(
            &["send", "--network", "ln", "--to", "lnbc1exampleinvoice"],
            "t_us_2",
        )
        .expect("unified ln send should parse");
        match input {
            Input::Send { network, to, .. } => {
                assert_eq!(network, Some(Network::Ln));
                assert_eq!(to, "lnbc1exampleinvoice");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_unified_send_sol() {
        let input = parse_subcommand(
            &[
                "send",
                "--network",
                "sol",
                "--to",
                "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU",
                "--amount",
                "1000000",
                "--token",
                "native",
            ],
            "t_us_3",
        )
        .expect("unified sol send should parse");
        match input {
            Input::Send { to, network, .. } => {
                assert_eq!(network, Some(Network::Sol));
                assert!(to.contains("amount=1000000"));
                assert!(to.contains("token=native"));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_unified_send_evm() {
        let input = parse_subcommand(
            &[
                "send",
                "--network",
                "evm",
                "--to",
                "0x1234567890abcdef1234567890abcdef12345678",
                "--amount",
                "1000000000",
                "--token",
                "native",
            ],
            "t_us_4",
        )
        .expect("unified evm send should parse");
        match input {
            Input::Send { to, network, .. } => {
                assert_eq!(network, Some(Network::Evm));
                assert!(to.contains("amount=1000000000"));
                assert!(to.contains("token=native"));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_unified_send_cashu_rejects_token() {
        let err = parse_subcommand(
            &[
                "send",
                "--network",
                "cashu",
                "--amount",
                "100",
                "--token",
                "sats",
            ],
            "t_us_5",
        )
        .expect_err("cashu send should reject --token");
        assert!(
            err.contains("--token"),
            "error should mention --token: {err}"
        );
    }

    #[test]
    fn parse_unified_send_ln_rejects_amount() {
        let err = parse_subcommand(
            &[
                "send",
                "--network",
                "ln",
                "--to",
                "lnbc1example",
                "--amount",
                "100",
            ],
            "t_us_6",
        )
        .expect_err("ln send should reject --amount");
        assert!(
            err.contains("--amount"),
            "error should mention --amount: {err}"
        );
    }

    #[test]
    fn parse_unified_send_sol_requires_token() {
        let err = parse_subcommand(
            &[
                "send",
                "--network",
                "sol",
                "--to",
                "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU",
                "--amount",
                "100",
            ],
            "t_us_7",
        )
        .expect_err("sol send should require --token");
        assert!(
            err.contains("--token"),
            "error should mention --token: {err}"
        );
    }

    #[test]
    fn parse_unified_send_sol_rejects_contract_address() {
        let err = parse_subcommand(
            &[
                "send",
                "--network",
                "sol",
                "--to",
                "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU",
                "--amount",
                "100",
                "--token",
                "0xabcdef1234567890abcdef1234567890abcdef12",
            ],
            "t_us_8",
        )
        .expect_err("sol send should reject contract address as --token");
        assert!(
            err.contains("token-add"),
            "error should hint token-add: {err}"
        );
    }

    #[test]
    fn parse_unified_send_sol_rejects_bad_address() {
        let err = parse_subcommand(
            &[
                "send",
                "--network",
                "sol",
                "--to",
                "0x1234567890abcdef1234567890abcdef12345678",
                "--amount",
                "100",
                "--token",
                "native",
            ],
            "t_us_9",
        )
        .expect_err("sol send should reject EVM-style address");
        assert!(
            err.contains("Solana address"),
            "error should mention Solana: {err}"
        );
    }

    #[test]
    fn parse_unified_send_evm_rejects_bad_address() {
        let err = parse_subcommand(
            &[
                "send",
                "--network",
                "evm",
                "--to",
                "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU",
                "--amount",
                "100",
                "--token",
                "native",
            ],
            "t_us_10",
        )
        .expect_err("evm send should reject base58 address");
        assert!(
            err.contains("EVM address"),
            "error should mention EVM: {err}"
        );
    }

    // ═══════════════════════════════════════════
    // Unified receive tests
    // ═══════════════════════════════════════════

    #[test]
    fn parse_unified_receive_cashu_token_token() {
        let input = parse_subcommand(
            &[
                "receive",
                "--network",
                "cashu",
                "--cashu-token",
                "cashuBxyz123",
            ],
            "t_ur_1",
        )
        .expect("unified cashu receive --cashu-token should parse");
        match input {
            Input::CashuReceive { token, .. } => {
                assert_eq!(token, "cashuBxyz123");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_unified_receive_cashu_invoice() {
        let input = parse_subcommand(
            &["receive", "--network", "cashu", "--amount", "1000"],
            "t_ur_2",
        )
        .expect("unified cashu receive --amount should parse");
        match input {
            Input::Receive {
                network, amount, ..
            } => {
                assert_eq!(network, Some(Network::Cashu));
                assert_eq!(amount.expect("amount").value, 1000);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_unified_receive_ln() {
        let input = parse_subcommand(
            &["receive", "--network", "ln", "--amount", "1000"],
            "t_ur_3",
        )
        .expect("unified ln receive should parse");
        match input {
            Input::Receive {
                network, amount, ..
            } => {
                assert_eq!(network, Some(Network::Ln));
                assert_eq!(amount.expect("amount").value, 1000);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_unified_receive_sol_wait_memo() {
        let input = parse_subcommand(
            &[
                "receive",
                "--network",
                "sol",
                "--wait",
                "--onchain-memo",
                "order:abc",
                "--wait-timeout-s",
                "30",
            ],
            "t_ur_4",
        )
        .expect("unified sol receive --wait --onchain-memo should parse");
        match input {
            Input::Receive {
                network,
                onchain_memo,
                wait_until_paid,
                wait_timeout_s,
                ..
            } => {
                assert_eq!(network, Some(Network::Sol));
                assert_eq!(onchain_memo.as_deref(), Some("order:abc"));
                assert!(wait_until_paid);
                assert_eq!(wait_timeout_s, Some(30));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_unified_receive_sol_wait_amount() {
        let input = parse_subcommand(
            &[
                "receive",
                "--network",
                "sol",
                "--wait",
                "--amount",
                "1000000",
            ],
            "t_ur_5",
        )
        .expect("unified sol receive --wait --amount should parse");
        match input {
            Input::Receive {
                network,
                amount,
                wait_until_paid,
                ..
            } => {
                assert_eq!(network, Some(Network::Sol));
                assert!(wait_until_paid);
                assert_eq!(amount.expect("amount").value, 1000000);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_unified_receive_sol_wait_no_condition() {
        let err = parse_subcommand(&["receive", "--network", "sol", "--wait"], "t_ur_6")
            .expect_err("sol receive --wait without condition should error");
        assert!(
            err.contains("--wait requires"),
            "error should explain --wait requirement: {err}"
        );
    }

    #[test]
    fn parse_unified_receive_sol_min_confirmations() {
        let input = parse_subcommand(
            &[
                "receive",
                "--network",
                "sol",
                "--wait",
                "--onchain-memo",
                "order:abc",
                "--min-confirmations",
                "32",
            ],
            "t_ur_mc1",
        )
        .expect("unified sol receive --wait --min-confirmations should parse");
        match input {
            Input::Receive {
                network,
                min_confirmations,
                wait_until_paid,
                ..
            } => {
                assert_eq!(network, Some(Network::Sol));
                assert!(wait_until_paid);
                assert_eq!(min_confirmations, Some(32));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_unified_receive_evm_min_confirmations() {
        let input = parse_subcommand(
            &[
                "receive",
                "--network",
                "evm",
                "--wait",
                "--amount",
                "1000000",
                "--min-confirmations",
                "12",
            ],
            "t_ur_mc2",
        )
        .expect("unified evm receive --wait --min-confirmations should parse");
        match input {
            Input::Receive {
                network,
                min_confirmations,
                wait_until_paid,
                ..
            } => {
                assert_eq!(network, Some(Network::Evm));
                assert!(wait_until_paid);
                assert_eq!(min_confirmations, Some(12));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_unified_receive_evm_wait_memo_rejected() {
        let err = parse_subcommand(
            &[
                "receive",
                "--network",
                "evm",
                "--wait",
                "--onchain-memo",
                "order:abc",
                "--amount",
                "100",
            ],
            "t_ur_mc2a",
        )
        .expect_err("evm receive --wait should reject onchain memo matching");
        assert!(
            err.contains("not yet supported"),
            "error should mention unsupported onchain memo matching: {err}"
        );
    }

    #[test]
    fn parse_unified_receive_evm_wait_requires_amount() {
        let err = parse_subcommand(&["receive", "--network", "evm", "--wait"], "t_ur_mc2b")
            .expect_err("evm receive --wait should require amount");
        assert!(
            err.contains("requires --amount"),
            "error should mention amount requirement: {err}"
        );
    }

    #[test]
    fn parse_unified_receive_min_confirmations_requires_wait() {
        let err = parse_subcommand(
            &[
                "receive",
                "--network",
                "sol",
                "--onchain-memo",
                "x",
                "--min-confirmations",
                "5",
            ],
            "t_ur_mc3",
        )
        .expect_err("--min-confirmations without --wait should error");
        assert!(
            err.contains("--min-confirmations requires --wait"),
            "error: {err}"
        );
    }

    #[test]
    fn parse_unified_receive_min_confirmations_rejects_cashu() {
        let err = parse_subcommand(
            &[
                "receive",
                "--network",
                "cashu",
                "--wait",
                "--min-confirmations",
                "5",
            ],
            "t_ur_mc4",
        )
        .expect_err("--min-confirmations with cashu should error");
        assert!(
            err.contains("only supported for sol and evm"),
            "error: {err}"
        );
    }

    #[test]
    fn parse_sol_receive_min_confirmations() {
        let input = parse_subcommand(
            &[
                "sol",
                "receive",
                "--wallet",
                "w_12345678",
                "--wait",
                "--onchain-memo",
                "test",
                "--min-confirmations",
                "10",
            ],
            "t_sol_mc1",
        )
        .expect("sol receive --min-confirmations should parse");
        match input {
            Input::Receive {
                min_confirmations, ..
            } => {
                assert_eq!(min_confirmations, Some(10));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_unified_receive_ln_rejects_from() {
        let err = parse_subcommand(
            &[
                "receive",
                "--network",
                "ln",
                "--cashu-token",
                "cashuBxyz",
                "--amount",
                "100",
            ],
            "t_ur_7",
        )
        .expect_err("ln receive should reject --cashu-token");
        assert!(
            err.contains("--cashu-token"),
            "error should mention --cashu-token: {err}"
        );
    }

    // ═══════════════════════════════════════════
    // Existing subcommand validation tests
    // ═══════════════════════════════════════════

    #[test]
    fn parse_sol_send_rejects_evm_address() {
        let err = parse_subcommand(
            &[
                "sol",
                "send",
                "--to",
                "0x1234567890abcdef1234567890abcdef12345678",
                "--amount",
                "100",
                "--token",
                "native",
            ],
            "t_val_1",
        )
        .expect_err("sol send should reject EVM address");
        assert!(
            err.contains("Solana address"),
            "error should mention Solana: {err}"
        );
    }

    #[test]
    fn parse_evm_send_rejects_sol_address() {
        let err = parse_subcommand(
            &[
                "evm",
                "send",
                "--to",
                "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU",
                "--amount",
                "100",
                "--token",
                "native",
            ],
            "t_val_2",
        )
        .expect_err("evm send should reject Solana address");
        assert!(
            err.contains("EVM address"),
            "error should mention EVM: {err}"
        );
    }

    #[test]
    fn parse_ln_send_rejects_bad_invoice() {
        let err = parse_subcommand(
            &[
                "ln",
                "send",
                "--to",
                "0x1234567890abcdef1234567890abcdef12345678",
            ],
            "t_val_3",
        )
        .expect_err("ln send should reject non-bolt11");
        assert!(
            err.contains("Lightning invoice"),
            "error should mention Lightning: {err}"
        );
    }

    #[test]
    fn parse_sol_send_rejects_contract_token() {
        let err = parse_subcommand(
            &[
                "sol",
                "send",
                "--to",
                "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU",
                "--amount",
                "100",
                "--token",
                "0xabcdef1234567890abcdef1234567890abcdef12",
            ],
            "t_val_4",
        )
        .expect_err("sol send should reject contract address as --token");
        assert!(
            err.contains("token-add"),
            "error should hint token-add: {err}"
        );
    }

    // ═══════════════════════════════════════════
    // Receive --ln-quote-id (claim via receive)
    // ═══════════════════════════════════════════

    #[test]
    fn parse_unified_receive_cashu_claim() {
        let input = parse_subcommand(
            &[
                "receive",
                "--network",
                "cashu",
                "--wallet",
                "w_abc",
                "--ln-quote-id",
                "ph_123",
            ],
            "t_claim_1",
        )
        .expect("receive --ln-quote-id should parse as claim");
        match input {
            Input::ReceiveClaim {
                wallet, quote_id, ..
            } => {
                assert_eq!(wallet, "w_abc");
                assert_eq!(quote_id, "ph_123");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_unified_receive_ln_claim() {
        let input = parse_subcommand(
            &[
                "receive",
                "--network",
                "ln",
                "--wallet",
                "w_ln1",
                "--ln-quote-id",
                "ph_789",
            ],
            "t_claim_2",
        )
        .expect("receive --network ln --ln-quote-id should parse as claim");
        match input {
            Input::ReceiveClaim {
                wallet, quote_id, ..
            } => {
                assert_eq!(wallet, "w_ln1");
                assert_eq!(quote_id, "ph_789");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_unified_receive_sol_rejects_ln_quote_id() {
        let err = parse_subcommand(
            &[
                "receive",
                "--network",
                "sol",
                "--wallet",
                "w_sol1",
                "--ln-quote-id",
                "ph_bad",
            ],
            "t_claim_3",
        )
        .expect_err("sol receive should reject --ln-quote-id");
        assert!(
            err.contains("--ln-quote-id"),
            "error should mention --ln-quote-id: {err}"
        );
    }

    #[test]
    fn parse_unified_receive_claim_requires_wallet() {
        let err = parse_subcommand(
            &["receive", "--network", "cashu", "--ln-quote-id", "ph_123"],
            "t_claim_4",
        )
        .expect_err("claim via receive should require --wallet");
        assert!(
            err.contains("--wallet"),
            "error should mention --wallet: {err}"
        );
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

    // ═══════════════════════════════════════════
    // Unified wallet create
    // ═══════════════════════════════════════════

    #[test]
    fn parse_wallet_create_cashu() {
        let input = parse_subcommand(
            &[
                "wallet",
                "create",
                "--network",
                "cashu",
                "--cashu-mint",
                "https://mint.example",
            ],
            "t_wc_1",
        )
        .expect("wallet create --network cashu should parse");
        match input {
            Input::WalletCreate {
                network, mint_url, ..
            } => {
                assert_eq!(network, Network::Cashu);
                assert_eq!(mint_url.as_deref(), Some("https://mint.example"));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_wallet_create_cashu_requires_mint_url() {
        let err = parse_subcommand(&["wallet", "create", "--network", "cashu"], "t_wc_2")
            .expect_err("cashu wallet create should require --cashu-mint");
        assert!(
            err.contains("--cashu-mint"),
            "error should mention --cashu-mint: {err}"
        );
    }

    #[test]
    fn parse_wallet_create_sol() {
        let input = parse_subcommand(
            &[
                "wallet",
                "create",
                "--network",
                "sol",
                "--sol-rpc-endpoint",
                "https://rpc-a.example",
                "--sol-rpc-endpoint",
                "https://rpc-b.example",
            ],
            "t_wc_3",
        )
        .expect("wallet create --network sol should parse");
        match input {
            Input::WalletCreate {
                network,
                rpc_endpoints,
                ..
            } => {
                assert_eq!(network, Network::Sol);
                assert_eq!(
                    rpc_endpoints,
                    vec!["https://rpc-a.example", "https://rpc-b.example"]
                );
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_wallet_create_sol_requires_rpc() {
        let err = parse_subcommand(&["wallet", "create", "--network", "sol"], "t_wc_4")
            .expect_err("sol wallet create should require --sol-rpc-endpoint");
        assert!(
            err.contains("--sol-rpc-endpoint"),
            "error should mention --sol-rpc-endpoint: {err}"
        );
    }

    #[test]
    fn parse_wallet_create_evm() {
        let input = parse_subcommand(
            &[
                "wallet",
                "create",
                "--network",
                "evm",
                "--evm-rpc-endpoint",
                "https://rpc.example",
                "--chain-id",
                "42161",
            ],
            "t_wc_5",
        )
        .expect("wallet create --network evm should parse");
        match input {
            Input::WalletCreate {
                network,
                rpc_endpoints,
                chain_id,
                ..
            } => {
                assert_eq!(network, Network::Evm);
                assert_eq!(rpc_endpoints, vec!["https://rpc.example"]);
                assert_eq!(chain_id, Some(42161));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_wallet_create_evm_default_chain_id() {
        let input = parse_subcommand(
            &[
                "wallet",
                "create",
                "--network",
                "evm",
                "--evm-rpc-endpoint",
                "https://rpc.example",
            ],
            "t_wc_6",
        )
        .expect("evm wallet create should default chain_id to 8453");
        match input {
            Input::WalletCreate { chain_id, .. } => {
                assert_eq!(chain_id, Some(8453));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_wallet_create_ln_nwc() {
        let input = parse_subcommand(
            &[
                "wallet",
                "create",
                "--network",
                "ln",
                "--backend",
                "nwc",
                "--nwc-uri-secret",
                "nostr+walletconnect://...",
            ],
            "t_wc_7",
        )
        .expect("wallet create --network ln --backend nwc should parse");
        match input {
            Input::LnWalletCreate { request, .. } => {
                assert_eq!(request.backend, LnWalletBackend::Nwc);
                assert!(request.nwc_uri_secret.is_some());
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_wallet_create_ln_requires_backend() {
        let err = parse_subcommand(&["wallet", "create", "--network", "ln"], "t_wc_8")
            .expect_err("ln wallet create should require --backend");
        assert!(
            err.contains("--backend"),
            "error should mention --backend: {err}"
        );
    }

    // ═══════════════════════════════════════════
    // Unified wallet dangerously-show-seed / restore
    // ═══════════════════════════════════════════

    #[test]
    fn parse_wallet_dangerously_show_seed_top_level() {
        let input = parse_subcommand(
            &["wallet", "dangerously-show-seed", "--wallet", "w_abc"],
            "t_ws_1",
        )
        .expect("wallet dangerously-show-seed should parse");
        match input {
            Input::WalletShowSeed { wallet, .. } => {
                assert_eq!(wallet, "w_abc");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn parse_wallet_cashu_restore_top_level() {
        let input = parse_subcommand(
            &["wallet", "cashu-restore", "--wallet", "w_cashu1"],
            "t_wr_1",
        )
        .expect("wallet cashu-restore should parse");
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

    // ── --network optional when --wallet is given ──

    #[test]
    fn send_wallet_without_network_builds_withdraw() {
        let input = parse_subcommand(
            &[
                "send",
                "--wallet",
                "w_abc",
                "--to",
                "lnbc1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx",
            ],
            "t_no_net_1",
        )
        .expect("send --wallet without --network should parse");
        match input {
            Input::Send {
                wallet,
                network,
                to,
                ..
            } => {
                assert_eq!(wallet.as_deref(), Some("w_abc"));
                assert!(
                    network.is_none(),
                    "network should be None for handler to infer"
                );
                assert!(to.starts_with("lnbc"));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn send_wallet_without_network_p2p_builds_send() {
        let input = parse_subcommand(
            &["send", "--wallet", "w_abc", "--amount", "100"],
            "t_no_net_2",
        )
        .expect("send --wallet --amount without --network should parse as P2P send");
        match input {
            Input::CashuSend { wallet, amount, .. } => {
                assert_eq!(wallet.as_deref(), Some("w_abc"));
                assert_eq!(amount.value, 100);
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn send_no_wallet_no_network_errors() {
        let err = parse_subcommand(
            &["send", "--to", "lnbc1xxx", "--amount", "100"],
            "t_no_net_3",
        )
        .expect_err("send without --wallet and --network should error");
        assert!(err.contains("--network is required"), "error: {err}");
    }

    #[test]
    fn receive_wallet_without_network_builds_deposit() {
        let input = parse_subcommand(
            &["receive", "--wallet", "w_abc", "--amount", "500"],
            "t_no_net_4",
        )
        .expect("receive --wallet without --network should parse");
        match input {
            Input::Receive {
                wallet,
                network,
                amount,
                ..
            } => {
                assert_eq!(wallet, "w_abc");
                assert!(
                    network.is_none(),
                    "network should be None for handler to infer"
                );
                assert_eq!(amount.as_ref().map(|a| a.value), Some(500));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn receive_from_without_network_builds_receive() {
        let input = parse_subcommand(&["receive", "--cashu-token", "cashuBo2Fxyz"], "t_no_net_5")
            .expect("receive --cashu-token without --network should parse");
        match input {
            Input::CashuReceive { wallet, token, .. } => {
                assert!(wallet.is_none());
                assert_eq!(token, "cashuBo2Fxyz");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn receive_no_wallet_no_network_no_from_errors() {
        let err = parse_subcommand(&["receive", "--amount", "100"], "t_no_net_6")
            .expect_err("receive without --wallet, --network, --cashu-token should error");
        assert!(err.contains("--network is required"), "error: {err}");
    }

    #[test]
    fn receive_ln_quote_id_without_network_works() {
        let input = parse_subcommand(
            &["receive", "--wallet", "w_abc", "--ln-quote-id", "q_123"],
            "t_no_net_7",
        )
        .expect("receive --wallet --ln-quote-id without --network should parse");
        match input {
            Input::ReceiveClaim {
                wallet, quote_id, ..
            } => {
                assert_eq!(wallet, "w_abc");
                assert_eq!(quote_id, "q_123");
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    // ═══════════════════════════════════════════
    // Unsupported flag rejection tests
    // ═══════════════════════════════════════════

    // --onchain-memo rejected for cashu/ln send
    #[test]
    fn send_cashu_rejects_onchain_memo() {
        let err = parse_subcommand(
            &[
                "send",
                "--network",
                "cashu",
                "--amount",
                "100",
                "--onchain-memo",
                "test",
            ],
            "t_rej_1",
        )
        .expect_err("cashu send should reject --onchain-memo");
        assert!(err.contains("--onchain-memo"), "error: {err}");
    }

    #[test]
    fn send_ln_rejects_onchain_memo() {
        let err = parse_subcommand(
            &[
                "send",
                "--network",
                "ln",
                "--to",
                "lnbc1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx",
                "--onchain-memo",
                "test",
            ],
            "t_rej_2",
        )
        .expect_err("ln send should reject --onchain-memo");
        assert!(err.contains("--onchain-memo"), "error: {err}");
    }

    // --onchain-memo rejected for cashu/ln receive
    #[test]
    fn receive_cashu_rejects_onchain_memo() {
        let err = parse_subcommand(
            &[
                "receive",
                "--network",
                "cashu",
                "--amount",
                "100",
                "--onchain-memo",
                "test",
            ],
            "t_rej_3",
        )
        .expect_err("cashu receive should reject --onchain-memo");
        assert!(err.contains("--onchain-memo"), "error: {err}");
    }

    #[test]
    fn receive_ln_rejects_onchain_memo() {
        let err = parse_subcommand(
            &[
                "receive",
                "--network",
                "ln",
                "--amount",
                "100",
                "--onchain-memo",
                "test",
            ],
            "t_rej_4",
        )
        .expect_err("ln receive should reject --onchain-memo");
        assert!(err.contains("--onchain-memo"), "error: {err}");
    }

    // --onchain-memo accepted for sol/evm send
    #[test]
    fn send_sol_accepts_onchain_memo() {
        let input = parse_subcommand(
            &[
                "send",
                "--network",
                "sol",
                "--to",
                "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU",
                "--amount",
                "1000",
                "--token",
                "native",
                "--onchain-memo",
                "payment",
            ],
            "t_rej_5",
        )
        .expect("sol send should accept --onchain-memo");
        match input {
            Input::Send { onchain_memo, .. } => {
                assert_eq!(onchain_memo.as_deref(), Some("payment"));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    #[test]
    fn send_evm_accepts_onchain_memo() {
        let input = parse_subcommand(
            &[
                "send",
                "--network",
                "evm",
                "--to",
                "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045",
                "--amount",
                "1000",
                "--token",
                "native",
                "--onchain-memo",
                "payment",
            ],
            "t_rej_6",
        )
        .expect("evm send should accept --onchain-memo");
        match input {
            Input::Send { onchain_memo, .. } => {
                assert_eq!(onchain_memo.as_deref(), Some("payment"));
            }
            other => panic!("unexpected input: {other:?}"),
        }
    }

    // --cashu-mint rejected for non-cashu send
    #[test]
    fn send_ln_rejects_cashu_mint() {
        let err = parse_subcommand(
            &[
                "send",
                "--network",
                "ln",
                "--to",
                "lnbc1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx",
                "--cashu-mint",
                "https://mint.example.com",
            ],
            "t_rej_7",
        )
        .expect_err("ln send should reject --cashu-mint");
        assert!(err.contains("--cashu-mint"), "error: {err}");
    }

    #[test]
    fn send_sol_rejects_cashu_mint() {
        let err = parse_subcommand(
            &[
                "send",
                "--network",
                "sol",
                "--to",
                "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU",
                "--amount",
                "1000",
                "--token",
                "native",
                "--cashu-mint",
                "https://mint.example.com",
            ],
            "t_rej_8",
        )
        .expect_err("sol send should reject --cashu-mint");
        assert!(err.contains("--cashu-mint"), "error: {err}");
    }

    #[test]
    fn send_evm_rejects_cashu_mint() {
        let err = parse_subcommand(
            &[
                "send",
                "--network",
                "evm",
                "--to",
                "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045",
                "--amount",
                "1000",
                "--token",
                "native",
                "--cashu-mint",
                "https://mint.example.com",
            ],
            "t_rej_9",
        )
        .expect_err("evm send should reject --cashu-mint");
        assert!(err.contains("--cashu-mint"), "error: {err}");
    }

    // --cashu-token rejected for non-cashu receive
    #[test]
    fn receive_sol_rejects_from() {
        let err = parse_subcommand(
            &["receive", "--network", "sol", "--cashu-token", "cashuBxyz"],
            "t_rej_10",
        )
        .expect_err("sol receive should reject --cashu-token");
        assert!(err.contains("--cashu-token"), "error: {err}");
    }

    #[test]
    fn receive_evm_rejects_from() {
        let err = parse_subcommand(
            &["receive", "--network", "evm", "--cashu-token", "cashuBxyz"],
            "t_rej_11",
        )
        .expect_err("evm receive should reject --cashu-token");
        assert!(err.contains("--cashu-token"), "error: {err}");
    }

    // --token rejected for cashu/ln send and receive
    #[test]
    fn receive_cashu_rejects_token() {
        let err = parse_subcommand(
            &[
                "receive",
                "--network",
                "cashu",
                "--amount",
                "100",
                "--token",
                "sats",
            ],
            "t_rej_12",
        )
        .expect_err("cashu receive should reject --token");
        assert!(err.contains("--token"), "error: {err}");
    }

    #[test]
    fn receive_ln_rejects_token() {
        let err = parse_subcommand(
            &[
                "receive",
                "--network",
                "ln",
                "--amount",
                "100",
                "--token",
                "sats",
            ],
            "t_rej_13",
        )
        .expect_err("ln receive should reject --token");
        assert!(err.contains("--token"), "error: {err}");
    }

    // hidden cashu subcommand also rejects --onchain-memo
    #[test]
    fn cashu_send_subcommand_rejects_onchain_memo() {
        let err = parse_subcommand(
            &["cashu", "send", "--amount", "100", "--onchain-memo", "test"],
            "t_rej_14",
        )
        .expect_err("cashu send subcommand should reject --onchain-memo");
        assert!(err.contains("--onchain-memo"), "error: {err}");
    }

    #[test]
    fn cashu_send_to_ln_subcommand_rejects_onchain_memo() {
        let err = parse_subcommand(
            &[
                "cashu",
                "send-to-ln",
                "--to",
                "lnbc1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx",
                "--onchain-memo",
                "test",
            ],
            "t_rej_15",
        )
        .expect_err("cashu send-to-ln subcommand should reject --onchain-memo");
        assert!(err.contains("--onchain-memo"), "error: {err}");
    }

    #[test]
    fn ln_send_subcommand_rejects_onchain_memo() {
        let err = parse_subcommand(
            &[
                "ln",
                "send",
                "--to",
                "lnbc1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx",
                "--onchain-memo",
                "test",
            ],
            "t_rej_16",
        )
        .expect_err("ln send subcommand should reject --onchain-memo");
        assert!(err.contains("--onchain-memo"), "error: {err}");
    }

    // --ln-quote-id rejected for sol/evm
    #[test]
    fn receive_evm_rejects_ln_quote_id() {
        let err = parse_subcommand(
            &["receive", "--network", "evm", "--ln-quote-id", "q_123"],
            "t_rej_17",
        )
        .expect_err("evm receive should reject --ln-quote-id");
        assert!(err.contains("--ln-quote-id"), "error: {err}");
    }

    // --wait-timeout-s without --wait
    #[test]
    fn receive_rejects_wait_timeout_without_wait() {
        let err = parse_subcommand(
            &["receive", "--network", "sol", "--wait-timeout-s", "30"],
            "t_rej_18",
        )
        .expect_err("--wait-timeout-s without --wait should error");
        assert!(err.contains("--wait"), "error: {err}");
    }
}
