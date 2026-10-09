#![allow(dead_code)]

use agent_first_pay::handler::{App, dispatch};
use agent_first_pay::store::{PayStore, StorageBackend, wallet::WalletMetadata};
use agent_first_pay::types::{
    Input, Network, Output, Request, RuntimeConfig, SpendLimit, SpendScope,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

// Valid one-satoshi BOLT11 fixture from the parser's examples. No live node is used.
pub const INVOICE: &str = concat!(
    "lnbc100p1psj9jhxdqud3jxktt5w46x7unfv9kz6mn0v3jsnp4q0d3p2sfluzdx45tqcs",
    "h2pu5qc7lgq0xs578ngs6s0s68ua4h7cvspp5q6rmq35js88zp5dvwrv9m459tnk2zunwj5jalqtyxqulh0l",
    "5gflssp5nf55ny5gcrfl30xuhzj3nphgj27rstekmr9fw3ny5989s300gyus9qyysgqcqpcrzjqw2sxwe993",
    "h5pcm4dxzpvttgza8zhkqxpgffcrf5v25nwpr3cmfg7z54kuqq8rgqqqqqqqq2qqqqq9qq9qrzjqd0ylaqcl",
    "j9424x9m8h2vcukcgnm6s56xfgu3j78zyqzhgs4hlpzvznlugqq9vsqqqqqqqlgqqqqqeqq9qrzjqwldmj9d",
    "ha74df76zhx6l9we0vjdquygcdt3kssupehe64g6yyp5yz5rhuqqwccqqyqqqqlgqqqqjcqq9qrzjqf9e58a",
    "guqr0rcun0ajlvmzq3ek63cw2w282gv3z5uupmuwvgjtq2z55qsqqg6qqqyqqqrtnqqqzq3cqygrzjqvphms",
    "ywntrrhqjcraumvc4y6r8v4z5v593trte429v4hredj7ms5z52usqq9ngqqqqqqqlgqqqqqqgq9qrzjq2v0v",
    "p62g49p7569ev48cmulecsxe59lvaw3wlxm7r982zxa9zzj7z5l0cqqxusqqyqqqqlgqqqqqzsqygarl9fh3",
    "8s0gyuxjjgux34w75dnc6xp2l35j7es3jd4ugt3lu0xzre26yg5m7ke54n2d5sym4xcmxtl8238xxvw5h5h5",
    "j5r6drg6k6zcqj0fcwg",
);

#[derive(Clone, Copy)]
pub enum PaymentResponse {
    Lost,
    Success,
}

pub struct PhoenixStub {
    pub endpoint: String,
    pub calls: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for PhoenixStub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl PhoenixStub {
    pub async fn start(
        response: PaymentResponse,
        accepted: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let task_calls = calls.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let calls = task_calls.clone();
                let accepted = accepted.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 2048];
                    let header_end = loop {
                        let count = stream.read(&mut chunk).await.unwrap();
                        if count == 0 {
                            return;
                        }
                        request.extend_from_slice(&chunk[..count]);
                        if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                            break pos + 4;
                        }
                    };
                    let headers = String::from_utf8_lossy(&request[..header_end]).into_owned();
                    let length = headers
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
                    let body = if headers.starts_with("POST /payinvoice ") {
                        // The upstream accepted the complete payment request.
                        calls.fetch_add(1, Ordering::SeqCst);
                        if let Some(accepted) = accepted {
                            accepted();
                        }
                        if matches!(response, PaymentResponse::Lost) {
                            return;
                        }
                        // A receipt has to identify the payment to count as one.
                        r#"{"paymentHash":"stub-payment-hash","paymentPreimage":"stub-preimage","routingFeeSat":0}"#
                    } else if headers.starts_with("GET /getbalance ") {
                        r#"{"balanceSat":10000000}"#
                    } else {
                        "[]"
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).await.unwrap();
                });
            }
        });
        Self {
            endpoint,
            calls,
            task,
        }
    }
}

pub fn make_app(
    mut config: RuntimeConfig,
    store: StorageBackend,
    endpoint: &str,
    enforce: bool,
) -> (App, mpsc::Receiver<Output>) {
    config.log.extend([
        "idempotency_finalize_failed".to_string(),
        "idempotency_clear_failed".to_string(),
    ]);
    let metadata: WalletMetadata = serde_json::from_value(serde_json::json!({
        "id": "w_submission", "network": "ln", "backend": "phoenixd",
        "mint_url": endpoint, "seed_secret": "test-password", "created_at_epoch_s": 1,
    }))
    .unwrap();
    store.save_wallet_metadata(&metadata).unwrap();
    let (tx, rx) = mpsc::channel(256);
    (App::new(config, tx, Some(enforce), Some(store)), rx)
}

pub async fn install_limit(app: &App) {
    app.spend_ledger
        .set_limits(&[SpendLimit {
            rule_id: None,
            scope: SpendScope::Network,
            network: Some("ln-phoenixd".into()),
            wallet: None,
            window_s: 3600,
            max_spend: 2,
            token: None,
        }])
        .await
        .unwrap();
}

pub async fn terminal(app: &App, rx: &mut mpsc::Receiver<Output>, input: Input) -> Output {
    dispatch(app, Request::from_input(input)).await;
    loop {
        let output = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if !matches!(output, Output::Log { .. }) {
            return output;
        }
    }
}

pub async fn plan(app: &App, rx: &mut mpsc::Receiver<Output>, id: &str) -> String {
    let output = terminal(
        app,
        rx,
        Input::SendPlan {
            id: id.into(),
            wallet: Some("w_submission".into()),
            network: Some(Network::Ln),
            to: INVOICE.into(),
            amount: None,
            onchain_memo: None,
            local_memo: None,
            mints: None,
            chain_id: None,
        },
    )
    .await;
    match output {
        Output::PayPlanned { plan_id, .. } => plan_id,
        other => panic!("expected a plan: {other:?}"),
    }
}

pub fn confirm(plan: &str, key: Option<&str>, id: &str) -> Input {
    Input::PayConfirm {
        id: id.into(),
        plan_id: plan.into(),
        expect: None,
        idempotency_key: key.map(str::to_string),
    }
}

pub fn assert_error(output: Output, code: &str) {
    match output {
        Output::Error { error_code, .. } => assert_eq!(error_code, code),
        other => panic!("expected {code}: {other:?}"),
    }
}

/// Corrupt only the pending key's hash after the provider accepted payment.
/// Reservation confirmation can still commit; finalizing this key must fail.
#[cfg(feature = "redb")]
pub fn fail_pending_finalization(directory: &str) {
    use redb::ReadableTable;
    let db =
        redb::Database::open(std::path::Path::new(directory).join("spend/spend.redb")).unwrap();
    let tx = db.begin_write().unwrap();
    {
        let mut table = tx
            .open_table(redb::TableDefinition::<&str, &str>::new(
                "idempotency_by_key_v1",
            ))
            .unwrap();
        let pending = table
            .iter()
            .unwrap()
            .filter_map(|entry| {
                let (key, bytes) = entry.unwrap();
                let mut record: serde_json::Value = serde_json::from_str(bytes.value()).unwrap();
                if record["state"] != "pending" {
                    return None;
                }
                record["input_hash"] = serde_json::json!("controlled-finalization-drift");
                Some((
                    key.value().to_string(),
                    serde_json::to_string(&record).unwrap(),
                ))
            })
            .collect::<Vec<_>>();
        assert!(!pending.is_empty());
        for (key, bytes) in pending {
            table.insert(key.as_str(), bytes.as_str()).unwrap();
        }
    }
    tx.commit().unwrap();
}
