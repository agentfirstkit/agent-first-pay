#![cfg(all(feature = "federation", feature = "rest"))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Federation, driven against a real afpay HTTP face over a real socket.
//!
//! There is no test double for the peer here: every "reaches the peer" claim
//! goes through `api::router` — the router `afpay --mode rest` serves — bound
//! to a loopback port, so a federation call that only works because a stub
//! answered it cannot exist. The doubles that *are* here impersonate things
//! that are **not** afpay, because refusing those legibly is the other half of
//! the contract.

use agent_first_pay::api::{ApiState, router};
use agent_first_pay::provider::remote;
use agent_first_pay::types::{Input, Network, Output, PeerConfig, Request, RuntimeConfig};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::mpsc;

/// Satisfies the credential rule `--mode rest` enforces at startup, so the
/// tests exercise a key a real daemon would accept.
const TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// Nothing listens here. Used to prove a refusal happened before any bytes
/// left the process: had the client tried to send, it would have reported
/// `peer_unreachable` instead.
const DEAD_PEER: &str = "http://127.0.0.1:1";

// ═══════════════════════════════════════════
// A real afpay peer
// ═══════════════════════════════════════════

struct Peer {
    url: String,
    _directory: tempfile::TempDir,
}

/// Bind a real afpay HTTP API on a free loopback port.
async fn start_peer() -> Peer {
    let directory = tempfile::tempdir().unwrap();
    let config = RuntimeConfig {
        data_dir: directory.path().to_string_lossy().into_owned(),
        ..RuntimeConfig::default()
    };
    let app = router(ApiState::new(config, TOKEN, Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Peer {
        url: format!("http://{address}"),
        _directory: directory,
    }
}

/// Bind a listener that answers every request with `body`, so the client's
/// "this is not afpay" path can be driven against something that really is
/// not afpay.
async fn start_impostor(status: u16, content_type: &'static str, body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut scratch = [0u8; 4096];
                let _ = stream.read(&mut scratch).await;
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    format!("http://{address}")
}

fn error_code(value: &serde_json::Value) -> &str {
    value
        .get("error_code")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
}

fn message(value: &serde_json::Value) -> &str {
    value.get("error").and_then(|v| v.as_str()).unwrap_or("")
}

// ═══════════════════════════════════════════
// The federation leg, over real bytes
// ═══════════════════════════════════════════

/// One HTTP request per call, against the routes the peer publishes, coming
/// back in the flat `Output` shape the rest of afpay reads.
#[tokio::test]
async fn federation_reads_a_real_peer_over_its_published_routes() {
    let peer = start_peer().await;

    let outputs = remote::peer_call(&peer.url, TOKEN, &Input::Version).await;
    assert_eq!(outputs.len(), 1, "one HTTP answer, one output");
    assert_eq!(outputs[0]["code"], "version", "{:?}", outputs[0]);
    assert!(outputs[0]["version"].is_string());

    let outputs = remote::peer_call(
        &peer.url,
        TOKEN,
        &Input::WalletList {
            id: "req_wallets".to_string(),
            network: Some(Network::Cashu),
        },
    )
    .await;
    assert_eq!(outputs[0]["code"], "wallet_list", "{:?}", outputs[0]);
    assert_eq!(outputs[0]["wallets"], serde_json::json!([]));

    let outputs = remote::peer_call(
        &peer.url,
        TOKEN,
        &Input::Balance {
            id: "req_balances".to_string(),
            wallet: None,
            network: None,
            check: false,
        },
    )
    .await;
    assert_eq!(outputs[0]["code"], "wallet_balances", "{:?}", outputs[0]);

    let outputs = remote::peer_call(
        &peer.url,
        TOKEN,
        &Input::HistoryList {
            id: "req_history".to_string(),
            wallet: None,
            network: None,
            onchain_memo: None,
            limit: Some(5),
            offset: Some(0),
            since_epoch_s: None,
            until_epoch_s: None,
        },
    )
    .await;
    assert_eq!(outputs[0]["code"], "history", "{:?}", outputs[0]);

    // The limit forwarding the cascading-topology view depends on: a read the
    // peer already publishes, not a private door.
    let outputs = remote::peer_call(
        &peer.url,
        TOKEN,
        &Input::LimitList {
            id: "req_limits".to_string(),
        },
    )
    .await;
    assert_eq!(outputs[0]["code"], "limit_status", "{:?}", outputs[0]);
    assert!(outputs[0]["limits"].is_array());
}

/// Planning a payment reaches the peer's dispatcher — the same one the local
/// CLI runs — and comes back as a domain answer rather than a transport
/// complaint.
#[tokio::test]
async fn a_payment_plan_forwarded_to_a_peer_reaches_its_dispatcher() {
    let peer = start_peer().await;
    let outputs = remote::peer_call(
        &peer.url,
        TOKEN,
        &Input::SendPlan {
            id: "req_00000000000000000000000000000001".to_string(),
            wallet: Some("w_missing".to_string()),
            network: None,
            to: "bc1qexample".to_string(),
            amount: Some(agent_first_pay::types::Amount {
                value: 1,
                token: "sats".to_string(),
            }),
            onchain_memo: None,
            local_memo: None,
            mints: None,
            chain_id: None,
        },
    )
    .await;
    // No such wallet on the peer, which is a domain answer: the request got
    // through the credential and the typed body.
    assert_eq!(outputs[0]["code"], "error", "{:?}", outputs[0]);
    assert_eq!(error_code(&outputs[0]), "wallet_not_found");
}

/// A confirm reaches the peer too, and the peer refuses an id it never issued
/// rather than paying. This is the second half of §9 seen from the client:
/// there is no route that pays without a plan the peer itself recorded.
#[tokio::test]
async fn a_confirm_for_a_plan_the_peer_never_issued_is_refused() {
    let peer = start_peer().await;
    let outputs = remote::peer_call(
        &peer.url,
        TOKEN,
        &Input::PayConfirm {
            id: "req_00000000000000000000000000000002".to_string(),
            plan_id: "plan_00000000000000000000000000000000".to_string(),
            expect: Some(agent_first_pay::types::PayPlanOperation::Send),
            idempotency_key: Some("federation-confirm-1".to_string()),
        },
    )
    .await;
    assert_eq!(outputs[0]["code"], "error", "{:?}", outputs[0]);
    assert_eq!(error_code(&outputs[0]), "plan_not_found");
}

// ═══════════════════════════════════════════
// The surface federation is not given
// ═══════════════════════════════════════════

/// Writing a spend-limit rule is what a leaked bearer must never be able to
/// do, so it has no route — and federation does not get a private one. The
/// refusal is produced without contacting the peer at all, which is why
/// pointing at a dead address still answers immediately.
#[tokio::test]
async fn a_local_only_operation_is_refused_before_any_bytes_leave() {
    let outputs = remote::peer_call(
        DEAD_PEER,
        TOKEN,
        &Input::LimitSet {
            id: "req_limit_set".to_string(),
            limits: vec![agent_first_pay::types::SpendLimit {
                rule_id: None,
                scope: agent_first_pay::types::SpendScope::Network,
                network: Some("cashu".to_string()),
                wallet: None,
                window_s: 3600,
                max_spend: 10_000,
                token: None,
            }],
        },
    )
    .await;
    assert_eq!(error_code(&outputs[0]), "forbidden", "{:?}", outputs[0]);
    assert!(
        message(&outputs[0]).contains("only on the machine that holds the data"),
        "{:?}",
        outputs[0]
    );

    for input in [
        Input::WalletShowSeed {
            id: "req_seed".to_string(),
            wallet: "w_1".to_string(),
        },
        Input::Restore {
            id: "req_restore".to_string(),
            wallet: "w_1".to_string(),
        },
        Input::ConfigSet {
            id: "req_config".to_string(),
            key: "log".to_string(),
            values: vec!["wallet".to_string()],
        },
        Input::ReconcileReservation {
            id: "req_reconcile".to_string(),
            reservation_id: 1,
            action: agent_first_pay::types::ReconcileAction::Cancel,
            reason: "test".to_string(),
        },
    ] {
        let outputs = remote::peer_call(DEAD_PEER, TOKEN, &input).await;
        assert_eq!(error_code(&outputs[0]), "forbidden", "{:?}", outputs[0]);
    }
}

/// The same exclusion, proved against a peer that is actually listening: the
/// HTTP face has no route for these either, so neither side has a back door.
#[tokio::test]
async fn a_live_peer_has_no_route_for_a_local_only_operation() {
    let peer = start_peer().await;
    let client = reqwest::Client::new();
    for path in ["/v1/spend-limits", "/v1/config", "/v1/wallets/w_1/seed"] {
        let response = client
            .post(format!("{}{path}", peer.url))
            .bearer_auth(TOKEN)
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert!(
            response.status() == 404 || response.status() == 405,
            "{path} is writable over HTTP: {}",
            response.status()
        );
    }
}

// ═══════════════════════════════════════════
// A peer that is not this afpay
// ═══════════════════════════════════════════

/// The assumption this change is built on: nobody is federating in the field
/// yet, so a mismatched peer must fail loudly rather than be quietly
/// tolerated. Each case below names what was found.
#[tokio::test]
async fn a_peer_that_is_not_afpay_is_named_rather_than_parsed_hopefully() {
    let impostor = start_impostor(
        200,
        "text/html",
        "<html><body>nginx welcome page</body></html>",
    )
    .await;

    let outputs = remote::peer_call(
        &impostor,
        TOKEN,
        &Input::WalletList {
            id: "req_wallets".to_string(),
            network: None,
        },
    )
    .await;
    assert_eq!(
        error_code(&outputs[0]),
        "peer_not_afpay",
        "{:?}",
        outputs[0]
    );
    let text = message(&outputs[0]);
    assert!(text.contains("GET /v1/wallets"), "{text}");
    assert!(text.contains(&impostor), "{text}");
    assert!(text.contains("text/html"), "{text}");
    assert!(text.contains("nginx welcome page"), "{text}");
    assert!(
        outputs[0]["hint"].as_str().unwrap().contains("/health"),
        "{:?}",
        outputs[0]
    );
}

/// A JSON API that is not afpay is the nastier case: the body parses, so a
/// hopeful client would hand back a wrong answer. It must not.
#[tokio::test]
async fn a_json_service_that_is_not_afpay_is_still_refused() {
    let impostor = start_impostor(200, "application/json", r#"{"wallets":[],"ok":true}"#).await;
    let outputs = remote::peer_call(
        &impostor,
        TOKEN,
        &Input::WalletList {
            id: "req_wallets".to_string(),
            network: None,
        },
    )
    .await;
    assert_eq!(
        error_code(&outputs[0]),
        "peer_not_afpay",
        "a body without afpay's envelope is not an answer: {:?}",
        outputs[0]
    );
}

/// An afpay-shaped `/health` from something that is not afpay: the identity
/// probe reads the payload, not just its shape.
#[tokio::test]
async fn a_health_route_belonging_to_another_service_is_refused() {
    let impostor = start_impostor(
        200,
        "application/json",
        r#"{"kind":"result","result":{"service":"grafana","version":"11.0.0","status":"ready"},"trace":{"duration_ms":0}}"#,
    )
    .await;
    let outputs = remote::peer_call(&impostor, TOKEN, &Input::Version).await;
    assert_eq!(
        error_code(&outputs[0]),
        "peer_not_afpay",
        "{:?}",
        outputs[0]
    );
    assert!(message(&outputs[0]).contains("grafana"), "{:?}", outputs[0]);
}

/// A real afpay whose resource routes this build does not know about reports
/// the version gap, not "route not found" — which would read like the
/// caller's mistake.
#[tokio::test]
async fn a_peer_missing_a_route_reports_the_version_gap() {
    let impostor = start_impostor(
        404,
        "application/json",
        r#"{"kind":"error","error":{"code":"api_route_not_found","message":"API route not found","retryable":false},"trace":{"duration_ms":0}}"#,
    )
    .await;
    let outputs = remote::peer_call(
        &impostor,
        TOKEN,
        &Input::LimitList {
            id: "req_limits".to_string(),
        },
    )
    .await;
    assert_eq!(
        error_code(&outputs[0]),
        "peer_route_unsupported",
        "{:?}",
        outputs[0]
    );
    let text = message(&outputs[0]);
    assert!(text.contains("GET /v1/spend-limits"), "{text}");
    assert!(text.contains("compatible afpay version"), "{text}");
}

#[tokio::test]
async fn a_wrong_credential_names_the_flag_that_carries_it() {
    let peer = start_peer().await;
    let outputs = remote::peer_call(
        &peer.url,
        "0000000000000000000000000000000f",
        &Input::WalletList {
            id: "req_wallets".to_string(),
            network: None,
        },
    )
    .await;
    assert_eq!(
        error_code(&outputs[0]),
        "peer_unauthorized",
        "{:?}",
        outputs[0]
    );
    assert!(
        outputs[0]["hint"]
            .as_str()
            .unwrap()
            .contains("--peer-api-key-secret"),
        "{:?}",
        outputs[0]
    );
    assert!(
        !message(&outputs[0]).contains("0000000000000000000000000000000f"),
        "the refusal echoed the credential"
    );
}

#[tokio::test]
async fn an_unreachable_peer_is_retryable_and_says_where_it_looked() {
    let outputs = remote::peer_call(DEAD_PEER, TOKEN, &Input::Version).await;
    assert_eq!(
        error_code(&outputs[0]),
        "peer_unreachable",
        "{:?}",
        outputs[0]
    );
    assert_eq!(outputs[0]["retryable"], true);
    assert!(
        message(&outputs[0]).contains("127.0.0.1:1"),
        "{:?}",
        outputs[0]
    );
}

/// The version gate, exercised through the path that actually runs it: every
/// long-lived mode validates its configured peers before serving.
#[tokio::test]
async fn startup_refuses_a_peer_running_a_different_afpay_version() {
    let impostor = start_impostor(
        200,
        "application/json",
        r#"{"kind":"result","result":{"service":"afpay","version":"0.0.1-not-this-build","protocol_version":1,"status":"ready"},"trace":{"duration_ms":0}}"#,
    )
    .await;
    let directory = tempfile::tempdir().unwrap();
    let config = RuntimeConfig {
        data_dir: directory.path().to_string_lossy().into_owned(),
        peers: [(
            "ln-server".to_string(),
            PeerConfig {
                url: impostor,
                api_key_secret: Some(TOKEN.to_string()),
            },
        )]
        .into_iter()
        .collect(),
        providers: [("ln".to_string(), "ln-server".to_string())]
            .into_iter()
            .collect(),
        ..RuntimeConfig::default()
    };

    let outputs = agent_first_pay::handler::startup_provider_validation_errors(&config).await;
    assert_eq!(outputs.len(), 1);
    match &outputs[0] {
        Output::Error {
            error_code, error, ..
        } => {
            assert_eq!(error_code, "provider_unreachable");
            assert!(error.contains("ln-server"), "{error}");
            assert!(error.contains("version mismatch"), "{error}");
            assert!(error.contains("0.0.1-not-this-build"), "{error}");
        }
        other => panic!("expected error output, got: {other:?}"),
    }
}

#[tokio::test]
async fn startup_reports_an_unreachable_peer() {
    let directory = tempfile::tempdir().unwrap();
    let config = RuntimeConfig {
        data_dir: directory.path().to_string_lossy().into_owned(),
        peers: [(
            "ln-server".to_string(),
            PeerConfig {
                url: DEAD_PEER.to_string(),
                api_key_secret: Some(TOKEN.to_string()),
            },
        )]
        .into_iter()
        .collect(),
        providers: [("ln".to_string(), "ln-server".to_string())]
            .into_iter()
            .collect(),
        ..RuntimeConfig::default()
    };

    let outputs = agent_first_pay::handler::startup_provider_validation_errors(&config).await;
    assert_eq!(outputs.len(), 1);
    match &outputs[0] {
        Output::Error {
            error_code,
            error,
            retryable,
            ..
        } => {
            assert_eq!(error_code, "provider_unreachable");
            assert!(error.contains("ln-server"), "{error}");
            assert!(*retryable);
        }
        other => panic!("expected error output, got: {other:?}"),
    }
}

#[tokio::test]
async fn startup_names_a_provider_pointed_at_an_unknown_peer() {
    let directory = tempfile::tempdir().unwrap();
    let config = RuntimeConfig {
        data_dir: directory.path().to_string_lossy().into_owned(),
        providers: [("ln".to_string(), "typo-server".to_string())]
            .into_iter()
            .collect(),
        ..RuntimeConfig::default()
    };
    let outputs = agent_first_pay::handler::startup_provider_validation_errors(&config).await;
    assert_eq!(outputs.len(), 1);
    match &outputs[0] {
        Output::Error {
            error_code, error, ..
        } => {
            assert_eq!(error_code, "invalid_config");
            assert!(error.contains("typo-server"), "{error}");
        }
        other => panic!("expected error output, got: {other:?}"),
    }
}

// ═══════════════════════════════════════════
// Wiring: a peer really is used as a provider
// ═══════════════════════════════════════════

/// `providers.<network> = "<peer>"` makes the local dispatcher answer that
/// network's requests from the peer's wallets, over the HTTP leg.
#[tokio::test]
async fn a_configured_peer_serves_a_network_through_the_local_dispatcher() {
    let peer = start_peer().await;
    let directory = tempfile::tempdir().unwrap();
    let config = RuntimeConfig {
        data_dir: directory.path().to_string_lossy().into_owned(),
        peers: [(
            "ln-server".to_string(),
            PeerConfig {
                url: peer.url.clone(),
                api_key_secret: Some(TOKEN.to_string()),
            },
        )]
        .into_iter()
        .collect(),
        providers: [("ln".to_string(), "ln-server".to_string())]
            .into_iter()
            .collect(),
        ..RuntimeConfig::default()
    };

    let (tx, mut rx) = mpsc::channel::<Output>(256);
    let store = agent_first_pay::store::create_storage_backend(&config);
    let app = Arc::new(agent_first_pay::handler::App::new(config, tx, None, store));
    app.requests_total.fetch_add(1, Ordering::Relaxed);

    agent_first_pay::handler::dispatch(
        &app,
        Request::from_input(Input::WalletList {
            id: "req_remote".to_string(),
            network: Some(Network::Ln),
        }),
    )
    .await;
    drop(app);

    let mut outputs = Vec::new();
    while let Some(out) = rx.recv().await {
        outputs.push(serde_json::to_value(&out).unwrap_or(serde_json::Value::Null));
    }
    assert!(!outputs.is_empty());
    let last = outputs.last().unwrap();
    assert_eq!(
        last["code"], "wallet_list",
        "the peer answered the listing: {last:?}"
    );
    assert_eq!(last["wallets"], serde_json::json!([]));
}

// ═══════════════════════════════════════════
// Rendering peer outputs
// ═══════════════════════════════════════════

#[test]
fn emit_remote_outputs_detects_error() {
    let outputs = vec![
        serde_json::json!({"code": "version", "version": "0.1.0", "trace": {"uptime_s": 1, "requests_total": 1, "in_flight": 0}}),
    ];
    let had_error = remote::emit_remote_outputs(
        &outputs,
        agent_first_data::OutputFormat::Json,
        &agent_first_data::LogFilters::new(Vec::<String>::new()),
    );
    assert!(!had_error, "version should not be an error");

    let outputs_with_error = vec![
        serde_json::json!({"code": "error", "error_code": "test", "error": "boom", "retryable": false}),
    ];
    let had_error = remote::emit_remote_outputs(
        &outputs_with_error,
        agent_first_data::OutputFormat::Json,
        &agent_first_data::LogFilters::new(Vec::<String>::new()),
    );
    assert!(had_error, "error output should be detected");
}

#[test]
fn emit_remote_outputs_filters_logs() {
    let outputs = vec![
        serde_json::json!({"code": "log", "event": "startup", "trace": {"duration_ms": 0}}),
        serde_json::json!({"code": "version", "version": "0.1.0", "trace": {"uptime_s": 1, "requests_total": 1, "in_flight": 0}}),
    ];

    let had_error = remote::emit_remote_outputs(
        &outputs,
        agent_first_data::OutputFormat::Json,
        &agent_first_data::LogFilters::new(Vec::<String>::new()),
    );
    assert!(!had_error);

    let had_error = remote::emit_remote_outputs(
        &outputs,
        agent_first_data::OutputFormat::Json,
        &agent_first_data::LogFilters::new(["startup"]),
    );
    assert!(!had_error);
}

/// `limit list` against a peer presents the peer as a downstream node, so a
/// cascading deployment reads as one tree.
#[tokio::test]
async fn limit_list_against_a_peer_renders_it_as_a_downstream_node() {
    let peer = start_peer().await;
    let mut outputs = remote::peer_call(
        &peer.url,
        TOKEN,
        &Input::LimitList {
            id: "req_limits".to_string(),
        },
    )
    .await;
    remote::wrap_remote_limit_topology(&mut outputs, &peer.url);
    assert_eq!(outputs[0]["limits"], serde_json::json!([]));
    assert_eq!(outputs[0]["downstream"][0]["endpoint"], peer.url);
}

// ═══════════════════════════════════════════
// Config
// ═══════════════════════════════════════════

#[test]
fn config_load_from_dir() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    // No config file → defaults
    let cfg = RuntimeConfig::load_from_dir(&dir.path().to_string_lossy()).unwrap();
    assert!(cfg.providers.is_empty());
    assert_eq!(cfg.data_dir, dir.path().to_string_lossy().as_ref());

    std::fs::write(
        &config_path,
        r#"
log = ["cashu"]

[peers.wallet-server]
url = "http://10.0.1.5:9401"
api_key_secret = "my-secret"

[peers.chain-server]
url = "http://10.0.1.6:9401"

[providers]
ln = "wallet-server"
sol = "chain-server"
"#,
    )
    .unwrap();

    let cfg = RuntimeConfig::load_from_dir(&dir.path().to_string_lossy()).unwrap();
    assert_eq!(cfg.peers.len(), 2);
    assert_eq!(cfg.peers["wallet-server"].url, "http://10.0.1.5:9401");
    assert_eq!(
        cfg.peers["wallet-server"].api_key_secret.as_deref(),
        Some("my-secret")
    );
    assert_eq!(cfg.peers["chain-server"].url, "http://10.0.1.6:9401");
    assert!(cfg.peers["chain-server"].api_key_secret.is_none());
    assert_eq!(cfg.providers.len(), 2);
    assert_eq!(cfg.providers["ln"], "wallet-server");
    assert_eq!(cfg.providers["sol"], "chain-server");
    assert_eq!(cfg.log, vec!["cashu"]);
    // data_dir should be set to the provided dir, not from the config file
    assert_eq!(cfg.data_dir, dir.path().to_string_lossy().as_ref());
}

#[tokio::test]
async fn config_update_rejects_unsupported_fields() {
    let dir = tempfile::tempdir().unwrap();
    let config = RuntimeConfig {
        data_dir: dir.path().to_string_lossy().into_owned(),
        ..RuntimeConfig::default()
    };

    let (tx, mut rx) = mpsc::channel::<Output>(64);
    let store = agent_first_pay::store::create_storage_backend(&config);
    let app = Arc::new(agent_first_pay::handler::App::new(config, tx, None, store));
    app.requests_total.fetch_add(1, Ordering::Relaxed);

    agent_first_pay::handler::dispatch(
        &app,
        Request::from_input(Input::ConfigSet {
            id: "t".to_string(),
            key: "data_dir".to_string(),
            values: vec!["/tmp/alt".to_string()],
        }),
    )
    .await;
    drop(app);

    let output = rx.recv().await.expect("config output");
    match output {
        Output::Error {
            error_code, error, ..
        } => {
            assert_eq!(error_code, "invalid_request");
            assert!(error.contains("data_dir"), "got: {error}");
        }
        other => panic!("expected error output, got: {other:?}"),
    }
}

#[tokio::test]
async fn config_update_allows_log() {
    let dir = tempfile::tempdir().unwrap();
    let config = RuntimeConfig {
        data_dir: dir.path().to_string_lossy().into_owned(),
        ..RuntimeConfig::default()
    };

    let (tx, mut rx) = mpsc::channel::<Output>(64);
    let store = agent_first_pay::store::create_storage_backend(&config);
    let app = Arc::new(agent_first_pay::handler::App::new(config, tx, None, store));
    app.requests_total.fetch_add(1, Ordering::Relaxed);

    agent_first_pay::handler::dispatch(
        &app,
        Request::from_input(Input::ConfigSet {
            id: "t".to_string(),
            key: "log".to_string(),
            values: vec!["wallet".to_string(), "pay".to_string()],
        }),
    )
    .await;
    drop(app);

    let output = rx.recv().await.expect("config output");
    match output {
        Output::Config(cfg) => {
            assert_eq!(cfg.log, vec!["wallet", "pay"]);
        }
        other => panic!("expected config output, got: {other:?}"),
    }
}

// ═══════════════════════════════════════════
// Ledger and lock invariants
// ═══════════════════════════════════════════

#[tokio::test]
async fn send_failure_does_not_consume_limit() {
    let dir = tempfile::tempdir().unwrap();
    let config = RuntimeConfig {
        data_dir: dir.path().to_string_lossy().into_owned(),
        ..RuntimeConfig::default()
    };

    let (tx, mut rx) = mpsc::channel::<Output>(256);
    let store = agent_first_pay::store::create_storage_backend(&config);
    let app = Arc::new(agent_first_pay::handler::App::new(config, tx, None, store));
    app.requests_total.fetch_add(1, Ordering::Relaxed);

    agent_first_pay::handler::dispatch(
        &app,
        Request::from_input(Input::LimitSet {
            id: "limit_set".to_string(),
            limits: vec![agent_first_pay::types::SpendLimit {
                rule_id: None,
                scope: agent_first_pay::types::SpendScope::Network,
                network: Some("cashu".to_string()),
                wallet: None,
                window_s: 3600,
                max_spend: 1000,
                token: None,
            }],
        }),
    )
    .await;
    let _ = rx.recv().await.expect("limit_set output");

    agent_first_pay::handler::dispatch(
        &app,
        Request::from_input(Input::CashuSendPlan {
            id: "send_fail".to_string(),
            wallet: None,
            amount: agent_first_pay::types::Amount {
                value: 500,
                token: "sats".to_string(),
            },
            onchain_memo: None,
            local_memo: None,
            mints: None,
        }),
    )
    .await;
    let send_out = rx.recv().await.expect("send output");
    assert!(
        matches!(send_out, Output::Error { .. }),
        "expected send to fail without wallets"
    );

    agent_first_pay::handler::dispatch(
        &app,
        Request::from_input(Input::LimitList {
            id: "limit_get".to_string(),
        }),
    )
    .await;
    drop(app);

    let out = rx.recv().await.expect("limit_get output");
    match out {
        Output::LimitStatus { limits, .. } => {
            assert_eq!(limits.len(), 1);
            assert_eq!(limits[0].spent, 0);
            assert_eq!(limits[0].remaining, 1000);
        }
        other => panic!("expected limit status, got: {other:?}"),
    }
}

/// Per-operation lock: acquire, verify re-acquire times out, release,
/// re-acquire succeeds.
#[test]
fn lock_per_operation() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_string_lossy().into_owned();

    let guard = agent_first_pay::store::lock::acquire(&data_dir, None).unwrap();

    let result = agent_first_pay::store::lock::acquire(&data_dir, Some(200));
    assert!(result.is_err(), "second lock should timeout");
    let err = result.unwrap_err();
    assert!(
        err.contains("timeout"),
        "error should mention timeout, got: {err}"
    );

    drop(guard);

    let guard2 = agent_first_pay::store::lock::acquire(&data_dir, Some(200));
    assert!(guard2.is_ok(), "lock after release should succeed");
}
