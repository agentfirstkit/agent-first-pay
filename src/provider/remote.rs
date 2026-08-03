use crate::mode::rpc::crypto::Cipher;
use crate::mode::rpc::proto::af_pay_client::AfPayClient;
use crate::mode::rpc::proto::{EncryptedRequest, HandshakeRequest};
use agent_first_data::OutputFormat;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;
use tonic::transport::Channel;

/// Process-wide cache of negotiated RPC sessions, keyed by (endpoint, secret).
/// The first call to a given endpoint runs Handshake; subsequent calls reuse the
/// cached `session_id + Cipher` until the server reports `session_expired`, at
/// which point we re-handshake transparently and retry once.
static SESSION_CACHE: std::sync::OnceLock<Mutex<HashMap<(String, String), CachedSession>>> =
    std::sync::OnceLock::new();

#[derive(Clone)]
struct CachedSession {
    session_id: u64,
    cipher: Cipher,
    #[allow(dead_code)]
    opened_at: Instant,
}

fn session_cache() -> &'static Mutex<HashMap<(String, String), CachedSession>> {
    SESSION_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key(endpoint: &str, secret: &str) -> (String, String) {
    (endpoint.to_string(), secret.to_string())
}

fn get_cached_session(endpoint: &str, secret: &str) -> Option<CachedSession> {
    session_cache()
        .lock()
        .ok()
        .and_then(|g| g.get(&cache_key(endpoint, secret)).cloned())
}

fn store_session(endpoint: &str, secret: &str, session: CachedSession) {
    if let Ok(mut g) = session_cache().lock() {
        g.insert(cache_key(endpoint, secret), session);
    }
}

fn drop_session(endpoint: &str, secret: &str) {
    if let Ok(mut g) = session_cache().lock() {
        g.remove(&cache_key(endpoint, secret));
    }
}

fn endpoint_url(endpoint: &str) -> String {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        endpoint.to_string()
    } else {
        format!("http://{endpoint}")
    }
}

async fn connect(endpoint: &str) -> Result<AfPayClient<Channel>, String> {
    AfPayClient::connect(endpoint_url(endpoint))
        .await
        .map_err(|e| e.to_string())
}

/// Negotiate a fresh session with the daemon: Handshake → derive Cipher from
/// (PSK, server-issued salt) → cache the result. Returns the cached session that
/// was just inserted so the caller can use it without re-reading the cache.
async fn handshake(endpoint: &str, secret: &str) -> Result<CachedSession, String> {
    let mut client = connect(endpoint).await?;
    let resp = client
        .handshake(HandshakeRequest {
            // Client nonce currently unused by the server (see proto note); we
            // still send 16 zero bytes so the field is present for future use.
            client_nonce: vec![0u8; 16],
        })
        .await
        .map_err(|e| e.to_string())?
        .into_inner();
    let session = CachedSession {
        session_id: resp.session_id,
        cipher: Cipher::from_secret_with_salt(secret, &resp.salt),
        opened_at: Instant::now(),
    };
    store_session(endpoint, secret, session.clone());
    Ok(session)
}

async fn session_for(endpoint: &str, secret: &str) -> Result<CachedSession, String> {
    if let Some(cached) = get_cached_session(endpoint, secret) {
        return Ok(cached);
    }
    handshake(endpoint, secret).await
}

/// Send an Input to a remote RPC server, return the decrypted Output array.
///
/// First-call cost is one Handshake + one Call (≈ 2 round-trips). Cached
/// sessions reduce subsequent calls to a single round-trip. If the daemon
/// rejects the cached session as expired, the client re-handshakes once and
/// retries the request transparently.
pub async fn rpc_call(
    endpoint: &str,
    secret: &str,
    input: &impl serde::Serialize,
) -> Vec<serde_json::Value> {
    let input_json = match serde_json::to_vec(input) {
        Ok(v) => v,
        Err(e) => return vec![rpc_error_output("serialize_error", &format!("{e}"))],
    };

    // First attempt with cached session (or fresh handshake if none).
    match attempt_call(endpoint, secret, &input_json).await {
        AttemptOutcome::Success(outputs) => outputs,
        AttemptOutcome::SessionExpired => {
            drop_session(endpoint, secret);
            match attempt_call(endpoint, secret, &input_json).await {
                AttemptOutcome::Success(outputs) => outputs,
                AttemptOutcome::SessionExpired => vec![rpc_error_output(
                    "session_expired",
                    "daemon kept rejecting session after re-handshake",
                )],
                AttemptOutcome::Error(out) => vec![out],
            }
        }
        AttemptOutcome::Error(out) => vec![out],
    }
}

enum AttemptOutcome {
    Success(Vec<serde_json::Value>),
    /// Server returned Unauthenticated with `session_expired`; caller should
    /// drop the cached session and re-handshake.
    SessionExpired,
    Error(serde_json::Value),
}

async fn attempt_call(endpoint: &str, secret: &str, input_json: &[u8]) -> AttemptOutcome {
    let session = match session_for(endpoint, secret).await {
        Ok(s) => s,
        Err(e) => return AttemptOutcome::Error(rpc_error_output("connect_error", &e)),
    };

    let (nonce, ciphertext) = match session.cipher.encrypt(input_json) {
        Ok(v) => v,
        Err(e) => return AttemptOutcome::Error(rpc_error_output("encrypt_error", &e)),
    };

    let mut client = match connect(endpoint).await {
        Ok(c) => c,
        Err(e) => return AttemptOutcome::Error(rpc_error_output("connect_error", &e)),
    };

    let response = match client
        .call(EncryptedRequest {
            session_id: session.session_id,
            nonce,
            ciphertext,
        })
        .await
    {
        Ok(r) => r,
        Err(status) => {
            if status.code() == tonic::Code::Unauthenticated
                && status.message().contains("session_expired")
            {
                return AttemptOutcome::SessionExpired;
            }
            let error_code = match status.code() {
                tonic::Code::PermissionDenied => "permission_denied",
                tonic::Code::Unauthenticated => "unauthenticated",
                tonic::Code::Unavailable => "unavailable",
                tonic::Code::InvalidArgument => "invalid_argument",
                _ => "rpc_error",
            };
            return AttemptOutcome::Error(rpc_error_output(error_code, status.message()));
        }
    };

    let resp = response.into_inner();
    let plaintext = match session.cipher.decrypt(&resp.nonce, &resp.ciphertext) {
        Ok(v) => v,
        Err(e) => return AttemptOutcome::Error(rpc_error_output("decrypt_error", &e)),
    };

    match serde_json::from_slice::<Vec<serde_json::Value>>(plaintext.as_slice()) {
        Ok(events) => match decode_protocol_events(events) {
            Ok(outputs) => AttemptOutcome::Success(outputs),
            Err(error) => AttemptOutcome::Error(rpc_error_output("remote_protocol_error", &error)),
        },
        Err(e) => AttemptOutcome::Error(rpc_error_output("parse_error", &format!("{e}"))),
    }
}

fn decode_protocol_events(
    events: Vec<serde_json::Value>,
) -> Result<Vec<serde_json::Value>, String> {
    events
        .into_iter()
        .map(|event| {
            agent_first_data::validate_protocol_event(&event, true)
                .map_err(|violation| violation.to_string())?;
            let kind = event
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "protocol event kind is missing".to_string())?;
            let mut payload = event
                .get(kind)
                .cloned()
                .ok_or_else(|| format!("protocol event payload {kind:?} is missing"))?;
            if kind == "error" {
                let fields = payload
                    .as_object_mut()
                    .ok_or_else(|| "error payload must be an object".to_string())?;
                let code = fields
                    .remove("code")
                    .unwrap_or_else(|| serde_json::Value::String("remote_error".to_string()));
                let message = fields
                    .remove("message")
                    .unwrap_or_else(|| serde_json::Value::String("remote error".to_string()));
                fields.insert(
                    "code".to_string(),
                    serde_json::Value::String("error".to_string()),
                );
                fields.insert("error_code".to_string(), code);
                fields.insert("error".to_string(), message);
            }
            // Re-attach the envelope's `trace` alongside the rest of the flat
            // fields: afpay's own `Output` shape always carries `trace` at the
            // same level as `code` (never nested), matching what local
            // (non-RPC) dispatch produces, and every AFDATA builder pulls
            // `trace` out to the envelope's top level before this point.
            if let Some(fields) = payload.as_object_mut()
                && let Some(trace) = event.get("trace")
            {
                fields.insert("trace".to_string(), trace.clone());
            }
            Ok(payload)
        })
        .collect()
}

/// Build a flat `Output::Error`-shaped value for a client-side failure that
/// never reached the daemon (connect/decrypt/parse/session errors). This is
/// deliberately the same un-enveloped `code:"error"` shape [`decode_protocol_events`]
/// produces when it unwraps a real daemon error, not an AFDATA protocol
/// envelope — every consumer of this crate's `outputs: Vec<Value>` (
/// [`RemoteProvider::map_remote_error`], the CLI's `emit_remote_outputs`, and
/// [`crate::output_fmt::render_value_with_policy`]'s own envelope-wrapping
/// fallback) keys off this flat shape.
fn rpc_error_output(error_code: &str, error: &str) -> serde_json::Value {
    let hint = match error_code {
        "connect_error" => Some("check --rpc-endpoint address and that the daemon is running"),
        "unauthenticated" | "decrypt_error" => Some("check --rpc-secret matches the daemon"),
        "permission_denied" => Some("this operation can only be run on the daemon directly"),
        _ => None,
    };
    let mut value = serde_json::json!({
        "code": "error",
        "error_code": error_code,
        "error": error,
        "retryable": matches!(error_code, "connect_error" | "unavailable"),
    });
    if let Some(h) = hint {
        value["hint"] = serde_json::Value::String(h.to_string());
    }
    value
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
            let value: serde_json::Value = agent_first_data::build_cli_error(
                "--rpc-endpoint is required",
                Some("pass the address of the afpay daemon"),
            )
            .into();
            let _ = crate::output_fmt::emit_process_event(value, format);
            std::process::exit(1);
        }
    };
    let sec = match secret {
        Some(s) if !s.is_empty() => s,
        _ => {
            let value: serde_json::Value = agent_first_data::build_cli_error(
                "--rpc-secret is required with --rpc-endpoint",
                Some("must match the --rpc-secret used by the daemon"),
            )
            .into();
            let _ = crate::output_fmt::emit_process_event(value, format);
            std::process::exit(1);
        }
    };
    (ep, sec)
}

/// Render remote RPC outputs, filtering log events. Returns true if any output was an error.
pub fn emit_remote_outputs(
    outputs: &[serde_json::Value],
    format: OutputFormat,
    log_filters: &agent_first_data::LogFilters,
) -> bool {
    let mut had_error = false;
    for value in outputs {
        let kind = value.get("kind").and_then(|v| v.as_str());
        let payload = kind.and_then(|kind| value.get(kind)).unwrap_or(value);
        if kind == Some("error") || payload.get("code").and_then(|v| v.as_str()) == Some("error") {
            had_error = true;
        }
        if (kind == Some("log") || payload.get("code").and_then(|v| v.as_str()) == Some("log"))
            && let Some(event) = payload.get("event").and_then(|v| v.as_str())
            && !log_filters.enabled(event)
        {
            continue;
        }
        let _ = crate::output_fmt::emit_process_value_with_policy(value, format);
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

// ═══════════════════════════════════════════
// RemoteProvider — PayProvider over RPC
// ═══════════════════════════════════════════

use crate::provider::{HistorySyncStats, PayError, PayProvider};
use crate::types::*;
use async_trait::async_trait;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::sync::atomic::{AtomicU64, Ordering};

static REMOTE_REQUEST_FALLBACK_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Deserialize)]
struct WalletCreatedOut {
    wallet: String,
    address: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    #[serde(alias = "mnemonic")]
    mnemonic_secret: Option<String>,
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

    /// Extract a structured `LimitExceeded` from a remote response, or report
    /// `RemoteProtocolError` if any required field is missing or has the wrong
    /// type. This refuses to fabricate a partial LimitExceeded with zeros and
    /// empty strings — silently dropping fields had previously let bad upstream
    /// JSON look like a legitimate limit hit, which then surprised callers.
    fn parse_limit_exceeded(&self, value: &serde_json::Value) -> PayError {
        // rule_id, scope_key, scope, spent, max_spend, remaining_s — all required.
        let required_str = |field: &str| -> Result<String, String> {
            value
                .get(field)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| format!("limit_exceeded missing required string field `{field}`"))
        };
        let required_u64 = |field: &str| -> Result<u64, String> {
            value
                .get(field)
                .and_then(|v| v.as_u64())
                .ok_or_else(|| format!("limit_exceeded missing required u64 field `{field}`"))
        };

        let rule_id = match required_str("rule_id") {
            Ok(v) => v,
            Err(detail) => return self.protocol_error(detail),
        };
        let scope_key = match required_str("scope_key") {
            Ok(v) => v,
            Err(detail) => return self.protocol_error(detail),
        };
        let scope = match value.get("scope").cloned() {
            Some(raw) => match serde_json::from_value::<SpendScope>(raw) {
                Ok(s) => s,
                Err(e) => {
                    return self.protocol_error(format!(
                        "limit_exceeded scope is not a known variant: {e}"
                    ));
                }
            },
            None => {
                return self
                    .protocol_error("limit_exceeded missing required field `scope`".to_string());
            }
        };
        let spent = match required_u64("spent") {
            Ok(v) => v,
            Err(detail) => return self.protocol_error(detail),
        };
        let max_spend = match required_u64("max_spend") {
            Ok(v) => v,
            Err(detail) => return self.protocol_error(detail),
        };
        let remaining_s = match required_u64("remaining_s") {
            Ok(v) => v,
            Err(detail) => return self.protocol_error(detail),
        };

        PayError::LimitExceeded {
            rule_id,
            scope,
            scope_key,
            spent,
            max_spend,
            token: value
                .get("token")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            remaining_s,
            origin: Some(
                value
                    .get("origin")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| self.endpoint.clone()),
            ),
            hint: value
                .get("hint")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        }
    }

    fn protocol_error(&self, detail: String) -> PayError {
        PayError::RemoteProtocolError {
            endpoint: self.endpoint.clone(),
            detail,
            hint: Some(
                "the remote daemon returned a malformed limit_exceeded payload; verify it is running a compatible afpay version"
                    .to_string(),
            ),
        }
    }

    fn map_remote_error(&self, value: &serde_json::Value) -> Option<PayError> {
        let code = value
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        match code {
            "error" => {
                // For non-LimitExceeded errors the daemon's `error` string is
                // authoritative; surface it verbatim under the variant the
                // error_code maps to so the agent sees the same retryable flag
                // and hint it would see for a local error.
                let msg = value
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                let error_code = value
                    .get("error_code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("remote_error");
                Some(match error_code {
                    "wallet_not_found" => PayError::wallet_not_found(msg.to_string()),
                    "invalid_amount" => PayError::invalid_amount(msg.to_string()),
                    "not_implemented" => PayError::not_implemented(msg.to_string()),
                    "limit_exceeded" => self.parse_limit_exceeded(value),
                    _ => PayError::network_error(msg.to_string()),
                })
            }
            "limit_exceeded" => Some(self.parse_limit_exceeded(value)),
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
            return Err(PayError::network_error(format!(
                "unexpected remote output code '{code}'"
            )));
        }
        Err(PayError::network_error(
            "empty or log-only response from remote".to_string(),
        ))
    }

    fn parse_output<T: DeserializeOwned>(
        &self,
        value: serde_json::Value,
        label: &str,
    ) -> Result<T, PayError> {
        serde_json::from_value(value)
            .map_err(|e| PayError::network_error(format!("parse {label}: {e}")))
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
            return Err(PayError::wallet_not_found(format!(
                "wallet {wallet} not found in remote balance response"
            )));
        };
        item.balance.ok_or_else(|| {
            PayError::network_error(
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
                    return Err(PayError::network_error(format!(
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
                sol_cluster: None,
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
            mnemonic_secret: parsed.mnemonic_secret,
        })
    }

    async fn create_ln_wallet(
        &self,
        request: LnWalletCreateRequest,
    ) -> Result<WalletInfo, PayError> {
        if self.network != Network::Ln {
            return Err(PayError::invalid_amount(
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
            mnemonic_secret: parsed.mnemonic_secret,
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
                // Remote-provider hop is the upstream daemon's view; idempotency
                // is enforced at the agent-facing boundary, not on inter-daemon
                // proxy calls.
                idempotency_key: None,
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
                amount: None,
                onchain_memo: onchain_memo.map(|s| s.to_string()),
                local_memo: None,
                mints: mints.map(|m| m.to_vec()),
                chain_id: None,
                // See cashu_send above — idempotency enforced at the agent boundary.
                idempotency_key: None,
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
        assert!(matches!(err, PayError::WalletNotFound { .. }));
    }

    #[test]
    fn first_output_maps_limit_exceeded() {
        let provider = RemoteProvider::new("http://127.0.0.1:1", "secret", Network::Cashu);
        let err = provider
            .first_output(
                vec![serde_json::json!({
                    "code": "limit_exceeded",
                    "rule_id": "r_abc123",
                    "scope": "network",
                    "scope_key": "cashu",
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
    fn malformed_limit_exceeded_yields_remote_protocol_error() {
        // Missing required scope_key — strict parser must refuse rather than
        // fabricate a LimitExceeded with empty strings/zeros, which would have
        // looked like a legitimate cap hit to the agent.
        let provider = RemoteProvider::new("http://127.0.0.1:1", "secret", Network::Cashu);
        let err = provider
            .first_output(
                vec![serde_json::json!({
                    "code": "limit_exceeded",
                    "rule_id": "r_abc123",
                    "scope": "network",
                    // scope_key omitted intentionally
                    "spent": 1500,
                    "max_spend": 1000,
                    "remaining_s": 42
                })],
                &["sent"],
            )
            .expect_err("malformed limit_exceeded should be mapped to a protocol error");
        match err {
            PayError::RemoteProtocolError { detail, .. } => {
                assert!(
                    detail.contains("scope_key"),
                    "detail should name the missing field, got: {detail}"
                );
            }
            other => panic!("expected RemoteProtocolError, got: {other:?}"),
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
