#![cfg(all(feature = "redb", any(feature = "sol", feature = "evm")))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use agent_first_pay::provider::{PayError, PayProvider};
use agent_first_pay::store::{StorageBackend, redb_store::RedbStore};
use agent_first_pay::types::WalletCreateRequest;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct RpcStub {
    endpoint: String,
    requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for RpcStub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl RpcStub {
    async fn start(answer: impl Fn(&Value) -> Option<Value> + Send + Sync + 'static) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let task_requests = requests.clone();
        let answer = Arc::new(answer);
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let requests = task_requests.clone();
                let answer = answer.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let header_end = loop {
                        let count = stream.read(&mut chunk).await.unwrap();
                        if count == 0 {
                            return;
                        }
                        request.extend_from_slice(&chunk[..count]);
                        if let Some(pos) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                            break pos + 4;
                        }
                    };
                    let length = String::from_utf8_lossy(&request[..header_end])
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    while request.len() < header_end + length {
                        let count = stream.read(&mut chunk).await.unwrap();
                        if count == 0 {
                            return;
                        }
                        request.extend_from_slice(&chunk[..count]);
                    }
                    let request: Value =
                        serde_json::from_slice(&request[header_end..header_end + length]).unwrap();
                    requests.lock().unwrap().push(request.clone());
                    // A missing answer closes the connection after the full
                    // submission was received, without revealing its outcome.
                    if let Some(answer) = answer(&request) {
                        let body = answer.to_string();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        stream.write_all(response.as_bytes()).await.unwrap();
                    }
                });
            }
        });
        Self {
            endpoint,
            requests,
            task,
        }
    }
}

/// Only a guard against a hung submission: the assertions are about which
/// requests were sent and with which bytes, never about speed. A send derives
/// keys and signs in a debug build, which on a loaded or emulated host takes
/// seconds on its own.
const SUBMISSION_HANG_GUARD: std::time::Duration = std::time::Duration::from_secs(30);

fn wallet_request(endpoints: Vec<String>) -> WalletCreateRequest {
    WalletCreateRequest {
        label: "submission-fixture".into(),
        mint_url: None,
        rpc_endpoints: endpoints,
        chain_id: Some(8453),
        mnemonic_secret: Some("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".into()),
        btc_esplora_url: None,
        btc_network: None,
        btc_address_type: None,
        btc_backend: None,
        btc_core_url: None,
        btc_core_auth_secret: None,
        btc_electrum_url: None,
        sol_cluster: None,
    }
}

#[cfg(feature = "sol")]
fn sol_signature_of(request: &Value) -> String {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(request["params"][0].as_str().unwrap())
        .unwrap();
    let transaction: solana_sdk::transaction::Transaction = wincode::deserialize(&bytes).unwrap();
    transaction.signatures[0].to_string()
}

#[cfg(feature = "sol")]
fn sol_methods(requests: &[Value]) -> Vec<String> {
    requests
        .iter()
        .map(|request| request["method"].as_str().unwrap().to_string())
        .collect()
}

#[cfg(feature = "sol")]
async fn sol_send(endpoints: Vec<String>) -> Result<agent_first_pay::types::SendResult, PayError> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    let provider = agent_first_pay::provider::sol::SolProvider::new(
        path,
        Arc::new(StorageBackend::Redb(RedbStore::new(path))),
    );
    let wallet = provider
        .create_wallet(&wallet_request(endpoints))
        .await
        .unwrap();
    tokio::time::timeout(
        SUBMISSION_HANG_GUARD,
        provider.send(
            &wallet.id,
            &format!("solana:{}?amount=1000&token=native", wallet.address),
            None,
            None,
        ),
    )
    .await
    .expect("submission must finish")
}

#[cfg(feature = "sol")]
#[tokio::test]
async fn sol_response_loss_rebroadcasts_identical_bytes_and_never_resigns() {
    let first = RpcStub::start(|request| {
        match request["method"].as_str().unwrap() {
            "getLatestBlockhash" => Some(json!({"jsonrpc":"2.0", "id":request["id"], "result":{"value":{"blockhash":"11111111111111111111111111111111"}}})),
            "sendTransaction" => None,
            method => panic!("unexpected sol RPC: {method}"),
        }
    }).await;
    // The fallback only ever sees the already signed transaction: fetching a
    // new blockhash here would mean a second signature, and a second payment.
    let second = RpcStub::start(|request| match request["method"].as_str().unwrap() {
        "sendTransaction" => None,
        method => panic!("fallback must not issue {method} after a broadcast"),
    })
    .await;
    let error = sol_send(vec![first.endpoint.clone(), second.endpoint.clone()])
        .await
        .unwrap_err();
    assert!(matches!(&error, PayError::SubmissionUnknown { .. }));
    assert!(!error.retryable());
    let first_requests = first.requests.lock().unwrap();
    let second_requests = second.requests.lock().unwrap();
    assert_eq!(
        sol_methods(&first_requests),
        ["getLatestBlockhash", "sendTransaction"]
    );
    assert_eq!(sol_methods(&second_requests), ["sendTransaction"]);
    assert_eq!(
        first_requests[1]["params"][0], second_requests[0]["params"][0],
        "both endpoints must receive the same signed bytes"
    );
    assert_eq!(
        error.submission_transaction_id(),
        Some(sol_signature_of(&first_requests[1]).as_str())
    );
}

#[cfg(feature = "sol")]
#[tokio::test]
async fn sol_fallback_endpoint_can_confirm_the_same_signed_transaction() {
    let first = RpcStub::start(|request| {
        match request["method"].as_str().unwrap() {
            "getLatestBlockhash" => Some(json!({"jsonrpc":"2.0", "id":request["id"], "result":{"value":{"blockhash":"11111111111111111111111111111111"}}})),
            "sendTransaction" => None,
            "getTransaction" => Some(json!({"jsonrpc":"2.0", "id":request["id"], "error":{"code":-32004, "message":"not found"}})),
            method => panic!("unexpected sol RPC: {method}"),
        }
    }).await;
    let second = RpcStub::start(|request| match request["method"].as_str().unwrap() {
        "sendTransaction" => Some(
            json!({"jsonrpc":"2.0", "id":request["id"], "result": sol_signature_of(request)}),
        ),
        "getTransaction" => Some(
            json!({"jsonrpc":"2.0", "id":request["id"], "error":{"code":-32004, "message":"not found"}}),
        ),
        method => panic!("fallback must not issue {method} after a broadcast"),
    })
    .await;
    let result = sol_send(vec![first.endpoint.clone(), second.endpoint.clone()])
        .await
        .unwrap();
    let first_requests = first.requests.lock().unwrap();
    let second_requests = second.requests.lock().unwrap();
    let submissions: Vec<&Value> = first_requests
        .iter()
        .chain(second_requests.iter())
        .filter(|request| request["method"] == "sendTransaction")
        .collect();
    assert_eq!(submissions.len(), 2);
    assert_eq!(submissions[0]["params"][0], submissions[1]["params"][0]);
    assert_eq!(result.transaction_id, sol_signature_of(submissions[0]));
    assert_eq!(
        sol_methods(&second_requests)
            .iter()
            .filter(|method| *method == "getLatestBlockhash")
            .count(),
        0
    );
}

#[cfg(feature = "evm")]
fn evm_raw_hash(request: &Value) -> String {
    let raw = request["params"][0].as_str().unwrap();
    let bytes = hex::decode(raw.trim_start_matches("0x")).unwrap();
    format!("{:?}", alloy::primitives::keccak256(bytes))
}

#[cfg(feature = "evm")]
fn evm_fill_answer(request: &Value) -> Option<Value> {
    let result = match request["method"].as_str().unwrap() {
        "eth_chainId" => json!("0x2105"),
        "eth_getTransactionCount" => json!("0x0"),
        "eth_estimateGas" => json!("0x5208"),
        "eth_gasPrice" | "eth_maxPriorityFeePerGas" => json!("0x3b9aca00"),
        "eth_feeHistory" => {
            json!({"oldestBlock":"0x0", "baseFeePerGas":["0x3b9aca00","0x3b9aca00"], "gasUsedRatio":[0.5], "reward":[["0x3b9aca00"]]})
        }
        "eth_getTransactionReceipt" => Value::Null,
        "eth_sendRawTransaction" => return None,
        method => panic!("unexpected evm RPC: {method}"),
    };
    Some(json!({"jsonrpc":"2.0", "id":request["id"], "result":result}))
}

#[cfg(feature = "evm")]
fn evm_count(requests: &[Value], method: &str) -> usize {
    requests
        .iter()
        .filter(|request| request["method"] == method)
        .count()
}

#[cfg(feature = "evm")]
async fn evm_send(endpoints: Vec<String>) -> Result<agent_first_pay::types::SendResult, PayError> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    let provider = agent_first_pay::provider::evm::EvmProvider::new(
        path,
        Arc::new(StorageBackend::Redb(RedbStore::new(path))),
    );
    let wallet = provider
        .create_wallet(&wallet_request(endpoints))
        .await
        .unwrap();
    tokio::time::timeout(
        SUBMISSION_HANG_GUARD,
        provider.send(
            &wallet.id,
            &format!("ethereum:{}?amount-wei=1000", wallet.address),
            None,
            None,
        ),
    )
    .await
    .expect("submission must finish")
}

#[cfg(feature = "evm")]
#[tokio::test]
async fn evm_response_loss_rebroadcasts_identical_bytes_and_never_refills_nonce() {
    let first = RpcStub::start(evm_fill_answer).await;
    // The fallback only ever sees the raw signed transaction: filling a nonce
    // here would mean a second transaction, and a second payment.
    let second = RpcStub::start(|request| match request["method"].as_str().unwrap() {
        "eth_sendRawTransaction" => None,
        method => panic!("fallback must not issue {method} after a broadcast"),
    })
    .await;
    let error = evm_send(vec![first.endpoint.clone(), second.endpoint.clone()])
        .await
        .unwrap_err();
    assert!(matches!(&error, PayError::SubmissionUnknown { .. }));
    assert!(!error.retryable());
    let first_requests = first.requests.lock().unwrap();
    let second_requests = second.requests.lock().unwrap();
    assert_eq!(evm_count(&first_requests, "eth_sendRawTransaction"), 1);
    assert_eq!(evm_count(&first_requests, "eth_getTransactionCount"), 1);
    assert_eq!(second_requests.len(), 1);
    assert_eq!(second_requests[0]["method"], "eth_sendRawTransaction");
    let first_raw = first_requests
        .iter()
        .find(|request| request["method"] == "eth_sendRawTransaction")
        .unwrap();
    assert_eq!(
        first_raw["params"][0], second_requests[0]["params"][0],
        "both endpoints must receive the same raw transaction"
    );
    assert_eq!(
        error.submission_transaction_id(),
        Some(evm_raw_hash(first_raw).as_str())
    );
}

#[cfg(feature = "evm")]
#[tokio::test]
async fn evm_fallback_endpoint_can_confirm_the_same_raw_transaction() {
    let first = RpcStub::start(evm_fill_answer).await;
    let second = RpcStub::start(|request| match request["method"].as_str().unwrap() {
        "eth_sendRawTransaction" => {
            Some(json!({"jsonrpc":"2.0", "id":request["id"], "result": evm_raw_hash(request)}))
        }
        "eth_getTransactionReceipt" => {
            Some(json!({"jsonrpc":"2.0", "id":request["id"], "result": Value::Null}))
        }
        method => panic!("fallback must not issue {method} after a broadcast"),
    })
    .await;
    let result = evm_send(vec![first.endpoint.clone(), second.endpoint.clone()])
        .await
        .unwrap();
    let first_requests = first.requests.lock().unwrap();
    let second_requests = second.requests.lock().unwrap();
    assert_eq!(evm_count(&first_requests, "eth_sendRawTransaction"), 1);
    assert_eq!(evm_count(&second_requests, "eth_sendRawTransaction"), 1);
    assert_eq!(evm_count(&second_requests, "eth_getTransactionCount"), 0);
    let first_raw = first_requests
        .iter()
        .find(|request| request["method"] == "eth_sendRawTransaction")
        .unwrap();
    let second_raw = second_requests
        .iter()
        .find(|request| request["method"] == "eth_sendRawTransaction")
        .unwrap();
    assert_eq!(first_raw["params"][0], second_raw["params"][0]);
    assert_eq!(result.transaction_id, evm_raw_hash(first_raw));
}

#[cfg(feature = "evm")]
#[tokio::test]
async fn evm_quote_rejects_bad_gas_instead_of_pricing_it_as_zero() {
    for bad_method in ["eth_gasPrice", "eth_estimateGas"] {
        for rpc_error in [false, true] {
            let bad = RpcStub::start(move |request| {
                if request["method"] == bad_method {
                    Some(if rpc_error {
                        json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":"unavailable"}})
                    } else {
                        json!({"jsonrpc":"2.0","id":request["id"],"result":"not-hex"})
                    })
                } else { evm_fill_answer(request) }
            }).await;
            let good = RpcStub::start(evm_fill_answer).await;
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().to_str().unwrap();
            let provider = agent_first_pay::provider::evm::EvmProvider::new(
                path,
                Arc::new(StorageBackend::Redb(RedbStore::new(path))),
            );
            let wallet = provider
                .create_wallet(&wallet_request(vec![bad.endpoint.clone()]))
                .await
                .unwrap();
            let target = format!("ethereum:{}?amount-wei=1000000000", wallet.address);
            assert!(matches!(
                provider.send_quote(&wallet.id, &target, None).await,
                Err(PayError::NetworkError { .. })
            ));
            #[cfg(feature = "rest")]
            {
                use tower::ServiceExt;
                let token = "0123456789abcdef0123456789abcdef";
                let config = agent_first_pay::types::RuntimeConfig {
                    data_dir: path.to_owned(),
                    ..Default::default()
                };
                let router = agent_first_pay::api::router(agent_first_pay::api::ApiState::new(
                    config,
                    token,
                    Vec::new(),
                ));
                let request = axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/send-plans")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        json!({"wallet":wallet.id,"network":"evm","to":target}).to_string(),
                    ))
                    .unwrap();
                let response = router.oneshot(request).await.unwrap();
                assert!(!response.status().is_success());
                let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
                    .await
                    .unwrap();
                let event: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(event["error"]["code"], "network_error");
                assert!(event["result"]["plan_id"].is_null());
                let plans = dir.path().join("pay-plans");
                if plans.exists() {
                    assert_eq!(std::fs::read_dir(plans).unwrap().count(), 0);
                }
            }
            let wallet = provider
                .create_wallet(&wallet_request(vec![
                    bad.endpoint.clone(),
                    good.endpoint.clone(),
                ]))
                .await
                .unwrap();
            let quote = provider
                .send_quote(&wallet.id, &target, None)
                .await
                .unwrap();
            assert_eq!(quote.fee_estimate_native, 21_000);
            assert!(
                quote
                    .spend_debits
                    .iter()
                    .all(|debit| debit.amount_native > 0)
            );
        }
    }
}

#[cfg(all(feature = "evm", feature = "sol", feature = "rest"))]
#[tokio::test]
async fn preparation_failure_releases_plan_budget_and_idempotency_for_retry() {
    use agent_first_pay::spend::SpendLedger;
    use agent_first_pay::types::{RuntimeConfig, SpendLimit, SpendScope};
    use std::sync::atomic::{AtomicBool, Ordering};
    use tower::ServiceExt;
    for network in ["evm", "sol"] {
        let unavailable = Arc::new(AtomicBool::new(false));
        let phase = unavailable.clone();
        let stub = RpcStub::start(move |request| {
            let method = request["method"].as_str().unwrap();
            if network == "evm" {
                if method == "eth_getTransactionCount" && phase.load(Ordering::SeqCst) {
                    return None;
                }
                if method == "eth_sendRawTransaction" {
                    return Some(
                        json!({"jsonrpc":"2.0","id":request["id"],"result":evm_raw_hash(request)}),
                    );
                }
                evm_fill_answer(request)
            } else {
                let result = match method {
                    "getLatestBlockhash" if phase.load(Ordering::SeqCst) => return None,
                    "getLatestBlockhash" => {
                        json!({"value":{"blockhash":"11111111111111111111111111111111"}})
                    }
                    "sendTransaction" => json!(sol_signature_of(request)),
                    "getTransaction" => Value::Null,
                    other => panic!("unexpected Solana RPC {other}"),
                };
                Some(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
            }
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        let store = Arc::new(StorageBackend::Redb(RedbStore::new(path)));
        let provider: Arc<dyn PayProvider> = if network == "evm" {
            Arc::new(agent_first_pay::provider::evm::EvmProvider::new(
                path, store,
            ))
        } else {
            Arc::new(agent_first_pay::provider::sol::SolProvider::new(
                path, store,
            ))
        };
        let wallet = provider
            .create_wallet(&wallet_request(vec![stub.endpoint.clone()]))
            .await
            .unwrap();
        let target = if network == "evm" {
            format!("ethereum:{}?amount-wei=1000000000", wallet.address)
        } else {
            format!("solana:{}?amount=1000&token=native", wallet.address)
        };
        let quote = provider
            .send_quote(&wallet.id, &target, None)
            .await
            .unwrap();
        let limit = quote
            .spend_debits
            .iter()
            .map(|debit| debit.amount_native)
            .sum();
        let ledger = SpendLedger::new(path, None);
        ledger
            .set_limits(&[SpendLimit {
                rule_id: None,
                scope: SpendScope::Network,
                network: Some(network.to_owned()),
                wallet: None,
                window_s: 3600,
                max_spend: limit,
                token: None,
            }])
            .await
            .unwrap();
        drop(ledger);
        let token = "0123456789abcdef0123456789abcdef";
        let router = agent_first_pay::api::router(agent_first_pay::api::ApiState::new(
            RuntimeConfig {
                data_dir: path.to_owned(),
                ..Default::default()
            },
            token,
            Vec::new(),
        ));
        let request = |route: &str, body: Value, key: Option<&str>| {
            let mut builder = axum::http::Request::builder()
                .method("POST")
                .uri(route)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json");
            if let Some(key) = key {
                builder = builder.header("idempotency-key", key);
            }
            builder
                .body(axum::body::Body::from(body.to_string()))
                .unwrap()
        };
        let response = router
            .clone()
            .oneshot(request(
                "/v1/send-plans",
                json!({"wallet":wallet.id,"network":network,"to":target}),
                None,
            ))
            .await
            .unwrap();
        assert!(response.status().is_success());
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let plan: Value = serde_json::from_slice(&bytes).unwrap();
        let body = json!({"plan_id":plan["result"]["plan_id"]});
        assert!(body["plan_id"].is_string());
        unavailable.store(true, Ordering::SeqCst);
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            router.clone().oneshot(request(
                "/v1/sends",
                body.clone(),
                Some("preparation-retry"),
            )),
        )
        .await
        .unwrap()
        .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let failed: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(failed["error"]["code"], "network_error", "{failed}");
        assert_eq!(failed["error"]["retryable"], true);
        assert!(
            stub.requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request["method"] != "eth_sendRawTransaction"
                    && request["method"] != "sendTransaction")
        );
        unavailable.store(false, Ordering::SeqCst);
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            router.clone().oneshot(request(
                "/v1/sends",
                body.clone(),
                Some("preparation-retry"),
            )),
        )
        .await
        .unwrap()
        .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let sent: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(sent["kind"], "result", "{sent}");
        assert!(sent["result"]["transaction_id"].is_string(), "{sent}");
        let calls = stub.requests.lock().unwrap().len();
        let response = router
            .oneshot(request("/v1/sends", body, Some("preparation-retry")))
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let replay: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            replay["result"]["transaction_id"],
            sent["result"]["transaction_id"]
        );
        assert_eq!(stub.requests.lock().unwrap().len(), calls);
    }
}
