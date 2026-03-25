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
pub mod remote;
#[cfg(feature = "sol")]
pub mod sol;

use crate::types::*;
use async_trait::async_trait;
use std::fmt;

// ═══════════════════════════════════════════
// PayError
// ═══════════════════════════════════════════

#[derive(Debug)]
#[allow(dead_code)]
pub enum PayError {
    NotImplemented(String),
    WalletNotFound(String),
    InvalidAmount(String),
    NetworkError(String),
    InternalError(String),
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
    },
}

impl fmt::Display for PayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotImplemented(msg) => write!(f, "{msg}"),
            Self::WalletNotFound(msg) => write!(f, "{msg}"),
            Self::InvalidAmount(msg) => write!(f, "{msg}"),
            Self::NetworkError(msg) => write!(f, "{msg}"),
            Self::InternalError(msg) => write!(f, "{msg}"),
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
        }
    }
}

impl PayError {
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::NotImplemented(_) => "not_implemented",
            Self::WalletNotFound(_) => "wallet_not_found",
            Self::InvalidAmount(_) => "invalid_amount",
            Self::NetworkError(_) => "network_error",
            Self::InternalError(_) => "internal_error",
            Self::LimitExceeded { .. } => "limit_exceeded",
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(self, Self::NetworkError(_))
    }

    pub fn hint(&self) -> Option<String> {
        match self {
            Self::NotImplemented(_) => Some("enable redb or postgres storage backend".to_string()),
            Self::WalletNotFound(_) => Some("list wallets with: afpay wallet list".to_string()),
            Self::LimitExceeded { .. } => Some("check limits with: afpay limit list".to_string()),
            _ => None,
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
    async fn ping(&self) -> Result<(), PayError> {
        Ok(())
    }

    async fn create_wallet(&self, request: &WalletCreateRequest) -> Result<WalletInfo, PayError>;
    async fn create_ln_wallet(
        &self,
        _request: LnWalletCreateRequest,
    ) -> Result<WalletInfo, PayError> {
        Err(PayError::NotImplemented(
            "ln wallet creation not supported".to_string(),
        ))
    }
    async fn close_wallet(&self, wallet: &str) -> Result<(), PayError>;
    async fn list_wallets(&self) -> Result<Vec<WalletSummary>, PayError>;
    async fn balance(&self, wallet: &str) -> Result<BalanceInfo, PayError>;
    async fn check_balance(&self, _wallet: &str) -> Result<BalanceInfo, PayError> {
        Err(PayError::NotImplemented(
            "check_balance not supported".to_string(),
        ))
    }
    async fn restore(&self, _wallet: &str) -> Result<RestoreResult, PayError> {
        Err(PayError::NotImplemented(
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
        Err(PayError::NotImplemented(
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
        Err(PayError::NotImplemented(
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
        Err(PayError::NotImplemented("network not enabled".to_string()))
    }

    async fn create_ln_wallet(
        &self,
        _request: LnWalletCreateRequest,
    ) -> Result<WalletInfo, PayError> {
        Err(PayError::NotImplemented("network not enabled".to_string()))
    }

    async fn close_wallet(&self, _wallet: &str) -> Result<(), PayError> {
        Err(PayError::NotImplemented("network not enabled".to_string()))
    }

    async fn list_wallets(&self) -> Result<Vec<WalletSummary>, PayError> {
        Err(PayError::NotImplemented("network not enabled".to_string()))
    }

    async fn balance(&self, _wallet: &str) -> Result<BalanceInfo, PayError> {
        Err(PayError::NotImplemented("network not enabled".to_string()))
    }

    async fn balance_all(&self) -> Result<Vec<WalletBalanceItem>, PayError> {
        Err(PayError::NotImplemented("network not enabled".to_string()))
    }

    async fn receive_info(
        &self,
        _wallet: &str,
        _amount: Option<Amount>,
    ) -> Result<ReceiveInfo, PayError> {
        Err(PayError::NotImplemented("network not enabled".to_string()))
    }

    async fn receive_claim(&self, _wallet: &str, _quote_id: &str) -> Result<u64, PayError> {
        Err(PayError::NotImplemented("network not enabled".to_string()))
    }

    async fn cashu_send(
        &self,
        _wallet: &str,
        _amount: Amount,
        _onchain_memo: Option<&str>,
        _mints: Option<&[String]>,
    ) -> Result<CashuSendResult, PayError> {
        Err(PayError::NotImplemented("network not enabled".to_string()))
    }

    async fn cashu_receive(
        &self,
        _wallet: &str,
        _token: &str,
    ) -> Result<CashuReceiveResult, PayError> {
        Err(PayError::NotImplemented("network not enabled".to_string()))
    }

    async fn send(
        &self,
        _wallet: &str,
        _to: &str,
        _onchain_memo: Option<&str>,
        _mints: Option<&[String]>,
    ) -> Result<SendResult, PayError> {
        Err(PayError::NotImplemented("network not enabled".to_string()))
    }

    async fn history_list(
        &self,
        _wallet: &str,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<HistoryRecord>, PayError> {
        Err(PayError::NotImplemented("network not enabled".to_string()))
    }

    async fn history_status(&self, _transaction_id: &str) -> Result<HistoryStatusInfo, PayError> {
        Err(PayError::NotImplemented("network not enabled".to_string()))
    }
}
