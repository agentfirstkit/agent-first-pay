use crate::provider::{HistorySyncStats, PayError, PayProvider};
use crate::spend::tokens;
use crate::store::wallet::{self, WalletMetadata};
use crate::store::{PayStore, StorageBackend};
use crate::types::*;
use alloy::network::EthereumWallet;
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::{coins_bip39::English, MnemonicBuilder, PrivateKeySigner};
use async_trait::async_trait;
use bip39::Mnemonic;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn evm_wallet_summary(meta: WalletMetadata, address: String) -> WalletSummary {
    WalletSummary {
        id: meta.id,
        network: Network::Evm,
        label: meta.label,
        address,
        backend: None,
        mint_url: None,
        rpc_endpoints: meta.evm_rpc_endpoints,
        chain_id: meta.evm_chain_id,
        created_at_epoch_s: meta.created_at_epoch_s,
    }
}

pub struct EvmProvider {
    _data_dir: String,
    http_client: reqwest::Client,
    store: Arc<StorageBackend>,
}

const INVALID_EVM_WALLET_ADDRESS: &str = "invalid:evm-wallet-secret";

// Well-known chain IDs
const CHAIN_ID_BASE: u64 = 8453;

// Legacy USDC contract address resolver — kept for backward-compat tests.
// New code uses tokens::resolve_evm_token().
#[cfg(test)]
fn usdc_contract_address(chain_id: u64) -> Option<Address> {
    tokens::resolve_evm_token(chain_id, "usdc").and_then(|t| t.address.parse().ok())
}

#[derive(Debug, Clone)]
struct EvmTransferTarget {
    recipient_address: Address,
    amount_wei: U256,
    /// If set, this is an ERC-20 token transfer instead of the chain's native token.
    token_contract: Option<Address>,
}

impl EvmProvider {
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
                "evm wallet requires --evm-rpc-endpoint".to_string(),
            ));
        }
        let endpoint = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            trimmed.to_string()
        } else {
            format!("https://{trimmed}")
        };
        reqwest::Url::parse(&endpoint)
            .map_err(|e| PayError::InvalidAmount(format!("invalid --evm-rpc-endpoint: {e}")))?;
        Ok(endpoint)
    }

    fn signer_from_mnemonic(mnemonic_str: &str) -> Result<PrivateKeySigner, PayError> {
        // Derive EVM key from BIP39 mnemonic using BIP44 path m/44'/60'/0'/0/0
        MnemonicBuilder::<English>::default()
            .phrase(mnemonic_str)
            .index(0u32)
            .map_err(|e| PayError::InternalError(format!("evm derivation index: {e}")))?
            .build()
            .map_err(|e| PayError::InternalError(format!("build evm signer from mnemonic: {e}")))
    }

    fn wallet_signer(meta: &WalletMetadata) -> Result<PrivateKeySigner, PayError> {
        let seed_secret = meta.seed_secret.as_deref().ok_or_else(|| {
            PayError::InternalError(format!("wallet {} missing evm secret", meta.id))
        })?;
        Self::signer_from_mnemonic(seed_secret)
    }

    fn wallet_address(meta: &WalletMetadata) -> Result<String, PayError> {
        Ok(format!("{:?}", Self::wallet_signer(meta)?.address()))
    }

    fn rpc_endpoints_for_wallet(meta: &WalletMetadata) -> Result<Vec<String>, PayError> {
        meta.evm_rpc_endpoints
            .as_ref()
            .filter(|v| !v.is_empty())
            .cloned()
            .ok_or_else(|| {
                PayError::InternalError(format!(
                    "wallet {} missing evm rpc endpoints; re-create with --evm-rpc-endpoint",
                    meta.id
                ))
            })
    }

    fn chain_id_for_wallet(meta: &WalletMetadata) -> u64 {
        meta.evm_chain_id.unwrap_or(CHAIN_ID_BASE)
    }

    fn load_evm_wallet(&self, wallet_id: &str) -> Result<WalletMetadata, PayError> {
        let meta = self.store.load_wallet_metadata(wallet_id)?;
        if meta.network != Network::Evm {
            return Err(PayError::WalletNotFound(format!(
                "wallet {wallet_id} is not an evm wallet"
            )));
        }
        Ok(meta)
    }

    fn resolve_wallet_id(&self, wallet_id: &str) -> Result<String, PayError> {
        if wallet_id.is_empty() {
            let wallets = self.store.list_wallet_metadata(Some(Network::Evm))?;
            if wallets.len() == 1 {
                return Ok(wallets[0].id.clone());
            }
            return Err(PayError::InvalidAmount(
                "multiple evm wallets exist; specify --wallet".to_string(),
            ));
        }
        Ok(wallet_id.to_string())
    }

    fn parse_transfer_target(to: &str, chain_id: u64) -> Result<EvmTransferTarget, PayError> {
        let trimmed = to.trim();
        if trimmed.is_empty() {
            return Err(PayError::InvalidAmount(
                "evm send target is empty".to_string(),
            ));
        }
        // Format: ethereum:<address>?amount=<amount>&token=native
        // or:     ethereum:<address>?amount-wei=<amount> (legacy alias)
        // or:     ethereum:<address>?amount-gwei=<amount> (legacy alias, gwei×1e9→wei)
        let no_scheme = trimmed.strip_prefix("ethereum:").unwrap_or(trimmed);
        let (recipient_str, query) = match no_scheme.split_once('?') {
            Some(parts) => parts,
            None => (no_scheme, ""),
        };
        let recipient_address: Address = recipient_str
            .trim()
            .parse()
            .map_err(|e| PayError::InvalidAmount(format!("invalid evm recipient address: {e}")))?;

        let mut amount_wei: Option<U256> = None;
        let mut token_contract: Option<Address> = None;

        for pair in query.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (key, value) = pair
                .split_once('=')
                .ok_or_else(|| PayError::InvalidAmount(format!("invalid query pair: {pair}")))?;
            match key {
                "amount" | "amount-wei" => {
                    amount_wei =
                        Some(value.parse::<U256>().map_err(|e| {
                            PayError::InvalidAmount(format!("invalid amount: {e}"))
                        })?);
                }
                "amount-gwei" => {
                    let gwei: u64 = value.parse().map_err(|e| {
                        PayError::InvalidAmount(format!("invalid amount-gwei: {e}"))
                    })?;
                    amount_wei = Some(U256::from(gwei) * U256::from(1_000_000_000u64));
                }
                "token" => {
                    if value == "native" {
                        // Explicit native token — no ERC-20 contract
                    } else if let Some(known) = tokens::resolve_evm_token(chain_id, value) {
                        token_contract = known.address.parse().ok();
                        if token_contract.is_none() {
                            return Err(PayError::InvalidAmount(format!(
                                "failed to parse known token address for {value}"
                            )));
                        }
                    } else if value.starts_with("0x") || value.starts_with("0X") {
                        token_contract = Some(value.parse().map_err(|e| {
                            PayError::InvalidAmount(format!("invalid token contract address: {e}"))
                        })?);
                    } else {
                        return Err(PayError::InvalidAmount(format!(
                            "unknown token '{value}' on chain_id {chain_id}; use a known symbol (native, usdc, usdt) or contract address"
                        )));
                    }
                }
                _ => {
                    // ignore unknown query params
                }
            }
        }

        let amount_wei = amount_wei.ok_or_else(|| {
            PayError::InvalidAmount(
                "evm send target missing amount; use ethereum:<address>?amount=<u64>&token=native"
                    .to_string(),
            )
        })?;

        Ok(EvmTransferTarget {
            recipient_address,
            amount_wei,
            token_contract,
        })
    }

    // Provider is built inline in withdraw() to avoid complex generic return types.

    /// Get ETH balance for an address via JSON-RPC (raw reqwest, no alloy provider needed).
    async fn get_balance_raw(&self, endpoints: &[String], address: &str) -> Result<U256, PayError> {
        let mut last_error: Option<String> = None;
        for endpoint in endpoints {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_getBalance",
                "params": [address, "latest"],
                "id": 1
            });
            match self.http_client.post(endpoint).json(&body).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    if !status.is_success() {
                        last_error =
                            Some(format!("endpoint={endpoint} status={status} body={text}"));
                        continue;
                    }
                    let parsed: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
                        PayError::NetworkError(format!("endpoint={endpoint} invalid json: {e}"))
                    })?;
                    if let Some(err) = parsed.get("error") {
                        last_error = Some(format!("endpoint={endpoint} rpc error: {err}"));
                        continue;
                    }
                    let result_hex =
                        parsed
                            .get("result")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                PayError::NetworkError(format!(
                                    "endpoint={endpoint} missing result in response"
                                ))
                            })?;
                    let balance = U256::from_str_radix(
                        result_hex.strip_prefix("0x").unwrap_or(result_hex),
                        16,
                    )
                    .map_err(|e| {
                        PayError::NetworkError(format!(
                            "endpoint={endpoint} invalid balance hex: {e}"
                        ))
                    })?;
                    return Ok(balance);
                }
                Err(e) => {
                    last_error = Some(format!("endpoint={endpoint} request failed: {e}"));
                }
            }
        }
        Err(PayError::NetworkError(format!(
            "all evm rpc endpoints failed: {}",
            last_error.unwrap_or_default()
        )))
    }

    /// Get ERC-20 token balance for an address via `eth_call` (balanceOf).
    async fn get_erc20_balance_raw(
        &self,
        endpoints: &[String],
        token_contract: &str,
        address: &str,
    ) -> Result<U256, PayError> {
        // balanceOf(address) selector: 0x70a08231
        let addr_no_prefix = address.strip_prefix("0x").unwrap_or(address);
        let calldata = format!("0x70a08231000000000000000000000000{addr_no_prefix}");
        let mut last_error: Option<String> = None;
        for endpoint in endpoints {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_call",
                "params": [
                    {"to": token_contract, "data": calldata},
                    "latest"
                ],
                "id": 1
            });
            match self.http_client.post(endpoint).json(&body).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    if !status.is_success() {
                        last_error =
                            Some(format!("endpoint={endpoint} status={status} body={text}"));
                        continue;
                    }
                    let parsed: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
                        PayError::NetworkError(format!("endpoint={endpoint} invalid json: {e}"))
                    })?;
                    if let Some(err) = parsed.get("error") {
                        last_error = Some(format!("endpoint={endpoint} rpc error: {err}"));
                        continue;
                    }
                    let result_hex = parsed
                        .get("result")
                        .and_then(|v| v.as_str())
                        .unwrap_or("0x0");
                    let balance = U256::from_str_radix(
                        result_hex.strip_prefix("0x").unwrap_or(result_hex),
                        16,
                    )
                    .map_err(|e| {
                        PayError::NetworkError(format!(
                            "endpoint={endpoint} invalid balanceOf hex: {e}"
                        ))
                    })?;
                    return Ok(balance);
                }
                Err(e) => {
                    last_error = Some(format!("endpoint={endpoint} request failed: {e}"));
                }
            }
        }
        Err(PayError::NetworkError(format!(
            "all evm rpc endpoints failed for balanceOf: {}",
            last_error.unwrap_or_default()
        )))
    }

    /// Query token balances for known tokens and custom tokens, adding to BalanceInfo.additional.
    async fn enrich_with_token_balances(
        &self,
        endpoints: &[String],
        address: &str,
        chain_id: u64,
        custom_tokens: &[wallet::CustomToken],
        balance: &mut BalanceInfo,
    ) {
        for known in tokens::evm_known_tokens(chain_id) {
            if let Ok(raw) = self
                .get_erc20_balance_raw(endpoints, known.address, address)
                .await
            {
                let val: u64 = raw.try_into().unwrap_or(u64::MAX);
                if val > 0 {
                    balance
                        .additional
                        .insert(format!("{}_base_units", known.symbol), val);
                    balance
                        .additional
                        .insert(format!("{}_decimals", known.symbol), known.decimals as u64);
                }
            }
        }
        for ct in custom_tokens {
            if let Ok(raw) = self
                .get_erc20_balance_raw(endpoints, &ct.address, address)
                .await
            {
                let val: u64 = raw.try_into().unwrap_or(u64::MAX);
                if val > 0 {
                    balance
                        .additional
                        .insert(format!("{}_base_units", ct.symbol), val);
                    balance
                        .additional
                        .insert(format!("{}_decimals", ct.symbol), ct.decimals as u64);
                }
            }
        }
    }

    /// Make a single JSON-RPC call, returning the hex string result.
    async fn json_rpc_hex(
        &self,
        endpoint: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<String, String> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": 1
        });
        let resp = self
            .http_client
            .post(endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("endpoint={endpoint} {method}: {e}"))?;
        let text = resp.text().await.unwrap_or_default();
        let parsed: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("invalid json: {e}"))?;
        if let Some(err) = parsed.get("error") {
            return Err(format!("endpoint={endpoint} {method} rpc error: {err}"));
        }
        parsed
            .get("result")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("endpoint={endpoint} {method}: missing result"))
    }

    /// Estimate gas fee in gwei using eth_estimateGas + eth_gasPrice.
    async fn estimate_fee_gwei(
        &self,
        endpoints: &[String],
        from: &str,
        to_addr: &str,
        data: Option<&str>,
    ) -> Result<u64, PayError> {
        let mut last_error: Option<String> = None;
        for endpoint in endpoints {
            let tx_obj = if let Some(d) = data {
                serde_json::json!({ "from": from, "to": to_addr, "data": d })
            } else {
                serde_json::json!({ "from": from, "to": to_addr })
            };
            let gas_hex = match self
                .json_rpc_hex(endpoint, "eth_estimateGas", serde_json::json!([tx_obj]))
                .await
            {
                Ok(h) => h,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };
            let price_hex = match self
                .json_rpc_hex(endpoint, "eth_gasPrice", serde_json::json!([]))
                .await
            {
                Ok(h) => h,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };
            let gas = u128::from_str_radix(gas_hex.strip_prefix("0x").unwrap_or(&gas_hex), 16)
                .unwrap_or(21000);
            let price =
                u128::from_str_radix(price_hex.strip_prefix("0x").unwrap_or(&price_hex), 16)
                    .unwrap_or(0);
            let fee_wei = gas.saturating_mul(price);
            return Ok((fee_wei / 1_000_000_000) as u64);
        }
        Err(PayError::NetworkError(format!(
            "estimate_fee failed: {}",
            last_error.unwrap_or_default()
        )))
    }

    /// Get the current block number.
    async fn get_block_number_raw(&self, endpoints: &[String]) -> Result<u64, PayError> {
        let mut last_error: Option<String> = None;
        for endpoint in endpoints {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_blockNumber",
                "params": [],
                "id": 1
            });
            match self.http_client.post(endpoint).json(&body).send().await {
                Ok(resp) => {
                    let text = resp.text().await.unwrap_or_default();
                    let parsed: serde_json::Value = serde_json::from_str(&text)
                        .map_err(|e| PayError::NetworkError(format!("invalid json: {e}")))?;
                    if let Some(err) = parsed.get("error") {
                        last_error = Some(format!("endpoint={endpoint} rpc error: {err}"));
                        continue;
                    }
                    let hex = parsed
                        .get("result")
                        .and_then(|v| v.as_str())
                        .unwrap_or("0x0");
                    let num =
                        u64::from_str_radix(hex.strip_prefix("0x").unwrap_or(hex), 16).unwrap_or(0);
                    return Ok(num);
                }
                Err(e) => {
                    last_error = Some(format!("endpoint={endpoint}: {e}"));
                }
            }
        }
        Err(PayError::NetworkError(format!(
            "eth_blockNumber failed: {}",
            last_error.unwrap_or_default()
        )))
    }

    /// Get transaction receipt to check confirmation status.
    async fn get_transaction_receipt_raw(
        &self,
        endpoints: &[String],
        tx_hash: &str,
    ) -> Result<Option<EvmTxReceipt>, PayError> {
        let mut last_error: Option<String> = None;
        for endpoint in endpoints {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_getTransactionReceipt",
                "params": [tx_hash],
                "id": 1
            });
            match self.http_client.post(endpoint).json(&body).send().await {
                Ok(resp) => {
                    let text = resp.text().await.unwrap_or_default();
                    let parsed: serde_json::Value = serde_json::from_str(&text)
                        .map_err(|e| PayError::NetworkError(format!("invalid json: {e}")))?;
                    if let Some(err) = parsed.get("error") {
                        last_error = Some(format!("endpoint={endpoint} rpc error: {err}"));
                        continue;
                    }
                    let result = parsed.get("result");
                    if result.is_none() || result == Some(&serde_json::Value::Null) {
                        return Ok(None); // pending
                    }
                    let receipt: EvmTxReceipt =
                        serde_json::from_value(result.cloned().unwrap_or_default())
                            .map_err(|e| PayError::NetworkError(format!("parse receipt: {e}")))?;
                    return Ok(Some(receipt));
                }
                Err(e) => {
                    last_error = Some(format!("endpoint={endpoint}: {e}"));
                }
            }
        }
        Err(PayError::NetworkError(format!(
            "eth_getTransactionReceipt failed: {}",
            last_error.unwrap_or_default()
        )))
    }

    async fn get_transaction_input_raw(
        &self,
        endpoints: &[String],
        tx_hash: &str,
    ) -> Result<Option<Vec<u8>>, PayError> {
        let mut last_error: Option<String> = None;
        for endpoint in endpoints {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_getTransactionByHash",
                "params": [tx_hash],
                "id": 1
            });
            match self.http_client.post(endpoint).json(&body).send().await {
                Ok(resp) => {
                    let text = resp.text().await.unwrap_or_default();
                    let parsed: serde_json::Value = serde_json::from_str(&text)
                        .map_err(|e| PayError::NetworkError(format!("invalid json: {e}")))?;
                    if let Some(err) = parsed.get("error") {
                        last_error = Some(format!("endpoint={endpoint} rpc error: {err}"));
                        continue;
                    }
                    let result = parsed.get("result");
                    if result.is_none() || result == Some(&serde_json::Value::Null) {
                        return Ok(None);
                    }
                    let tx: EvmTxByHash = serde_json::from_value(
                        result.cloned().unwrap_or_default(),
                    )
                    .map_err(|e| PayError::NetworkError(format!("parse transaction: {e}")))?;
                    let input = tx.input.as_deref().unwrap_or("0x");
                    return Ok(Some(decode_hex_data_bytes(input)?));
                }
                Err(e) => {
                    last_error = Some(format!("endpoint={endpoint}: {e}"));
                }
            }
        }
        Err(PayError::NetworkError(format!(
            "eth_getTransactionByHash failed: {}",
            last_error.unwrap_or_default()
        )))
    }

    async fn get_block_with_transactions_raw(
        &self,
        endpoints: &[String],
        block_number: u64,
    ) -> Result<Option<EvmBlockByNumber>, PayError> {
        let block_hex = format!("0x{block_number:x}");
        let mut last_error: Option<String> = None;
        for endpoint in endpoints {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_getBlockByNumber",
                "params": [block_hex, true],
                "id": 1
            });
            match self.http_client.post(endpoint).json(&body).send().await {
                Ok(resp) => {
                    let text = resp.text().await.unwrap_or_default();
                    let parsed: serde_json::Value = serde_json::from_str(&text)
                        .map_err(|e| PayError::NetworkError(format!("invalid json: {e}")))?;
                    if let Some(err) = parsed.get("error") {
                        last_error = Some(format!("endpoint={endpoint} rpc error: {err}"));
                        continue;
                    }
                    let result = parsed.get("result");
                    if result.is_none() || result == Some(&serde_json::Value::Null) {
                        return Ok(None);
                    }
                    let block: EvmBlockByNumber =
                        serde_json::from_value(result.cloned().unwrap_or_default())
                            .map_err(|e| PayError::NetworkError(format!("parse block: {e}")))?;
                    return Ok(Some(block));
                }
                Err(e) => {
                    last_error = Some(format!("endpoint={endpoint}: {e}"));
                }
            }
        }
        Err(PayError::NetworkError(format!(
            "eth_getBlockByNumber failed: {}",
            last_error.unwrap_or_default()
        )))
    }

    async fn get_block_timestamp_raw(
        &self,
        endpoints: &[String],
        block_number: u64,
    ) -> Result<Option<u64>, PayError> {
        let block_hex = format!("0x{block_number:x}");
        let mut last_error: Option<String> = None;
        for endpoint in endpoints {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_getBlockByNumber",
                "params": [block_hex, false],
                "id": 1
            });
            match self.http_client.post(endpoint).json(&body).send().await {
                Ok(resp) => {
                    let text = resp.text().await.unwrap_or_default();
                    let parsed: serde_json::Value = serde_json::from_str(&text)
                        .map_err(|e| PayError::NetworkError(format!("invalid json: {e}")))?;
                    if let Some(err) = parsed.get("error") {
                        last_error = Some(format!("endpoint={endpoint} rpc error: {err}"));
                        continue;
                    }
                    let result = parsed.get("result");
                    if result.is_none() || result == Some(&serde_json::Value::Null) {
                        return Ok(None);
                    }
                    let header: EvmBlockHeader = serde_json::from_value(
                        result.cloned().unwrap_or_default(),
                    )
                    .map_err(|e| PayError::NetworkError(format!("parse block header: {e}")))?;
                    return Ok(header.timestamp.as_deref().and_then(parse_hex_u64));
                }
                Err(e) => {
                    last_error = Some(format!("endpoint={endpoint}: {e}"));
                }
            }
        }
        Err(PayError::NetworkError(format!(
            "eth_getBlockByNumber(timestamp) failed: {}",
            last_error.unwrap_or_default()
        )))
    }

    async fn get_erc20_transfer_logs_to_address(
        &self,
        endpoints: &[String],
        token_contract: &str,
        from_block: u64,
        to_block: u64,
        recipient: &str,
    ) -> Result<Vec<EvmLogEntry>, PayError> {
        if from_block > to_block {
            return Ok(vec![]);
        }
        let recipient_topic = address_topic(recipient)
            .ok_or_else(|| PayError::InvalidAmount("invalid evm recipient address".to_string()))?;
        let from_hex = format!("0x{from_block:x}");
        let to_hex = format!("0x{to_block:x}");

        let mut last_error: Option<String> = None;
        for endpoint in endpoints {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_getLogs",
                "params": [{
                    "fromBlock": from_hex,
                    "toBlock": to_hex,
                    "address": token_contract,
                    "topics": [
                        ERC20_TRANSFER_EVENT_TOPIC,
                        serde_json::Value::Null,
                        recipient_topic
                    ]
                }],
                "id": 1
            });
            match self.http_client.post(endpoint).json(&body).send().await {
                Ok(resp) => {
                    let text = resp.text().await.unwrap_or_default();
                    let parsed: serde_json::Value = serde_json::from_str(&text)
                        .map_err(|e| PayError::NetworkError(format!("invalid json: {e}")))?;
                    if let Some(err) = parsed.get("error") {
                        last_error = Some(format!("endpoint={endpoint} rpc error: {err}"));
                        continue;
                    }
                    let result = parsed.get("result").cloned().unwrap_or_default();
                    let logs: Vec<EvmLogEntry> = serde_json::from_value(result)
                        .map_err(|e| PayError::NetworkError(format!("parse logs: {e}")))?;
                    return Ok(logs);
                }
                Err(e) => {
                    last_error = Some(format!("endpoint={endpoint}: {e}"));
                }
            }
        }
        Err(PayError::NetworkError(format!(
            "eth_getLogs failed: {}",
            last_error.unwrap_or_default()
        )))
    }

    async fn sync_receive_records_from_chain(
        &self,
        ctx: ReceiveSyncContext<'_>,
        known_txids: &mut HashSet<String>,
    ) -> Result<HistorySyncStats, PayError> {
        let mut stats = HistorySyncStats::default();
        let scan_limit = ctx.limit.max(1);
        let latest_block = self.get_block_number_raw(ctx.endpoints).await?;
        let lookback_blocks = (scan_limit as u64).saturating_mul(4).clamp(32, 2048);
        let start_block = latest_block.saturating_sub(lookback_blocks.saturating_sub(1));
        let now = wallet::now_epoch_seconds();
        let normalized_wallet = normalize_address(ctx.wallet_address)
            .ok_or_else(|| PayError::InvalidAmount("invalid evm wallet address".to_string()))?;
        let mut memo_cache: HashMap<String, Option<String>> = HashMap::new();
        let mut block_ts_cache: HashMap<u64, u64> = HashMap::new();

        for block_number in (start_block..=latest_block).rev() {
            if stats.records_added >= scan_limit {
                break;
            }
            let Some(block) = self
                .get_block_with_transactions_raw(ctx.endpoints, block_number)
                .await?
            else {
                continue;
            };
            let block_timestamp = block
                .timestamp
                .as_deref()
                .and_then(parse_hex_u64)
                .unwrap_or(now);
            block_ts_cache.insert(block_number, block_timestamp);

            for tx in block.transactions {
                stats.records_scanned = stats.records_scanned.saturating_add(1);
                if stats.records_added >= scan_limit {
                    break;
                }
                let Some(tx_hash) = tx.hash else {
                    continue;
                };
                if known_txids.contains(&tx_hash) {
                    continue;
                }
                let Some(to_addr) = tx.to.as_deref().and_then(normalize_address) else {
                    continue;
                };
                if to_addr != normalized_wallet {
                    continue;
                }
                let Some(value_wei) = tx.value.as_deref().and_then(parse_hex_u256) else {
                    continue;
                };
                if value_wei.is_zero() {
                    continue;
                }
                let amount_gwei: u64 = (value_wei / U256::from(1_000_000_000u64))
                    .try_into()
                    .unwrap_or(u64::MAX);
                if amount_gwei == 0 {
                    continue;
                }
                let memo = tx
                    .input
                    .as_deref()
                    .and_then(|input| decode_hex_data_bytes(input).ok())
                    .and_then(|input| decode_onchain_memo(&input));
                let record = HistoryRecord {
                    transaction_id: tx_hash.clone(),
                    wallet: ctx.wallet_id.to_string(),
                    network: Network::Evm,
                    direction: Direction::Receive,
                    amount: Amount {
                        value: amount_gwei,
                        token: "gwei".to_string(),
                    },
                    status: TxStatus::Confirmed,
                    onchain_memo: memo,
                    local_memo: None,
                    remote_addr: tx.from.as_deref().and_then(normalize_address),
                    preimage: None,
                    created_at_epoch_s: block_timestamp,
                    confirmed_at_epoch_s: Some(block_timestamp),
                    fee: None,
                    reference_keys: None,
                };
                let _ = self.store.append_transaction_record(&record);
                known_txids.insert(tx_hash);
                stats.records_added = stats.records_added.saturating_add(1);
            }
        }

        let mut tracked_tokens: Vec<(String, String)> = tokens::evm_known_tokens(ctx.chain_id)
            .iter()
            .map(|token| (token.symbol.to_string(), token.address.to_ascii_lowercase()))
            .collect();
        for ct in ctx.custom_tokens {
            tracked_tokens.push((
                ct.symbol.to_ascii_lowercase(),
                ct.address.to_ascii_lowercase(),
            ));
        }
        let mut seen_contracts = HashSet::new();
        tracked_tokens.retain(|(_, contract)| seen_contracts.insert(contract.clone()));

        for (symbol, contract) in tracked_tokens {
            if stats.records_added >= scan_limit {
                break;
            }
            let logs = self
                .get_erc20_transfer_logs_to_address(
                    ctx.endpoints,
                    &contract,
                    start_block,
                    latest_block,
                    &normalized_wallet,
                )
                .await?;
            stats.records_scanned = stats.records_scanned.saturating_add(logs.len());
            for log in logs {
                if stats.records_added >= scan_limit {
                    break;
                }
                let Some(tx_hash) = log.transaction_hash else {
                    continue;
                };
                if known_txids.contains(&tx_hash) {
                    continue;
                }
                let Some(data_hex) = log.data.as_deref() else {
                    continue;
                };
                let Some(amount_raw) = parse_hex_u256(data_hex) else {
                    continue;
                };
                if amount_raw.is_zero() {
                    continue;
                }
                let amount_value: u64 = amount_raw.try_into().unwrap_or(u64::MAX);
                let block_number = log
                    .block_number
                    .as_deref()
                    .and_then(parse_hex_u64)
                    .unwrap_or(latest_block);
                let block_timestamp = if let Some(ts) = block_ts_cache.get(&block_number) {
                    *ts
                } else {
                    let ts = self
                        .get_block_timestamp_raw(ctx.endpoints, block_number)
                        .await?
                        .unwrap_or(now);
                    block_ts_cache.insert(block_number, ts);
                    ts
                };
                let memo = if let Some(cached) = memo_cache.get(&tx_hash) {
                    cached.clone()
                } else {
                    let decoded = match self
                        .get_transaction_input_raw(ctx.endpoints, &tx_hash)
                        .await?
                    {
                        Some(input) => decode_onchain_memo(&input),
                        None => None,
                    };
                    memo_cache.insert(tx_hash.clone(), decoded.clone());
                    decoded
                };
                let remote_addr = log.topics.get(1).and_then(|t| topic_to_address(t));
                let record = HistoryRecord {
                    transaction_id: tx_hash.clone(),
                    wallet: ctx.wallet_id.to_string(),
                    network: Network::Evm,
                    direction: Direction::Receive,
                    amount: Amount {
                        value: amount_value,
                        token: symbol.clone(),
                    },
                    status: TxStatus::Confirmed,
                    onchain_memo: memo,
                    local_memo: None,
                    remote_addr,
                    preimage: None,
                    created_at_epoch_s: block_timestamp,
                    confirmed_at_epoch_s: Some(block_timestamp),
                    fee: None,
                    reference_keys: None,
                };
                let _ = self.store.append_transaction_record(&record);
                known_txids.insert(tx_hash);
                stats.records_added = stats.records_added.saturating_add(1);
            }
        }

        Ok(stats)
    }
}

#[derive(Debug, serde::Deserialize)]
struct EvmTxReceipt {
    #[serde(default, rename = "blockNumber")]
    block_number: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default, rename = "gasUsed")]
    gas_used: Option<String>,
    #[serde(default, rename = "effectiveGasPrice")]
    effective_gas_price: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct EvmTxByHash {
    #[serde(default)]
    input: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct EvmBlockByNumber {
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    transactions: Vec<EvmBlockTransaction>,
}

#[derive(Debug, serde::Deserialize)]
struct EvmBlockHeader {
    #[serde(default)]
    timestamp: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct EvmBlockTransaction {
    #[serde(default)]
    hash: Option<String>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    input: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct EvmLogEntry {
    #[serde(default, rename = "transactionHash")]
    transaction_hash: Option<String>,
    #[serde(default, rename = "blockNumber")]
    block_number: Option<String>,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    topics: Vec<String>,
}

struct ReceiveSyncContext<'a> {
    wallet_id: &'a str,
    endpoints: &'a [String],
    chain_id: u64,
    wallet_address: &'a str,
    custom_tokens: &'a [wallet::CustomToken],
    limit: usize,
}

impl EvmTxReceipt {
    /// Calculate fee in gwei from gasUsed * effectiveGasPrice.
    fn fee_gwei(&self) -> Option<u64> {
        let gas_used_hex = self.gas_used.as_deref()?;
        let gas_price_hex = self.effective_gas_price.as_deref()?;
        let gas_used =
            u128::from_str_radix(gas_used_hex.strip_prefix("0x").unwrap_or(gas_used_hex), 16)
                .ok()?;
        let gas_price = u128::from_str_radix(
            gas_price_hex.strip_prefix("0x").unwrap_or(gas_price_hex),
            16,
        )
        .ok()?;
        // fee_wei = gasUsed * effectiveGasPrice; convert to gwei
        let fee_wei = gas_used.checked_mul(gas_price)?;
        Some((fee_wei / 1_000_000_000) as u64)
    }
}

// ERC-20 transfer(address,uint256) function selector
const ERC20_TRANSFER_SELECTOR: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];
const ERC20_TRANSFER_EVENT_TOPIC: &str =
    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

fn encode_erc20_transfer(to: Address, amount: U256) -> Vec<u8> {
    let mut data = Vec::with_capacity(68);
    data.extend_from_slice(&ERC20_TRANSFER_SELECTOR);
    // pad address to 32 bytes
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(to.as_slice());
    // amount as 32 bytes big-endian
    data.extend_from_slice(&amount.to_be_bytes::<32>());
    data
}

fn normalize_onchain_memo(onchain_memo: Option<&str>) -> Result<Option<Vec<u8>>, PayError> {
    let Some(memo) = onchain_memo.map(str::trim).filter(|memo| !memo.is_empty()) else {
        return Ok(None);
    };
    let memo_bytes = memo.as_bytes();
    if memo_bytes.len() > 256 {
        return Err(PayError::InvalidAmount(
            "evm onchain-memo must be <= 256 bytes".to_string(),
        ));
    }
    Ok(Some(memo_bytes.to_vec()))
}

fn append_memo_payload(mut data: Vec<u8>, memo_bytes: Option<&[u8]>) -> Vec<u8> {
    if let Some(memo) = memo_bytes {
        data.extend_from_slice(memo);
    }
    data
}

fn decode_onchain_memo(input_data: &[u8]) -> Option<String> {
    let memo_slice = if input_data.starts_with(&ERC20_TRANSFER_SELECTOR) {
        if input_data.len() <= 68 {
            return None;
        }
        &input_data[68..]
    } else {
        input_data
    };
    if memo_slice.is_empty() {
        return None;
    }
    String::from_utf8(memo_slice.to_vec()).ok()
}

fn decode_hex_data_bytes(raw: &str) -> Result<Vec<u8>, PayError> {
    let trimmed = raw.trim();
    let hex_data = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    if hex_data.is_empty() {
        return Ok(Vec::new());
    }
    if !hex_data.len().is_multiple_of(2) {
        return Err(PayError::NetworkError(
            "invalid tx input hex length".to_string(),
        ));
    }
    hex::decode(hex_data).map_err(|e| PayError::NetworkError(format!("invalid tx input hex: {e}")))
}

fn parse_hex_u64(raw: &str) -> Option<u64> {
    let hex = raw.strip_prefix("0x").unwrap_or(raw);
    u64::from_str_radix(hex, 16).ok()
}

fn parse_hex_u256(raw: &str) -> Option<U256> {
    let hex = raw.strip_prefix("0x").unwrap_or(raw);
    U256::from_str_radix(hex, 16).ok()
}

fn normalize_address(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let body = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))?;
    if body.len() != 40 || !body.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("0x{}", body.to_ascii_lowercase()))
}

fn address_topic(address: &str) -> Option<String> {
    let normalized = normalize_address(address)?;
    let body = normalized.strip_prefix("0x")?;
    Some(format!("0x{:0>64}", body))
}

fn topic_to_address(topic: &str) -> Option<String> {
    let body = topic
        .strip_prefix("0x")
        .or_else(|| topic.strip_prefix("0X"))?;
    if body.len() != 64 || !body.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    normalize_address(&format!("0x{}", &body[24..]))
}

fn receipt_status(receipt: &EvmTxReceipt) -> TxStatus {
    match receipt.status.as_deref() {
        Some("0x1") => TxStatus::Confirmed,
        Some("0x0") => TxStatus::Failed,
        _ => TxStatus::Pending,
    }
}

fn receipt_confirmations(receipt: &EvmTxReceipt, current_block: u64) -> Option<u32> {
    let block_hex = receipt.block_number.as_deref()?;
    let block_num =
        u64::from_str_radix(block_hex.strip_prefix("0x").unwrap_or(block_hex), 16).ok()?;
    if current_block < block_num {
        return Some(0);
    }
    let depth = current_block.saturating_sub(block_num).saturating_add(1);
    Some(depth.min(u32::MAX as u64) as u32)
}

#[async_trait]
impl PayProvider for EvmProvider {
    fn network(&self) -> Network {
        Network::Evm
    }

    fn writes_locally(&self) -> bool {
        true
    }

    async fn create_wallet(&self, request: &WalletCreateRequest) -> Result<WalletInfo, PayError> {
        if request.rpc_endpoints.is_empty() {
            return Err(PayError::InvalidAmount(
                "evm wallet requires --evm-rpc-endpoint (or rpc_endpoints in JSON)".to_string(),
            ));
        }
        let mut endpoints = Vec::new();
        for ep in &request.rpc_endpoints {
            let n = Self::normalize_rpc_endpoint(ep)?;
            if !endpoints.contains(&n) {
                endpoints.push(n);
            }
        }
        let chain_id = request.chain_id.unwrap_or(CHAIN_ID_BASE);

        let mnemonic_str = if let Some(raw) = request.mnemonic_secret.as_deref() {
            let mnemonic: Mnemonic = raw.parse().map_err(|_| {
                PayError::InvalidAmount(
                    "invalid mnemonic-secret for evm wallet: expected BIP39 words".to_string(),
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

        let signer = Self::signer_from_mnemonic(&mnemonic_str)?;
        let address = format!("{:?}", signer.address());

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
            network: Network::Evm,
            label: normalized_label.clone(),
            mint_url: None,
            sol_rpc_endpoints: None,
            evm_rpc_endpoints: Some(endpoints),
            evm_chain_id: Some(chain_id),
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
            network: Network::Evm,
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
        let wallets = self.store.list_wallet_metadata(Some(Network::Evm))?;
        Ok(wallets
            .into_iter()
            .map(|meta| {
                let address = Self::wallet_address(&meta)
                    .unwrap_or_else(|_| INVALID_EVM_WALLET_ADDRESS.to_string());
                evm_wallet_summary(meta, address)
            })
            .collect())
    }

    async fn balance(&self, wallet_id: &str) -> Result<BalanceInfo, PayError> {
        let resolved = self.resolve_wallet_id(wallet_id)?;
        let meta = self.load_evm_wallet(&resolved)?;
        let endpoints = Self::rpc_endpoints_for_wallet(&meta)?;
        let address = Self::wallet_address(&meta)?;
        let chain_id = Self::chain_id_for_wallet(&meta);
        let custom_tokens = meta.custom_tokens.as_deref().unwrap_or_default();
        let balance_wei = self.get_balance_raw(&endpoints, &address).await?;
        // Convert to gwei for the additional field
        let balance_gwei = balance_wei / U256::from(1_000_000_000u64);
        let gwei_u64: u64 = balance_gwei.try_into().unwrap_or(u64::MAX);
        let mut info = BalanceInfo::new(gwei_u64, 0, "gwei");
        self.enrich_with_token_balances(&endpoints, &address, chain_id, custom_tokens, &mut info)
            .await;
        Ok(info)
    }

    async fn balance_all(&self) -> Result<Vec<WalletBalanceItem>, PayError> {
        let wallets = self.store.list_wallet_metadata(Some(Network::Evm))?;
        let mut items = Vec::with_capacity(wallets.len());
        for meta in wallets {
            let chain_id = Self::chain_id_for_wallet(&meta);
            let custom_tokens = meta.custom_tokens.as_deref().unwrap_or_default().to_vec();
            let endpoints = Self::rpc_endpoints_for_wallet(&meta);
            let address = Self::wallet_address(&meta);
            let result = match (endpoints, address) {
                (Ok(endpoints), Ok(address)) => {
                    match self.get_balance_raw(&endpoints, &address).await {
                        Ok(wei) => {
                            let gwei = wei / U256::from(1_000_000_000u64);
                            let gwei_u64: u64 = gwei.try_into().unwrap_or(u64::MAX);
                            let mut info = BalanceInfo::new(gwei_u64, 0, "gwei");
                            self.enrich_with_token_balances(
                                &endpoints,
                                &address,
                                chain_id,
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
                .unwrap_or_else(|_| INVALID_EVM_WALLET_ADDRESS.to_string());
            let summary = evm_wallet_summary(meta, summary_address);
            match result {
                Ok(info) => {
                    items.push(WalletBalanceItem {
                        wallet: summary,
                        balance: Some(info),
                        error: None,
                    });
                }
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
        let meta = self.load_evm_wallet(&resolved)?;
        let _ = Self::rpc_endpoints_for_wallet(&meta)?;
        Ok(ReceiveInfo {
            address: Some(Self::wallet_address(&meta)?),
            invoice: None,
            quote_id: None,
        })
    }

    async fn receive_claim(&self, _wallet: &str, _quote_id: &str) -> Result<u64, PayError> {
        Err(PayError::NotImplemented(
            "evm receive has no claim step".to_string(),
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
            "evm does not use cashu send".to_string(),
        ))
    }

    async fn cashu_receive(
        &self,
        _wallet: &str,
        _token: &str,
    ) -> Result<CashuReceiveResult, PayError> {
        Err(PayError::NotImplemented(
            "evm does not use cashu receive".to_string(),
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
        let meta = self.load_evm_wallet(&resolved_wallet_id)?;
        let endpoints = Self::rpc_endpoints_for_wallet(&meta)?;
        let chain_id = Self::chain_id_for_wallet(&meta);
        let transfer_target = Self::parse_transfer_target(to, chain_id)?;
        let memo_bytes = normalize_onchain_memo(onchain_memo)?;
        let memo_payload = memo_bytes.as_deref();

        let signer = Self::wallet_signer(&meta)?;

        let mut last_error: Option<String> = None;
        let mut transaction_id: Option<String> = None;

        for endpoint in &endpoints {
            let url: reqwest::Url = match endpoint.parse() {
                Ok(u) => u,
                Err(e) => {
                    last_error = Some(format!("endpoint={endpoint} invalid url: {e}"));
                    continue;
                }
            };
            let wallet = EthereumWallet::from(signer.clone());
            let provider = ProviderBuilder::new().wallet(wallet).connect_http(url);

            let tx_result = if let Some(token_contract) = transfer_target.token_contract {
                // ERC-20 transfer
                let call_data = append_memo_payload(
                    encode_erc20_transfer(
                        transfer_target.recipient_address,
                        transfer_target.amount_wei,
                    ),
                    memo_bytes.as_deref(),
                );
                let tx = alloy::rpc::types::TransactionRequest::default()
                    .to(token_contract)
                    .input(call_data.into());
                provider.send_transaction(tx).await
            } else {
                // Native ETH transfer (memo as raw calldata bytes)
                let mut tx = alloy::rpc::types::TransactionRequest::default()
                    .to(transfer_target.recipient_address)
                    .value(transfer_target.amount_wei);
                if let Some(memo) = memo_payload {
                    tx = tx.input(memo.to_vec().into());
                }
                provider.send_transaction(tx).await
            };

            match tx_result {
                Ok(pending) => {
                    let tx_hash = format!("{:?}", pending.tx_hash());
                    transaction_id = Some(tx_hash);
                    break;
                }
                Err(err) => {
                    last_error = Some(format!("endpoint={endpoint} sendTransaction: {err}"));
                }
            }
        }

        let transaction_id = transaction_id.ok_or_else(|| {
            PayError::NetworkError(format!(
                "all evm rpc endpoints failed for withdraw: {}",
                last_error.unwrap_or_default()
            ))
        })?;

        // Determine amount unit based on whether it's a token or native transfer
        let (amount_value, amount_token) = if transfer_target.token_contract.is_some() {
            // For USDC (6 decimals), the raw value is in micro-units
            let val: u64 = transfer_target.amount_wei.try_into().unwrap_or(u64::MAX);
            (val, "token-units".to_string())
        } else {
            let gwei = transfer_target.amount_wei / U256::from(1_000_000_000u64);
            let val: u64 = gwei.try_into().unwrap_or(u64::MAX);
            (val, "gwei".to_string())
        };

        // Try to get fee from receipt (may be pending)
        let fee_amount = match self
            .get_transaction_receipt_raw(&endpoints, &transaction_id)
            .await
        {
            Ok(Some(receipt)) => receipt.fee_gwei().map(|g| Amount {
                value: g,
                token: "gwei".to_string(),
            }),
            _ => {
                // Fallback: estimate fee
                self.estimate_fee_gwei(
                    &endpoints,
                    &format!("{:?}", signer.address()),
                    &format!("{:?}", transfer_target.recipient_address),
                    None,
                )
                .await
                .ok()
                .map(|g| Amount {
                    value: g,
                    token: "gwei".to_string(),
                })
            }
        };

        let history = HistoryRecord {
            transaction_id: transaction_id.clone(),
            wallet: resolved_wallet_id.clone(),
            network: Network::Evm,
            direction: Direction::Send,
            amount: Amount {
                value: amount_value,
                token: amount_token.clone(),
            },
            status: TxStatus::Pending,
            onchain_memo: onchain_memo.map(|s| s.to_string()),
            local_memo: None,
            remote_addr: Some(format!("{:?}", transfer_target.recipient_address)),
            preimage: None,
            created_at_epoch_s: wallet::now_epoch_seconds(),
            confirmed_at_epoch_s: None,
            fee: fee_amount.clone(),
            reference_keys: None,
        };
        let _ = self.store.append_transaction_record(&history);

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
        let meta = self.load_evm_wallet(&resolved)?;
        let endpoints = Self::rpc_endpoints_for_wallet(&meta)?;
        let chain_id = Self::chain_id_for_wallet(&meta);
        let transfer_target = Self::parse_transfer_target(to, chain_id)?;
        let signer = Self::wallet_signer(&meta)?;

        let (to_addr, data) = if let Some(token_contract) = transfer_target.token_contract {
            let call_data = encode_erc20_transfer(
                transfer_target.recipient_address,
                transfer_target.amount_wei,
            );
            (
                format!("{:?}", token_contract),
                Some(format!("0x{}", hex::encode(&call_data))),
            )
        } else {
            (format!("{:?}", transfer_target.recipient_address), None)
        };

        let fee_gwei = self
            .estimate_fee_gwei(
                &endpoints,
                &format!("{:?}", signer.address()),
                &to_addr,
                data.as_deref(),
            )
            .await
            .unwrap_or(0);

        // amount_native in the same unit as the transfer
        let amount_native = if transfer_target.token_contract.is_some() {
            let val: u64 = transfer_target.amount_wei.try_into().unwrap_or(u64::MAX);
            val
        } else {
            let gwei = transfer_target.amount_wei / U256::from(1_000_000_000u64);
            gwei.try_into().unwrap_or(u64::MAX)
        };

        Ok(SendQuoteInfo {
            wallet: resolved,
            amount_native,
            fee_estimate_native: fee_gwei,
            fee_unit: "gwei".to_string(),
        })
    }

    async fn history_list(
        &self,
        wallet: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<HistoryRecord>, PayError> {
        let resolved = self.resolve_wallet_id(wallet)?;
        let _ = self.load_evm_wallet(&resolved)?;
        let all = self.store.load_wallet_transaction_records(&resolved)?;
        let total = all.len();
        let start = offset.min(total);
        let end = (start + limit).min(total);
        // Return newest first
        let mut slice = all[start..end].to_vec();
        slice.reverse();
        Ok(slice)
    }

    async fn history_status(&self, transaction_id: &str) -> Result<HistoryStatusInfo, PayError> {
        let mut record = self.store.find_transaction_record_by_id(transaction_id)?;
        let Some(existing) = record.as_ref() else {
            return Err(PayError::WalletNotFound(format!(
                "transaction {transaction_id} not found"
            )));
        };
        if existing.network != Network::Evm {
            return Err(PayError::WalletNotFound(format!(
                "transaction {transaction_id} not found"
            )));
        }

        let mut confirmations: Option<u32> = None;
        if let Ok(meta) = self.load_evm_wallet(&existing.wallet) {
            if let Ok(endpoints) = Self::rpc_endpoints_for_wallet(&meta) {
                if let Ok(Some(receipt)) = self
                    .get_transaction_receipt_raw(&endpoints, transaction_id)
                    .await
                {
                    let status = receipt_status(&receipt);
                    let current_block = if receipt.block_number.is_some() {
                        self.get_block_number_raw(&endpoints).await.unwrap_or(0)
                    } else {
                        0
                    };
                    confirmations = receipt_confirmations(&receipt, current_block);

                    if let Some(rec) = record.as_mut() {
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
                                transaction_id,
                                status,
                                confirmed_at_epoch_s,
                            );
                            rec.status = status;
                            rec.confirmed_at_epoch_s = confirmed_at_epoch_s;
                        }

                        if let Some(fee_gwei) = receipt.fee_gwei() {
                            let update_fee = rec
                                .fee
                                .as_ref()
                                .map(|f| f.token != "gwei" || f.value != fee_gwei)
                                .unwrap_or(true);
                            if update_fee {
                                let _ = self.store.update_transaction_record_fee(
                                    transaction_id,
                                    fee_gwei,
                                    "gwei",
                                );
                                rec.fee = Some(Amount {
                                    value: fee_gwei,
                                    token: "gwei".to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }

        let record = record.ok_or_else(|| {
            PayError::WalletNotFound(format!("transaction {transaction_id} not found"))
        })?;
        Ok(HistoryStatusInfo {
            transaction_id: transaction_id.to_string(),
            status: record.status,
            confirmations,
            preimage: record.preimage.clone(),
            item: Some(record),
        })
    }

    async fn history_onchain_memo(
        &self,
        wallet: &str,
        transaction_id: &str,
    ) -> Result<Option<String>, PayError> {
        let resolved = self.resolve_wallet_id(wallet)?;
        let meta = self.load_evm_wallet(&resolved)?;
        let endpoints = Self::rpc_endpoints_for_wallet(&meta)?;
        let Some(input_data) = self
            .get_transaction_input_raw(&endpoints, transaction_id)
            .await?
        else {
            return Ok(None);
        };
        Ok(decode_onchain_memo(&input_data))
    }

    async fn history_sync(&self, wallet: &str, limit: usize) -> Result<HistorySyncStats, PayError> {
        let resolved = self.resolve_wallet_id(wallet)?;
        let meta = self.load_evm_wallet(&resolved)?;
        let endpoints = Self::rpc_endpoints_for_wallet(&meta)?;
        let chain_id = Self::chain_id_for_wallet(&meta);
        let wallet_address = Self::wallet_address(&meta)?;
        let local_records = self.store.load_wallet_transaction_records(&resolved)?;
        let mut known_txids: HashSet<String> = local_records
            .iter()
            .filter(|record| record.network == Network::Evm)
            .map(|record| record.transaction_id.clone())
            .collect();
        let pending_ids: Vec<String> = local_records
            .iter()
            .filter(|record| record.network == Network::Evm && record.status == TxStatus::Pending)
            .map(|record| record.transaction_id.clone())
            .take(limit)
            .collect();

        let mut stats = HistorySyncStats {
            records_scanned: pending_ids.len(),
            records_added: 0,
            records_updated: 0,
        };

        for txid in pending_ids {
            let before = self.store.find_transaction_record_by_id(&txid)?;
            let status_info = self.history_status(&txid).await?;
            let after = status_info.item;
            if let (Some(before), Some(after)) = (before, after) {
                let fee_changed = match (before.fee.as_ref(), after.fee.as_ref()) {
                    (Some(lhs), Some(rhs)) => lhs.value != rhs.value || lhs.token != rhs.token,
                    (None, None) => false,
                    _ => true,
                };
                if before.status != after.status
                    || before.confirmed_at_epoch_s != after.confirmed_at_epoch_s
                    || fee_changed
                {
                    stats.records_updated = stats.records_updated.saturating_add(1);
                }
            }
        }

        let incoming = self
            .sync_receive_records_from_chain(
                ReceiveSyncContext {
                    wallet_id: &resolved,
                    endpoints: &endpoints,
                    chain_id,
                    wallet_address: &wallet_address,
                    custom_tokens: meta.custom_tokens.as_deref().unwrap_or_default(),
                    limit,
                },
                &mut known_txids,
            )
            .await?;
        stats.records_scanned = stats
            .records_scanned
            .saturating_add(incoming.records_scanned);
        stats.records_added = stats.records_added.saturating_add(incoming.records_added);
        stats.records_updated = stats
            .records_updated
            .saturating_add(incoming.records_updated);

        Ok(stats)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parse_native_eth_transfer() {
        let target = EvmProvider::parse_transfer_target(
            "ethereum:0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045?amount-wei=1000000000000000",
            CHAIN_ID_BASE,
        )
        .expect("parse native eth transfer");
        assert_eq!(
            target.recipient_address,
            "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"
                .parse::<Address>()
                .expect("parse address")
        );
        assert_eq!(target.amount_wei, U256::from(1_000_000_000_000_000u64));
        assert!(target.token_contract.is_none());
    }

    #[test]
    fn parse_gwei_amount() {
        let target = EvmProvider::parse_transfer_target(
            "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045?amount-gwei=100000",
            CHAIN_ID_BASE,
        )
        .expect("parse gwei");
        assert_eq!(
            target.amount_wei,
            U256::from(100_000u64) * U256::from(1_000_000_000u64)
        );
    }

    #[test]
    fn parse_usdc_transfer() {
        let target = EvmProvider::parse_transfer_target(
            "ethereum:0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045?amount-wei=1000000&token=usdc",
            CHAIN_ID_BASE,
        )
        .expect("parse usdc transfer");
        assert!(target.token_contract.is_some());
        assert_eq!(target.amount_wei, U256::from(1_000_000u64));
    }

    #[test]
    fn parse_empty_target_fails() {
        assert!(EvmProvider::parse_transfer_target("", CHAIN_ID_BASE).is_err());
    }

    #[test]
    fn parse_missing_amount_fails() {
        assert!(EvmProvider::parse_transfer_target(
            "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045",
            CHAIN_ID_BASE,
        )
        .is_err());
    }

    #[test]
    fn erc20_transfer_encoding_length() {
        let to: Address = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"
            .parse()
            .expect("parse addr");
        let data = encode_erc20_transfer(to, U256::from(1_000_000u64));
        assert_eq!(data.len(), 68); // 4 selector + 32 address + 32 amount
        assert_eq!(&data[..4], &ERC20_TRANSFER_SELECTOR);
    }

    #[test]
    fn normalize_onchain_memo_trims_and_enforces_limit() {
        let memo = normalize_onchain_memo(Some("  hello  ")).expect("memo should normalize");
        assert_eq!(memo, Some(b"hello".to_vec()));

        let long_memo = "x".repeat(257);
        assert!(normalize_onchain_memo(Some(&long_memo)).is_err());
    }

    #[test]
    fn append_memo_payload_appends_bytes() {
        let encoded = encode_erc20_transfer(
            "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"
                .parse()
                .expect("address"),
            U256::from(42u64),
        );
        let with_memo = append_memo_payload(encoded.clone(), Some(b"memo"));
        assert_eq!(with_memo.len(), encoded.len() + 4);
        assert!(with_memo.ends_with(b"memo"));
    }

    #[test]
    fn decode_onchain_memo_supports_native_and_erc20_inputs() {
        let native = b"order:abc";
        assert_eq!(decode_onchain_memo(native), Some("order:abc".to_string()));

        let erc20 = append_memo_payload(
            encode_erc20_transfer(
                "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"
                    .parse()
                    .expect("address"),
                U256::from(42u64),
            ),
            Some(b"order:def"),
        );
        assert_eq!(decode_onchain_memo(&erc20), Some("order:def".to_string()));

        let legacy = append_memo_payload(
            encode_erc20_transfer(
                "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"
                    .parse()
                    .expect("address"),
                U256::from(42u64),
            ),
            None,
        );
        assert_eq!(decode_onchain_memo(&legacy), None);
    }

    #[test]
    fn receipt_confirmations_includes_inclusion_block() {
        let receipt = EvmTxReceipt {
            block_number: Some("0x10".to_string()),
            status: Some("0x1".to_string()),
            gas_used: None,
            effective_gas_price: None,
        };
        assert_eq!(receipt_confirmations(&receipt, 0x10), Some(1));
        assert_eq!(receipt_confirmations(&receipt, 0x12), Some(3));
    }

    #[test]
    fn usdc_address_base() {
        let addr = usdc_contract_address(CHAIN_ID_BASE);
        assert!(addr.is_some());
    }

    #[test]
    fn usdc_address_unknown_chain() {
        let addr = usdc_contract_address(999999);
        assert!(addr.is_none());
    }

    #[test]
    fn erc20_balance_of_calldata_encoding() {
        // balanceOf(address) selector: 0x70a08231
        let addr = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";
        let addr_no_prefix = addr.strip_prefix("0x").unwrap();
        let calldata = format!("0x70a08231000000000000000000000000{addr_no_prefix}");
        // Selector (10 chars) + 64 hex chars for padded address = 74 chars + 0x prefix
        assert_eq!(calldata.len(), 2 + 8 + 64);
        assert!(calldata.starts_with("0x70a08231"));
    }

    #[test]
    fn parse_usdt_transfer_via_registry() {
        let target = EvmProvider::parse_transfer_target(
            "ethereum:0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045?amount-wei=500000&token=usdt",
            CHAIN_ID_BASE,
        )
        .expect("parse usdt transfer");
        assert!(target.token_contract.is_some());
        assert_eq!(target.amount_wei, U256::from(500_000u64));
    }

    #[test]
    fn parse_unknown_token_symbol_fails() {
        let err = EvmProvider::parse_transfer_target(
            "ethereum:0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045?amount-wei=100&token=doge",
            CHAIN_ID_BASE,
        );
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("unknown token"));
    }

    #[test]
    fn parse_custom_contract_address_token() {
        let target = EvmProvider::parse_transfer_target(
            "ethereum:0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045?amount-wei=100&token=0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            CHAIN_ID_BASE,
        )
        .expect("parse custom token");
        assert!(target.token_contract.is_some());
    }

    #[test]
    fn normalize_rpc_endpoints_adds_https() {
        let result = EvmProvider::normalize_rpc_endpoint("base-mainnet.g.alchemy.com/v2/key");
        assert!(result.is_ok());
        assert!(result.as_ref().is_ok_and(|s| s.starts_with("https://")));
    }

    #[test]
    fn normalize_rpc_endpoints_empty_fails() {
        assert!(EvmProvider::normalize_rpc_endpoint("").is_err());
    }

    #[test]
    fn chain_id_defaults_to_base() {
        let meta = WalletMetadata {
            id: "w_test".to_string(),
            network: Network::Evm,
            label: None,
            mint_url: None,
            sol_rpc_endpoints: None,
            evm_rpc_endpoints: Some(vec!["https://rpc.example".to_string()]),
            evm_chain_id: None,
            seed_secret: None,
            backend: None,
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
        assert_eq!(EvmProvider::chain_id_for_wallet(&meta), CHAIN_ID_BASE);
    }
}
