#![cfg(all(feature = "postgres", feature = "ln-phoenixd"))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Disposable PostgreSQL regression suite, invoked by `scripts/test.sh postgres`.
//! Every case owns a schema in the script's isolated database.

#[path = "support/pay_submission.rs"]
mod pay_submission;

use agent_first_pay::handler::{App, dispatch};
use agent_first_pay::store::{StorageBackend, postgres_store::PostgresStore};
use agent_first_pay::types::{Output, Request, RuntimeConfig};
use pay_submission::*;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;

static SCHEMA_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    directory: tempfile::TempDir,
    config: RuntimeConfig,
    store: PostgresStore,
    admin: sqlx::PgPool,
    schema: String,
}

impl Fixture {
    async fn new() -> Self {
        let dsn = std::env::var("AFPAY_TEST_POSTGRES_DSN_SECRET")
            .expect("run the isolated database harness with scripts/test.sh postgres");
        let admin = sqlx::PgPool::connect(&dsn).await.unwrap();
        let schema = format!(
            "submission_{}_{}",
            std::process::id(),
            SCHEMA_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        // Only a literal prefix and numeric PID/counter form this identifier;
        // no caller input or SQL metacharacters can enter the generated SQL.
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&admin)
            .await
            .unwrap();
        let mut url = reqwest::Url::parse(&dsn).unwrap();
        url.query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let directory = tempfile::tempdir().unwrap();
        let config = RuntimeConfig {
            data_dir: directory.path().to_string_lossy().into_owned(),
            storage_backend: Some("postgres".into()),
            postgres_url_secret: Some(url.to_string()),
            ..RuntimeConfig::default()
        };
        let store = PostgresStore::connect(url.as_str(), &config.data_dir)
            .await
            .unwrap();
        Self {
            directory,
            config,
            store,
            admin,
            schema,
        }
    }

    fn app(&self, endpoint: &str, enforce: bool) -> (App, mpsc::Receiver<Output>) {
        make_app(
            self.config.clone(),
            StorageBackend::Postgres(self.store.clone()),
            endpoint,
            enforce,
        )
    }

    async fn cleanup(self) {
        self.store.pool().close().await;
        // `schema` is the same internally generated ASCII identifier audited above.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA {} CASCADE",
            self.schema
        )))
        .execute(&self.admin)
        .await
        .unwrap();
        self.admin.close().await;
        drop(self.directory);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires scripts/test.sh postgres isolated database"]
async fn postgres_response_loss_retains_pending_budget_and_key_after_restart() {
    let stub = PhoenixStub::start(PaymentResponse::Lost, None).await;
    let fixture = Fixture::new().await;
    let (app, mut rx) = fixture.app(&stub.endpoint, true);
    install_limit(&app).await;
    let plan_id = plan(&app, &mut rx, "pg-lost-plan").await;
    let output = terminal(
        &app,
        &mut rx,
        confirm(&plan_id, Some("pg-lost-key"), "pg-lost-confirm"),
    )
    .await;
    let Output::AccountingInconsistent {
        reservation_ids,
        transaction_id,
        hint,
        ..
    } = output
    else {
        panic!("unexpected result: {output:?}")
    };
    assert_eq!(reservation_ids.len(), 1);
    assert!(hint.contains("may have completed"));
    let invoice: lightning_invoice::Bolt11Invoice = INVOICE.parse().unwrap();
    assert_eq!(transaction_id, Some(invoice.payment_hash().to_string()));
    let reservation_id = reservation_ids[0] as i64;
    let (expiry,): (i64,) = sqlx::query_as("SELECT (reservation->>'expires_at_epoch_ms')::BIGINT FROM spend_reservations WHERE reservation_id = $1")
        .bind(reservation_id).fetch_one(fixture.store.pool()).await.unwrap();
    assert_eq!(
        expiry,
        i64::MAX,
        "protection must fit the PostgreSQL BIGINT range"
    );
    let (key_expiry,): (i64,) =
        sqlx::query_as("SELECT expires_at_ms FROM afpay_idempotency WHERE key = 'pg-lost-key'")
            .fetch_one(fixture.store.pool())
            .await
            .unwrap();
    assert_eq!(key_expiry, i64::MAX);
    // Cross Lightning's 90-second Pending TTL, while remaining inside the
    // one-hour spend window. No sleeping or external node is involved.
    sqlx::query("UPDATE spend_reservations SET reservation = jsonb_set(reservation, '{created_at_epoch_ms}', to_jsonb((reservation->>'created_at_epoch_ms')::BIGINT - 120000::BIGINT)) WHERE reservation_id = $1")
        .bind(reservation_id).execute(fixture.store.pool()).await.unwrap();
    drop(app);
    let (app, mut rx) = fixture.app(&stub.endpoint, true);
    assert_eq!(app.spend_ledger.get_status().await.unwrap()[0].spent, 2);
    assert_error(
        terminal(
            &app,
            &mut rx,
            confirm(&plan_id, Some("pg-lost-key"), "pg-retry-key"),
        )
        .await,
        "idempotency_in_progress",
    );
    assert_error(
        terminal(
            &app,
            &mut rx,
            confirm(&plan_id, Some("pg-different-key"), "pg-retry-different"),
        )
        .await,
        "plan_not_found",
    );
    assert_error(
        terminal(&app, &mut rx, confirm(&plan_id, None, "pg-retry-no-key")).await,
        "plan_not_found",
    );
    let second_plan = plan(&app, &mut rx, "pg-second-plan").await;
    assert!(matches!(
        terminal(
            &app,
            &mut rx,
            confirm(&second_plan, Some("pg-second-key"), "pg-second-confirm")
        )
        .await,
        Output::LimitExceeded { .. }
    ));
    assert_eq!(stub.calls.load(Ordering::SeqCst), 1);
    app.spend_ledger
        .force_cancel(reservation_ids[0], "provider verifies no completed payment")
        .await
        .unwrap();
    assert_eq!(app.spend_ledger.get_status().await.unwrap()[0].spent, 0);
    drop(app);
    fixture.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires scripts/test.sh postgres isolated database"]
async fn postgres_finalize_failure_keeps_the_receipt_and_pending_replay_guard() {
    let stub = PhoenixStub::start(PaymentResponse::Success, None).await;
    let fixture = Fixture::new().await;
    sqlx::raw_sql("CREATE FUNCTION reject_finalization() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.state = 'final' THEN RAISE EXCEPTION 'controlled finalization failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_finalization BEFORE UPDATE ON afpay_idempotency FOR EACH ROW EXECUTE FUNCTION reject_finalization();")
        .execute(fixture.store.pool()).await.unwrap();
    let (app, mut rx) = fixture.app(&stub.endpoint, false);
    let plan_id = plan(&app, &mut rx, "pg-finalize-plan").await;
    dispatch(
        &app,
        Request::from_input(confirm(
            &plan_id,
            Some("pg-finalize-key"),
            "pg-finalize-confirm",
        )),
    )
    .await;
    let mut saw_log = false;
    loop {
        match rx.recv().await.unwrap() {
            Output::Log { event, .. } if event == "idempotency_finalize_failed" => saw_log = true,
            Output::Sent {
                idempotency_finalize_failed,
                ..
            } => {
                assert!(idempotency_finalize_failed);
                break;
            }
            Output::Log { .. } => {}
            other => panic!("expected a successful payment receipt: {other:?}"),
        }
    }
    assert!(saw_log);
    let (state, expiry): (String, i64) = sqlx::query_as(
        "SELECT state, expires_at_ms FROM afpay_idempotency WHERE key = 'pg-finalize-key'",
    )
    .fetch_one(fixture.store.pool())
    .await
    .unwrap();
    assert_eq!(state, "pending");
    assert_eq!(expiry, i64::MAX);
    assert_error(
        terminal(
            &app,
            &mut rx,
            confirm(&plan_id, Some("pg-finalize-key"), "pg-retry-finalize"),
        )
        .await,
        "idempotency_in_progress",
    );
    assert_error(
        terminal(
            &app,
            &mut rx,
            confirm(&plan_id, Some("pg-fresh-finalize-key"), "pg-fresh-finalize"),
        )
        .await,
        "plan_not_found",
    );
    assert_eq!(stub.calls.load(Ordering::SeqCst), 1);
    drop(app);
    fixture.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires scripts/test.sh postgres isolated database"]
async fn postgres_clear_failure_is_logged_and_preserves_the_pending_claim() {
    let stub = PhoenixStub::start(PaymentResponse::Success, None).await;
    let fixture = Fixture::new().await;
    sqlx::raw_sql("CREATE FUNCTION reject_clear() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'controlled clear failure'; END $$; CREATE TRIGGER reject_clear BEFORE DELETE ON afpay_idempotency FOR EACH ROW EXECUTE FUNCTION reject_clear();")
        .execute(fixture.store.pool()).await.unwrap();
    let (app, mut rx) = fixture.app(&stub.endpoint, false);
    dispatch(
        &app,
        Request::from_input(confirm(
            "plan_0000000000000000",
            Some("pg-clear-key"),
            "pg-clear",
        )),
    )
    .await;
    let mut saw_log = false;
    loop {
        match rx.recv().await.unwrap() {
            Output::Log { event, .. } if event == "idempotency_clear_failed" => saw_log = true,
            Output::Error { error_code, .. } => {
                assert_eq!(error_code, "plan_not_found");
                break;
            }
            Output::Log { .. } => {}
            other => panic!("unexpected output: {other:?}"),
        }
    }
    assert!(saw_log);
    let (state,): (String,) =
        sqlx::query_as("SELECT state FROM afpay_idempotency WHERE key = 'pg-clear-key'")
            .fetch_one(fixture.store.pool())
            .await
            .unwrap();
    assert_eq!(state, "pending");
    assert_eq!(stub.calls.load(Ordering::SeqCst), 0);
    drop(app);
    fixture.cleanup().await;
}
