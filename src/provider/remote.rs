//! Federation: another afpay node used as a local [`PayProvider`].
//!
//! The wire is the peer's own HTTP domain API — the `/v1` resource routes
//! `afpay --mode rest` serves — carrying the peer's bearer API key. There is
//! no afpay-specific transport left: no handshake, no session table, no
//! payload cipher, nothing to negotiate before the first request. Every call
//! is one HTTP round trip.
//!
//! Encryption and peer identity are the operator's network's job — Tailscale
//! or WireGuard, an SSH tunnel, or a TLS reverse proxy. None of those is
//! authentication, so the bearer credential is required in all three cases.
//! README's "Reaching a daemon that is not on this machine" is the guidance.
//!
//! **This client can only reach what the REST face publishes.** Everything
//! [`Input::is_local_only`] marks — seed material, spend-limit rule writes,
//! reservation reconcile, wallet-config writes, `cashu wallet restore` — is
//! refused here, before any bytes leave the process, with the same "run it on
//! the daemon host" answer the peer itself would give. Federation does not get
//! a private door into a peer that an agent holding the same token does not
//! have; that symmetry is what keeps a leaked bearer from raising its own
//! spending limit.

use agent_first_data::OutputFormat;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

/// How long to wait for the TCP/TLS connection itself. There is deliberately
/// no whole-request timeout: `receive --wait` legitimately holds a request
/// open until a payment settles.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Bytes of a non-afpay response body quoted back in the error that names the
/// mismatch. Enough to recognise an nginx 404 page or a gRPC frame; short
/// enough not to dump a page into an agent's context.
const BODY_SNIPPET_BYTES: usize = 160;

/// One shared connection pool. This is HTTP keep-alive, not session state:
/// nothing is negotiated, nothing expires, and a cold client behaves
/// identically to a warm one.
fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .unwrap_or_default()
    })
}

// ═══════════════════════════════════════════
// Where a request goes
// ═══════════════════════════════════════════

/// One peer route, resolved from an [`Input`].
///
/// `result_code` is the union tag the peer's REST face strips before it ships
/// a result (the route already fixes the shape, so `code` is transport noise
/// there). This client puts it back, because everything downstream — the
/// `RemoteProvider` parsers, `emit_remote_outputs`, the topology wrapper —
/// reads afpay's flat `Output` shape, which is tagged.
struct RoutePlan {
    method: Method,
    path: String,
    query: Vec<(&'static str, String)>,
    body: Option<Value>,
    /// Payments require it; the peer refuses without one.
    idempotency_key: Option<String>,
    result_code: &'static str,
    /// `/health` is the one unauthenticated route, and the only one whose
    /// payload is checked for "is this actually afpay".
    identity_probe: bool,
}

impl RoutePlan {
    fn get(path: impl Into<String>, result_code: &'static str) -> Self {
        Self {
            method: Method::GET,
            path: path.into(),
            query: Vec::new(),
            body: None,
            idempotency_key: None,
            result_code,
            identity_probe: false,
        }
    }

    fn post(path: impl Into<String>, body: Value, result_code: &'static str) -> Self {
        Self {
            method: Method::POST,
            path: path.into(),
            query: Vec::new(),
            body: Some(body),
            idempotency_key: None,
            result_code,
            identity_probe: false,
        }
    }

    fn delete(path: impl Into<String>, result_code: &'static str) -> Self {
        Self {
            method: Method::DELETE,
            path: path.into(),
            query: Vec::new(),
            body: None,
            idempotency_key: None,
            result_code,
            identity_probe: false,
        }
    }

    fn query(mut self, name: &'static str, value: Option<String>) -> Self {
        if let Some(value) = value {
            self.query.push((name, value));
        }
        self
    }

    fn idempotent(mut self, key: &str) -> Self {
        self.idempotency_key = Some(key.to_string());
        self
    }

    /// How the request reads in an error message: `GET /v1/wallets`.
    fn describe(&self) -> String {
        format!("{} {}", self.method, self.path)
    }
}

/// Resolve an `Input` to the peer route that serves it.
///
/// `Err` is a refusal this client makes on its own behalf, without contacting
/// the peer: the operation has no route on the REST face and must not grow
/// one. That list is [`Input::is_local_only`] plus the two session verbs
/// (`Close`, `Schema`) that describe a local process rather than a resource.
fn route_for(input: &Input) -> Result<RoutePlan, Value> {
    if input.is_local_only() {
        return Err(local_only_refusal(input));
    }
    let plan = match input {
        Input::Version => RoutePlan {
            identity_probe: true,
            ..RoutePlan::get("/health", "version")
        },

        Input::WalletList { network, .. } => RoutePlan::get("/v1/wallets", "wallet_list")
            .query("network", network.map(|network| network.to_string())),

        // The peer requires a key on both of these; the request id stands in
        // when the caller did not supply one, exactly as it does for a confirm.
        Input::WalletCreate {
            id,
            idempotency_key,
            ..
        }
        | Input::LnWalletCreate {
            id,
            idempotency_key,
            ..
        } => RoutePlan::post("/v1/wallets", wallet_create_body(input), "wallet_created")
            .idempotent(idempotency_key.as_deref().unwrap_or(id.as_str())),

        Input::WalletConfigShow { wallet, .. } => RoutePlan::get(
            format!("/v1/wallets/{}", path_segment(wallet)),
            "wallet_config",
        ),

        // The `dangerously_skip_balance_check_and_may_lose_money: true` shape
        // is local-only and was already refused above.
        Input::WalletClose { wallet, .. } => RoutePlan::delete(
            format!("/v1/wallets/{}", path_segment(wallet)),
            "wallet_closed",
        ),

        Input::Balance {
            wallet,
            network,
            check,
            ..
        } => RoutePlan::get("/v1/balances", "wallet_balances")
            .query("wallet", wallet.clone())
            .query("network", network.map(|network| network.to_string()))
            .query("check", check.then(|| "true".to_string())),

        Input::Receive {
            id,
            idempotency_key,
            ..
        } => RoutePlan::post("/v1/receives", receive_body(input), "receive_info")
            .idempotent(idempotency_key.as_deref().unwrap_or(id.as_str())),

        Input::ReceiveClaim {
            wallet, quote_id, ..
        } => RoutePlan::post(
            format!("/v1/receives/{}/claim", path_segment(quote_id)),
            json!({"wallet": wallet}),
            "receive_claimed",
        ),

        Input::SendPlan {
            wallet,
            network,
            to,
            amount,
            onchain_memo,
            local_memo,
            mints,
            chain_id,
            ..
        } => RoutePlan::post(
            "/v1/send-plans",
            json!({
                "to": to,
                "wallet": wallet,
                "network": network,
                "amount": amount,
                "onchain_memo": onchain_memo,
                "local_memo": local_memo,
                "mints": mints,
                "chain_id": chain_id,
            }),
            "pay_planned",
        ),

        Input::CashuSendPlan {
            wallet,
            amount,
            onchain_memo,
            local_memo,
            mints,
            ..
        } => RoutePlan::post(
            "/v1/cashu/token-plans",
            json!({
                "amount": amount,
                "wallet": wallet,
                "onchain_memo": onchain_memo,
                "local_memo": local_memo,
                "mints": mints,
            }),
            "pay_planned",
        ),

        // A confirm names one plan on the peer, and the peer decides what that
        // plan authorises. This client picks the route by what it planned; the
        // peer refuses if the two disagree.
        Input::PayConfirm {
            id,
            plan_id,
            expect,
            idempotency_key,
        } => {
            let path = match expect {
                Some(PayPlanOperation::CashuSend) => "/v1/cashu/tokens",
                _ => "/v1/sends",
            };
            let result_code = match expect {
                Some(PayPlanOperation::CashuSend) => "cashu_sent",
                _ => "sent",
            };
            RoutePlan::post(path, json!({"plan_id": plan_id}), result_code)
                .idempotent(idempotency_key.as_deref().unwrap_or(id.as_str()))
        }

        Input::CashuReceive { wallet, token, .. } => RoutePlan::post(
            "/v1/cashu/redemptions",
            json!({"token": token, "wallet": wallet}),
            "cashu_received",
        ),

        Input::HistoryList {
            wallet,
            network,
            onchain_memo,
            limit,
            offset,
            since_epoch_s,
            until_epoch_s,
            ..
        } => RoutePlan::get("/v1/transactions", "history")
            .query("wallet", wallet.clone())
            .query("network", network.map(|network| network.to_string()))
            .query("onchain_memo", onchain_memo.clone())
            .query("limit", limit.map(|value| value.to_string()))
            .query("offset", offset.map(|value| value.to_string()))
            .query("since_epoch_s", since_epoch_s.map(|v| v.to_string()))
            .query("until_epoch_s", until_epoch_s.map(|v| v.to_string())),

        Input::HistoryStatus { transaction_id, .. } => RoutePlan::get(
            format!("/v1/transactions/{}", path_segment(transaction_id)),
            "history_status",
        ),

        Input::HistoryUpdate {
            wallet,
            network,
            limit,
            ..
        } => RoutePlan::post(
            "/v1/transactions/sync",
            json!({"wallet": wallet, "network": network, "limit": limit}),
            "history_updated",
        ),

        Input::LimitList { .. } => RoutePlan::get("/v1/spend-limits", "limit_status"),

        // A long-lived local process, not a resource on another machine.
        Input::Close | Input::Schema => return Err(no_peer_route_refusal(input)),

        // Every remaining variant is local-only and was refused above; this
        // arm exists so a new `Input` cannot silently fall through to a
        // request with no route.
        other => return Err(no_peer_route_refusal(other)),
    };
    Ok(plan)
}

fn wallet_create_body(input: &Input) -> Value {
    match input {
        Input::LnWalletCreate { request, .. } => json!({
            "network": "ln",
            "label": request.label,
            "backend": request.backend,
            "endpoint_url": request.endpoint_url,
            "nwc_uri_secret": request.nwc_uri_secret,
            "password_secret": request.password_secret,
            "admin_key_secret": request.admin_key_secret,
        }),
        Input::WalletCreate {
            network,
            label,
            mint_url,
            rpc_endpoints,
            chain_id,
            mnemonic_secret,
            btc_esplora_url,
            btc_network,
            btc_address_type,
            btc_backend,
            btc_core_url,
            btc_core_auth_secret,
            btc_electrum_url,
            sol_cluster,
            ..
        } => match network {
            Network::Cashu => json!({
                "network": "cashu",
                "label": label,
                "mint_url": mint_url,
                "mnemonic_secret": mnemonic_secret,
            }),
            Network::Sol => json!({
                "network": "sol",
                "label": label,
                "rpc_endpoints": rpc_endpoints,
                "cluster": sol_cluster,
                "mnemonic_secret": mnemonic_secret,
            }),
            Network::Evm => json!({
                "network": "evm",
                "label": label,
                "rpc_endpoints": rpc_endpoints,
                "chain_id": chain_id,
                "mnemonic_secret": mnemonic_secret,
            }),
            Network::Btc => json!({
                "network": "btc",
                "label": label,
                "backend": btc_backend,
                "esplora_url": btc_esplora_url,
                "core_url": btc_core_url,
                "core_auth_secret": btc_core_auth_secret,
                "electrum_url": btc_electrum_url,
                "btc_network": btc_network,
                "address_type": btc_address_type,
                "mnemonic_secret": mnemonic_secret,
            }),
            // `Input::WalletCreate { network: Ln }` is not reachable — the
            // registry routes every Lightning create through LnWalletCreate —
            // but the peer's tagged union still needs a discriminant.
            Network::Ln => json!({"network": "ln", "label": label}),
        },
        _ => Value::Null,
    }
}

fn receive_body(input: &Input) -> Value {
    let Input::Receive {
        wallet,
        network,
        amount,
        onchain_memo,
        wait_until_paid,
        wait_timeout_s,
        wait_poll_interval_ms,
        wait_sync_limit,
        min_confirmations,
        reference,
        // `write_qr_svg_file` renders a file on *this* machine; the peer has
        // no business knowing about it, and the caller writes the QR from the
        // receive_info that comes back.
        ..
    } = input
    else {
        return Value::Null;
    };
    json!({
        "wallet": wallet,
        "network": network,
        "amount": amount,
        "onchain_memo": onchain_memo,
        "min_confirmations": min_confirmations,
        "reference": reference,
        "wait": wait_until_paid.then(|| json!({
            "timeout_s": wait_timeout_s,
            "poll_interval_ms": wait_poll_interval_ms,
            "sync_limit": wait_sync_limit,
        })),
    })
}

/// Percent-encode one path segment. Wallet ids and transaction ids come from
/// the peer, but a label-resolved id or an operator typo must not be able to
/// climb out of the segment it was written into.
fn path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

/// `host:port`, `http://host:port`, or a URL with a path prefix all name the
/// same thing; normalise to a base with no trailing slash. Scheme-less input
/// is assumed plaintext HTTP, matching how the daemon binds.
fn base_url(peer_url: &str) -> String {
    let trimmed = peer_url.trim().trim_end_matches('/');
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    }
}

// ═══════════════════════════════════════════
// The call
// ═══════════════════════════════════════════

/// Send one `Input` to a peer afpay node and return its outputs in afpay's
/// flat `Output` shape — the same values a local dispatch would have produced.
///
/// Exactly one output comes back per call: HTTP answers a request with a
/// result or an error, and the peer's own log events stay on the peer's log
/// stream rather than being tunnelled into this process's output.
pub async fn peer_call(peer_url: &str, api_key_secret: &str, input: &Input) -> Vec<Value> {
    let plan = match route_for(input) {
        Ok(plan) => plan,
        Err(refusal) => return vec![refusal],
    };
    match send(peer_url, api_key_secret, &plan).await {
        Ok(value) | Err(value) => vec![value],
    }
}

async fn send(peer_url: &str, api_key_secret: &str, plan: &RoutePlan) -> Result<Value, Value> {
    let url = format!("{}{}", base_url(peer_url), plan.path);
    let mut request = http_client().request(plan.method.clone(), &url);
    if !plan.query.is_empty() {
        request = request.query(&plan.query);
    }
    if !plan.identity_probe {
        request = request.bearer_auth(api_key_secret);
    }
    if let Some(key) = &plan.idempotency_key {
        request = request.header("idempotency-key", key);
    }
    if let Some(body) = &plan.body {
        request = request.json(body);
    }

    let response = request.send().await.map_err(|error| {
        peer_error(
            "peer_unreachable",
            format!("cannot reach afpay peer at {url}: {error}"),
            Some(
                "check --peer-url and that the peer's HTTP API is running and reachable from here",
            ),
            true,
        )
    })?;

    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("(none)")
        .to_string();
    let bytes = response.bytes().await.map_err(|error| {
        peer_error(
            "peer_unreachable",
            format!("afpay peer at {url} closed the response early: {error}"),
            None,
            true,
        )
    })?;

    let envelope: Value = serde_json::from_slice(&bytes)
        .map_err(|_| not_afpay(&url, plan, status, &content_type, &snippet(&bytes)))?;
    interpret(envelope, plan, &url, status, &content_type, &bytes)
}

/// Turn the peer's AFDATA envelope into the flat `Output`-shaped value the
/// rest of this crate reads, or into a refusal that names the mismatch.
fn interpret(
    envelope: Value,
    plan: &RoutePlan,
    url: &str,
    status: StatusCode,
    content_type: &str,
    raw: &[u8],
) -> Result<Value, Value> {
    let kind = envelope.get("kind").and_then(Value::as_str);
    let trace = envelope.get("trace").cloned();

    match kind {
        Some("result") if status.is_success() => {
            let mut payload = envelope
                .get("result")
                .cloned()
                .unwrap_or(Value::Object(serde_json::Map::new()));
            let Some(fields) = payload.as_object_mut() else {
                return Err(not_afpay(url, plan, status, content_type, &snippet(raw)));
            };
            if plan.identity_probe && fields.get("service").and_then(Value::as_str) != Some("afpay")
            {
                return Err(peer_error(
                    "peer_not_afpay",
                    format!(
                        "{url}/health answered, but it is not an afpay daemon (service={})",
                        fields
                            .get("service")
                            .and_then(Value::as_str)
                            .unwrap_or("(absent)")
                    ),
                    Some("point --peer-url at an `afpay --mode rest` listener"),
                    false,
                ));
            }
            fields.insert("code".to_string(), json!(plan.result_code));
            if let Some(trace) = trace {
                fields.insert("trace".to_string(), trace);
            }
            Ok(payload)
        }
        Some("error") if !status.is_success() => Ok(peer_failure(&envelope, plan, url, status)),
        // A `kind` this client does not know, or one that disagrees with the
        // HTTP status, is a peer speaking a protocol this build cannot read.
        // Reporting it as a domain answer would be the silent wrong answer.
        _ => Err(not_afpay(url, plan, status, content_type, &snippet(raw))),
    }
}

/// Map the peer's error envelope onto the flat error shape local code reads.
///
/// Two of afpay's refusals travel as errors on HTTP but as *outputs*
/// everywhere else — `limit_exceeded` and `accounting_inconsistent` — and
/// carry their ledger state in `error.details`. Unwrapping them back into
/// tagged outputs is what keeps `RemoteProvider::parse_limit_exceeded` and
/// the CLI's rendering identical to a local run.
fn peer_failure(envelope: &Value, plan: &RoutePlan, url: &str, status: StatusCode) -> Value {
    let error = envelope.get("error").cloned().unwrap_or(Value::Null);
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("remote_error");
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("the afpay peer refused this request");
    let trace = envelope.get("trace").cloned();

    if matches!(code, "limit_exceeded" | "accounting_inconsistent") {
        let mut payload = error.get("details").cloned().unwrap_or(Value::Null);
        if let Some(fields) = payload.as_object_mut() {
            fields.insert("code".to_string(), json!(code));
            if let Some(hint) = error.get("hint") {
                fields.entry("hint").or_insert_with(|| hint.clone());
            }
            if let Some(trace) = trace {
                fields.insert("trace".to_string(), trace);
            }
            return payload;
        }
    }

    // A route this build expects but the peer does not serve means the two
    // sides are not the same afpay. Say that, rather than passing on a bare
    // "API route not found" that reads like the caller's mistake.
    let (code, message, hint) = match (status, code) {
        (_, "api_route_not_found" | "api_method_not_allowed") => (
            "peer_route_unsupported",
            format!(
                "afpay peer at {url} does not serve `{}`; it is not running a compatible afpay version",
                plan.describe()
            ),
            Some(format!(
                "read {url}/health for the peer's version and match it to this node's"
            )),
        ),
        (StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN, "authentication_required") => (
            "peer_unauthorized",
            format!("afpay peer at {url} rejected the credential: {message}"),
            Some("--peer-api-key-secret must be the peer's --rest-api-key-secret".to_string()),
        ),
        _ => (
            code,
            message.to_string(),
            error
                .get("hint")
                .and_then(Value::as_str)
                .map(str::to_string),
        ),
    };

    let mut value = json!({
        "code": "error",
        "error_code": code,
        "error": message,
        "retryable": error.get("retryable").and_then(Value::as_bool).unwrap_or(false),
    });
    if let Some(hint) = hint {
        value["hint"] = json!(hint);
    }
    if let Some(retry_after_ms) = error.get("retry_after_ms").and_then(Value::as_u64) {
        value["retry_after_ms"] = json!(retry_after_ms);
    }
    if let Some(trace) = envelope.get("trace") {
        value["trace"] = trace.clone();
    }
    value
}

fn snippet(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "(empty body)".to_string();
    }
    let mut out: String = trimmed.chars().take(BODY_SNIPPET_BYTES).collect();
    if trimmed.chars().count() > BODY_SNIPPET_BYTES {
        out.push('…');
    }
    out.replace(['\n', '\r'], " ")
}

/// The peer answered, but not with afpay's protocol. Name every part of the
/// mismatch an operator needs in order to see what they actually pointed at.
fn not_afpay(
    url: &str,
    plan: &RoutePlan,
    status: StatusCode,
    content_type: &str,
    body: &str,
) -> Value {
    peer_error(
        "peer_not_afpay",
        format!(
            "`{}` to {url} did not answer with an afpay protocol envelope \
             (HTTP {status}, content-type {content_type}): {body}",
            plan.describe()
        ),
        Some(
            "--peer-url must name an `afpay --mode rest` listener, e.g. http://host:9401; \
             read <url>/health to confirm before retrying",
        ),
        false,
    )
}

/// A refusal this client makes without contacting the peer: the operation is
/// local-only, so it has no route on the peer's REST face and must not grow
/// one. The wording matches the peer's own `forbidden` answer.
fn local_only_refusal(input: &Input) -> Value {
    peer_error(
        "forbidden",
        format!(
            "`{}` is available only on the machine that holds the data; \
             it has no route on an afpay peer",
            input_label(input)
        ),
        Some("run it through the afpay CLI on the peer's host"),
        false,
    )
}

fn no_peer_route_refusal(input: &Input) -> Value {
    peer_error(
        "not_implemented",
        format!(
            "`{}` describes a local afpay process and cannot be forwarded to a peer",
            input_label(input)
        ),
        None,
        false,
    )
}

fn input_label(input: &Input) -> String {
    serde_json::to_value(input)
        .ok()
        .and_then(|value| {
            value
                .get("code")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "this operation".to_string())
}

/// Build afpay's flat `Output::Error` shape for a failure this client
/// produced. Deliberately the same un-enveloped `code:"error"` shape
/// [`peer_failure`] produces when it unwraps a peer error, because every
/// consumer of `outputs: Vec<Value>` — [`RemoteProvider::map_remote_error`],
/// [`emit_remote_outputs`], and [`crate::output_fmt::render_value_with_policy`]
/// — keys off exactly that.
fn peer_error(error_code: &str, error: String, hint: Option<&str>, retryable: bool) -> Value {
    let mut value = json!({
        "code": "error",
        "error_code": error_code,
        "error": error,
        "retryable": retryable,
    });
    if let Some(hint) = hint {
        value["hint"] = json!(hint);
    }
    value
}

/// Validate the `--peer-url` / `--peer-api-key-secret` pair, or print the
/// error and exit.
pub fn require_peer_args<'a>(
    peer_url: Option<&'a str>,
    api_key_secret: Option<&'a str>,
    format: OutputFormat,
) -> (&'a str, &'a str) {
    let url = match peer_url {
        Some(url) if !url.is_empty() => url,
        _ => {
            let value: Value = agent_first_data::build_cli_error(
                "--peer-url is required",
                Some("pass the HTTP API URL of the afpay peer, e.g. http://host:9401"),
            )
            .into();
            let _ = crate::output_fmt::emit_process_event(value, format);
            std::process::exit(1);
        }
    };
    let secret = match api_key_secret {
        Some(secret) if !secret.is_empty() => secret,
        _ => {
            let value: Value = agent_first_data::build_cli_error(
                "--peer-api-key-secret is required with --peer-url",
                Some("must match the peer's --rest-api-key-secret"),
            )
            .into();
            let _ = crate::output_fmt::emit_process_event(value, format);
            std::process::exit(1);
        }
    };
    (url, secret)
}

/// Render peer outputs, filtering log events. Returns true if any output was
/// an error.
pub fn emit_remote_outputs(
    outputs: &[Value],
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

/// When a client connects via `--peer-url`, wrap the peer's LimitStatus
/// response so the connected node appears as a node in the topology.
/// Also stamps `origin` on limit_exceeded errors that lack one.
pub fn wrap_remote_limit_topology(outputs: &mut [Value], peer_url: &str) {
    for value in outputs.iter_mut() {
        let code = value.get("code").and_then(|v| v.as_str()).unwrap_or("");
        match code {
            "limit_status" => {
                // Extract the peer's limits + downstream, wrap as a downstream node
                let limits = value.get("limits").cloned().unwrap_or(Value::Array(vec![]));
                let downstream = value
                    .get("downstream")
                    .cloned()
                    .unwrap_or(Value::Array(vec![]));
                let node = json!({
                    "name": peer_url,
                    "endpoint": peer_url,
                    "limits": limits,
                    "downstream": downstream,
                });
                value["limits"] = Value::Array(vec![]);
                value["downstream"] = Value::Array(vec![node]);
            }
            "limit_exceeded"
                if value.get("origin").is_none() || value.get("origin") == Some(&Value::Null) =>
            {
                // If no origin, stamp the peer so the client knows which node rejected
                value["origin"] = Value::String(peer_url.to_string());
            }
            _ => {}
        }
    }
}

// ═══════════════════════════════════════════
// RemoteProvider — PayProvider over the peer's HTTP API
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

/// The peer's `pay_planned` output, which is both this client's fee quote and
/// its authorisation to pay: `plan_id` is what the confirm submits.
#[derive(Deserialize)]
struct PayPlannedOut {
    plan_id: String,
    wallet: String,
    amount_native: u64,
    fee_estimate_native: u64,
    fee_unit: String,
    #[serde(default)]
    spend_debits: Vec<SpendDebit>,
    #[serde(default)]
    warnings: Vec<PlanWarning>,
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
    peer_url: String,
    api_key_secret: String,
    network: Network,
}

impl RemoteProvider {
    pub fn new(peer_url: &str, api_key_secret: &str, network: Network) -> Self {
        Self {
            peer_url: peer_url.to_string(),
            api_key_secret: api_key_secret.to_string(),
            network,
        }
    }

    async fn call(&self, input: &Input) -> Vec<Value> {
        peer_call(&self.peer_url, &self.api_key_secret, input).await
    }

    /// Extract a structured `LimitExceeded` from a peer response, or report
    /// `RemoteProtocolError` if any required field is missing or has the wrong
    /// type. This refuses to fabricate a partial LimitExceeded with zeros and
    /// empty strings — silently dropping fields had previously let bad upstream
    /// JSON look like a legitimate limit hit, which then surprised callers.
    fn parse_limit_exceeded(&self, value: &Value) -> PayError {
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
                    .unwrap_or_else(|| self.peer_url.clone()),
            ),
            hint: value
                .get("hint")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        }
    }

    fn protocol_error(&self, detail: String) -> PayError {
        PayError::RemoteProtocolError {
            endpoint: self.peer_url.clone(),
            detail,
            hint: Some(
                "the afpay peer returned a malformed limit_exceeded payload; verify it is running a compatible afpay version"
                    .to_string(),
            ),
        }
    }

    fn map_remote_error(&self, value: &Value) -> Option<PayError> {
        let code = value
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        match code {
            "error" => {
                // For non-LimitExceeded errors the peer's `error` string is
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
                    "invalid_amount" | "invalid_request" => {
                        PayError::invalid_amount(msg.to_string())
                    }
                    "not_implemented" => PayError::not_implemented(msg.to_string()),
                    "forbidden" => PayError::Forbidden {
                        message: msg.to_string(),
                        hint: value
                            .get("hint")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                    },
                    "limit_exceeded" => self.parse_limit_exceeded(value),
                    // A peer that is not afpay, is a different afpay, or
                    // rejected the credential is a configuration fault, not a
                    // transient network blip: report it under its own code so
                    // the message is not flattened into "network error".
                    "peer_not_afpay"
                    | "peer_route_unsupported"
                    | "peer_version_mismatch"
                    | "peer_unauthorized" => PayError::PeerMismatch {
                        peer: self.peer_url.clone(),
                        detail: msg.to_string(),
                        hint: value
                            .get("hint")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                    },
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
        outputs: Vec<Value>,
        expected_codes: &[&str],
    ) -> Result<Value, PayError> {
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
                "unexpected peer output code '{code}'"
            )));
        }
        Err(PayError::network_error(
            "empty or log-only response from the afpay peer".to_string(),
        ))
    }

    fn parse_output<T: DeserializeOwned>(&self, value: Value, label: &str) -> Result<T, PayError> {
        serde_json::from_value(value).map_err(|e| PayError::RemoteProtocolError {
            endpoint: self.peer_url.clone(),
            detail: format!("{label}: {e}"),
            hint: Some(
                "the afpay peer answered in a shape this build does not know; \
                 read <peer-url>/health and match the two versions"
                    .to_string(),
            ),
        })
    }

    fn balance_from_output(&self, value: Value, wallet: &str) -> Result<BalanceInfo, PayError> {
        let parsed: WalletBalancesOut = self.parse_output(value, "wallet_balances")?;
        let mut wallets = parsed.wallets;
        let item = wallets
            .iter()
            .position(|item| item.wallet.id == wallet)
            .map(|idx| wallets.remove(idx))
            .or_else(|| {
                // A single-wallet balance query answers with a one-item list;
                // use it even when the id is not echoed back.
                (wallets.len() == 1).then(|| wallets.remove(0))
            });
        let Some(item) = item else {
            return Err(PayError::wallet_not_found(format!(
                "wallet {wallet} not found in the peer's balance response"
            )));
        };
        item.balance.ok_or_else(|| {
            PayError::network_error(
                item.error
                    .unwrap_or_else(|| "peer balance response has no balance".to_string()),
            )
        })
    }

    /// Ask the peer to resolve a payment, without paying for it.
    ///
    /// This is the plan half of §9 run one hop out: the peer resolves against
    /// its own wallets, records the plan in its own workspace, and returns an
    /// id that only its confirm route accepts. Nothing on either node has
    /// moved when this returns.
    async fn plan_on_peer(&self, input: &Input) -> Result<PayPlannedOut, PayError> {
        let out = self.first_output(self.call(input).await, &["pay_planned"])?;
        self.parse_output(out, "pay_planned")
    }

    /// The plan request for a send, in the one place both the quote and the
    /// confirm read it from.
    fn send_plan_input(
        &self,
        wallet: &str,
        to: &str,
        onchain_memo: Option<&str>,
        mints: Option<&[String]>,
    ) -> Input {
        Input::SendPlan {
            id: self.gen_id(),
            wallet: (!wallet.is_empty()).then(|| wallet.to_string()),
            network: Some(self.network),
            to: to.to_string(),
            amount: None,
            onchain_memo: onchain_memo.map(|s| s.to_string()),
            local_memo: None,
            mints: mints.map(|m| m.to_vec()),
            chain_id: None,
        }
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

    /// Identity check, not a handshake: one unauthenticated `GET /health`
    /// that refuses loudly when the answer is not this exact afpay.
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
                    .unwrap_or("(absent)");
                let local = crate::config::VERSION;
                if remote_version != local {
                    return Err(PayError::PeerMismatch {
                        peer: self.peer_url.clone(),
                        detail: format!(
                            "afpay version mismatch: this node is v{local}, the peer is v{remote_version}"
                        ),
                        hint: Some(
                            "run the same afpay version on both nodes; federation has no \
                             cross-version compatibility layer"
                                .to_string(),
                        ),
                    });
                }
                let remote_protocol = value.get("protocol_version").and_then(|v| v.as_u64());
                let local_protocol = u64::from(JSON_PROTOCOL_VERSION);
                if remote_protocol != Some(local_protocol) {
                    return Err(PayError::PeerMismatch {
                        peer: self.peer_url.clone(),
                        detail: format!(
                            "afpay protocol mismatch: this node speaks v{local_protocol}, the peer reports {}",
                            remote_protocol
                                .map(|value| format!("v{value}"))
                                .unwrap_or_else(|| "no protocol version".to_string())
                        ),
                        hint: Some("run the same afpay version on both nodes".to_string()),
                    });
                }
            }
        }
        Ok(())
    }

    async fn create_wallet(&self, request: &WalletCreateRequest) -> Result<WalletInfo, PayError> {
        let out = self.first_output(
            self.call(&Input::WalletCreate {
                idempotency_key: None,
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
                idempotency_key: None,
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
            &["wallet_balances"],
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
            &["wallet_balances"],
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
            &["wallet_balances"],
        )?;
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
                idempotency_key: None,
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

    /// Resolve the mint on the peer. This opens a plan there and hands its id
    /// back on the quote, so the local confirm submits the plan this call
    /// reviewed rather than opening a second one.
    async fn cashu_send_quote(
        &self,
        wallet: &str,
        amount: &Amount,
    ) -> Result<CashuSendQuoteInfo, PayError> {
        let planned = self
            .plan_on_peer(&Input::CashuSendPlan {
                id: self.gen_id(),
                wallet: (!wallet.is_empty()).then(|| wallet.to_string()),
                amount: amount.clone(),
                onchain_memo: None,
                local_memo: None,
                mints: None,
            })
            .await?;
        Ok(CashuSendQuoteInfo {
            wallet: planned.wallet,
            amount_native: planned.amount_native,
            fee_native: planned.fee_estimate_native,
            fee_unit: planned.fee_unit,
            warnings: planned.warnings,
            upstream_plan_id: Some(planned.plan_id),
        })
    }

    async fn cashu_send(
        &self,
        wallet: &str,
        amount: Amount,
        onchain_memo: Option<&str>,
        mints: Option<&[String]>,
    ) -> Result<CashuSendResult, PayError> {
        self.cashu_send_confirmed(wallet, amount, onchain_memo, mints, None)
            .await
    }

    async fn cashu_send_confirmed(
        &self,
        wallet: &str,
        amount: Amount,
        onchain_memo: Option<&str>,
        mints: Option<&[String]>,
        upstream_plan_id: Option<&str>,
    ) -> Result<CashuSendResult, PayError> {
        let plan_id = match upstream_plan_id {
            Some(plan_id) => plan_id.to_string(),
            None => {
                self.plan_on_peer(&Input::CashuSendPlan {
                    id: self.gen_id(),
                    wallet: (!wallet.is_empty()).then(|| wallet.to_string()),
                    amount: amount.clone(),
                    onchain_memo: onchain_memo.map(|s| s.to_string()),
                    local_memo: None,
                    mints: mints.map(|m| m.to_vec()),
                })
                .await?
                .plan_id
            }
        };
        let out = self.first_output(
            self.call(&Input::PayConfirm {
                id: self.gen_id(),
                plan_id,
                expect: Some(PayPlanOperation::CashuSend),
                // The federation hop is the upstream node's view; idempotency
                // is enforced at the agent-facing boundary, not on
                // inter-daemon proxy calls. The request id travels as the
                // peer's Idempotency-Key so the peer still refuses a
                // duplicate delivery of this same attempt.
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

    /// Resolve the payment on the peer, which is the only node that can price
    /// it. The peer records a plan; its id rides back on the quote so the
    /// local confirm submits that same plan.
    async fn send_quote(
        &self,
        wallet: &str,
        to: &str,
        mints: Option<&[String]>,
    ) -> Result<SendQuoteInfo, PayError> {
        let planned = self
            .plan_on_peer(&self.send_plan_input(wallet, to, None, mints))
            .await?;
        Ok(SendQuoteInfo {
            wallet: planned.wallet,
            amount_native: planned.amount_native,
            fee_estimate_native: planned.fee_estimate_native,
            fee_unit: planned.fee_unit,
            spend_debits: planned.spend_debits,
            warnings: planned.warnings,
            upstream_plan_id: Some(planned.plan_id),
        })
    }

    async fn send(
        &self,
        wallet: &str,
        to: &str,
        onchain_memo: Option<&str>,
        mints: Option<&[String]>,
    ) -> Result<SendResult, PayError> {
        self.send_confirmed(wallet, to, onchain_memo, mints, None)
            .await
    }

    async fn send_confirmed(
        &self,
        wallet: &str,
        to: &str,
        onchain_memo: Option<&str>,
        mints: Option<&[String]>,
        upstream_plan_id: Option<&str>,
    ) -> Result<SendResult, PayError> {
        let plan_id = match upstream_plan_id {
            Some(plan_id) => plan_id.to_string(),
            None => {
                self.plan_on_peer(&self.send_plan_input(wallet, to, onchain_memo, mints))
                    .await?
                    .plan_id
            }
        };
        let out = self.first_output(
            self.call(&Input::PayConfirm {
                id: self.gen_id(),
                plan_id,
                expect: Some(PayPlanOperation::Send),
                // See cashu_send_confirmed above — idempotency is enforced at
                // the agent boundary.
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

    /// `restore` rebuilds wallet state from the wallet's key material, which
    /// is exactly what the machine face withholds: `Input::Restore` is
    /// `is_local_only`, so it has no route on a peer — and the gRPC daemon it
    /// replaces refused the same call with `permission_denied`. Nothing is
    /// lost here; the refusal just moved to the client, before any bytes go
    /// out. Run `afpay <network> wallet restore` on the node that holds the
    /// wallet.
    async fn restore(&self, wallet: &str) -> Result<RestoreResult, PayError> {
        Err(PayError::Forbidden {
            message: format!(
                "wallet {wallet} lives on the afpay peer at {}; restore runs only on the node that holds its key material",
                self.peer_url
            ),
            hint: Some(
                "run `afpay <network> wallet restore --wallet <id>` on the peer's host".to_string(),
            ),
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

    fn provider() -> RemoteProvider {
        RemoteProvider::new("http://127.0.0.1:1", "secret", Network::Cashu)
    }

    #[test]
    fn first_output_skips_log_events() {
        let out = provider()
            .first_output(
                vec![
                    json!({"code": "log", "event": "startup"}),
                    json!({"code": "wallet_list", "wallets": []}),
                ],
                &["wallet_list"],
            )
            .expect("wallet_list output");
        assert_eq!(out["code"], "wallet_list");
    }

    #[test]
    fn first_output_maps_error() {
        let err = provider()
            .first_output(
                vec![
                    json!({"code": "log", "event": "wallet"}),
                    json!({"code": "error", "error_code": "wallet_not_found", "error": "missing"}),
                ],
                &["wallet_list"],
            )
            .expect_err("error output should be mapped");
        assert!(matches!(err, PayError::WalletNotFound { .. }));
    }

    #[test]
    fn first_output_maps_limit_exceeded() {
        let err = provider()
            .first_output(
                vec![json!({
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
        let err = provider()
            .first_output(
                vec![json!({
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
    fn balance_parses_the_wallet_balances_schema() {
        let balance = provider()
            .balance_from_output(
                json!({
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

    /// The whole point of collapsing onto one machine face: federation asks
    /// the peer for exactly what an agent holding the same token could ask
    /// for, and nothing else. The route table and `is_local_only` must agree
    /// exactly — a local-only input is refused in-process with no request
    /// made, and everything else has a published route.
    #[test]
    fn the_peer_route_table_agrees_with_is_local_only() {
        let inputs = [
            Input::Restore {
                id: "t".into(),
                wallet: "w_1".into(),
            },
            Input::WalletShowSeed {
                id: "t".into(),
                wallet: "w_1".into(),
            },
            Input::LimitRemove {
                id: "t".into(),
                rule_id: "r_1".into(),
            },
            Input::LimitList { id: "t".into() },
            Input::ConfigGet {
                id: "t".into(),
                key: None,
            },
            Input::WalletConfigSet {
                id: "t".into(),
                wallet: "w_1".into(),
                label: None,
                rpc_endpoints: Vec::new(),
                chain_id: None,
            },
            Input::ReconcileReservation {
                id: "t".into(),
                reservation_id: 1,
                action: ReconcileAction::Cancel,
                reason: "test".into(),
            },
            Input::WalletClose {
                id: "t".into(),
                wallet: "w_1".into(),
                dangerously_skip_balance_check_and_may_lose_money: true,
            },
        ];
        for input in inputs {
            let local_only = input.is_local_only();
            let routed = route_for(&input);
            assert_eq!(
                local_only,
                routed.is_err(),
                "is_local_only and the peer route table disagree about {}",
                input_label(&input)
            );
            if let Err(refusal) = routed {
                assert_eq!(refusal["code"], "error");
                assert_eq!(refusal["error_code"], "forbidden");
            }
        }
    }

    #[test]
    fn base_url_accepts_bare_host_port_and_full_urls() {
        assert_eq!(base_url("127.0.0.1:9401"), "http://127.0.0.1:9401");
        assert_eq!(base_url("http://host:9401/"), "http://host:9401");
        assert_eq!(base_url("https://pay.example"), "https://pay.example");
    }

    #[test]
    fn path_segments_cannot_climb_out_of_their_slot() {
        assert_eq!(path_segment("w_ab-12.x~y"), "w_ab-12.x~y");
        assert_eq!(path_segment("../config"), "..%2Fconfig");
        assert_eq!(path_segment("a b"), "a%20b");
    }

    /// A peer that answers with anything but afpay's envelope must be named
    /// as such — never parsed hopefully into a domain answer.
    #[test]
    fn a_non_afpay_answer_is_reported_as_a_mismatch_not_a_result() {
        let plan = RoutePlan::get("/v1/wallets", "wallet_list");
        let refusal = interpret(
            json!({"data": {"wallets": []}}),
            &plan,
            "http://peer:9401",
            StatusCode::OK,
            "application/json",
            br#"{"data":{"wallets":[]}}"#,
        )
        .expect_err("an envelope-less body is not a result");
        assert_eq!(refusal["error_code"], "peer_not_afpay");
        let message = refusal["error"].as_str().unwrap();
        assert!(message.contains("GET /v1/wallets"), "{message}");
        assert!(message.contains("http://peer:9401"), "{message}");
    }

    #[test]
    fn a_missing_route_names_the_version_mismatch() {
        let plan = RoutePlan::get("/v1/spend-limits", "limit_status");
        let value = interpret(
            json!({
                "kind": "error",
                "error": {"code": "api_route_not_found", "message": "API route not found"},
                "trace": {"duration_ms": 0},
            }),
            &plan,
            "http://peer:9401",
            StatusCode::NOT_FOUND,
            "application/json",
            b"{}",
        )
        .expect("an afpay error envelope is a legible answer");
        assert_eq!(value["error_code"], "peer_route_unsupported");
        assert!(
            value["error"]
                .as_str()
                .unwrap()
                .contains("GET /v1/spend-limits")
        );
        assert!(matches!(
            provider().map_remote_error(&value),
            Some(PayError::PeerMismatch { .. })
        ));
    }

    /// `limit_exceeded` travels as an HTTP error but is an *output* everywhere
    /// else in afpay; it has to arrive back as a tagged output or the typed
    /// parser above never sees it.
    #[test]
    fn limit_exceeded_comes_back_as_an_output_not_an_error() {
        let plan = RoutePlan::post("/v1/sends", json!({}), "sent");
        let value = interpret(
            json!({
                "kind": "error",
                "error": {
                    "code": "limit_exceeded",
                    "message": "a spend limit refused this payment",
                    "details": {
                        "rule_id": "r_abc123",
                        "scope": "network",
                        "scope_key": "cashu",
                        "spent": 1500,
                        "max_spend": 1000,
                        "remaining_s": 42,
                    },
                },
                "trace": {"duration_ms": 3},
            }),
            &plan,
            "http://peer:9401",
            StatusCode::UNPROCESSABLE_ENTITY,
            "application/json",
            b"{}",
        )
        .expect("limit_exceeded is a domain answer");
        assert_eq!(value["code"], "limit_exceeded");
        assert!(matches!(
            provider().map_remote_error(&value),
            Some(PayError::LimitExceeded { .. })
        ));
    }

    #[test]
    fn a_result_is_retagged_with_the_code_the_route_promises() {
        let plan = RoutePlan::get("/v1/wallets", "wallet_list");
        let value = interpret(
            json!({
                "kind": "result",
                "result": {"wallets": []},
                "trace": {"duration_ms": 1},
            }),
            &plan,
            "http://peer:9401",
            StatusCode::OK,
            "application/json",
            b"{}",
        )
        .expect("a result envelope");
        assert_eq!(value["code"], "wallet_list");
        assert_eq!(value["trace"]["duration_ms"], 1);
    }

    /// Only the confirm carries a key, because only the confirm can pay twice.
    /// Resolving a plan moves nothing, so a key there would be a header the
    /// peer has no outcome to replay.
    #[test]
    fn confirms_always_carry_an_idempotency_key_and_plans_never_do() {
        let planned = route_for(&Input::SendPlan {
            id: "req_0123456789abcdef".into(),
            wallet: None,
            network: None,
            to: "bc1qexample".into(),
            amount: None,
            onchain_memo: None,
            local_memo: None,
            mints: None,
            chain_id: None,
        })
        .expect("a plan is routable");
        assert_eq!(planned.path, "/v1/send-plans");
        assert_eq!(planned.idempotency_key, None);

        let confirm = route_for(&Input::PayConfirm {
            id: "req_0123456789abcdef".into(),
            plan_id: "plan_abc".into(),
            expect: Some(PayPlanOperation::Send),
            idempotency_key: None,
        })
        .expect("a confirm is routable");
        assert_eq!(confirm.path, "/v1/sends");
        assert_eq!(
            confirm.idempotency_key.as_deref(),
            Some("req_0123456789abcdef"),
            "the peer refuses a payment without one, so the request id stands in"
        );

        let confirm = route_for(&Input::PayConfirm {
            id: "req_0123456789abcdef".into(),
            plan_id: "plan_abc".into(),
            expect: Some(PayPlanOperation::CashuSend),
            idempotency_key: Some("agent-chosen-key".into()),
        })
        .expect("a confirm is routable");
        assert_eq!(confirm.path, "/v1/cashu/tokens");
        assert_eq!(confirm.idempotency_key.as_deref(), Some("agent-chosen-key"));
    }
}
