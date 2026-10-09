#![cfg(feature = "redb")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Handler-level idempotency tests.
//!
//! The spend-ledger unit tests cover the claim/finalize/clear state machine
//! directly. These tests prove the wiring: that a confirm short-circuits on
//! replay, returns `idempotency_conflict` when the same key is aimed at a
//! different plan, and that only a definite pre-submission failure releases
//! the slot. Unknown submissions retain their key and budget for reconciliation.
//!
//! Every payment here is two dispatches — plan, then confirm — because that is
//! the only way afpay moves money. The mock provider counts `cashu_send`
//! calls, so "the plan contacted nobody" is visible as a call count of zero
//! after the plan step.

use agent_first_pay::handler::{App, dispatch};
use agent_first_pay::provider::{HistorySyncStats, PayError, PayProvider};
use agent_first_pay::store::create_storage_backend;
use agent_first_pay::types::{
    Amount, BalanceInfo, CashuReceiveResult, CashuSendQuoteInfo, CashuSendResult, HistoryRecord,
    HistoryStatusInfo, Input, Network, Output, ReceiveInfo, Request, RuntimeConfig, SendResult,
    TxStatus, WalletBalanceItem, WalletCreateRequest, WalletInfo, WalletSummary,
};
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;

#[cfg(feature = "ln-phoenixd")]
#[path = "support/pay_submission.rs"]
mod pay_submission;

/// Mock Cashu provider: counts cashu_send calls, returns either a canned
/// success or a controlled error per the mode set at construction.
struct ControlledCashuProvider {
    calls: Arc<AtomicUsize>,
    behavior: Behavior,
}

#[derive(Clone)]
enum Behavior {
    /// Every call returns a fresh success with this transaction_id template.
    Success {
        tx_id: String,
    },
    SuccessWithWarning {
        tx_id: String,
    },
    /// A definite refusal releases the idempotency claim and reviewed plan.
    AlwaysError,
    Unknown {
        transaction_id: Option<String>,
        directory: String,
    },
    SuccessWithLedgerHeld {
        tx_id: String,
        directory: String,
        held: Arc<std::sync::Mutex<Option<redb::Database>>>,
    },
    RefusalWithLedgerHeld {
        directory: String,
        held: Arc<std::sync::Mutex<Option<redb::Database>>>,
    },
}

#[async_trait]
impl PayProvider for ControlledCashuProvider {
    fn network(&self) -> Network {
        Network::Cashu
    }

    async fn create_wallet(&self, _req: &WalletCreateRequest) -> Result<WalletInfo, PayError> {
        Err(PayError::not_implemented("unused".to_string()))
    }

    async fn close_wallet(&self, _wallet: &str) -> Result<(), PayError> {
        Err(PayError::not_implemented("unused".to_string()))
    }

    async fn list_wallets(&self) -> Result<Vec<WalletSummary>, PayError> {
        Ok(vec![])
    }

    async fn balance(&self, _wallet: &str) -> Result<BalanceInfo, PayError> {
        Ok(BalanceInfo::new(0, 0, "sats"))
    }

    async fn balance_all(&self) -> Result<Vec<WalletBalanceItem>, PayError> {
        Ok(vec![])
    }

    async fn receive_info(
        &self,
        _wallet: &str,
        _amount: Option<Amount>,
    ) -> Result<ReceiveInfo, PayError> {
        Err(PayError::not_implemented("unused".to_string()))
    }

    async fn receive_claim(&self, _wallet: &str, _quote_id: &str) -> Result<u64, PayError> {
        Err(PayError::not_implemented("unused".to_string()))
    }

    /// Resolving costs nothing and is not counted: `calls` exists to prove
    /// the mint is reached exactly once, at confirm time.
    async fn cashu_send_quote(
        &self,
        _wallet: &str,
        amount: &Amount,
    ) -> Result<CashuSendQuoteInfo, PayError> {
        Ok(CashuSendQuoteInfo {
            wallet: "w_mock".to_string(),
            amount_native: amount.value,
            fee_native: 0,
            fee_unit: "sats".to_string(),
            warnings: Vec::new(),
            upstream_plan_id: None,
        })
    }

    async fn cashu_send(
        &self,
        _wallet: &str,
        amount: Amount,
        _onchain_memo: Option<&str>,
        _mints: Option<&[String]>,
    ) -> Result<CashuSendResult, PayError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if let Behavior::SuccessWithLedgerHeld {
            directory, held, ..
        }
        | Behavior::RefusalWithLedgerHeld { directory, held } = &self.behavior
        {
            *held.lock().unwrap() = Some(
                redb::Database::open(std::path::Path::new(directory).join("spend/spend.redb"))
                    .unwrap(),
            );
        }
        match &self.behavior {
            Behavior::Success { tx_id }
            | Behavior::SuccessWithWarning { tx_id }
            | Behavior::SuccessWithLedgerHeld { tx_id, .. } => Ok(CashuSendResult {
                wallet: "w_mock".to_string(),
                transaction_id: tx_id.clone(),
                status: TxStatus::Confirmed,
                fee: None,
                token: format!("cashuT_{}", amount.value),
                idempotency_finalize_failed: matches!(
                    &self.behavior,
                    Behavior::SuccessWithWarning { .. }
                ),
            }),
            Behavior::AlwaysError | Behavior::RefusalWithLedgerHeld { .. } => {
                Err(PayError::invalid_amount("mock refused before submission"))
            }
            Behavior::Unknown {
                transaction_id,
                directory,
            } => {
                assert_redb_submission_is_protected(directory);
                Err(PayError::submission_unknown(
                    "mock accepted the payment but lost its response",
                    transaction_id.clone(),
                ))
            }
        }
    }

    async fn cashu_receive(
        &self,
        _wallet: &str,
        _token: &str,
    ) -> Result<CashuReceiveResult, PayError> {
        Err(PayError::not_implemented("unused".to_string()))
    }

    async fn send(
        &self,
        _wallet: &str,
        _to: &str,
        _onchain_memo: Option<&str>,
        _mints: Option<&[String]>,
    ) -> Result<SendResult, PayError> {
        Err(PayError::not_implemented("unused".to_string()))
    }

    async fn history_list(
        &self,
        _wallet: &str,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<HistoryRecord>, PayError> {
        Ok(vec![])
    }

    async fn history_status(&self, _transaction_id: &str) -> Result<HistoryStatusInfo, PayError> {
        Err(PayError::not_implemented("unused".to_string()))
    }

    async fn history_sync(
        &self,
        _wallet: &str,
        _limit: usize,
    ) -> Result<HistorySyncStats, PayError> {
        Err(PayError::not_implemented("unused".to_string()))
    }
}

fn make_app(data_dir: String, provider: Box<dyn PayProvider>) -> (App, mpsc::Receiver<Output>) {
    let config = RuntimeConfig {
        data_dir,
        log: vec![
            "idempotency_finalize_failed".into(),
            "idempotency_clear_failed".into(),
        ],
        ..RuntimeConfig::default()
    };
    let store = create_storage_backend(&config).expect("storage backend");
    let (tx, rx) = mpsc::channel::<Output>(64);
    // enforce_limits=false → handler skips spend-reservation flow so the
    // mock provider sees the cashu_send call directly. Idempotency still
    // engages because the spend_ledger DB is initialized from data_dir.
    let mut app = App::new(config, tx, Some(false), Some(store));
    app.providers.insert(Network::Cashu, provider);
    (app, rx)
}

/// Resolve a token mint into a plan, and return the id that would pay for it.
///
/// Nothing has moved when this returns — that is the property the tests below
/// lean on when they assert the mint was contacted zero times.
async fn plan_cashu_send(
    app: &App,
    rx: &mut mpsc::Receiver<Output>,
    amount: u64,
    id: &str,
) -> String {
    dispatch(
        app,
        Request::from_input(Input::CashuSendPlan {
            id: id.to_string(),
            wallet: None,
            amount: Amount {
                value: amount,
                token: "sats".to_string(),
            },
            onchain_memo: None,
            local_memo: None,
            mints: None,
        }),
    )
    .await;
    let planned = drain_until(rx, |o| matches!(o, Output::PayPlanned { .. })).await;
    let Output::PayPlanned { plan_id, .. } = planned else {
        unreachable!()
    };
    plan_id
}

fn confirm_with_key(plan_id: &str, key: Option<&str>, id: &str) -> Input {
    Input::PayConfirm {
        id: id.to_string(),
        plan_id: plan_id.to_string(),
        expect: None,
        idempotency_key: key.map(|s| s.to_string()),
    }
}

async fn drain_until<F>(rx: &mut mpsc::Receiver<Output>, predicate: F) -> Output
where
    F: Fn(&Output) -> bool,
{
    loop {
        let next = rx.recv().await.expect("channel closed before match");
        if predicate(&next) {
            return next;
        }
        // Log outputs (and others) are dropped silently — we only care about
        // the one we're waiting for.
    }
}

#[tokio::test]
async fn confirm_idempotency_replay_returns_same_transaction_id() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Box::new(ControlledCashuProvider {
        calls: calls.clone(),
        behavior: Behavior::Success {
            tx_id: "tx_replay_check".to_string(),
        },
    });
    let (app, mut rx) = make_app(tmp.path().to_string_lossy().into_owned(), provider);

    let plan_id = plan_cashu_send(&app, &mut rx, 100, "req_plan").await;
    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "resolving a plan must not reach the mint"
    );

    dispatch(
        &app,
        Request::from_input(confirm_with_key(&plan_id, Some("idem_k"), "req_first")),
    )
    .await;
    let first = drain_until(&mut rx, |o| matches!(o, Output::CashuSent { .. })).await;
    let Output::CashuSent {
        id, transaction_id, ..
    } = first
    else {
        unreachable!()
    };
    assert_eq!(id, "req_first");
    assert_eq!(transaction_id, "tx_replay_check");
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    // Same key + same plan, new request id → replay, NOT a second mint. Note
    // the plan itself is spent by now: it is the key, not the plan, that makes
    // a retry safe.
    dispatch(
        &app,
        Request::from_input(confirm_with_key(&plan_id, Some("idem_k"), "req_second")),
    )
    .await;
    let second = drain_until(&mut rx, |o| matches!(o, Output::CashuSent { .. })).await;
    let Output::CashuSent {
        id, transaction_id, ..
    } = second
    else {
        unreachable!()
    };
    assert_eq!(
        id, "req_second",
        "replay must rewrite id to the new request's id"
    );
    assert_eq!(
        transaction_id, "tx_replay_check",
        "replay must surface the original transaction_id"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "cashu_send must NOT be called a second time"
    );
}

/// A plan is single-use. A second confirm carrying a *fresh* key cannot fall
/// through to the mint — the key has no replay to serve and the plan is gone.
#[tokio::test]
async fn a_confirmed_plan_cannot_be_confirmed_again_under_a_new_key() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Box::new(ControlledCashuProvider {
        calls: calls.clone(),
        behavior: Behavior::Success {
            tx_id: "tx_single_use".to_string(),
        },
    });
    let (app, mut rx) = make_app(tmp.path().to_string_lossy().into_owned(), provider);

    let plan_id = plan_cashu_send(&app, &mut rx, 100, "req_plan").await;
    dispatch(
        &app,
        Request::from_input(confirm_with_key(&plan_id, Some("key_one"), "req_a")),
    )
    .await;
    let _ = drain_until(&mut rx, |o| matches!(o, Output::CashuSent { .. })).await;

    dispatch(
        &app,
        Request::from_input(confirm_with_key(&plan_id, Some("key_two"), "req_b")),
    )
    .await;
    let refused = drain_until(&mut rx, |o| matches!(o, Output::Error { .. })).await;
    let Output::Error { error_code, .. } = refused else {
        unreachable!()
    };
    assert_eq!(error_code, "plan_not_found");
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "a spent plan must not mint a second time"
    );
}

#[tokio::test]
async fn confirm_idempotency_conflict_when_the_key_is_aimed_at_another_plan() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Box::new(ControlledCashuProvider {
        calls: calls.clone(),
        behavior: Behavior::Success {
            tx_id: "tx_conflict_check".to_string(),
        },
    });
    let (app, mut rx) = make_app(tmp.path().to_string_lossy().into_owned(), provider);

    let first_plan = plan_cashu_send(&app, &mut rx, 100, "req_plan_a").await;
    let second_plan = plan_cashu_send(&app, &mut rx, 999, "req_plan_b").await;

    dispatch(
        &app,
        Request::from_input(confirm_with_key(&first_plan, Some("idem_k"), "req_a")),
    )
    .await;
    let _ = drain_until(&mut rx, |o| matches!(o, Output::CashuSent { .. })).await;
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    // Same key, DIFFERENT plan → conflict, and the second plan is untouched.
    dispatch(
        &app,
        Request::from_input(confirm_with_key(&second_plan, Some("idem_k"), "req_b")),
    )
    .await;
    let err = drain_until(&mut rx, |o| matches!(o, Output::Error { .. })).await;
    let Output::Error { error_code, .. } = err else {
        unreachable!()
    };
    assert_eq!(error_code, "idempotency_conflict");
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "conflict must NOT trigger another send"
    );
}

#[tokio::test]
async fn confirm_idempotency_clears_on_definite_refusal_and_hands_the_plan_back() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Box::new(ControlledCashuProvider {
        calls: calls.clone(),
        behavior: Behavior::AlwaysError,
    });
    let (app, mut rx) = make_app(tmp.path().to_string_lossy().into_owned(), provider);

    let plan_id = plan_cashu_send(&app, &mut rx, 50, "req_plan").await;
    dispatch(
        &app,
        Request::from_input(confirm_with_key(&plan_id, Some("retry_k"), "req_a")),
    )
    .await;
    let _ = drain_until(&mut rx, |o| matches!(o, Output::Error { .. })).await;
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    // Second confirm with the same key and plan must NOT be blocked by
    // InProgress, and must find the plan available again: nothing happened, so
    // neither the key nor the plan was spent.
    dispatch(
        &app,
        Request::from_input(confirm_with_key(&plan_id, Some("retry_k"), "req_b")),
    )
    .await;
    let _ = drain_until(&mut rx, |o| matches!(o, Output::Error { .. })).await;
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "a definite refusal must release both the idempotency slot and the plan"
    );
}

#[tokio::test]
async fn unknown_cashu_submission_retains_keys_budget_and_recovery_ids_after_restart() {
    use agent_first_pay::types::{SpendLimit, SpendScope};
    for key in [Some("unknown-key"), None] {
        for recovery_id in [Some("real-melt-id".to_string()), None] {
            let tmp = tempfile::tempdir().unwrap();
            let directory = tmp.path().to_string_lossy().into_owned();
            let calls = Arc::new(AtomicUsize::new(0));
            let make_provider = || {
                Box::new(ControlledCashuProvider {
                    calls: calls.clone(),
                    behavior: Behavior::Unknown {
                        transaction_id: recovery_id.clone(),
                        directory: directory.clone(),
                    },
                }) as Box<dyn PayProvider>
            };
            let (mut app, mut rx) = make_app(directory.clone(), make_provider());
            app.enforce_limits = true;
            app.spend_ledger
                .set_limits(&[SpendLimit {
                    rule_id: None,
                    scope: SpendScope::Network,
                    network: Some("cashu".into()),
                    wallet: None,
                    window_s: 3600,
                    max_spend: 50,
                    token: None,
                }])
                .await
                .unwrap();
            let plan = plan_cashu_send(&app, &mut rx, 50, "unknown-plan").await;
            dispatch(
                &app,
                Request::from_input(confirm_with_key(&plan, key, "unknown-confirm")),
            )
            .await;
            let output = drain_until(&mut rx, |o| {
                matches!(o, Output::AccountingInconsistent { .. })
            })
            .await;
            let wire = serde_json::to_value(&output).unwrap();
            let Output::AccountingInconsistent {
                transaction_id,
                reservation_ids,
                confirm_errors,
                hint,
                ..
            } = output
            else {
                unreachable!()
            };
            assert_eq!(transaction_id, recovery_id);
            assert_eq!(wire.get("transaction_id").is_some(), recovery_id.is_some());
            assert_eq!(reservation_ids.len(), 1);
            assert!(confirm_errors[0].contains("unknown"));
            assert!(hint.contains("may have completed"));
            assert!(!hint.contains("money left"));
            age_redb_reservation(&directory, reservation_ids[0], 90_000);
            assert_eq!(app.spend_ledger.get_status().await.unwrap()[0].spent, 50);
            drop(app);

            let (mut app, mut rx) = make_app(directory.clone(), make_provider());
            app.enforce_limits = true;
            // Existing reservation crossed Cashu's 60-second TTL, but remains
            // within the rule's ordinary one-hour window after a process restart.
            assert_eq!(app.spend_ledger.get_status().await.unwrap()[0].spent, 50);
            for (retry_key, expected) in [
                (
                    key,
                    if key.is_some() {
                        "idempotency_in_progress"
                    } else {
                        "plan_not_found"
                    },
                ),
                (Some("different-key"), "plan_not_found"),
                (None, "plan_not_found"),
            ] {
                dispatch(
                    &app,
                    Request::from_input(confirm_with_key(&plan, retry_key, "retry-original")),
                )
                .await;
                let output = drain_until(&mut rx, |o| matches!(o, Output::Error { .. })).await;
                let Output::Error { error_code, .. } = output else {
                    unreachable!()
                };
                assert_eq!(error_code, expected);
            }
            let second_plan = plan_cashu_send(&app, &mut rx, 50, "new-plan").await;
            dispatch(
                &app,
                Request::from_input(confirm_with_key(
                    &second_plan,
                    Some("new-plan-key"),
                    "new-confirm",
                )),
            )
            .await;
            let _ = drain_until(&mut rx, |o| matches!(o, Output::LimitExceeded { .. })).await;
            assert_eq!(
                calls.load(Ordering::Relaxed),
                1,
                "no retry may reach the provider"
            );
            app.spend_ledger
                .force_cancel(reservation_ids[0], "provider confirms no payment completed")
                .await
                .unwrap();
            assert_eq!(app.spend_ledger.get_status().await.unwrap()[0].spent, 0);
        }
    }
}

fn age_redb_reservation(directory: &str, reservation_id: u64, age_ms: u64) {
    use redb::ReadableTable;
    let db =
        redb::Database::open(std::path::Path::new(directory).join("spend/spend.redb")).unwrap();
    let tx = db.begin_write().unwrap();
    {
        let mut table = tx
            .open_table(redb::TableDefinition::<u64, &str>::new("reservation_by_id"))
            .unwrap();
        let bytes = table
            .get(reservation_id)
            .unwrap()
            .unwrap()
            .value()
            .to_string();
        let mut value: serde_json::Value = serde_json::from_str(&bytes).unwrap();
        assert_eq!(value["expires_at_epoch_ms"], i64::MAX as u64);
        let created = value["created_at_epoch_ms"].as_u64().unwrap();
        value["created_at_epoch_ms"] = serde_json::json!(created - age_ms);
        let encoded = serde_json::to_string(&value).unwrap();
        table.insert(reservation_id, encoded.as_str()).unwrap();
    }
    tx.commit().unwrap();
}

fn assert_redb_submission_is_protected(directory: &str) {
    use redb::{ReadableDatabase, ReadableTable};
    let db =
        redb::Database::open(std::path::Path::new(directory).join("spend/spend.redb")).unwrap();
    let tx = db.begin_read().unwrap();
    let reservations = tx
        .open_table(redb::TableDefinition::<u64, &str>::new("reservation_by_id"))
        .unwrap();
    for entry in reservations.iter().unwrap() {
        let (_, bytes) = entry.unwrap();
        let value: serde_json::Value = serde_json::from_str(bytes.value()).unwrap();
        if value["status"] == "Pending" {
            assert_eq!(
                value["expires_at_epoch_ms"],
                i64::MAX as u64,
                "reservation protection must precede the provider call"
            );
        }
    }
    let Ok(keys) = tx.open_table(redb::TableDefinition::<&str, &str>::new(
        "idempotency_by_key_v1",
    )) else {
        return;
    };
    for entry in keys.iter().unwrap() {
        let (_, bytes) = entry.unwrap();
        let value: serde_json::Value = serde_json::from_str(bytes.value()).unwrap();
        if value["state"] == "pending" {
            assert_eq!(
                value["expires_at_epoch_ms"],
                i64::MAX as u64,
                "key protection must precede the provider call"
            );
        }
    }
}

#[tokio::test]
async fn successful_cashu_send_reports_redb_finalize_failure_without_losing_the_token() {
    let tmp = tempfile::tempdir().unwrap();
    let directory = tmp.path().to_string_lossy().into_owned();
    let calls = Arc::new(AtomicUsize::new(0));
    let held = Arc::new(std::sync::Mutex::new(None));
    let provider = Box::new(ControlledCashuProvider {
        calls: calls.clone(),
        behavior: Behavior::SuccessWithLedgerHeld {
            tx_id: "successful-mint-id".into(),
            directory: directory.clone(),
            held: held.clone(),
        },
    });
    let (app, mut rx) = make_app(directory, provider);
    let plan = plan_cashu_send(&app, &mut rx, 10, "mint-finalize-plan").await;
    dispatch(
        &app,
        Request::from_input(confirm_with_key(
            &plan,
            Some("mint-finalize-key"),
            "mint-finalize-confirm",
        )),
    )
    .await;
    let mut saw_failure_log = false;
    loop {
        match rx.recv().await.unwrap() {
            Output::Log { event, .. } if event == "idempotency_finalize_failed" => {
                saw_failure_log = true
            }
            Output::CashuSent {
                idempotency_finalize_failed,
                token,
                ..
            } => {
                assert!(idempotency_finalize_failed);
                assert_eq!(token, "cashuT_10");
                break;
            }
            Output::Log { .. } => {}
            other => panic!("unexpected mint receipt: {other:?}"),
        }
    }
    assert!(saw_failure_log);
    held.lock().unwrap().take();
    dispatch(
        &app,
        Request::from_input(confirm_with_key(
            &plan,
            Some("mint-finalize-key"),
            "mint-retry",
        )),
    )
    .await;
    let output = drain_until(&mut rx, |o| matches!(o, Output::Error { .. })).await;
    let Output::Error { error_code, .. } = output else {
        unreachable!()
    };
    assert_eq!(error_code, "idempotency_in_progress");
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn downstream_finalize_warning_survives_successful_local_finalization_and_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Box::new(ControlledCashuProvider {
        calls: calls.clone(),
        behavior: Behavior::SuccessWithWarning {
            tx_id: "downstream-success".into(),
        },
    });
    let (app, mut rx) = make_app(tmp.path().to_string_lossy().into_owned(), provider);
    let plan = plan_cashu_send(&app, &mut rx, 10, "warning-plan").await;
    for id in ["warning-first", "warning-replay"] {
        dispatch(
            &app,
            Request::from_input(confirm_with_key(&plan, Some("warning-key"), id)),
        )
        .await;
        let output = drain_until(&mut rx, |o| matches!(o, Output::CashuSent { .. })).await;
        let Output::CashuSent {
            transaction_id,
            idempotency_finalize_failed,
            ..
        } = output
        else {
            unreachable!()
        };
        assert_eq!(transaction_id, "downstream-success");
        assert!(
            idempotency_finalize_failed,
            "local success cannot erase a downstream warning"
        );
    }
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "upstream must still finalize its replay even when downstream warned"
    );
}

#[tokio::test]
async fn redb_clear_failure_is_logged_and_does_not_release_the_key() {
    let tmp = tempfile::tempdir().unwrap();
    let directory = tmp.path().to_string_lossy().into_owned();
    let calls = Arc::new(AtomicUsize::new(0));
    let held = Arc::new(std::sync::Mutex::new(None));
    let provider = Box::new(ControlledCashuProvider {
        calls: calls.clone(),
        behavior: Behavior::RefusalWithLedgerHeld {
            directory: directory.clone(),
            held: held.clone(),
        },
    });
    let (app, mut rx) = make_app(directory, provider);
    let plan = plan_cashu_send(&app, &mut rx, 10, "clear-plan").await;
    dispatch(
        &app,
        Request::from_input(confirm_with_key(&plan, Some("clear-key"), "clear-confirm")),
    )
    .await;
    let mut saw_failure_log = false;
    loop {
        match rx.recv().await.unwrap() {
            Output::Log { event, .. } if event == "idempotency_clear_failed" => {
                saw_failure_log = true
            }
            Output::Error { error_code, .. } => {
                assert_eq!(error_code, "invalid_amount");
                break;
            }
            Output::Log { .. } => {}
            other => panic!("unexpected refusal output: {other:?}"),
        }
    }
    assert!(saw_failure_log);
    held.lock().unwrap().take();
    dispatch(
        &app,
        Request::from_input(confirm_with_key(&plan, Some("clear-key"), "clear-retry")),
    )
    .await;
    let output = drain_until(&mut rx, |o| matches!(o, Output::Error { .. })).await;
    let Output::Error { error_code, .. } = output else {
        unreachable!()
    };
    assert_eq!(error_code, "idempotency_in_progress");
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[cfg(feature = "ln-phoenixd")]
#[tokio::test]
async fn real_phoenixd_response_loss_retains_the_send_budget_and_plan() {
    use pay_submission::*;
    let tmp = tempfile::tempdir().unwrap();
    let stub = PhoenixStub::start(PaymentResponse::Lost, None).await;
    let config = RuntimeConfig {
        data_dir: tmp.path().to_string_lossy().into_owned(),
        ..RuntimeConfig::default()
    };
    let store = create_storage_backend(&config).unwrap();
    let (app, mut rx) = pay_submission::make_app(config.clone(), store, &stub.endpoint, true);
    install_limit(&app).await;
    let plan_id = plan(&app, &mut rx, "lost-plan").await;
    let output = terminal(
        &app,
        &mut rx,
        confirm(&plan_id, Some("lost-key"), "lost-confirm"),
    )
    .await;
    let Output::AccountingInconsistent {
        reservation_ids,
        transaction_id,
        ..
    } = output
    else {
        panic!("unexpected output: {output:?}")
    };
    let invoice: lightning_invoice::Bolt11Invoice = INVOICE.parse().unwrap();
    assert_eq!(transaction_id, Some(invoice.payment_hash().to_string()));
    assert_eq!(reservation_ids.len(), 1);
    assert_eq!(app.spend_ledger.get_status().await.unwrap()[0].spent, 2);
    age_redb_reservation(&config.data_dir, reservation_ids[0], 120_000);
    drop(app);
    let store = create_storage_backend(&config).unwrap();
    let (app, mut rx) = pay_submission::make_app(config, store, &stub.endpoint, true);
    assert_eq!(app.spend_ledger.get_status().await.unwrap()[0].spent, 2);
    assert_error(
        terminal(
            &app,
            &mut rx,
            confirm(&plan_id, Some("lost-key"), "retry-key"),
        )
        .await,
        "idempotency_in_progress",
    );
    assert_error(
        terminal(
            &app,
            &mut rx,
            confirm(&plan_id, Some("fresh-key"), "retry-fresh"),
        )
        .await,
        "plan_not_found",
    );
    assert_error(
        terminal(&app, &mut rx, confirm(&plan_id, None, "retry-no-key")).await,
        "plan_not_found",
    );
    let second = plan(&app, &mut rx, "new-lost-plan").await;
    assert!(matches!(
        terminal(
            &app,
            &mut rx,
            confirm(&second, Some("second-key"), "second-confirm")
        )
        .await,
        Output::LimitExceeded { .. }
    ));
    assert_eq!(stub.calls.load(Ordering::SeqCst), 1);
    app.spend_ledger
        .force_cancel(reservation_ids[0], "provider confirms absent payment")
        .await
        .unwrap();
    assert_eq!(app.spend_ledger.get_status().await.unwrap()[0].spent, 0);
}

#[cfg(feature = "ln-phoenixd")]
#[tokio::test]
async fn successful_send_reports_redb_finalize_failure_without_resubmission() {
    use pay_submission::*;
    let tmp = tempfile::tempdir().unwrap();
    let directory = tmp.path().to_string_lossy().into_owned();
    let held_db = Arc::new(std::sync::Mutex::new(None));
    let fault_db = held_db.clone();
    let db_path = tmp.path().join("spend/spend.redb");
    let stub = PhoenixStub::start(
        PaymentResponse::Success,
        Some(Arc::new(move || {
            *fault_db.lock().unwrap() = Some(redb::Database::open(&db_path).unwrap());
        })),
    )
    .await;
    let config = RuntimeConfig {
        data_dir: directory,
        ..RuntimeConfig::default()
    };
    let store = create_storage_backend(&config).unwrap();
    let (app, mut rx) = pay_submission::make_app(config, store, &stub.endpoint, false);
    let plan_id = plan(&app, &mut rx, "finalize-plan").await;
    dispatch(
        &app,
        Request::from_input(confirm(&plan_id, Some("finalize-key"), "finalize-confirm")),
    )
    .await;
    let mut saw_failure_log = false;
    loop {
        match rx.recv().await.unwrap() {
            Output::Log { event, .. } if event == "idempotency_finalize_failed" => {
                saw_failure_log = true
            }
            Output::Sent {
                idempotency_finalize_failed,
                ..
            } => {
                assert!(idempotency_finalize_failed);
                break;
            }
            Output::Log { .. } => {}
            other => panic!("unexpected receipt: {other:?}"),
        }
    }
    assert!(saw_failure_log);
    held_db.lock().unwrap().take();
    assert_error(
        terminal(
            &app,
            &mut rx,
            confirm(&plan_id, Some("finalize-key"), "retry-finalize"),
        )
        .await,
        "idempotency_in_progress",
    );
    assert_error(
        terminal(
            &app,
            &mut rx,
            confirm(&plan_id, Some("fresh-finalize-key"), "retry-fresh-finalize"),
        )
        .await,
        "plan_not_found",
    );
    assert_eq!(stub.calls.load(Ordering::SeqCst), 1);
}

/// Without a key there is no replay record — but the plan is still single-use,
/// so a second confirm is refused rather than paying twice. The plan boundary
/// is what protects the caller here, not the key.
#[tokio::test]
async fn a_confirm_without_a_key_is_still_protected_by_the_plan() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Box::new(ControlledCashuProvider {
        calls: calls.clone(),
        behavior: Behavior::Success {
            tx_id: "tx_no_key".to_string(),
        },
    });
    let (app, mut rx) = make_app(tmp.path().to_string_lossy().into_owned(), provider);

    let plan_id = plan_cashu_send(&app, &mut rx, 10, "req_plan").await;
    dispatch(
        &app,
        Request::from_input(confirm_with_key(&plan_id, None, "req_a")),
    )
    .await;
    let _ = drain_until(&mut rx, |o| matches!(o, Output::CashuSent { .. })).await;

    dispatch(
        &app,
        Request::from_input(confirm_with_key(&plan_id, None, "req_b")),
    )
    .await;
    let refused = drain_until(&mut rx, |o| matches!(o, Output::Error { .. })).await;
    let Output::Error { error_code, .. } = refused else {
        unreachable!()
    };
    assert_eq!(error_code, "plan_not_found");
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "one plan is one payment, key or no key"
    );
}

#[tokio::test]
async fn reconcile_reservation_confirms_expired_pending() {
    use agent_first_pay::types::ReconcileAction;

    let tmp = tempfile::tempdir().unwrap();
    // Provider not exercised here — we only test the reconcile handler path.
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Box::new(ControlledCashuProvider {
        calls: calls.clone(),
        behavior: Behavior::Success {
            tx_id: "unused".to_string(),
        },
    });
    let (app, mut rx) = make_app(tmp.path().to_string_lossy().into_owned(), provider);

    // Reserve a budget directly so we have something to reconcile. enforce_limits
    // is false in tests, so we use the ledger API directly.
    use agent_first_pay::spend::SpendContext;
    use agent_first_pay::types::{SpendLimit, SpendScope};
    app.spend_ledger
        .set_limits(&[SpendLimit {
            rule_id: None,
            scope: SpendScope::Network,
            network: Some("cashu".to_string()),
            wallet: None,
            window_s: 3600,
            max_spend: 10_000,
            token: None,
        }])
        .await
        .unwrap();
    let ctx = SpendContext {
        network: "cashu".to_string(),
        wallet: Some("w_test".to_string()),
        amount_native: 250,
        token: None,
    };
    let rid = app.spend_ledger.reserve("op_recon", &ctx).await.unwrap();

    dispatch(
        &app,
        Request::from_input(Input::ReconcileReservation {
            id: "req_recon".to_string(),
            reservation_id: rid,
            action: ReconcileAction::Confirm,
            reason: "operator vouches the payment landed".to_string(),
        }),
    )
    .await;

    let out = drain_until(&mut rx, |o| matches!(o, Output::Reconciled { .. })).await;
    let Output::Reconciled {
        reservation_id,
        previous_status,
        new_status,
        ..
    } = out
    else {
        unreachable!()
    };
    assert_eq!(reservation_id, rid);
    assert_eq!(previous_status, "pending");
    assert_eq!(new_status, "confirmed");

    // Budget reflects the confirmed spend.
    let status = app.spend_ledger.get_status().await.unwrap();
    assert_eq!(status[0].spent, 250);
}

/// A memo the caller asked for must survive into the plan a person approves.
///
/// Regression: `Output::PayPlanned` carried no memo at all, so the confirm
/// card's memo row was permanently dead and someone approving a payment could
/// not see what it would write on-chain — which is public and cannot be undone.
/// The rendering half is covered in `mode::ui`; this covers the plumbing, which
/// is the half that silently broke and the half a unit test on the page cannot
/// reach.
#[tokio::test]
async fn a_plan_carries_the_memo_the_payment_will_write_on_chain() {
    let tmp = tempfile::tempdir().unwrap();
    let provider = Box::new(ControlledCashuProvider {
        calls: Arc::new(AtomicUsize::new(0)),
        behavior: Behavior::Success {
            tx_id: "tx_memo".to_string(),
        },
    });
    let (app, mut rx) = make_app(tmp.path().to_string_lossy().into_owned(), provider);

    dispatch(
        &app,
        Request::from_input(Input::CashuSendPlan {
            id: "memo_plan".to_string(),
            wallet: None,
            amount: Amount {
                value: 10,
                token: "sats".to_string(),
            },
            onchain_memo: Some("invoice 4471".to_string()),
            local_memo: None,
            mints: None,
        }),
    )
    .await;

    let planned = drain_until(&mut rx, |o| matches!(o, Output::PayPlanned { .. })).await;
    let Output::PayPlanned { onchain_memo, .. } = planned else {
        unreachable!()
    };
    assert_eq!(onchain_memo.as_deref(), Some("invoice 4471"));
}
