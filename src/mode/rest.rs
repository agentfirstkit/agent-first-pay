//! `afpay --mode rest`: serve the HTTP domain API.
//!
//! This file is the process boundary — credential, config, operator
//! allowlists, bind address, ready event. The routes themselves, and the
//! OpenAPI contract that describes them, live in `crate::api`.

use crate::api;
use crate::handler;
use crate::types::*;

pub struct RestInit {
    pub listen: String,
    pub api_key_secret: Option<String>,
    pub allow_public_listen: bool,
    pub log: Vec<String>,
    pub data_dir: Option<String>,
    pub startup_argv: Vec<String>,
    pub startup_args: serde_json::Value,
    pub startup_requested: bool,
}

pub async fn run_rest(init: RestInit) {
    let api_key_secret = match resolve_api_key(init.api_key_secret) {
        Ok(secret) => secret,
        Err((message, hint)) => fail_startup(&message, Some(&hint)),
    };

    let resolved_dir = init
        .data_dir
        .unwrap_or_else(|| RuntimeConfig::default().data_dir);
    let mut config = match RuntimeConfig::load_from_dir(&resolved_dir) {
        Ok(config) => config,
        Err(error) => fail_startup(&error, None),
    };
    if !init.log.is_empty() {
        config.log = init.log.clone();
    }

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

    let startup_errors = handler::startup_provider_validation_errors(&config).await;
    for error_output in &startup_errors {
        crate::mode::cli::emit_output(error_output, agent_first_data::OutputFormat::Json);
    }
    if !startup_errors.is_empty() {
        std::process::exit(1);
    }

    let policy = AllowlistPolicy::from_config(&config);
    let address: std::net::SocketAddr = match init.listen.parse() {
        Ok(address) => address,
        Err(error) => fail_startup(
            &format!("invalid --rest-listen address: {error}"),
            Some("expected format: host:port (e.g. 0.0.0.0:9401)"),
        ),
    };
    if !address.ip().is_loopback() && !init.allow_public_listen {
        fail_startup(
            "refusing to bind the HTTP API to a non-loopback address without --public-listen",
            Some(
                "use the default 127.0.0.1:9401, or pass --public-listen only behind TLS/firewall",
            ),
        );
    }
    if init.allow_public_listen
        && let Err(message) = policy.require_for_public_listen()
    {
        fail_startup(
            &message,
            Some(
                "add at least one entry to allowed_mint_urls / allowed_esplora_urls / allowed_sol_rpc_endpoints / allowed_evm_rpc_endpoints in your runtime config before exposing the daemon",
            ),
        );
    }

    let banner = Output::Log {
        event: "startup_policy".to_string(),
        request_id: None,
        version: Some(crate::config::VERSION.to_string()),
        argv: None,
        config: None,
        args: Some(serde_json::json!({
            "listen_address": address.to_string(),
            "policy": policy.banner(),
        })),
        env: None,
        trace: Trace::from_duration(0),
    };
    crate::mode::cli::emit_output(&banner, agent_first_data::OutputFormat::Json);

    let listener = match tokio::net::TcpListener::bind(address).await {
        Ok(listener) => listener,
        Err(error) => fail_startup(&format!("REST bind failed: {error}"), None),
    };
    let bound = listener
        .local_addr()
        .map(|address| address.to_string())
        .unwrap_or_else(|_| address.to_string());
    let api_url = format!("http://{bound}");

    // The ready event names the discovery face rather than describing it, so
    // an agent that just started this daemon can read the contract without
    // being told where it lives. It carries no credential.
    let ready = agent_first_data::json_progress(serde_json::json!({
        "phase": "api_ready",
        "message": if address.ip().is_loopback() {
            "The afpay HTTP API is available on this machine."
        } else {
            "The afpay HTTP API is bound to a non-loopback address; keep the bearer credential private and terminate TLS in front of it."
        },
        "api_url": api_url,
        "openapi_url": format!("{api_url}/openapi.json"),
        "schema_index_url": format!("{api_url}/schemas/index.json"),
        "mode": if address.ip().is_loopback() { "local" } else { "public" },
        "port": address.port(),
    }))
    .trace(serde_json::json!({"duration_ms": 0}))
    .build();
    let _ =
        crate::output_fmt::emit_process_event(ready.into(), agent_first_data::OutputFormat::Json);

    let router = api::router(api::ApiState::new(config, &api_key_secret, init.log));
    if let Err(error) = axum::serve(listener, router).await {
        fail_startup(&format!("REST server error: {error}"), None);
    }
}

/// 32–512 bearer-safe ASCII characters, as the baseline requires, and never
/// echoed: the rejection describes the rule, not the value it rejected.
fn resolve_api_key(explicit: Option<String>) -> Result<String, (String, String)> {
    let secret = explicit
        .or_else(|| std::env::var("AFPAY_REST_API_KEY_SECRET").ok())
        .filter(|secret| !secret.is_empty())
        .ok_or_else(|| {
            (
                "--rest-api-key-secret is required for the HTTP API".to_string(),
                "pass a bearer API key, or set AFPAY_REST_API_KEY_SECRET so it stays out of argv"
                    .to_string(),
            )
        })?;
    let bearer_safe = secret.bytes().all(|byte| {
        byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/' | b'=')
    });
    if !(32..=512).contains(&secret.len()) || !bearer_safe {
        return Err((
            "the HTTP API key must contain 32 through 512 bearer-safe ASCII characters".to_string(),
            "generate one with `openssl rand -hex 32`".to_string(),
        ));
    }
    Ok(secret)
}

fn fail_startup(message: &str, hint: Option<&str>) -> ! {
    crate::mode::cli::emit_cli_error_hint(
        "rest_startup_failed",
        message,
        hint,
        agent_first_data::OutputFormat::Json,
    );
    std::process::exit(1)
}
