use crate::provider::{HistorySyncStats, PayError, PayProvider};
use crate::spend::tokens;
use crate::store::wallet::{self, WalletMetadata};
use crate::store::{PayStore, StorageBackend};
use crate::types::*;
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use bip39::Mnemonic;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use solana_sdk::hash::Hash;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{keypair_from_seed_phrase_and_passphrase, Keypair, Signer};
use solana_sdk::transaction::Transaction;
use solana_system_interface::instruction as system_instruction;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

fn sol_wallet_summary(meta: WalletMetadata, address: String) -> WalletSummary {
    WalletSummary {
        id: meta.id,
        network: Network::Sol,
        label: meta.label,
        address,
        backend: None,
        mint_url: None,
        rpc_endpoints: meta.sol_rpc_endpoints,
        chain_id: None,
        created_at_epoch_s: meta.created_at_epoch_s,
    }
}

pub struct SolProvider {
    _data_dir: String,
    http_client: reqwest::Client,
    store: Arc<StorageBackend>,
}

const INVALID_SOL_WALLET_ADDRESS: &str = "invalid:sol-wallet-secret";
const MAX_CHAIN_HISTORY_SCAN: usize = 200;
const SOL_MEMO_PROGRAM_ID: &str = "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr";
const SPL_TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const SPL_ASSOCIATED_TOKEN_PROGRAM_ID: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

#[derive(Debug, Clone)]
struct SolTransferTarget {
    recipient_address: String,
    amount_lamports: u64,
    /// If set, this is an SPL token transfer instead of the native token.
    token_mint: Option<Pubkey>,
    /// Reference key for order binding (per strain-payment-method-solana).
    /// Added as a read-only non-signer account on the transfer instruction.
    reference: Option<Pubkey>,
}

#[derive(Debug, Clone, Copy)]
struct SolChainStatus {
    status: TxStatus,
    confirmations: Option<u32>,
}

impl SolProvider {
    pub fn new(data_dir: &str, store: Arc<StorageBackend>) -> Self {
        Self {
            _data_dir: data_dir.to_string(),
            http_client: reqwest::Client::new(),
            store,
        }
    }

    fn normalize_rpc_endpoint(raw: &str) -> Result<String, PayError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(PayError::InvalidAmount(
                "sol wallet requires --sol-rpc-endpoint".to_string(),
            ));
        }
        let endpoint = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            trimmed.to_string()
        } else {
            format!("http://{trimmed}")
        };
        reqwest::Url::parse(&endpoint)
            .map_err(|e| PayError::InvalidAmount(format!("invalid --sol-rpc-endpoint: {e}")))?;
        Ok(endpoint)
    }

    #[cfg(test)]
    fn decode_rpc_endpoint_list(raw: &str) -> Result<Vec<String>, PayError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(PayError::InvalidAmount(
                "sol wallet requires --sol-rpc-endpoint".to_string(),
            ));
        }
        if !trimmed.starts_with('[') {
            return Ok(vec![trimmed.to_string()]);
        }
        let values = serde_json::from_str::<Vec<String>>(trimmed).map_err(|e| {
            PayError::InvalidAmount(format!(
                "invalid --sol-rpc-endpoint list: expected JSON string array: {e}"
            ))
        })?;
        if values.is_empty() {
            return Err(PayError::InvalidAmount(
                "--sol-rpc-endpoint requires at least one value".to_string(),
            ));
        }
        Ok(values)
    }

    #[cfg(test)]
    fn normalize_rpc_endpoints(raw: &str) -> Result<Vec<String>, PayError> {
        let mut endpoints = Vec::new();
        for candidate in Self::decode_rpc_endpoint_list(raw)? {
            let normalized = Self::normalize_rpc_endpoint(&candidate)?;
            if !endpoints.contains(&normalized) {
                endpoints.push(normalized);
            }
        }
        if endpoints.is_empty() {
            return Err(PayError::InvalidAmount(
                "--sol-rpc-endpoint requires at least one value".to_string(),
            ));
        }
        Ok(endpoints)
    }

    fn keypair_from_seed_secret(seed_secret: &str) -> Result<Keypair, PayError> {
        seed_secret.parse::<Mnemonic>().map_err(|_| {
            PayError::InternalError(
                "invalid sol wallet secret: expected BIP39 mnemonic words".to_string(),
            )
        })?;
        keypair_from_seed_phrase_and_passphrase(seed_secret, "")
            .map_err(|e| PayError::InternalError(format!("build keypair from sol mnemonic: {e}")))
    }

    fn wallet_keypair(meta: &WalletMetadata) -> Result<Keypair, PayError> {
        let seed_secret = meta.seed_secret.as_deref().ok_or_else(|| {
            PayError::InternalError(format!("wallet {} missing sol secret", meta.id))
        })?;
        Self::keypair_from_seed_secret(seed_secret)
    }

    fn wallet_address(meta: &WalletMetadata) -> Result<String, PayError> {
        Ok(Self::wallet_keypair(meta)?.pubkey().to_string())
    }

    fn parse_transfer_target(
        to: &str,
        rpc_endpoints: &[String],
    ) -> Result<SolTransferTarget, PayError> {
        let trimmed = to.trim();
        if trimmed.is_empty() {
            return Err(PayError::InvalidAmount(
                "sol send target is empty".to_string(),
            ));
        }
        let no_scheme = trimmed.strip_prefix("solana:").unwrap_or(trimmed);
        let (recipient, query) = match no_scheme.split_once('?') {
            Some(parts) => parts,
            None => (no_scheme, ""),
        };
        let recipient_address = recipient.trim();
        if recipient_address.is_empty() {
            return Err(PayError::InvalidAmount(
                "sol send target missing recipient address".to_string(),
            ));
        }
        let _ = Pubkey::from_str(recipient_address)
            .map_err(|e| PayError::InvalidAmount(format!("invalid sol recipient address: {e}")))?;

        let mut amount_lamports: Option<u64> = None;
        let mut token_mint: Option<Pubkey> = None;
        let mut reference: Option<Pubkey> = None;
        for pair in query.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (key, value) = match pair.split_once('=') {
                Some(kv) => kv,
                None => (pair, ""),
            };
            match key {
                "amount" | "amount-lamports" => {
                    let parsed = value.parse::<u64>().map_err(|_| {
                        PayError::InvalidAmount(format!("invalid amount value '{value}'"))
                    })?;
                    amount_lamports = Some(parsed);
                }
                "token" => {
                    if value == "native" {
                        // Explicit native token — no SPL mint
                    } else {
                        // Try as known symbol first, then as raw mint address
                        let cluster = rpc_endpoints
                            .first()
                            .map(|e| tokens::sol_cluster_from_endpoint(e))
                            .unwrap_or("mainnet-beta");
                        if let Some(known) = tokens::resolve_sol_token(cluster, value) {
                            token_mint = Some(Pubkey::from_str(known.address).map_err(|e| {
                                PayError::InternalError(format!(
                                    "invalid known token mint address: {e}"
                                ))
                            })?);
                        } else {
                            token_mint = Some(Pubkey::from_str(value).map_err(|e| {
                                PayError::InvalidAmount(format!(
                                    "unknown token '{value}'; provide a known symbol (native, usdc, usdt) or mint address: {e}"
                                ))
                            })?);
                        }
                    }
                }
                "reference" => {
                    reference = Some(Pubkey::from_str(value).map_err(|e| {
                        PayError::InvalidAmount(format!("invalid reference key '{value}': {e}"))
                    })?);
                }
                _ => {}
            }
        }
        let Some(amount_lamports) = amount_lamports else {
            return Err(PayError::InvalidAmount(
                "sol send target missing amount; use solana:<address>?amount=<u64>&token=native"
                    .to_string(),
            ));
        };
        if amount_lamports == 0 {
            return Err(PayError::InvalidAmount("amount must be >= 1".to_string()));
        }

        Ok(SolTransferTarget {
            recipient_address: recipient_address.to_string(),
            amount_lamports,
            token_mint,
            reference,
        })
    }

    fn load_sol_wallet(&self, wallet_id: &str) -> Result<WalletMetadata, PayError> {
        let meta = self.store.load_wallet_metadata(wallet_id)?;
        if meta.network != Network::Sol {
            return Err(PayError::WalletNotFound(format!(
                "{wallet_id} is not a sol wallet"
            )));
        }
        Ok(meta)
    }

    fn resolve_wallet_id(&self, wallet_id: &str) -> Result<String, PayError> {
        if !wallet_id.trim().is_empty() {
            return Ok(wallet_id.to_string());
        }
        let wallets = self.store.list_wallet_metadata(Some(Network::Sol))?;
        match wallets.len() {
            0 => Err(PayError::WalletNotFound("no sol wallet found".to_string())),
            1 => Ok(wallets[0].id.clone()),
            _ => Err(PayError::InvalidAmount(
                "multiple sol wallets found; pass --wallet".to_string(),
            )),
        }
    }

    fn rpc_endpoints_for_wallet(meta: &WalletMetadata) -> Result<Vec<String>, PayError> {
        let Some(configured) = meta.sol_rpc_endpoints.as_ref() else {
            return Err(PayError::InternalError(format!(
                "wallet {} missing sol rpc endpoints; recreate wallet",
                meta.id
            )));
        };
        let mut endpoints = Vec::new();
        for candidate in configured {
            let normalized = Self::normalize_rpc_endpoint(candidate)?;
            if !endpoints.contains(&normalized) {
                endpoints.push(normalized);
            }
        }
        if endpoints.is_empty() {
            return Err(PayError::InternalError(format!(
                "wallet {} has empty sol rpc endpoints; recreate wallet",
                meta.id
            )));
        }
        Ok(endpoints)
    }

    async fn rpc_call<T>(
        &self,
        endpoint: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T, PayError>
    where
        T: DeserializeOwned,
    {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });

        let response = self
            .http_client
            .post(endpoint)
            .json(&payload)
            .send()
            .await
            .map_err(|e| PayError::NetworkError(format!("sol rpc {method} request: {e}")))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| PayError::NetworkError(format!("sol rpc {method} read body: {e}")))?;

        if !status.is_success() {
            return Err(PayError::NetworkError(format!(
                "sol rpc {method} {}: {}",
                status.as_u16(),
                body
            )));
        }

        let envelope: SolRpcEnvelope<T> = serde_json::from_str(&body)
            .map_err(|e| PayError::NetworkError(format!("sol rpc {method} decode: {e}")))?;

        if let Some(error) = envelope.error {
            return Err(PayError::NetworkError(format!(
                "sol rpc {method} {}: {}",
                error.code, error.message
            )));
        }

        envelope
            .result
            .ok_or_else(|| PayError::NetworkError(format!("sol rpc {method} missing result field")))
    }

    async fn rpc_call_with_failover<T>(
        &self,
        endpoints: &[String],
        method: &str,
        params: serde_json::Value,
    ) -> Result<T, PayError>
    where
        T: DeserializeOwned,
    {
        let mut last_error: Option<String> = None;
        for endpoint in endpoints {
            match self.rpc_call(endpoint, method, params.clone()).await {
                Ok(result) => return Ok(result),
                Err(err) => {
                    last_error = Some(format!("endpoint={endpoint} err={err}"));
                }
            }
        }
        Err(PayError::NetworkError(format!(
            "all sol rpc endpoints failed for {method}; {}",
            last_error.unwrap_or_else(|| "no endpoints configured".to_string())
        )))
    }

    async fn fetch_chain_status(
        &self,
        endpoint: &str,
        transaction_id: &str,
    ) -> Result<Option<SolChainStatus>, PayError> {
        let result: SolGetSignatureStatusesResult = self
            .rpc_call(
                endpoint,
                "getSignatureStatuses",
                serde_json::json!([[transaction_id], {"searchTransactionHistory": true}]),
            )
            .await?;
        let Some(entry) = result.value.into_iter().next().flatten() else {
            return Ok(None);
        };

        if entry.err.is_some() {
            return Ok(Some(SolChainStatus {
                status: TxStatus::Failed,
                confirmations: entry.confirmations.map(|v| v as u32),
            }));
        }

        let status = match entry.confirmation_status.as_deref() {
            Some("finalized") | Some("confirmed") => TxStatus::Confirmed,
            Some("processed") => TxStatus::Pending,
            Some(_) => TxStatus::Pending,
            None => {
                if entry.confirmations.is_none() {
                    TxStatus::Confirmed
                } else {
                    TxStatus::Pending
                }
            }
        };
        Ok(Some(SolChainStatus {
            status,
            confirmations: entry.confirmations.map(|v| v as u32),
        }))
    }

    fn tx_status_from_chain(confirmation_status: Option<&str>, has_error: bool) -> TxStatus {
        if has_error {
            return TxStatus::Failed;
        }
        match confirmation_status {
            Some("finalized") | Some("confirmed") => TxStatus::Confirmed,
            Some("processed") | Some(_) => TxStatus::Pending,
            None => TxStatus::Pending,
        }
    }

    /// Derive the Associated Token Account address for (wallet, mint).
    fn derive_ata(wallet: &Pubkey, mint: &Pubkey) -> Result<Pubkey, PayError> {
        let token_program = Pubkey::from_str(SPL_TOKEN_PROGRAM_ID)
            .map_err(|e| PayError::InternalError(format!("invalid spl token program id: {e}")))?;
        let ata_program = Pubkey::from_str(SPL_ASSOCIATED_TOKEN_PROGRAM_ID)
            .map_err(|e| PayError::InternalError(format!("invalid ata program id: {e}")))?;
        let (ata, _bump) = Pubkey::find_program_address(
            &[wallet.as_ref(), token_program.as_ref(), mint.as_ref()],
            &ata_program,
        );
        Ok(ata)
    }

    /// Build a create-associated-token-account instruction.
    fn build_create_ata_instruction(
        funder: &Pubkey,
        owner: &Pubkey,
        mint: &Pubkey,
    ) -> Result<Instruction, PayError> {
        let token_program = Pubkey::from_str(SPL_TOKEN_PROGRAM_ID)
            .map_err(|e| PayError::InternalError(format!("invalid spl token program id: {e}")))?;
        let ata_program = Pubkey::from_str(SPL_ASSOCIATED_TOKEN_PROGRAM_ID)
            .map_err(|e| PayError::InternalError(format!("invalid ata program id: {e}")))?;
        let ata = Self::derive_ata(owner, mint)?;
        // System program: 11111111111111111111111111111111
        let system_program = Pubkey::default();
        Ok(Instruction {
            program_id: ata_program,
            accounts: vec![
                AccountMeta::new(*funder, true),
                AccountMeta::new(ata, false),
                AccountMeta::new_readonly(*owner, false),
                AccountMeta::new_readonly(*mint, false),
                AccountMeta::new_readonly(system_program, false),
                AccountMeta::new_readonly(token_program, false),
            ],
            data: vec![], // CreateAssociatedTokenAccount has no data
        })
    }

    /// Build an SPL token transfer_checked instruction.
    fn build_spl_transfer_instruction(
        source_ata: &Pubkey,
        mint: &Pubkey,
        dest_ata: &Pubkey,
        authority: &Pubkey,
        amount: u64,
        decimals: u8,
    ) -> Result<Instruction, PayError> {
        let token_program = Pubkey::from_str(SPL_TOKEN_PROGRAM_ID)
            .map_err(|e| PayError::InternalError(format!("invalid spl token program id: {e}")))?;
        // transfer_checked instruction data: [12u8, amount(8 bytes LE), decimals(1 byte)]
        let mut data = Vec::with_capacity(10);
        data.push(12u8); // transfer_checked discriminator
        data.extend_from_slice(&amount.to_le_bytes());
        data.push(decimals);
        Ok(Instruction {
            program_id: token_program,
            accounts: vec![
                AccountMeta::new(*source_ata, false),
                AccountMeta::new_readonly(*mint, false),
                AccountMeta::new(*dest_ata, false),
                AccountMeta::new_readonly(*authority, true),
            ],
            data,
        })
    }

    /// Check if an account exists on-chain (non-null value from getAccountInfo).
    async fn account_exists(&self, endpoints: &[String], address: &str) -> Result<bool, PayError> {
        let result: serde_json::Value = self
            .rpc_call_with_failover(
                endpoints,
                "getAccountInfo",
                serde_json::json!([address, {"encoding": "base64"}]),
            )
            .await?;
        Ok(result.get("value").is_some_and(|v| !v.is_null()))
    }

    /// Query SPL token accounts by owner and add known token balances to BalanceInfo.
    async fn enrich_with_token_balances(
        &self,
        endpoints: &[String],
        address: &str,
        custom_tokens: &[wallet::CustomToken],
        balance: &mut BalanceInfo,
    ) {
        // Detect cluster from first endpoint
        let cluster = endpoints
            .first()
            .map(|e| tokens::sol_cluster_from_endpoint(e))
            .unwrap_or("mainnet-beta");

        for known in tokens::sol_known_tokens(cluster) {
            self.query_spl_token_balance(
                endpoints,
                address,
                known.symbol,
                known.address,
                known.decimals,
                balance,
            )
            .await;
        }
        for ct in custom_tokens {
            self.query_spl_token_balance(
                endpoints,
                address,
                &ct.symbol,
                &ct.address,
                ct.decimals,
                balance,
            )
            .await;
        }
    }

    async fn query_spl_token_balance(
        &self,
        endpoints: &[String],
        address: &str,
        symbol: &str,
        mint_address: &str,
        decimals: u8,
        balance: &mut BalanceInfo,
    ) {
        let mint_pubkey = match Pubkey::from_str(mint_address) {
            Ok(p) => p,
            Err(_) => return,
        };
        let owner_pubkey = match Pubkey::from_str(address) {
            Ok(p) => p,
            Err(_) => return,
        };
        let ata = match Self::derive_ata(&owner_pubkey, &mint_pubkey) {
            Ok(a) => a,
            Err(_) => return,
        };
        let result: Result<serde_json::Value, _> = self
            .rpc_call_with_failover(
                endpoints,
                "getTokenAccountBalance",
                serde_json::json!([ata.to_string()]),
            )
            .await;
        if let Ok(val) = result {
            if let Some(amount_str) = val
                .get("value")
                .and_then(|v| v.get("amount"))
                .and_then(|v| v.as_str())
            {
                if let Ok(amount) = amount_str.parse::<u64>() {
                    if amount > 0 {
                        balance
                            .additional
                            .insert(format!("{symbol}_base_units"), amount);
                        balance
                            .additional
                            .insert(format!("{symbol}_decimals"), decimals as u64);
                    }
                }
            }
        }
    }

    fn build_memo_instruction(memo_text: &str, signer: &Pubkey) -> Result<Instruction, PayError> {
        let memo_program = Pubkey::from_str(SOL_MEMO_PROGRAM_ID)
            .map_err(|e| PayError::InternalError(format!("invalid memo program id: {e}")))?;
        Ok(Instruction {
            program_id: memo_program,
            accounts: vec![AccountMeta::new_readonly(*signer, true)],
            data: memo_text.as_bytes().to_vec(),
        })
    }

    fn extract_memo_from_transaction(tx: &SolGetTransactionResult) -> Option<String> {
        for ix in &tx.transaction.message.instructions {
            let Some(program_id) = tx.transaction.message.account_keys.get(ix.program_id_index)
            else {
                continue;
            };
            if program_id != SOL_MEMO_PROGRAM_ID || ix.data.trim().is_empty() {
                continue;
            }
            let memo_bytes = bs58::decode(&ix.data).into_vec().ok()?;
            let memo = String::from_utf8(memo_bytes).ok()?;
            if memo.trim().is_empty() {
                continue;
            }
            return Some(memo);
        }
        None
    }

    /// Extract reference keys from a transaction's transfer instructions.
    /// A reference key is any read-only non-signer account on a system transfer
    /// or SPL token transfer instruction that isn't a known program or the
    /// sender/recipient (per strain-payment-method-solana convention).
    fn extract_reference_keys(tx: &SolGetTransactionResult) -> Vec<String> {
        const KNOWN_PROGRAMS: &[&str] = &[
            "11111111111111111111111111111111", // System Program
            SPL_TOKEN_PROGRAM_ID,
            SPL_ASSOCIATED_TOKEN_PROGRAM_ID,
            SOL_MEMO_PROGRAM_ID,
            "SysvarRent111111111111111111111111111111111",
        ];
        let account_keys = &tx.transaction.message.account_keys;
        let mut refs = Vec::new();
        for ix in &tx.transaction.message.instructions {
            let Some(program_id) = account_keys.get(ix.program_id_index) else {
                continue;
            };
            // Only inspect system transfer or SPL token transfer instructions
            let is_transfer = program_id == "11111111111111111111111111111111"
                || program_id == SPL_TOKEN_PROGRAM_ID;
            if !is_transfer {
                continue;
            }
            // Account indices beyond the standard transfer accounts are reference keys.
            // System transfer: [from, to] = 2 accounts
            // SPL transfer_checked: [source_ata, mint, dest_ata, authority] = 4 accounts
            let expected_count = if program_id == SPL_TOKEN_PROGRAM_ID {
                4
            } else {
                2
            };
            for &acct_idx in ix.accounts.iter().skip(expected_count) {
                if let Some(key) = account_keys.get(acct_idx) {
                    if !KNOWN_PROGRAMS.contains(&key.as_str()) {
                        refs.push(key.clone());
                    }
                }
            }
        }
        refs
    }

    async fn fetch_recent_chain_signatures(
        &self,
        endpoints: &[String],
        address: &str,
        limit: usize,
    ) -> Result<Vec<SolAddressSignatureEntry>, PayError> {
        self.rpc_call_with_failover(
            endpoints,
            "getSignaturesForAddress",
            serde_json::json!([address, {"limit": limit}]),
        )
        .await
    }

    async fn fetch_chain_transaction_record(
        &self,
        endpoints: &[String],
        wallet_id: &str,
        wallet_address: &str,
        signature: &SolAddressSignatureEntry,
    ) -> Result<Option<HistoryRecord>, PayError> {
        let tx_value: serde_json::Value = self
            .rpc_call_with_failover(
                endpoints,
                "getTransaction",
                serde_json::json!([
                    signature.signature,
                    {
                        "encoding": "json",
                        "maxSupportedTransactionVersion": 0
                    }
                ]),
            )
            .await?;

        if tx_value.is_null() {
            return Ok(None);
        }

        let tx: SolGetTransactionResult = serde_json::from_value(tx_value).map_err(|e| {
            PayError::NetworkError(format!(
                "sol rpc getTransaction decode {}: {e}",
                signature.signature
            ))
        })?;

        let wallet_index = tx
            .transaction
            .message
            .account_keys
            .iter()
            .position(|key| key == wallet_address);
        let Some(wallet_index) = wallet_index else {
            return Ok(None);
        };

        let pre = tx.meta.pre_balances.get(wallet_index).copied().unwrap_or(0);
        let post = tx
            .meta
            .post_balances
            .get(wallet_index)
            .copied()
            .unwrap_or(0);
        if pre == post {
            return Ok(None);
        }

        let delta = post as i128 - pre as i128;
        let amount_value = if delta >= 0 {
            delta as u64
        } else {
            (-delta) as u64
        };
        let direction = if delta >= 0 {
            Direction::Receive
        } else {
            Direction::Send
        };

        let status = Self::tx_status_from_chain(
            signature.confirmation_status.as_deref(),
            signature.err.is_some() || tx.meta.err.is_some(),
        );
        let created_at_epoch_s = signature
            .block_time
            .or(tx.block_time)
            .unwrap_or_else(wallet::now_epoch_seconds);
        let confirmed_at_epoch_s = (status == TxStatus::Confirmed).then_some(created_at_epoch_s);

        let fee_amount = if tx.meta.fee > 0 {
            Some(Amount {
                value: tx.meta.fee,
                token: "lamports".to_string(),
            })
        } else {
            None
        };
        Ok(Some(HistoryRecord {
            transaction_id: signature.signature.clone(),
            wallet: wallet_id.to_string(),
            network: Network::Sol,
            direction,
            amount: Amount {
                value: amount_value,
                token: "lamports".to_string(),
            },
            status,
            onchain_memo: Self::extract_memo_from_transaction(&tx),
            local_memo: None,
            remote_addr: None,
            preimage: None,
            created_at_epoch_s,
            confirmed_at_epoch_s,
            fee: fee_amount,
            reference_keys: {
                let refs = Self::extract_reference_keys(&tx);
                if refs.is_empty() {
                    None
                } else {
                    Some(refs)
                }
            },
        }))
    }

    async fn fetch_chain_history_records(
        &self,
        wallet_id: &str,
        fetch_limit: usize,
    ) -> Result<Vec<HistoryRecord>, PayError> {
        let meta = self.load_sol_wallet(wallet_id)?;
        let endpoints = Self::rpc_endpoints_for_wallet(&meta)?;
        let address = Self::wallet_address(&meta)?;
        let signatures = self
            .fetch_recent_chain_signatures(&endpoints, &address, fetch_limit)
            .await?;

        let mut records = Vec::new();
        for signature in &signatures {
            match self
                .fetch_chain_transaction_record(&endpoints, wallet_id, &address, signature)
                .await
            {
                Ok(Some(record)) => records.push(record),
                Ok(None) => {}
                Err(_) => {}
            }
        }
        Ok(records)
    }

    async fn fetch_chain_record_for_wallet(
        &self,
        wallet_id: &str,
        transaction_id: &str,
    ) -> Result<Option<(HistoryRecord, Option<u32>)>, PayError> {
        let meta = self.load_sol_wallet(wallet_id)?;
        let endpoints = Self::rpc_endpoints_for_wallet(&meta)?;
        let address = Self::wallet_address(&meta)?;

        let Some(chain_status) = self
            .fetch_chain_status_for_wallet(wallet_id, transaction_id)
            .await?
        else {
            return Ok(None);
        };

        let tx_value: serde_json::Value = self
            .rpc_call_with_failover(
                &endpoints,
                "getTransaction",
                serde_json::json!([
                    transaction_id,
                    {
                        "encoding": "json",
                        "maxSupportedTransactionVersion": 0
                    }
                ]),
            )
            .await?;
        if tx_value.is_null() {
            return Ok(None);
        }

        let tx: SolGetTransactionResult = serde_json::from_value(tx_value).map_err(|e| {
            PayError::NetworkError(format!(
                "sol rpc getTransaction decode {transaction_id}: {e}"
            ))
        })?;
        let wallet_index = tx
            .transaction
            .message
            .account_keys
            .iter()
            .position(|key| key == &address);
        let Some(wallet_index) = wallet_index else {
            return Ok(None);
        };

        let pre = tx.meta.pre_balances.get(wallet_index).copied().unwrap_or(0);
        let post = tx
            .meta
            .post_balances
            .get(wallet_index)
            .copied()
            .unwrap_or(0);
        let delta = post as i128 - pre as i128;
        let direction = if delta >= 0 {
            Direction::Receive
        } else {
            Direction::Send
        };
        let amount_value = if delta >= 0 {
            delta as u64
        } else {
            (-delta) as u64
        };
        let created_at_epoch_s = tx.block_time.unwrap_or_else(wallet::now_epoch_seconds);
        let confirmed_at_epoch_s =
            (chain_status.status == TxStatus::Confirmed).then_some(created_at_epoch_s);

        let fee_amount = if tx.meta.fee > 0 {
            Some(Amount {
                value: tx.meta.fee,
                token: "lamports".to_string(),
            })
        } else {
            None
        };
        Ok(Some((
            HistoryRecord {
                transaction_id: transaction_id.to_string(),
                wallet: wallet_id.to_string(),
                network: Network::Sol,
                direction,
                amount: Amount {
                    value: amount_value,
                    token: "lamports".to_string(),
                },
                status: chain_status.status,
                onchain_memo: Self::extract_memo_from_transaction(&tx),
                local_memo: None,
                remote_addr: None,
                preimage: None,
                created_at_epoch_s,
                confirmed_at_epoch_s,
                fee: fee_amount,
                reference_keys: {
                    let refs = Self::extract_reference_keys(&tx);
                    if refs.is_empty() {
                        None
                    } else {
                        Some(refs)
                    }
                },
            },
            chain_status.confirmations,
        )))
    }

    async fn fetch_chain_record_across_wallets(
        &self,
        transaction_id: &str,
    ) -> Result<Option<(HistoryRecord, Option<u32>)>, PayError> {
        let wallets = self.store.list_wallet_metadata(Some(Network::Sol))?;
        for wallet in wallets {
            if let Some(record) = self
                .fetch_chain_record_for_wallet(&wallet.id, transaction_id)
                .await?
            {
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    async fn fetch_chain_status_for_wallet(
        &self,
        wallet_id: &str,
        transaction_id: &str,
    ) -> Result<Option<SolChainStatus>, PayError> {
        let meta = self.load_sol_wallet(wallet_id)?;
        let endpoints = Self::rpc_endpoints_for_wallet(&meta)?;
        let mut last_error: Option<PayError> = None;
        for endpoint in &endpoints {
            match self.fetch_chain_status(endpoint, transaction_id).await {
                Ok(status) => return Ok(status),
                Err(err) => {
                    last_error = Some(err);
                }
            }
        }
        match last_error {
            Some(err) => Err(err),
            None => Ok(None),
        }
    }

    async fn fetch_chain_status_across_wallets(
        &self,
        transaction_id: &str,
    ) -> Result<Option<SolChainStatus>, PayError> {
        let wallets = self.store.list_wallet_metadata(Some(Network::Sol))?;
        for meta in wallets {
            let Ok(endpoints) = Self::rpc_endpoints_for_wallet(&meta) else {
                continue;
            };
            for endpoint in &endpoints {
                match self.fetch_chain_status(endpoint, transaction_id).await {
                    Ok(Some(status)) => return Ok(Some(status)),
                    Ok(None) => {}
                    Err(_) => {}
                }
            }
        }
        Ok(None)
    }
}

#[derive(Debug, Deserialize)]
struct SolRpcEnvelope<T> {
    result: Option<T>,
    error: Option<SolRpcError>,
}

#[derive(Debug, Deserialize)]
struct SolRpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
struct SolGetBalanceResult {
    value: u64,
}

#[derive(Debug, Deserialize)]
struct SolGetLatestBlockhashResult {
    value: SolGetLatestBlockhashValue,
}

#[derive(Debug, Deserialize)]
struct SolGetLatestBlockhashValue {
    blockhash: String,
}

#[derive(Debug, Deserialize)]
struct SolGetSignatureStatusesResult {
    value: Vec<Option<SolSignatureStatusValue>>,
}

#[derive(Debug, Deserialize)]
struct SolSignatureStatusValue {
    confirmations: Option<u64>,
    err: Option<serde_json::Value>,
    #[serde(rename = "confirmationStatus")]
    confirmation_status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SolAddressSignatureEntry {
    signature: String,
    err: Option<serde_json::Value>,
    block_time: Option<u64>,
    confirmation_status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SolGetTransactionResult {
    meta: SolTransactionMeta,
    transaction: SolTransactionEnvelope,
    block_time: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SolTransactionMeta {
    pre_balances: Vec<u64>,
    post_balances: Vec<u64>,
    err: Option<serde_json::Value>,
    #[serde(default)]
    fee: u64,
}

#[derive(Debug, Deserialize)]
struct SolTransactionEnvelope {
    message: SolTransactionMessage,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SolTransactionMessage {
    account_keys: Vec<String>,
    #[serde(default)]
    instructions: Vec<SolCompiledInstruction>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SolCompiledInstruction {
    program_id_index: usize,
    #[serde(default)]
    accounts: Vec<usize>,
    #[serde(default)]
    data: String,
}

#[async_trait]
impl PayProvider for SolProvider {
    fn network(&self) -> Network {
        Network::Sol
    }

    fn writes_locally(&self) -> bool {
        true
    }

    async fn create_wallet(&self, request: &WalletCreateRequest) -> Result<WalletInfo, PayError> {
        let endpoints = if request.rpc_endpoints.is_empty() {
            return Err(PayError::InvalidAmount(
                "sol wallet requires --sol-rpc-endpoint (or rpc_endpoints in JSON)".to_string(),
            ));
        } else {
            let mut normalized = Vec::new();
            for ep in &request.rpc_endpoints {
                let n = Self::normalize_rpc_endpoint(ep)?;
                if !normalized.contains(&n) {
                    normalized.push(n);
                }
            }
            normalized
        };
        let mnemonic_str = if let Some(raw) = request.mnemonic_secret.as_deref() {
            let mnemonic: Mnemonic = raw.parse().map_err(|_| {
                PayError::InvalidAmount(
                    "invalid mnemonic-secret for sol wallet: expected BIP39 words".to_string(),
                )
            })?;
            mnemonic.words().collect::<Vec<_>>().join(" ")
        } else {
            let mut entropy = [0u8; 16];
            getrandom::fill(&mut entropy)
                .map_err(|e| PayError::InternalError(format!("rng failed: {e}")))?;
            let mnemonic = Mnemonic::from_entropy(&entropy)
                .map_err(|e| PayError::InternalError(format!("mnemonic gen: {e}")))?;
            mnemonic.words().collect::<Vec<_>>().join(" ")
        };
        let keypair = keypair_from_seed_phrase_and_passphrase(&mnemonic_str, "").map_err(|e| {
            PayError::InternalError(format!("build keypair from sol mnemonic: {e}"))
        })?;
        let address = keypair.pubkey().to_string();

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
            network: Network::Sol,
            label: normalized_label.clone(),
            mint_url: None,
            sol_rpc_endpoints: Some(endpoints),
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
            id: wallet_id,
            network: Network::Sol,
            address,
            label: normalized_label,
            mnemonic: None,
        })
    }

    async fn close_wallet(&self, wallet_id: &str) -> Result<(), PayError> {
        let balance = self.balance(wallet_id).await?;
        let non_zero_components = balance.non_zero_components();
        if !non_zero_components.is_empty() {
            let component_list = non_zero_components
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(PayError::InvalidAmount(format!(
                "wallet {wallet_id} has non-zero balance components ({component_list}); transfer funds first"
            )));
        }
        self.store.delete_wallet_metadata(wallet_id)
    }

    async fn list_wallets(&self) -> Result<Vec<WalletSummary>, PayError> {
        let wallets = self.store.list_wallet_metadata(Some(Network::Sol))?;
        Ok(wallets
            .into_iter()
            .map(|meta| {
                let address = Self::wallet_address(&meta)
                    .unwrap_or_else(|_| INVALID_SOL_WALLET_ADDRESS.to_string());
                sol_wallet_summary(meta, address)
            })
            .collect())
    }

    async fn balance(&self, wallet_id: &str) -> Result<BalanceInfo, PayError> {
        let resolved = self.resolve_wallet_id(wallet_id)?;
        let meta = self.load_sol_wallet(&resolved)?;
        let endpoints = Self::rpc_endpoints_for_wallet(&meta)?;
        let address = Self::wallet_address(&meta)?;
        let result: SolGetBalanceResult = self
            .rpc_call_with_failover(
                &endpoints,
                "getBalance",
                serde_json::json!([address, {"commitment": "confirmed"}]),
            )
            .await?;
        let custom_tokens = meta.custom_tokens.as_deref().unwrap_or_default();
        let lamports = result.value;
        let mut info = BalanceInfo::new(lamports, 0, "lamports");
        self.enrich_with_token_balances(&endpoints, &address, custom_tokens, &mut info)
            .await;
        Ok(info)
    }

    async fn balance_all(&self) -> Result<Vec<WalletBalanceItem>, PayError> {
        let wallets = self.store.list_wallet_metadata(Some(Network::Sol))?;
        let mut items = Vec::with_capacity(wallets.len());
        for meta in wallets {
            let custom_tokens = meta.custom_tokens.as_deref().unwrap_or_default().to_vec();
            let endpoints = Self::rpc_endpoints_for_wallet(&meta);
            let address = Self::wallet_address(&meta);
            let result = match (endpoints, address) {
                (Ok(endpoints), Ok(address)) => {
                    let rpc_result: Result<SolGetBalanceResult, PayError> = self
                        .rpc_call_with_failover(
                            &endpoints,
                            "getBalance",
                            serde_json::json!([address, {"commitment": "confirmed"}]),
                        )
                        .await;
                    match rpc_result {
                        Ok(v) => {
                            let mut info = BalanceInfo::new(v.value, 0, "lamports");
                            self.enrich_with_token_balances(
                                &endpoints,
                                &address,
                                &custom_tokens,
                                &mut info,
                            )
                            .await;
                            Ok(info)
                        }
                        Err(e) => Err(e),
                    }
                }
                (Err(e), _) | (_, Err(e)) => Err(e),
            };
            let summary_address = Self::wallet_address(&meta)
                .unwrap_or_else(|_| INVALID_SOL_WALLET_ADDRESS.to_string());
            let summary = sol_wallet_summary(meta, summary_address);
            match result {
                Ok(info) => items.push(WalletBalanceItem {
                    wallet: summary,
                    balance: Some(info),
                    error: None,
                }),
                Err(error) => items.push(WalletBalanceItem {
                    wallet: summary,
                    balance: None,
                    error: Some(error.to_string()),
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
        let resolved = self.resolve_wallet_id(wallet_id)?;
        let meta = self.load_sol_wallet(&resolved)?;
        let _ = Self::rpc_endpoints_for_wallet(&meta)?;
        Ok(ReceiveInfo {
            address: Some(Self::wallet_address(&meta)?),
            invoice: None,
            quote_id: None,
        })
    }

    async fn receive_claim(&self, _wallet: &str, _quote_id: &str) -> Result<u64, PayError> {
        Err(PayError::NotImplemented(
            "sol receive has no claim step".to_string(),
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
            "sol does not use cashu send".to_string(),
        ))
    }

    async fn cashu_receive(
        &self,
        _wallet: &str,
        _token: &str,
    ) -> Result<CashuReceiveResult, PayError> {
        Err(PayError::NotImplemented(
            "sol does not use cashu receive".to_string(),
        ))
    }

    async fn send(
        &self,
        wallet: &str,
        to: &str,
        onchain_memo: Option<&str>,
        _mints: Option<&[String]>,
    ) -> Result<SendResult, PayError> {
        let resolved_wallet_id = self.resolve_wallet_id(wallet)?;
        let meta = self.load_sol_wallet(&resolved_wallet_id)?;
        let endpoints = Self::rpc_endpoints_for_wallet(&meta)?;
        let transfer_target = Self::parse_transfer_target(to, &endpoints)?;
        let recipient_pubkey = Pubkey::from_str(&transfer_target.recipient_address)
            .map_err(|e| PayError::InvalidAmount(format!("invalid sol recipient address: {e}")))?;

        let keypair = Self::wallet_keypair(&meta)?;
        let memo_instruction = onchain_memo
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| Self::build_memo_instruction(text, &keypair.pubkey()))
            .transpose()?;

        // Build SPL token transfer instructions if token_mint is set
        let spl_instructions = if let Some(token_mint) = transfer_target.token_mint {
            let cluster = endpoints
                .first()
                .map(|e| tokens::sol_cluster_from_endpoint(e))
                .unwrap_or("mainnet-beta");
            let decimals = tokens::sol_known_tokens(cluster)
                .iter()
                .find(|t| Pubkey::from_str(t.address).ok().as_ref() == Some(&token_mint))
                .map(|t| t.decimals)
                .unwrap_or(6); // default to 6 decimals for unknown tokens

            let sender_ata = Self::derive_ata(&keypair.pubkey(), &token_mint)?;
            let recipient_ata = Self::derive_ata(&recipient_pubkey, &token_mint)?;

            let mut ixs = Vec::new();
            // Check if recipient ATA exists; if not, create it
            let recipient_ata_exists = self
                .account_exists(&endpoints, &recipient_ata.to_string())
                .await
                .unwrap_or(false);
            if !recipient_ata_exists {
                ixs.push(Self::build_create_ata_instruction(
                    &keypair.pubkey(),
                    &recipient_pubkey,
                    &token_mint,
                )?);
            }
            ixs.push(Self::build_spl_transfer_instruction(
                &sender_ata,
                &token_mint,
                &recipient_ata,
                &keypair.pubkey(),
                transfer_target.amount_lamports,
                decimals,
            )?);
            Some(ixs)
        } else {
            None
        };

        let mut last_error: Option<String> = None;
        let mut transaction_id: Option<String> = None;
        for endpoint in &endpoints {
            let latest_blockhash: SolGetLatestBlockhashResult = match self
                .rpc_call(
                    endpoint,
                    "getLatestBlockhash",
                    serde_json::json!([{"commitment":"confirmed"}]),
                )
                .await
            {
                Ok(result) => result,
                Err(err) => {
                    last_error = Some(format!("endpoint={endpoint} getLatestBlockhash: {err}"));
                    continue;
                }
            };

            let recent_blockhash = match Hash::from_str(&latest_blockhash.value.blockhash) {
                Ok(hash) => hash,
                Err(err) => {
                    last_error = Some(format!(
                        "endpoint={endpoint} invalid latest blockhash: {err}"
                    ));
                    continue;
                }
            };

            let mut instructions = Vec::new();
            if let Some(ix) = memo_instruction.as_ref() {
                instructions.push(ix.clone());
            }
            if let Some(ref spl_ixs) = spl_instructions {
                instructions.extend(spl_ixs.iter().cloned());
            } else {
                let transfer_ix = system_instruction::transfer(
                    &keypair.pubkey(),
                    &recipient_pubkey,
                    transfer_target.amount_lamports,
                );
                instructions.push(transfer_ix);
            }
            // Add reference key as read-only non-signer account on the transfer
            // instruction (per strain-payment-method-solana convention).
            if let Some(ref_key) = &transfer_target.reference {
                if let Some(last_ix) = instructions.last_mut() {
                    last_ix
                        .accounts
                        .push(AccountMeta::new_readonly(*ref_key, false));
                }
            }
            let transaction = Transaction::new_signed_with_payer(
                &instructions,
                Some(&keypair.pubkey()),
                &[&keypair],
                recent_blockhash,
            );
            let encoded_transaction = BASE64_STANDARD.encode(
                wincode::serialize(&transaction)
                    .map_err(|e| PayError::InternalError(format!("serialize transaction: {e}")))?,
            );

            match self
                .rpc_call(
                    endpoint,
                    "sendTransaction",
                    serde_json::json!([
                        encoded_transaction,
                        {
                            "encoding": "base64",
                            "preflightCommitment": "confirmed"
                        }
                    ]),
                )
                .await
            {
                Ok(signature) => {
                    transaction_id = Some(signature);
                    break;
                }
                Err(err) => {
                    last_error = Some(format!("endpoint={endpoint} sendTransaction: {err}"));
                }
            }
        }
        let transaction_id = transaction_id.ok_or_else(|| {
            PayError::NetworkError(format!(
                "all sol rpc endpoints failed for transfer: {}",
                last_error.unwrap_or_else(|| "unknown error".to_string())
            ))
        })?;

        let (amount_value, amount_token) = if transfer_target.token_mint.is_some() {
            (transfer_target.amount_lamports, "token-units".to_string())
        } else {
            (transfer_target.amount_lamports, "lamports".to_string())
        };

        // Try to fetch precise fee from the transaction; fallback to 5000 lamports estimate
        let tx_fee = {
            let mut fee_val = 5000u64; // Solana base fee per signature
            for ep in &endpoints {
                let result: Result<SolGetTransactionResult, _> = self
                    .rpc_call(
                        ep,
                        "getTransaction",
                        serde_json::json!([
                            transaction_id,
                            { "encoding": "json", "maxSupportedTransactionVersion": 0 }
                        ]),
                    )
                    .await;
                if let Ok(tx) = result {
                    if tx.meta.fee > 0 {
                        fee_val = tx.meta.fee;
                    }
                    break;
                }
            }
            fee_val
        };
        let fee_amount = Some(Amount {
            value: tx_fee,
            token: "lamports".to_string(),
        });

        let now = wallet::now_epoch_seconds();
        let record = HistoryRecord {
            transaction_id: transaction_id.clone(),
            wallet: resolved_wallet_id.clone(),
            network: Network::Sol,
            direction: Direction::Send,
            amount: Amount {
                value: amount_value,
                token: amount_token.clone(),
            },
            status: TxStatus::Pending,
            onchain_memo: onchain_memo.map(|v| v.to_string()),
            local_memo: None,
            remote_addr: Some(transfer_target.recipient_address.clone()),
            preimage: None,
            created_at_epoch_s: now,
            confirmed_at_epoch_s: None,
            fee: fee_amount.clone(),
            reference_keys: None,
        };
        let _ = self.store.append_transaction_record(&record);

        Ok(SendResult {
            wallet: resolved_wallet_id,
            transaction_id,
            amount: Amount {
                value: amount_value,
                token: amount_token,
            },
            fee: fee_amount,
            preimage: None,
        })
    }

    async fn send_quote(
        &self,
        wallet: &str,
        to: &str,
        _mints: Option<&[String]>,
    ) -> Result<SendQuoteInfo, PayError> {
        let resolved = self.resolve_wallet_id(wallet)?;
        let meta = self.load_sol_wallet(&resolved)?;
        let endpoints = Self::rpc_endpoints_for_wallet(&meta)?;
        let target = Self::parse_transfer_target(to, &endpoints)?;
        Ok(SendQuoteInfo {
            wallet: resolved,
            amount_native: target.amount_lamports,
            fee_estimate_native: 5000,
            fee_unit: "lamports".to_string(),
        })
    }

    async fn history_list(
        &self,
        wallet_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<HistoryRecord>, PayError> {
        let resolved = self.resolve_wallet_id(wallet_id)?;
        let _ = self.load_sol_wallet(&resolved)?;
        let mut local_records = self.store.load_wallet_transaction_records(&resolved)?;
        for record in &mut local_records {
            if record.status != TxStatus::Pending || record.network != Network::Sol {
                continue;
            }
            if let Ok(Some(chain_status)) = self
                .fetch_chain_status_for_wallet(&resolved, &record.transaction_id)
                .await
            {
                let confirmed_at_epoch_s = if chain_status.status == TxStatus::Confirmed {
                    Some(
                        record
                            .confirmed_at_epoch_s
                            .unwrap_or_else(wallet::now_epoch_seconds),
                    )
                } else {
                    None
                };
                if record.status != chain_status.status
                    || record.confirmed_at_epoch_s != confirmed_at_epoch_s
                {
                    let _ = self.store.update_transaction_record_status(
                        &record.transaction_id,
                        chain_status.status,
                        confirmed_at_epoch_s,
                    );
                    record.status = chain_status.status;
                    record.confirmed_at_epoch_s = confirmed_at_epoch_s;
                }
            }
        }

        let fetch_limit = limit
            .saturating_add(offset)
            .clamp(20, MAX_CHAIN_HISTORY_SCAN);
        let chain_records = self
            .fetch_chain_history_records(&resolved, fetch_limit)
            .await
            .unwrap_or_default();

        let mut merged_by_id: HashMap<String, HistoryRecord> = HashMap::new();
        for record in local_records {
            merged_by_id.insert(record.transaction_id.clone(), record);
        }
        for record in chain_records {
            merged_by_id
                .entry(record.transaction_id.clone())
                .or_insert(record);
        }

        let mut merged: Vec<HistoryRecord> = merged_by_id.into_values().collect();
        merged.sort_by(|a, b| b.created_at_epoch_s.cmp(&a.created_at_epoch_s));

        let start = merged.len().min(offset);
        let end = merged.len().min(offset.saturating_add(limit));
        Ok(merged[start..end].to_vec())
    }

    async fn history_status(&self, transaction_id: &str) -> Result<HistoryStatusInfo, PayError> {
        let local_record = self.store.find_transaction_record_by_id(transaction_id)?;
        let local_sol_record = local_record.filter(|r| r.network == Network::Sol);

        let chain_record = if let Some(record) = &local_sol_record {
            self.fetch_chain_record_for_wallet(&record.wallet, transaction_id)
                .await?
        } else {
            self.fetch_chain_record_across_wallets(transaction_id)
                .await?
        };

        if let Some((chain_item, confirmations)) = chain_record {
            let mut item = local_sol_record
                .clone()
                .unwrap_or_else(|| chain_item.clone());
            item.status = chain_item.status;
            if item.confirmed_at_epoch_s.is_none() {
                item.confirmed_at_epoch_s = chain_item.confirmed_at_epoch_s;
            }
            if item.onchain_memo.is_none() {
                item.onchain_memo = chain_item.onchain_memo;
            }
            if let Some(local) = local_sol_record.as_ref() {
                if local.status != item.status
                    || local.confirmed_at_epoch_s != item.confirmed_at_epoch_s
                {
                    let _ = self.store.update_transaction_record_status(
                        transaction_id,
                        item.status,
                        item.confirmed_at_epoch_s,
                    );
                }
            }
            return Ok(HistoryStatusInfo {
                transaction_id: transaction_id.to_string(),
                status: item.status,
                confirmations,
                preimage: None,
                item: Some(item),
            });
        }

        let chain_status = self
            .fetch_chain_status_across_wallets(transaction_id)
            .await?;
        if let Some(chain_status) = chain_status {
            let item = local_sol_record.clone().map(|mut local| {
                let confirmed_at_epoch_s = if chain_status.status == TxStatus::Confirmed {
                    Some(
                        local
                            .confirmed_at_epoch_s
                            .unwrap_or_else(wallet::now_epoch_seconds),
                    )
                } else {
                    None
                };
                if local.status != chain_status.status
                    || local.confirmed_at_epoch_s != confirmed_at_epoch_s
                {
                    let _ = self.store.update_transaction_record_status(
                        transaction_id,
                        chain_status.status,
                        confirmed_at_epoch_s,
                    );
                    local.status = chain_status.status;
                    local.confirmed_at_epoch_s = confirmed_at_epoch_s;
                }
                local
            });
            return Ok(HistoryStatusInfo {
                transaction_id: transaction_id.to_string(),
                status: chain_status.status,
                confirmations: chain_status.confirmations,
                preimage: None,
                item,
            });
        }

        if let Some(record) = local_sol_record {
            return Ok(HistoryStatusInfo {
                transaction_id: record.transaction_id.clone(),
                status: record.status,
                confirmations: None,
                preimage: record.preimage.clone(),
                item: Some(record),
            });
        }

        Err(PayError::WalletNotFound(format!(
            "transaction {transaction_id} not found"
        )))
    }

    async fn history_sync(
        &self,
        wallet_id: &str,
        limit: usize,
    ) -> Result<HistorySyncStats, PayError> {
        let resolved = self.resolve_wallet_id(wallet_id)?;
        let _ = self.load_sol_wallet(&resolved)?;

        let mut local_records = self.store.load_wallet_transaction_records(&resolved)?;
        let mut stats = HistorySyncStats::default();

        for record in &mut local_records {
            if record.network != Network::Sol {
                continue;
            }
            if record.status != TxStatus::Pending {
                continue;
            }
            stats.records_scanned = stats.records_scanned.saturating_add(1);
            if let Ok(Some(chain_status)) = self
                .fetch_chain_status_for_wallet(&resolved, &record.transaction_id)
                .await
            {
                let confirmed_at_epoch_s = if chain_status.status == TxStatus::Confirmed {
                    Some(
                        record
                            .confirmed_at_epoch_s
                            .unwrap_or_else(wallet::now_epoch_seconds),
                    )
                } else {
                    None
                };
                if record.status != chain_status.status
                    || record.confirmed_at_epoch_s != confirmed_at_epoch_s
                {
                    let _ = self.store.update_transaction_record_status(
                        &record.transaction_id,
                        chain_status.status,
                        confirmed_at_epoch_s,
                    );
                    record.status = chain_status.status;
                    record.confirmed_at_epoch_s = confirmed_at_epoch_s;
                    stats.records_updated = stats.records_updated.saturating_add(1);
                }
            }
        }

        let fetch_limit = limit.clamp(1, MAX_CHAIN_HISTORY_SCAN);
        let chain_records = self
            .fetch_chain_history_records(&resolved, fetch_limit)
            .await?;
        stats.records_scanned = stats.records_scanned.saturating_add(chain_records.len());

        let mut local_by_id: HashMap<String, HistoryRecord> = local_records
            .into_iter()
            .filter(|record| record.network == Network::Sol)
            .map(|record| (record.transaction_id.clone(), record))
            .collect();

        for chain_record in chain_records {
            if let Some(existing) = local_by_id.get(&chain_record.transaction_id) {
                if existing.status != chain_record.status
                    || existing.confirmed_at_epoch_s != chain_record.confirmed_at_epoch_s
                {
                    let _ = self.store.update_transaction_record_status(
                        &chain_record.transaction_id,
                        chain_record.status,
                        chain_record.confirmed_at_epoch_s,
                    );
                    stats.records_updated = stats.records_updated.saturating_add(1);
                }
                continue;
            }

            let _ = self.store.append_transaction_record(&chain_record);
            local_by_id.insert(chain_record.transaction_id.clone(), chain_record);
            stats.records_added = stats.records_added.saturating_add(1);
        }

        Ok(stats)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{SolGetTransactionResult, SolProvider, SOL_MEMO_PROGRAM_ID};
    use crate::provider::PayProvider;
    use crate::store::wallet::{self, WalletMetadata};
    use crate::store::StorageBackend;
    use crate::types::{Network, WalletCreateRequest};
    use solana_sdk::pubkey::Pubkey;
    use solana_sdk::signature::{keypair_from_seed_phrase_and_passphrase, Signer};
    use std::str::FromStr;
    use std::sync::Arc;

    #[cfg(feature = "redb")]
    fn test_store(data_dir: &str) -> Arc<StorageBackend> {
        Arc::new(StorageBackend::Redb(
            crate::store::redb_store::RedbStore::new(data_dir),
        ))
    }

    #[test]
    fn normalize_endpoint_adds_scheme() {
        let endpoint = SolProvider::normalize_rpc_endpoint("127.0.0.1:8899").unwrap();
        assert_eq!(endpoint, "http://127.0.0.1:8899");
    }

    #[test]
    fn normalize_rpc_endpoints_from_json_array() {
        let endpoints = SolProvider::normalize_rpc_endpoints(
            "[\"https://rpc-a.example\",\"rpc-b.example:8899\"]",
        )
        .unwrap();
        assert_eq!(
            endpoints,
            vec![
                "https://rpc-a.example".to_string(),
                "http://rpc-b.example:8899".to_string()
            ]
        );
    }

    #[test]
    fn rpc_endpoints_for_wallet_requires_new_field() {
        let meta = WalletMetadata {
            id: "w_old0001".to_string(),
            network: Network::Sol,
            label: None,
            mint_url: Some("https://api.mainnet-beta.solana.com".to_string()),
            sol_rpc_endpoints: None,
            evm_rpc_endpoints: None,
            evm_chain_id: None,
            seed_secret: Some(
                "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".to_string(),
            ),
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
        let err = SolProvider::rpc_endpoints_for_wallet(&meta).unwrap_err();
        assert!(err.to_string().contains("missing sol rpc endpoints"));
    }

    #[test]
    fn parse_transfer_target_success() {
        let target = SolProvider::parse_transfer_target(
            "solana:8nTKRhLQDcnCaS5s8Z4KZPb1i9ddfbfQDeJpw7g4QxjV?amount-lamports=123",
            &[],
        )
        .unwrap();
        assert_eq!(target.amount_lamports, 123);
        assert_eq!(
            target.recipient_address,
            "8nTKRhLQDcnCaS5s8Z4KZPb1i9ddfbfQDeJpw7g4QxjV"
        );
        assert!(target.token_mint.is_none());
    }

    #[test]
    fn parse_transfer_target_missing_amount_fails() {
        let error = SolProvider::parse_transfer_target(
            "solana:8nTKRhLQDcnCaS5s8Z4KZPb1i9ddfbfQDeJpw7g4QxjV",
            &[],
        )
        .unwrap_err();
        assert!(error.to_string().contains("amount"));
    }

    #[test]
    fn parse_transfer_target_with_usdc_token() {
        let endpoints = vec!["https://api.mainnet-beta.solana.com".to_string()];
        let target = SolProvider::parse_transfer_target(
            "solana:8nTKRhLQDcnCaS5s8Z4KZPb1i9ddfbfQDeJpw7g4QxjV?amount-lamports=1000000&token=usdc",
            &endpoints,
        )
        .unwrap();
        assert_eq!(target.amount_lamports, 1_000_000);
        assert!(target.token_mint.is_some());
        assert_eq!(
            target.token_mint.unwrap().to_string(),
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
        );
    }

    #[test]
    fn parse_transfer_target_with_devnet_usdc() {
        let endpoints = vec!["https://api.devnet.solana.com".to_string()];
        let target = SolProvider::parse_transfer_target(
            "solana:8nTKRhLQDcnCaS5s8Z4KZPb1i9ddfbfQDeJpw7g4QxjV?amount-lamports=500000&token=usdc",
            &endpoints,
        )
        .unwrap();
        assert!(target.token_mint.is_some());
        assert_eq!(
            target.token_mint.unwrap().to_string(),
            "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU"
        );
    }

    #[test]
    fn parse_transfer_target_with_raw_mint_address() {
        let mint = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
        let target = SolProvider::parse_transfer_target(
            &format!("solana:8nTKRhLQDcnCaS5s8Z4KZPb1i9ddfbfQDeJpw7g4QxjV?amount-lamports=100&token={mint}"),
            &[],
        )
        .unwrap();
        assert_eq!(target.token_mint.unwrap().to_string(), mint);
    }

    #[test]
    fn derive_ata_deterministic() {
        let wallet = Pubkey::from_str("8nTKRhLQDcnCaS5s8Z4KZPb1i9ddfbfQDeJpw7g4QxjV").unwrap();
        let mint = Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();
        let ata1 = SolProvider::derive_ata(&wallet, &mint).unwrap();
        let ata2 = SolProvider::derive_ata(&wallet, &mint).unwrap();
        assert_eq!(ata1, ata2);
        // ATA should be different from both wallet and mint
        assert_ne!(ata1, wallet);
        assert_ne!(ata1, mint);
    }

    #[test]
    fn spl_transfer_instruction_encoding() {
        let source = Pubkey::from_str("8nTKRhLQDcnCaS5s8Z4KZPb1i9ddfbfQDeJpw7g4QxjV").unwrap();
        let mint = Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();
        let dest = Pubkey::from_str("7YWbWN4E6TQVYAPEZyyRhhmQvawbcSbPVFepW1uCNooe").unwrap();
        let authority = Pubkey::from_str("8nTKRhLQDcnCaS5s8Z4KZPb1i9ddfbfQDeJpw7g4QxjV").unwrap();
        let ix = SolProvider::build_spl_transfer_instruction(
            &source, &mint, &dest, &authority, 1_000_000, 6,
        )
        .unwrap();
        // data: 1 byte discriminator + 8 bytes amount + 1 byte decimals = 10 bytes
        assert_eq!(ix.data.len(), 10);
        assert_eq!(ix.data[0], 12); // transfer_checked discriminator
        assert_eq!(ix.data[9], 6); // decimals
                                   // 4 accounts: source, mint, dest, authority
        assert_eq!(ix.accounts.len(), 4);
    }

    #[test]
    fn keypair_from_mnemonic_secret() {
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let keypair = SolProvider::keypair_from_seed_secret(mnemonic).unwrap();
        let expected = keypair_from_seed_phrase_and_passphrase(mnemonic, "").unwrap();
        assert_eq!(keypair.pubkey(), expected.pubkey());
    }

    #[test]
    fn keypair_from_non_mnemonic_secret_fails() {
        let err = SolProvider::keypair_from_seed_secret("not-a-valid-mnemonic").unwrap_err();
        assert!(err.to_string().contains("expected BIP39 mnemonic words"));
    }

    #[test]
    fn extract_memo_from_transaction_returns_memo_text() {
        let memo_text = "order:ord_123";
        let tx_value = serde_json::json!({
            "meta": {
                "preBalances": [10, 0],
                "postBalances": [9, 1],
                "err": null
            },
            "transaction": {
                "message": {
                    "accountKeys": [
                        "11111111111111111111111111111111",
                        SOL_MEMO_PROGRAM_ID
                    ],
                    "instructions": [
                        {
                            "programIdIndex": 1,
                            "data": bs58::encode(memo_text.as_bytes()).into_string()
                        }
                    ]
                }
            },
            "blockTime": 1772808557u64
        });
        let tx: SolGetTransactionResult = serde_json::from_value(tx_value).unwrap();
        let extracted = SolProvider::extract_memo_from_transaction(&tx);
        assert_eq!(extracted.as_deref(), Some(memo_text));
    }

    #[test]
    fn extract_memo_from_transaction_returns_none_when_missing() {
        let tx_value = serde_json::json!({
            "meta": {
                "preBalances": [10, 0],
                "postBalances": [9, 1],
                "err": null
            },
            "transaction": {
                "message": {
                    "accountKeys": [
                        "11111111111111111111111111111111"
                    ],
                    "instructions": [
                        {
                            "programIdIndex": 0,
                            "data": bs58::encode(b"not-memo").into_string()
                        }
                    ]
                }
            },
            "blockTime": 1772808557u64
        });
        let tx: SolGetTransactionResult = serde_json::from_value(tx_value).unwrap();
        assert!(SolProvider::extract_memo_from_transaction(&tx).is_none());
    }

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn list_wallets_tolerates_invalid_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_string_lossy().to_string();
        let provider = SolProvider::new(&data_dir, test_store(&data_dir));
        let endpoint = "https://api.devnet.solana.com".to_string();

        let valid_mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let valid_address = keypair_from_seed_phrase_and_passphrase(valid_mnemonic, "")
            .unwrap()
            .pubkey()
            .to_string();

        wallet::save_wallet_metadata(
            &data_dir,
            &WalletMetadata {
                id: "w_good0001".to_string(),
                network: Network::Sol,
                label: Some("good".to_string()),
                mint_url: Some(endpoint.clone()),
                sol_rpc_endpoints: Some(vec![endpoint.clone()]),
                evm_rpc_endpoints: None,
                evm_chain_id: None,
                seed_secret: Some(valid_mnemonic.to_string()),
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
            },
        )
        .unwrap();

        wallet::save_wallet_metadata(
            &data_dir,
            &WalletMetadata {
                id: "w_bad0002".to_string(),
                network: Network::Sol,
                label: Some("bad".to_string()),
                mint_url: Some(endpoint),
                sol_rpc_endpoints: Some(vec!["https://api.devnet.solana.com".to_string()]),
                evm_rpc_endpoints: None,
                evm_chain_id: None,
                seed_secret: Some("not-a-valid-mnemonic".to_string()),
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
            },
        )
        .unwrap();

        let wallets = provider.list_wallets().await.unwrap();
        assert_eq!(wallets.len(), 2);
        let good = wallets.iter().find(|w| w.id == "w_good0001").unwrap();
        assert_eq!(good.address, valid_address);
        let bad = wallets.iter().find(|w| w.id == "w_bad0002").unwrap();
        assert_eq!(bad.address, "invalid:sol-wallet-secret");
    }

    #[cfg(feature = "redb")]
    #[tokio::test]
    async fn send_quote_resolves_wallet_identifier() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_string_lossy().to_string();
        let provider = SolProvider::new(&data_dir, test_store(&data_dir));
        let mnemonic =
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

        let wallet = provider
            .create_wallet(&WalletCreateRequest {
                label: "quote-wallet".to_string(),
                mint_url: None,
                rpc_endpoints: vec!["https://api.devnet.solana.com".to_string()],
                chain_id: None,
                mnemonic_secret: Some(mnemonic.to_string()),
                btc_esplora_url: None,
                btc_network: None,
                btc_address_type: None,
                btc_backend: None,
                btc_core_url: None,
                btc_core_auth_secret: None,
                btc_electrum_url: None,
            })
            .await
            .expect("create wallet");

        let quote = provider
            .send_quote(
                "",
                &format!("solana:{}?amount=1000&token=native", wallet.address),
                None,
            )
            .await
            .expect("send quote should resolve single wallet");

        assert_eq!(quote.wallet, wallet.id);
        assert_eq!(quote.amount_native, 1000);
        assert_eq!(quote.fee_unit, "lamports");
    }
}
