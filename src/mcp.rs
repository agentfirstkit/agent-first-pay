use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use crate::config::VERSION;
use crate::handler::{self, App};
use crate::types::*;
use agent_first_data::RedactionPolicy;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn mcp_id() -> String {
    format!(
        "mcp_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    )
}

async fn drain_outputs(_app: &App, rx: &Mutex<mpsc::Receiver<Output>>) -> CallToolResult {
    // Drop sender refcount so rx sees closure after dispatch finishes.
    // We don't drop the sender here because app owns it; instead we drain what's available.
    // The handler sends all outputs synchronously before returning from dispatch,
    // so we can drain with try_recv after dispatch completes.
    let mut outputs = vec![];
    let mut guard = rx.lock().await;
    while let Ok(msg) = guard.try_recv() {
        let value = serde_json::to_value(&msg).unwrap_or(Value::Null);
        let rendered = crate::output_fmt::render_value_with_policy(
            &value,
            agent_first_data::OutputFormat::Json,
        );
        let normalized = serde_json::from_str::<Value>(&rendered).unwrap_or(Value::Null);
        outputs.push(normalized);
    }
    drop(guard);

    let result = json!({"events": outputs});
    // Per-event redaction policy has already been applied above.
    let text = agent_first_data::output_json_with(&result, RedactionPolicy::RedactionNone);
    CallToolResult::success(vec![Content::text(text)])
}

// ---------------------------------------------------------------------------
// Parameter structs
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
struct CashuWalletCreateParams {
    /// Cashu mint URL
    mint_url: String,
    /// Optional wallet label
    #[serde(default)]
    label: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct CashuWalletListParams {}

#[derive(Deserialize, JsonSchema)]
struct CashuWalletCloseParams {
    /// Wallet ID to close
    wallet: String,
}

#[derive(Deserialize, JsonSchema)]
struct CashuBalanceParams {
    /// Wallet ID (omit for all cashu wallets)
    #[serde(default)]
    wallet: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct CashuReceiveFromLnParams {
    /// Wallet ID
    wallet: String,
    /// Amount in sats
    amount_sats: u64,
}

#[derive(Deserialize, JsonSchema)]
struct CashuReceiveFromLnClaimParams {
    /// Wallet ID
    wallet: String,
    /// Mint quote ID to claim
    quote_id: String,
}

#[derive(Deserialize, JsonSchema)]
struct CashuSendParams {
    /// Wallet ID (auto-selected if omitted)
    #[serde(default)]
    wallet: Option<String>,
    /// Amount in sats
    amount_sats: u64,
    /// Memo sent with the transaction (immutable)
    #[serde(default)]
    onchain_memo: Option<String>,
    /// Local bookkeeping annotations (key-value pairs, editable)
    #[serde(default)]
    local_memo: Option<BTreeMap<String, String>>,
    /// Restrict to wallets on these mints (tried in order)
    #[serde(default)]
    mints: Option<Vec<String>>,
}

#[derive(Deserialize, JsonSchema)]
struct CashuReceiveParams {
    /// Wallet ID (auto-matched from token mint_url if omitted)
    #[serde(default)]
    wallet: Option<String>,
    /// Cashu token string
    token: String,
}

#[derive(Deserialize, JsonSchema)]
struct CashuSendToLnParams {
    /// Wallet ID (auto-selected if omitted)
    #[serde(default)]
    wallet: Option<String>,
    /// Lightning invoice (bolt11)
    to: String,
    /// Memo sent with the transaction (immutable)
    #[serde(default)]
    onchain_memo: Option<String>,
    /// Local bookkeeping annotations (key-value pairs, editable)
    #[serde(default)]
    local_memo: Option<BTreeMap<String, String>>,
}

#[derive(Deserialize, JsonSchema)]
struct WalletCreateParams {
    /// Network: cashu, ln, sol, evm, or btc
    network: Network,
    /// Optional wallet label
    #[serde(default)]
    label: Option<String>,
    /// Cashu mint URL (cashu only)
    #[serde(default)]
    mint_url: Option<String>,
    /// RPC endpoints (sol/evm)
    #[serde(default)]
    rpc_endpoints: Vec<String>,
    /// EVM chain ID (evm only)
    #[serde(default)]
    chain_id: Option<u64>,
    /// BIP-39 mnemonic (import existing key)
    #[serde(default)]
    mnemonic_secret: Option<String>,
    /// Esplora API URL (btc only)
    #[serde(default)]
    btc_esplora_url: Option<String>,
    /// BTC sub-network: "mainnet" or "signet" (btc only, default: mainnet)
    #[serde(default)]
    btc_network: Option<String>,
    /// BTC address type: "taproot" or "segwit" (btc only, default: taproot)
    #[serde(default)]
    btc_address_type: Option<String>,
    /// BTC chain-source backend: esplora (default), core-rpc, electrum (btc only)
    #[serde(default)]
    btc_backend: Option<BtcBackend>,
    /// Bitcoin Core RPC URL (btc core-rpc backend only)
    #[serde(default)]
    btc_core_url: Option<String>,
    /// Bitcoin Core RPC auth "user:pass" (btc core-rpc backend only)
    #[serde(default)]
    btc_core_auth_secret: Option<String>,
    /// Electrum server URL (btc electrum backend only)
    #[serde(default)]
    btc_electrum_url: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct LnWalletCreateParams {
    /// Backend: nwc, phoenixd, or lnbits
    backend: LnWalletBackend,
    /// Optional wallet label
    #[serde(default)]
    label: Option<String>,
    /// NWC connection URI (nwc backend)
    #[serde(default)]
    nwc_uri_secret: Option<String>,
    /// API endpoint URL (phoenixd/lnbits backend)
    #[serde(default)]
    endpoint: Option<String>,
    /// Password (phoenixd backend)
    #[serde(default)]
    password_secret: Option<String>,
    /// Admin key (lnbits backend)
    #[serde(default)]
    admin_key_secret: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct WalletListParams {
    /// Filter by network
    #[serde(default)]
    network: Option<Network>,
}

#[derive(Deserialize, JsonSchema)]
struct WalletCloseParams {
    /// Wallet ID to close
    wallet: String,
    /// Skip balance check (may lose funds!)
    #[serde(default)]
    dangerously_skip: bool,
}

#[derive(Deserialize, JsonSchema)]
struct BalanceParams {
    /// Wallet ID (omit for all wallets)
    #[serde(default)]
    wallet: Option<String>,
    /// Filter by network
    #[serde(default)]
    network: Option<Network>,
    /// Force on-chain balance check
    #[serde(default)]
    check: bool,
}

#[derive(Deserialize, JsonSchema)]
struct SendParams {
    /// Wallet ID (auto-selected if omitted)
    #[serde(default)]
    wallet: Option<String>,
    /// Network filter
    #[serde(default)]
    network: Option<Network>,
    /// Destination: address, invoice, or Lightning address
    to: String,
    /// Memo sent with the transaction (immutable)
    #[serde(default)]
    onchain_memo: Option<String>,
    /// Local bookkeeping annotations (key-value pairs, editable)
    #[serde(default)]
    local_memo: Option<BTreeMap<String, String>>,
    /// Restrict to wallets on these mints (cashu only)
    #[serde(default)]
    mints: Option<Vec<String>>,
}

#[derive(Deserialize, JsonSchema)]
struct ReceiveParams {
    /// Wallet ID
    wallet: String,
    /// Network filter
    #[serde(default)]
    network: Option<Network>,
    /// Amount to receive (value + token)
    #[serde(default)]
    amount: Option<Amount>,
    /// Memo sent with the transaction (immutable)
    #[serde(default)]
    onchain_memo: Option<String>,
    /// Block until payment arrives
    #[serde(default)]
    wait_until_paid: bool,
    /// Timeout for wait_until_paid (seconds)
    #[serde(default)]
    wait_timeout_s: Option<u64>,
    /// Poll interval for wait_until_paid (milliseconds)
    #[serde(default)]
    wait_poll_interval_ms: Option<u64>,
    /// Max records scanned per poll when resolving tx id during wait (evm/btc)
    #[serde(default)]
    wait_sync_limit: Option<usize>,
    /// Minimum confirmations before considering paid
    #[serde(default)]
    min_confirmations: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
struct ReceiveClaimParams {
    /// Wallet ID
    wallet: String,
    /// Mint quote ID to claim
    quote_id: String,
}

#[derive(Deserialize, JsonSchema)]
struct WalletShowSeedParams {
    /// Wallet ID
    wallet: String,
}

#[derive(Deserialize, JsonSchema)]
struct WalletRestoreParams {
    /// Wallet ID
    wallet: String,
}

#[derive(Deserialize, JsonSchema)]
struct HistoryListParams {
    /// Filter by wallet ID
    #[serde(default)]
    wallet: Option<String>,
    /// Filter by network
    #[serde(default)]
    network: Option<Network>,
    /// Filter by onchain memo (substring match)
    #[serde(default)]
    onchain_memo: Option<String>,
    /// Max records to return
    #[serde(default)]
    limit: Option<usize>,
    /// Skip N records
    #[serde(default)]
    offset: Option<usize>,
    /// Only include records created at or after this epoch second
    #[serde(default)]
    since_epoch_s: Option<u64>,
    /// Only include records created before this epoch second
    #[serde(default)]
    until_epoch_s: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct HistoryStatusParams {
    /// Transaction ID
    transaction_id: String,
}

#[derive(Deserialize, JsonSchema)]
struct HistoryUpdateParams {
    /// Sync a specific wallet (defaults to all wallets)
    #[serde(default)]
    wallet: Option<String>,
    /// Restrict sync to a single network
    #[serde(default)]
    network: Option<Network>,
    /// Max records to scan per wallet during sync
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
struct LimitAddParams {
    /// Scope: all, network, or wallet
    scope: SpendScope,
    /// Network name (required for network/wallet scope)
    #[serde(default)]
    network: Option<String>,
    /// Wallet ID (required for wallet scope)
    #[serde(default)]
    wallet: Option<String>,
    /// Time window in seconds
    window_s: u64,
    /// Maximum spend in the window
    max_spend: u64,
    /// Token symbol (omit for native unit)
    #[serde(default)]
    token: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct LimitRemoveParams {
    /// Rule ID to remove
    rule_id: String,
}

#[derive(Deserialize, JsonSchema)]
struct LimitListParams {}

#[derive(Deserialize, JsonSchema)]
struct LimitSetParams {
    /// Complete set of spend limits (replaces all existing)
    limits: Vec<SpendLimit>,
}

#[derive(Deserialize, JsonSchema)]
struct WalletConfigShowParams {
    /// Wallet ID or label
    wallet: String,
}

#[derive(Deserialize, JsonSchema)]
struct WalletConfigSetParams {
    /// Wallet ID
    wallet: String,
    /// New label
    #[serde(default)]
    label: Option<String>,
    /// New RPC endpoints
    #[serde(default)]
    rpc_endpoints: Vec<String>,
    /// New chain ID
    #[serde(default)]
    chain_id: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct WalletConfigTokenAddParams {
    /// Wallet ID
    wallet: String,
    /// Token symbol
    symbol: String,
    /// Token contract address
    address: String,
    /// Token decimals (default 6)
    #[serde(default = "default_decimals")]
    decimals: u8,
}

fn default_decimals() -> u8 {
    6
}

#[derive(Deserialize, JsonSchema)]
struct WalletConfigTokenRemoveParams {
    /// Wallet ID
    wallet: String,
    /// Token symbol
    symbol: String,
}

#[derive(Deserialize, JsonSchema)]
struct PayConfigParams {
    /// Replace spend limits
    #[serde(default)]
    limits: Option<Vec<SpendLimit>>,
    /// Log filter categories
    #[serde(default)]
    log: Option<Vec<String>>,
}

#[derive(Deserialize, JsonSchema)]
struct VersionParams {}

// ---------------------------------------------------------------------------
// MCP server struct
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AfpayMcp {
    app: Arc<App>,
    rx: Arc<Mutex<mpsc::Receiver<Output>>>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl AfpayMcp {
    pub fn new(app: Arc<App>, rx: mpsc::Receiver<Output>) -> Self {
        Self {
            app,
            rx: Arc::new(Mutex::new(rx)),
            tool_router: Self::tool_router(),
        }
    }

    // ── Cashu convenience tools ──────────────────────────────────────────

    /// Create a new Cashu wallet
    #[tool(description = "Create a new Cashu wallet")]
    async fn cashu_wallet_create(
        &self,
        params: Parameters<CashuWalletCreateParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::WalletCreate {
            id: mcp_id(),
            network: Network::Cashu,
            label: p.label,
            mint_url: Some(p.mint_url),
            rpc_endpoints: vec![],
            chain_id: None,
            mnemonic_secret: None,
            btc_esplora_url: None,
            btc_network: None,
            btc_address_type: None,
            btc_backend: None,
            btc_core_url: None,
            btc_core_auth_secret: None,
            btc_electrum_url: None,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// List cashu wallets
    #[tool(description = "List Cashu wallets")]
    async fn cashu_wallet_list(
        &self,
        _params: Parameters<CashuWalletListParams>,
    ) -> Result<CallToolResult, McpError> {
        let input = Input::WalletList {
            id: mcp_id(),
            network: Some(Network::Cashu),
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Close a zero-balance cashu wallet
    #[tool(description = "Close a zero-balance Cashu wallet")]
    async fn cashu_wallet_close(
        &self,
        params: Parameters<CashuWalletCloseParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::WalletClose {
            id: mcp_id(),
            wallet: p.wallet,
            dangerously_skip_balance_check_and_may_lose_money: false,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Check cashu wallet balance
    #[tool(description = "Check Cashu wallet balance")]
    async fn cashu_balance(
        &self,
        params: Parameters<CashuBalanceParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::Balance {
            id: mcp_id(),
            wallet: p.wallet,
            network: Some(Network::Cashu),
            check: false,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Get Lightning invoice to receive into cashu wallet
    #[tool(description = "Get Lightning invoice to receive into Cashu wallet")]
    async fn cashu_receive_from_ln(
        &self,
        params: Parameters<CashuReceiveFromLnParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::Receive {
            id: mcp_id(),
            wallet: p.wallet,
            network: Some(Network::Cashu),
            amount: Some(Amount {
                value: p.amount_sats,
                token: "sats".to_string(),
            }),
            onchain_memo: None,
            wait_until_paid: false,
            wait_timeout_s: None,
            wait_poll_interval_ms: None,
            wait_sync_limit: None,
            write_qr_svg_file: false,
            min_confirmations: None,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Claim minted tokens from receive-from-ln quote
    #[tool(description = "Claim minted tokens from receive-from-ln quote")]
    async fn cashu_receive_from_ln_claim(
        &self,
        params: Parameters<CashuReceiveFromLnClaimParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::ReceiveClaim {
            id: mcp_id(),
            wallet: p.wallet,
            quote_id: p.quote_id,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Send P2P cashu token (wallet auto-selected if omitted)
    #[tool(description = "Send P2P Cashu token (wallet auto-selected if omitted)")]
    async fn cashu_send(
        &self,
        params: Parameters<CashuSendParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::CashuSend {
            id: mcp_id(),
            wallet: p.wallet,
            amount: Amount {
                value: p.amount_sats,
                token: "sats".to_string(),
            },
            onchain_memo: p.onchain_memo,
            local_memo: p.local_memo,
            mints: p.mints,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Receive a cashu token into a wallet
    #[tool(
        description = "Receive a Cashu token into a wallet (wallet auto-matched from token if omitted)"
    )]
    async fn cashu_receive(
        &self,
        params: Parameters<CashuReceiveParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::CashuReceive {
            id: mcp_id(),
            wallet: p.wallet,
            token: p.token,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Send cashu to Lightning invoice
    #[tool(description = "Send Cashu to Lightning invoice (wallet auto-selected if omitted)")]
    async fn cashu_send_to_ln(
        &self,
        params: Parameters<CashuSendToLnParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::Send {
            id: mcp_id(),
            wallet: p.wallet,
            network: Some(Network::Cashu),
            to: p.to,
            onchain_memo: p.onchain_memo,
            local_memo: p.local_memo,
            mints: None,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    // ── Generic (cross-network) tools ────────────────────────────────────

    /// Create a wallet for any network (cashu, sol, evm)
    #[tool(description = "Create a wallet for any network (cashu, sol, evm, btc)")]
    async fn wallet_create(
        &self,
        params: Parameters<WalletCreateParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::WalletCreate {
            id: mcp_id(),
            network: p.network,
            label: p.label,
            mint_url: p.mint_url,
            rpc_endpoints: p.rpc_endpoints,
            chain_id: p.chain_id,
            mnemonic_secret: p.mnemonic_secret,
            btc_esplora_url: p.btc_esplora_url,
            btc_network: p.btc_network,
            btc_address_type: p.btc_address_type,
            btc_backend: p.btc_backend,
            btc_core_url: p.btc_core_url,
            btc_core_auth_secret: p.btc_core_auth_secret,
            btc_electrum_url: p.btc_electrum_url,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Create a Lightning wallet (nwc, phoenixd, or lnbits)
    #[tool(description = "Create a Lightning wallet (nwc, phoenixd, or lnbits)")]
    async fn ln_wallet_create(
        &self,
        params: Parameters<LnWalletCreateParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::LnWalletCreate {
            id: mcp_id(),
            request: LnWalletCreateRequest {
                backend: p.backend,
                label: p.label,
                nwc_uri_secret: p.nwc_uri_secret,
                endpoint: p.endpoint,
                password_secret: p.password_secret,
                admin_key_secret: p.admin_key_secret,
            },
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// List all wallets (cross-network)
    #[tool(description = "List all wallets (cross-network)")]
    async fn wallet_list(
        &self,
        params: Parameters<WalletListParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::WalletList {
            id: mcp_id(),
            network: p.network,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Close a wallet
    #[tool(description = "Close a wallet (must have zero balance unless dangerously_skip is set)")]
    async fn wallet_close(
        &self,
        params: Parameters<WalletCloseParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::WalletClose {
            id: mcp_id(),
            wallet: p.wallet,
            dangerously_skip_balance_check_and_may_lose_money: p.dangerously_skip,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Check wallet balance (cross-network)
    #[tool(description = "Check wallet balance (cross-network)")]
    async fn balance(&self, params: Parameters<BalanceParams>) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::Balance {
            id: mcp_id(),
            wallet: p.wallet,
            network: p.network,
            check: p.check,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Send payment (cross-network)
    #[tool(description = "Send payment to address, invoice, or Lightning address (cross-network)")]
    async fn send(&self, params: Parameters<SendParams>) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::Send {
            id: mcp_id(),
            wallet: p.wallet,
            network: p.network,
            to: p.to,
            onchain_memo: p.onchain_memo,
            local_memo: p.local_memo,
            mints: p.mints,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Receive payment (get address or invoice)
    #[tool(description = "Receive payment — returns address or invoice depending on network")]
    async fn receive(&self, params: Parameters<ReceiveParams>) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::Receive {
            id: mcp_id(),
            wallet: p.wallet,
            network: p.network,
            amount: p.amount,
            onchain_memo: p.onchain_memo,
            wait_until_paid: p.wait_until_paid,
            wait_timeout_s: p.wait_timeout_s,
            wait_poll_interval_ms: p.wait_poll_interval_ms,
            wait_sync_limit: p.wait_sync_limit,
            write_qr_svg_file: false,
            min_confirmations: p.min_confirmations,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Claim minted tokens from a receive quote
    #[tool(description = "Claim minted tokens from a receive quote")]
    async fn receive_claim(
        &self,
        params: Parameters<ReceiveClaimParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::ReceiveClaim {
            id: mcp_id(),
            wallet: p.wallet,
            quote_id: p.quote_id,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Show wallet seed phrase (mnemonic)
    #[tool(description = "Show wallet seed phrase (mnemonic) — handle with care")]
    async fn wallet_show_seed(
        &self,
        params: Parameters<WalletShowSeedParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::WalletShowSeed {
            id: mcp_id(),
            wallet: p.wallet,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Restore wallet proofs from mint
    #[tool(description = "Restore wallet proofs from mint (Cashu only)")]
    async fn wallet_restore(
        &self,
        params: Parameters<WalletRestoreParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::Restore {
            id: mcp_id(),
            wallet: p.wallet,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    // ── History tools ────────────────────────────────────────────────────

    /// List transaction history from local store
    #[tool(description = "List transaction history from local store")]
    async fn history_list(
        &self,
        params: Parameters<HistoryListParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::HistoryList {
            id: mcp_id(),
            wallet: p.wallet,
            network: p.network,
            onchain_memo: p.onchain_memo,
            limit: p.limit,
            offset: p.offset,
            since_epoch_s: p.since_epoch_s,
            until_epoch_s: p.until_epoch_s,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Check transaction status
    #[tool(description = "Check transaction status by ID")]
    async fn history_status(
        &self,
        params: Parameters<HistoryStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::HistoryStatus {
            id: mcp_id(),
            transaction_id: p.transaction_id,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Incrementally sync history into local store
    #[tool(description = "Incrementally sync provider history into local store")]
    async fn history_update(
        &self,
        params: Parameters<HistoryUpdateParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::HistoryUpdate {
            id: mcp_id(),
            wallet: p.wallet,
            network: p.network,
            limit: p.limit,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    // ── Spend limit tools ────────────────────────────────────────────────

    /// Add a spend limit rule
    #[tool(description = "Add a spend limit rule")]
    async fn limit_add(
        &self,
        params: Parameters<LimitAddParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::LimitAdd {
            id: mcp_id(),
            limit: SpendLimit {
                rule_id: None,
                scope: p.scope,
                network: p.network,
                wallet: p.wallet,
                window_s: p.window_s,
                max_spend: p.max_spend,
                token: p.token,
            },
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Remove a spend limit rule by ID
    #[tool(description = "Remove a spend limit rule by ID")]
    async fn limit_remove(
        &self,
        params: Parameters<LimitRemoveParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::LimitRemove {
            id: mcp_id(),
            rule_id: p.rule_id,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// List spend limit status
    #[tool(description = "List spend limit status")]
    async fn limit_list(
        &self,
        _params: Parameters<LimitListParams>,
    ) -> Result<CallToolResult, McpError> {
        let input = Input::LimitList { id: mcp_id() };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Set spend limits (replace all)
    #[tool(description = "Set spend limits (replace all existing)")]
    async fn limit_set(
        &self,
        params: Parameters<LimitSetParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::LimitSet {
            id: mcp_id(),
            limits: p.limits,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    // ── Wallet config tools ──────────────────────────────────────────────

    /// Show wallet configuration
    #[tool(
        description = "Show wallet configuration (label, rpc endpoints, chain id, custom tokens)"
    )]
    async fn wallet_config_show(
        &self,
        params: Parameters<WalletConfigShowParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::WalletConfigShow {
            id: mcp_id(),
            wallet: p.wallet,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Update wallet settings
    #[tool(description = "Update wallet settings (label, rpc endpoints, chain id)")]
    async fn wallet_config_set(
        &self,
        params: Parameters<WalletConfigSetParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::WalletConfigSet {
            id: mcp_id(),
            wallet: p.wallet,
            label: p.label,
            rpc_endpoints: p.rpc_endpoints,
            chain_id: p.chain_id,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Register a custom token for balance tracking
    #[tool(description = "Register a custom token for balance tracking")]
    async fn wallet_config_token_add(
        &self,
        params: Parameters<WalletConfigTokenAddParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::WalletConfigTokenAdd {
            id: mcp_id(),
            wallet: p.wallet,
            symbol: p.symbol,
            address: p.address,
            decimals: p.decimals,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Unregister a custom token
    #[tool(description = "Unregister a custom token")]
    async fn wallet_config_token_remove(
        &self,
        params: Parameters<WalletConfigTokenRemoveParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::WalletConfigTokenRemove {
            id: mcp_id(),
            wallet: p.wallet,
            symbol: p.symbol,
        };
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    // ── Runtime config + version ─────────────────────────────────────────

    /// Read or update runtime configuration
    #[tool(description = "Read or update runtime configuration (limits, log filters)")]
    async fn pay_config(
        &self,
        params: Parameters<PayConfigParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;
        let input = Input::Config(ConfigPatch {
            data_dir: None,
            limits: p.limits,
            log: p.log,
            exchange_rate: None,
            afpay_rpc: None,
            providers: None,
        });
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }

    /// Show afpay version
    #[tool(description = "Show afpay version and server info")]
    async fn version(
        &self,
        _params: Parameters<VersionParams>,
    ) -> Result<CallToolResult, McpError> {
        let input = Input::Version;
        handler::dispatch(&self.app, input).await;
        Ok(drain_outputs(&self.app, &self.rx).await)
    }
}

#[tool_handler]
impl ServerHandler for AfpayMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("afpay", VERSION))
            .with_instructions(
                "afpay cryptocurrency micropayment tool — multi-currency wallet management, \
                 send/receive payments, spend limits, and transaction history. \
                 Supports Cashu, Lightning, Solana, and EVM networks.",
            )
    }
}
