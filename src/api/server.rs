//! The HTTP domain API: resource routes over the same dispatcher the CLI runs.
//!
//! Every `/v1` handler builds an `Input`, hands it to [`handler::dispatch`],
//! and renders what comes back. There is no second execution path: spend
//! limits, the reserve/execute/confirm ledger, wallet locks, and the
//! persistent idempotency store are reached exactly as `afpay send` reaches
//! them. Operations `Input::is_local_only` marks — key material, spend-limit
//! rules, reservation repair, daemon config — have no route here at all, and
//! [`dispatch`] refuses one anyway if a future route ever builds one.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use axum::body::Body;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, Query, State};
use axum::http::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, WWW_AUTHENTICATE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::model::{
    BalanceListQuery, BalanceListResult, CashuRedemptionRequest, CashuRedemptionResult,
    CashuTokenPlanRequest, CashuTokenResult, PayConfirmRequest, PayPlanResult, ReceiveClaimRequest,
    ReceiveClaimedResult, ReceiveCreateRequest, ReceiveResult, SendPlanRequest, SendResult,
    SpendLimitListResult, TransactionListQuery, TransactionListResult, TransactionStatusResult,
    TransactionSyncRequest, TransactionSyncResult, WalletClosedResult, WalletCreateRequest,
    WalletCreatedResult, WalletDetailResult, WalletListQuery, WalletListResult,
};
use super::rate_limit::RateLimiter;
use super::schema::{openapi_document, schema_index, standalone_schemas};
use crate::handler::{self, App};
use crate::output_fmt;
use crate::store;
use crate::types::{Input, Output, Request as PayRequest, RuntimeConfig};

/// Payment bodies are small; nothing legitimate approaches this.
const MAX_BODY_BYTES: usize = 256 * 1024;
/// Mirrors `IDEMPOTENCY_KEY_MAX_LEN` in the spend ledger, which is what
/// actually stores the key.
const IDEMPOTENCY_KEY_MAX: usize = 128;
const IDEMPOTENCY_KEY_MIN: usize = 8;

#[derive(Clone)]
pub struct ApiState {
    inner: Arc<ApiStateInner>,
}

struct ApiStateInner {
    config: RuntimeConfig,
    /// Digest of the configured key. The plaintext is never held here, and
    /// comparison is on digests so it does not leak the credential's length.
    api_key_digest: blake3::Hash,
    log: Vec<String>,
    rate_limiter: Option<RateLimiter>,
}

impl ApiState {
    /// `api_key_secret` is hashed on the way in; this state never holds
    /// the credential itself.
    pub fn new(config: RuntimeConfig, api_key_secret: &str, log: Vec<String>) -> Self {
        let rate_limiter = config.rate_limit.as_ref().map(RateLimiter::new);
        Self {
            inner: Arc::new(ApiStateInner {
                api_key_digest: blake3::hash(api_key_secret.as_bytes()),
                config,
                log,
                rate_limiter,
            }),
        }
    }
}

struct ApiPath<T>(T);

impl<S, T> FromRequestParts<S> for ApiPath<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(value)| Self(value))
            .map_err(path_rejection)
    }
}

pub fn router(state: ApiState) -> Router {
    let public = Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(openapi))
        .route("/schemas/index.json", get(schemas_index))
        .route("/schemas/{schema_file}", get(schema))
        .method_not_allowed_fallback(method_not_allowed);
    let protected = Router::new()
        .route("/v1/wallets", get(list_wallets).post(create_wallet))
        .route("/v1/wallets/{wallet}", get(get_wallet).delete(close_wallet))
        .route("/v1/balances", get(list_balances))
        .route("/v1/receives", post(create_receive))
        .route("/v1/receives/{quote_id}/claim", post(claim_receive))
        .route("/v1/send-plans", post(plan_send))
        .route("/v1/sends", post(create_send))
        .route("/v1/cashu/token-plans", post(plan_cashu_token))
        .route("/v1/cashu/tokens", post(mint_cashu_token))
        .route("/v1/cashu/redemptions", post(redeem_cashu_token))
        .route("/v1/transactions", get(list_transactions))
        .route("/v1/transactions/sync", post(sync_transactions))
        .route("/v1/transactions/{transaction_id}", get(get_transaction))
        .route("/v1/spend-limits", get(list_spend_limits))
        .method_not_allowed_fallback(method_not_allowed)
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer,
        ));

    // No CORS layer, by design: a browser origin that wants this API is
    // authorised by putting it behind the same host, not by a header afpay
    // hands to anyone who asks.
    public
        .merge(protected)
        .fallback(not_found)
        .with_state(state.clone())
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(state, rate_limit))
        .layer(middleware::from_fn(security_headers))
}

// ═══════════════════════════════════════════
// Discovery
// ═══════════════════════════════════════════

async fn health() -> Response {
    result_response(
        StatusCode::OK,
        json!({
            "service": "afpay",
            "version": crate::config::VERSION,
            "protocol_version": crate::types::JSON_PROTOCOL_VERSION,
            "status": "ready",
        }),
        0,
        None,
    )
}

async fn openapi() -> Response {
    raw_json_response(
        openapi_document(),
        "application/vnd.oai.openapi+json;version=3.2",
    )
}

async fn schemas_index() -> Response {
    raw_json_response(schema_index(), "application/json")
}

async fn schema(ApiPath(schema_file): ApiPath<String>) -> Response {
    match standalone_schemas().remove(&schema_file) {
        Some(schema) => raw_json_response(schema, "application/schema+json"),
        None => error_response(
            ApiError::new("schema_not_found", "JSON Schema not found")
                .status(StatusCode::NOT_FOUND),
            0,
            None,
        ),
    }
}

async fn not_found() -> Response {
    error_response(
        ApiError::new("api_route_not_found", "API route not found")
            .status(StatusCode::NOT_FOUND)
            .hint("read GET /openapi.json for the routes this daemon serves"),
        0,
        None,
    )
}

async fn method_not_allowed() -> Response {
    error_response(
        ApiError::new(
            "api_method_not_allowed",
            "HTTP method is not allowed for this route",
        )
        .status(StatusCode::METHOD_NOT_ALLOWED),
        0,
        None,
    )
}

// ═══════════════════════════════════════════
// Wallets
// ═══════════════════════════════════════════

async fn list_wallets(
    State(state): State<ApiState>,
    query: Result<Query<WalletListQuery>, QueryRejection>,
) -> Response {
    let query = match query_value(query) {
        Ok(query) => query,
        Err(response) => return *response,
    };
    dispatch::<WalletListResult>(state, |id| query.into_input(id)).await
}

/// Creating a wallet generates a key. A retry that cannot be recognised
/// creates a second one, and the caller has no way to tell which it got — so
/// this route requires an `Idempotency-Key` and afpay honours it out of the
/// same 24-hour store a payment uses.
async fn create_wallet(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Result<Json<WalletCreateRequest>, JsonRejection>,
) -> Response {
    let key = match idempotency_key(&headers) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    dispatch::<WalletCreatedResult>(state, |id| body.into_input(id, key)).await
}

async fn get_wallet(State(state): State<ApiState>, ApiPath(wallet): ApiPath<String>) -> Response {
    dispatch::<WalletDetailResult>(state, |id| Input::WalletConfigShow { id, wallet }).await
}

async fn close_wallet(State(state): State<ApiState>, ApiPath(wallet): ApiPath<String>) -> Response {
    dispatch::<WalletClosedResult>(state, |id| Input::WalletClose {
        id,
        wallet,
        // The override that skips the balance check is local-only: losing
        // money on purpose is not something a bearer token may ask for.
        dangerously_skip_balance_check_and_may_lose_money: false,
    })
    .await
}

// ═══════════════════════════════════════════
// Balances
// ═══════════════════════════════════════════

async fn list_balances(
    State(state): State<ApiState>,
    query: Result<Query<BalanceListQuery>, QueryRejection>,
) -> Response {
    let query = match query_value(query) {
        Ok(query) => query,
        Err(response) => return *response,
    };
    dispatch::<BalanceListResult>(state, |id| query.into_input(id)).await
}

// ═══════════════════════════════════════════
// Receives
// ═══════════════════════════════════════════

/// A repeat mints a second invoice or quote while a payer may already be
/// holding the first, and the caller then watches the wrong one. The key makes
/// a retry return the receive that was already handed out.
async fn create_receive(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Result<Json<ReceiveCreateRequest>, JsonRejection>,
) -> Response {
    let key = match idempotency_key(&headers) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    dispatch::<ReceiveResult>(state, |id| body.into_input(id, key)).await
}

async fn claim_receive(
    State(state): State<ApiState>,
    ApiPath(quote_id): ApiPath<String>,
    body: Result<Json<ReceiveClaimRequest>, JsonRejection>,
) -> Response {
    let body = match json_body(body) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    dispatch::<ReceiveClaimedResult>(state, |id| body.into_input(id, quote_id)).await
}

// ═══════════════════════════════════════════
// Sends
// ═══════════════════════════════════════════

/// The plan half of a payment: resolve it, record it, return its id.
///
/// No `Idempotency-Key`. Nothing moves here, each call resolves a fresh plan
/// against current network conditions, and there is no outcome for a repeat to
/// converge on — a key afpay could not honour would be worse than none.
async fn plan_send(
    State(state): State<ApiState>,
    body: Result<Json<SendPlanRequest>, JsonRejection>,
) -> Response {
    let body = match json_body(body) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    dispatch::<PayPlanResult>(state, |id| body.into_input(id)).await
}

/// The confirm half: submit the reviewed plan.
///
/// This is the only route on this face that moves money out of a wallet, which
/// is why it is the one that requires an `Idempotency-Key`. The body carries
/// the plan id and nothing else — the payment comes off the stored plan.
async fn create_send(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Result<Json<PayConfirmRequest>, JsonRejection>,
) -> Response {
    let key = match idempotency_key(&headers) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    dispatch::<SendResult>(state, |id| {
        body.into_input(id, crate::types::PayPlanOperation::Send, key)
    })
    .await
}

async fn plan_cashu_token(
    State(state): State<ApiState>,
    body: Result<Json<CashuTokenPlanRequest>, JsonRejection>,
) -> Response {
    let body = match json_body(body) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    dispatch::<PayPlanResult>(state, |id| body.into_input(id)).await
}

/// Confirm a reviewed Cashu token plan. A plan resolved for a send is refused
/// here, and vice versa: the id alone never decides what happens.
async fn mint_cashu_token(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Result<Json<PayConfirmRequest>, JsonRejection>,
) -> Response {
    let key = match idempotency_key(&headers) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    let body = match json_body(body) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    dispatch::<CashuTokenResult>(state, |id| {
        body.into_input(id, crate::types::PayPlanOperation::CashuSend, key)
    })
    .await
}

async fn redeem_cashu_token(
    State(state): State<ApiState>,
    body: Result<Json<CashuRedemptionRequest>, JsonRejection>,
) -> Response {
    let body = match json_body(body) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    dispatch::<CashuRedemptionResult>(state, |id| body.into_input(id)).await
}

// ═══════════════════════════════════════════
// Transactions
// ═══════════════════════════════════════════

async fn list_transactions(
    State(state): State<ApiState>,
    query: Result<Query<TransactionListQuery>, QueryRejection>,
) -> Response {
    let query = match query_value(query) {
        Ok(query) => query,
        Err(response) => return *response,
    };
    dispatch::<TransactionListResult>(state, |id| query.into_input(id)).await
}

async fn get_transaction(
    State(state): State<ApiState>,
    ApiPath(transaction_id): ApiPath<String>,
) -> Response {
    dispatch::<TransactionStatusResult>(state, |id| Input::HistoryStatus { id, transaction_id })
        .await
}

async fn sync_transactions(
    State(state): State<ApiState>,
    body: Result<Json<TransactionSyncRequest>, JsonRejection>,
) -> Response {
    let body = match json_body(body) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    dispatch::<TransactionSyncResult>(state, |id| body.into_input(id)).await
}

// ═══════════════════════════════════════════
// Spend limits
// ═══════════════════════════════════════════

async fn list_spend_limits(State(state): State<ApiState>) -> Response {
    dispatch::<SpendLimitListResult>(state, |id| Input::LimitList { id }).await
}

// ═══════════════════════════════════════════
// The one path into the domain
// ═══════════════════════════════════════════

/// Build the request's `Input`, run it through the CLI's dispatcher, and turn
/// the outputs it emitted into one AFDATA envelope.
///
/// `T` is the operation's documented result schema. The payload is read back
/// through it before it ships, so a dispatcher that starts returning a
/// different shape is reported as a contract violation instead of quietly
/// invalidating the committed OpenAPI document.
async fn dispatch<T>(state: ApiState, build: impl FnOnce(String) -> Input) -> Response
where
    T: DeserializeOwned,
{
    let started = Instant::now();
    let request_id = match store::wallet::generate_request_identifier() {
        Ok(id) => id,
        Err(error) => {
            return error_response(
                ApiError::new("internal_error", error.to_string())
                    .status(StatusCode::INTERNAL_SERVER_ERROR),
                elapsed_ms(started),
                None,
            );
        }
    };
    let input = build(request_id.clone());

    // Defence in depth. No route above builds a local-only input; this makes
    // it impossible for one added later to do so by accident.
    if input.is_local_only() {
        return error_response(
            ApiError::new(
                "forbidden",
                "this operation is available only on the machine that holds the data",
            )
            .status(StatusCode::FORBIDDEN)
            .hint("run it through the afpay CLI on the daemon host"),
            elapsed_ms(started),
            Some(&request_id),
        );
    }

    let (tx, mut rx) = mpsc::channel::<Output>(256);
    let store = store::create_storage_backend(&state.inner.config);
    let app = Arc::new(App::new(state.inner.config.clone(), tx, Some(true), store));
    app.requests_total.fetch_add(1, Ordering::Relaxed);
    handler::dispatch(&app, PayRequest::from_input(input)).await;
    drop(app);

    let filters = agent_first_data::LogFilters::new(state.inner.log.clone());
    let mut terminal: Option<Output> = None;
    let mut ledger_broken = false;
    while let Some(output) = rx.recv().await {
        if let Output::Log { ref event, .. } = output {
            // Daemon logs keep going to the daemon's own stream, through the
            // same redacting emitter every other mode uses.
            if filters.enabled(event) {
                let _ =
                    output_fmt::emit_process_output(&output, agent_first_data::OutputFormat::Json);
            }
            continue;
        }
        if ledger_broken {
            continue;
        }
        // `accounting_inconsistent` is terminal. Keep it dominant as defence
        // in depth if a provider accidentally emits anything afterwards.
        if matches!(output, Output::AccountingInconsistent { .. }) {
            ledger_broken = true;
            terminal = Some(output);
        } else if terminal.is_none() {
            terminal = Some(output);
        }
    }
    let duration_ms = elapsed_ms(started);

    let Some(output) = terminal else {
        return error_response(
            ApiError::new(
                "api_contract_violation",
                "the daemon produced no terminal output for this request",
            )
            .status(StatusCode::INTERNAL_SERVER_ERROR),
            duration_ms,
            Some(&request_id),
        );
    };

    // The one serialization seam: `protocol_event` builds the strict AFDATA
    // envelope and runs AFDATA redaction over it.
    let event = match output_fmt::protocol_event(&output) {
        Ok(event) => event,
        Err(error) => {
            return error_response(
                ApiError::new("api_contract_violation", error)
                    .status(StatusCode::INTERNAL_SERVER_ERROR),
                duration_ms,
                Some(&request_id),
            );
        }
    };

    match classify(event) {
        Classified::Error(error) => error_response(error, duration_ms, Some(&request_id)),
        Classified::Result(payload) => {
            if let Err(error) = serde_json::from_value::<T>(payload.clone()) {
                return error_response(
                    ApiError::new(
                        "api_contract_violation",
                        format!("afpay returned a result outside the documented schema: {error}"),
                    )
                    .status(StatusCode::INTERNAL_SERVER_ERROR),
                    duration_ms,
                    Some(&request_id),
                );
            }
            result_response(StatusCode::OK, payload, duration_ms, Some(&request_id))
        }
    }
}

enum Classified {
    Result(Value),
    Error(ApiError),
}

/// Split an AFDATA event into "this succeeded" and "this did not".
///
/// Two of afpay's outputs are refusals the protocol carries as results:
/// `limit_exceeded` (a spend rule said no) and `accounting_inconsistent`
/// (money left but the ledger could not record it). Shipping either as a 200
/// result would report a business outcome that did not happen, so both become
/// error envelopes carrying their payload as typed detail.
fn classify(mut event: Value) -> Classified {
    if event["kind"] == "error" {
        let error = event["error"].take();
        return Classified::Error(ApiError {
            code: string_at(&error, "code").unwrap_or_else(|| "internal_error".to_string()),
            message: string_at(&error, "message")
                .unwrap_or_else(|| "payment command failed".to_string()),
            hint: string_at(&error, "hint"),
            retryable: error["retryable"].as_bool().unwrap_or(false),
            retry_after_ms: error["retry_after_ms"].as_u64(),
            status: None,
            details: None,
        });
    }
    let mut payload = event["result"].take();
    let code = string_at(&payload, "code").unwrap_or_default();
    if let Value::Object(fields) = &mut payload {
        // The union tag and the request correlation id are transport noise
        // here: the route already fixes the shape, and the correlation id
        // travels in the `x-request-id` header.
        fields.remove("code");
        fields.remove("id");
    }
    match code.as_str() {
        "limit_exceeded" => Classified::Error(
            ApiError::new("limit_exceeded", "a spend limit refused this payment")
                .status(StatusCode::UNPROCESSABLE_ENTITY)
                .hint("read GET /v1/spend-limits for the rule and its window, or wait for the window to reset")
                .details(payload),
        ),
        "accounting_inconsistent" => Classified::Error(
            ApiError::new(
                "accounting_inconsistent",
                "the payment reached the network but the spend ledger could not record it",
            )
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .hint("do not retry: the money already moved. Reconcile the named reservations on the daemon host before sending again")
            .details(payload),
        ),
        _ => Classified::Result(payload),
    }
}

fn string_at(value: &Value, key: &str) -> Option<String> {
    value[key].as_str().map(str::to_string)
}

// ═══════════════════════════════════════════
// Errors
// ═══════════════════════════════════════════

struct ApiError {
    code: String,
    message: String,
    hint: Option<String>,
    retryable: bool,
    retry_after_ms: Option<u64>,
    status: Option<StatusCode>,
    details: Option<Value>,
}

impl ApiError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            hint: None,
            retryable: false,
            retry_after_ms: None,
            status: None,
            details: None,
        }
    }

    fn status(mut self, status: StatusCode) -> Self {
        self.status = Some(status);
        self
    }

    fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    fn details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    /// The status a code means when the failure did not name one itself.
    /// Both halves are load-bearing: the code says what happened, the status
    /// says which class of caller mistake or daemon state it belongs to.
    fn resolved_status(&self) -> StatusCode {
        if let Some(status) = self.status {
            return status;
        }
        match self.code.as_str() {
            "wallet_not_found" => StatusCode::NOT_FOUND,
            "invalid_request" | "invalid_amount" => StatusCode::BAD_REQUEST,
            "forbidden" | "configure_on_daemon" => StatusCode::FORBIDDEN,
            "busy" | "idempotency_conflict" | "idempotency_in_progress" => StatusCode::CONFLICT,
            "network_error" => StatusCode::SERVICE_UNAVAILABLE,
            "internal_error" | "remote_protocol_error" => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::UNPROCESSABLE_ENTITY,
        }
    }
}

fn error_response(error: ApiError, duration_ms: u64, request_id: Option<&str>) -> Response {
    let status = error.resolved_status();
    let mut body = json!({
        "code": error.code,
        "message": error.message,
        "retryable": error.retryable,
    });
    if let Some(hint) = &error.hint {
        body["hint"] = json!(hint);
    }
    if let Some(retry_after_ms) = error.retry_after_ms {
        body["retry_after_ms"] = json!(retry_after_ms);
    }
    if let Some(details) = error.details {
        body["details"] = details;
    }
    let event = json!({
        "kind": "error",
        "error": body,
        "trace": {"duration_ms": duration_ms},
    });
    let mut response = json_response(status, output_fmt::redacted_value(&event), request_id);
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"afpay\""),
        );
    }
    response
}

fn result_response(
    status: StatusCode,
    payload: Value,
    duration_ms: u64,
    request_id: Option<&str>,
) -> Response {
    let event = json!({
        "kind": "result",
        "result": payload,
        "trace": {"duration_ms": duration_ms},
    });
    json_response(status, output_fmt::redacted_value(&event), request_id)
}

fn raw_json_response(value: Value, media_type: &'static str) -> Response {
    let mut response = (StatusCode::OK, Json(output_fmt::redacted_value(&value))).into_response();
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(media_type));
    response
}

fn json_response(status: StatusCode, value: Value, request_id: Option<&str>) -> Response {
    let mut response = (status, Json(value)).into_response();
    if let Some(request_id) = request_id
        && let Ok(value) = HeaderValue::from_str(request_id)
    {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}

// ═══════════════════════════════════════════
// Extraction
// ═══════════════════════════════════════════

fn json_body<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, Box<Response>> {
    body.map(|Json(body)| body).map_err(|rejection| {
        let (code, status) = match rejection.status() {
            StatusCode::PAYLOAD_TOO_LARGE => ("payload_too_large", StatusCode::PAYLOAD_TOO_LARGE),
            StatusCode::UNSUPPORTED_MEDIA_TYPE => {
                ("unsupported_media_type", StatusCode::UNSUPPORTED_MEDIA_TYPE)
            }
            _ => ("invalid_request", StatusCode::BAD_REQUEST),
        };
        Box::new(error_response(
            ApiError::new(code, format!("invalid JSON request body: {rejection}"))
                .status(status)
                .hint("the request body schema is at GET /openapi.json"),
            0,
            None,
        ))
    })
}

fn query_value<T>(query: Result<Query<T>, QueryRejection>) -> Result<T, Box<Response>> {
    query.map(|Query(query)| query).map_err(|rejection| {
        Box::new(error_response(
            ApiError::new(
                "invalid_request",
                format!("invalid query parameters: {rejection}"),
            )
            .status(StatusCode::BAD_REQUEST),
            0,
            None,
        ))
    })
}

fn path_rejection(rejection: PathRejection) -> Response {
    error_response(
        ApiError::new(
            "invalid_request",
            format!("invalid path parameters: {rejection}"),
        )
        .status(StatusCode::BAD_REQUEST),
        0,
        None,
    )
}

/// The header afpay hands straight to the input's own `idempotency_key`, i.e.
/// the same key the CLI's `--idempotency-key` writes, stored in the same
/// ledger with the same 24-hour replay window. Every route that declares it
/// honours it; no route requires a key afpay would then ignore.
fn idempotency_key(headers: &HeaderMap) -> Result<String, Box<Response>> {
    let reject = |message: &str| {
        Box::new(error_response(
            ApiError::new("idempotency_key_required", message)
                .status(StatusCode::BAD_REQUEST)
                .hint("generate one stable key per attempt and resend it verbatim on retry"),
            0,
            None,
        ))
    };
    let value = headers
        .get("idempotency-key")
        .ok_or_else(|| reject("Idempotency-Key header is required for a payment"))?
        .to_str()
        .map_err(|_| reject("Idempotency-Key must be ASCII"))?;
    let mut characters = value.chars();
    let valid_first = characters
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric());
    let valid_rest = characters
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-'));
    if !(IDEMPOTENCY_KEY_MIN..=IDEMPOTENCY_KEY_MAX).contains(&value.len())
        || !valid_first
        || !valid_rest
    {
        return Err(reject(
            "Idempotency-Key must contain 8 through 128 ASCII letters, digits, dots, underscores, or hyphens and start with a letter or digit",
        ));
    }
    Ok(value.to_string())
}

// ═══════════════════════════════════════════
// Middleware
// ═══════════════════════════════════════════

async fn require_bearer(
    State(state): State<ApiState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if query_may_contain_credential(request.uri().query()) {
        return error_response(
            ApiError::new(
                "credential_location_invalid",
                "credentials are accepted only in the Authorization header",
            )
            .status(StatusCode::BAD_REQUEST),
            0,
            None,
        );
    }
    let authorized = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(bearer_token)
        .is_some_and(|token| blake3::hash(token.as_bytes()) == state.inner.api_key_digest);
    if !authorized {
        return error_response(
            ApiError::new(
                "authentication_required",
                "missing or invalid bearer credential",
            )
            .status(StatusCode::UNAUTHORIZED),
            0,
            None,
        );
    }
    next.run(request).await
}

fn bearer_token(value: &str) -> Option<&str> {
    let mut parts = value.split_ascii_whitespace();
    let scheme = parts.next()?;
    let token = parts.next()?;
    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(token)
}

fn query_may_contain_credential(query: Option<&str>) -> bool {
    query.unwrap_or_default().split('&').any(|pair| {
        let key = pair
            .split_once('=')
            .map_or(pair, |(key, _)| key)
            .to_ascii_lowercase();
        matches!(key.as_str(), "token" | "authorization" | "api_key")
            || key.ends_with("_token")
            || key.ends_with("_secret")
            || key.ends_with("_key")
    })
}

async fn rate_limit(State(state): State<ApiState>, request: Request<Body>, next: Next) -> Response {
    let Some(limiter) = &state.inner.rate_limiter else {
        return next.run(request).await;
    };
    let Ok(_permit) = limiter.try_acquire() else {
        return error_response(
            ApiError::new("rate_limited", "rate limit exceeded")
                .status(StatusCode::TOO_MANY_REQUESTS)
                .retryable(),
            0,
            None,
        );
    };
    next.run(request).await
}

async fn security_headers(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
    );
    response
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}
