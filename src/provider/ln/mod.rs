use crate::provider::{HistorySyncStats, PayError, PayProvider};
use crate::store::transaction;
use crate::store::wallet::{self, WalletMetadata};
use crate::types::*;
use async_trait::async_trait;

#[cfg(feature = "ln-lnbits")]
mod lnbits;
#[cfg(feature = "ln-nwc")]
mod nwc;
#[cfg(feature = "ln-phoenixd")]
mod phoenixd;

// ═══════════════════════════════════════════
// LnBackend — internal trait for each backend
// ═══════════════════════════════════════════

#[derive(Debug, Clone)]
pub(crate) struct LnPayResult {
    pub confirmed_amount_sats: u64,
    pub fee_msats: Option<u64>,
    pub preimage: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct LnInvoiceResult {
    pub bolt11: String,
    pub payment_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum LnPaymentStatus {
    Pending,
    Paid,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LnInvoiceStatus {
    Pending,
    Paid { confirmed_amount_sats: u64 },
    Failed,
    Unknown,
}

#[derive(Debug, Clone)]
pub(crate) struct LnPaymentInfo {
    pub payment_hash: String,
    pub amount_msats: u64,
    pub is_outgoing: bool,
    pub status: LnPaymentStatus,
    pub created_at_epoch_s: u64,
    pub memo: Option<String>,
    pub preimage: Option<String>,
}

#[async_trait]
pub(crate) trait LnBackend: Send + Sync {
    async fn pay_invoice(
        &self,
        bolt11: &str,
        amount_msats: Option<u64>,
    ) -> Result<LnPayResult, PayError>;

    async fn create_invoice(
        &self,
        amount_sats: u64,
        memo: Option<&str>,
    ) -> Result<LnInvoiceResult, PayError>;

    async fn invoice_status(&self, payment_hash: &str) -> Result<LnInvoiceStatus, PayError>;

    async fn get_balance(&self) -> Result<BalanceInfo, PayError>;

    async fn list_payments(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<LnPaymentInfo>, PayError>;
}

// ═══════════════════════════════════════════
// LnProvider — PayProvider implementation
// ═══════════════════════════════════════════

fn ln_wallet_summary(m: &WalletMetadata) -> WalletSummary {
    let backend = m.backend.clone().unwrap_or_else(|| "unknown".to_string());
    WalletSummary {
        id: m.id.clone(),
        network: Network::Ln,
        label: m.label.clone(),
        address: format!("ln:{backend}"),
        backend: Some(backend),
        mint_url: None,
        rpc_endpoints: None,
        chain_id: None,
        created_at_epoch_s: m.created_at_epoch_s,
    }
}

pub struct LnProvider {
    data_dir: String,
}

impl LnProvider {
    pub fn new(data_dir: &str) -> Self {
        Self {
            data_dir: data_dir.to_string(),
        }
    }

    fn resolve_backend(&self, meta: &WalletMetadata) -> Result<Box<dyn LnBackend>, PayError> {
        let backend_name = meta.backend.as_deref().ok_or_else(|| {
            PayError::InternalError("ln wallet missing backend field".to_string())
        })?;

        #[cfg(feature = "ln-nwc")]
        if backend_name == "nwc" {
            let secret = meta.seed_secret.as_deref().unwrap_or("");
            return Ok(Box::new(nwc::NwcBackend::new(secret)?));
        }
        #[cfg(feature = "ln-phoenixd")]
        if backend_name == "phoenixd" {
            let endpoint = meta.mint_url.as_deref().unwrap_or("");
            let secret = meta.seed_secret.as_deref().unwrap_or("");
            return Ok(Box::new(phoenixd::PhoenixdBackend::new(endpoint, secret)));
        }
        #[cfg(feature = "ln-lnbits")]
        if backend_name == "lnbits" {
            let endpoint = meta.mint_url.as_deref().unwrap_or("");
            let secret = meta.seed_secret.as_deref().unwrap_or("");
            return Ok(Box::new(lnbits::LnbitsBackend::new(endpoint, secret)));
        }

        Err(PayError::NotImplemented(format!(
            "ln backend '{backend_name}' not enabled"
        )))
    }

    fn load_ln_wallet(&self, wallet_id: &str) -> Result<WalletMetadata, PayError> {
        let meta = wallet::load_wallet_metadata(&self.data_dir, wallet_id)?;
        if meta.network != Network::Ln {
            return Err(PayError::WalletNotFound(format!(
                "{wallet_id} is not a ln wallet"
            )));
        }
        Ok(meta)
    }

    /// Find a LN wallet — if wallet_id is empty, pick the first available.
    fn resolve_wallet_id(&self, wallet_id: &str) -> Result<String, PayError> {
        if !wallet_id.is_empty() {
            return Ok(wallet_id.to_string());
        }
        let wallets = wallet::list_wallet_metadata(&self.data_dir, Some(Network::Ln))?;
        wallets
            .first()
            .map(|w| w.id.clone())
            .ok_or_else(|| PayError::WalletNotFound("no ln wallet found".to_string()))
    }

    fn validate_backend_enabled(backend: LnWalletBackend) -> Result<(), PayError> {
        #[allow(unreachable_patterns)]
        let enabled = match backend {
            #[cfg(feature = "ln-nwc")]
            LnWalletBackend::Nwc => true,
            #[cfg(feature = "ln-phoenixd")]
            LnWalletBackend::Phoenixd => true,
            #[cfg(feature = "ln-lnbits")]
            LnWalletBackend::Lnbits => true,
            _ => false,
        };
        if !enabled {
            return Err(PayError::NotImplemented(format!(
                "backend '{}' not compiled; rebuild with --features {}",
                backend.as_str(),
                backend.as_str()
            )));
        }
        Ok(())
    }

    fn has_value(value: Option<&str>) -> bool {
        value.map(|v| !v.trim().is_empty()).unwrap_or(false)
    }

    fn require_field(
        backend: LnWalletBackend,
        field_name: &str,
        value: Option<String>,
    ) -> Result<String, PayError> {
        if Self::has_value(value.as_deref()) {
            return Ok(value.unwrap_or_default());
        }
        Err(PayError::InvalidAmount(format!(
            "{} backend requires --{}",
            backend.as_str(),
            field_name
        )))
    }

    fn reject_field(
        backend: LnWalletBackend,
        field_name: &str,
        value: Option<&str>,
    ) -> Result<(), PayError> {
        if Self::has_value(value) {
            return Err(PayError::InvalidAmount(format!(
                "{} backend does not accept --{}",
                backend.as_str(),
                field_name
            )));
        }
        Ok(())
    }

    async fn validate_backend_credentials(
        &self,
        backend: LnWalletBackend,
        endpoint: Option<String>,
        secret: Option<String>,
        label: Option<String>,
    ) -> Result<(), PayError> {
        let probe_meta = WalletMetadata {
            id: "__probe__".to_string(),
            network: Network::Ln,
            label,
            mint_url: endpoint,
            sol_rpc_endpoints: None,
            evm_rpc_endpoints: None,
            evm_chain_id: None,
            seed_secret: secret,
            backend: Some(backend.as_str().to_string()),
            btc_esplora_url: None,
            btc_network: None,
            btc_address_type: None,
            btc_core_url: None,
            btc_core_auth_secret: None,
            btc_electrum_url: None,
            custom_tokens: None,
            created_at_epoch_s: 0,
            error: None,
        };
        let backend_impl = self.resolve_backend(&probe_meta)?;
        backend_impl.get_balance().await.map(|_| ()).map_err(|e| {
            PayError::NetworkError(format!(
                "{} backend validation failed: {}",
                backend.as_str(),
                e
            ))
        })
    }
}

#[async_trait]
impl PayProvider for LnProvider {
    fn network(&self) -> Network {
        Network::Ln
    }

    fn writes_locally(&self) -> bool {
        true
    }

    async fn create_wallet(&self, _request: &WalletCreateRequest) -> Result<WalletInfo, PayError> {
        Err(PayError::InvalidAmount(
            "ln wallets must be created with ln_wallet_create parameters".to_string(),
        ))
    }

    async fn create_ln_wallet(
        &self,
        request: LnWalletCreateRequest,
    ) -> Result<WalletInfo, PayError> {
        Self::validate_backend_enabled(request.backend)?;

        let backend = request.backend;
        let label = request.label.as_deref().unwrap_or("default").trim();
        let wallet_label = if label.is_empty() || label == "default" {
            None
        } else {
            Some(label.to_string())
        };

        let (endpoint, secret) = match backend {
            LnWalletBackend::Nwc => {
                Self::reject_field(backend, "endpoint", request.endpoint.as_deref())?;
                Self::reject_field(
                    backend,
                    "password-secret",
                    request.password_secret.as_deref(),
                )?;
                Self::reject_field(
                    backend,
                    "admin-key-secret",
                    request.admin_key_secret.as_deref(),
                )?;
                let nwc_uri =
                    Self::require_field(backend, "nwc-uri-secret", request.nwc_uri_secret)?;
                (None, Some(nwc_uri))
            }
            LnWalletBackend::Phoenixd => {
                Self::reject_field(backend, "nwc-uri-secret", request.nwc_uri_secret.as_deref())?;
                Self::reject_field(
                    backend,
                    "admin-key-secret",
                    request.admin_key_secret.as_deref(),
                )?;
                let endpoint = Self::require_field(backend, "endpoint", request.endpoint)?;
                let password =
                    Self::require_field(backend, "password-secret", request.password_secret)?;
                (Some(endpoint), Some(password))
            }
            LnWalletBackend::Lnbits => {
                Self::reject_field(backend, "nwc-uri-secret", request.nwc_uri_secret.as_deref())?;
                Self::reject_field(
                    backend,
                    "password-secret",
                    request.password_secret.as_deref(),
                )?;
                let endpoint = Self::require_field(backend, "endpoint", request.endpoint)?;
                let admin_key =
                    Self::require_field(backend, "admin-key-secret", request.admin_key_secret)?;
                (Some(endpoint), Some(admin_key))
            }
        };

        self.validate_backend_credentials(
            backend,
            endpoint.clone(),
            secret.clone(),
            wallet_label.clone(),
        )
        .await?;

        let id = wallet::generate_wallet_identifier()?;
        let meta = WalletMetadata {
            id: id.clone(),
            network: Network::Ln,
            label: wallet_label,
            mint_url: endpoint,
            sol_rpc_endpoints: None,
            evm_rpc_endpoints: None,
            evm_chain_id: None,
            seed_secret: secret,
            backend: Some(backend.as_str().to_string()),
            btc_esplora_url: None,
            btc_network: None,
            btc_address_type: None,
            btc_core_url: None,
            btc_core_auth_secret: None,
            btc_electrum_url: None,
            custom_tokens: None,
            created_at_epoch_s: wallet::now_epoch_seconds(),
            error: None,
        };
        wallet::save_wallet_metadata(&self.data_dir, &meta)?;

        Ok(WalletInfo {
            id,
            network: Network::Ln,
            address: format!("ln:{}", backend.as_str()),
            label: meta.label,
            mnemonic: None,
        })
    }

    async fn close_wallet(&self, wallet_id: &str) -> Result<(), PayError> {
        let meta = self.load_ln_wallet(wallet_id)?;
        // Check balance — only allow closing zero-balance wallets
        let backend = self.resolve_backend(&meta)?;
        let balance = backend.get_balance().await?;
        let non_zero_components = balance.non_zero_components();
        if !non_zero_components.is_empty() {
            let component_list = non_zero_components
                .iter()
                .map(|(name, value)| format!("{name}={value}sats"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(PayError::InvalidAmount(format!(
                "wallet {wallet_id} has non-zero balance components ({component_list}); withdraw first"
            )));
        }
        wallet::delete_wallet_metadata(&self.data_dir, wallet_id)?;
        Ok(())
    }

    async fn list_wallets(&self) -> Result<Vec<WalletSummary>, PayError> {
        let wallets = wallet::list_wallet_metadata(&self.data_dir, Some(Network::Ln))?;
        Ok(wallets.iter().map(ln_wallet_summary).collect())
    }

    async fn balance(&self, wallet_id: &str) -> Result<BalanceInfo, PayError> {
        let meta = self.load_ln_wallet(wallet_id)?;
        let backend = self.resolve_backend(&meta)?;
        backend.get_balance().await
    }

    async fn balance_all(&self) -> Result<Vec<WalletBalanceItem>, PayError> {
        let wallets = wallet::list_wallet_metadata(&self.data_dir, Some(Network::Ln))?;
        let mut items = Vec::new();
        for meta in &wallets {
            let (balance, error) = match self.resolve_backend(meta) {
                Ok(backend) => match backend.get_balance().await {
                    Ok(balance) => (Some(balance), None),
                    Err(error) => (None, Some(error.to_string())),
                },
                Err(error) => (None, Some(error.to_string())),
            };
            items.push(WalletBalanceItem {
                wallet: ln_wallet_summary(meta),
                balance,
                error,
            });
        }
        Ok(items)
    }

    async fn receive_info(
        &self,
        wallet_id: &str,
        amount: Option<Amount>,
    ) -> Result<ReceiveInfo, PayError> {
        let resolved_wallet_id = if wallet_id.trim().is_empty() {
            let wallets = wallet::list_wallet_metadata(&self.data_dir, Some(Network::Ln))?;
            match wallets.len() {
                0 => return Err(PayError::WalletNotFound("no ln wallet found".to_string())),
                1 => wallets[0].id.clone(),
                _ => {
                    return Err(PayError::InvalidAmount(
                        "multiple ln wallets found; pass --wallet".to_string(),
                    ))
                }
            }
        } else {
            wallet_id.to_string()
        };

        let meta = self.load_ln_wallet(&resolved_wallet_id)?;
        let backend = self.resolve_backend(&meta)?;
        let amount_sats = amount.as_ref().map(|a| a.value).ok_or_else(|| {
            PayError::InvalidAmount("amount-sats required for ln receive".to_string())
        })?;
        let result = backend.create_invoice(amount_sats, None).await?;
        Ok(ReceiveInfo {
            address: None,
            invoice: Some(result.bolt11),
            quote_id: Some(result.payment_hash),
        })
    }

    async fn receive_claim(&self, wallet_id: &str, quote_id: &str) -> Result<u64, PayError> {
        let meta = self.load_ln_wallet(wallet_id)?;
        let backend = self.resolve_backend(&meta)?;
        match backend.invoice_status(quote_id).await? {
            LnInvoiceStatus::Paid {
                confirmed_amount_sats,
            } => {
                // Record a local receive tx once so tx_status/history remain consistent
                // even when backend history APIs are unavailable.
                if transaction::find_transaction_record_by_id(&self.data_dir, quote_id)?.is_none() {
                    let now = wallet::now_epoch_seconds();
                    let record = HistoryRecord {
                        transaction_id: quote_id.to_string(),
                        wallet: wallet_id.to_string(),
                        network: Network::Ln,
                        direction: Direction::Receive,
                        amount: Amount {
                            value: confirmed_amount_sats,
                            token: "sats".to_string(),
                        },
                        status: TxStatus::Confirmed,
                        onchain_memo: Some("ln receive".to_string()),
                        local_memo: None,
                        remote_addr: None,
                        preimage: None,
                        created_at_epoch_s: now,
                        confirmed_at_epoch_s: Some(now),
                        fee: None,
                    };
                    let _ = transaction::append_transaction_record(&self.data_dir, &record);
                }
                Ok(confirmed_amount_sats)
            }
            LnInvoiceStatus::Pending => {
                Err(PayError::NetworkError("invoice not yet paid".to_string()))
            }
            LnInvoiceStatus::Failed => {
                Err(PayError::NetworkError("invoice payment failed".to_string()))
            }
            LnInvoiceStatus::Unknown => {
                Err(PayError::NetworkError("invoice status unknown".to_string()))
            }
        }
    }

    async fn cashu_send(
        &self,
        _wallet: &str,
        _amount: Amount,
        _memo: Option<&str>,
        _mints: Option<&[String]>,
    ) -> Result<CashuSendResult, PayError> {
        Err(PayError::NotImplemented(
            "ln does not support bearer-token send; use `ln send --to <bolt11>`".to_string(),
        ))
    }

    async fn cashu_receive(
        &self,
        _wallet: &str,
        _token: &str,
    ) -> Result<CashuReceiveResult, PayError> {
        Err(PayError::NotImplemented(
            "ln does not support token receive; use `ln receive --amount-sats <amount>`"
                .to_string(),
        ))
    }

    async fn send_quote(
        &self,
        wallet_id: &str,
        to: &str,
        _mints: Option<&[String]>,
    ) -> Result<SendQuoteInfo, PayError> {
        let resolved = self.resolve_wallet_id(wallet_id)?;
        let amount_sats = parse_bolt11_amount_sats(to)?;
        let fee_estimate = (amount_sats / 100).max(1);
        Ok(SendQuoteInfo {
            wallet: resolved,
            amount_native: amount_sats,
            fee_estimate_native: fee_estimate,
            fee_unit: "sats".to_string(),
        })
    }

    async fn send(
        &self,
        wallet_id: &str,
        to: &str,
        onchain_memo: Option<&str>,
        _mints: Option<&[String]>,
    ) -> Result<SendResult, PayError> {
        let resolved = self.resolve_wallet_id(wallet_id)?;
        let meta = self.load_ln_wallet(&resolved)?;
        let backend = self.resolve_backend(&meta)?;
        // Use invoice payment_hash as transaction_id so tx_status can be correlated with backend records.
        let transaction_id = parse_bolt11_payment_hash(to).unwrap_or_else(|_| {
            // Fallback keeps behavior if backend accepted a non-standard invoice format.
            wallet::generate_transaction_identifier().unwrap_or_else(|_| "tx_unknown".to_string())
        });

        let result = backend.pay_invoice(to, None).await?;
        if result.confirmed_amount_sats == 0 {
            return Err(PayError::NetworkError(
                "backend did not return confirmed payment amount".to_string(),
            ));
        }

        let fee_sats = result.fee_msats.map(|f| f / 1000);
        let amount = Amount {
            value: result.confirmed_amount_sats,
            token: "sats".to_string(),
        };

        let fee_amount = fee_sats.filter(|&f| f > 0).map(|f| Amount {
            value: f,
            token: "sats".to_string(),
        });
        let record = HistoryRecord {
            transaction_id: transaction_id.clone(),
            wallet: resolved.clone(),
            network: Network::Ln,
            direction: Direction::Send,
            amount: amount.clone(),
            status: TxStatus::Confirmed,
            onchain_memo: onchain_memo
                .map(|s| s.to_string())
                .or(Some("ln send".to_string())),
            local_memo: None,
            remote_addr: Some(to.to_string()),
            preimage: result.preimage.clone(),
            created_at_epoch_s: wallet::now_epoch_seconds(),
            confirmed_at_epoch_s: Some(wallet::now_epoch_seconds()),
            fee: fee_amount.clone(),
        };
        let _ = transaction::append_transaction_record(&self.data_dir, &record);

        Ok(SendResult {
            wallet: resolved,
            transaction_id,
            amount,
            fee: fee_amount,
            preimage: result.preimage,
        })
    }

    async fn history_list(
        &self,
        wallet_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<HistoryRecord>, PayError> {
        let meta = self.load_ln_wallet(wallet_id)?;
        // Try backend first, fall back to local transaction log store
        if let Ok(backend) = self.resolve_backend(&meta) {
            if let Ok(payments) = backend.list_payments(limit, offset).await {
                return Ok(payments
                    .into_iter()
                    .map(|p| HistoryRecord {
                        transaction_id: p.payment_hash.clone(),
                        wallet: wallet_id.to_string(),
                        network: Network::Ln,
                        direction: if p.is_outgoing {
                            Direction::Send
                        } else {
                            Direction::Receive
                        },
                        amount: Amount {
                            value: p.amount_msats / 1000,
                            token: "sats".to_string(),
                        },
                        status: match p.status {
                            LnPaymentStatus::Paid => TxStatus::Confirmed,
                            LnPaymentStatus::Pending => TxStatus::Pending,
                            LnPaymentStatus::Failed => TxStatus::Failed,
                            LnPaymentStatus::Unknown => TxStatus::Pending,
                        },
                        onchain_memo: p.memo,
                        local_memo: None,
                        remote_addr: None,
                        preimage: p.preimage,
                        created_at_epoch_s: p.created_at_epoch_s,
                        confirmed_at_epoch_s: if p.status == LnPaymentStatus::Paid {
                            Some(p.created_at_epoch_s)
                        } else {
                            None
                        },
                        fee: None,
                    })
                    .collect());
            }
        }
        // Fallback to local transaction log store
        let all = transaction::load_wallet_transaction_records(&self.data_dir, wallet_id)?;
        let end = all.len().min(offset + limit);
        let start = all.len().min(offset);
        Ok(all[start..end].to_vec())
    }

    async fn history_status(&self, transaction_id: &str) -> Result<HistoryStatusInfo, PayError> {
        match transaction::find_transaction_record_by_id(&self.data_dir, transaction_id)? {
            Some(rec) => Ok(HistoryStatusInfo {
                transaction_id: rec.transaction_id.clone(),
                status: rec.status,
                confirmations: None,
                preimage: rec.preimage.clone(),
                item: Some(rec),
            }),
            None => {
                // Backend fallback: scan LN wallets and query both invoice-status and payments.
                let wallets = wallet::list_wallet_metadata(&self.data_dir, Some(Network::Ln))?;
                for w in &wallets {
                    let meta = self.load_ln_wallet(&w.id)?;
                    let backend = match self.resolve_backend(&meta) {
                        Ok(b) => b,
                        Err(_) => continue,
                    };
                    match backend.invoice_status(transaction_id).await {
                        Ok(LnInvoiceStatus::Paid { .. }) => {
                            return Ok(HistoryStatusInfo {
                                transaction_id: transaction_id.to_string(),
                                status: TxStatus::Confirmed,
                                confirmations: None,
                                preimage: None,
                                item: None,
                            });
                        }
                        Ok(LnInvoiceStatus::Pending) => {
                            return Ok(HistoryStatusInfo {
                                transaction_id: transaction_id.to_string(),
                                status: TxStatus::Pending,
                                confirmations: None,
                                preimage: None,
                                item: None,
                            });
                        }
                        Ok(LnInvoiceStatus::Failed) => {
                            return Ok(HistoryStatusInfo {
                                transaction_id: transaction_id.to_string(),
                                status: TxStatus::Failed,
                                confirmations: None,
                                preimage: None,
                                item: None,
                            });
                        }
                        Ok(LnInvoiceStatus::Unknown) | Err(_) => {}
                    }

                    if let Ok(payments) = backend.list_payments(200, 0).await {
                        if let Some(p) = payments
                            .into_iter()
                            .find(|p| p.payment_hash == transaction_id)
                        {
                            let status = match p.status {
                                LnPaymentStatus::Paid => TxStatus::Confirmed,
                                LnPaymentStatus::Pending | LnPaymentStatus::Unknown => {
                                    TxStatus::Pending
                                }
                                LnPaymentStatus::Failed => TxStatus::Failed,
                            };
                            let item = HistoryRecord {
                                transaction_id: p.payment_hash.clone(),
                                wallet: w.id.clone(),
                                network: Network::Ln,
                                direction: if p.is_outgoing {
                                    Direction::Send
                                } else {
                                    Direction::Receive
                                },
                                amount: Amount {
                                    value: p.amount_msats / 1000,
                                    token: "sats".to_string(),
                                },
                                status,
                                onchain_memo: p.memo.clone(),
                                local_memo: None,
                                remote_addr: None,
                                preimage: p.preimage.clone(),
                                created_at_epoch_s: p.created_at_epoch_s,
                                confirmed_at_epoch_s: if p.status == LnPaymentStatus::Paid {
                                    Some(p.created_at_epoch_s)
                                } else {
                                    None
                                },
                                fee: None,
                            };
                            return Ok(HistoryStatusInfo {
                                transaction_id: transaction_id.to_string(),
                                status,
                                confirmations: None,
                                preimage: p.preimage,
                                item: Some(item),
                            });
                        }
                    }
                }
                Err(PayError::WalletNotFound(format!(
                    "transaction {transaction_id} not found"
                )))
            }
        }
    }

    async fn history_sync(
        &self,
        wallet_id: &str,
        limit: usize,
    ) -> Result<HistorySyncStats, PayError> {
        let resolved = self.resolve_wallet_id(wallet_id)?;
        let meta = self.load_ln_wallet(&resolved)?;
        let backend = self.resolve_backend(&meta)?;
        let payments = backend.list_payments(limit, 0).await?;

        let mut stats = HistorySyncStats {
            records_scanned: payments.len(),
            records_added: 0,
            records_updated: 0,
        };

        for payment in payments {
            let status = match payment.status {
                LnPaymentStatus::Paid => TxStatus::Confirmed,
                LnPaymentStatus::Pending | LnPaymentStatus::Unknown => TxStatus::Pending,
                LnPaymentStatus::Failed => TxStatus::Failed,
            };
            let confirmed_at_epoch_s = if status == TxStatus::Confirmed {
                Some(payment.created_at_epoch_s)
            } else {
                None
            };

            match transaction::find_transaction_record_by_id(&self.data_dir, &payment.payment_hash)?
            {
                Some(existing) => {
                    if existing.status != status
                        || existing.confirmed_at_epoch_s != confirmed_at_epoch_s
                    {
                        let _ = transaction::update_transaction_record_status(
                            &self.data_dir,
                            &payment.payment_hash,
                            status,
                            confirmed_at_epoch_s,
                        );
                        stats.records_updated = stats.records_updated.saturating_add(1);
                    }
                }
                None => {
                    let record = HistoryRecord {
                        transaction_id: payment.payment_hash.clone(),
                        wallet: resolved.clone(),
                        network: Network::Ln,
                        direction: if payment.is_outgoing {
                            Direction::Send
                        } else {
                            Direction::Receive
                        },
                        amount: Amount {
                            value: payment.amount_msats / 1000,
                            token: "sats".to_string(),
                        },
                        status,
                        onchain_memo: payment.memo.clone(),
                        local_memo: None,
                        remote_addr: None,
                        preimage: payment.preimage.clone(),
                        created_at_epoch_s: payment.created_at_epoch_s,
                        confirmed_at_epoch_s,
                        fee: None,
                    };
                    let _ = transaction::append_transaction_record(&self.data_dir, &record);
                    stats.records_added = stats.records_added.saturating_add(1);
                }
            }
        }

        Ok(stats)
    }
}

pub(crate) fn parse_bolt11_amount_sats(bolt11: &str) -> Result<u64, PayError> {
    let invoice: lightning_invoice::Bolt11Invoice = bolt11
        .parse()
        .map_err(|e| PayError::InvalidAmount(format!("invalid bolt11 invoice: {e}")))?;
    let amount_msats = invoice.amount_milli_satoshis().ok_or_else(|| {
        PayError::InvalidAmount("bolt11 invoice does not include amount".to_string())
    })?;
    Ok(amount_msats.saturating_add(999) / 1000)
}

pub(crate) fn parse_bolt11_payment_hash(bolt11: &str) -> Result<String, PayError> {
    let invoice: lightning_invoice::Bolt11Invoice = bolt11
        .parse()
        .map_err(|e| PayError::InvalidAmount(format!("invalid bolt11 invoice: {e}")))?;
    Ok(invoice.payment_hash().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_field_detects_wrong_parameter() {
        let err =
            LnProvider::reject_field(LnWalletBackend::Phoenixd, "admin-key-secret", Some("x"))
                .expect_err("phoenixd should reject admin-key-secret");
        assert!(err
            .to_string()
            .contains("does not accept --admin-key-secret"));
    }

    #[test]
    fn parse_bolt11_payment_hash_invalid() {
        assert!(parse_bolt11_payment_hash("not-an-invoice").is_err());
    }
}
