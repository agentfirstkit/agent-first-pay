#[cfg(any(
    feature = "btc-esplora",
    feature = "btc-core",
    feature = "btc-electrum"
))]
pub mod btc;
#[cfg(feature = "cashu")]
pub mod cashu;
#[cfg(feature = "evm")]
pub mod evm;
#[cfg(any(feature = "ln-nwc", feature = "ln-phoenixd", feature = "ln-lnbits"))]
pub mod ln;
#[cfg(feature = "rpc")]
pub mod remote;
#[cfg(feature = "sol")]
pub mod sol;

use crate::types::*;
use async_trait::async_trait;
use std::fmt;

// ═══════════════════════════════════════════
// PayError
// ═══════════════════════════════════════════
//
// All variants carry a structured `hint: Option<String>` so agents can recover
// programmatically. Constructor helpers (`not_implemented`, `wallet_not_found`, …)
// keep ergonomic call-sites short; pass an explicit struct literal when a call-site
// has a more specific hint than the default returned by `hint()`.

#[derive(Debug)]
#[allow(dead_code)]
pub enum PayError {
    NotImplemented {
        message: String,
        hint: Option<String>,
    },
    WalletNotFound {
        wallet: String,
        hint: Option<String>,
    },
    InvalidAmount {
        message: String,
        hint: Option<String>,
    },
    NetworkError {
        message: String,
        hint: Option<String>,
        retry_after_ms: Option<u64>,
    },
    InternalError {
        message: String,
        hint: Option<String>,
    },
    LimitExceeded {
        rule_id: String,
        scope: SpendScope,
        scope_key: String,
        spent: u64,
        max_spend: u64,
        token: Option<String>,
        remaining_s: u64,
        /// Which node rejected: None = local, Some(endpoint) = remote.
        origin: Option<String>,
        hint: Option<String>,
    },
    /// A mutating limit/admin operation was attempted on a client that does not
    /// hold the spend ledger locally. The agent should send the request to the
    /// daemon at `daemon_endpoint` instead.
    ConfigureOnDaemon {
        operation: String,
        daemon_endpoint: Option<String>,
    },
    /// A remote afpay daemon returned a response that violates the wire protocol
    /// (missing required fields, wrong types, etc.). Distinct from NetworkError
    /// which is transient.
    RemoteProtocolError {
        endpoint: String,
        detail: String,
        hint: Option<String>,
    },
    /// A request targeted a resource (mint URL, esplora endpoint, …) that is not
    /// in the operator's allowlist. Distinct from InvalidAmount so agents can
    /// surface the allowlist hint without ambiguity.
    Forbidden {
        message: String,
        hint: Option<String>,
    },
    InvalidRequest {
        message: String,
    },
}

impl fmt::Display for PayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotImplemented { message, .. } => write!(f, "{message}"),
            Self::WalletNotFound { wallet, .. } => write!(f, "wallet not found: {wallet}"),
            Self::InvalidAmount { message, .. } => write!(f, "{message}"),
            Self::NetworkError { message, .. } => write!(f, "{message}"),
            Self::InternalError { message, .. } => write!(f, "{message}"),
            Self::LimitExceeded {
                scope,
                scope_key,
                spent,
                max_spend,
                token,
                origin,
                ..
            } => {
                let token_str = token.as_deref().unwrap_or("base-units");
                if let Some(node) = origin {
                    write!(
                        f,
                        "spend limit exceeded at {node} ({scope:?}:{scope_key}): spent {spent} of {max_spend} {token_str}"
                    )
                } else {
                    write!(
                        f,
                        "spend limit exceeded ({scope:?}:{scope_key}): spent {spent} of {max_spend} {token_str}"
                    )
                }
            }
            Self::ConfigureOnDaemon {
                operation,
                daemon_endpoint,
            } => match daemon_endpoint {
                Some(ep) => write!(
                    f,
                    "operation `{operation}` must be sent to the daemon at {ep}"
                ),
                None => write!(
                    f,
                    "operation `{operation}` must be configured on the spend-ledger daemon"
                ),
            },
            Self::RemoteProtocolError {
                endpoint, detail, ..
            } => write!(f, "remote {endpoint} returned malformed response: {detail}"),
            Self::Forbidden { message, .. } => write!(f, "{message}"),
            Self::InvalidRequest { message } => write!(f, "{message}"),
        }
    }
}

impl PayError {
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::NotImplemented { .. } => "not_implemented",
            Self::WalletNotFound { .. } => "wallet_not_found",
            Self::InvalidAmount { .. } => "invalid_amount",
            Self::NetworkError { .. } => "network_error",
            Self::InternalError { .. } => "internal_error",
            Self::LimitExceeded { .. } => "limit_exceeded",
            Self::ConfigureOnDaemon { .. } => "configure_on_daemon",
            Self::RemoteProtocolError { .. } => "remote_protocol_error",
            Self::Forbidden { .. } => "forbidden",
            Self::InvalidRequest { .. } => "invalid_request",
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(self, Self::NetworkError { .. })
    }

    /// Optional retry-after hint for caller backoff. Returns Some only for
    /// transient errors where the upstream gave a concrete delay (e.g. a 429).
    #[allow(dead_code)]
    pub fn retry_after_ms(&self) -> Option<u64> {
        match self {
            Self::NetworkError { retry_after_ms, .. } => *retry_after_ms,
            _ => None,
        }
    }

    /// Agent-actionable hint. Variants return the call-site override when set,
    /// otherwise a sensible default. Every variant returns Some — silent None
    /// means a programming error.
    pub fn hint(&self) -> Option<String> {
        match self {
            Self::NotImplemented { hint, .. } => hint.clone().or_else(|| {
                Some(
                    "this build does not support the requested operation; check `--help` for enabled features"
                        .to_string(),
                )
            }),
            Self::WalletNotFound { hint, wallet } => hint.clone().or_else(|| {
                Some(format!(
                    "wallet `{wallet}` not found; list wallets with `afpay wallet list` or create one with `afpay wallet create`"
                ))
            }),
            Self::InvalidAmount { hint, .. } => hint.clone().or_else(|| {
                Some(
                    "amount must be a positive integer in the network's base unit (sats/lamports/wei)"
                        .to_string(),
                )
            }),
            Self::NetworkError { hint, .. } => hint.clone().or_else(|| {
                Some("transient network failure; retry with exponential backoff".to_string())
            }),
            Self::InternalError { hint, .. } => hint.clone().or_else(|| {
                Some(
                    "internal error; rerun with `--log debug` for diagnostics and report if reproducible"
                        .to_string(),
                )
            }),
            Self::LimitExceeded { hint, .. } => hint
                .clone()
                .or_else(|| Some("inspect active rules with `afpay limit list`".to_string())),
            Self::ConfigureOnDaemon {
                operation,
                daemon_endpoint,
            } => Some(match daemon_endpoint {
                Some(ep) => format!(
                    "send `{operation}` to the spend-ledger daemon at {ep}; this client does not enforce limits locally"
                ),
                None => format!(
                    "`{operation}` requires a local spend-ledger; enable a storage backend (redb/postgres) or point `afpay_rpc` at a daemon in config.toml"
                ),
            }),
            Self::RemoteProtocolError { hint, endpoint, .. } => hint.clone().or_else(|| {
                Some(format!(
                    "remote {endpoint} returned a malformed response; verify it runs a compatible afpay version"
                ))
            }),
            Self::Forbidden { hint, .. } => hint.clone().or_else(|| {
                Some(
                    "operator policy rejected this request; check `allowed_*_urls` in runtime config"
                        .to_string(),
                )
            }),
            Self::InvalidRequest { message } => Some(format!(
                "invalid request: {message}; check `afpay config --help` for valid keys"
            )),
        }
    }

    // ────────────────────────────────────────
    // Constructor helpers (the common "no call-site hint" case)
    // ────────────────────────────────────────

    pub fn not_implemented(message: impl Into<String>) -> Self {
        Self::NotImplemented {
            message: message.into(),
            hint: None,
        }
    }

    pub fn wallet_not_found(wallet: impl Into<String>) -> Self {
        Self::WalletNotFound {
            wallet: wallet.into(),
            hint: None,
        }
    }

    pub fn invalid_amount(message: impl Into<String>) -> Self {
        Self::InvalidAmount {
            message: message.into(),
            hint: None,
        }
    }

    pub fn network_error(message: impl Into<String>) -> Self {
        Self::NetworkError {
            message: message.into(),
            hint: None,
            retry_after_ms: None,
        }
    }

    pub fn internal_error(message: impl Into<String>) -> Self {
        Self::InternalError {
            message: message.into(),
            hint: None,
        }
    }

    #[allow(dead_code)]
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::Forbidden {
            message: message.into(),
            hint: None,
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::InvalidRequest {
            message: message.into(),
        }
    }
}

// ═══════════════════════════════════════════
// PayProvider Trait
// ═══════════════════════════════════════════

#[derive(Debug, Clone, Copy, Default)]
pub struct HistorySyncStats {
    pub records_scanned: usize,
    pub records_added: usize,
    pub records_updated: usize,
}

#[async_trait]
pub trait PayProvider: Send + Sync {
    #[allow(dead_code)]
    fn network(&self) -> Network;

    /// Whether this provider writes to local disk (needs data-dir lock).
    fn writes_locally(&self) -> bool {
        false
    }

    /// Connectivity check. Remote providers ping the RPC endpoint; local providers no-op.
    #[allow(dead_code)]
    async fn ping(&self) -> Result<(), PayError> {
        Ok(())
    }

    async fn create_wallet(&self, request: &WalletCreateRequest) -> Result<WalletInfo, PayError>;
    async fn create_ln_wallet(
        &self,
        _request: LnWalletCreateRequest,
    ) -> Result<WalletInfo, PayError> {
        Err(PayError::not_implemented(
            "ln wallet creation not supported".to_string(),
        ))
    }
    async fn close_wallet(&self, wallet: &str) -> Result<(), PayError>;
    async fn list_wallets(&self) -> Result<Vec<WalletSummary>, PayError>;
    async fn balance(&self, wallet: &str) -> Result<BalanceInfo, PayError>;
    async fn check_balance(&self, _wallet: &str) -> Result<BalanceInfo, PayError> {
        Err(PayError::not_implemented(
            "check_balance not supported".to_string(),
        ))
    }
    async fn restore(&self, _wallet: &str) -> Result<RestoreResult, PayError> {
        Err(PayError::not_implemented(
            "restore not supported".to_string(),
        ))
    }
    async fn balance_all(&self) -> Result<Vec<WalletBalanceItem>, PayError>;
    async fn receive_info(
        &self,
        wallet: &str,
        amount: Option<Amount>,
    ) -> Result<ReceiveInfo, PayError>;
    async fn receive_claim(&self, wallet: &str, quote_id: &str) -> Result<u64, PayError>;

    #[cfg(feature = "interactive")]
    async fn cashu_send_quote(
        &self,
        _wallet: &str,
        _amount: &Amount,
    ) -> Result<CashuSendQuoteInfo, PayError> {
        Err(PayError::not_implemented(
            "cashu_send_quote not supported".to_string(),
        ))
    }
    async fn cashu_send(
        &self,
        wallet: &str,
        amount: Amount,
        onchain_memo: Option<&str>,
        mints: Option<&[String]>,
    ) -> Result<CashuSendResult, PayError>;
    async fn cashu_receive(
        &self,
        wallet: &str,
        token: &str,
    ) -> Result<CashuReceiveResult, PayError>;
    async fn send(
        &self,
        wallet: &str,
        to: &str,
        onchain_memo: Option<&str>,
        mints: Option<&[String]>,
    ) -> Result<SendResult, PayError>;

    async fn send_quote(
        &self,
        _wallet: &str,
        _to: &str,
        _mints: Option<&[String]>,
    ) -> Result<SendQuoteInfo, PayError> {
        Err(PayError::not_implemented(
            "send_quote not supported".to_string(),
        ))
    }

    async fn history_list(
        &self,
        wallet: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<HistoryRecord>, PayError>;
    async fn history_status(&self, transaction_id: &str) -> Result<HistoryStatusInfo, PayError>;
    /// Optional provider-specific on-chain memo decoding for a transaction.
    /// Returns `Ok(None)` when memo cannot be decoded or is absent.
    async fn history_onchain_memo(
        &self,
        _wallet: &str,
        _transaction_id: &str,
    ) -> Result<Option<String>, PayError> {
        Ok(None)
    }
    async fn history_sync(&self, wallet: &str, limit: usize) -> Result<HistorySyncStats, PayError> {
        let items = self.history_list(wallet, limit, 0).await?;
        Ok(HistorySyncStats {
            records_scanned: items.len(),
            records_added: 0,
            records_updated: 0,
        })
    }
}

// ═══════════════════════════════════════════
// StubProvider
// ═══════════════════════════════════════════

pub struct StubProvider {
    #[allow(dead_code)]
    network: Network,
}

impl StubProvider {
    pub fn new(network: Network) -> Self {
        Self { network }
    }
}

#[async_trait]
impl PayProvider for StubProvider {
    fn network(&self) -> Network {
        self.network
    }

    async fn create_wallet(&self, _request: &WalletCreateRequest) -> Result<WalletInfo, PayError> {
        Err(PayError::not_implemented("network not enabled".to_string()))
    }

    async fn create_ln_wallet(
        &self,
        _request: LnWalletCreateRequest,
    ) -> Result<WalletInfo, PayError> {
        Err(PayError::not_implemented("network not enabled".to_string()))
    }

    async fn close_wallet(&self, _wallet: &str) -> Result<(), PayError> {
        Err(PayError::not_implemented("network not enabled".to_string()))
    }

    async fn list_wallets(&self) -> Result<Vec<WalletSummary>, PayError> {
        Err(PayError::not_implemented("network not enabled".to_string()))
    }

    async fn balance(&self, _wallet: &str) -> Result<BalanceInfo, PayError> {
        Err(PayError::not_implemented("network not enabled".to_string()))
    }

    async fn balance_all(&self) -> Result<Vec<WalletBalanceItem>, PayError> {
        Err(PayError::not_implemented("network not enabled".to_string()))
    }

    async fn receive_info(
        &self,
        _wallet: &str,
        _amount: Option<Amount>,
    ) -> Result<ReceiveInfo, PayError> {
        Err(PayError::not_implemented("network not enabled".to_string()))
    }

    async fn receive_claim(&self, _wallet: &str, _quote_id: &str) -> Result<u64, PayError> {
        Err(PayError::not_implemented("network not enabled".to_string()))
    }

    async fn cashu_send(
        &self,
        _wallet: &str,
        _amount: Amount,
        _onchain_memo: Option<&str>,
        _mints: Option<&[String]>,
    ) -> Result<CashuSendResult, PayError> {
        Err(PayError::not_implemented("network not enabled".to_string()))
    }

    async fn cashu_receive(
        &self,
        _wallet: &str,
        _token: &str,
    ) -> Result<CashuReceiveResult, PayError> {
        Err(PayError::not_implemented("network not enabled".to_string()))
    }

    async fn send(
        &self,
        _wallet: &str,
        _to: &str,
        _onchain_memo: Option<&str>,
        _mints: Option<&[String]>,
    ) -> Result<SendResult, PayError> {
        Err(PayError::not_implemented("network not enabled".to_string()))
    }

    async fn history_list(
        &self,
        _wallet: &str,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<HistoryRecord>, PayError> {
        Err(PayError::not_implemented("network not enabled".to_string()))
    }

    async fn history_status(&self, _transaction_id: &str) -> Result<HistoryStatusInfo, PayError> {
        Err(PayError::not_implemented("network not enabled".to_string()))
    }
}
