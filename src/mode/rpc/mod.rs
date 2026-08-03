pub mod crypto;

use self::crypto::{Cipher, HANDSHAKE_SALT_LEN};
use crate::handler::{self, App};
use crate::types::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tonic::Code;
use tonic::{Request, Response, Status};

/// Default replay-cache TTL for nonces within a single session. Long enough to
/// cover normal request round-trips + modest clock drift, short enough that the
/// cache can't grow unbounded under load.
const REPLAY_NONCE_TTL: Duration = Duration::from_secs(120);
/// Idle timeout for an RPC session. Sessions older than this are evicted on the
/// next handshake or call; the client must re-handshake transparently.
const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(3600);
/// Hard cap on concurrent sessions to bound memory and keep the eviction sweep
/// cheap. A daemon serving 1024 distinct clients at once is already extreme.
const MAX_SESSIONS: usize = 1024;

pub struct RpcInit {
    pub listen: String,
    pub rpc_secret: Option<String>,
    pub allow_public_listen: bool,
    pub log: agent_first_data::LogFilters,
    pub data_dir: Option<String>,
    pub startup_argv: Vec<String>,
    pub startup_args: serde_json::Value,
    pub startup_requested: bool,
}

pub mod proto {
    tonic::include_proto!("afpay");
}

use proto::af_pay_server::{AfPay, AfPayServer};
use proto::{EncryptedRequest, EncryptedResponse, HandshakeRequest, HandshakeResponse};

struct AfPayService {
    /// Server-side copy of the PSK, retained so each Handshake can re-derive
    /// a fresh Cipher with a new per-session salt. The PSK never leaves the
    /// process; the wire only ever sees the salt + ciphertext.
    psk: Arc<String>,
    sessions: Arc<Mutex<HashMap<u64, Arc<SessionEntry>>>>,
    config: RuntimeConfig,
    rate_limiter: Option<RpcRateLimiter>,
}

/// Per-session encryption state. Cipher is cloneable (32-byte key), so callers
/// can pull it out under the outer lock without holding it across decrypt/encrypt.
/// The inner mutex protects replay cache + last-used timestamp.
struct SessionEntry {
    cipher: Cipher,
    inner: Mutex<SessionInner>,
}

struct SessionInner {
    replay_cache: ReplayCache,
    last_used: Instant,
}

/// Time-bounded replay protection. Each accepted nonce is stamped with its
/// arrival `Instant`; entries older than `REPLAY_NONCE_TTL` are evicted on each
/// insert. Replaying a nonce inside the TTL window is rejected; outside the
/// window the original entry is already gone and the request would have failed
/// AEAD decrypt anyway (session salt is fresh per handshake).
struct ReplayCache {
    seen: HashMap<Vec<u8>, Instant>,
    ttl: Duration,
}

impl ReplayCache {
    fn new(ttl: Duration) -> Self {
        Self {
            seen: HashMap::new(),
            ttl,
        }
    }

    fn insert_unique(&mut self, nonce: &[u8], now: Instant) -> bool {
        self.evict_expired(now);
        if self.seen.contains_key(nonce) {
            return false;
        }
        self.seen.insert(nonce.to_vec(), now);
        true
    }

    fn evict_expired(&mut self, now: Instant) {
        let ttl = self.ttl;
        self.seen.retain(|_, t| now.duration_since(*t) < ttl);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod replay_cache_tests {
    use super::ReplayCache;
    use std::time::{Duration, Instant};

    #[test]
    fn first_nonce_accepted_replay_rejected() {
        let mut cache = ReplayCache::new(Duration::from_secs(60));
        let now = Instant::now();
        let nonce = [1u8, 2, 3];
        assert!(cache.insert_unique(&nonce, now), "first sighting is unique");
        assert!(
            !cache.insert_unique(&nonce, now),
            "second sighting must be rejected within TTL"
        );
    }

    #[test]
    fn nonce_reusable_after_ttl_expires() {
        // Use a very short TTL so we can pin instants without sleeping.
        let mut cache = ReplayCache::new(Duration::from_millis(10));
        let t0 = Instant::now();
        let nonce = [9u8, 9, 9];
        assert!(cache.insert_unique(&nonce, t0));
        // Same instant → still in the window → rejected.
        assert!(!cache.insert_unique(&nonce, t0));
        // Past the TTL → entry is evicted on insert, the nonce is fresh again.
        let later = t0 + Duration::from_millis(50);
        assert!(
            cache.insert_unique(&nonce, later),
            "nonce older than TTL is no longer protected"
        );
    }

    #[test]
    fn evict_drops_only_old_entries() {
        let mut cache = ReplayCache::new(Duration::from_secs(1));
        let t0 = Instant::now();
        cache.insert_unique(&[1], t0);
        cache.insert_unique(&[2], t0 + Duration::from_millis(500));
        // Sweep at t0 + 1.2s: nonce [1] is older than 1s and should be evicted;
        // nonce [2] is 700ms old and should survive.
        cache.evict_expired(t0 + Duration::from_millis(1200));
        assert!(
            cache.insert_unique(&[1], t0 + Duration::from_millis(1200)),
            "evicted nonce is reusable"
        );
        assert!(
            !cache.insert_unique(&[2], t0 + Duration::from_millis(1200)),
            "still-fresh nonce stays protected"
        );
    }
}

/// Simple token-bucket rate limiter for RPC.
struct RpcRateLimiter {
    rps: u32,
    max_concurrent: u32,
    in_flight: AtomicU32,
    tokens_milli: AtomicU64,
    last_refill_ms: AtomicU64,
}

impl RpcRateLimiter {
    fn new(config: &RateLimitConfig) -> Self {
        let rps = config.requests_per_second;
        Self {
            rps,
            max_concurrent: config.max_concurrent,
            in_flight: AtomicU32::new(0),
            tokens_milli: AtomicU64::new(u64::from(rps) * 1000),
            last_refill_ms: AtomicU64::new(rpc_now_ms()),
        }
    }

    fn try_acquire(&self) -> Result<RpcRateLimitGuard<'_>, ()> {
        if self.max_concurrent > 0 {
            let prev = self.in_flight.fetch_add(1, Ordering::Relaxed);
            if prev >= self.max_concurrent {
                self.in_flight.fetch_sub(1, Ordering::Relaxed);
                return Err(());
            }
        }
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
        Ok(RpcRateLimitGuard { limiter: self })
    }

    fn refill(&self) {
        let now = rpc_now_ms();
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
            let add = elapsed * u64::from(self.rps);
            let max = u64::from(self.rps) * 1000;
            let _ = self
                .tokens_milli
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| {
                    Some(c.saturating_add(add).min(max))
                });
        }
    }
}

struct RpcRateLimitGuard<'a> {
    limiter: &'a RpcRateLimiter,
}

impl Drop for RpcRateLimitGuard<'_> {
    fn drop(&mut self) {
        if self.limiter.max_concurrent > 0 {
            self.limiter.in_flight.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

fn rpc_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl AfPayService {
    /// Generate a fresh `(salt, session_id)` pair, derive a Cipher from the PSK
    /// plus salt, and insert into the session table. Old entries are evicted
    /// either lazily (idle TTL) or by FIFO when capacity is reached.
    fn open_session(&self) -> Result<(Vec<u8>, u64), Status> {
        let mut salt = vec![0u8; HANDSHAKE_SALT_LEN];
        getrandom::fill(&mut salt).map_err(|e| Status::internal(format!("random salt: {e}")))?;
        let mut id_bytes = [0u8; 8];
        getrandom::fill(&mut id_bytes)
            .map_err(|e| Status::internal(format!("random session id: {e}")))?;
        let session_id = u64::from_le_bytes(id_bytes);

        let cipher = Cipher::from_secret_with_salt(&self.psk, &salt);
        let entry = Arc::new(SessionEntry {
            cipher,
            inner: Mutex::new(SessionInner {
                replay_cache: ReplayCache::new(REPLAY_NONCE_TTL),
                last_used: Instant::now(),
            }),
        });

        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| Status::internal("session table poisoned"))?;
        // Evict by idle TTL first; cheap when there are few stale entries.
        let cutoff = Instant::now() - SESSION_IDLE_TIMEOUT;
        sessions.retain(|_, e| match e.inner.lock() {
            Ok(g) => g.last_used >= cutoff,
            Err(_) => false,
        });
        // Hard cap: if still above ceiling, drop the oldest entry by last_used
        // until we're under. This is a last-resort defense — in practice idle
        // TTL keeps us far below MAX_SESSIONS.
        while sessions.len() >= MAX_SESSIONS {
            let oldest_key = sessions
                .iter()
                .filter_map(|(k, e)| e.inner.lock().ok().map(|g| (g.last_used, *k)))
                .min()
                .map(|(_, k)| k);
            match oldest_key {
                Some(k) => {
                    sessions.remove(&k);
                }
                None => break,
            }
        }
        sessions.insert(session_id, entry);
        Ok((salt, session_id))
    }

    /// Look up a session, refresh its last-used timestamp, and replay-check the
    /// nonce. Returns the session's cipher (clone — cheap) on success.
    fn use_session(&self, session_id: u64, nonce: &[u8]) -> Result<Cipher, Status> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| Status::internal("session table poisoned"))?;
        let entry = sessions
            .get(&session_id)
            .cloned()
            .ok_or_else(|| Status::unauthenticated("session_expired"))?;
        drop(sessions);

        let now = Instant::now();
        let mut inner = entry
            .inner
            .lock()
            .map_err(|_| Status::internal("session inner poisoned"))?;
        if now.duration_since(inner.last_used) >= SESSION_IDLE_TIMEOUT {
            return Err(Status::unauthenticated("session_expired"));
        }
        if !inner.replay_cache.insert_unique(nonce, now) {
            return Err(Status::unauthenticated("replayed request nonce"));
        }
        inner.last_used = now;
        Ok(entry.cipher.clone())
    }
}

#[tonic::async_trait]
impl AfPay for AfPayService {
    async fn handshake(
        &self,
        request: Request<HandshakeRequest>,
    ) -> Result<Response<HandshakeResponse>, Status> {
        // Rate-limit handshakes on the same token bucket as `call`. Without
        // this, an unauthenticated peer can churn `MAX_SESSIONS` slots in a
        // tight loop and starve legitimate clients — `open_session` is
        // otherwise gated only by the hard cap, with a 1h idle timeout
        // before slots free up. Keep the guard alive for the duration of
        // session-table mutation so concurrent floods can't slip past.
        let _rate_guard = if let Some(rl) = &self.rate_limiter {
            match rl.try_acquire() {
                Ok(guard) => Some(guard),
                Err(()) => {
                    return Err(Status::resource_exhausted("rate limit exceeded"));
                }
            }
        } else {
            None
        };

        let _req = request.into_inner();
        // We deliberately ignore client_nonce in salt derivation right now —
        // the server's own random salt suffices for the "defeat rainbow tables
        // for weak PSKs" goal. Keeping the field in the proto lets a future
        // hardening pass mix it in without a wire break.
        let (salt, session_id) = self.open_session()?;
        Ok(Response::new(HandshakeResponse {
            salt,
            session_id,
            session_idle_timeout_s: SESSION_IDLE_TIMEOUT.as_secs(),
        }))
    }

    async fn call(
        &self,
        request: Request<EncryptedRequest>,
    ) -> Result<Response<EncryptedResponse>, Status> {
        let req = request.into_inner();

        // Rate limit check
        let _rate_guard = if let Some(rl) = &self.rate_limiter {
            match rl.try_acquire() {
                Ok(guard) => Some(guard),
                Err(()) => {
                    return Err(Status::resource_exhausted("rate limit exceeded"));
                }
            }
        } else {
            None
        };

        let cipher = match self.use_session(req.session_id, &req.nonce) {
            Ok(c) => c,
            Err(status) => {
                emit_rpc_response_log(&self.config, None, &[], Some(&status));
                return Err(status);
            }
        };

        // Decrypt request
        let plaintext = match cipher.decrypt(&req.nonce, &req.ciphertext) {
            Ok(plaintext) => plaintext,
            Err(_) => {
                emit_rpc_request_log(
                    &self.config,
                    None,
                    serde_json::json!({
                        "input": serde_json::Value::Null,
                        "decode_error": "decryption failed",
                    }),
                );
                let status = Status::unauthenticated("decryption failed");
                emit_rpc_response_log(&self.config, None, &[], Some(&status));
                return Err(status);
            }
        };

        let mut raw_input_value = serde_json::from_slice::<serde_json::Value>(&plaintext)
            .unwrap_or(serde_json::Value::Null);
        if let Some(object) = raw_input_value.as_object_mut() {
            object.remove("id");
        }

        // Parse Request (carries dry_run flag plus the Input).
        let request: crate::types::Request = match serde_json::from_slice(&plaintext) {
            Ok(request) => request,
            Err(e) => {
                emit_rpc_request_log(
                    &self.config,
                    None,
                    serde_json::json!({
                        "input": raw_input_value,
                        "decode_error": format!("invalid input: {e}"),
                    }),
                );
                let status = Status::invalid_argument(format!("invalid input: {e}"));
                emit_rpc_response_log(&self.config, None, &[], Some(&status));
                return Err(status);
            }
        };
        let request_id = input_request_id(&request.input).map(|s| s.to_string());
        emit_rpc_request_log(
            &self.config,
            request_id.clone(),
            serde_json::json!({
                "input": raw_input_value,
                "dry_run": request.dry_run,
            }),
        );

        // Block local-only operations over RPC
        if request.input.is_local_only() {
            let status = Status::permission_denied("local-only operation");
            emit_rpc_response_log(&self.config, request_id, &[], Some(&status));
            return Err(status);
        }

        // Create per-request channel and App
        let (tx, mut rx) = mpsc::channel::<Output>(256);
        let store = crate::store::create_storage_backend(&self.config);
        let app = Arc::new(App::new(self.config.clone(), tx, Some(true), store));
        app.requests_total.fetch_add(1, Ordering::Relaxed);

        // Dispatch
        handler::dispatch(&app, request).await;

        // Drop app to close the sender side, then collect all outputs
        drop(app);
        let mut outputs = Vec::new();
        while let Some(out) = rx.recv().await {
            // Mirror server-side log events to rpc daemon stdout so operators can
            // observe request flow in long-running rpc mode.
            if let Output::Log { .. } = &out {
                crate::mode::cli::emit_output(&out, agent_first_data::OutputFormat::Json);
            }
            match crate::output_fmt::protocol_event(&out) {
                Ok(value) => outputs.push(value),
                Err(error) => outputs.push(
                    agent_first_data::json_error("serialization_failed", &error)
                        .build()
                        .map(Into::into)
                        .unwrap_or(serde_json::Value::Null),
                ),
            }
        }

        // Serialize outputs as JSON array
        let response_json = match serde_json::to_vec(&outputs) {
            Ok(response_json) => response_json,
            Err(e) => {
                let status = Status::internal(format!("serialize: {e}"));
                emit_rpc_response_log(&self.config, request_id, &outputs, Some(&status));
                return Err(status);
            }
        };

        // Encrypt response with the session's Cipher (already cloned out above).
        let (nonce, ciphertext) = match cipher.encrypt(&response_json) {
            Ok(payload) => payload,
            Err(e) => {
                let status = Status::internal(format!("encrypt: {e}"));
                emit_rpc_response_log(&self.config, request_id, &outputs, Some(&status));
                return Err(status);
            }
        };

        emit_rpc_response_log(&self.config, request_id, &outputs, None);

        Ok(Response::new(EncryptedResponse {
            session_id: req.session_id,
            nonce,
            ciphertext,
        }))
    }
}

pub async fn run_rpc(init: RpcInit) {
    let secret: String = match init
        .rpc_secret
        .or_else(|| std::env::var("AFPAY_RPC_SECRET").ok())
    {
        Some(s) if !s.is_empty() => s,
        _ => {
            crate::mode::cli::emit_cli_error_hint(
                "rpc_startup_failed",
                "--rpc-secret is required for RPC mode",
                Some("pass a shared secret for client authentication or set AFPAY_RPC_SECRET"),
                agent_first_data::OutputFormat::Json,
            );
            std::process::exit(1);
        }
    };
    if let Err(e) = Cipher::validate_secret(&secret) {
        crate::mode::cli::emit_cli_error_hint(
            "rpc_startup_failed",
            &e,
            Some("use a random 32+ byte secret"),
            agent_first_data::OutputFormat::Json,
        );
        std::process::exit(1);
    }

    let resolved_dir = init
        .data_dir
        .unwrap_or_else(|| RuntimeConfig::default().data_dir);
    let mut config = match RuntimeConfig::load_from_dir(&resolved_dir) {
        Ok(c) => c,
        Err(e) => {
            crate::mode::cli::emit_cli_error_hint(
                "rpc_startup_failed",
                &e,
                None,
                agent_first_data::OutputFormat::Json,
            );
            std::process::exit(1);
        }
    };
    if !init.log.is_empty() {
        config.log = init.log.as_slice().to_vec();
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
        crate::mode::cli::emit_output(&startup, agent_first_data::OutputFormat::Json);
    }

    let startup_errors = crate::handler::startup_provider_validation_errors(&config).await;
    for error_output in &startup_errors {
        crate::mode::cli::emit_output(error_output, agent_first_data::OutputFormat::Json);
    }
    if !startup_errors.is_empty() {
        std::process::exit(1);
    }

    let rate_limiter = config.rate_limit.as_ref().map(RpcRateLimiter::new);
    let policy = AllowlistPolicy::from_config(&config);
    let service = AfPayService {
        psk: Arc::new(secret),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        config,
        rate_limiter,
    };

    let addr = match init.listen.parse() {
        Ok(a) => a,
        Err(e) => {
            crate::mode::cli::emit_cli_error_hint(
                "rpc_startup_failed",
                &format!("invalid --rpc-listen address: {e}"),
                Some("expected format: host:port (e.g. 127.0.0.1:9100)"),
                agent_first_data::OutputFormat::Json,
            );
            std::process::exit(1);
        }
    };
    if public_listen_requires_ack(addr) && !init.allow_public_listen {
        crate::mode::cli::emit_cli_error_hint(
            "rpc_startup_failed",
            "refusing to bind RPC to a non-loopback address without --public-listen",
            Some(
                "use the default 127.0.0.1:9400, or pass --public-listen only behind TLS/firewall",
            ),
            agent_first_data::OutputFormat::Json,
        );
        std::process::exit(1);
    }
    if init.allow_public_listen
        && let Err(msg) = policy.require_for_public_listen()
    {
        crate::mode::cli::emit_cli_error_hint(
            "rpc_startup_failed",
            &msg,
            Some(
                "add at least one entry to allowed_mint_urls / allowed_esplora_urls / allowed_sol_rpc_endpoints / allowed_evm_rpc_endpoints in your runtime config before exposing the daemon",
            ),
            agent_first_data::OutputFormat::Json,
        );
        std::process::exit(1);
    }
    let banner = Output::Log {
        event: "startup_policy".to_string(),
        request_id: None,
        version: Some(crate::config::VERSION.to_string()),
        argv: None,
        config: None,
        args: Some(serde_json::json!({
            "listen_address": addr.to_string(),
            "policy": policy.banner(),
        })),
        env: None,
        trace: Trace::from_duration(0),
    };
    crate::mode::cli::emit_output(&banner, agent_first_data::OutputFormat::Json);

    let server = tonic::transport::Server::builder()
        .add_service(AfPayServer::new(service))
        .serve(addr);

    if let Err(e) = server.await {
        crate::mode::cli::emit_cli_error_hint(
            "rpc_startup_failed",
            &format!("RPC server error: {e}"),
            None,
            agent_first_data::OutputFormat::Json,
        );
        std::process::exit(1);
    }
}

fn public_listen_requires_ack(addr: std::net::SocketAddr) -> bool {
    !addr.ip().is_loopback()
}

fn emit_rpc_request_log(
    config: &RuntimeConfig,
    request_id: Option<String>,
    args: serde_json::Value,
) {
    emit_rpc_log(config, "rpc_request", request_id, args);
}

fn emit_rpc_response_log(
    config: &RuntimeConfig,
    request_id: Option<String>,
    outputs: &[serde_json::Value],
    status: Option<&Status>,
) {
    let has_output_error = outputs
        .iter()
        .any(|item| item.get("code").and_then(|v| v.as_str()) == Some("error"));
    let mut args = serde_json::json!({
        "output_count": outputs.len(),
        "has_error": has_output_error || status.is_some(),
        "outputs": outputs,
    });
    if let Some(status) = status
        && let Some(object) = args.as_object_mut()
    {
        object.insert(
            "grpc_error".to_string(),
            serde_json::json!({
                "code": grpc_code_name(status.code()),
                "message": status.message(),
            }),
        );
    }
    emit_rpc_log(config, "rpc_response", request_id, args);
}

fn emit_rpc_log(
    config: &RuntimeConfig,
    event: &str,
    request_id: Option<String>,
    args: serde_json::Value,
) {
    if !agent_first_data::LogFilters::new(config.log.clone()).enabled(event) {
        return;
    }
    let log = Output::Log {
        event: event.to_string(),
        request_id,
        version: None,
        argv: None,
        config: None,
        args: Some(args),
        env: None,
        trace: Trace::from_duration(0),
    };
    crate::mode::cli::emit_output(&log, agent_first_data::OutputFormat::Json);
}

fn grpc_code_name(code: Code) -> &'static str {
    match code {
        Code::Ok => "ok",
        Code::Cancelled => "cancelled",
        Code::Unknown => "unknown",
        Code::InvalidArgument => "invalid_argument",
        Code::DeadlineExceeded => "deadline_exceeded",
        Code::NotFound => "not_found",
        Code::AlreadyExists => "already_exists",
        Code::PermissionDenied => "permission_denied",
        Code::ResourceExhausted => "resource_exhausted",
        Code::FailedPrecondition => "failed_precondition",
        Code::Aborted => "aborted",
        Code::OutOfRange => "out_of_range",
        Code::Unimplemented => "unimplemented",
        Code::Internal => "internal",
        Code::Unavailable => "unavailable",
        Code::DataLoss => "data_loss",
        Code::Unauthenticated => "unauthenticated",
    }
}

fn input_request_id(input: &Input) -> Option<&str> {
    match input {
        Input::WalletCreate { id, .. }
        | Input::LnWalletCreate { id, .. }
        | Input::WalletClose { id, .. }
        | Input::WalletList { id, .. }
        | Input::Balance { id, .. }
        | Input::Receive { id, .. }
        | Input::ReceiveClaim { id, .. }
        | Input::CashuSend { id, .. }
        | Input::CashuReceive { id, .. }
        | Input::Send { id, .. }
        | Input::Restore { id, .. }
        | Input::WalletShowSeed { id, .. }
        | Input::HistoryList { id, .. }
        | Input::HistoryStatus { id, .. }
        | Input::HistoryUpdate { id, .. }
        | Input::LimitAdd { id, .. }
        | Input::LimitRemove { id, .. }
        | Input::LimitList { id, .. }
        | Input::LimitSet { id, .. }
        | Input::ReconcileReservation { id, .. }
        | Input::WalletConfigShow { id, .. }
        | Input::WalletConfigSet { id, .. }
        | Input::WalletConfigTokenAdd { id, .. }
        | Input::WalletConfigTokenRemove { id, .. } => Some(id.as_str()),
        Input::ConfigGet { .. }
        | Input::ConfigSet { .. }
        | Input::Version
        | Input::Schema
        | Input::Close => None,
    }
}
