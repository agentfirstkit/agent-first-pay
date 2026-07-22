use crate::handler::{self, App};
use crate::store;
use crate::types::*;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use tokio::sync::mpsc;

pub struct RestInit {
    pub listen: String,
    pub api_key: Option<String>,
    pub allow_public_listen: bool,
    pub log: Vec<String>,
    pub data_dir: Option<String>,
    pub startup_argv: Vec<String>,
    pub startup_args: serde_json::Value,
    pub startup_requested: bool,
}

struct AppState {
    app: Arc<App>,
    api_key: String,
    log: Vec<String>,
    rate_limiter: Option<RateLimiter>,
}

/// Simple token-bucket rate limiter with concurrent request tracking.
struct RateLimiter {
    /// Requests per second (refill rate).
    rps: u32,
    /// Maximum concurrent in-flight requests.
    max_concurrent: u32,
    /// Current in-flight count.
    in_flight: AtomicU32,
    /// Available tokens (scaled by 1000 for sub-integer precision).
    tokens_milli: AtomicU64,
    /// Last refill timestamp in milliseconds.
    last_refill_ms: AtomicU64,
}

impl RateLimiter {
    fn new(config: &RateLimitConfig) -> Self {
        let rps = config.requests_per_second;
        Self {
            rps,
            max_concurrent: config.max_concurrent,
            in_flight: AtomicU32::new(0),
            tokens_milli: AtomicU64::new(u64::from(rps) * 1000),
            last_refill_ms: AtomicU64::new(now_ms()),
        }
    }

    /// Try to acquire a permit. Returns Err if rate-limited.
    fn try_acquire(&self) -> Result<RateLimitGuard<'_>, ()> {
        // Check concurrent limit
        if self.max_concurrent > 0 {
            let prev = self.in_flight.fetch_add(1, Ordering::Relaxed);
            if prev >= self.max_concurrent {
                self.in_flight.fetch_sub(1, Ordering::Relaxed);
                return Err(());
            }
        }

        // Token bucket check
        if self.rps > 0 {
            self.refill();
            let cost = 1000u64;
            loop {
                let current = self.tokens_milli.load(Ordering::Relaxed);
                if current < cost {
                    if self.max_concurrent > 0 {
                        self.in_flight.fetch_sub(1, Ordering::Relaxed);
                    }
                    return Err(());
                }
                if self
                    .tokens_milli
                    .compare_exchange_weak(
                        current,
                        current - cost,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    break;
                }
            }
        }

        Ok(RateLimitGuard { limiter: self })
    }

    fn refill(&self) {
        let now = now_ms();
        let last = self.last_refill_ms.load(Ordering::Relaxed);
        let elapsed = now.saturating_sub(last);
        if elapsed == 0 {
            return;
        }
        if self
            .last_refill_ms
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            let add = elapsed * u64::from(self.rps); // milli-tokens per ms = rps
            let max = u64::from(self.rps) * 1000;
            let _ =
                self.tokens_milli
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                        Some(current.saturating_add(add).min(max))
                    });
        }
    }
}

struct RateLimitGuard<'a> {
    limiter: &'a RateLimiter,
}

impl Drop for RateLimitGuard<'_> {
    fn drop(&mut self) {
        if self.limiter.max_concurrent > 0 {
            self.limiter.in_flight.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub async fn run_rest(init: RestInit) {
    let api_key: String = match init
        .api_key
        .or_else(|| std::env::var("AFPAY_REST_API_KEY").ok())
    {
        Some(s) if !s.is_empty() => s,
        _ => {
            let value = agent_first_data::build_cli_error(
                "--rest-api-key is required for REST mode",
                Some("pass an API key for bearer authentication or set AFPAY_REST_API_KEY"),
            );
            let rendered = agent_first_data::render(
                value.as_value(),
                agent_first_data::OutputFormat::Json,
                &agent_first_data::OutputOptions::default(),
            );
            let _ = writeln!(std::io::stdout(), "{rendered}");
            std::process::exit(1);
        }
    };

    let resolved_dir = init
        .data_dir
        .unwrap_or_else(|| RuntimeConfig::default().data_dir);
    let mut config = match RuntimeConfig::load_from_dir(&resolved_dir) {
        Ok(c) => c,
        Err(e) => {
            let value = agent_first_data::build_cli_error(&e, None);
            let rendered = agent_first_data::render(
                value.as_value(),
                agent_first_data::OutputFormat::Json,
                &agent_first_data::OutputOptions::default(),
            );
            let _ = writeln!(std::io::stdout(), "{rendered}");
            std::process::exit(1);
        }
    };
    if !init.log.is_empty() {
        config.log = init.log.clone();
    }

    // Emit startup log
    let log_filters = agent_first_data::LogFilters::new(config.log.clone());
    if let Some(startup) = crate::config::maybe_startup_log(
        &log_filters,
        init.startup_requested,
        Some(init.startup_argv),
        Some(&config),
        init.startup_args,
    ) {
        let value = serde_json::to_value(&startup).unwrap_or(serde_json::Value::Null);
        let rendered = agent_first_data::render(
            &value,
            agent_first_data::OutputFormat::Json,
            &agent_first_data::OutputOptions::default(),
        );
        let _ = writeln!(std::io::stdout(), "{rendered}");
    }

    let startup_errors = handler::startup_provider_validation_errors(&config).await;
    for error_output in &startup_errors {
        let value = serde_json::to_value(error_output).unwrap_or(serde_json::Value::Null);
        let rendered = agent_first_data::render(
            &value,
            agent_first_data::OutputFormat::Json,
            &agent_first_data::OutputOptions::default(),
        );
        let _ = writeln!(std::io::stdout(), "{rendered}");
    }
    if !startup_errors.is_empty() {
        std::process::exit(1);
    }

    let rate_limiter = config.rate_limit.as_ref().map(RateLimiter::new);
    let policy = AllowlistPolicy::from_config(&config);
    let (tx, _rx) = mpsc::channel::<Output>(4096);
    let st = store::create_storage_backend(&config);
    let app = Arc::new(App::new(config, tx, Some(true), st));
    let state = Arc::new(AppState {
        app,
        api_key,
        log: init.log,
        rate_limiter,
    });

    let router = axum::Router::new()
        .route("/v1/afpay", post(handle_call))
        .route("/v1/schema", get(handle_schema))
        .with_state(state);

    let addr: std::net::SocketAddr = match init.listen.parse() {
        Ok(a) => a,
        Err(e) => {
            let value = agent_first_data::build_cli_error(
                &format!("invalid --rest-listen address: {e}"),
                Some("expected format: host:port (e.g. 0.0.0.0:9401)"),
            );
            let rendered = agent_first_data::render(
                value.as_value(),
                agent_first_data::OutputFormat::Json,
                &agent_first_data::OutputOptions::default(),
            );
            let _ = writeln!(std::io::stdout(), "{rendered}");
            std::process::exit(1);
        }
    };
    if public_listen_requires_ack(addr) && !init.allow_public_listen {
        let value = agent_first_data::build_cli_error(
            "refusing to bind REST to a non-loopback address without --public-listen",
            Some(
                "use the default 127.0.0.1:9401, or pass --public-listen only behind TLS/firewall",
            ),
        );
        let rendered = agent_first_data::render(
            value.as_value(),
            agent_first_data::OutputFormat::Json,
            &agent_first_data::OutputOptions::default(),
        );
        let _ = writeln!(std::io::stdout(), "{rendered}");
        std::process::exit(1);
    }

    if init.allow_public_listen
        && let Err(msg) = policy.require_for_public_listen()
    {
        let value = agent_first_data::build_cli_error(
            &msg,
            Some(
                "add at least one entry to allowed_mint_urls / allowed_esplora_urls / allowed_sol_rpc_endpoints / allowed_evm_rpc_endpoints in your runtime config before exposing the daemon",
            ),
        );
        let rendered = agent_first_data::render(
            value.as_value(),
            agent_first_data::OutputFormat::Json,
            &agent_first_data::OutputOptions::default(),
        );
        let _ = writeln!(std::io::stdout(), "{rendered}");
        std::process::exit(1);
    }
    let banner = serde_json::json!({"code": "startup", "policy": policy.banner()});
    let rendered = agent_first_data::render(
        &banner,
        agent_first_data::OutputFormat::Json,
        &agent_first_data::OutputOptions::default(),
    );
    let _ = writeln!(std::io::stdout(), "{rendered}");

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            let value = agent_first_data::build_cli_error(&format!("REST bind failed: {e}"), None);
            let rendered = agent_first_data::render(
                value.as_value(),
                agent_first_data::OutputFormat::Json,
                &agent_first_data::OutputOptions::default(),
            );
            let _ = writeln!(std::io::stdout(), "{rendered}");
            std::process::exit(1);
        }
    };

    if let Err(e) = axum::serve(listener, router).await {
        let value = agent_first_data::build_cli_error(&format!("REST server error: {e}"), None);
        let rendered = agent_first_data::render(
            value.as_value(),
            agent_first_data::OutputFormat::Json,
            &agent_first_data::OutputOptions::default(),
        );
        let _ = writeln!(std::io::stdout(), "{rendered}");
        std::process::exit(1);
    }
}

fn public_listen_requires_ack(addr: std::net::SocketAddr) -> bool {
    !addr.ip().is_loopback()
}

fn check_auth(headers: &HeaderMap, expected: &str) -> Result<(), StatusCode> {
    // Try Authorization: Bearer <key>
    if let Some(val) = headers.get("authorization") {
        let val = val.to_str().map_err(|_| StatusCode::UNAUTHORIZED)?;
        if let Some(token) = val.strip_prefix("Bearer ")
            && constant_time_eq(token.as_bytes(), expected.as_bytes())
        {
            return Ok(());
        }
    }
    // Try X-API-Key: <key>
    if let Some(val) = headers.get("x-api-key") {
        let val = val.to_str().map_err(|_| StatusCode::UNAUTHORIZED)?;
        if constant_time_eq(val.as_bytes(), expected.as_bytes()) {
            return Ok(());
        }
    }
    Err(StatusCode::UNAUTHORIZED)
}

/// Constant-time byte comparison to prevent timing attacks on API key.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// GET /v1/schema — return a machine-readable description of the wire protocol
/// so agents can self-discover Input/Output shapes without scraping `--help` or
/// the source tree. No auth required: the schema reveals only structural info
/// (operation codes + required fields), no secrets.
///
/// This is hand-written rather than derived from the Rust types so adding a
/// schemars dependency does not balloon every nested type (Amount, Network,
/// SpendLimit, …). The trade-off: when an Input variant changes, this file
/// must be updated by hand — the `wire_protocol_schema_listed_inputs_match_protocol_rs`
/// test in src/mode/rest.rs catches drift.
async fn handle_schema() -> impl IntoResponse {
    Json(crate::handler::schema::wire_protocol_schema())
}

fn http_error(code: &str, message: &str, hint: Option<&str>, retryable: bool) -> serde_json::Value {
    agent_first_data::json_error(code, message)
        .hint_if_some(hint)
        .retryable_if(retryable)
        .build()
        .map(Into::into)
        .unwrap_or_else(|_| serde_json::json!({}))
}

async fn handle_call(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    // Rate limit check
    let _rate_guard = if let Some(rl) = &state.rate_limiter {
        match rl.try_acquire() {
            Ok(guard) => Some(guard),
            Err(()) => {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(http_error(
                        "rate_limited",
                        "rate limit exceeded",
                        None,
                        true,
                    )),
                );
            }
        }
    } else {
        None
    };

    // Auth check
    if let Err(status) = check_auth(&headers, &state.api_key) {
        return (
            status,
            Json(http_error("unauthorized", "unauthorized", None, false)),
        );
    }

    // Parse Request from body. Plain Input JSON (without `dry_run`) still
    // deserializes via Request's #[serde(default)] flatten layout.
    let request: Request = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(http_error(
                    "invalid_request",
                    "invalid input",
                    Some(
                        "request must match the Input/Request schema; GET /v1/schema for the full spec",
                    ),
                    false,
                )),
            );
        }
    };

    // Block local-only operations
    if request.input.is_local_only() {
        return (
            StatusCode::FORBIDDEN,
            Json(http_error(
                "forbidden",
                "local-only operation not allowed over REST",
                None,
                false,
            )),
        );
    }

    // Create per-request channel and App
    let (tx, mut rx) = mpsc::channel::<Output>(256);
    let config = state.app.config.read().await.clone();
    let st = store::create_storage_backend(&config);
    let app = Arc::new(App::new(config, tx, Some(true), st));
    app.requests_total.fetch_add(1, Ordering::Relaxed);

    // Dispatch
    handler::dispatch(&app, request).await;

    // Collect outputs
    drop(app);
    let log_filters = agent_first_data::LogFilters::new(state.log.clone());
    let mut outputs = Vec::new();
    while let Some(out) = rx.recv().await {
        // Mirror log events to daemon stdout
        if let Output::Log { ref event, .. } = out
            && log_filters.enabled(event)
        {
            crate::mode::cli::emit_output(&out, agent_first_data::OutputFormat::Json);
        }
        match crate::output_fmt::protocol_event(&out) {
            Ok(value) => outputs.push(value),
            Err(error) => outputs.push(http_error("serialization_failed", &error, None, false)),
        }
    }

    // Check if any output is an error
    let has_error = outputs
        .iter()
        .any(|item| item.get("kind").and_then(|v| v.as_str()) == Some("error"));

    let status = if has_error {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };

    (status, Json(serde_json::Value::Array(outputs)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod schema_tests {
    use crate::handler::schema::wire_protocol_schema;

    /// Every Input variant that goes over the wire must appear in the schema.
    /// If you add a new Input variant and forget to update wire_protocol_schema,
    /// this test fails — fix it by adding the new code (with fields and
    /// description) to the inputs[] array. Codes here are the serde-rename
    /// strings from src/types/protocol.rs.
    const EXPECTED_INPUT_CODES: &[&str] = &[
        "version",
        "schema",
        "config_get",
        "config_set",
        "wallet_create",
        "ln_wallet_create",
        "wallet_close",
        "wallet_list",
        "balance",
        "receive",
        "receive_claim",
        "cashu_send",
        "cashu_receive",
        "send",
        "restore",
        "history",
        "history_status",
        "history_update",
        "limit_add",
        "limit_remove",
        "limit_list",
        "limit_set",
        "reconcile_reservation",
        "wallet_config_show",
        "wallet_config_set",
        "wallet_config_token_add",
        "wallet_config_token_remove",
        "close",
    ];

    #[test]
    fn schema_includes_every_expected_input_code() {
        let schema = wire_protocol_schema();
        let inputs = schema.get("inputs").and_then(|v| v.as_array()).unwrap();
        let listed: std::collections::HashSet<&str> = inputs
            .iter()
            .filter_map(|entry| entry.get("code").and_then(|v| v.as_str()))
            .collect();
        for expected in EXPECTED_INPUT_CODES {
            assert!(
                listed.contains(expected),
                "wire_protocol_schema is missing input code `{expected}`; \
                 add it to the inputs[] array in src/mode/rest.rs"
            );
        }
    }

    #[test]
    fn schema_advertises_handshake_friendly_endpoints() {
        let schema = wire_protocol_schema();
        let endpoints = schema.get("endpoints").and_then(|v| v.as_object()).unwrap();
        assert!(endpoints.contains_key("POST /v1/afpay"));
        assert!(endpoints.contains_key("GET /v1/schema"));
    }

    #[test]
    fn schema_documents_all_pay_error_codes() {
        let schema = wire_protocol_schema();
        let errors = schema
            .get("error_codes")
            .and_then(|v| v.as_array())
            .unwrap();
        let listed: std::collections::HashSet<&str> = errors
            .iter()
            .filter_map(|entry| entry.get("code").and_then(|v| v.as_str()))
            .collect();
        // Mirror of PayError::error_code() in src/provider/mod.rs.
        for expected in &[
            "not_implemented",
            "wallet_not_found",
            "invalid_amount",
            "network_error",
            "internal_error",
            "limit_exceeded",
            "configure_on_daemon",
            "remote_protocol_error",
            "forbidden",
        ] {
            assert!(
                listed.contains(expected),
                "wire_protocol_schema error_codes missing `{expected}` — \
                 keep in sync with PayError::error_code()"
            );
        }
    }
}
