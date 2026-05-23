use crate::provider::PayError;
use crate::spend::SpendContext;
use crate::types::Output;
use std::future::Future;
use std::time::Instant;

use super::helpers::{emit_error, emit_log, trace_from};
use super::App;

/// Reserve spend budget, execute an async operation, then confirm or cancel.
pub(super) async fn with_spend_reserve<F, Fut, T>(
    app: &App,
    id: &str,
    op_prefix: &str,
    spend_ctx: SpendContext,
    start: Instant,
    send_fn: F,
) -> Option<Result<T, PayError>>
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
) -> Option<Result<T, PayError>>
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

    if !reservation_ids.is_empty() {
        match &result {
            Ok(_) => {
                for rid in &reservation_ids {
                    if let Err(e) = app.spend_ledger.confirm(*rid).await {
                        emit_log(
                            app,
                            "spend_confirm_failed",
                            Some(id.to_string()),
                            serde_json::json!({
                                "reservation_id": rid,
                                "error": e.to_string(),
                            }),
                        )
                        .await;
                    }
                }
            }
            Err(_) => {
                cancel_reservations(app, id, &reservation_ids).await;
            }
        }
    }

    Some(result)
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
        if let Err(first_err) = app.spend_ledger.cancel(*rid).await {
            if let Err(retry_err) = app.spend_ledger.cancel(*rid).await {
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
}
