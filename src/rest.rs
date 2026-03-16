use crate::handler::{self, App};
use crate::store;
use crate::types::*;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Json;
use std::io::Write;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct RestInit {
    pub listen: String,
    pub api_key: Option<String>,
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
    let api_key: String = match init.api_key {
        Some(s) if !s.is_empty() => s,
        _ => {
            let value = agent_first_data::build_cli_error(
                "--rest-api-key is required for REST mode",
                Some("pass an API key for bearer authentication"),
            );
            let rendered =
                agent_first_data::cli_output(&value, agent_first_data::OutputFormat::Json);
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
            let rendered =
                agent_first_data::cli_output(&value, agent_first_data::OutputFormat::Json);
            let _ = writeln!(std::io::stdout(), "{rendered}");
            std::process::exit(1);
        }
    };
    if !init.log.is_empty() {
        config.log = init.log.clone();
    }

    // Emit startup log
    if let Some(startup) = crate::config::maybe_startup_log(
        &config.log,
        init.startup_requested,
        Some(init.startup_argv),
        Some(&config),
        init.startup_args,
    ) {
        let value = serde_json::to_value(&startup).unwrap_or(serde_json::Value::Null);
        let rendered = agent_first_data::cli_output(&value, agent_first_data::OutputFormat::Json);
        let _ = writeln!(std::io::stdout(), "{rendered}");
    }

    let startup_errors = handler::startup_provider_validation_errors(&config).await;
    for error_output in &startup_errors {
        let value = serde_json::to_value(error_output).unwrap_or(serde_json::Value::Null);
        let rendered = agent_first_data::cli_output(&value, agent_first_data::OutputFormat::Json);
        let _ = writeln!(std::io::stdout(), "{rendered}");
    }
    if !startup_errors.is_empty() {
        std::process::exit(1);
    }

    let rate_limiter = config.rate_limit.as_ref().map(RateLimiter::new);
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
        .with_state(state);

    let addr: std::net::SocketAddr = match init.listen.parse() {
        Ok(a) => a,
        Err(e) => {
            let value = agent_first_data::build_cli_error(
                &format!("invalid --rest-listen address: {e}"),
                Some("expected format: host:port (e.g. 0.0.0.0:9401)"),
            );
            let rendered =
                agent_first_data::cli_output(&value, agent_first_data::OutputFormat::Json);
            let _ = writeln!(std::io::stdout(), "{rendered}");
            std::process::exit(1);
        }
    };

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            let value = agent_first_data::build_cli_error(&format!("REST bind failed: {e}"), None);
            let rendered =
                agent_first_data::cli_output(&value, agent_first_data::OutputFormat::Json);
            let _ = writeln!(std::io::stdout(), "{rendered}");
            std::process::exit(1);
        }
    };

    if let Err(e) = axum::serve(listener, router).await {
        let value = agent_first_data::build_cli_error(&format!("REST server error: {e}"), None);
        let rendered = agent_first_data::cli_output(&value, agent_first_data::OutputFormat::Json);
        let _ = writeln!(std::io::stdout(), "{rendered}");
        std::process::exit(1);
    }
}

fn check_auth(headers: &HeaderMap, expected: &str) -> Result<(), StatusCode> {
    // Try Authorization: Bearer <key>
    if let Some(val) = headers.get("authorization") {
        let val = val.to_str().map_err(|_| StatusCode::UNAUTHORIZED)?;
        if let Some(token) = val.strip_prefix("Bearer ") {
            if constant_time_eq(token.as_bytes(), expected.as_bytes()) {
                return Ok(());
            }
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
                    Json(serde_json::json!({
                        "code": "error",
                        "error": "rate limit exceeded",
                    })),
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
            Json(serde_json::json!({
                "code": "error",
                "error": "unauthorized",
            })),
        );
    }

    // Parse Input from body
    let input: Input = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "code": "error",
                    "error": format!("invalid input: {e}"),
                })),
            );
        }
    };

    // Block local-only operations
    if input.is_local_only() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "code": "error",
                "error": "local-only operation not allowed over REST",
            })),
        );
    }

    // Create per-request channel and App
    let (tx, mut rx) = mpsc::channel::<Output>(256);
    let config = state.app.config.read().await.clone();
    let st = store::create_storage_backend(&config);
    let app = Arc::new(App::new(config, tx, Some(true), st));
    app.requests_total.fetch_add(1, Ordering::Relaxed);

    // Dispatch
    handler::dispatch(&app, input).await;

    // Collect outputs
    drop(app);
    let mut outputs = Vec::new();
    while let Some(out) = rx.recv().await {
        // Mirror log events to daemon stdout
        if let Output::Log { ref event, .. } = out {
            if log_event_enabled(&state.log, event) {
                let rendered = agent_first_data::cli_output(
                    &serde_json::to_value(&out).unwrap_or(serde_json::Value::Null),
                    agent_first_data::OutputFormat::Json,
                );
                let _ = writeln!(std::io::stdout(), "{rendered}");
            }
        }
        let value = serde_json::to_value(&out).unwrap_or(serde_json::Value::Null);
        outputs.push(value);
    }

    // Check if any output is an error
    let has_error = outputs
        .iter()
        .any(|item| item.get("code").and_then(|v| v.as_str()) == Some("error"));

    let status = if has_error {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };

    (status, Json(serde_json::Value::Array(outputs)))
}

fn log_event_enabled(log_filters: &[String], event: &str) -> bool {
    if log_filters.is_empty() {
        return false;
    }
    let ev = event.to_ascii_lowercase();
    log_filters
        .iter()
        .any(|f| f == "*" || f == "all" || ev.starts_with(f.as_str()))
}
