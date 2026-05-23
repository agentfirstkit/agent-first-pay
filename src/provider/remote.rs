use crate::mode::rpc::crypto::Cipher;
use crate::mode::rpc::proto::af_pay_client::AfPayClient;
use crate::mode::rpc::proto::EncryptedRequest;
use agent_first_data::OutputFormat;
use std::io::Write;

/// Send an Input to a remote RPC server, return the decrypted Output array.
pub async fn rpc_call(
    endpoint: &str,
    secret: &str,
    input: &impl serde::Serialize,
) -> Vec<serde_json::Value> {
    let cipher = Cipher::from_secret(secret);

    // Serialize input to JSON
    let input_json = match serde_json::to_vec(input) {
        Ok(v) => v,
        Err(e) => return vec![rpc_error_output("serialize_error", &format!("{e}"))],
    };

    // Encrypt
    let (nonce, ciphertext) = match cipher.encrypt(&input_json) {
        Ok(v) => v,
        Err(e) => return vec![rpc_error_output("encrypt_error", &e)],
    };

    // Build endpoint URL (tonic needs http:// prefix)
    let url = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        endpoint.to_string()
    } else {
        format!("http://{endpoint}")
    };

    // Connect and call
    let mut client = match AfPayClient::connect(url).await {
        Ok(c) => c,
        Err(e) => return vec![rpc_error_output("connect_error", &format!("{e}"))],
    };

    let response = match client.call(EncryptedRequest { nonce, ciphertext }).await {
        Ok(r) => r,
        Err(status) => {
            let error_code = match status.code() {
                tonic::Code::PermissionDenied => "permission_denied",
                tonic::Code::Unauthenticated => "unauthenticated",
                tonic::Code::Unavailable => "unavailable",
                tonic::Code::InvalidArgument => "invalid_argument",
                _ => "rpc_error",
            };
            return vec![rpc_error_output(error_code, status.message())];
        }
    };

    let resp = response.into_inner();

    // Decrypt response
    let plaintext = match cipher.decrypt(&resp.nonce, &resp.ciphertext) {
        Ok(v) => v,
        Err(e) => return vec![rpc_error_output("decrypt_error", &e)],
    };

    // Parse as JSON array of Outputs
    match serde_json::from_slice(&plaintext) {
        Ok(v) => v,
        Err(e) => vec![rpc_error_output("parse_error", &format!("{e}"))],
    }
}

fn rpc_error_output(error_code: &str, error: &str) -> serde_json::Value {
    let hint = match error_code {
        "connect_error" => Some("check --rpc-endpoint address and that the daemon is running"),
        "unauthenticated" | "decrypt_error" => Some("check --rpc-secret matches the daemon"),
        "permission_denied" => Some("this operation can only be run on the daemon directly"),
        _ => None,
    };
    let mut v = serde_json::json!({
        "code": "error",
        "error_code": error_code,
        "error": error,
        "retryable": matches!(error_code, "connect_error" | "unavailable"),
    });
    if let Some(h) = hint {
        v["hint"] = serde_json::Value::String(h.to_string());
    }
    v
}

/// Validate rpc_endpoint + rpc_secret pair. Returns (endpoint, secret) or prints error and exits.
pub fn require_remote_args<'a>(
    endpoint: Option<&'a str>,
    secret: Option<&'a str>,
    format: OutputFormat,
) -> (&'a str, &'a str) {
    let ep = match endpoint {
        Some(ep) if !ep.is_empty() => ep,
        _ => {
            let value = agent_first_data::build_cli_error(
                "--rpc-endpoint is required",
                Some("pass the address of the afpay daemon"),
            );
            let rendered = agent_first_data::cli_output(&value, format);
            let _ = writeln!(std::io::stdout(), "{rendered}");
            std::process::exit(1);
        }
    };
    let sec = match secret {
        Some(s) if !s.is_empty() => s,
        _ => {
            let value = agent_first_data::build_cli_error(
                "--rpc-secret is required with --rpc-endpoint",
                Some("must match the --rpc-secret used by the daemon"),
            );
            let rendered = agent_first_data::cli_output(&value, format);
            let _ = writeln!(std::io::stdout(), "{rendered}");
            std::process::exit(1);
        }
    };
    (ep, sec)
}

/// Render remote RPC outputs, filtering log events. Returns true if any output was an error.
pub fn emit_remote_outputs(
    outputs: &[serde_json::Value],
    format: OutputFormat,
    log_filters: &[String],
) -> bool {
    let mut had_error = false;
    for value in outputs {
        if value.get("code").and_then(|v| v.as_str()) == Some("error") {
            had_error = true;
        }
        if let Some("log") = value.get("code").and_then(|v| v.as_str()) {
            if let Some(event) = value.get("event").and_then(|v| v.as_str()) {
                if !log_event_enabled(log_filters, event) {
                    continue;
                }
            }
        }
        let rendered = crate::output_fmt::render_value_with_policy(value, format);
        let _ = writeln!(std::io::stdout(), "{rendered}");
    }
    had_error
}

/// When a client connects via --rpc-endpoint, wrap the daemon's LimitStatus response
/// so the connected daemon appears as a node in the topology.
/// Also stamps `origin` on limit_exceeded errors that lack one.
pub fn wrap_remote_limit_topology(outputs: &mut [serde_json::Value], endpoint: &str) {
    for value in outputs.iter_mut() {
        let code = value.get("code").and_then(|v| v.as_str()).unwrap_or("");
        match code {
            "limit_status" => {
                // Extract daemon's limits + downstream, wrap as a downstream node
                let limits = value
                    .get("limits")
                    .cloned()
                    .unwrap_or(serde_json::Value::Array(vec![]));
                let downstream = value
                    .get("downstream")
                    .cloned()
                    .unwrap_or(serde_json::Value::Array(vec![]));
                let node = serde_json::json!({
                    "name": endpoint,
                    "endpoint": endpoint,
                    "limits": limits,
                    "downstream": downstream,
                });
                value["limits"] = serde_json::Value::Array(vec![]);
                value["downstream"] = serde_json::Value::Array(vec![node]);
            }
            "limit_exceeded"
                if value.get("origin").is_none()
                    || value.get("origin") == Some(&serde_json::Value::Null) =>
            {
                // If no origin, stamp the endpoint so the client knows which node rejected
                value["origin"] = serde_json::Value::String(endpoint.to_string());
            }
            _ => {}
        }
    }
}

fn log_event_enabled(log: &[String], event: &str) -> bool {
    if log.is_empty() {
        return false;
    }
    let ev = event.to_ascii_lowercase();
    log.iter()
        .any(|f| f == "*" || f == "all" || ev.starts_with(f.as_str()))
}

// ═══════════════════════════════════════════
// RemoteProvider — PayProvider over RPC
// ═══════════════════════════════════════════

use crate::provider::{HistorySyncStats, PayError, PayProvider};
use crate::types::*;
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::sync::atomic::{AtomicU64, Ordering};

static REMOTE_REQUEST_FALLBACK_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Deserialize)]
struct WalletCreatedOut {
    wallet: String,
    address: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    mnemonic: Option<String>,
}

#[derive(Deserialize)]
struct WalletListOut {
    #[serde(default)]
    wallets: Vec<WalletSummary>,
}

#[derive(Deserialize)]
struct WalletBalancesOut {
    #[serde(default)]
    wallets: Vec<WalletBalanceItem>,
}

#[derive(Deserialize)]
struct LegacyWalletBalanceOut {
    #[serde(default)]
    balance: Option<BalanceInfo>,
}

#[derive(Deserialize)]
struct ReceiveInfoOut {
    receive_info: ReceiveInfo,
}

#[derive(Deserialize)]
struct ReceiveClaimedOut {
    amount: Amount,
}

#[derive(Deserialize)]
struct CashuSentOut {
    wallet: String,
    transaction_id: String,
    status: TxStatus,
    #[serde(default)]
    fee: Option<Amount>,
    token: String,
}

#[derive(Deserialize)]
struct CashuReceivedOut {
    wallet: String,
    amount: Amount,
    #[serde(default)]
    memo: Option<String>,
}

#[derive(Deserialize)]
struct SentOut {
    wallet: String,
    transaction_id: String,
    amount: Amount,
    #[serde(default)]
    fee: Option<Amount>,
    #[serde(default)]
    preimage: Option<String>,
}

#[derive(Deserialize)]
struct RestoredOut {
    wallet: String,
    unspent: u64,
    spent: u64,
    pending: u64,
    unit: String,
}

#[derive(Deserialize)]
struct HistoryOut {
    #[serde(default)]
    items: Vec<HistoryRecord>,
}

#[derive(Deserialize)]
struct HistoryStatusOut {
    transaction_id: String,
    status: TxStatus,
    #[serde(default)]
    confirmations: Option<u32>,
    #[serde(default)]
    preimage: Option<String>,
    #[serde(default)]
    item: Option<HistoryRecord>,
}

#[derive(Deserialize)]
struct HistoryUpdatedOut {
    #[serde(default)]
    records_scanned: usize,
    #[serde(default)]
    records_added: usize,
    #[serde(default)]
    records_updated: usize,
}

pub struct RemoteProvider {
    endpoint: String,
    secret: String,
    network: Network,
}

impl RemoteProvider {
    pub fn new(endpoint: &str, secret: &str, network: Network) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            secret: secret.to_string(),
            network,
        }
    }

    async fn call(&self, input: &Input) -> Vec<serde_json::Value> {
        rpc_call(&self.endpoint, &self.secret, input).await
    }

    fn map_remote_error(&self, value: &serde_json::Value) -> Option<PayError> {
        let code = value
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        match code {
            "error" => {
                let msg = value
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                let error_code = value
                    .get("error_code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("remote_error");
                Some(match error_code {
                    "wallet_not_found" => PayError::WalletNotFound(msg.to_string()),
                    "invalid_amount" => PayError::InvalidAmount(msg.to_string()),
                    "not_implemented" => PayError::NotImplemented(msg.to_string()),
                    "limit_exceeded" => PayError::LimitExceeded {
                        rule_id: value
                            .get("rule_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        scope: serde_json::from_value(
                            value
                                .get("scope")
                                .cloned()
                                .unwrap_or_else(|| serde_json::json!("network")),
                        )
                        .unwrap_or(SpendScope::Network),
                        scope_key: value
                            .get("scope_key")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        spent: value.get("spent").and_then(|v| v.as_u64()).unwrap_or(0),
                        max_spend: value.get("max_spend").and_then(|v| v.as_u64()).unwrap_or(0),
                        token: value
                            .get("token")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        remaining_s: value
                            .get("remaining_s")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0),
                        origin: Some(
                            value
                                .get("origin")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| self.endpoint.clone()),
                        ),
                    },
                    _ => PayError::NetworkError(msg.to_string()),
                })
            }
            "limit_exceeded" => Some(PayError::LimitExceeded {
                rule_id: value
                    .get("rule_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                scope: serde_json::from_value(
                    value
                        .get("scope")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!("network")),
                )
                .unwrap_or(SpendScope::Network),
                scope_key: value
                    .get("scope_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                spent: value.get("spent").and_then(|v| v.as_u64()).unwrap_or(0),
                max_spend: value.get("max_spend").and_then(|v| v.as_u64()).unwrap_or(0),
                token: value
                    .get("token")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                remaining_s: value
                    .get("remaining_s")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                origin: Some(
                    value
                        .get("origin")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| self.endpoint.clone()),
                ),
            }),
            _ => None,
        }
    }

    /// Extract the first non-log expected output.
    fn first_output(
        &self,
        outputs: Vec<serde_json::Value>,
        expected_codes: &[&str],
    ) -> Result<serde_json::Value, PayError> {
        for value in outputs {
            let code = value
                .get("code")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if code == "log" {
                continue;
            }
            if let Some(err) = self.map_remote_error(&value) {
                return Err(err);
            }
            if expected_codes.contains(&code) {
                return Ok(value);
            }
            return Err(PayError::NetworkError(format!(
                "unexpected remote output code '{code}'"
            )));
        }
        Err(PayError::NetworkError(
            "empty or log-only response from remote".to_string(),
        ))
    }

    fn parse_output<T: DeserializeOwned>(
        &self,
        value: serde_json::Value,
        label: &str,
    ) -> Result<T, PayError> {
        serde_json::from_value(value)
            .map_err(|e| PayError::NetworkError(format!("parse {label}: {e}")))
    }

    fn balance_from_output(
        &self,
        value: serde_json::Value,
        wallet: &str,
    ) -> Result<BalanceInfo, PayError> {
        if value.get("code").and_then(|v| v.as_str()) == Some("wallet_balance") {
            let parsed: LegacyWalletBalanceOut = self.parse_output(value, "wallet_balance")?;
            return Ok(parsed
                .balance
                .unwrap_or_else(|| BalanceInfo::new(0, 0, "unknown")));
        }

        let parsed: WalletBalancesOut = self.parse_output(value, "wallet_balances")?;
        let mut wallets = parsed.wallets;
        let item = wallets
            .iter()
            .position(|item| item.wallet.id == wallet)
            .map(|idx| wallets.remove(idx))
            .or_else(|| {
                // Current daemon returns a single-item wallet_balances response for
                // single-wallet balance queries. Use it even if older daemons omit id.
                (wallets.len() == 1).then(|| wallets.remove(0))
            });
        let Some(item) = item else {
            return Err(PayError::WalletNotFound(format!(
                "wallet {wallet} not found in remote balance response"
            )));
        };
        item.balance.ok_or_else(|| {
            PayError::NetworkError(
                item.error
                    .unwrap_or_else(|| "remote balance response has no balance".to_string()),
            )
        })
    }

    fn gen_id(&self) -> String {
        crate::store::wallet::generate_request_identifier().unwrap_or_else(|_| {
            let seq = REMOTE_REQUEST_FALLBACK_COUNTER.fetch_add(1, Ordering::Relaxed);
            format!(
                "req_fallback_{}_{}",
                crate::store::wallet::now_epoch_seconds(),
                seq
            )
        })
    }
}

#[async_trait]
impl PayProvider for RemoteProvider {
    fn network(&self) -> Network {
        self.network
    }

    async fn ping(&self) -> Result<(), PayError> {
        let outputs = self.call(&Input::Version).await;
        for value in &outputs {
            if let Some(err) = self.map_remote_error(value) {
                return Err(err);
            }
            if value.get("code").and_then(|v| v.as_str()) == Some("version") {
                let remote_version = value
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let local = crate::config::VERSION;
                if remote_version != local {
                    return Err(PayError::NetworkError(format!(
                        "version mismatch: local v{local}, remote v{remote_version}"
                    )));
                }
            }
        }
        Ok(())
    }

    async fn create_wallet(&self, request: &WalletCreateRequest) -> Result<WalletInfo, PayError> {
        let out = self.first_output(
            self.call(&Input::WalletCreate {
                id: self.gen_id(),
                network: self.network,
                label: Some(request.label.clone()),
                mint_url: request.mint_url.clone(),
                rpc_endpoints: request.rpc_endpoints.clone(),
                chain_id: request.chain_id,
                mnemonic_secret: request.mnemonic_secret.clone(),
                btc_esplora_url: request.btc_esplora_url.clone(),
                btc_network: request.btc_network.clone(),
                btc_address_type: request.btc_address_type.clone(),
                btc_backend: request.btc_backend,
                btc_core_url: request.btc_core_url.clone(),
                btc_core_auth_secret: request.btc_core_auth_secret.clone(),
                btc_electrum_url: request.btc_electrum_url.clone(),
            })
            .await,
            &["wallet_created"],
        )?;
        let parsed: WalletCreatedOut = self.parse_output(out, "wallet_created")?;
        Ok(WalletInfo {
            id: parsed.wallet,
            network: self.network,
            address: parsed.address,
            label: parsed.label,
            mnemonic: parsed.mnemonic,
        })
    }

    async fn create_ln_wallet(
        &self,
        request: LnWalletCreateRequest,
    ) -> Result<WalletInfo, PayError> {
        if self.network != Network::Ln {
            return Err(PayError::InvalidAmount(
                "ln_wallet_create can only be used with ln provider".to_string(),
            ));
        }
        let out = self.first_output(
            self.call(&Input::LnWalletCreate {
                id: self.gen_id(),
                request,
            })
            .await,
            &["wallet_created"],
        )?;
        let parsed: WalletCreatedOut = self.parse_output(out, "wallet_created")?;
        Ok(WalletInfo {
            id: parsed.wallet,
            network: self.network,
            address: parsed.address,
            label: parsed.label,
            mnemonic: parsed.mnemonic,
        })
    }

    async fn close_wallet(&self, wallet: &str) -> Result<(), PayError> {
        self.first_output(
            self.call(&Input::WalletClose {
                id: self.gen_id(),
                wallet: wallet.to_string(),
                dangerously_skip_balance_check_and_may_lose_money: false,
            })
            .await,
            &["wallet_closed"],
        )?;
        Ok(())
    }

    async fn list_wallets(&self) -> Result<Vec<WalletSummary>, PayError> {
        let out = self.first_output(
            self.call(&Input::WalletList {
                id: self.gen_id(),
                network: Some(self.network),
            })
            .await,
            &["wallet_list"],
        )?;
        let parsed: WalletListOut = self.parse_output(out, "wallet_list")?;
        Ok(parsed.wallets)
    }

    async fn balance(&self, wallet: &str) -> Result<BalanceInfo, PayError> {
        let out = self.first_output(
            self.call(&Input::Balance {
                id: self.gen_id(),
                wallet: Some(wallet.to_string()),
                network: None,
                check: false,
            })
            .await,
            &["wallet_balances", "wallet_balance"],
        )?;
        self.balance_from_output(out, wallet)
    }

    async fn check_balance(&self, wallet: &str) -> Result<BalanceInfo, PayError> {
        let out = self.first_output(
            self.call(&Input::Balance {
                id: self.gen_id(),
                wallet: Some(wallet.to_string()),
                network: None,
                check: true,
            })
            .await,
            &["wallet_balances", "wallet_balance"],
        )?;
        self.balance_from_output(out, wallet)
    }

    async fn balance_all(&self) -> Result<Vec<WalletBalanceItem>, PayError> {
        let out = self.first_output(
            self.call(&Input::Balance {
                id: self.gen_id(),
                wallet: None,
                network: None,
                check: false,
            })
            .await,
            &["wallet_balances", "wallet_balance"],
        )?;
        // Could be wallet_balance (legacy single) or wallet_balances (current).
        if out.get("code").and_then(|v| v.as_str()) == Some("wallet_balance") {
            let legacy: LegacyWalletBalanceOut = self.parse_output(out, "wallet_balance")?;
            let Some(balance) = legacy.balance else {
                return Ok(vec![]);
            };
            return Ok(vec![WalletBalanceItem {
                wallet: WalletSummary {
                    id: String::new(),
                    network: self.network,
                    label: None,
                    address: String::new(),
                    backend: None,
                    mint_url: None,
                    rpc_endpoints: None,
                    chain_id: None,
                    created_at_epoch_s: 0,
                },
                balance: Some(balance),
                error: None,
            }]);
        }
        let parsed: WalletBalancesOut = self.parse_output(out, "wallet_balances")?;
        Ok(parsed.wallets)
    }

    async fn receive_info(
        &self,
        wallet: &str,
        amount: Option<Amount>,
    ) -> Result<ReceiveInfo, PayError> {
        let out = self.first_output(
            self.call(&Input::Receive {
                id: self.gen_id(),
                wallet: wallet.to_string(),
                network: Some(self.network),
                amount,
                onchain_memo: None,
                wait_until_paid: false,
                wait_timeout_s: None,
                wait_poll_interval_ms: None,
                wait_sync_limit: None,
                write_qr_svg_file: false,
                min_confirmations: None,
                reference: None,
            })
            .await,
            &["receive_info"],
        )?;
        let parsed: ReceiveInfoOut = self.parse_output(out, "receive_info")?;
        Ok(parsed.receive_info)
    }

    async fn receive_claim(&self, wallet: &str, quote_id: &str) -> Result<u64, PayError> {
        let out = self.first_output(
            self.call(&Input::ReceiveClaim {
                id: self.gen_id(),
                wallet: wallet.to_string(),
                quote_id: quote_id.to_string(),
            })
            .await,
            &["receive_claimed"],
        )?;
        let parsed: ReceiveClaimedOut = self.parse_output(out, "receive_claimed")?;
        Ok(parsed.amount.value)
    }

    async fn cashu_send(
        &self,
        wallet: &str,
        amount: Amount,
        onchain_memo: Option<&str>,
        mints: Option<&[String]>,
    ) -> Result<CashuSendResult, PayError> {
        let out = self.first_output(
            self.call(&Input::CashuSend {
                id: self.gen_id(),
                wallet: Some(wallet.to_string()),
                amount: amount.clone(),
                onchain_memo: onchain_memo.map(|s| s.to_string()),
                local_memo: None,
                mints: mints.map(|m| m.to_vec()),
            })
            .await,
            &["cashu_sent"],
        )?;
        let parsed: CashuSentOut = self.parse_output(out, "cashu_sent")?;
        Ok(CashuSendResult {
            wallet: parsed.wallet,
            transaction_id: parsed.transaction_id,
            status: parsed.status,
            fee: parsed.fee,
            token: parsed.token,
        })
    }

    async fn cashu_receive(
        &self,
        wallet: &str,
        token: &str,
    ) -> Result<CashuReceiveResult, PayError> {
        let out = self.first_output(
            self.call(&Input::CashuReceive {
                id: self.gen_id(),
                wallet: Some(wallet.to_string()),
                token: token.to_string(),
            })
            .await,
            &["cashu_received"],
        )?;
        let parsed: CashuReceivedOut = self.parse_output(out, "cashu_received")?;
        Ok(CashuReceiveResult {
            wallet: parsed.wallet,
            amount: parsed.amount,
            memo: parsed.memo,
        })
    }

    async fn send(
        &self,
        wallet: &str,
        to: &str,
        onchain_memo: Option<&str>,
        mints: Option<&[String]>,
    ) -> Result<SendResult, PayError> {
        let out = self.first_output(
            self.call(&Input::Send {
                id: self.gen_id(),
                wallet: Some(wallet.to_string()),
                network: Some(self.network),
                to: to.to_string(),
                onchain_memo: onchain_memo.map(|s| s.to_string()),
                local_memo: None,
                mints: mints.map(|m| m.to_vec()),
            })
            .await,
            &["sent"],
        )?;
        let parsed: SentOut = self.parse_output(out, "sent")?;
        Ok(SendResult {
            wallet: parsed.wallet,
            transaction_id: parsed.transaction_id,
            amount: parsed.amount,
            fee: parsed.fee,
            preimage: parsed.preimage,
        })
    }

    async fn restore(&self, wallet: &str) -> Result<RestoreResult, PayError> {
        let out = self.first_output(
            self.call(&Input::Restore {
                id: self.gen_id(),
                wallet: wallet.to_string(),
            })
            .await,
            &["restored"],
        )?;
        let parsed: RestoredOut = self.parse_output(out, "restored")?;
        Ok(RestoreResult {
            wallet: parsed.wallet,
            unspent: parsed.unspent,
            spent: parsed.spent,
            pending: parsed.pending,
            unit: parsed.unit,
        })
    }

    async fn history_list(
        &self,
        wallet: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<HistoryRecord>, PayError> {
        let out = self.first_output(
            self.call(&Input::HistoryList {
                id: self.gen_id(),
                wallet: Some(wallet.to_string()),
                network: None,
                onchain_memo: None,
                limit: Some(limit),
                offset: Some(offset),
                since_epoch_s: None,
                until_epoch_s: None,
            })
            .await,
            &["history"],
        )?;
        let parsed: HistoryOut = self.parse_output(out, "history")?;
        Ok(parsed.items)
    }

    async fn history_status(&self, transaction_id: &str) -> Result<HistoryStatusInfo, PayError> {
        let out = self.first_output(
            self.call(&Input::HistoryStatus {
                id: self.gen_id(),
                transaction_id: transaction_id.to_string(),
            })
            .await,
            &["history_status"],
        )?;
        let parsed: HistoryStatusOut = self.parse_output(out, "history_status")?;
        Ok(HistoryStatusInfo {
            transaction_id: parsed.transaction_id,
            status: parsed.status,
            confirmations: parsed.confirmations,
            preimage: parsed.preimage,
            item: parsed.item,
        })
    }

    async fn history_sync(&self, wallet: &str, limit: usize) -> Result<HistorySyncStats, PayError> {
        let out = self.first_output(
            self.call(&Input::HistoryUpdate {
                id: self.gen_id(),
                wallet: Some(wallet.to_string()),
                network: Some(self.network),
                limit: Some(limit),
            })
            .await,
            &["history_updated"],
        )?;
        let parsed: HistoryUpdatedOut = self.parse_output(out, "history_updated")?;
        Ok(HistorySyncStats {
            records_scanned: parsed.records_scanned,
            records_added: parsed.records_added,
            records_updated: parsed.records_updated,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn first_output_skips_log_events() {
        let provider = RemoteProvider::new("http://127.0.0.1:1", "secret", Network::Cashu);
        let out = provider
            .first_output(
                vec![
                    serde_json::json!({"code": "log", "event": "startup"}),
                    serde_json::json!({"code": "wallet_list", "wallets": []}),
                ],
                &["wallet_list"],
            )
            .expect("wallet_list output");
        assert_eq!(out["code"], "wallet_list");
    }

    #[test]
    fn first_output_maps_error() {
        let provider = RemoteProvider::new("http://127.0.0.1:1", "secret", Network::Cashu);
        let err = provider
            .first_output(
                vec![
                    serde_json::json!({"code": "log", "event": "wallet"}),
                    serde_json::json!({"code": "error", "error_code": "wallet_not_found", "error": "missing"}),
                ],
                &["wallet_list"],
            )
            .expect_err("error output should be mapped");
        assert!(matches!(err, PayError::WalletNotFound(_)));
    }

    #[test]
    fn first_output_maps_limit_exceeded() {
        let provider = RemoteProvider::new("http://127.0.0.1:1", "secret", Network::Cashu);
        let err = provider
            .first_output(
                vec![serde_json::json!({
                    "code": "limit_exceeded",
                    "rule_id": "r_abc123",
                    "spent": 1500,
                    "max_spend": 1000,
                    "remaining_s": 42
                })],
                &["sent"],
            )
            .expect_err("limit_exceeded should be mapped");
        match err {
            PayError::LimitExceeded {
                rule_id,
                spent,
                max_spend,
                remaining_s,
                ..
            } => {
                assert_eq!(rule_id, "r_abc123");
                assert_eq!(spent, 1500);
                assert_eq!(max_spend, 1000);
                assert_eq!(remaining_s, 42);
            }
            other => panic!("expected LimitExceeded, got: {other:?}"),
        }
    }

    #[test]
    fn balance_parses_current_wallet_balances_schema() {
        let provider = RemoteProvider::new("http://127.0.0.1:1", "secret", Network::Cashu);
        let balance = provider
            .balance_from_output(
                serde_json::json!({
                    "code": "wallet_balances",
                    "wallets": [{
                        "id": "w_1",
                        "network": "cashu",
                        "address": "https://mint.example",
                        "created_at_epoch_s": 1,
                        "balance": {
                            "confirmed": 42,
                            "pending": 0,
                            "unit": "sats"
                        }
                    }]
                }),
                "w_1",
            )
            .expect("balance should parse");
        assert_eq!(balance.confirmed, 42);
        assert_eq!(balance.unit, "sats");
    }
}
