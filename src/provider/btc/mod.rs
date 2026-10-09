mod common;
#[cfg(feature = "btc-core")]
mod core_rpc;
#[cfg(feature = "btc-electrum")]
mod electrum;
#[cfg(feature = "btc-esplora")]
mod esplora;

use crate::provider::{HistorySyncStats, PayError, PayProvider};
use crate::store::wallet::{self, WalletMetadata};
use crate::store::{PayStore, StorageBackend};
use crate::types::*;
use async_trait::async_trait;
use bdk_wallet::bitcoin::{Address, Amount as BtcAmount, Transaction, Txid};
use bdk_wallet::chain::{ChainPosition, ConfirmationBlockTime};
use bdk_wallet::keys::bip39::Mnemonic;
use bdk_wallet::{KeychainKind, Wallet};
use common::*;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

// ═══════════════════════════════════════════
// BtcChainSource trait
// ═══════════════════════════════════════════

#[async_trait]
pub(crate) trait BtcChainSource: Send + Sync {
    /// Sync wallet with revealed SPKs (incremental).
    async fn sync(&self, wallet: &mut Wallet) -> Result<(), PayError>;
    /// Full scan (gap limit based).
    async fn full_scan(&self, wallet: &mut Wallet) -> Result<(), PayError>;
    /// Estimate the fee in sat/vB for a six-block confirmation target.
    async fn fee_rate_sat_per_vb(&self) -> Result<f64, PayError>;
    /// Broadcast a signed transaction.
    async fn broadcast(&self, tx: &Transaction) -> Result<(), PayError>;
}

/// Safety ceiling on a chain-source fee estimate, not a fee policy: it exists so
/// a faulty or hijacked chain source cannot make a payment burn its inputs as
/// fees. An estimate above it is treated like any other invalid estimate, which
/// happens before signing, so nothing is broadcast.
const MAX_FEE_RATE_SAT_PER_VB: f64 = 1000.0;

fn fee_rate_from_sat_per_vb(
    rate_sat_per_vb: f64,
) -> Result<bdk_wallet::bitcoin::FeeRate, PayError> {
    let rate_sat_per_kwu = (rate_sat_per_vb * 250.0).ceil();
    if !rate_sat_per_vb.is_finite()
        || rate_sat_per_vb <= 0.0
        || rate_sat_per_vb > MAX_FEE_RATE_SAT_PER_VB
        || !rate_sat_per_kwu.is_finite()
        || rate_sat_per_kwu >= u64::MAX as f64
    {
        return Err(PayError::network_error("invalid Bitcoin fee estimate"));
    }
    Ok(bdk_wallet::bitcoin::FeeRate::from_sat_per_kwu(
        rate_sat_per_kwu as u64,
    ))
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
        None => Err(PayError::internal_error(
            "no default btc backend available; enable btc-esplora feature".to_string(),
        )),

        Some(other) => Err(PayError::internal_error(format!(
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
    Err(PayError::not_implemented(format!(
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
                return Err(PayError::invalid_amount(
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
                return Err(PayError::invalid_amount(
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
                return Err(PayError::invalid_amount(
                    "btc_electrum_url is required when btc_backend=electrum".to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn chain_txid_from_record(record: &HistoryRecord) -> Option<Txid> {
    if let Some(onchain_id) = record.onchain_memo.as_deref()
        && let Ok(txid) = Txid::from_str(onchain_id)
    {
        return Some(txid);
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
    store: Arc<StorageBackend>,
    #[cfg(test)]
    chain_source_override: Option<Arc<dyn BtcChainSource>>,
}

impl BtcProvider {
    pub fn new(data_dir: &str, store: Arc<StorageBackend>) -> Self {
        Self {
            data_dir: data_dir.to_string(),
            store,
            #[cfg(test)]
            chain_source_override: None,
        }
    }

    fn resolve_wallet_id(&self, wallet_id: &str) -> Result<String, PayError> {
        self.store.resolve_wallet_id(wallet_id)
    }

    fn load_btc_wallet(&self, wallet_id: &str) -> Result<WalletMetadata, PayError> {
        let id = self.resolve_wallet_id(wallet_id)?;
        let meta = self.store.load_wallet_metadata(&id)?;
        if meta.network != Network::Btc {
            return Err(PayError::wallet_not_found(format!(
                "wallet {id} is not a btc wallet"
            )));
        }
        Ok(meta)
    }

    fn chain_source(&self, meta: &WalletMetadata) -> Result<Arc<dyn BtcChainSource>, PayError> {
        #[cfg(test)]
        if let Some(source) = &self.chain_source_override {
            return Ok(source.clone());
        }
        resolve_chain_source(meta).map(Arc::from)
    }

    async fn sync_wallet(
        &self,
        meta: &WalletMetadata,
        wallet: &mut Wallet,
    ) -> Result<(), PayError> {
        let source = self.chain_source(meta)?;
        source.sync(wallet).await?;
        persist_changeset(&self.data_dir, meta, wallet)?;
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
                .map_err(|e| PayError::invalid_amount(format!("invalid mnemonic: {e}")))?;
            mnemonic.clone()
        } else {
            let mut entropy = [0u8; 16];
            getrandom::fill(&mut entropy)
                .map_err(|e| PayError::internal_error(format!("rng failed: {e}")))?;
            let mnemonic = Mnemonic::from_entropy(&entropy)
                .map_err(|e| PayError::internal_error(format!("mnemonic gen: {e}")))?;
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
            return Err(PayError::invalid_amount(format!(
                "unsupported btc_network '{btc_network_str}'; expected: mainnet, signet"
            )));
        }
        if !["taproot", "segwit"].contains(&btc_address_type.as_str()) {
            return Err(PayError::invalid_amount(format!(
                "unsupported btc_address_type '{btc_address_type}'; expected: taproot, segwit"
            )));
        }

        let btc_backend = request.btc_backend.unwrap_or_else(default_btc_backend);
        ensure_backend_enabled(btc_backend)?;
        validate_backend_request(request, btc_backend)?;

        // Derive a stable wallet_id from (network, btc_network, address_type,
        // mnemonic). The same mnemonic on mainnet vs signet, or as taproot vs
        // segwit, lives in distinct id-spaces; re-running create with identical
        // parameters always hits the load_wallet_metadata idempotency path.
        let wallet_id = wallet::derive_wallet_identifier(&[
            b"btc",
            btc_network_str.as_bytes(),
            btc_address_type.as_bytes(),
            mnemonic_str.trim().as_bytes(),
        ]);
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
            sol_cluster: None,
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

        self.store.save_wallet_metadata(&meta)?;

        let mut bdk_wallet = open_bdk_wallet_with_dir(&self.data_dir, &meta)?;
        let _ = bdk_wallet.reveal_addresses_to(KeychainKind::External, 0);
        persist_changeset(&self.data_dir, &meta, &mut bdk_wallet)?;

        if is_restore
            && let Err(e) = Self::full_scan_wallet(&self.data_dir, &meta, &mut bdk_wallet).await
        {
            let _ = self.store.delete_wallet_metadata(&wallet_id);
            return Err(e);
        }

        Ok(WalletInfo {
            id: wallet_id,
            network: Network::Btc,
            address,
            label: normalized_label,
            mnemonic_secret: Some(mnemonic_str),
        })
    }

    async fn close_wallet(&self, wallet_id: &str) -> Result<(), PayError> {
        let id = self.resolve_wallet_id(wallet_id)?;
        let meta = self.load_btc_wallet(&id)?;

        let mut bdk_wallet = open_bdk_wallet_with_dir(&self.data_dir, &meta)?;
        self.sync_wallet(&meta, &mut bdk_wallet).await?;
        let balance = bdk_wallet.balance();
        let total = balance.total().to_sat();
        if total > 0 {
            return Err(PayError::invalid_amount(format!(
                "wallet {id} has {total} sats remaining; transfer funds before closing, \
                 or use --dangerously-skip-balance-check-and-may-lose-money"
            )));
        }

        self.store.delete_wallet_metadata(&id)?;
        Ok(())
    }

    async fn list_wallets(&self) -> Result<Vec<WalletSummary>, PayError> {
        let metas = self.store.list_wallet_metadata(Some(Network::Btc))?;
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
        self.sync_wallet(&meta, &mut bdk_wallet).await?;
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
        Err(PayError::not_implemented(
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
        Err(PayError::not_implemented(
            "cashu_send not supported for btc".to_string(),
        ))
    }

    async fn cashu_receive(
        &self,
        _wallet: &str,
        _token: &str,
    ) -> Result<CashuReceiveResult, PayError> {
        Err(PayError::not_implemented(
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
            return Err(PayError::invalid_amount(
                "amount must be greater than 0 sats".to_string(),
            ));
        }

        let btc_net = btc_network_for_meta(&meta);
        let recipient = Address::from_str(&target.address)
            .map_err(|e| PayError::invalid_amount(format!("invalid btc address: {e}")))?
            .require_network(btc_net)
            .map_err(|e| PayError::invalid_amount(format!("address network mismatch: {e}")))?;

        let mut bdk_wallet = open_bdk_wallet_with_dir(&self.data_dir, &meta)?;

        self.sync_wallet(&meta, &mut bdk_wallet)
            .await
            .map_err(|error| match error {
                PayError::NetworkError { message, .. } => PayError::network_not_submitted(message),
                error => error,
            })?;

        let source = self.chain_source(&meta)?;
        let fee_rate = source
            .fee_rate_sat_per_vb()
            .await
            .and_then(fee_rate_from_sat_per_vb)
            .map_err(|error| match error {
                PayError::NetworkError { message, .. } => PayError::network_not_submitted(message),
                error => error,
            })?;
        let mut tx_builder = bdk_wallet.build_tx();
        tx_builder.fee_rate(fee_rate);
        tx_builder.add_recipient(
            recipient.script_pubkey(),
            BtcAmount::from_sat(target.amount_sats),
        );

        let mut psbt = tx_builder
            .finish()
            .map_err(|e| PayError::internal_error(format!("build tx: {e}")))?;

        let [external_signers, internal_signers] = signers_for_wallet(&meta, &bdk_wallet)?;
        let finalized = bdk_wallet
            .sign_with_signers(
                &mut psbt,
                &[&external_signers, &internal_signers],
                bdk_wallet::SignOptions::default(),
            )
            .map_err(|e| PayError::internal_error(format!("sign tx: {e}")))?;

        if !finalized {
            return Err(PayError::internal_error(
                "transaction signing did not finalize".to_string(),
            ));
        }

        let tx = psbt
            .extract_tx()
            .map_err(|e| PayError::internal_error(format!("extract tx: {e}")))?;
        let txid = tx.compute_txid().to_string();
        let tx_id = wallet::generate_transaction_identifier()?;

        // Broadcast via resolved chain source
        let source = self.chain_source(&meta)?;
        source.broadcast(&tx).await.map_err(|error| {
            PayError::submission_unknown(
                format!("bitcoin broadcast outcome is unknown: {error}"),
                Some(txid.clone()),
            )
        })?;

        persist_changeset(&self.data_dir, &meta, &mut bdk_wallet).map_err(|error| {
            PayError::submission_unknown(
                format!("bitcoin transaction was broadcast but wallet persistence failed: {error}"),
                Some(txid.clone()),
            )
        })?;

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
            reference_keys: None,
            reservation_ids: Vec::new(),
        };

        let _ = self.store.append_transaction_record(&record);

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
            idempotency_finalize_failed: false,
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
            return Err(PayError::invalid_amount(
                "amount must be greater than 0 sats".to_string(),
            ));
        }

        let btc_net = btc_network_for_meta(&meta);
        let recipient = Address::from_str(&target.address)
            .map_err(|e| PayError::invalid_amount(format!("invalid btc address: {e}")))?
            .require_network(btc_net)
            .map_err(|e| PayError::invalid_amount(format!("address network mismatch: {e}")))?;

        let mut bdk_wallet = open_bdk_wallet_with_dir(&self.data_dir, &meta)?;
        self.sync_wallet(&meta, &mut bdk_wallet).await?;

        let source = self.chain_source(&meta)?;
        let fee_rate = source
            .fee_rate_sat_per_vb()
            .await
            .and_then(fee_rate_from_sat_per_vb)?;
        let mut tx_builder = bdk_wallet.build_tx();
        tx_builder.fee_rate(fee_rate);
        tx_builder.add_recipient(
            recipient.script_pubkey(),
            BtcAmount::from_sat(target.amount_sats),
        );

        let mut psbt = tx_builder
            .finish()
            .map_err(|e| PayError::internal_error(format!("build tx quote: {e}")))?;

        let [external_signers, internal_signers] = signers_for_wallet(&meta, &bdk_wallet)?;
        let finalized = bdk_wallet
            .sign_with_signers(
                &mut psbt,
                &[&external_signers, &internal_signers],
                bdk_wallet::SignOptions::default(),
            )
            .map_err(|e| PayError::internal_error(format!("sign tx quote: {e}")))?;
        if !finalized {
            return Err(PayError::internal_error(
                "transaction quote signing did not finalize".to_string(),
            ));
        }

        let tx = psbt
            .extract_tx()
            .map_err(|e| PayError::internal_error(format!("extract tx quote: {e}")))?;
        let fee_estimate_native = bdk_wallet
            .calculate_fee(&tx)
            .map(|fee| fee.to_sat())
            .map_err(|error| PayError::internal_error(format!("calculate quote fee: {error}")))?;
        persist_changeset(&self.data_dir, &meta, &mut bdk_wallet)?;

        Ok(SendQuoteInfo {
            wallet: id,
            amount_native: target.amount_sats,
            fee_estimate_native,
            fee_unit: "sats".to_string(),
            spend_debits: vec![SpendDebit {
                amount_native: target.amount_sats.saturating_add(fee_estimate_native),
                token: None,
            }],
            warnings: Vec::new(),
            upstream_plan_id: None,
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
        let all = self.store.load_wallet_transaction_records(&id)?;
        let end = all.len().min(offset + limit);
        let start = all.len().min(offset);
        Ok(all[start..end].to_vec())
    }

    async fn history_status(&self, transaction_id: &str) -> Result<HistoryStatusInfo, PayError> {
        match self.store.find_transaction_record_by_id(transaction_id)? {
            Some(mut rec) => {
                if let Some(chain_txid) = chain_txid_from_record(&rec)
                    && let Ok(meta) = self.load_btc_wallet(&rec.wallet)
                {
                    let mut bdk_wallet = open_bdk_wallet_with_dir(&self.data_dir, &meta)?;
                    self.sync_wallet(&meta, &mut bdk_wallet).await?;
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

                        if rec.status != status || rec.confirmed_at_epoch_s != confirmed_at_epoch_s
                        {
                            let _ = self.store.update_transaction_record_status(
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

                Ok(HistoryStatusInfo {
                    transaction_id: rec.transaction_id.clone(),
                    status: rec.status,
                    confirmations: None,
                    preimage: rec.preimage.clone(),
                    item: Some(rec),
                })
            }
            None => Err(PayError::wallet_not_found(format!(
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
        self.sync_wallet(&meta, &mut bdk_wallet).await?;

        let local_records = self.store.load_wallet_transaction_records(&id)?;
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
                    let _ = self.store.update_transaction_record_status(
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
                reference_keys: None,
                reservation_ids: Vec::new(),
            };
            let _ = self.store.append_transaction_record(&record);
            local_by_chain_txid.insert(chain_txid, record);
            stats.records_added = stats.records_added.saturating_add(1);
        }

        Ok(stats)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    #[cfg(feature = "redb")]
    use super::BtcChainSource;
    use super::common::*;
    use super::{BtcProvider, fee_rate_from_sat_per_vb};
    use crate::provider::{PayError, PayProvider};
    #[cfg(feature = "redb")]
    use crate::store::PayStore;
    use crate::store::StorageBackend;
    use crate::store::wallet::WalletMetadata;
    use crate::types::{BtcBackend, Network, WalletCreateRequest};
    #[cfg(feature = "redb")]
    use crate::types::{Input, Output, Request, RuntimeConfig};
    use bdk_wallet::bitcoin::hashes::Hash as _;
    use bdk_wallet::bitcoin::{
        Amount as BtcAmount, Network as BtcNetwork, OutPoint, Transaction, TxIn, TxOut, Txid,
        absolute, transaction,
    };
    use bdk_wallet::{KeychainKind, Wallet};
    use std::sync::Arc;

    const TEST_MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[cfg(feature = "redb")]
    fn test_store(data_dir: &str) -> Arc<StorageBackend> {
        Arc::new(StorageBackend::Redb(
            crate::store::redb_store::RedbStore::new(data_dir),
        ))
    }

    fn signing_test_metadata(address_type: &str) -> WalletMetadata {
        WalletMetadata {
            id: "btc-signing-test".to_string(),
            network: Network::Btc,
            label: None,
            mint_url: None,
            sol_rpc_endpoints: None,
            evm_rpc_endpoints: None,
            evm_chain_id: None,
            sol_cluster: None,
            seed_secret: Some(TEST_MNEMONIC.to_string()),
            backend: None,
            btc_esplora_url: None,
            btc_network: Some("signet".to_string()),
            btc_address_type: Some(address_type.to_string()),
            btc_core_url: None,
            btc_core_auth_secret: None,
            btc_electrum_url: None,
            custom_tokens: None,
            created_at_epoch_s: 0,
            error: None,
        }
    }

    /// A send is only a send if the signed transaction finalizes. This asserts that
    /// promise against a wallet funded entirely in memory, so it holds whichever API
    /// happens to be doing the signing — the point that broke when bdk_wallet stopped
    /// being the key store.
    #[test]
    fn a_built_transaction_finalizes_for_every_address_type() {
        for address_type in ["taproot", "segwit"] {
            let meta = signing_test_metadata(address_type);
            let (external, internal) =
                descriptors_from_mnemonic(TEST_MNEMONIC, BtcNetwork::Signet, address_type).unwrap();
            let mut wallet = Wallet::create(external, internal)
                .network(BtcNetwork::Signet)
                .create_wallet_no_persist()
                .unwrap();

            let funded = wallet.reveal_next_address(KeychainKind::External);
            let funding = Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                // Some non-null previous output: an input that spends nothing is what a
                // coinbase looks like, and bdk refuses to hold an unconfirmed coinbase.
                input: vec![TxIn {
                    previous_output: OutPoint {
                        txid: Txid::from_byte_array([7u8; 32]),
                        vout: 0,
                    },
                    ..TxIn::default()
                }],
                output: vec![TxOut {
                    value: BtcAmount::from_sat(100_000),
                    script_pubkey: funded.address.script_pubkey(),
                }],
            };
            wallet.apply_unconfirmed_txs([(funding, 0u64)]);

            let mut psbt = {
                let mut builder = wallet.build_tx();
                builder.add_recipient(funded.address.script_pubkey(), BtcAmount::from_sat(20_000));
                builder.finish().unwrap()
            };

            let [external_signers, internal_signers] = signers_for_wallet(&meta, &wallet).unwrap();
            let finalized = wallet
                .sign_with_signers(
                    &mut psbt,
                    &[&external_signers, &internal_signers],
                    bdk_wallet::SignOptions::default(),
                )
                .unwrap();

            assert!(finalized, "{address_type}: signing did not finalize");
            psbt.extract_tx()
                .unwrap_or_else(|e| panic!("{address_type}: transaction not extractable: {e}"));
        }
    }

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
            sol_cluster: None,
        }
    }

    #[tokio::test]
    async fn create_wallet_rejects_empty_esplora_url() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_str().unwrap();
        let provider = BtcProvider::new(data_dir, test_store(data_dir));
        let mut req = signet_request("bad-esplora");
        req.btc_esplora_url = Some("   ".to_string());

        let err = provider.create_wallet(&req).await.unwrap_err();
        assert!(
            matches!(err, PayError::InvalidAmount { .. }),
            "expected InvalidAmount, got: {err}"
        );
    }

    #[cfg(not(feature = "btc-core"))]
    #[tokio::test]
    async fn create_wallet_rejects_core_rpc_when_feature_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_str().unwrap();
        let provider = BtcProvider::new(data_dir, test_store(data_dir));
        let mut req = signet_request("core-disabled");
        req.btc_backend = Some(BtcBackend::CoreRpc);
        req.btc_core_url = Some("http://127.0.0.1:18443".to_string());

        let err = provider.create_wallet(&req).await.unwrap_err();
        assert!(
            matches!(err, PayError::NotImplemented { .. }),
            "expected NotImplemented, got: {err}"
        );
    }

    #[cfg(feature = "btc-core")]
    #[tokio::test]
    async fn create_wallet_core_rpc_requires_url() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_str().unwrap();
        let provider = BtcProvider::new(data_dir, test_store(data_dir));
        let mut req = signet_request("core-needs-url");
        req.btc_backend = Some(BtcBackend::CoreRpc);
        req.btc_core_url = None;

        let err = provider.create_wallet(&req).await.unwrap_err();
        assert!(
            matches!(err, PayError::InvalidAmount { .. }),
            "expected InvalidAmount, got: {err}"
        );
    }

    #[cfg(feature = "btc-electrum")]
    #[tokio::test]
    async fn create_wallet_electrum_requires_url() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_str().unwrap();
        let provider = BtcProvider::new(data_dir, test_store(data_dir));
        let mut req = signet_request("electrum-needs-url");
        req.btc_backend = Some(BtcBackend::Electrum);
        req.btc_electrum_url = None;

        let err = provider.create_wallet(&req).await.unwrap_err();
        assert!(
            matches!(err, PayError::InvalidAmount { .. }),
            "expected InvalidAmount, got: {err}"
        );
    }

    #[tokio::test]
    async fn send_quote_rejects_invalid_address() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_str().unwrap();
        let provider = BtcProvider::new(data_dir, test_store(data_dir));
        let wallet = provider
            .create_wallet(&signet_request("send-quote-invalid"))
            .await
            .unwrap();

        let err = provider
            .send_quote(&wallet.id, "bitcoin:not-a-btc-address?amount=1000", None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, PayError::InvalidAmount { .. }),
            "expected InvalidAmount, got: {err}"
        );
    }

    #[cfg(feature = "btc-esplora")]
    #[tokio::test]
    async fn restore_wallet_runs_full_scan_and_cleans_up_on_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_str().unwrap();
        let provider = BtcProvider::new(data_dir, test_store(data_dir));
        let mut req = signet_request("restore-full-scan");
        req.mnemonic_secret = Some(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
                .to_string(),
        );
        // Force full_scan to fail fast so we can assert restore path is exercised.
        req.btc_esplora_url = Some("http://127.0.0.1:1".to_string());

        let err = provider.create_wallet(&req).await.unwrap_err();
        assert!(
            matches!(err, PayError::NetworkError { .. }),
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
        let provider = BtcProvider::new(data_dir, test_store(data_dir));
        let mut req = signet_request("create-no-fullscan");
        req.btc_esplora_url = Some("http://127.0.0.1:1".to_string());

        let wallet = provider.create_wallet(&req).await.unwrap();
        assert!(wallet.id.starts_with("w_"));
    }
    #[cfg(feature = "redb")]
    struct FeeSource {
        fail: std::sync::atomic::AtomicBool,
        rate_sat_per_vb: std::sync::Mutex<f64>,
        broadcasts: std::sync::Mutex<Vec<Transaction>>,
    }

    #[cfg(feature = "redb")]
    #[async_trait::async_trait]
    impl BtcChainSource for FeeSource {
        async fn sync(&self, _: &mut Wallet) -> Result<(), PayError> {
            Ok(())
        }
        async fn full_scan(&self, _: &mut Wallet) -> Result<(), PayError> {
            Ok(())
        }
        async fn fee_rate_sat_per_vb(&self) -> Result<f64, PayError> {
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                Err(PayError::network_error("fee estimate unavailable"))
            } else {
                self.rate_sat_per_vb
                    .lock()
                    .map(|rate| *rate)
                    .map_err(|_| PayError::internal_error("fixture mutex poisoned"))
            }
        }
        async fn broadcast(&self, tx: &Transaction) -> Result<(), PayError> {
            self.broadcasts
                .lock()
                .map_err(|_| PayError::internal_error("fixture mutex poisoned"))?
                .push(tx.clone());
            Ok(())
        }
    }

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn btc_fee_estimate_controls_quote_and_signed_transaction_and_failed_plan() {
        use std::sync::atomic::Ordering;
        for address_type in ["taproot", "segwit"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().to_str().unwrap();
            let store = Arc::new(StorageBackend::Redb(
                crate::store::redb_store::RedbStore::new(path),
            ));
            let mut meta = signing_test_metadata(address_type);
            meta.id = "w_fee_probe".to_owned();
            store.save_wallet_metadata(&meta).unwrap();
            let mut wallet = open_bdk_wallet_with_dir(path, &meta).unwrap();
            let address = wallet.reveal_next_address(KeychainKind::External).address;
            let funding = Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: vec![TxIn {
                    previous_output: OutPoint {
                        txid: Txid::from_byte_array([7; 32]),
                        vout: 0,
                    },
                    ..Default::default()
                }],
                output: vec![TxOut {
                    value: BtcAmount::from_sat(100_000),
                    script_pubkey: address.script_pubkey(),
                }],
            };
            wallet.apply_unconfirmed_txs([(funding, 0)]);
            persist_changeset(path, &meta, &mut wallet).unwrap();
            let source = Arc::new(FeeSource {
                fail: std::sync::atomic::AtomicBool::new(true),
                rate_sat_per_vb: std::sync::Mutex::new(20.0),
                broadcasts: std::sync::Mutex::new(Vec::new()),
            });
            let mut provider = BtcProvider::new(path, store);
            provider.chain_source_override = Some(source.clone());
            let target = format!("bitcoin:{address}?amount=20000");
            assert!(matches!(
                provider.send_quote(&meta.id, &target, None).await,
                Err(PayError::NetworkError { .. })
            ));
            assert!(matches!(
                provider.send(&meta.id, &target, None, None).await,
                Err(PayError::NetworkNotSubmitted { .. })
            ));
            let (writer, mut receiver) = tokio::sync::mpsc::channel(32);
            let mut app = crate::handler::App::new(
                RuntimeConfig {
                    data_dir: path.to_owned(),
                    ..Default::default()
                },
                writer,
                Some(true),
                Some(StorageBackend::Redb(
                    crate::store::redb_store::RedbStore::new(path),
                )),
            );
            app.providers.insert(Network::Btc, Box::new(provider));
            crate::handler::dispatch(
                &app,
                Request::from_input(Input::SendPlan {
                    id: "failed_fee_plan".into(),
                    wallet: Some(meta.id.clone()),
                    network: Some(Network::Btc),
                    to: target.clone(),
                    amount: None,
                    onchain_memo: None,
                    local_memo: None,
                    mints: None,
                    chain_id: None,
                }),
            )
            .await;
            loop {
                let output =
                    tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
                        .await
                        .unwrap()
                        .unwrap();
                if matches!(output, Output::Log { .. }) {
                    continue;
                }
                assert!(
                    matches!(output, Output::Error { error_code, .. } if error_code == "network_error")
                );
                break;
            }
            let plans = directory.path().join("pay-plans");
            assert!(!plans.exists() || std::fs::read_dir(plans).unwrap().next().is_none());
            assert!(source.broadcasts.lock().unwrap().is_empty());
            source.fail.store(false, Ordering::SeqCst);
            let provider = app.providers.get(&Network::Btc).unwrap();
            let quote = provider.send_quote(&meta.id, &target, None).await.unwrap();
            // The chain source turns hostile after the quote: the send must stop
            // before signing rather than pay the inflated rate.
            *source.rate_sat_per_vb.lock().unwrap() = 50_000.0;
            let error = provider
                .send(&meta.id, &target, None, None)
                .await
                .unwrap_err();
            assert!(
                matches!(error, PayError::NetworkNotSubmitted { .. }),
                "{address_type}: {error}"
            );
            assert_eq!(error.submission_transaction_id(), None);
            assert!(source.broadcasts.lock().unwrap().is_empty());
            assert!(
                provider
                    .history_list(&meta.id, 10, 0)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(matches!(
                provider.send_quote(&meta.id, &target, None).await,
                Err(PayError::NetworkError { .. })
            ));
            *source.rate_sat_per_vb.lock().unwrap() = 20.0;
            let sent = provider.send(&meta.id, &target, None, None).await.unwrap();
            let broadcasts = source.broadcasts.lock().unwrap();
            assert_eq!(broadcasts.len(), 1);
            let tx = &broadcasts[0];
            let fee = 100_000
                - tx.output
                    .iter()
                    .map(|output| output.value.to_sat())
                    .sum::<u64>();
            let actual_rate = fee as f64 / tx.vsize() as f64;
            // BDK budgets weight units; rounding the final witness to vbytes can differ slightly.
            assert!((actual_rate - 20.0).abs() < 0.25, "rate={actual_rate}");
            assert!(quote.fee_estimate_native.abs_diff(fee) <= 40);
            assert_eq!(sent.fee.unwrap().value, fee);
        }
    }

    #[test]
    fn btc_fee_estimate_rejects_invalid_values_and_preserves_fractional_rates() {
        for rate in [
            0.0,
            -1.0,
            f64::NAN,
            f64::INFINITY,
            f64::MAX,
            1000.5,
            50_000.0,
        ] {
            assert!(matches!(
                fee_rate_from_sat_per_vb(rate),
                Err(PayError::NetworkError { .. })
            ));
        }
        assert_eq!(
            fee_rate_from_sat_per_vb(20.0).unwrap().to_sat_per_kwu(),
            5000
        );
        assert_eq!(
            fee_rate_from_sat_per_vb(1.25).unwrap().to_sat_per_kwu(),
            313
        );
        assert_eq!(
            fee_rate_from_sat_per_vb(1000.0).unwrap().to_sat_per_kwu(),
            250_000
        );
    }
}
