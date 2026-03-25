#![cfg(feature = "rest")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use agent_first_pay::handler::{self, App};
use agent_first_pay::types::{Input, Output, RuntimeConfig};
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::mpsc;
use tower::ServiceExt as _;

// ═══════════════════════════════════════════
// Shared test server
// ═══════════════════════════════════════════

struct TestAppState {
    app: Arc<App>,
    api_key: String,
}

fn make_test_router(api_key: &str) -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = RuntimeConfig {
        data_dir: dir.path().to_string_lossy().into_owned(),
        ..RuntimeConfig::default()
    };

    let (tx, _rx) = mpsc::channel::<Output>(4096);
    let store = agent_first_pay::store::create_storage_backend(&config);
    let app = Arc::new(App::new(config, tx, Some(true), store));

    let state = Arc::new(TestAppState {
        app,
        api_key: api_key.to_string(),
    });

    let router = axum::Router::new()
        .route("/v1/afpay", post(handle_call))
        .with_state(state);

    (router, dir)
}

async fn handle_call(
    State(state): State<Arc<TestAppState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    // Auth check — Bearer or X-API-Key
    let authed = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t == state.api_key)
        .unwrap_or(false)
        || headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .map(|v| v == state.api_key)
            .unwrap_or(false);

    if !authed {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({"code":"error","error":"unauthorized"})),
        )
            .into_response();
    }

    let input: Input = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(
                    serde_json::json!({"code":"error","error":format!("invalid input: {e}")}),
                ),
            )
                .into_response();
        }
    };

    if input.is_local_only() {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(
                serde_json::json!({"code":"error","error":"local-only operation not allowed over REST"}),
            ),
        )
            .into_response();
    }

    let (tx, mut rx) = mpsc::channel::<Output>(256);
    let config = state.app.config.read().await.clone();
    let store = agent_first_pay::store::create_storage_backend(&config);
    let app = Arc::new(App::new(config, tx, Some(true), store));
    app.requests_total.fetch_add(1, Ordering::Relaxed);

    handler::dispatch(&app, input).await;
    drop(app);

    let mut outputs = Vec::new();
    while let Some(out) = rx.recv().await {
        let v = serde_json::to_value(&out).unwrap_or(serde_json::Value::Null);
        outputs.push(v);
    }

    let has_error = outputs
        .iter()
        .any(|item| item.get("code").and_then(|v| v.as_str()) == Some("error"));
    let status = if has_error {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };

    (status, axum::Json(serde_json::Value::Array(outputs))).into_response()
}

/// Start a real TCP REST server. Returns (addr, api_key, tempdir).
async fn start_rest_server() -> (SocketAddr, String, tempfile::TempDir) {
    let api_key = "test-rest-api-key".to_string();
    let (router, dir) = make_test_router(&api_key);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    (addr, api_key, dir)
}

// ═══════════════════════════════════════════
// Tests — in-process (tower::ServiceExt)
// ═══════════════════════════════════════════

#[tokio::test]
async fn rest_version_bearer_auth() {
    let (router, _dir) = make_test_router("my-key");

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/afpay")
                .header("authorization", "Bearer my-key")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"code":"version"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let outputs: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();

    assert!(!outputs.is_empty());
    assert_eq!(
        outputs[0].get("code").and_then(|v| v.as_str()),
        Some("version")
    );
    assert!(outputs[0].get("version").and_then(|v| v.as_str()).is_some());
}

#[tokio::test]
async fn rest_version_x_api_key_auth() {
    let (router, _dir) = make_test_router("my-key");

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/afpay")
                .header("x-api-key", "my-key")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"code":"version"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn rest_unauthorized_no_header() {
    let (router, _dir) = make_test_router("my-key");

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/afpay")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"code":"version"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn rest_unauthorized_wrong_key() {
    let (router, _dir) = make_test_router("my-key");

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/afpay")
                .header("authorization", "Bearer wrong-key")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"code":"version"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn rest_bad_json() {
    let (router, _dir) = make_test_router("my-key");

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/afpay")
                .header("authorization", "Bearer my-key")
                .header("content-type", "application/json")
                .body(Body::from(r#"{invalid json"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn rest_local_only_rejected() {
    let (router, _dir) = make_test_router("my-key");

    // limit_set is a local-only operation
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/afpay")
                .header("authorization", "Bearer my-key")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"code":"limit_set","id":"test_1","limits":[]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn rest_wallet_list_empty() {
    let (router, _dir) = make_test_router("my-key");

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/afpay")
                .header("authorization", "Bearer my-key")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"code":"wallet_list","id":"test_list"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let outputs: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();

    assert!(!outputs.is_empty());
    assert_eq!(
        outputs[0].get("code").and_then(|v| v.as_str()),
        Some("wallet_list")
    );
    let wallets = outputs[0].get("wallets").and_then(|v| v.as_array());
    assert!(wallets.is_some());
    assert!(wallets.unwrap().is_empty());
}

#[tokio::test]
async fn rest_limit_list_allowed() {
    let (router, _dir) = make_test_router("my-key");

    // limit_list is read-only — should be allowed over REST
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/afpay")
                .header("authorization", "Bearer my-key")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"code":"limit_list","id":"test_limit"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let outputs: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();

    assert!(!outputs.is_empty());
    assert_eq!(
        outputs[0].get("code").and_then(|v| v.as_str()),
        Some("limit_status")
    );
}

// ═══════════════════════════════════════════
// Tests — real TCP (reqwest)
// ═══════════════════════════════════════════

#[tokio::test]
async fn rest_tcp_version() {
    let (addr, api_key, _dir) = start_rest_server().await;
    let url = format!("http://{}/v1/afpay", addr);

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&serde_json::json!({"code":"version"}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    let outputs: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(!outputs.is_empty());
    assert_eq!(outputs[0]["code"], "version");
    assert!(outputs[0]["version"].as_str().is_some());
}

#[tokio::test]
async fn rest_tcp_unauthorized() {
    let (addr, _api_key, _dir) = start_rest_server().await;
    let url = format!("http://{}/v1/afpay", addr);

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("Authorization", "Bearer wrong")
        .json(&serde_json::json!({"code":"version"}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn rest_tcp_wallet_list_and_balance() {
    let (addr, api_key, _dir) = start_rest_server().await;
    let url = format!("http://{}/v1/afpay", addr);
    let client = reqwest::Client::new();

    // Wallet list
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&serde_json::json!({"code":"wallet_list","id":"tcp_1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let outputs: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(outputs[0]["code"], "wallet_list");

    // Balance (all wallets — empty)
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&serde_json::json!({"code":"balance","id":"tcp_2"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let outputs: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(outputs[0]["code"], "wallet_balances");
}
