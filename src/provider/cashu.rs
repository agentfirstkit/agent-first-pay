use crate::provider::{PayError, PayProvider};
use crate::store::wallet::{self, WalletMetadata};
use crate::store::{PayStore, StorageBackend};
use crate::types::*;
use async_trait::async_trait;
use bip39::Mnemonic;
use cdk::nuts::{CurrencyUnit, PaymentMethod, ProofsMethods, State, Token};
use cdk::wallet::{ReceiveOptions, SendOptions, Wallet, WalletBuilder};
use cdk::Amount as CdkAmount;
#[cfg(feature = "redb")]
use cdk_redb::wallet::WalletRedbDatabase;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Normalize mint URL per NUT-00: strip trailing slashes.
fn normalize_mint_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

fn cashu_wallet_summary(m: WalletMetadata) -> WalletSummary {
    let mint_url = m.mint_url.clone();
    WalletSummary {
        id: m.id,
        network: Network::Cashu,
        label: m.label,
        address: mint_url.clone().unwrap_or_default(),
        backend: None,
        mint_url,
        rpc_endpoints: None,
        chain_id: None,
        created_at_epoch_s: m.created_at_epoch_s,
    }
}

pub struct CashuProvider {
    _data_dir: String,
    postgres_url: Option<String>,
    store: Arc<StorageBackend>,
    wallet_cache: RwLock<HashMap<String, Arc<Wallet>>>,
}

impl CashuProvider {
    pub fn new(data_dir: &str, postgres_url: Option<String>, store: Arc<StorageBackend>) -> Self {
        Self {
            _data_dir: data_dir.to_string(),
            postgres_url,
            store,
            wallet_cache: RwLock::new(HashMap::new()),
        }
    }

    fn get_mint_url(&self, meta: &WalletMetadata) -> Result<String, PayError> {
        meta.mint_url
            .clone()
            .ok_or_else(|| PayError::InternalError("wallet has no mint_url".to_string()))
    }

    async fn select_wallet_by_balance(
        &self,
        min_sats: u64,
        prefer_smallest: bool,
        mints: Option<&[String]>,
    ) -> Result<String, PayError> {
        let wallets = self.store.list_wallet_metadata(Some(Network::Cashu))?;
        let mut wallet_infos = Vec::new();
        let mut balance_failures = Vec::new();

        // Best-effort balance collection: one broken wallet should not block routing.
        for meta in &wallets {
            let sats = match self.get_or_create_cdk_wallet(&meta.id).await {
                Ok(w) => match w.total_balance().await {
                    Ok(bal) => bal.to_u64(),
                    Err(e) => {
                        balance_failures.push(format!("{}: balance: {e}", meta.id));
                        continue;
                    }
                },
                Err(e) => {
                    balance_failures.push(format!("{}: {e}", meta.id));
                    continue;
                }
            };
            wallet_infos.push((meta, sats));
        }

        let unavailable_error = || {
            let detail = balance_failures
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ");
            let suffix = if balance_failures.len() > 3 {
                format!(" (+{} more)", balance_failures.len() - 3)
            } else {
                String::new()
            };
            PayError::NetworkError(format!(
                "failed to query cashu wallet balances: {detail}{suffix}"
            ))
        };

        // If mints filter provided, try each mint in order (caller's priority)
        if let Some(mint_list) = mints {
            let normalized_mints: Vec<String> =
                mint_list.iter().map(|m| normalize_mint_url(m)).collect();

            // Try mints in order
            for mint_url in &normalized_mints {
                let mut candidates: Vec<_> = wallet_infos
                    .iter()
                    .filter(|(meta, sats)| {
                        meta.mint_url
                            .as_deref()
                            .map(normalize_mint_url)
                            .is_some_and(|u| u == *mint_url)
                            && *sats >= min_sats
                    })
                    .collect();
                if prefer_smallest {
                    candidates.sort_by_key(|(_, bal)| *bal);
                } else {
                    candidates.sort_by_key(|(_, bal)| std::cmp::Reverse(*bal));
                }
                if let Some((meta, _)) = candidates.first() {
                    return Ok(meta.id.clone());
                }
            }

            // No match — build a helpful error
            let has_wallet_on_mint = wallets.iter().any(|meta| {
                meta.mint_url
                    .as_deref()
                    .map(normalize_mint_url)
                    .is_some_and(|u| normalized_mints.iter().any(|m| m == &u))
            });
            let has_healthy_wallet_on_mint = wallet_infos.iter().any(|(meta, _)| {
                meta.mint_url
                    .as_deref()
                    .map(normalize_mint_url)
                    .is_some_and(|u| normalized_mints.iter().any(|m| m == &u))
            });
            return if has_wallet_on_mint {
                if !has_healthy_wallet_on_mint && !balance_failures.is_empty() {
                    Err(unavailable_error())
                } else {
                    Err(PayError::InvalidAmount(format!(
                        "insufficient balance on accepted mints; need {min_sats} sats"
                    )))
                }
            } else {
                Err(PayError::WalletNotFound(format!(
                    "no wallet on accepted mints: {}; create one with: afpay cashu wallet create --mint-url <mint>",
                    mint_list.join(", ")
                )))
            };
        }

        // No mints filter — original behavior
        let mut candidates = Vec::new();
        for (meta, sats) in wallet_infos {
            if sats >= min_sats {
                candidates.push((meta.id.clone(), sats));
            }
        }
        if prefer_smallest {
            candidates.sort_by_key(|(_, bal)| *bal);
        } else {
            candidates.sort_by_key(|(_, bal)| std::cmp::Reverse(*bal));
        }
        if candidates.is_empty() && !wallets.is_empty() && !balance_failures.is_empty() {
            return Err(unavailable_error());
        }
        candidates.first().map(|(id, _)| id.clone()).ok_or_else(|| {
            PayError::WalletNotFound("no wallet with sufficient balance".to_string())
        })
    }

    async fn get_or_create_cdk_wallet(&self, wallet_id: &str) -> Result<Arc<Wallet>, PayError> {
        // Check cache first
        {
            let cache = self.wallet_cache.read().await;
            if let Some(w) = cache.get(wallet_id) {
                return Ok(w.clone());
            }
        }

        // Load wallet metadata
        let meta = self.store.load_wallet_metadata(wallet_id)?;
        if meta.network != Network::Cashu {
            return Err(PayError::WalletNotFound(format!(
                "{wallet_id} is not a cashu wallet"
            )));
        }

        let seed_secret = meta
            .seed_secret
            .as_deref()
            .ok_or_else(|| PayError::InternalError("wallet missing seed".to_string()))?;
        let mnemonic: Mnemonic = seed_secret
            .parse()
            .map_err(|e| PayError::InternalError(format!("parse mnemonic: {e}")))?;
        let seed = mnemonic.to_seed_normalized("");

        let mint_url = self.get_mint_url(&meta)?;
        let mint_url_parsed: cdk::mint_url::MintUrl = mint_url
            .parse()
            .map_err(|e| PayError::InternalError(format!("parse mint url: {e}")))?;

        let localstore: Arc<
            dyn cdk::cdk_database::WalletDatabase<cdk::cdk_database::Error> + Send + Sync,
        > = if let Some(url) = &self.postgres_url {
            #[cfg(feature = "postgres")]
            {
                let db = cdk_postgres::new_wallet_pg_database(url)
                    .await
                    .map_err(|e| PayError::InternalError(format!("cdk postgres: {e}")))?;
                Arc::new(db)
            }
            #[cfg(not(feature = "postgres"))]
            return Err(PayError::NotImplemented(format!(
                "postgres feature not compiled (url: {url})"
            )));
        } else {
            #[cfg(feature = "redb")]
            {
                let db_dir = self.store.wallet_data_directory_path_for_meta(&meta);
                std::fs::create_dir_all(&db_dir)
                    .map_err(|e| PayError::InternalError(format!("create cashu db dir: {e}")))?;
                let db = WalletRedbDatabase::new(&db_dir.join("cdk-wallet.redb"))
                    .map_err(|e| PayError::InternalError(format!("open redb: {e}")))?;
                Arc::new(db)
            }
            #[cfg(not(feature = "redb"))]
            return Err(PayError::NotImplemented(
                "redb feature not compiled".to_string(),
            ));
        };

        let wallet = WalletBuilder::new()
            .mint_url(mint_url_parsed)
            .unit(CurrencyUnit::Sat)
            .localstore(localstore)
            .seed(seed)
            .build()
            .map_err(|e| PayError::InternalError(format!("build cdk wallet: {e}")))?;

        let wallet = Arc::new(wallet);

        // Cache
        let mut cache = self.wallet_cache.write().await;
        cache.insert(wallet_id.to_string(), wallet.clone());

        Ok(wallet)
    }
}

#[async_trait]
impl PayProvider for CashuProvider {
    fn network(&self) -> Network {
        Network::Cashu
    }

    fn writes_locally(&self) -> bool {
        true
    }

    async fn create_wallet(&self, request: &WalletCreateRequest) -> Result<WalletInfo, PayError> {
        let id = wallet::generate_wallet_identifier()?;
        let resolved_mint = request.mint_url.as_deref().ok_or_else(|| {
            PayError::InvalidAmount("mint_url is required for cashu wallets".to_string())
        })?;

        let mnemonic_str = if let Some(raw) = request.mnemonic_secret.as_deref() {
            let mnemonic: Mnemonic = raw.parse().map_err(|e| {
                PayError::InvalidAmount(format!("invalid mnemonic-secret for cashu wallet: {e}"))
            })?;
            mnemonic.words().collect::<Vec<_>>().join(" ")
        } else {
            // Generate BIP39 12-word mnemonic (128-bit entropy)
            let mut entropy = [0u8; 16];
            getrandom::fill(&mut entropy)
                .map_err(|e| PayError::InternalError(format!("rng failed: {e}")))?;
            let mnemonic = Mnemonic::from_entropy(&entropy)
                .map_err(|e| PayError::InternalError(format!("mnemonic gen: {e}")))?;
            mnemonic.words().collect::<Vec<_>>().join(" ")
        };

        let meta = WalletMetadata {
            id: id.clone(),
            network: Network::Cashu,
            label: {
                let trimmed = request.label.trim();
                if trimmed.is_empty() || trimmed == "default" {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            },
            mint_url: Some(normalize_mint_url(resolved_mint)),
            sol_rpc_endpoints: None,
            evm_rpc_endpoints: None,
            evm_chain_id: None,
            seed_secret: Some(mnemonic_str.clone()),
            backend: None,
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
        self.store.save_wallet_metadata(&meta)?;

        Ok(WalletInfo {
            id,
            network: Network::Cashu,
            address: resolved_mint.to_string(),
            label: meta.label,
            mnemonic: None,
        })
    }

    async fn close_wallet(&self, wallet_id: &str) -> Result<(), PayError> {
        // Check balance first — only allow closing zero-balance wallets
        let bal = self.balance(wallet_id).await?;
        if bal.confirmed > 0 || bal.pending > 0 {
            return Err(PayError::InvalidAmount(format!(
                "wallet {wallet_id} has {} confirmed + {} pending {}; send or withdraw first",
                bal.confirmed, bal.pending, bal.unit
            )));
        }
        // Remove from cache
        {
            let mut cache = self.wallet_cache.write().await;
            cache.remove(wallet_id);
        }
        self.store.delete_wallet_metadata(wallet_id)?;
        Ok(())
    }

    async fn list_wallets(&self) -> Result<Vec<WalletSummary>, PayError> {
        let wallets = self.store.list_wallet_metadata(Some(Network::Cashu))?;
        Ok(wallets.into_iter().map(cashu_wallet_summary).collect())
    }

    async fn balance(&self, wallet_id: &str) -> Result<BalanceInfo, PayError> {
        let w = self.get_or_create_cdk_wallet(wallet_id).await?;
        let confirmed = w
            .total_balance()
            .await
            .map_err(|e| PayError::NetworkError(format!("balance: {e}")))?;
        let pending = w
            .total_pending_balance()
            .await
            .map_err(|e| PayError::NetworkError(format!("pending balance: {e}")))?;
        Ok(BalanceInfo::new(
            confirmed.to_u64(),
            pending.to_u64(),
            "sats",
        ))
    }

    async fn check_balance(&self, wallet_id: &str) -> Result<BalanceInfo, PayError> {
        let w = self.get_or_create_cdk_wallet(wallet_id).await?;

        // Check unspent proofs against the mint
        let unspent_proofs = w
            .get_unspent_proofs()
            .await
            .map_err(|e| PayError::NetworkError(format!("get proofs: {e}")))?;
        let states = if unspent_proofs.is_empty() {
            vec![]
        } else {
            w.check_proofs_spent(unspent_proofs.clone())
                .await
                .map_err(|e| PayError::NetworkError(format!("check proofs: {e}")))?
        };

        // Sum only truly unspent
        let mut confirmed: u64 = 0;
        for (proof, state) in unspent_proofs.iter().zip(states.iter()) {
            if state.state == State::Unspent {
                confirmed += proof.amount.to_u64();
            }
        }

        // Also check pending proofs for recovery
        let pending_amount = w
            .check_all_pending_proofs()
            .await
            .map_err(|e| PayError::NetworkError(format!("check pending: {e}")))?;

        Ok(BalanceInfo::new(confirmed, pending_amount.to_u64(), "sats"))
    }

    async fn restore(&self, wallet_id: &str) -> Result<RestoreResult, PayError> {
        let w = self.get_or_create_cdk_wallet(wallet_id).await?;
        let restored = w
            .restore()
            .await
            .map_err(|e| PayError::NetworkError(format!("restore: {e}")))?;
        Ok(RestoreResult {
            wallet: wallet_id.to_string(),
            unspent: restored.unspent.to_u64(),
            spent: restored.spent.to_u64(),
            pending: restored.pending.to_u64(),
            unit: "sats".to_string(),
        })
    }

    async fn balance_all(&self) -> Result<Vec<WalletBalanceItem>, PayError> {
        let wallets = self.store.list_wallet_metadata(Some(Network::Cashu))?;
        let mut items = Vec::new();
        for meta in &wallets {
            let w = self.get_or_create_cdk_wallet(&meta.id).await?;
            let confirmed = w
                .total_balance()
                .await
                .map_err(|e| PayError::NetworkError(format!("balance: {e}")))?;
            let pending = w
                .total_pending_balance()
                .await
                .map_err(|e| PayError::NetworkError(format!("pending balance: {e}")))?;
            items.push(WalletBalanceItem {
                wallet: cashu_wallet_summary(meta.clone()),
                balance: Some(BalanceInfo::new(
                    confirmed.to_u64(),
                    pending.to_u64(),
                    "sats",
                )),
                error: None,
            });
        }
        Ok(items)
    }

    async fn receive_info(
        &self,
        wallet_id: &str,
        amount: Option<Amount>,
    ) -> Result<ReceiveInfo, PayError> {
        let w = self.get_or_create_cdk_wallet(wallet_id).await?;
        let cdk_amount = amount.map(|a| CdkAmount::from(a.value));
        let quote = w
            .mint_quote(PaymentMethod::BOLT11, cdk_amount, None, None)
            .await
            .map_err(|e| PayError::NetworkError(format!("mint quote: {e}")))?;
        Ok(ReceiveInfo {
            address: None,
            invoice: Some(quote.request),
            quote_id: Some(quote.id),
        })
    }

    async fn receive_claim(&self, wallet_id: &str, quote_id: &str) -> Result<u64, PayError> {
        let w = self.get_or_create_cdk_wallet(wallet_id).await?;
        let proofs = w
            .mint(quote_id, cdk::amount::SplitTarget::default(), None)
            .await
            .map_err(|e| PayError::NetworkError(format!("mint: {e}")))?;
        let total: u64 = proofs
            .total_amount()
            .map_err(|e| PayError::InternalError(format!("sum proofs: {e}")))?
            .to_u64();

        // Persist claim as a receive history item so history/status can track mint deposits.
        if self
            .store
            .find_transaction_record_by_id(quote_id)?
            .is_none()
        {
            let now = wallet::now_epoch_seconds();
            let record = HistoryRecord {
                transaction_id: quote_id.to_string(),
                wallet: wallet_id.to_string(),
                network: Network::Cashu,
                direction: Direction::Receive,
                amount: Amount {
                    value: total,
                    token: "sats".to_string(),
                },
                status: TxStatus::Confirmed,
                onchain_memo: Some("cashu mint claim".to_string()),
                local_memo: None,
                remote_addr: None,
                preimage: None,
                created_at_epoch_s: now,
                confirmed_at_epoch_s: Some(now),
                fee: None,
            };
            let _ = self.store.append_transaction_record(&record);
        }
        Ok(total)
    }

    #[cfg(feature = "interactive")]
    async fn cashu_send_quote(
        &self,
        wallet_id: &str,
        amount: &Amount,
    ) -> Result<CashuSendQuoteInfo, PayError> {
        let resolved = if wallet_id.is_empty() {
            self.select_wallet_by_balance(amount.value, true, None)
                .await?
        } else {
            wallet_id.to_string()
        };
        let w = self.get_or_create_cdk_wallet(&resolved).await?;
        let cdk_amount = CdkAmount::from(amount.value);
        let send_options = SendOptions {
            include_fee: true,
            ..SendOptions::default()
        };
        let prepared = w
            .prepare_send(cdk_amount, send_options)
            .await
            .map_err(|e| PayError::NetworkError(format!("prepare send: {e}")))?;
        let fee_sats = prepared.fee().to_u64();
        // Release reserved proofs — this is quote-only
        let _ = prepared.cancel().await;
        Ok(CashuSendQuoteInfo {
            wallet: resolved,
            amount_native: amount.value,
            fee_native: fee_sats,
            fee_unit: "sats".to_string(),
        })
    }

    async fn cashu_send(
        &self,
        wallet_id: &str,
        amount: Amount,
        onchain_memo: Option<&str>,
        mints: Option<&[String]>,
    ) -> Result<CashuSendResult, PayError> {
        let resolved = if wallet_id.is_empty() {
            self.select_wallet_by_balance(amount.value, true, mints)
                .await?
        } else if let Some(mint_list) = mints {
            // Explicit wallet — validate it's on an accepted mint
            let meta = self.store.load_wallet_metadata(wallet_id)?;
            if let Some(url) = &meta.mint_url {
                let normalized = normalize_mint_url(url);
                if !mint_list
                    .iter()
                    .any(|m| normalize_mint_url(m) == normalized)
                {
                    return Err(PayError::InvalidAmount(format!(
                        "wallet {wallet_id} is on mint {url}, not in accepted mints: {}",
                        mint_list.join(", ")
                    )));
                }
            }
            wallet_id.to_string()
        } else {
            wallet_id.to_string()
        };
        let w = self.get_or_create_cdk_wallet(&resolved).await?;
        let transaction_id = wallet::generate_transaction_identifier()?;
        let balance_before_send = w
            .total_balance()
            .await
            .map_err(|e| PayError::NetworkError(format!("balance before send: {e}")))?
            .to_u64();

        // P2P cashu token send
        let cdk_amount = CdkAmount::from(amount.value);
        let send_options = SendOptions {
            include_fee: true,
            ..SendOptions::default()
        };
        let prepared = w
            .prepare_send(cdk_amount, send_options)
            .await
            .map_err(|e| PayError::NetworkError(format!("prepare send: {e}")))?;

        let token = prepared
            .confirm(None)
            .await
            .map_err(|e| PayError::NetworkError(format!("confirm send: {e}")))?;

        let balance_after_send = w
            .total_balance()
            .await
            .map_err(|e| PayError::NetworkError(format!("balance after send: {e}")))?
            .to_u64();
        let total_spent = balance_before_send.saturating_sub(balance_after_send);
        let fee_sats = total_spent.saturating_sub(amount.value);

        let token_str = token.to_string();

        let fee_amount = if fee_sats > 0 {
            Some(Amount {
                value: fee_sats,
                token: "sats".to_string(),
            })
        } else {
            None
        };
        let record = HistoryRecord {
            transaction_id: transaction_id.clone(),
            wallet: resolved.clone(),
            network: Network::Cashu,
            direction: Direction::Send,
            amount: amount.clone(),
            status: TxStatus::Confirmed,
            onchain_memo: onchain_memo.map(|s| s.to_string()),
            local_memo: None,
            remote_addr: None,
            preimage: None,
            created_at_epoch_s: wallet::now_epoch_seconds(),
            confirmed_at_epoch_s: Some(wallet::now_epoch_seconds()),
            fee: fee_amount.clone(),
        };
        let _ = self.store.append_transaction_record(&record);

        Ok(CashuSendResult {
            wallet: resolved,
            transaction_id,
            status: TxStatus::Confirmed,
            fee: fee_amount,
            token: token_str,
        })
    }

    async fn cashu_receive(
        &self,
        wallet_id: &str,
        token: &str,
    ) -> Result<CashuReceiveResult, PayError> {
        let resolved_wallet = if wallet_id.is_empty() {
            // Parse token to extract mint_url
            let parsed = Token::from_str(token)
                .map_err(|e| PayError::InvalidAmount(format!("parse token: {e}")))?;
            let mint_url_str = normalize_mint_url(
                &parsed
                    .mint_url()
                    .map_err(|e| PayError::InvalidAmount(format!("token mint_url: {e}")))?
                    .to_string(),
            );

            // Find existing wallet with matching mint_url
            let wallets = self.store.list_wallet_metadata(Some(Network::Cashu))?;
            if let Some(w) = wallets
                .iter()
                .find(|w| w.mint_url.as_deref() == Some(mint_url_str.as_str()))
            {
                w.id.clone()
            } else {
                // Auto-create wallet for this mint
                self.create_wallet(&WalletCreateRequest {
                    label: "default".to_string(),
                    mint_url: Some(mint_url_str.clone()),
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
                })
                .await?
                .id
            }
        } else {
            // Validate mint URL matches the wallet's mint
            let parsed = Token::from_str(token)
                .map_err(|e| PayError::InvalidAmount(format!("parse token: {e}")))?;
            let token_mint = normalize_mint_url(
                &parsed
                    .mint_url()
                    .map_err(|e| PayError::InvalidAmount(format!("token mint_url: {e}")))?
                    .to_string(),
            );
            let meta = self.store.load_wallet_metadata(wallet_id)?;
            if let Some(wallet_mint) = meta.mint_url.as_deref() {
                if normalize_mint_url(wallet_mint) != token_mint {
                    return Err(PayError::InvalidAmount(format!(
                        "token mint ({token_mint}) does not match wallet {} mint ({wallet_mint})",
                        wallet_id
                    )));
                }
            }
            wallet_id.to_string()
        };

        let w = self.get_or_create_cdk_wallet(&resolved_wallet).await?;
        let transaction_id = wallet::generate_transaction_identifier()?;

        let received = w
            .receive(token, ReceiveOptions::default())
            .await
            .map_err(|e| PayError::NetworkError(format!("receive: {e}")))?;

        let sats = received.to_u64();

        let record = HistoryRecord {
            transaction_id,
            wallet: resolved_wallet.clone(),
            network: Network::Cashu,
            direction: Direction::Receive,
            amount: Amount {
                value: sats,
                token: "sats".to_string(),
            },
            status: TxStatus::Confirmed,
            onchain_memo: Some("receive cashu token".to_string()),
            local_memo: None,
            remote_addr: None,
            preimage: None,
            created_at_epoch_s: wallet::now_epoch_seconds(),
            confirmed_at_epoch_s: Some(wallet::now_epoch_seconds()),
            fee: None,
        };
        let _ = self.store.append_transaction_record(&record);

        Ok(CashuReceiveResult {
            wallet: resolved_wallet,
            amount: Amount {
                value: sats,
                token: "sats".to_string(),
            },
        })
    }

    async fn send_quote(
        &self,
        wallet_id: &str,
        to: &str,
        mints: Option<&[String]>,
    ) -> Result<SendQuoteInfo, PayError> {
        let resolved = if wallet_id.is_empty() {
            self.select_wallet_by_balance(1, false, mints).await?
        } else {
            wallet_id.to_string()
        };
        let w = self.get_or_create_cdk_wallet(&resolved).await?;

        let quote = w
            .melt_quote(PaymentMethod::BOLT11, to, None, None)
            .await
            .map_err(|e| PayError::NetworkError(format!("melt quote: {e}")))?;

        Ok(SendQuoteInfo {
            wallet: resolved,
            amount_native: quote.amount.to_u64(),
            fee_estimate_native: quote.fee_reserve.to_u64(),
            fee_unit: "sats".to_string(),
        })
    }

    async fn send(
        &self,
        wallet_id: &str,
        to: &str,
        onchain_memo: Option<&str>,
        mints: Option<&[String]>,
    ) -> Result<SendResult, PayError> {
        let resolved = if wallet_id.is_empty() {
            // Select wallet with largest balance for withdraw
            self.select_wallet_by_balance(1, false, mints).await?
        } else {
            wallet_id.to_string()
        };
        let w = self.get_or_create_cdk_wallet(&resolved).await?;
        let transaction_id = wallet::generate_transaction_identifier()?;

        let quote = w
            .melt_quote(PaymentMethod::BOLT11, to, None, None)
            .await
            .map_err(|e| PayError::NetworkError(format!("melt quote: {e}")))?;

        let prepared = w
            .prepare_melt(&quote.id, HashMap::new())
            .await
            .map_err(|e| PayError::NetworkError(format!("prepare melt: {e}")))?;

        let finalized = prepared
            .confirm()
            .await
            .map_err(|e| PayError::NetworkError(format!("confirm melt: {e}")))?;

        let fee_sats = finalized.fee_paid().to_u64();
        let amount_sats = quote.amount.to_u64();
        let amount = Amount {
            value: amount_sats,
            token: "sats".to_string(),
        };

        let fee_amount = if fee_sats > 0 {
            Some(Amount {
                value: fee_sats,
                token: "sats".to_string(),
            })
        } else {
            None
        };
        let record = HistoryRecord {
            transaction_id: transaction_id.clone(),
            wallet: resolved.clone(),
            network: Network::Cashu,
            direction: Direction::Send,
            amount: amount.clone(),
            status: TxStatus::Confirmed,
            onchain_memo: onchain_memo
                .map(|s| s.to_string())
                .or(Some("withdraw to Lightning".to_string())),
            local_memo: None,
            remote_addr: Some(to.to_string()),
            preimage: None,
            created_at_epoch_s: wallet::now_epoch_seconds(),
            confirmed_at_epoch_s: Some(wallet::now_epoch_seconds()),
            fee: fee_amount.clone(),
        };
        let _ = self.store.append_transaction_record(&record);

        Ok(SendResult {
            wallet: resolved,
            transaction_id,
            amount,
            fee: fee_amount,
            preimage: None,
        })
    }

    async fn history_list(
        &self,
        wallet_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<HistoryRecord>, PayError> {
        // Verify wallet exists and is cashu
        let meta = self.store.load_wallet_metadata(wallet_id)?;
        if meta.network != Network::Cashu {
            return Err(PayError::WalletNotFound(format!(
                "{wallet_id} is not a cashu wallet"
            )));
        }
        let all = self.store.load_wallet_transaction_records(wallet_id)?;
        let end = all.len().min(offset + limit);
        let start = all.len().min(offset);
        Ok(all[start..end].to_vec())
    }

    async fn history_status(&self, transaction_id: &str) -> Result<HistoryStatusInfo, PayError> {
        match self.store.find_transaction_record_by_id(transaction_id)? {
            Some(rec) => Ok(HistoryStatusInfo {
                transaction_id: rec.transaction_id.clone(),
                status: rec.status,
                confirmations: None,
                preimage: rec.preimage.clone(),
                item: Some(rec),
            }),
            None => Err(PayError::WalletNotFound(format!(
                "transaction {transaction_id} not found"
            ))),
        }
    }

    async fn history_sync(
        &self,
        wallet_id: &str,
        limit: usize,
    ) -> Result<crate::provider::HistorySyncStats, PayError> {
        let records = self.history_list(wallet_id, limit, 0).await?;
        Ok(crate::provider::HistorySyncStats {
            records_scanned: records.len(),
            records_added: 0,
            records_updated: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "redb")]
    fn test_store(data_dir: &str) -> Arc<StorageBackend> {
        Arc::new(crate::store::StorageBackend::Redb(
            crate::store::redb_store::RedbStore::new(data_dir),
        ))
    }

    /// CashuProvider with redb store: create wallet, list, load metadata.
    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn create_and_list_wallets_redb() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();
        let store = test_store(dir);
        let provider = CashuProvider::new(dir, None, store);

        let w = provider
            .create_wallet(&WalletCreateRequest {
                label: "test".to_string(),
                mint_url: Some("https://mint.example.com".to_string()),
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
            })
            .await
            .unwrap();

        assert_eq!(w.network, Network::Cashu);
        assert_eq!(w.address, "https://mint.example.com");

        let wallets = provider.list_wallets().await.unwrap();
        assert_eq!(wallets.len(), 1);
        assert_eq!(wallets[0].id, w.id);
        assert_eq!(
            wallets[0].mint_url.as_deref(),
            Some("https://mint.example.com")
        );
    }

    /// CashuProvider with postgres_url=Some but redb store: CDK wallet should
    /// attempt postgres. Without a real PG server we just verify the error path.
    #[cfg(all(feature = "redb", feature = "postgres"))]
    #[tokio::test]
    async fn cdk_postgres_url_errors_without_server() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();
        let store = test_store(dir);
        let provider = CashuProvider::new(
            dir,
            Some("postgres://invalid:5432/nonexistent".to_string()),
            store,
        );

        // Create a wallet (metadata stored in redb)
        let w = provider
            .create_wallet(&WalletCreateRequest {
                label: "pg-test".to_string(),
                mint_url: Some("https://mint.example.com".to_string()),
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
            })
            .await
            .unwrap();

        // get_or_create_cdk_wallet should try cdk-postgres and fail
        let err = provider.balance(&w.id).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("cdk postgres"),
            "expected cdk postgres error, got: {msg}"
        );
    }

    /// CashuProvider with postgres_url=None: CDK wallet uses redb localstore.
    /// Verify the redb CDK database file is created.
    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn cdk_redb_creates_database_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();
        let store = test_store(dir);
        let provider = CashuProvider::new(dir, None, store);

        let w = provider
            .create_wallet(&WalletCreateRequest {
                label: "redb-cdk".to_string(),
                mint_url: Some("https://mint.example.com".to_string()),
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
            })
            .await
            .unwrap();

        // Trigger CDK wallet creation (will fail to connect to mint, but
        // the redb database file should be created before the network call)
        let _ = provider.balance(&w.id).await;

        // Check that the cdk-wallet.redb file exists
        let meta = provider.store.load_wallet_metadata(&w.id).unwrap();
        let db_dir = provider.store.wallet_data_directory_path_for_meta(&meta);
        let redb_path = db_dir.join("cdk-wallet.redb");
        assert!(
            redb_path.exists(),
            "cdk-wallet.redb should be created at {redb_path:?}"
        );
    }

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn select_wallet_skips_invalid_wallet_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();
        let store = test_store(dir);
        let provider = CashuProvider::new(dir, None, store);

        let healthy = provider
            .create_wallet(&WalletCreateRequest {
                label: "healthy".to_string(),
                mint_url: Some("https://mint.example.com".to_string()),
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
            })
            .await
            .unwrap();

        let bad_id = wallet::generate_wallet_identifier().unwrap();
        provider
            .store
            .save_wallet_metadata(&WalletMetadata {
                id: bad_id,
                network: Network::Cashu,
                label: Some("broken".to_string()),
                mint_url: Some("https://mint.example.com".to_string()),
                sol_rpc_endpoints: None,
                evm_rpc_endpoints: None,
                evm_chain_id: None,
                seed_secret: Some("not a mnemonic".to_string()),
                backend: None,
                btc_esplora_url: None,
                btc_network: None,
                btc_address_type: None,
                btc_core_url: None,
                btc_core_auth_secret: None,
                btc_electrum_url: None,
                custom_tokens: None,
                created_at_epoch_s: wallet::now_epoch_seconds(),
                error: None,
            })
            .unwrap();

        let selected = provider
            .select_wallet_by_balance(0, true, None)
            .await
            .unwrap();
        assert_eq!(
            selected, healthy.id,
            "wallet selection should skip invalid wallet metadata"
        );
    }

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn select_wallet_reports_unavailable_when_all_wallets_fail() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();
        let store = test_store(dir);
        let provider = CashuProvider::new(dir, None, store);

        let bad_id = wallet::generate_wallet_identifier().unwrap();
        provider
            .store
            .save_wallet_metadata(&WalletMetadata {
                id: bad_id,
                network: Network::Cashu,
                label: Some("broken".to_string()),
                mint_url: Some("https://mint.example.com".to_string()),
                sol_rpc_endpoints: None,
                evm_rpc_endpoints: None,
                evm_chain_id: None,
                seed_secret: Some("not a mnemonic".to_string()),
                backend: None,
                btc_esplora_url: None,
                btc_network: None,
                btc_address_type: None,
                btc_core_url: None,
                btc_core_auth_secret: None,
                btc_electrum_url: None,
                custom_tokens: None,
                created_at_epoch_s: wallet::now_epoch_seconds(),
                error: None,
            })
            .unwrap();

        let err = provider
            .select_wallet_by_balance(0, true, None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, PayError::NetworkError(_)),
            "expected NetworkError, got: {err}"
        );
    }

    #[test]
    fn bip39_roundtrip() {
        let mut entropy = [0u8; 16];
        getrandom::fill(&mut entropy).ok();
        let mnemonic = Mnemonic::from_entropy(&entropy).unwrap();
        let words: Vec<&str> = mnemonic.words().collect();
        assert_eq!(
            words.len(),
            12,
            "BIP39 128-bit entropy should produce 12 words"
        );

        let mnemonic_str = words.join(" ");
        let parsed: Mnemonic = mnemonic_str.parse().unwrap();
        let seed = parsed.to_seed_normalized("");
        assert_eq!(seed.len(), 64, "BIP39 seed should be 64 bytes");

        // Same mnemonic should produce same seed
        let seed2 = mnemonic.to_seed_normalized("");
        assert_eq!(seed, seed2);
    }
}
