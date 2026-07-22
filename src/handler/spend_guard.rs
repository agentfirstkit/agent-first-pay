use crate::provider::PayError;
use crate::spend::SpendContext;
use crate::types::Output;
use std::future::Future;
use std::time::{Duration, Instant};

use super::App;
use super::helpers::{emit_error, emit_log, trace_from};

/// Outcome of `with_spend_reserves`.
///
/// * `result` — what the underlying send/cashu_send returned. `Some(Ok(_))` means
///   the network operation succeeded; `Some(Err(_))` means it failed (and any
///   reservations were cancelled cleanly).
/// * `confirmed_reservation_ids` — populated on success with the reservation
///   ids that the ledger acknowledged. Surface these in `Output::Sent` /
///   `Output::CashuSent` so an agent can drive future reconciliation /
///   cancel flows against the exact debits its payment consumed.
/// * `unconfirmed_reservations` — populated ONLY when the network operation
///   succeeded but one or more `confirm` attempts on the ledger failed even
///   after a retry. Each entry is `(reservation_id, last_error)`. When this is
///   non-empty, the caller MUST surface `Output::AccountingInconsistent` to the
///   agent before emitting any "success" output: the money left the wallet but
///   the ledger does not reflect it, so retrying would double-spend the budget.
pub(super) struct SpendOutcome<T> {
    pub result: Result<T, PayError>,
    pub confirmed_reservation_ids: Vec<u64>,
    pub unconfirmed_reservations: Vec<(u64, String)>,
}

/// Reserve spend budget, execute an async operation, then confirm or cancel.
pub(super) async fn with_spend_reserve<F, Fut, T>(
    app: &App,
    id: &str,
    op_prefix: &str,
    spend_ctx: SpendContext,
    start: Instant,
    send_fn: F,
) -> Option<SpendOutcome<T>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, PayError>>,
{
    with_spend_reserves(app, id, op_prefix, vec![spend_ctx], start, send_fn).await
}

/// Reserve multiple asset debits for one payment, execute it, then confirm or cancel all debits.
pub(super) async fn with_spend_reserves<F, Fut, T>(
    app: &App,
    id: &str,
    op_prefix: &str,
    spend_contexts: Vec<SpendContext>,
    start: Instant,
    send_fn: F,
) -> Option<SpendOutcome<T>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, PayError>>,
{
    let reservation_ids = if app.enforce_limits {
        let mut reservation_ids = Vec::new();
        for (idx, spend_ctx) in spend_contexts.iter().enumerate() {
            let op_id = if spend_contexts.len() == 1 {
                format!("{op_prefix}:{id}")
            } else {
                format!("{op_prefix}:{id}:{idx}")
            };
            match app.spend_ledger.reserve(&op_id, spend_ctx).await {
                Ok(rid) => {
                    if app.spend_ledger.take_fx_stale_warning() {
                        emit_log(
                            app,
                            "fx_quote_stale",
                            Some(id.to_string()),
                            serde_json::json!({
                                "message": "exchange rate quote age exceeds 80% of TTL; rate may be outdated",
                            }),
                        )
                        .await;
                    }
                    reservation_ids.push(rid);
                }
                Err(e) => {
                    cancel_reservations(app, id, &reservation_ids).await;
                    emit_reservation_error(app, id, &e, start).await;
                    return None;
                }
            }
        }
        reservation_ids
    } else {
        Vec::new()
    };

    let result = send_fn().await;

    let mut unconfirmed_reservations: Vec<(u64, String)> = Vec::new();
    let mut confirmed_reservation_ids: Vec<u64> = Vec::new();
    if !reservation_ids.is_empty() {
        match &result {
            Ok(_) => {
                for rid in &reservation_ids {
                    // One short-backoff retry: spend_ledger writes can fail on
                    // transient lock contention or storage hiccups; a second
                    // attempt usually clears them. If both fail the money has
                    // left the wallet AND the ledger lost the debit — the caller
                    // surfaces AccountingInconsistent so an operator can fix it.
                    let first = app.spend_ledger.confirm(*rid).await;
                    match first {
                        Ok(()) => {
                            confirmed_reservation_ids.push(*rid);
                        }
                        Err(first_err) => {
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            match app.spend_ledger.confirm(*rid).await {
                                Ok(()) => {
                                    confirmed_reservation_ids.push(*rid);
                                    emit_log(
                                        app,
                                        "spend_confirm_retry_succeeded",
                                        Some(id.to_string()),
                                        serde_json::json!({
                                            "reservation_id": rid,
                                            "first_error": first_err.to_string(),
                                        }),
                                    )
                                    .await;
                                }
                                Err(second_err) => {
                                    emit_log(
                                        app,
                                        "spend_confirm_failed",
                                        Some(id.to_string()),
                                        serde_json::json!({
                                            "reservation_id": rid,
                                            "first_error": first_err.to_string(),
                                            "second_error": second_err.to_string(),
                                        }),
                                    )
                                    .await;
                                    unconfirmed_reservations.push((*rid, second_err.to_string()));
                                }
                            }
                        }
                    }
                }
            }
            Err(_) => {
                cancel_reservations(app, id, &reservation_ids).await;
            }
        }
    }

    Some(SpendOutcome {
        result,
        confirmed_reservation_ids,
        unconfirmed_reservations,
    })
}

/// Emit `Output::AccountingInconsistent` when the spend ledger could not confirm
/// reservations after a successful send. Call this from the success branch of
/// every `with_spend_reserves` caller BEFORE the normal success output, so an
/// agent sees the inconsistency first and never retries the request.
pub(super) async fn emit_accounting_inconsistent(
    app: &App,
    id: &str,
    transaction_id: &str,
    unconfirmed: Vec<(u64, String)>,
    start: Instant,
) {
    if unconfirmed.is_empty() {
        return;
    }
    let (reservation_ids, confirm_errors): (Vec<_>, Vec<_>) = unconfirmed.into_iter().unzip();
    let _ = app
        .writer
        .send(Output::AccountingInconsistent {
            id: id.to_string(),
            transaction_id: transaction_id.to_string(),
            reservation_ids,
            confirm_errors,
            hint: "money left the wallet but the spend ledger could not record one or more debits; reconcile manually before issuing further sends to avoid double-spending the budget".to_string(),
            trace: trace_from(start),
        })
        .await;
}

async fn emit_reservation_error(app: &App, id: &str, e: &PayError, start: Instant) {
    if let PayError::LimitExceeded {
        rule_id,
        scope,
        scope_key,
        spent,
        max_spend,
        token,
        remaining_s,
        origin,
        ..
    } = e
    {
        let _ = app
            .writer
            .send(Output::LimitExceeded {
                id: id.to_string(),
                rule_id: rule_id.clone(),
                scope: *scope,
                scope_key: scope_key.clone(),
                spent: *spent,
                max_spend: *max_spend,
                token: token.clone(),
                remaining_s: *remaining_s,
                origin: origin.clone(),
                trace: trace_from(start),
            })
            .await;
    } else {
        emit_error(&app.writer, Some(id.to_string()), e, start).await;
    }
}

async fn cancel_reservations(app: &App, id: &str, reservation_ids: &[u64]) {
    for rid in reservation_ids {
        if let Err(first_err) = app.spend_ledger.cancel(*rid).await
            && let Err(retry_err) = app.spend_ledger.cancel(*rid).await
        {
            emit_log(
                app,
                "spend_cancel_failed",
                Some(id.to_string()),
                serde_json::json!({
                    "reservation_id": rid,
                    "first_error": first_err.to_string(),
                    "retry_error": retry_err.to_string(),
                }),
            )
            .await;
        }
    }
}
