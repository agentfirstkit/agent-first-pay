#![cfg(all(feature = "redb", feature = "ln-phoenixd", feature = "ln-lnbits"))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[path = "support/pay_submission.rs"]
mod pay_submission;
use agent_first_pay::store::PayStore;
use agent_first_pay::types::{Output, RuntimeConfig, SpendLimit, SpendScope};
use pay_submission::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn paid_lnbits_receipt_confirms_reservation_after_detail_disconnect() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for expected in ["POST /api/v1/payments ", "GET /api/v1/payments/hash "] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            let header_end = loop {
                let length = stream.read(&mut buffer).await.unwrap();
                assert!(length > 0);
                request.extend_from_slice(&buffer[..length]);
                if let Some(offset) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    break offset + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            assert!(headers.starts_with(expected));
            let length = headers
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            while request.len() < header_end + length {
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
            }
            if expected.starts_with("POST") {
                let body = r#"{"payment_hash":"hash","preimage":"receipt-preimage","fee":0}"#;
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        }
    });
    let directory = tempfile::tempdir().unwrap();
    let config = RuntimeConfig {
        data_dir: directory.path().to_string_lossy().into_owned(),
        ..Default::default()
    };
    let store = agent_first_pay::store::create_storage_backend(&config).unwrap();
    let (app, mut receiver) = make_app(config, store, &endpoint, true);
    let store = app.store.as_ref().unwrap();
    let mut metadata = store.load_wallet_metadata("w_submission").unwrap();
    metadata.backend = Some("lnbits".into());
    store.save_wallet_metadata(&metadata).unwrap();
    app.spend_ledger
        .set_limits(&[SpendLimit {
            rule_id: None,
            scope: SpendScope::Network,
            network: Some("ln-lnbits".into()),
            wallet: None,
            window_s: 3600,
            max_spend: 2,
            token: None,
        }])
        .await
        .unwrap();
    let planned = plan(&app, &mut receiver, "plan").await;
    match terminal(
        &app,
        &mut receiver,
        confirm(&planned, Some("receipt-key"), "send"),
    )
    .await
    {
        Output::Sent {
            preimage,
            reservation_ids,
            idempotency_finalize_failed,
            ..
        } => {
            assert_eq!(preimage.as_deref(), Some("receipt-preimage"));
            assert!(!reservation_ids.is_empty());
            assert!(!idempotency_finalize_failed);
        }
        output => panic!("expected a confirmed send: {output:?}"),
    }
    server.await.unwrap();
}
