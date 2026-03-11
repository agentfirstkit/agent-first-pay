mod common;
#[cfg(feature = "btc-core")]
mod core_rpc;
#[cfg(feature = "btc-electrum")]
mod electrum;
#[cfg(feature = "btc-esplora")]
mod esplora;

use crate::provider::{HistorySyncStats, PayError, PayProvider};
use crate::store::transaction;
use crate::store::wallet::{self, WalletMetadata};
use crate::types::*;
use async_trait::async_trait;
use bdk_wallet::bitcoin::{Address, Amount as BtcAmount, Transaction, Txid};
use bdk_wallet::chain::{ChainPosition, ConfirmationBlockTime};
use bdk_wallet::keys::bip39::Mnemonic;
use bdk_wallet::{KeychainKind, Wallet};
use common::*;
use std::collections::HashMap;
use std::str::FromStr;

// ═══════════════════════════════════════════
// BtcChainSource trait
// ═══════════════════════════════════════════

#[async_trait]
pub(crate) trait BtcChainSource: Send + Sync {
    /// Sync wallet with revealed SPKs (incremental).
    async fn sync(&self, wallet: &mut Wallet) -> Result<(), PayError>;
    /// Full scan (gap limit based).
    async fn full_scan(&self, wallet: &mut Wallet) -> Result<(), PayError>;
    /// Broadcast a signed transaction.
    async fn broadcast(&self, tx: &Transaction) -> Result<(), PayError>;
}

// ═══════════════════════════════════════════
// Chain source resolver
// ═══════════════════════════════════════════

fn resolve_chain_source(meta: &WalletMetadata) -> Result<Box<dyn BtcChainSource>, PayError> {
    let backend = meta.backend.as_deref();
    match backend {
        #[cfg(feature = "btc-esplora")]
        None | Some("esplora") => Ok(Box::new(esplora::EsploraSource::new(meta))),

        #[cfg(feature = "btc-core")]
        Some("core-rpc") => Ok(Box::new(core_rpc::CoreRpcSource::new(meta)?)),

        #[cfg(feature = "btc-electrum")]
        Some("electrum") => Ok(Box::new(electrum::ElectrumSource::new(meta)?)),

        #[cfg(not(feature = "btc-esplora"))]
        None => Err(PayError::InternalError(
            "no default btc backend available; enable btc-esplora feature".to_string(),
        )),

        Some(other) => Err(PayError::InternalError(format!(
            "unknown btc backend '{other}'; expected: esplora, core-rpc, electrum"
        ))),
    }
}

fn default_btc_backend() -> BtcBackend {
    if cfg!(feature = "btc-esplora") {
        BtcBackend::Esplora
    } else if cfg!(feature = "btc-core") {
        BtcBackend::CoreRpc
    } else {
        BtcBackend::Electrum
    }
}

fn backend_feature_name(backend: BtcBackend) -> &'static str {
    match backend {
        BtcBackend::Esplora => "btc-esplora",
        BtcBackend::CoreRpc => "btc-core",
        BtcBackend::Electrum => "btc-electrum",
    }
}

fn backend_enabled(backend: BtcBackend) -> bool {
    match backend {
        BtcBackend::Esplora => cfg!(feature = "btc-esplora"),
        BtcBackend::CoreRpc => cfg!(feature = "btc-core"),
        BtcBackend::Electrum => cfg!(feature = "btc-electrum"),
    }
}

fn ensure_backend_enabled(backend: BtcBackend) -> Result<(), PayError> {
    if backend_enabled(backend) {
        return Ok(());
    }
    let feature = backend_feature_name(backend);
    Err(PayError::NotImplemented(format!(
        "btc backend '{}' is not enabled in this build; rebuild with --features {feature}",
        backend.as_str()
    )))
}

fn validate_backend_request(
    request: &WalletCreateRequest,
    backend: BtcBackend,
) -> Result<(), PayError> {
    match backend {
        BtcBackend::Esplora => {
            if matches!(request.btc_esplora_url.as_deref(), Some(url) if url.trim().is_empty()) {
                return Err(PayError::InvalidAmount(
                    "btc_esplora_url must not be empty when provided".to_string(),
                ));
            }
        }
        BtcBackend::CoreRpc => {
            if request
                .btc_core_url
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_none()
            {
                return Err(PayError::InvalidAmount(
                    "btc_core_url is required when btc_backend=core-rpc".to_string(),
                ));
            }
        }
        BtcBackend::Electrum => {
            if request
                .btc_electrum_url
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_none()
            {
                return Err(PayError::InvalidAmount(
                    "btc_electrum_url is required when btc_backend=electrum".to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn chain_txid_from_record(record: &HistoryRecord) -> Option<Txid> {
    if let Some(onchain_id) = record.onchain_memo.as_deref() {
        if let Ok(txid) = Txid::from_str(onchain_id) {
            return Some(txid);
        }
    }
    Txid::from_str(&record.transaction_id).ok()
}

fn status_and_confirmations(
    chain_position: ChainPosition<ConfirmationBlockTime>,
    tip_height: u32,
) -> (TxStatus, u32) {
    match chain_position {
        ChainPosition::Confirmed { anchor, .. } => (
            TxStatus::Confirmed,
            tip_height
                .saturating_sub(anchor.block_id.height)
                .saturating_add(1),
        ),
        ChainPosition::Unconfirmed { .. } => (TxStatus::Pending, 0),
    }
}

fn chain_position_epoch_s(chain_position: ChainPosition<ConfirmationBlockTime>) -> u64 {
    match chain_position {
        ChainPosition::Confirmed { anchor, .. } => anchor.confirmation_time,
        ChainPosition::Unconfirmed {
            last_seen,
            first_seen,
        } => last_seen
            .or(first_seen)
            .unwrap_or_else(wallet::now_epoch_seconds),
    }
}

// ═══════════════════════════════════════════
// BtcProvider
// ═══════════════════════════════════════════

pub struct BtcProvider {
    data_dir: String,
}

impl BtcProvider {
    pub fn new(data_dir: &str) -> Self {
        Self {
            data_dir: data_dir.to_string(),
        }
    }

    fn resolve_wallet_id(&self, wallet_id: &str) -> Result<String, PayError> {
        wallet::resolve_wallet_id(&self.data_dir, wallet_id)
    }

    fn load_btc_wallet(&self, wallet_id: &str) -> Result<WalletMetadata, PayError> {
        let id = self.resolve_wallet_id(wallet_id)?;
        let meta = wallet::load_wallet_metadata(&self.data_dir, &id)?;
        if meta.network != Network::Btc {
            return Err(PayError::WalletNotFound(format!(
                "wallet {id} is not a btc wallet"
            )));
        }
        Ok(meta)
    }

    async fn sync_wallet(
        data_dir: &str,
        meta: &WalletMetadata,
        wallet: &mut Wallet,
    ) -> Result<(), PayError> {
        let source = resolve_chain_source(meta)?;
        source.sync(wallet).await?;
        persist_changeset(data_dir, meta, wallet)?;
        Ok(())
    }

    #[allow(dead_code)]
    async fn full_scan_wallet(
        data_dir: &str,
        meta: &WalletMetadata,
        wallet: &mut Wallet,
    ) -> Result<(), PayError> {
        let source = resolve_chain_source(meta)?;
        source.full_scan(wallet).await?;
        persist_changeset(data_dir, meta, wallet)?;
        Ok(())
    }
}

#[async_trait]
impl PayProvider for BtcProvider {
    fn network(&self) -> Network {
        Network::Btc
    }

    fn writes_locally(&self) -> bool {
        true
    }

    async fn create_wallet(&self, request: &WalletCreateRequest) -> Result<WalletInfo, PayError> {
        let is_restore = request.mnemonic_secret.is_some();
        let mnemonic_str = if let Some(ref mnemonic) = request.mnemonic_secret {
            Mnemonic::parse(mnemonic)
                .map_err(|e| PayError::InvalidAmount(format!("invalid mnemonic: {e}")))?;
            mnemonic.clone()
        } else {
            let mut entropy = [0u8; 16];
            getrandom::fill(&mut entropy)
                .map_err(|e| PayError::InternalError(format!("rng failed: {e}")))?;
            let mnemonic = Mnemonic::from_entropy(&entropy)
                .map_err(|e| PayError::InternalError(format!("mnemonic gen: {e}")))?;
            mnemonic.to_string()
        };

        let btc_network_str = request
            .btc_network
            .as_deref()
            .unwrap_or("mainnet")
            .to_string();
        let btc_address_type = request
            .btc_address_type
            .as_deref()
            .unwrap_or("taproot")
            .to_string();

        if !["mainnet", "signet"].contains(&btc_network_str.as_str()) {
            return Err(PayError::InvalidAmount(format!(
                "unsupported btc_network '{btc_network_str}'; expected: mainnet, signet"
            )));
        }
        if !["taproot", "segwit"].contains(&btc_address_type.as_str()) {
            return Err(PayError::InvalidAmount(format!(
                "unsupported btc_address_type '{btc_address_type}'; expected: taproot, segwit"
            )));
        }

        let btc_backend = request.btc_backend.unwrap_or_else(default_btc_backend);
        ensure_backend_enabled(btc_backend)?;
        validate_backend_request(request, btc_backend)?;

        let wallet_id = wallet::generate_wallet_identifier()?;
        let normalized_label = {
            let trimmed = request.label.trim();
            if trimmed.is_empty() || trimmed == "default" {
                None
            } else {
                Some(trimmed.to_string())
            }
        };

        let meta = WalletMetadata {
            id: wallet_id.clone(),
            network: Network::Btc,
            label: normalized_label.clone(),
            mint_url: None,
            sol_rpc_endpoints: None,
            evm_rpc_endpoints: None,
            evm_chain_id: None,
            seed_secret: Some(mnemonic_str.clone()),
            backend: Some(btc_backend.as_str().to_string()),
            btc_esplora_url: request.btc_esplora_url.clone(),
            btc_network: Some(btc_network_str),
            btc_address_type: Some(btc_address_type),
            btc_core_url: request.btc_core_url.clone(),
            btc_core_auth_secret: request.btc_core_auth_secret.clone(),
            btc_electrum_url: request.btc_electrum_url.clone(),
            custom_tokens: None,
            created_at_epoch_s: wallet::now_epoch_seconds(),
            error: None,
        };

        let address = wallet_address(&meta)?;

        wallet::save_wallet_metadata(&self.data_dir, &meta)?;

        let mut bdk_wallet = open_bdk_wallet_with_dir(&self.data_dir, &meta)?;
        let _ = bdk_wallet.reveal_addresses_to(KeychainKind::External, 0);
        persist_changeset(&self.data_dir, &meta, &mut bdk_wallet)?;

        if is_restore {
            if let Err(e) = Self::full_scan_wallet(&self.data_dir, &meta, &mut bdk_wallet).await {
                let _ = wallet::delete_wallet_metadata(&self.data_dir, &wallet_id);
                return Err(e);
            }
        }

        Ok(WalletInfo {
            id: wallet_id,
            network: Network::Btc,
            address,
            label: normalized_label,
            mnemonic: Some(mnemonic_str),
        })
    }

    async fn close_wallet(&self, wallet_id: &str) -> Result<(), PayError> {
        let id = self.resolve_wallet_id(wallet_id)?;
        let meta = self.load_btc_wallet(&id)?;

        let mut bdk_wallet = open_bdk_wallet_with_dir(&self.data_dir, &meta)?;
        Self::sync_wallet(&self.data_dir, &meta, &mut bdk_wallet).await?;
        let balance = bdk_wallet.balance();
        let total = balance.total().to_sat();
        if total > 0 {
            return Err(PayError::InvalidAmount(format!(
                "wallet {id} has {total} sats remaining; transfer funds before closing, \
                 or use --dangerously-skip-balance-check-and-may-lose-money"
            )));
        }

        wallet::delete_wallet_metadata(&self.data_dir, &id)?;
        Ok(())
    }

    async fn list_wallets(&self) -> Result<Vec<WalletSummary>, PayError> {
        let metas = wallet::list_wallet_metadata(&self.data_dir, Some(Network::Btc))?;
        let mut summaries = Vec::with_capacity(metas.len());
        for meta in metas {
            let address = wallet_address(&meta).unwrap_or_else(|_| "error".to_string());
            summaries.push(btc_wallet_summary(meta, address));
        }
        Ok(summaries)
    }

    async fn balance(&self, wallet_id: &str) -> Result<BalanceInfo, PayError> {
        let id = self.resolve_wallet_id(wallet_id)?;
        let meta = self.load_btc_wallet(&id)?;
        let mut bdk_wallet = open_bdk_wallet_with_dir(&self.data_dir, &meta)?;
        Self::sync_wallet(&self.data_dir, &meta, &mut bdk_wallet).await?;
        let balance = bdk_wallet.balance();
        Ok(BalanceInfo::new(
            balance.confirmed.to_sat(),
            balance.trusted_pending.to_sat() + balance.untrusted_pending.to_sat(),
            "sats",
        ))
    }

    async fn check_balance(&self, wallet_id: &str) -> Result<BalanceInfo, PayError> {
        self.balance(wallet_id).await
    }

    async fn balance_all(&self) -> Result<Vec<WalletBalanceItem>, PayError> {
        let wallets = self.list_wallets().await?;
        let mut items = Vec::with_capacity(wallets.len());
        for ws in wallets {
            match self.balance(&ws.id).await {
                Ok(bal) => items.push(WalletBalanceItem {
                    wallet: ws,
                    balance: Some(bal),
                    error: None,
                }),
                Err(e) => items.push(WalletBalanceItem {
                    wallet: ws,
                    balance: None,
                    error: Some(e.to_string()),
                }),
            }
        }
        Ok(items)
    }

    async fn receive_info(
        &self,
        wallet_id: &str,
        _amount: Option<Amount>,
    ) -> Result<ReceiveInfo, PayError> {
        let id = self.resolve_wallet_id(wallet_id)?;
        let meta = self.load_btc_wallet(&id)?;
        let mut bdk_wallet = open_bdk_wallet_with_dir(&self.data_dir, &meta)?;
        let addr_info = bdk_wallet.next_unused_address(KeychainKind::External);
        persist_changeset(&self.data_dir, &meta, &mut bdk_wallet)?;
        Ok(ReceiveInfo {
            address: Some(addr_info.address.to_string()),
            invoice: None,
            quote_id: None,
        })
    }

    async fn receive_claim(&self, _wallet: &str, _quote_id: &str) -> Result<u64, PayError> {
        Err(PayError::NotImplemented(
            "btc does not use receive_claim; on-chain transactions are automatic".to_string(),
        ))
    }

    async fn cashu_send(
        &self,
        _wallet: &str,
        _amount: Amount,
        _onchain_memo: Option<&str>,
        _mints: Option<&[String]>,
    ) -> Result<CashuSendResult, PayError> {
        Err(PayError::NotImplemented(
            "cashu_send not supported for btc".to_string(),
        ))
    }

    async fn cashu_receive(
        &self,
        _wallet: &str,
        _token: &str,
    ) -> Result<CashuReceiveResult, PayError> {
        Err(PayError::NotImplemented(
            "cashu_receive not supported for btc".to_string(),
        ))
    }

    async fn send(
        &self,
        wallet_id: &str,
        to: &str,
        _onchain_memo: Option<&str>,
        _mints: Option<&[String]>,
    ) -> Result<SendResult, PayError> {
        let id = self.resolve_wallet_id(wallet_id)?;
        let meta = self.load_btc_wallet(&id)?;
        let target = parse_transfer_target(to)?;
        if target.amount_sats == 0 {
            return Err(PayError::InvalidAmount(
                "amount must be greater than 0 sats".to_string(),
            ));
        }

        let btc_net = btc_network_for_meta(&meta);
        let recipient = Address::from_str(&target.address)
            .map_err(|e| PayError::InvalidAmount(format!("invalid btc address: {e}")))?
            .require_network(btc_net)
            .map_err(|e| PayError::InvalidAmount(format!("address network mismatch: {e}")))?;

        let mut bdk_wallet = open_bdk_wallet_with_dir(&self.data_dir, &meta)?;

        Self::sync_wallet(&self.data_dir, &meta, &mut bdk_wallet).await?;

        let mut tx_builder = bdk_wallet.build_tx();
        tx_builder.add_recipient(
            recipient.script_pubkey(),
            BtcAmount::from_sat(target.amount_sats),
        );

        let mut psbt = tx_builder
            .finish()
            .map_err(|e| PayError::InternalError(format!("build tx: {e}")))?;

        #[allow(deprecated)]
        let finalized = bdk_wallet
            .sign(&mut psbt, bdk_wallet::SignOptions::default())
            .map_err(|e| PayError::InternalError(format!("sign tx: {e}")))?;

        if !finalized {
            return Err(PayError::InternalError(
                "transaction signing did not finalize".to_string(),
            ));
        }

        let tx = psbt
            .extract_tx()
            .map_err(|e| PayError::InternalError(format!("extract tx: {e}")))?;
        let txid = tx.compute_txid().to_string();

        // Broadcast via resolved chain source
        let source = resolve_chain_source(&meta)?;
        source.broadcast(&tx).await?;

        persist_changeset(&self.data_dir, &meta, &mut bdk_wallet)?;

        let tx_id = wallet::generate_transaction_identifier()?;
        let fee_amount = bdk_wallet.calculate_fee(&tx).map(|f| f.to_sat()).ok();

        let record = HistoryRecord {
            transaction_id: tx_id.clone(),
            wallet: id.clone(),
            network: Network::Btc,
            direction: Direction::Send,
            amount: Amount {
                value: target.amount_sats,
                token: "sats".to_string(),
            },
            status: TxStatus::Pending,
            onchain_memo: Some(txid.clone()),
            local_memo: None,
            remote_addr: Some(target.address),
            preimage: None,
            created_at_epoch_s: wallet::now_epoch_seconds(),
            confirmed_at_epoch_s: None,
            fee: fee_amount.map(|f| Amount {
                value: f,
                token: "sats".to_string(),
            }),
        };

        let _ = transaction::append_transaction_record(&self.data_dir, &record);

        Ok(SendResult {
            wallet: id,
            transaction_id: tx_id,
            amount: Amount {
                value: target.amount_sats,
                token: "sats".to_string(),
            },
            fee: fee_amount.map(|f| Amount {
                value: f,
                token: "sats".to_string(),
            }),
            preimage: None,
        })
    }

    async fn send_quote(
        &self,
        wallet_id: &str,
        to: &str,
        _mints: Option<&[String]>,
    ) -> Result<SendQuoteInfo, PayError> {
        let id = self.resolve_wallet_id(wallet_id)?;
        let meta = self.load_btc_wallet(&id)?;
        let target = parse_transfer_target(to)?;
        if target.amount_sats == 0 {
            return Err(PayError::InvalidAmount(
                "amount must be greater than 0 sats".to_string(),
            ));
        }

        let btc_net = btc_network_for_meta(&meta);
        let recipient = Address::from_str(&target.address)
            .map_err(|e| PayError::InvalidAmount(format!("invalid btc address: {e}")))?
            .require_network(btc_net)
            .map_err(|e| PayError::InvalidAmount(format!("address network mismatch: {e}")))?;

        let mut bdk_wallet = open_bdk_wallet_with_dir(&self.data_dir, &meta)?;
        Self::sync_wallet(&self.data_dir, &meta, &mut bdk_wallet).await?;

        let mut tx_builder = bdk_wallet.build_tx();
        tx_builder.add_recipient(
            recipient.script_pubkey(),
            BtcAmount::from_sat(target.amount_sats),
        );

        let mut psbt = tx_builder
            .finish()
            .map_err(|e| PayError::InternalError(format!("build tx quote: {e}")))?;

        #[allow(deprecated)]
        let finalized = bdk_wallet
            .sign(&mut psbt, bdk_wallet::SignOptions::default())
            .map_err(|e| PayError::InternalError(format!("sign tx quote: {e}")))?;
        if !finalized {
            return Err(PayError::InternalError(
                "transaction quote signing did not finalize".to_string(),
            ));
        }

        let tx = psbt
            .extract_tx()
            .map_err(|e| PayError::InternalError(format!("extract tx quote: {e}")))?;
        let fee_estimate_native = bdk_wallet
            .calculate_fee(&tx)
            .map(|fee| fee.to_sat())
            .unwrap_or(0);
        persist_changeset(&self.data_dir, &meta, &mut bdk_wallet)?;

        Ok(SendQuoteInfo {
            wallet: id,
            amount_native: target.amount_sats,
            fee_estimate_native,
            fee_unit: "sats".to_string(),
        })
    }

    async fn history_list(
        &self,
        wallet_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<HistoryRecord>, PayError> {
        let id = self.resolve_wallet_id(wallet_id)?;
        let _meta = self.load_btc_wallet(&id)?;
        let all = transaction::load_wallet_transaction_records(&self.data_dir, &id)?;
        let end = all.len().min(offset + limit);
        let start = all.len().min(offset);
        Ok(all[start..end].to_vec())
    }

    async fn history_status(&self, transaction_id: &str) -> Result<HistoryStatusInfo, PayError> {
        match transaction::find_transaction_record_by_id(&self.data_dir, transaction_id)? {
            Some(mut rec) => {
                if let Some(chain_txid) = chain_txid_from_record(&rec) {
                    if let Ok(meta) = self.load_btc_wallet(&rec.wallet) {
                        let mut bdk_wallet = open_bdk_wallet_with_dir(&self.data_dir, &meta)?;
                        Self::sync_wallet(&self.data_dir, &meta, &mut bdk_wallet).await?;
                        if let Some(wallet_tx) = bdk_wallet.get_tx(chain_txid) {
                            let tip_height = bdk_wallet.latest_checkpoint().height();
                            let (status, confirmations) =
                                status_and_confirmations(wallet_tx.chain_position, tip_height);
                            let confirmed_at_epoch_s = if status == TxStatus::Confirmed {
                                Some(
                                    rec.confirmed_at_epoch_s
                                        .unwrap_or_else(wallet::now_epoch_seconds),
                                )
                            } else {
                                None
                            };

                            if rec.status != status
                                || rec.confirmed_at_epoch_s != confirmed_at_epoch_s
                            {
                                let _ = transaction::update_transaction_record_status(
                                    &self.data_dir,
                                    &rec.transaction_id,
                                    status,
                                    confirmed_at_epoch_s,
                                );
                                rec.status = status;
                                rec.confirmed_at_epoch_s = confirmed_at_epoch_s;
                            }

                            return Ok(HistoryStatusInfo {
                                transaction_id: rec.transaction_id.clone(),
                                status: rec.status,
                                confirmations: Some(confirmations),
                                preimage: rec.preimage.clone(),
                                item: Some(rec),
                            });
                        }
                    }
                }

                Ok(HistoryStatusInfo {
                    transaction_id: rec.transaction_id.clone(),
                    status: rec.status,
                    confirmations: None,
                    preimage: rec.preimage.clone(),
                    item: Some(rec),
                })
            }
            None => Err(PayError::WalletNotFound(format!(
                "transaction {transaction_id} not found"
            ))),
        }
    }

    async fn history_sync(
        &self,
        wallet_id: &str,
        limit: usize,
    ) -> Result<HistorySyncStats, PayError> {
        let id = self.resolve_wallet_id(wallet_id)?;
        let meta = self.load_btc_wallet(&id)?;
        let mut bdk_wallet = open_bdk_wallet_with_dir(&self.data_dir, &meta)?;
        Self::sync_wallet(&self.data_dir, &meta, &mut bdk_wallet).await?;

        let local_records = transaction::load_wallet_transaction_records(&self.data_dir, &id)?;
        let mut local_by_chain_txid: HashMap<String, HistoryRecord> = HashMap::new();
        for record in local_records {
            if record.network != Network::Btc {
                continue;
            }
            if let Some(chain_txid) = chain_txid_from_record(&record) {
                local_by_chain_txid.insert(chain_txid.to_string(), record);
            }
        }

        let mut wallet_txs: Vec<_> = bdk_wallet.transactions().collect();
        wallet_txs.sort_by(|a, b| {
            let b_ts = chain_position_epoch_s(b.chain_position);
            let a_ts = chain_position_epoch_s(a.chain_position);
            b_ts.cmp(&a_ts)
        });

        let mut stats = HistorySyncStats::default();
        let scan_limit = limit.max(1);
        let tip_height = bdk_wallet.latest_checkpoint().height();
        for wallet_tx in wallet_txs.into_iter().take(scan_limit) {
            stats.records_scanned = stats.records_scanned.saturating_add(1);
            let chain_txid = wallet_tx.tx_node.txid.to_string();
            let (status, _confirmations) =
                status_and_confirmations(wallet_tx.chain_position, tip_height);
            let created_at_epoch_s = chain_position_epoch_s(wallet_tx.chain_position);
            let confirmed_at_epoch_s = if status == TxStatus::Confirmed {
                Some(created_at_epoch_s)
            } else {
                None
            };

            if let Some(existing) = local_by_chain_txid.get(&chain_txid) {
                if existing.status != status
                    || existing.confirmed_at_epoch_s != confirmed_at_epoch_s
                {
                    let _ = transaction::update_transaction_record_status(
                        &self.data_dir,
                        &existing.transaction_id,
                        status,
                        confirmed_at_epoch_s,
                    );
                    stats.records_updated = stats.records_updated.saturating_add(1);
                }
                continue;
            }

            let tx = &wallet_tx.tx_node.tx;
            let (sent, received) = bdk_wallet.sent_and_received(tx);
            let sent_sats = sent.to_sat();
            let received_sats = received.to_sat();
            let (direction, amount_sats) = if received_sats >= sent_sats {
                (Direction::Receive, received_sats.saturating_sub(sent_sats))
            } else {
                (Direction::Send, sent_sats.saturating_sub(received_sats))
            };
            if amount_sats == 0 {
                continue;
            }

            let fee = bdk_wallet.calculate_fee(tx).map(|f| f.to_sat()).ok();
            let record = HistoryRecord {
                transaction_id: chain_txid.clone(),
                wallet: id.clone(),
                network: Network::Btc,
                direction,
                amount: Amount {
                    value: amount_sats,
                    token: "sats".to_string(),
                },
                status,
                onchain_memo: Some(chain_txid.clone()),
                local_memo: None,
                remote_addr: None,
                preimage: None,
                created_at_epoch_s,
                confirmed_at_epoch_s,
                fee: fee.map(|value| Amount {
                    value,
                    token: "sats".to_string(),
                }),
            };
            let _ = transaction::append_transaction_record(&self.data_dir, &record);
            local_by_chain_txid.insert(chain_txid, record);
            stats.records_added = stats.records_added.saturating_add(1);
        }

        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::common::*;
    use super::BtcProvider;
    use crate::provider::{PayError, PayProvider};
    use crate::types::{BtcBackend, WalletCreateRequest};
    use bdk_wallet::bitcoin::Network as BtcNetwork;

    #[test]
    fn parse_transfer_target_bitcoin_uri() {
        let target = parse_transfer_target("bitcoin:bc1qtest123?amount=50000").unwrap();
        assert_eq!(target.address, "bc1qtest123");
        assert_eq!(target.amount_sats, 50000);
    }

    #[test]
    fn parse_transfer_target_bare_address() {
        let target = parse_transfer_target("bc1qtest123?amount=1000").unwrap();
        assert_eq!(target.address, "bc1qtest123");
        assert_eq!(target.amount_sats, 1000);
    }

    #[test]
    fn parse_transfer_target_no_amount_fails() {
        let result = parse_transfer_target("bc1qtest123");
        assert!(result.is_err());
    }

    #[test]
    fn descriptors_from_mnemonic_taproot() {
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let (external, internal) =
            descriptors_from_mnemonic(mnemonic, BtcNetwork::Bitcoin, "taproot").unwrap();
        assert!(external.starts_with("tr("));
        assert!(external.contains("/86'/0'/0'/0/*)"));
        assert!(internal.starts_with("tr("));
        assert!(internal.contains("/86'/0'/0'/1/*)"));
    }

    #[test]
    fn descriptors_from_mnemonic_segwit() {
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let (external, internal) =
            descriptors_from_mnemonic(mnemonic, BtcNetwork::Bitcoin, "segwit").unwrap();
        assert!(external.starts_with("wpkh("));
        assert!(external.contains("/84'/0'/0'/0/*)"));
        assert!(internal.starts_with("wpkh("));
        assert!(internal.contains("/84'/0'/0'/1/*)"));
    }

    #[test]
    fn descriptors_from_mnemonic_signet_coin_type() {
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let (external, _) =
            descriptors_from_mnemonic(mnemonic, BtcNetwork::Signet, "taproot").unwrap();
        assert!(
            external.contains("/86'/1'/0'/0/*)"),
            "signet should use coin_type=1"
        );
    }

    fn signet_request(label: &str) -> WalletCreateRequest {
        WalletCreateRequest {
            label: label.to_string(),
            mint_url: None,
            rpc_endpoints: vec![],
            chain_id: None,
            mnemonic_secret: None,
            btc_esplora_url: None,
            btc_network: Some("signet".to_string()),
            btc_address_type: Some("taproot".to_string()),
            btc_backend: Some(BtcBackend::Esplora),
            btc_core_url: None,
            btc_core_auth_secret: None,
            btc_electrum_url: None,
        }
    }

    #[tokio::test]
    async fn create_wallet_rejects_empty_esplora_url() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_str().unwrap();
        let provider = BtcProvider::new(data_dir);
        let mut req = signet_request("bad-esplora");
        req.btc_esplora_url = Some("   ".to_string());

        let err = provider.create_wallet(&req).await.unwrap_err();
        assert!(
            matches!(err, PayError::InvalidAmount(_)),
            "expected InvalidAmount, got: {err}"
        );
    }

    #[cfg(not(feature = "btc-core"))]
    #[tokio::test]
    async fn create_wallet_rejects_core_rpc_when_feature_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_str().unwrap();
        let provider = BtcProvider::new(data_dir);
        let mut req = signet_request("core-disabled");
        req.btc_backend = Some(BtcBackend::CoreRpc);
        req.btc_core_url = Some("http://127.0.0.1:18443".to_string());

        let err = provider.create_wallet(&req).await.unwrap_err();
        assert!(
            matches!(err, PayError::NotImplemented(_)),
            "expected NotImplemented, got: {err}"
        );
    }

    #[cfg(feature = "btc-core")]
    #[tokio::test]
    async fn create_wallet_core_rpc_requires_url() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_str().unwrap();
        let provider = BtcProvider::new(data_dir);
        let mut req = signet_request("core-needs-url");
        req.btc_backend = Some(BtcBackend::CoreRpc);
        req.btc_core_url = None;

        let err = provider.create_wallet(&req).await.unwrap_err();
        assert!(
            matches!(err, PayError::InvalidAmount(_)),
            "expected InvalidAmount, got: {err}"
        );
    }

    #[cfg(feature = "btc-electrum")]
    #[tokio::test]
    async fn create_wallet_electrum_requires_url() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_str().unwrap();
        let provider = BtcProvider::new(data_dir);
        let mut req = signet_request("electrum-needs-url");
        req.btc_backend = Some(BtcBackend::Electrum);
        req.btc_electrum_url = None;

        let err = provider.create_wallet(&req).await.unwrap_err();
        assert!(
            matches!(err, PayError::InvalidAmount(_)),
            "expected InvalidAmount, got: {err}"
        );
    }

    #[tokio::test]
    async fn send_quote_rejects_invalid_address() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_str().unwrap();
        let provider = BtcProvider::new(data_dir);
        let wallet = provider
            .create_wallet(&signet_request("send-quote-invalid"))
            .await
            .unwrap();

        let err = provider
            .send_quote(&wallet.id, "bitcoin:not-a-btc-address?amount=1000", None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, PayError::InvalidAmount(_)),
            "expected InvalidAmount, got: {err}"
        );
    }

    #[cfg(feature = "btc-esplora")]
    #[tokio::test]
    async fn restore_wallet_runs_full_scan_and_cleans_up_on_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_str().unwrap();
        let provider = BtcProvider::new(data_dir);
        let mut req = signet_request("restore-full-scan");
        req.mnemonic_secret = Some(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
                .to_string(),
        );
        // Force full_scan to fail fast so we can assert restore path is exercised.
        req.btc_esplora_url = Some("http://127.0.0.1:1".to_string());

        let err = provider.create_wallet(&req).await.unwrap_err();
        assert!(
            matches!(err, PayError::NetworkError(_)),
            "expected NetworkError from full_scan, got: {err}"
        );
        let wallets = provider.list_wallets().await.unwrap();
        assert!(wallets.is_empty(), "failed restore should cleanup wallet");
    }

    #[cfg(feature = "btc-esplora")]
    #[tokio::test]
    async fn non_restore_create_skips_full_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_str().unwrap();
        let provider = BtcProvider::new(data_dir);
        let mut req = signet_request("create-no-fullscan");
        req.btc_esplora_url = Some("http://127.0.0.1:1".to_string());

        let wallet = provider.create_wallet(&req).await.unwrap();
        assert!(wallet.id.starts_with("w_"));
    }
}
