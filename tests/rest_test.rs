#![cfg(feature = "rest")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The HTTP domain API, driven in-process against the real router.
//!
//! Everything here goes through `api::router` — the same router `afpay --mode
//! rest` serves — so a route that only works because a test rebuilt it cannot
//! exist. The tests split into four claims: the discovery face is public and
//! complete, the domain face is closed (credential, typed bodies, no route to
//! anything local-only), no money moves without a reviewed plan, and what
//! comes back is an AFDATA envelope whose HTTP status agrees with its code.

use agent_first_pay::api::{ApiState, router};
use agent_first_pay::types::RuntimeConfig;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use std::net::SocketAddr;
use tower::ServiceExt as _;

/// Long enough to satisfy the credential rule `--mode rest` enforces at
/// startup, so tests exercise a key the daemon would actually accept.
const TOKEN: &str = "0123456789abcdef0123456789abcdef";

fn test_router() -> (Router, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let config = RuntimeConfig {
        data_dir: directory.path().to_string_lossy().into_owned(),
        ..RuntimeConfig::default()
    };
    (router(ApiState::new(config, TOKEN, Vec::new())), directory)
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 4 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "response body is not JSON ({error}): {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn authed(method: &str, uri: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {TOKEN}"))
}

fn authed_json(method: &str, uri: &str, body: &str) -> Request<Body> {
    authed(method, uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn keyed_json(method: &str, uri: &str, key: &str, body: &str) -> Request<Body> {
    authed(method, uri)
        .header("content-type", "application/json")
        .header("idempotency-key", key)
        .body(Body::from(body.to_string()))
        .unwrap()
}

// ═══════════════════════════════════════════
// Discovery face
// ═══════════════════════════════════════════

#[tokio::test]
async fn discovery_face_is_public_and_answers_without_touching_the_store() {
    let (app, _dir) = test_router();

    let health = app.clone().oneshot(get("/health")).await.unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    let value = body_json(health).await;
    assert_eq!(value["kind"], "result");
    assert_eq!(value["result"]["service"], "afpay");
    assert_eq!(value["result"]["status"], "ready");
    assert!(value["result"]["version"].is_string());
    assert!(value["trace"]["duration_ms"].is_u64());

    let openapi = app.clone().oneshot(get("/openapi.json")).await.unwrap();
    assert_eq!(openapi.status(), StatusCode::OK);
    assert_eq!(
        openapi
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/vnd.oai.openapi+json;version=3.2")
    );
    let document = body_json(openapi).await;
    assert_eq!(document["openapi"], "3.2.0");
    assert_eq!(document["servers"][0]["url"], "/");
    assert!(document["paths"]["/v1/sends"]["post"]["operationId"] == "create_send");

    let index = app
        .clone()
        .oneshot(get("/schemas/index.json"))
        .await
        .unwrap();
    assert_eq!(index.status(), StatusCode::OK);
    let index = body_json(index).await;
    let names = index["schemas"].as_array().unwrap();
    assert!(!names.is_empty());

    // Every schema the index names must actually be served: an index that
    // points at a 404 is worse than no index.
    for entry in names {
        let url = entry["schema_url"].as_str().unwrap();
        let response = app.clone().oneshot(get(url)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{url}");
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("application/schema+json"),
            "{url}"
        );
        let schema = body_json(response).await;
        assert_eq!(
            schema["$schema"], "https://json-schema.org/draft/2020-12/schema",
            "{url}"
        );
        assert!(
            schema["$id"]
                .as_str()
                .unwrap()
                .ends_with(entry["schema_name"].as_str().unwrap())
                || schema["$id"]
                    .as_str()
                    .unwrap()
                    .contains(entry["schema_name"].as_str().unwrap())
        );
    }

    let missing = app
        .oneshot(get("/schemas/not-a-schema.schema.json"))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(missing).await["error"]["code"],
        "schema_not_found"
    );
}

#[tokio::test]
async fn responses_carry_the_local_security_headers_and_never_a_cors_grant() {
    let (app, _dir) = test_router();
    let response = app.oneshot(get("/health")).await.unwrap();
    let headers = response.headers();
    assert_eq!(
        headers.get("cache-control").and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    assert_eq!(
        headers
            .get("x-content-type-options")
            .and_then(|v| v.to_str().ok()),
        Some("nosniff")
    );
    assert_eq!(
        headers.get("referrer-policy").and_then(|v| v.to_str().ok()),
        Some("no-referrer")
    );
    assert!(headers.get("access-control-allow-origin").is_none());
    assert!(headers.get("access-control-allow-credentials").is_none());
}

// ═══════════════════════════════════════════
// The credential
// ═══════════════════════════════════════════

#[tokio::test]
async fn domain_routes_require_a_bearer_credential() {
    let (app, _dir) = test_router();

    let anonymous = app.clone().oneshot(get("/v1/wallets")).await.unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        anonymous
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer realm=\"afpay\"")
    );
    let bytes = to_bytes(anonymous.into_body(), 1 << 20).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains(TOKEN), "the refusal echoed the credential");
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["kind"], "error");
    assert_eq!(value["error"]["code"], "authentication_required");

    let wrong = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/wallets")
                .header("authorization", "Bearer 0123456789abcdef0123456789abcdee")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    // The old command endpoint also accepted `X-API-Key`. One credential
    // header, named by the OpenAPI security scheme, is the whole rule now.
    let legacy = app
        .oneshot(
            Request::builder()
                .uri("/v1/wallets")
                .header("x-api-key", TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(legacy.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_credential_in_the_query_string_is_refused_before_it_is_read() {
    let (app, _dir) = test_router();
    for uri in [
        "/v1/wallets?token=abc",
        "/v1/wallets?api_key=abc",
        "/v1/wallets?rest_api_key_secret=abc",
    ] {
        let response = app.clone().oneshot(get(uri)).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(
            body_json(response).await["error"]["code"],
            "credential_location_invalid",
            "{uri}"
        );
    }
}

// ═══════════════════════════════════════════
// Reads reach the dispatcher
// ═══════════════════════════════════════════

#[tokio::test]
async fn listing_wallets_answers_in_a_typed_afdata_result_envelope() {
    let (app, _dir) = test_router();
    let response = app
        .oneshot(authed("GET", "/v1/wallets").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .expect("the correlation id travels in a header");
    assert!(request_id.starts_with("req_"));

    let value = body_json(response).await;
    assert_eq!(value["kind"], "result");
    assert_eq!(value["result"]["wallets"], serde_json::json!([]));
    assert!(value["trace"]["duration_ms"].is_u64());
    // The union tag and the correlation id are transport noise inside a
    // result the route has already typed; both belong outside the payload.
    assert!(value["result"].get("code").is_none());
    assert!(value["result"].get("id").is_none());
}

#[tokio::test]
async fn balances_and_transactions_and_spend_limits_are_readable() {
    let (app, _dir) = test_router();
    for (uri, key) in [
        ("/v1/balances", "wallets"),
        ("/v1/transactions", "items"),
        ("/v1/spend-limits", "limits"),
    ] {
        let response = app
            .clone()
            .oneshot(authed("GET", uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let value = body_json(response).await;
        assert_eq!(value["kind"], "result", "{uri}");
        assert!(value["result"][key].is_array(), "{uri} has no {key}");
    }
}

#[tokio::test]
async fn query_parameters_are_typed_and_closed() {
    let (app, _dir) = test_router();

    let good = app
        .clone()
        .oneshot(
            authed("GET", "/v1/wallets?network=sol")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(good.status(), StatusCode::OK);

    for uri in ["/v1/wallets?network=dogecoin", "/v1/wallets?netwrok=sol"] {
        let response = app
            .clone()
            .oneshot(authed("GET", uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(
            body_json(response).await["error"]["code"],
            "invalid_request",
            "{uri}"
        );
    }
}

// ═══════════════════════════════════════════
// Typed bodies
// ═══════════════════════════════════════════

#[tokio::test]
async fn a_typed_body_names_the_field_it_does_not_know() {
    let (app, _dir) = test_router();
    let response = app
        .oneshot(
            authed("POST", "/v1/send-plans")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"to":"bc1qexample","amount":{"value":1,"token":"sats"},"memo":"typo"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let value = body_json(response).await;
    assert_eq!(value["kind"], "error");
    assert_eq!(value["error"]["code"], "invalid_request");
    assert!(
        value["error"]["message"].as_str().unwrap().contains("memo"),
        "the refusal must name the field: {value}"
    );
}

#[tokio::test]
async fn wallet_creation_is_a_closed_tagged_union() {
    let (app, _dir) = test_router();

    // A Solana setting on a Bitcoin wallet is refused, not dropped.
    let crossed = app
        .clone()
        .oneshot(keyed_json(
            "POST",
            "/v1/wallets",
            "wallet-crossed-1",
            r#"{"network":"btc","cluster":"devnet"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(crossed.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(crossed).await["error"]["code"], "invalid_request");

    let unknown_network = app
        .oneshot(keyed_json(
            "POST",
            "/v1/wallets",
            "wallet-unknown-1",
            r#"{"network":"dogecoin"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(unknown_network.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn malformed_and_mistyped_and_oversized_bodies_all_answer_in_the_envelope() {
    let (app, _dir) = test_router();

    let malformed = app
        .clone()
        .oneshot(authed_json("POST", "/v1/cashu/redemptions", "{"))
        .await
        .unwrap();
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(malformed).await["kind"], "error");

    let no_content_type = app
        .clone()
        .oneshot(
            authed("POST", "/v1/cashu/redemptions")
                .body(Body::from(r#"{"token":"cashuA"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(no_content_type.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(
        body_json(no_content_type).await["error"]["code"],
        "unsupported_media_type"
    );

    let oversized = app
        .oneshot(authed_json(
            "POST",
            "/v1/cashu/redemptions",
            &format!(r#"{{"token":"{}"}}"#, "A".repeat(300 * 1024)),
        ))
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

// ═══════════════════════════════════════════
// Idempotency
// ═══════════════════════════════════════════

/// §8: every operation a retry could duplicate requires the header, and the
/// four that do are exactly the four the dispatcher can replay. A route that
/// asked for a key it could not honour would be worse than one that asks for
/// nothing.
#[tokio::test]
async fn every_operation_a_retry_could_duplicate_refuses_an_unusable_key() {
    let (app, _dir) = test_router();

    for (uri, payload) in [
        ("/v1/sends", r#"{"plan_id":"plan_abc"}"#),
        ("/v1/cashu/tokens", r#"{"plan_id":"plan_abc"}"#),
        ("/v1/wallets", r#"{"network":"btc"}"#),
        ("/v1/receives", r#"{"wallet":"w_missing"}"#),
    ] {
        for (label, key) in [
            ("missing", None),
            ("too short", Some("abc")),
            ("leading punctuation", Some("-not-allowed-here")),
            ("not ascii", Some("clé-de-paiement-1")),
        ] {
            let mut builder = authed("POST", uri).header("content-type", "application/json");
            if let Some(key) = key {
                builder = builder.header("idempotency-key", key);
            }
            let response = builder.body(Body::from(payload.to_string())).unwrap();
            let response = app.clone().oneshot(response).await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "{uri} accepted a {label} key"
            );
            assert_eq!(
                body_json(response).await["error"]["code"],
                "idempotency_key_required",
                "{uri} / {label}"
            );
        }
    }
}

/// Resolving a plan changes nothing and has no outcome to replay, so it must
/// not demand a header afpay would have nothing to do with.
#[tokio::test]
async fn resolving_a_plan_requires_no_idempotency_key() {
    let (app, _dir) = test_router();
    for uri in ["/v1/send-plans", "/v1/cashu/token-plans"] {
        let payload = if uri == "/v1/send-plans" {
            r#"{"to":"bc1qexample","wallet":"w_missing","amount":{"value":1,"token":"sats"}}"#
        } else {
            r#"{"amount":{"value":1,"token":"sats"},"wallet":"w_missing"}"#
        };
        let response = app
            .clone()
            .oneshot(authed_json("POST", uri, payload))
            .await
            .unwrap();
        // No such wallet is a domain answer: the request got past every
        // transport check without a key being asked for.
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(
            body_json(response).await["error"]["code"],
            "wallet_not_found",
            "{uri}"
        );
    }
}

// ═══════════════════════════════════════════
// The plan/confirm boundary
// ═══════════════════════════════════════════

/// §9: the only routes that move money take a plan id and nothing else. There
/// is no field on this face through which a payment can be described *and*
/// made in one call.
#[tokio::test]
async fn a_confirm_body_carries_a_plan_id_and_nothing_else() {
    let (app, _dir) = test_router();
    for uri in ["/v1/sends", "/v1/cashu/tokens"] {
        let response = app
            .clone()
            .oneshot(keyed_json(
                "POST",
                uri,
                "confirm-with-a-payment-1",
                r#"{"plan_id":"plan_abc","to":"bc1qexample","amount":{"value":1,"token":"sats"}}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        let value = body_json(response).await;
        assert_eq!(value["error"]["code"], "invalid_request", "{uri}");
        assert!(
            value["error"]["message"].as_str().unwrap().contains("to"),
            "{uri}: the refusal must name the field it will not take: {value}"
        );
    }
}

/// A plan id this workspace never issued buys nothing. The refusal is the
/// ordinary one — plans are single-use, so this is also what a replayed
/// confirm carrying a fresh key gets.
#[tokio::test]
async fn a_confirm_naming_an_unknown_plan_is_refused() {
    let (app, _dir) = test_router();
    for uri in ["/v1/sends", "/v1/cashu/tokens"] {
        let response = app
            .clone()
            .oneshot(keyed_json(
                "POST",
                uri,
                "confirm-unknown-plan-1",
                r#"{"plan_id":"plan_0000000000000000"}"#,
            ))
            .await
            .unwrap();
        let value = body_json(response).await;
        assert_eq!(value["kind"], "error", "{uri}");
        assert_eq!(value["error"]["code"], "plan_not_found", "{uri}");
        assert!(
            value["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("single-use"),
            "{uri}: {value}"
        );
    }
}

/// A plan id is a caller-supplied string that becomes a filename inside the
/// workspace. It must never be able to name anything outside it.
#[tokio::test]
async fn a_plan_id_cannot_be_a_path() {
    let (app, _dir) = test_router();
    let response = app
        .oneshot(keyed_json(
            "POST",
            "/v1/sends",
            "confirm-traversal-1",
            r#"{"plan_id":"../../config"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await["error"]["code"],
        "invalid_request"
    );
}

// ═══════════════════════════════════════════
// The closed surface
// ═══════════════════════════════════════════

/// Key material, spend-limit rules, reservation repair, and daemon config are
/// `Input::is_local_only`. They have no route here, and this test is what
/// keeps someone from adding one without noticing.
#[tokio::test]
async fn nothing_local_only_is_reachable_over_http() {
    let (app, _dir) = test_router();
    for uri in [
        "/v1/wallets/w_1/seed",
        "/v1/wallets/w_1/mnemonic",
        "/v1/config",
        "/v1/reservations/1",
        "/v1/reservations/1/reconcile",
        "/v1/afpay",
        // Neither half of the boundary may be bypassed by a route that pays
        // directly from a request body.
        "/v1/payments",
        "/v1/sends/execute",
    ] {
        let response = app
            .clone()
            .oneshot(authed("POST", uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri} is routed");
        let value = body_json(response).await;
        assert_eq!(value["kind"], "error", "{uri}");
        assert_eq!(value["error"]["code"], "api_route_not_found", "{uri}");
    }
}

#[tokio::test]
async fn spend_limits_are_readable_but_not_writable_over_http() {
    let (app, _dir) = test_router();
    let read = app
        .clone()
        .oneshot(
            authed("GET", "/v1/spend-limits")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::OK);

    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let response = app
            .clone()
            .oneshot(
                authed(method, "/v1/spend-limits")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} /v1/spend-limits"
        );
        assert_eq!(
            body_json(response).await["error"]["code"],
            "api_method_not_allowed"
        );
    }
}

#[tokio::test]
async fn an_unknown_route_answers_in_the_error_envelope() {
    let (app, _dir) = test_router();
    let response = app
        .oneshot(authed("GET", "/v1/nope").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let value = body_json(response).await;
    assert_eq!(value["error"]["code"], "api_route_not_found");
    assert!(value["error"]["hint"].as_str().unwrap().contains("openapi"));
}

// ═══════════════════════════════════════════
// Over a real socket
// ═══════════════════════════════════════════

/// The in-process tests above bypass the listener. This one proves the same
/// router answers over TCP, which is what `afpay --mode rest` actually serves.
#[tokio::test]
async fn the_same_router_answers_over_a_real_socket() {
    let (app, _dir) = test_router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let client = reqwest::Client::new();
    let health: Value = client
        .get(format!("http://{address}/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["result"]["service"], "afpay");

    let anonymous = client
        .get(format!("http://{address}/v1/wallets"))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);

    let wallets = client
        .get(format!("http://{address}/v1/wallets"))
        .header("Authorization", format!("Bearer {TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(wallets.status(), 200);
    let value: Value = wallets.json().await.unwrap();
    assert_eq!(value["result"]["wallets"], serde_json::json!([]));
}
