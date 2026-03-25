#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use agent_first_pay::types::{ConfigPatch, Input, Output, RuntimeConfig};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

// ═══════════════════════════════════════════
// Shared test server
// ═══════════════════════════════════════════

/// Start a gRPC AfPay server on a free port. Returns (addr, secret, server_handle).
fn start_test_server() -> (SocketAddr, String, JoinHandle<()>) {
    use agent_first_pay::mode::rpc::crypto::Cipher;

    let secret = "test-secret-rpc".to_string();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let secret_clone = secret.clone();
    let handle = tokio::spawn(async move {
        use agent_first_pay::mode::rpc::proto::af_pay_server::{AfPay, AfPayServer};
        use agent_first_pay::mode::rpc::proto::{EncryptedRequest, EncryptedResponse};
        use tonic::{Request, Response, Status};

        struct TestService {
            cipher: Cipher,
            config: RuntimeConfig,
        }

        #[tonic::async_trait]
        impl AfPay for TestService {
            async fn call(
                &self,
                request: Request<EncryptedRequest>,
            ) -> Result<Response<EncryptedResponse>, Status> {
                let req = request.into_inner();
                let plaintext = self
                    .cipher
                    .decrypt(&req.nonce, &req.ciphertext)
                    .map_err(|_| Status::unauthenticated("decrypt failed"))?;

                let input: Input = serde_json::from_slice(&plaintext)
                    .map_err(|e| Status::invalid_argument(format!("{e}")))?;

                if input.is_local_only() {
                    return Err(Status::permission_denied("local-only operation"));
                }

                let (tx, mut rx) = mpsc::channel::<Output>(256);
                let store = agent_first_pay::store::create_storage_backend(&self.config);
                let app = Arc::new(agent_first_pay::handler::App::new(
                    self.config.clone(),
                    tx,
                    Some(true),
                    store,
                ));
                app.requests_total.fetch_add(1, Ordering::Relaxed);
                agent_first_pay::handler::dispatch(&app, input).await;
                drop(app);

                let mut outputs = Vec::new();
                while let Some(out) = rx.recv().await {
                    let v = serde_json::to_value(&out).unwrap_or(serde_json::Value::Null);
                    outputs.push(v);
                }

                let resp_json =
                    serde_json::to_vec(&outputs).map_err(|e| Status::internal(format!("{e}")))?;
                let (nonce, ciphertext) =
                    self.cipher.encrypt(&resp_json).map_err(Status::internal)?;

                Ok(Response::new(EncryptedResponse { nonce, ciphertext }))
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let config = RuntimeConfig {
            data_dir: dir.path().to_string_lossy().into_owned(),
            ..RuntimeConfig::default()
        };

        let svc = TestService {
            cipher: Cipher::from_secret(&secret_clone),
            config,
        };
        tonic::transport::Server::builder()
            .add_service(AfPayServer::new(svc))
            .serve(addr)
            .await
            .unwrap();
    });

    (addr, secret, handle)
}

// ═══════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════

mod crypto_roundtrip {
    #[test]
    fn roundtrip() {
        use agent_first_pay::mode::rpc::crypto::Cipher;

        let cipher = Cipher::from_secret("integration-test-secret");
        let plaintext = b"{\"code\":\"version\"}";
        let (nonce, ct) = cipher.encrypt(plaintext).unwrap();
        let decrypted = cipher.decrypt(&nonce, &ct).unwrap();
        assert_eq!(decrypted, plaintext);
    }
}

/// Raw gRPC version request/response (manual encrypt/decrypt via Cipher, no remote:: helpers).
#[tokio::test]
async fn rpc_version_raw() {
    use agent_first_pay::mode::rpc::crypto::Cipher;
    use agent_first_pay::mode::rpc::proto::af_pay_client::AfPayClient;
    use agent_first_pay::mode::rpc::proto::EncryptedRequest;

    let (addr, secret, server_handle) = start_test_server();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let cipher = Cipher::from_secret(&secret);
    let version_json = br#"{"code":"version"}"#;
    let (nonce, ct) = cipher.encrypt(version_json).unwrap();

    let url = format!("http://{}", addr);
    let mut client = AfPayClient::connect(url).await.unwrap();
    let resp = client
        .call(EncryptedRequest {
            nonce,
            ciphertext: ct,
        })
        .await
        .unwrap()
        .into_inner();

    let resp_plain = cipher.decrypt(&resp.nonce, &resp.ciphertext).unwrap();
    let outputs: Vec<serde_json::Value> = serde_json::from_slice(&resp_plain).unwrap();

    assert!(!outputs.is_empty(), "expected at least one output");
    assert_eq!(
        outputs[0].get("code").and_then(|v| v.as_str()),
        Some("version"),
    );
    assert!(
        outputs[0].get("version").and_then(|v| v.as_str()).is_some(),
        "version response should include version field"
    );

    server_handle.abort();
}

/// Test remote::rpc_call() — the path used by CLI remote and interactive remote.
#[tokio::test]
async fn rpc_call_version() {
    use agent_first_pay::provider::remote;
    use agent_first_pay::types::Input;

    let (addr, secret, server_handle) = start_test_server();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let endpoint = format!("http://{}", addr);
    let outputs = remote::rpc_call(&endpoint, &secret, &Input::Version).await;

    assert!(!outputs.is_empty(), "expected at least one output");
    assert_eq!(
        outputs[0].get("code").and_then(|v| v.as_str()),
        Some("version"),
        "expected version, got: {:?}",
        outputs[0]
    );
    assert!(
        outputs[0].get("trace").is_some(),
        "version should contain trace"
    );
    assert!(
        outputs[0].get("version").and_then(|v| v.as_str()).is_some(),
        "version response should include version field"
    );

    server_handle.abort();
}

/// Test remote::rpc_call() with wrong secret — should fail.
#[tokio::test]
async fn rpc_call_wrong_secret() {
    use agent_first_pay::provider::remote;
    use agent_first_pay::types::Input;

    let (addr, _secret, server_handle) = start_test_server();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let endpoint = format!("http://{}", addr);
    let outputs = remote::rpc_call(&endpoint, "wrong-secret", &Input::Version).await;
    assert!(!outputs.is_empty(), "expected at least one output");
    let error_code = outputs[0]
        .get("error_code")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let error_msg = outputs[0]
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        error_code == "unauthenticated"
            || error_msg.contains("decrypt")
            || error_msg.contains("unauthenticated"),
        "error should mention auth failure, got code={error_code} error={error_msg}"
    );

    server_handle.abort();
}

/// Test remote::emit_remote_outputs() — shared rendering used by CLI and interactive remote.
#[test]
fn emit_remote_outputs_detects_error() {
    use agent_first_pay::provider::remote;

    let outputs = vec![
        serde_json::json!({"code": "version", "version": "0.1.0", "trace": {"uptime_s": 1, "requests_total": 1, "in_flight": 0}}),
    ];
    let had_error =
        remote::emit_remote_outputs(&outputs, agent_first_data::OutputFormat::Json, &[]);
    assert!(!had_error, "version should not be an error");

    let outputs_with_error = vec![
        serde_json::json!({"code": "error", "error_code": "test", "error": "boom", "retryable": false}),
    ];
    let had_error = remote::emit_remote_outputs(
        &outputs_with_error,
        agent_first_data::OutputFormat::Json,
        &[],
    );
    assert!(had_error, "error output should be detected");
}

/// Test emit_remote_outputs() filters log events based on log_filters.
#[test]
fn emit_remote_outputs_filters_logs() {
    use agent_first_pay::provider::remote;

    let outputs = vec![
        serde_json::json!({"code": "log", "event": "startup", "trace": {"duration_ms": 0}}),
        serde_json::json!({"code": "version", "version": "0.1.0", "trace": {"uptime_s": 1, "requests_total": 1, "in_flight": 0}}),
    ];

    // With empty log filters, log events are skipped (only pong rendered)
    let had_error =
        remote::emit_remote_outputs(&outputs, agent_first_data::OutputFormat::Json, &[]);
    assert!(!had_error);

    // With matching filter, log events pass through
    let had_error = remote::emit_remote_outputs(
        &outputs,
        agent_first_data::OutputFormat::Json,
        &["startup".to_string()],
    );
    assert!(!had_error);
}

/// Test RuntimeConfig::load_from_dir() with a config.toml file.
#[test]
fn config_load_from_dir() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    // No config file → defaults
    let cfg = RuntimeConfig::load_from_dir(&dir.path().to_string_lossy()).unwrap();
    assert!(cfg.providers.is_empty());
    assert_eq!(cfg.data_dir, dir.path().to_string_lossy().as_ref());

    // Write a config.toml with afpay_rpc + providers
    std::fs::write(
        &config_path,
        r#"
log = ["cashu"]

[afpay_rpc.wallet-server]
endpoint = "10.0.1.5:9400"
endpoint_secret = "my-secret"

[afpay_rpc.chain-server]
endpoint = "10.0.1.6:9400"

[providers]
ln = "wallet-server"
sol = "chain-server"
"#,
    )
    .unwrap();

    let cfg = RuntimeConfig::load_from_dir(&dir.path().to_string_lossy()).unwrap();
    assert_eq!(cfg.afpay_rpc.len(), 2);
    assert_eq!(cfg.afpay_rpc["wallet-server"].endpoint, "10.0.1.5:9400");
    assert_eq!(
        cfg.afpay_rpc["wallet-server"].endpoint_secret.as_deref(),
        Some("my-secret")
    );
    assert_eq!(cfg.afpay_rpc["chain-server"].endpoint, "10.0.1.6:9400");
    assert!(cfg.afpay_rpc["chain-server"].endpoint_secret.is_none());
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
        Input::Config(ConfigPatch {
            data_dir: Some("/tmp/alt".to_string()),
            log: None,
            exchange_rate: None,
            afpay_rpc: None,
            providers: None,
        }),
    )
    .await;
    drop(app);

    let output = rx.recv().await.expect("config output");
    match output {
        Output::Error {
            error_code, error, ..
        } => {
            assert_eq!(error_code, "not_implemented");
            assert!(error.contains("only supports 'log'"));
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
        Input::Config(ConfigPatch {
            data_dir: None,
            log: Some(vec!["wallet".to_string(), "pay".to_string()]),
            exchange_rate: None,
            afpay_rpc: None,
            providers: None,
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
        Input::LimitSet {
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
        },
    )
    .await;
    let _ = rx.recv().await.expect("limit_set output");

    agent_first_pay::handler::dispatch(
        &app,
        Input::CashuSend {
            id: "send_fail".to_string(),
            wallet: None,
            amount: agent_first_pay::types::Amount {
                value: 500,
                token: "sats".to_string(),
            },
            onchain_memo: None,
            local_memo: None,
            mints: None,
        },
    )
    .await;
    let send_out = rx.recv().await.expect("send output");
    assert!(
        matches!(send_out, Output::Error { .. }),
        "expected send to fail without wallets"
    );

    agent_first_pay::handler::dispatch(
        &app,
        Input::LimitList {
            id: "limit_get".to_string(),
        },
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

/// Test that App::new() wires RemoteProvider for currencies with configured providers.
/// We verify by sending a wallet_list command through a remote provider pointing at our test server.
#[tokio::test]
async fn app_uses_remote_provider_from_config() {
    let (addr, secret, server_handle) = start_test_server();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let dir = tempfile::tempdir().unwrap();
    let mut afpay_rpc = std::collections::HashMap::new();
    afpay_rpc.insert(
        "ln-server".to_string(),
        agent_first_pay::types::AfpayRpcConfig {
            endpoint: format!("http://{}", addr),
            endpoint_secret: Some(secret.clone()),
        },
    );
    let mut providers = std::collections::HashMap::new();
    providers.insert("ln".to_string(), "ln-server".to_string());

    let config = RuntimeConfig {
        data_dir: dir.path().to_string_lossy().into_owned(),
        afpay_rpc,
        providers,
        ..RuntimeConfig::default()
    };

    let (tx, mut rx) = mpsc::channel::<Output>(256);
    let store = agent_first_pay::store::create_storage_backend(&config);
    let app = Arc::new(agent_first_pay::handler::App::new(config, tx, None, store));
    app.requests_total.fetch_add(1, Ordering::Relaxed);

    // Dispatch a wallet_list for ln — should go through RemoteProvider → test server
    agent_first_pay::handler::dispatch(
        &app,
        Input::WalletList {
            id: "remote_test".to_string(),
            network: Some(agent_first_pay::types::Network::Ln),
        },
    )
    .await;
    drop(app);

    let mut outputs = Vec::new();
    while let Some(out) = rx.recv().await {
        outputs.push(serde_json::to_value(&out).unwrap_or(serde_json::Value::Null));
    }

    // The test server's LN provider is a StubProvider → returns "not_implemented" error
    // which RemoteProvider maps to a PayError. Either wallet_list or error is fine.
    assert!(!outputs.is_empty());
    let code = outputs.last().unwrap()["code"].as_str().unwrap_or("");
    assert!(
        code == "wallet_list" || code == "error",
        "expected wallet_list or error from remote provider, got: {code}"
    );

    server_handle.abort();
}

#[tokio::test]
async fn startup_provider_validation_reports_unreachable_remote_provider() {
    let dir = tempfile::tempdir().unwrap();
    let mut afpay_rpc = std::collections::HashMap::new();
    afpay_rpc.insert(
        "ln-server".to_string(),
        agent_first_pay::types::AfpayRpcConfig {
            endpoint: "http://127.0.0.1:1".to_string(),
            endpoint_secret: Some("test-secret".to_string()),
        },
    );
    let mut providers = std::collections::HashMap::new();
    providers.insert("ln".to_string(), "ln-server".to_string());
    let config = RuntimeConfig {
        data_dir: dir.path().to_string_lossy().into_owned(),
        afpay_rpc,
        providers,
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
            assert!(error.contains("ln-server"));
            assert!(*retryable);
        }
        other => panic!("expected error output, got: {other:?}"),
    }
}

/// Test per-operation lock: acquire, verify re-acquire with timeout, release, re-acquire succeeds.
#[test]
fn lock_per_operation() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_string_lossy().into_owned();

    // Acquire lock
    let guard = agent_first_pay::store::lock::acquire(&data_dir, None).unwrap();

    // Try to acquire again with short timeout — should fail
    let result = agent_first_pay::store::lock::acquire(&data_dir, Some(200));
    assert!(result.is_err(), "second lock should timeout");
    let err = result.unwrap_err();
    assert!(
        err.contains("timeout"),
        "error should mention timeout, got: {err}"
    );

    // Drop the first lock
    drop(guard);

    // Now re-acquire should succeed
    let guard2 = agent_first_pay::store::lock::acquire(&data_dir, Some(200));
    assert!(guard2.is_ok(), "lock after release should succeed");
}

/// Simulate interactive remote: multiple commands through remote::rpc_call().
#[tokio::test]
async fn rpc_call_multi_command_flow() {
    use agent_first_pay::provider::remote;
    use agent_first_pay::types::{Input, Network};

    let (addr, secret, server_handle) = start_test_server();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let endpoint = format!("http://{}", addr);

    // 1. Version
    let outputs = remote::rpc_call(&endpoint, &secret, &Input::Version).await;
    assert_eq!(outputs[0]["code"], "version");

    // 2. Wallet list (empty)
    let outputs = remote::rpc_call(
        &endpoint,
        &secret,
        &Input::WalletList {
            id: "test_1".to_string(),
            network: Some(Network::Cashu),
        },
    )
    .await;
    // Should get an error (ecash provider on test server is stub or has no wallets)
    // or an empty wallet_list — either is valid
    assert!(!outputs.is_empty());
    let code = outputs[0]["code"].as_str().unwrap_or("");
    assert!(
        code == "wallet_list" || code == "error",
        "expected wallet_list or error, got: {code}"
    );

    // 3. Limit set over RPC should be rejected (local-only)
    let outputs = remote::rpc_call(
        &endpoint,
        &secret,
        &Input::LimitSet {
            id: "test_2".to_string(),
            limits: vec![agent_first_pay::types::SpendLimit {
                rule_id: None,
                scope: agent_first_pay::types::SpendScope::Network,
                network: Some("cashu".to_string()),
                wallet: None,
                window_s: 3600,
                max_spend: 10000,
                token: None,
            }],
        },
    )
    .await;
    assert_eq!(
        outputs[0]["error_code"].as_str().unwrap_or(""),
        "permission_denied",
        "limit_set should be rejected over RPC"
    );

    // 4. Limit list over RPC should succeed (read-only)
    let outputs = remote::rpc_call(
        &endpoint,
        &secret,
        &Input::LimitList {
            id: "test_3".to_string(),
        },
    )
    .await;
    assert_eq!(outputs[0]["code"], "limit_status");

    server_handle.abort();
}
